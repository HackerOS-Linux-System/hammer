use anyhow::{bail, Context, Result};
use owo_colors::OwoColorize;
use std::path::Path;
use std::process::Command;

use crate::livecheck;
use crate::log;

pub const RO_PATHS: &[&str] = &[
    "/usr", "/bin", "/sbin", "/lib", "/lib64", "/lib32",
"/opt", "/srv", "/boot", "/hammer/store", "/hammer/profiles",
];

pub const ROOT_PATH: &str = "/";

pub const RW_ALWAYS: &[&str] = &[
    "/var", "/home", "/tmp", "/run", "/proc", "/sys", "/dev",
"/root", "/media", "/mnt", "/etc", "/hammer/db",
];

#[derive(Debug, Clone, PartialEq)]
pub enum FsType { Ext4, Btrfs, Overlay, Tmpfs, Other(String) }

#[derive(Debug, Clone)]
pub struct MountPoint {
    pub path:      String,
    pub fs_type:   FsType,
    pub is_ro:     bool,
    pub is_subvol: bool,
    pub subvol:    Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LockStatus { Locked, Unlocked, Partial, Unknown }

#[derive(Debug)]
pub struct ImmutableStatus {
    pub status:       LockStatus,
    pub ro_paths:     Vec<String>,
    pub rw_paths:     Vec<String>,
    pub fs_type:      FsType,
    pub btrfs_native: bool,
}

// ─────────────────────────────────────────────────────────────
//  Public API
// ─────────────────────────────────────────────────────────────

pub fn enable_immutable() -> Result<()> {
    if livecheck::is_live_system() {
        log::info("immutable: live system detected — skipping");
        println!("  {} Live system — immutable mode disabled.", "·".dimmed());
        return Ok(());
    }
    if nix::unistd::geteuid().as_raw() != 0 {
        bail!("hammer immutable requires root privileges.");
    }
    let root_fs = detect_root_fs_type()?;
    println!("  {}  Enabling immutable filesystem mode…", "⬡".bright_cyan().bold());
    println!("  {} Filesystem: {:?}", "·".dimmed(), root_fs);
    match root_fs {
        FsType::Btrfs => enable_btrfs_immutable()?,
        _             => enable_ext4_immutable()?,
    }
    std::fs::create_dir_all("/hammer/db")?;
    std::fs::write("/hammer/db/.immutable-enabled", "1\n")?;
    println!("  {} Immutable filesystem mode enabled.", "✔".bright_green().bold());
    log::info("immutable: enabled");
    Ok(())
}

pub fn unlock_for_transaction() -> Result<ImmutableGuard> {
    if livecheck::is_live_system() { return Ok(ImmutableGuard { was_locked: false }); }
    if !is_immutable_enabled()     { return Ok(ImmutableGuard { was_locked: false }); }
    let root_fs = detect_root_fs_type()?;
    log::info("immutable: unlocking for transaction");
    match root_fs {
        FsType::Btrfs => unlock_btrfs()?,
        _             => unlock_remount()?,
    }
    Ok(ImmutableGuard { was_locked: true })
}

pub fn relock() -> Result<()> {
    if livecheck::is_live_system() || !is_immutable_enabled() { return Ok(()); }
    let root_fs = detect_root_fs_type()?;
    log::info("immutable: relocking after transaction");
    match root_fs {
        FsType::Btrfs => enable_btrfs_immutable()?,
        _             => enable_ext4_immutable()?,
    }
    Ok(())
}

pub fn get_status() -> Result<ImmutableStatus> {
    let root_fs = detect_root_fs_type()?;
    let mounts  = parse_mounts()?;
    let mut ro_paths = Vec::new();
    let mut rw_paths = Vec::new();
    for path in RO_PATHS {
        if path_is_ro(path, &mounts) {
            ro_paths.push(path.to_string());
        } else if Path::new(path).exists() {
            rw_paths.push(path.to_string());
        }
    }
    let status = if rw_paths.is_empty() && !ro_paths.is_empty() { LockStatus::Locked }
    else if ro_paths.is_empty() { LockStatus::Unlocked }
    else { LockStatus::Partial };
    Ok(ImmutableStatus {
        status, ro_paths, rw_paths,
       btrfs_native: matches!(root_fs, FsType::Btrfs),
       fs_type: root_fs,
    })
}

pub fn is_immutable_enabled() -> bool {
    Path::new("/hammer/db/.immutable-enabled").exists()
}

// ─────────────────────────────────────────────────────────────
//  ImmutableGuard — RAII re-lock
// ─────────────────────────────────────────────────────────────

pub struct ImmutableGuard { was_locked: bool }

impl Drop for ImmutableGuard {
    fn drop(&mut self) {
        if self.was_locked {
            if let Err(e) = relock() {
                log::warn(&format!("immutable: relock failed on drop: {}", e));
                eprintln!("  {} WARNING: failed to re-lock filesystem: {}", "!".red().bold(), e);
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  hammer immutable CLI
// ─────────────────────────────────────────────────────────────

pub fn cmd_immutable(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("status");
    match sub {
        "enable" => { enable_immutable()?; }
        "disable" => {
            if nix::unistd::geteuid().as_raw() != 0 { bail!("hammer immutable disable requires root."); }
            println!("  {}  Disabling immutable filesystem mode…", "⬡".bright_cyan().bold());
            let root_fs = detect_root_fs_type()?;
            match root_fs {
                FsType::Btrfs => disable_btrfs_immutable()?,
                _             => unlock_remount()?,
            }
            std::fs::remove_file("/hammer/db/.immutable-enabled").ok();
            println!("  {} Immutable mode disabled.", "✔".bright_green());
            log::info("immutable: disabled");
        }
        "status" => { print_immutable_status()?; }
        "unlock" => {
            if nix::unistd::geteuid().as_raw() != 0 { bail!("hammer immutable unlock requires root."); }
            println!("  {}  Temporarily unlocking filesystem…", "⬡".bright_yellow().bold());
            println!("  {}  Run {} when done.", "!".yellow().bold(), "hammer immutable enable".cyan());
            let root_fs = detect_root_fs_type()?;
            match root_fs { FsType::Btrfs => unlock_btrfs()?, _ => unlock_remount()? }
            println!("  {} Filesystem unlocked.", "✔".bright_green());
        }
        "install-service" => {
            install_immutable_service()?;
            println!("  {} hammer-immutable.service installed.", "✔".bright_green());
        }
        other => bail!("Unknown immutable subcommand: '{}'\n  Usage: hammer immutable [status|enable|disable|unlock|install-service]", other),
    }
    Ok(())
}

fn print_immutable_status() -> Result<()> {
    println!();
    println!("  {}  Filesystem immutability status", "⬡".bright_cyan().bold());
    println!("  {}", "─".repeat(60).dimmed());

    if livecheck::is_live_system() {
        println!("  {}  Running in {} — immutable mode not active",
                 "ℹ".cyan().bold(), "live system".yellow());
        return Ok(());
    }

    let enabled = is_immutable_enabled();
    // FIX: each branch returns String separately to avoid type mismatch
    let enabled_str = if enabled { "yes".bright_green().to_string() }
    else       { "no".yellow().to_string() };
    println!("  {:<28} {}", "Immutable mode configured:".bold(), enabled_str);

    let status = get_status()?;
    let status_str = match &status.status {
        LockStatus::Locked   => "LOCKED (read-only)".bright_green().to_string(),
        LockStatus::Unlocked => "UNLOCKED (read-write)".yellow().to_string(),
        LockStatus::Partial  => "PARTIAL".yellow().to_string(),
        LockStatus::Unknown  => "UNKNOWN".dimmed().to_string(),
    };
    println!("  {:<28} {}", "Current state:".bold(), status_str);
    println!("  {:<28} {:?}", "Filesystem type:".bold(), status.fs_type);
    println!();
    println!("  {}", "Read-only paths:".bold());
    if status.ro_paths.is_empty() {
        println!("    {} (none — system is unlocked)", "·".dimmed());
    } else {
        for p in &status.ro_paths { println!("    {} {}", "✔".bright_green(), p); }
    }
    if !status.rw_paths.is_empty() {
        println!();
        println!("  {}", "Should be RO but are RW:".bold().yellow());
        for p in &status.rw_paths { println!("    {} {}", "!".yellow().bold(), p); }
    }
    println!();
    println!("  {}", "Always read-write (by design):".bold());
    for p in &["/var","/home","/tmp","/run","/etc","/hammer/db"] {
        println!("    {} {}", "·".dimmed(), p.cyan());
    }
    println!();
    if !enabled {
        println!("  Enable: {}", "hammer immutable enable".cyan());
    } else if status.status == LockStatus::Unlocked {
        println!("  Re-lock: {}", "hammer immutable enable".cyan());
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Btrfs
// ─────────────────────────────────────────────────────────────

fn enable_btrfs_immutable() -> Result<()> {
    let subvols = find_btrfs_subvolumes()?;
    for sv in &subvols {
        if !should_be_ro(sv) { continue; }
        let _ = Command::new("btrfs")
        .args(["property", "set", "-ts", sv, "ro", "true"])
        .output();
        log::info(&format!("immutable: btrfs set ro: {}", sv));
    }
    for path in RO_PATHS {
        if !Path::new(path).exists() { continue; }
        if is_always_rw(path) { continue; }
        do_remount(path, true);
    }
    Ok(())
}

fn disable_btrfs_immutable() -> Result<()> {
    let subvols = find_btrfs_subvolumes()?;
    for sv in &subvols {
        if !should_be_ro(sv) { continue; }
        let _ = Command::new("btrfs").args(["property","set","-ts",sv,"ro","false"]).output();
    }
    unlock_remount()
}

fn unlock_btrfs() -> Result<()> {
    let subvols = find_btrfs_subvolumes()?;
    for sv in &subvols {
        let _ = Command::new("btrfs").args(["property","set","-ts",sv,"ro","false"]).output();
    }
    unlock_remount()
}

fn find_btrfs_subvolumes() -> Result<Vec<String>> {
    let out = Command::new("btrfs").args(["subvolume","list","/"]).output();
    match out {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            Ok(text.lines()
            .filter_map(|l| l.split("path ").nth(1).map(|p| format!("/{}", p.trim())))
            .collect())
        }
        _ => Ok(vec![]),
    }
}

// ─────────────────────────────────────────────────────────────
//  Ext4 / generic
// ─────────────────────────────────────────────────────────────

fn enable_ext4_immutable() -> Result<()> {
    for path in RO_PATHS {
        if !Path::new(path).exists() { continue; }
        if is_always_rw(path) { continue; }
        do_remount(path, true);
    }
    Ok(())
}

fn unlock_remount() -> Result<()> {
    let mut paths: Vec<&str> = RO_PATHS.to_vec();
    paths.push(ROOT_PATH);
    for path in paths.iter().rev() {
        if !Path::new(path).exists() { continue; }
        if is_always_rw(path) { continue; }
        do_remount(path, false);
    }
    Ok(())
}

fn do_remount(path: &str, read_only: bool) {
    #[cfg(target_os = "linux")]
    {
        let path_cstr = std::ffi::CString::new(path).unwrap();
        let flags: libc::c_ulong = {
            let ms_remount: libc::c_ulong = 32;
            let ms_bind:    libc::c_ulong = 4096;
            let ms_rdonly:  libc::c_ulong = 1;
            if read_only { ms_remount | ms_bind | ms_rdonly }
            else         { ms_remount | ms_bind }
        };
        let ret = unsafe {
            libc::mount(
                std::ptr::null(),
                        path_cstr.as_ptr(),
                        std::ptr::null(),
                        flags,
                        std::ptr::null(),
            )
        };
        if ret == 0 {
            log::info(&format!("immutable: remounted {} as {}", path, if read_only { "ro" } else { "rw" }));
        } else {
            log::warn(&format!("immutable: remount {} failed (errno={})", path,
                               unsafe { *libc::__errno_location() }));
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  systemd service
// ─────────────────────────────────────────────────────────────

pub fn install_immutable_service() -> Result<()> {
    let hammer_bin = std::fs::read_link("/proc/self/exe")
    .unwrap_or_else(|_| std::path::PathBuf::from("/usr/bin/hammer"));
    let unit = format!(
        "[Unit]\n\
Description=Hammer Immutable Filesystem — Read-Only Root\n\
DefaultDependencies=no\n\
After=hammer-activate.service local-fs.target\n\
Before=sysinit.target\n\
ConditionVirtualization=no\n\
ConditionPathExists=!/run/live\n\
ConditionPathExists=!/etc/live\n\
\n\
[Service]\n\
Type=oneshot\n\
RemainAfterExit=yes\n\
ExecStart={hammer} immutable enable\n\
ExecStop={hammer} immutable disable\n\
StandardOutput=journal\n\
StandardError=journal\n\
\n\
[Install]\n\
WantedBy=sysinit.target\n",
hammer = hammer_bin.display()
    );
    std::fs::write("/etc/systemd/system/hammer-immutable.service", &unit)?;
    let _ = Command::new("systemctl").args(["enable","hammer-immutable.service","--no-reload"]).status();
    log::info("immutable: installed hammer-immutable.service");
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Mount helpers
// ─────────────────────────────────────────────────────────────

fn parse_mounts() -> Result<Vec<MountPoint>> {
    let content = std::fs::read_to_string("/proc/mounts").context("Reading /proc/mounts")?;
    let mut mounts = Vec::new();
    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 { continue; }
        let path    = parts[1].to_string();
        let fs_type = match parts[2] {
            "ext4"              => FsType::Ext4,
            "btrfs"             => FsType::Btrfs,
            "overlay"|"overlayfs" => FsType::Overlay,
            "tmpfs"             => FsType::Tmpfs,
            other               => FsType::Other(other.to_string()),
        };
        let opts   = parts[3];
        let is_ro  = opts.split(',').any(|o| o == "ro");
        let subvol = opts.split(',').find(|o| o.starts_with("subvol="))
        .map(|o| o.trim_start_matches("subvol=").to_string());
        mounts.push(MountPoint { path, fs_type, is_ro, is_subvol: subvol.is_some(), subvol });
    }
    Ok(mounts)
}

fn detect_root_fs_type() -> Result<FsType> {
    let mounts = parse_mounts()?;
    for target in &["/", "/usr"] {
        if let Some(m) = mounts.iter().find(|m| m.path == *target) {
            return Ok(m.fs_type.clone());
        }
    }
    Ok(FsType::Ext4)
}

fn path_is_ro(path: &str, mounts: &[MountPoint]) -> bool {
    mounts.iter()
    .filter(|m| path.starts_with(&m.path))
    .max_by_key(|m| m.path.len())
    .map(|m| m.is_ro).unwrap_or(false)
}

fn should_be_ro(path: &str) -> bool { !is_always_rw(path) }

fn is_always_rw(path: &str) -> bool {
    RW_ALWAYS.iter().any(|rw| path == *rw || path.starts_with(&format!("{}/", rw)))
}
