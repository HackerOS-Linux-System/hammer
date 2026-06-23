use anyhow::{bail, Context, Result};
use owo_colors::OwoColorize;
use std::path::Path;
use std::process::Command;

use crate::livecheck;
use crate::log;

// ──────────────────────────────────────────────────────────────────────────────
//  Constants
// ──────────────────────────────────────────────────────────────────────────────

pub const RO_PATHS: &[&str] = &[
    "/usr", "/bin", "/sbin",
    "/lib", "/lib64", "/lib32", "/libx32",
    "/opt", "/srv", "/boot",
    "/hammer/store", "/hammer/profiles",
    "/snap",
];

pub const RW_ALWAYS: &[&str] = &[
    "/var", "/home", "/tmp", "/run",
    "/proc", "/sys", "/dev",
    "/root", "/media", "/mnt",
    "/etc", "/hammer/db", "/hammer/tmp",
];

pub const OVERLAY_UPPER_PATHS: &[&str] = &[
    "/var/lib/overlayfs/upper",
    "/var/lib/overlayfs/work",
];

const IMMUTABLE_FLAG_FILE:    &str = "/hammer/db/.immutable-enabled";
const IMMUTABLE_BACKEND_FILE: &str = "/hammer/db/.immutable-backend";
const SNAPSHOTS_DIR:          &str = "/hammer/snapshots";

// ──────────────────────────────────────────────────────────────────────────────
//  Types
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum FsType {
    Ext4, Btrfs, Overlay, Tmpfs, Zfs, F2fs, Xfs, Other(String),
}

impl std::fmt::Display for FsType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FsType::Ext4       => write!(f, "ext4"),
            FsType::Btrfs      => write!(f, "btrfs"),
            FsType::Overlay    => write!(f, "overlayfs"),
            FsType::Tmpfs      => write!(f, "tmpfs"),
            FsType::Zfs        => write!(f, "zfs"),
            FsType::F2fs       => write!(f, "f2fs"),
            FsType::Xfs        => write!(f, "xfs"),
            FsType::Other(s)   => write!(f, "{}", s),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImmutableBackend {
    RemountRo,    // generic: mount -o remount,ro
    BtrfsSubvol,  // btrfs property set ro true + snapshots
    ZfsReadonly,  // zfs set readonly=on + snapshots
    Chattr,       // chattr +i (file-level fallback)
    SysExt,       // systemd-sysext merge
    Auto,
}

impl ImmutableBackend {
    fn from_str(s: &str) -> Self {
        match s {
            "remount" => Self::RemountRo,
            "btrfs"   => Self::BtrfsSubvol,
            "zfs"     => Self::ZfsReadonly,
            "chattr"  => Self::Chattr,
            "sysext"  => Self::SysExt,
            _         => Self::Auto,
        }
    }
    pub fn to_str(&self) -> &'static str {
        match self {
            Self::RemountRo   => "remount",
            Self::BtrfsSubvol => "btrfs",
            Self::ZfsReadonly => "zfs",
            Self::Chattr      => "chattr",
            Self::SysExt      => "sysext",
            Self::Auto        => "auto",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MountPoint {
    pub path:      String,
    pub fs_type:   FsType,
    pub is_ro:     bool,
    pub is_subvol: bool,
    pub subvol:    Option<String>,
    pub source:    String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LockStatus { Locked, Unlocked, Partial, Unknown }

impl std::fmt::Display for LockStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockStatus::Locked   => write!(f, "LOCKED"),
            LockStatus::Unlocked => write!(f, "UNLOCKED"),
            LockStatus::Partial  => write!(f, "PARTIAL"),
            LockStatus::Unknown  => write!(f, "UNKNOWN"),
        }
    }
}

#[derive(Debug)]
pub struct ImmutableStatus {
    pub status:       LockStatus,
    pub ro_paths:     Vec<String>,
    pub rw_paths:     Vec<String>,
    pub fs_type:      FsType,
    pub btrfs_native: bool,
    pub backend:      ImmutableBackend,
    pub overlay_mode: bool,
}

// ──────────────────────────────────────────────────────────────────────────────
//  RAII guard — auto-relock after transaction
// ──────────────────────────────────────────────────────────────────────────────

pub struct ImmutableGuard { was_locked: bool }

impl Drop for ImmutableGuard {
    fn drop(&mut self) {
        if self.was_locked {
            if let Err(e) = relock() {
                log::warn(&format!("immutable: relock failed on drop: {}", e));
                eprintln!("  {} WARNING: failed to re-lock filesystem: {}",
                          "!".red().bold(), e);
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
//  Public API
// ──────────────────────────────────────────────────────────────────────────────

#[allow(unreachable_code)]
pub fn enable_immutable() -> Result<()> {
    #[cfg(feature = "normal-mode")]
    { println!("  {} Normal-mode build — immutable filesystem disabled.", "ℹ".cyan()); return Ok(()); }

    if livecheck::is_live_system() {
        log::info("immutable: live system — skipping");
        println!("  {} Live system detected — immutable mode not activated.", "·".dimmed());
        return Ok(());
    }
    ensure_root("hammer immutable enable")?;

    let root_fs = detect_root_fs_type()?;
    let backend = choose_backend(&root_fs);

    println!("  {}  Enabling immutable filesystem mode…", "⬡".bright_cyan().bold());
    println!("  {} Filesystem : {}", "·".dimmed(), root_fs.to_string().cyan());
    println!("  {} Backend    : {}", "·".dimmed(), backend.to_str().cyan());

    match &backend {
        ImmutableBackend::BtrfsSubvol => enable_btrfs_immutable()?,
        ImmutableBackend::ZfsReadonly => enable_zfs_immutable()?,
        ImmutableBackend::SysExt      => enable_sysext()?,
        _                             => enable_remount_immutable()?,
    }

    std::fs::create_dir_all("/hammer/db")?;
    std::fs::write(IMMUTABLE_FLAG_FILE,    "1\n")?;
    std::fs::write(IMMUTABLE_BACKEND_FILE, format!("{}\n", backend.to_str()))?;

    println!("  {} Immutable filesystem mode enabled.", "✔".bright_green().bold());
    log::info(&format!("immutable: enabled (backend={})", backend.to_str()));
    Ok(())
}

#[allow(unreachable_code)]
pub fn disable_immutable() -> Result<()> {
    #[cfg(feature = "normal-mode")]
    { println!("  {} Normal-mode build — immutable filesystem not active.", "ℹ".cyan()); return Ok(()); }
    ensure_root("hammer immutable disable")?;
    println!("  {}  Disabling immutable filesystem mode…", "⬡".bright_cyan().bold());
    match saved_backend() {
        ImmutableBackend::BtrfsSubvol => disable_btrfs_immutable()?,
        ImmutableBackend::ZfsReadonly => disable_zfs_immutable()?,
        ImmutableBackend::SysExt      => disable_sysext()?,
        _                             => unlock_remount()?,
    }
    let _ = std::fs::remove_file(IMMUTABLE_FLAG_FILE);
    let _ = std::fs::remove_file(IMMUTABLE_BACKEND_FILE);
    println!("  {} Immutable mode disabled.", "✔".bright_green());
    log::info("immutable: disabled");
    Ok(())
}

/// Temporarily unlock for a package transaction (auto-relocks on drop).
#[allow(unreachable_code)]
pub fn unlock_for_transaction() -> Result<ImmutableGuard> {
    #[cfg(feature = "normal-mode")]
    { return Ok(ImmutableGuard { was_locked: false }); }

    if livecheck::is_live_system() || !is_immutable_enabled() {
        return Ok(ImmutableGuard { was_locked: false });
    }
    log::info("immutable: unlocking for transaction");
    match saved_backend() {
        ImmutableBackend::BtrfsSubvol => unlock_btrfs()?,
        ImmutableBackend::ZfsReadonly => unlock_zfs()?,
        ImmutableBackend::SysExt      => unmerge_sysext()?,
        _                             => unlock_remount()?,
    }
    Ok(ImmutableGuard { was_locked: true })
}

#[allow(unreachable_code)]
pub fn relock() -> Result<()> {
    #[cfg(feature = "normal-mode")]
    { return Ok(()); }
    if livecheck::is_live_system() || !is_immutable_enabled() { return Ok(()); }
    log::info("immutable: relocking after transaction");
    match saved_backend() {
        ImmutableBackend::BtrfsSubvol => enable_btrfs_immutable()?,
        ImmutableBackend::ZfsReadonly => enable_zfs_immutable()?,
        ImmutableBackend::SysExt      => enable_sysext()?,
        _                             => enable_remount_immutable()?,
    }
    Ok(())
}

pub fn is_immutable_enabled() -> bool {
    Path::new(IMMUTABLE_FLAG_FILE).exists()
}

pub fn get_status() -> Result<ImmutableStatus> {
    let root_fs = detect_root_fs_type()?;
    let mounts  = parse_mounts()?;
    let backend = saved_backend();
    let mut ro_paths = Vec::new();
    let mut rw_paths = Vec::new();
    for path in RO_PATHS {
        if !Path::new(path).exists() { continue; }
        if path_is_ro(path, &mounts) { ro_paths.push(path.to_string()); }
        else                         { rw_paths.push(path.to_string()); }
    }
    let status = if rw_paths.is_empty() && !ro_paths.is_empty() { LockStatus::Locked   }
                 else if ro_paths.is_empty()                     { LockStatus::Unlocked }
                 else                                            { LockStatus::Partial  };
    let overlay_mode = mounts.iter().any(|m| m.fs_type == FsType::Overlay);
    Ok(ImmutableStatus {
        status, ro_paths, rw_paths, overlay_mode, backend,
        btrfs_native: matches!(root_fs, FsType::Btrfs),
        fs_type: root_fs,
    })
}

// ──────────────────────────────────────────────────────────────────────────────
//  CLI dispatcher
// ──────────────────────────────────────────────────────────────────────────────

pub fn cmd_immutable(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("status");
    match sub {
        "enable"           => enable_immutable()?,
        "disable"          => disable_immutable()?,
        "status"           => print_immutable_status()?,
        "verify"           => verify_integrity()?,
        "lock"             => { ensure_root("hammer immutable lock")?;
                                println!("  {}  Relocking…", "⬡".bright_cyan().bold());
                                relock()?;
                                println!("  {} Relocked.", "✔".bright_green()); }
        "unlock"           => {
            ensure_root("hammer immutable unlock")?;
            println!("  {}  Temporarily unlocking…", "⬡".bright_yellow().bold());
            println!("  {}  Run {} when done.", "!".yellow().bold(), "hammer immutable lock".cyan());
            match saved_backend() {
                ImmutableBackend::BtrfsSubvol => unlock_btrfs()?,
                ImmutableBackend::ZfsReadonly => unlock_zfs()?,
                ImmutableBackend::SysExt      => unmerge_sysext()?,
                _                             => unlock_remount()?,
            }
            println!("  {} Unlocked (temporary).", "✔".bright_green());
            log::info("immutable: manually unlocked");
        }
        "install-service"  => { install_immutable_service()?;
                                println!("  {} hammer-immutable.service installed.", "✔".bright_green()); }
        "snapshot"         => {
            let label = args.get(1).map(|s| s.as_str()).unwrap_or("manual");
            create_snapshot(label)?;
        }
        "snapshots" | "list-snapshots" => list_snapshots()?,
        other => bail!(
            "Unknown immutable subcommand: '{}'\n  \
             Usage: hammer immutable [status|enable|disable|lock|unlock|\
             verify|snapshot <label>|snapshots|install-service]", other),
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
//  Status printer
// ──────────────────────────────────────────────────────────────────────────────

#[allow(unreachable_code)]
fn print_immutable_status() -> Result<()> {
    println!();
    println!("  {}  Filesystem Immutability — hammer v{}",
             "⬡".bright_cyan().bold(), env!("CARGO_PKG_VERSION"));
    println!("  {}", "─".repeat(60).dimmed());

    if livecheck::is_live_system() {
        println!("  {}  Running as {} — immutable mode inactive", "ℹ".cyan().bold(), "live system".yellow());
        return Ok(());
    }

    #[cfg(feature = "normal-mode")]
    {
        println!("  {}  Built in {} — immutable FS subsystem disabled.",
                 "ℹ".cyan().bold(), "normal-mode".yellow());
        return Ok(());
    }

    let enabled = is_immutable_enabled();
    println!("  {:<32} {}", "Configured:".bold(),
             if enabled { "yes".bright_green().to_string() }
             else       { "no  (hammer immutable enable)".yellow().to_string() });

    let s = get_status()?;
    let status_str = match &s.status {
        LockStatus::Locked   => "LOCKED (read-only)".bright_green().to_string(),
        LockStatus::Unlocked => "UNLOCKED".yellow().to_string(),
        LockStatus::Partial  => "PARTIAL — some paths writable!".yellow().bold().to_string(),
        LockStatus::Unknown  => "UNKNOWN".dimmed().to_string(),
    };
    println!("  {:<32} {}", "Current state:".bold(), status_str);
    println!("  {:<32} {}", "Filesystem:".bold(),    s.fs_type.to_string().cyan());
    println!("  {:<32} {}", "Backend:".bold(),        s.backend.to_str().cyan());
    if s.overlay_mode { println!("  {:<32} {}", "OverlayFS detected:".bold(), "yes".cyan()); }
    println!();
    println!("  {}", "Protected (read-only):".bold());
    if s.ro_paths.is_empty() {
        println!("    {} none", "·".dimmed());
    } else {
        for p in &s.ro_paths { println!("    {} {}", "✔".bright_green(), p); }
    }
    if !s.rw_paths.is_empty() {
        println!();
        println!("  {}", "⚠  Should be protected but writable:".bold().yellow());
        for p in &s.rw_paths { println!("    {} {}", "!".red().bold(), p.red()); }
    }
    println!();
    println!("  {}", "Always read-write:".bold());
    for p in RW_ALWAYS { println!("    {} {}", "·".dimmed(), p.cyan()); }
    if let Ok(snaps) = list_snapshots_raw() {
        if !snaps.is_empty() {
            println!();
            println!("  {}", "Snapshots:".bold());
            for s in &snaps { println!("    {} {}", "·".dimmed(), s.cyan()); }
        }
    }
    println!();
    println!("  Verify integrity : {}", "hammer immutable verify".cyan());
    println!("  Create snapshot  : {}", "hammer immutable snapshot <label>".cyan());
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
//  Backend: Generic remount
// ──────────────────────────────────────────────────────────────────────────────

fn enable_remount_immutable() -> Result<()> {
    for path in RO_PATHS {
        if !Path::new(path).exists() || is_always_rw(path) { continue; }
        do_remount(path, true);
    }
    Ok(())
}

fn unlock_remount() -> Result<()> {
    let mut paths: Vec<&str> = RO_PATHS.to_vec();
    paths.push("/");
    paths.reverse();
    for path in &paths {
        if !Path::new(path).exists() || is_always_rw(path) { continue; }
        do_remount(path, false);
    }
    Ok(())
}

fn do_remount(path: &str, ro: bool) {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        let path_c = CString::new(path).unwrap();
        const MS_REMOUNT: libc::c_ulong = 32;
        const MS_BIND:    libc::c_ulong = 4096;
        const MS_RDONLY:  libc::c_ulong = 1;
        let flags = if ro { MS_REMOUNT | MS_BIND | MS_RDONLY } else { MS_REMOUNT | MS_BIND };
        let ret = unsafe {
            libc::mount(std::ptr::null(), path_c.as_ptr(), std::ptr::null(), flags, std::ptr::null())
        };
        if ret == 0 {
            log::info(&format!("immutable: remounted {} as {}", path, if ro {"ro"} else {"rw"}));
        } else {
            let errno = unsafe { *libc::__errno_location() };
            log::warn(&format!("immutable: remount {} failed errno={}", path, errno));
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
//  Backend: Btrfs
// ──────────────────────────────────────────────────────────────────────────────

fn enable_btrfs_immutable() -> Result<()> {
    for sv in &find_btrfs_subvolumes()? {
        if !should_be_ro(sv) { continue; }
        let _ = Command::new("btrfs").args(["property","set","-ts",sv,"ro","true"]).output();
        log::info(&format!("immutable: btrfs ro on {}", sv));
    }
    enable_remount_immutable()
}

fn disable_btrfs_immutable() -> Result<()> {
    for sv in &find_btrfs_subvolumes()? {
        let _ = Command::new("btrfs").args(["property","set","-ts",sv,"ro","false"]).output();
    }
    unlock_remount()
}

fn unlock_btrfs() -> Result<()> { disable_btrfs_immutable() }

fn find_btrfs_subvolumes() -> Result<Vec<String>> {
    let out = Command::new("btrfs").args(["subvolume","list","/"]).output();
    match out {
        Ok(o) if o.status.success() => Ok(
            String::from_utf8_lossy(&o.stdout).lines()
                .filter_map(|l| l.split("path ").nth(1).map(|p| format!("/{}", p.trim())))
                .collect()
        ),
        _ => Ok(vec![]),
    }
}

fn create_btrfs_snapshot(label: &str) -> Result<String> {
    let ts   = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let dest = format!("{}/snap-{}-{}", SNAPSHOTS_DIR, label, ts);
    std::fs::create_dir_all(SNAPSHOTS_DIR)?;
    let out  = Command::new("btrfs")
        .args(["subvolume","snapshot","-r","/",&dest])
        .output().context("btrfs snapshot")?;
    if !out.status.success() {
        bail!("btrfs snapshot: {}", String::from_utf8_lossy(&out.stderr));
    }
    log::info(&format!("immutable: btrfs snapshot {}", dest));
    Ok(dest)
}

// ──────────────────────────────────────────────────────────────────────────────
//  Backend: ZFS
// ──────────────────────────────────────────────────────────────────────────────

fn detect_zfs_pool() -> Option<String> {
    let out = Command::new("zfs").args(["list","-H","-o","name","/"]).output().ok()?;
    if out.status.success() { Some(String::from_utf8_lossy(&out.stdout).trim().to_string()) }
    else                    { None }
}

fn enable_zfs_immutable() -> Result<()> {
    let pool = detect_zfs_pool().ok_or_else(|| anyhow::anyhow!("ZFS pool for / not found"))?;
    for ds in &zfs_ro_datasets(&pool)? {
        let _ = Command::new("zfs").args(["set","readonly=on",ds]).output();
        log::info(&format!("immutable: zfs readonly=on {}", ds));
    }
    Ok(())
}

fn disable_zfs_immutable() -> Result<()> {
    if let Some(pool) = detect_zfs_pool() {
        for ds in &zfs_ro_datasets(&pool).unwrap_or_default() {
            let _ = Command::new("zfs").args(["set","readonly=off",ds]).output();
        }
    }
    Ok(())
}

fn unlock_zfs() -> Result<()> { disable_zfs_immutable() }

fn zfs_ro_datasets(pool: &str) -> Result<Vec<String>> {
    let out  = Command::new("zfs").args(["list","-H","-o","name","-r",pool]).output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.lines().filter(|l| !l.is_empty())
        .map(|l| l.trim().to_string())
        .filter(|ds| RO_PATHS.iter().any(|p| ds.ends_with(p.trim_start_matches('/'))))
        .collect())
}

fn create_zfs_snapshot(pool: &str, label: &str) -> Result<String> {
    let ts   = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let snap = format!("{}@snap-{}-{}", pool, label, ts);
    let out  = Command::new("zfs").args(["snapshot","-r",&snap]).output().context("zfs snapshot")?;
    if !out.status.success() { bail!("zfs snapshot: {}", String::from_utf8_lossy(&out.stderr)); }
    log::info(&format!("immutable: zfs snapshot {}", snap));
    Ok(snap)
}

// ──────────────────────────────────────────────────────────────────────────────
//  Backend: systemd-sysext
// ──────────────────────────────────────────────────────────────────────────────

fn enable_sysext() -> Result<()> {
    let out = Command::new("systemd-sysext").arg("merge").output().context("sysext merge")?;
    if !out.status.success() { bail!("sysext merge: {}", String::from_utf8_lossy(&out.stderr)); }
    log::info("immutable: sysext merge");
    Ok(())
}

fn disable_sysext() -> Result<()> {
    let _ = Command::new("systemd-sysext").arg("unmerge").output();
    Ok(())
}

fn unmerge_sysext() -> Result<()> { disable_sysext() }

// ──────────────────────────────────────────────────────────────────────────────
//  Snapshots
// ──────────────────────────────────────────────────────────────────────────────

pub fn create_snapshot(label: &str) -> Result<()> {
    ensure_root("hammer immutable snapshot")?;
    let result = match saved_backend() {
        ImmutableBackend::BtrfsSubvol => create_btrfs_snapshot(label)?,
        ImmutableBackend::ZfsReadonly => {
            let pool = detect_zfs_pool().ok_or_else(|| anyhow::anyhow!("ZFS pool not found"))?;
            create_zfs_snapshot(&pool, label)?
        }
        _ => {
            let ts   = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let name = format!("snap-{}-{}", label, ts);
            std::fs::create_dir_all(SNAPSHOTS_DIR)?;
            std::fs::write(
                format!("{}/{}.snap", SNAPSHOTS_DIR, name),
                format!("label={}\ntime={}\nbackend=remount\n", label, ts),
            )?;
            name
        }
    };
    println!("  {} Snapshot created: {}", "✔".bright_green().bold(), result.cyan());
    Ok(())
}

pub fn list_snapshots() -> Result<()> {
    let snaps = list_snapshots_raw()?;
    println!("\n  {}  Snapshots\n  {}", "⬡".bright_cyan().bold(), "─".repeat(50).dimmed());
    if snaps.is_empty() { println!("  {} No snapshots found.", "·".dimmed()); }
    else { for s in &snaps { println!("    {} {}", "·".dimmed(), s.cyan()); } }
    Ok(())
}

fn list_snapshots_raw() -> Result<Vec<String>> {
    match saved_backend() {
        ImmutableBackend::BtrfsSubvol => {
            let out = Command::new("btrfs").args(["subvolume","list","-r",SNAPSHOTS_DIR]).output()?;
            Ok(String::from_utf8_lossy(&out.stdout).lines()
                .filter_map(|l| l.split("path ").nth(1).map(|p| p.trim().to_string()))
                .collect())
        }
        ImmutableBackend::ZfsReadonly => {
            if let Some(pool) = detect_zfs_pool() {
                let out = Command::new("zfs")
                    .args(["list","-H","-t","snapshot","-o","name","-r",&pool]).output()?;
                Ok(String::from_utf8_lossy(&out.stdout).lines()
                    .map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
            } else { Ok(vec![]) }
        }
        _ => {
            let dir = Path::new(SNAPSHOTS_DIR);
            if !dir.exists() { return Ok(vec![]); }
            Ok(std::fs::read_dir(dir)?
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect())
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
//  Integrity verification
// ──────────────────────────────────────────────────────────────────────────────

pub fn verify_integrity() -> Result<()> {
    println!("\n  {}  Verifying filesystem integrity…", "⬡".bright_cyan().bold());
    let mounts = parse_mounts()?;
    let (mut ok, mut fail) = (0usize, 0usize);
    for path in RO_PATHS {
        if !Path::new(path).exists() { continue; }
        if path_is_ro(path, &mounts) {
            println!("  {} {} — read-only", "✔".bright_green(), path);
            ok += 1;
        } else {
            println!("  {} {} — WRITABLE ✗", "!".red().bold(), path.red());
            fail += 1;
        }
    }
    println!();
    if fail == 0 { println!("  {} All {} protected paths are read-only.", "✔".bright_green().bold(), ok); }
    else { println!("  {} {} path(s) should be read-only. Run: {}", "✗".red().bold(), fail, "hammer immutable lock".cyan()); }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
//  systemd service
// ──────────────────────────────────────────────────────────────────────────────

pub fn install_immutable_service() -> Result<()> {
    let bin = std::fs::read_link("/proc/self/exe")
        .unwrap_or_else(|_| std::path::PathBuf::from("/usr/bin/hammer"));
    let unit = format!(
        "[Unit]\n\
         Description=Hammer Immutable Filesystem\n\
         DefaultDependencies=no\n\
         After=local-fs.target\n\
         Before=sysinit.target\n\
         ConditionVirtualization=no\n\
         ConditionPathExists=!/run/live\n\
         ConditionPathExists={flag}\n\n\
         [Service]\n\
         Type=oneshot\n\
         RemainAfterExit=yes\n\
         ExecStart={bin} immutable enable\n\
         ExecStop={bin} immutable disable\n\
         TimeoutSec=60\n\n\
         [Install]\n\
         WantedBy=sysinit.target\n",
        flag = IMMUTABLE_FLAG_FILE, bin = bin.display()
    );
    std::fs::write("/etc/systemd/system/hammer-immutable.service", &unit)?;
    let _ = Command::new("systemctl").args(["enable","hammer-immutable.service","--no-reload"]).status();
    let _ = Command::new("systemctl").arg("daemon-reload").status();
    log::info("immutable: service installed");
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
//  Mount/fs helpers
// ──────────────────────────────────────────────────────────────────────────────

fn parse_mounts() -> Result<Vec<MountPoint>> {
    let text = std::fs::read_to_string("/proc/mounts").context("/proc/mounts")?;
    Ok(text.lines().filter_map(|line| {
        let p: Vec<&str> = line.split_whitespace().collect();
        if p.len() < 4 { return None; }
        let opts   = p[3];
        let is_ro  = opts.split(',').any(|o| o == "ro");
        let subvol = opts.split(',').find(|o| o.starts_with("subvol="))
                         .map(|o| o.trim_start_matches("subvol=").to_string());
        Some(MountPoint {
            source: p[0].to_string(), path: p[1].to_string(),
            fs_type: parse_fs_type(p[2]), is_ro,
            is_subvol: subvol.is_some(), subvol,
        })
    }).collect())
}

fn parse_fs_type(s: &str) -> FsType {
    match s {
        "ext4"               => FsType::Ext4,
        "btrfs"              => FsType::Btrfs,
        "overlay"|"overlayfs"=> FsType::Overlay,
        "tmpfs"              => FsType::Tmpfs,
        "zfs"                => FsType::Zfs,
        "f2fs"               => FsType::F2fs,
        "xfs"                => FsType::Xfs,
        other                => FsType::Other(other.to_string()),
    }
}

pub fn detect_root_fs_type() -> Result<FsType> {
    let mounts = parse_mounts()?;
    for target in &["/usr", "/"] {
        if let Some(m) = mounts.iter().find(|m| m.path == *target) {
            return Ok(m.fs_type.clone());
        }
    }
    Ok(FsType::Ext4)
}

fn path_is_ro(path: &str, mounts: &[MountPoint]) -> bool {
    mounts.iter()
        .filter(|m| path == m.path || path.starts_with(&format!("{}/", m.path)))
        .max_by_key(|m| m.path.len())
        .map(|m| m.is_ro)
        .unwrap_or(false)
}

fn choose_backend(fs: &FsType) -> ImmutableBackend {
    match fs {
        FsType::Btrfs => ImmutableBackend::BtrfsSubvol,
        FsType::Zfs   => ImmutableBackend::ZfsReadonly,
        _             => ImmutableBackend::RemountRo,
    }
}

fn saved_backend() -> ImmutableBackend {
    std::fs::read_to_string(IMMUTABLE_BACKEND_FILE)
        .map(|s| ImmutableBackend::from_str(s.trim()))
        .unwrap_or(ImmutableBackend::RemountRo)
}

fn should_be_ro(path: &str) -> bool { !is_always_rw(path) }
fn is_always_rw(path: &str) -> bool {
    RW_ALWAYS.iter().any(|rw| path == *rw || path.starts_with(&format!("{}/", rw)))
}
fn ensure_root(ctx: &str) -> Result<()> {
    if nix::unistd::geteuid().as_raw() != 0 { bail!("{} requires root.", ctx); }
    Ok(())
}
