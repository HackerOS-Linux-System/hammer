use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::log;
use crate::store::{StoreEntry, ACTIVE_LINK, PROFILES_DIR};

pub const GENERATIONS_FILE: &str = "/hammer/db/generations.json";
pub const PENDING_LINK:     &str = "/hammer/pending";
pub const ACTIVATION_LOG:   &str = "/hammer/db/activation.log";

// ─────────────────────────────────────────────────────────────
//  Generation metadata
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum GenState { Active, Pending, Previous, Old }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Generation {
    pub number:    u32,
    pub timestamp: DateTime<Utc>,
    pub packages:  Vec<GenPackage>,
    pub note:      Option<String>,
    #[serde(default)]
    pub state:     Option<GenState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenPackage {
    pub name:       String,
    pub version:    String,
    pub store_hash: String,
}

impl Generation {
    pub fn profile_path(&self) -> PathBuf {
        PathBuf::from(PROFILES_DIR).join(format!("gen-{}", self.number))
    }
    pub fn package_count(&self) -> usize { self.packages.len() }
}

// ─────────────────────────────────────────────────────────────
//  GenerationsDb
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct GenerationsDb {
    pub generations: Vec<Generation>,
    pub current:     u32,
    pub pending:     Option<u32>,
}

impl GenerationsDb {
    pub fn load() -> Result<Self> {
        let path = Path::new(GENERATIONS_FILE);
        if !path.exists() { return Ok(GenerationsDb::default()); }
        let txt = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&txt)?)
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all("/hammer/db")?;
        let txt = serde_json::to_string_pretty(self)?;
        let tmp = format!("{}.tmp", GENERATIONS_FILE);
        std::fs::write(&tmp, &txt)?;
        std::fs::rename(&tmp, GENERATIONS_FILE)?;
        Ok(())
    }

    pub fn current_gen(&self)  -> Option<&Generation> { self.generations.iter().find(|g| g.number == self.current) }
    pub fn pending_gen(&self)  -> Option<&Generation> { self.pending.and_then(|p| self.generations.iter().find(|g| g.number == p)) }
    pub fn next_number(&self)  -> u32 { self.generations.iter().map(|g| g.number).max().unwrap_or(0) + 1 }
    pub fn get(&self, n: u32)  -> Option<&Generation> { self.generations.iter().find(|g| g.number == n) }
    pub fn has_pending(&self)  -> bool { self.pending.is_some() }

    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() { return Ok(Self::default()); }
        let txt = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&txt)?)
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
        let txt = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &txt)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────
//  Profile composer
// ─────────────────────────────────────────────────────────────

pub fn compose_profile(gen_number: u32, entries: &[StoreEntry], note: Option<String>) -> Result<Generation> {
    let profile_path = PathBuf::from(PROFILES_DIR).join(format!("gen-{}", gen_number));
    log::info(&format!("profile: composing gen-{} ({} packages)", gen_number, entries.len()));
    std::fs::create_dir_all(&profile_path)?;

    for entry in entries {
        compose_entry(&profile_path, entry)
            .with_context(|| format!("Composing {} into gen-{}", entry.name, gen_number))?;
    }

    Ok(Generation {
        number:    gen_number,
        timestamp: Utc::now(),
        packages:  entries.iter().map(|e| GenPackage {
            name: e.name.clone(), version: e.version.clone(), store_hash: e.hash.clone(),
        }).collect(),
        note,
        state: Some(GenState::Pending),
    })
}

fn compose_entry(profile: &Path, entry: &StoreEntry) -> Result<()> {
    use walkdir::WalkDir;
    if !entry.path.exists() {
        anyhow::bail!("Store entry missing: {}", entry.path.display());
    }
    for item in WalkDir::new(&entry.path).min_depth(1) {
        let item = item?;
        let rel  = item.path().strip_prefix(&entry.path)?;
        let dest = profile.join(rel);
        if item.file_type().is_dir() {
            std::fs::create_dir_all(&dest)?;
        } else {
            if let Some(p) = dest.parent() { std::fs::create_dir_all(p)?; }
            if dest.symlink_metadata().is_ok() { std::fs::remove_file(&dest)?; }
            std::os::unix::fs::symlink(item.path(), &dest)
                .with_context(|| format!("symlink {:?} → {:?}", dest, item.path()))?;
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Pending
// ─────────────────────────────────────────────────────────────

pub fn set_pending(gen: &Generation) -> Result<()> {
    let profile_path = gen.profile_path();
    if !profile_path.exists() {
        anyhow::bail!("Profile path does not exist: {}", profile_path.display());
    }
    let tmp = format!("{}.tmp", PENDING_LINK);
    if Path::new(&tmp).symlink_metadata().is_ok() { std::fs::remove_file(&tmp)?; }
    std::os::unix::fs::symlink(&profile_path, &tmp)?;
    std::fs::rename(&tmp, PENDING_LINK).context("Cannot update /hammer/pending symlink")?;
    log::info(&format!("profile: pending = gen-{}", gen.number));
    install_activate_service()?;
    Ok(())
}

pub fn clear_pending() -> Result<()> {
    let pending = Path::new(PENDING_LINK);
    if pending.symlink_metadata().is_ok() { std::fs::remove_file(pending)?; }
    log::info("profile: pending cleared");
    Ok(())
}

pub fn read_pending_gen() -> Option<u32> {
    let target = std::fs::read_link(PENDING_LINK).ok()?;
    let name   = target.file_name()?.to_string_lossy();
    name.strip_prefix("gen-")?.parse().ok()
}

// ─────────────────────────────────────────────────────────────
//  Boot-time activation
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct ActivationResult {
    pub generation:           u32,
    pub nothing_to_do:        bool,
    pub etc_merged:           usize,
    pub etc_conflicts:        Vec<String>,
    pub ldconfig_ran:         bool,
    pub units_installed:      Vec<String>,
    pub scripts_ran:          Vec<String>,
    pub scripts_failed:       Vec<String>,
    pub users_created:        Vec<String>,
    pub alternatives_updated: usize,
    pub warnings:             Vec<String>,
    pub bins_linked:          usize,
    pub bins_unlinked:        usize,
}

pub fn activate_pending() -> Result<ActivationResult> {
    let mut result = ActivationResult::default();

    let pending_num = match read_pending_gen() {
        Some(n) => n,
        None => {
            log::info("activate: no pending generation");
            result.nothing_to_do = true;
            return Ok(result);
        }
    };

    let gens_db = GenerationsDb::load()?;
    let gen = gens_db.get(pending_num)
        .ok_or_else(|| anyhow::anyhow!("Pending gen-{} not in DB", pending_num))?
        .clone();

    let old_num      = gens_db.current;
    let profile_path = gen.profile_path();
    result.generation = pending_num;

    log::info(&format!("activate: {} → {}", old_num, pending_num));

    switch_active_atomic(&profile_path)?;

    match merge_etc(&profile_path) {
        Ok((n, conflicts)) => { result.etc_merged = n; result.etc_conflicts = conflicts; }
        Err(e) => result.warnings.push(format!("etc-merge: {}", e)),
    }

    match run_ldconfig(&profile_path) {
        Ok(())  => result.ldconfig_ran = true,
        Err(e)  => result.warnings.push(format!("ldconfig: {}", e)),
    }

    // Bin linking must happen BEFORE postinst so scripts can find tools
    match link_bins_to_system(&profile_path, gens_db.current) {
        Ok((linked, unlinked)) => { result.bins_linked = linked; result.bins_unlinked = unlinked; }
        Err(e) => result.warnings.push(format!("bin-link: {}", e)),
    }

    // Create vim wrapper BEFORE running postinst
    create_editor_wrappers(&profile_path);

    match sync_systemd_units(&profile_path) {
        Ok(units) => result.units_installed = units,
        Err(e)    => result.warnings.push(format!("systemd-sync: {}", e)),
    }
    match ensure_system_users() {
        Ok(users) => result.users_created = users,
        Err(e)    => result.warnings.push(format!("users: {}", e)),
    }
    match run_postinst_scripts(&profile_path, &gen) {
        Ok((ran, failed)) => { result.scripts_ran = ran; result.scripts_failed = failed; }
        Err(e) => result.warnings.push(format!("postinst: {}", e)),
    }
    match systemd_reload_and_enable(&result.units_installed) {
        Ok(())  => {}
        Err(e)  => result.warnings.push(format!("daemon-reload: {}", e)),
    }
    match update_alternatives(&profile_path) {
        Ok(n)  => result.alternatives_updated = n,
        Err(e) => result.warnings.push(format!("alternatives: {}", e)),
    }

    // Second pass: re-link after alternatives ran (picks up new symlinks)
    match link_bins_to_system(&profile_path, gens_db.current) {
        Ok((linked, unlinked)) => {
            result.bins_linked   = result.bins_linked.max(linked);
            result.bins_unlinked = result.bins_unlinked.max(unlinked);
        }
        Err(e) => result.warnings.push(format!("bin-link-2: {}", e)),
    }

    // Final: make sure vim/vi/editor are in PATH
    create_editor_wrappers(&profile_path);

    let mut gens_db = GenerationsDb::load()?;
    gens_db.current = pending_num;
    gens_db.pending = None;
    for g in &mut gens_db.generations {
        if g.number == old_num     { g.state = Some(GenState::Previous); }
        if g.number == pending_num { g.state = Some(GenState::Active);   }
    }
    gens_db.save()?;
    clear_pending()?;

    write_activation_log(&result);
    log::info(&format!("activate: gen-{} is now active", pending_num));
    Ok(result)
}

// ─────────────────────────────────────────────────────────────
//  Editor wrappers
//
//  Debian vim package installs vim.basic, vim.tiny, etc. but the
//  "vim" binary is created by update-alternatives which may fail
//  in our environment.  We create wrappers manually.
// ─────────────────────────────────────────────────────────────

fn create_editor_wrappers(profile: &Path) {
    // Map of "canonical name" -> list of alternatives in preference order
    let editor_map: &[(&str, &[&str])] = &[
        ("vim",    &["vim.basic", "vim.nox", "vim.tiny", "vim.athena", "vim.motif"]),
        ("vi",     &["vim.basic", "vim.tiny", "vi.basic"]),
        ("editor", &["vim.basic", "vim.tiny", "nano", "vi"]),
        ("view",   &["vim.basic", "vim.tiny"]),
        ("vimdiff",&["vim.basic"]),
    ];

    let bin_dir   = profile.join("usr/bin");
    let local_bin = Path::new("/usr/local/bin");
    let _ = std::fs::create_dir_all(local_bin);

    for (canonical, alternatives) in editor_map {
        // Find the first alternative that exists in the profile
        let mut found: Option<PathBuf> = None;
        for alt in *alternatives {
            let p = bin_dir.join(alt);
            if p.exists() || p.symlink_metadata().is_ok() {
                found = Some(p);
                break;
            }
        }

        let target_path = match found {
            Some(p) => p,
            None    => continue,
        };

        // Create /usr/local/bin/<canonical> → /hammer/active/usr/bin/<alternative>
        let active_rel = target_path.strip_prefix(profile)
            .map(|r| PathBuf::from("/hammer/active").join(r))
            .unwrap_or(target_path.clone());

        let dest = local_bin.join(canonical);

        // Remove stale symlink if it points elsewhere
        if let Ok(existing) = std::fs::read_link(&dest) {
            if existing == active_rel { continue; } // already correct
            std::fs::remove_file(&dest).ok();
        } else if dest.exists() {
            continue; // real system file, don't touch
        }

        match std::os::unix::fs::symlink(&active_rel, &dest) {
            Ok(()) => log::info(&format!("editor-wrap: {} → {}", dest.display(), active_rel.display())),
            Err(e) => log::warn(&format!("editor-wrap: cannot create {}: {}", dest.display(), e)),
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  hammer-activate.service
// ─────────────────────────────────────────────────────────────

pub fn install_activate_service() -> Result<()> {
    std::fs::create_dir_all("/etc/systemd/system")?;

    // Ensure /usr/bin/hammer symlink exists
    let hammer_bin = find_hammer_binary()?;
    let symlink_target = Path::new("/usr/bin/hammer");
    if let Ok(existing) = std::fs::read_link(symlink_target) {
        if existing != hammer_bin {
            std::fs::remove_file(symlink_target).ok();
            std::os::unix::fs::symlink(&hammer_bin, symlink_target).ok();
        }
    } else if !symlink_target.exists() {
        std::os::unix::fs::symlink(&hammer_bin, symlink_target).ok();
    }

    let service_content = r#"[Unit]
Description=Hammer — Activate Pending Generation
Documentation=man:hammer(1)
DefaultDependencies=no
After=local-fs.target
Before=sysinit.target basic.target
ConditionPathExists=/hammer/pending

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/bin/hammer _activate
StandardOutput=journal+console
StandardError=journal+console
TimeoutStartSec=300

[Install]
WantedBy=sysinit.target
"#;

    let path = Path::new("/etc/systemd/system/hammer-activate.service");
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    if existing != service_content {
        std::fs::write(path, service_content)?;
        let _ = std::process::Command::new("systemctl").args(["daemon-reload"]).status();
        let _ = std::process::Command::new("systemctl")
            .args(["enable", "hammer-activate.service"]).status();
        log::info("profile: installed/updated hammer-activate.service");
    }
    Ok(())
}

fn find_hammer_binary() -> Result<PathBuf> {
    if let Ok(p) = std::fs::read_link("/proc/self/exe") {
        if p.exists() { return Ok(p); }
    }
    for candidate in &["/usr/bin/hammer", "/usr/local/bin/hammer"] {
        let p = Path::new(candidate);
        if p.exists() { return Ok(p.to_path_buf()); }
    }
    if let Ok(exe) = std::env::current_exe() { return Ok(exe); }
    anyhow::bail!("Cannot determine hammer binary path")
}

// ─────────────────────────────────────────────────────────────
//  /etc merge
// ─────────────────────────────────────────────────────────────

fn merge_etc(profile: &Path) -> Result<(usize, Vec<String>)> {
    let profile_etc = profile.join("etc");
    if !profile_etc.exists() { return Ok((0, vec![])); }
    let mut merged = 0usize;
    let mut conflicts = Vec::new();

    for entry in walkdir::WalkDir::new(&profile_etc).min_depth(1) {
        let entry = entry?;
        if entry.file_type().is_dir() { continue; }
        let rel      = entry.path().strip_prefix(&profile_etc)?;
        let sys_path = Path::new("/etc").join(rel);
        if let Some(parent) = sys_path.parent() { std::fs::create_dir_all(parent)?; }
        let src_content = match std::fs::read(entry.path()) { Ok(c) => c, Err(_) => continue };

        if !sys_path.exists() {
            std::fs::write(&sys_path, &src_content)?;
            log::file_op("etc-install", &sys_path.to_string_lossy());
            merged += 1;
        } else {
            let existing = std::fs::read(&sys_path).unwrap_or_default();
            if existing != src_content {
                let new_path = sys_path.parent().unwrap_or(Path::new("/etc"))
                    .join(format!("{}.hammer-new",
                        sys_path.file_name().unwrap_or_default().to_string_lossy()));
                std::fs::write(&new_path, &src_content)?;
                let s = sys_path.to_string_lossy().to_string();
                log::file_op("etc-conflict", &s);
                conflicts.push(s);
            }
        }
    }
    Ok((merged, conflicts))
}

// ─────────────────────────────────────────────────────────────
//  ldconfig
// ─────────────────────────────────────────────────────────────

fn run_ldconfig(profile: &Path) -> Result<()> {
    let lib_dirs = [
        profile.join("usr/lib"),
        profile.join("usr/lib64"),
        profile.join("usr/lib/x86_64-linux-gnu"),
        profile.join("usr/lib/aarch64-linux-gnu"),
        profile.join("lib"),
        profile.join("lib64"),
    ];
    let mut conf = String::new();
    for dir in &lib_dirs {
        if dir.exists() { conf.push_str(&format!("{}\n", dir.display())); }
    }
    if !conf.is_empty() {
        std::fs::create_dir_all("/etc/ld.so.conf.d")?;
        std::fs::write("/etc/ld.so.conf.d/hammer.conf", &conf)?;
    }
    let status = std::process::Command::new("ldconfig").status().context("ldconfig not found")?;
    if !status.success() { anyhow::bail!("ldconfig failed"); }
    log::info("activate: ldconfig ok");
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  systemd units
// ─────────────────────────────────────────────────────────────

fn sync_systemd_units(profile: &Path) -> Result<Vec<String>> {
    let unit_dirs = [
        profile.join("lib/systemd/system"),
        profile.join("usr/lib/systemd/system"),
        profile.join("usr/share/systemd"),
    ];
    let dest_dir = Path::new("/etc/systemd/system");
    std::fs::create_dir_all(dest_dir)?;
    let mut installed = Vec::new();
    let valid_exts = ["service", "socket", "timer", "target", "mount", "path", "slice"];

    for unit_dir in &unit_dirs {
        if !unit_dir.exists() { continue; }
        for entry in std::fs::read_dir(unit_dir)? {
            let entry = entry?;
            let path  = entry.path();
            let name  = entry.file_name().to_string_lossy().to_string();
            let ext   = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !valid_exts.contains(&ext) { continue; }
            let content  = std::fs::read(&path).unwrap_or_default();
            let dest     = dest_dir.join(&name);
            let existing = std::fs::read(&dest).unwrap_or_default();
            if existing != content {
                std::fs::write(&dest, &content)?;
                log::file_op("unit-install", &name);
                installed.push(name);
            }
        }
    }
    Ok(installed)
}

fn systemd_reload_and_enable(units: &[String]) -> Result<()> {
    if units.is_empty() { return Ok(()); }
    let _ = std::process::Command::new("systemctl").arg("daemon-reload").status();
    for unit in units {
        let content = std::fs::read_to_string(format!("/etc/systemd/system/{}", unit)).unwrap_or_default();
        if content.contains("[Install]") && content.contains("WantedBy=") {
            let _ = std::process::Command::new("systemctl").args(["enable", "--no-reload", unit]).status();
            log::info(&format!("activate: enabled {}", unit));
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  postinst scripts
//
//  FIX: libc6 postinst contains bash-specific syntax (function
//  declarations with `func()`) that fails when run by /bin/sh
//  on some systems. We detect the interpreter line and use bash
//  if needed. Also, vim postinst calls update-alternatives with
//  paths that may not exist yet — we skip those errors.
// ─────────────────────────────────────────────────────────────

fn run_postinst_scripts(profile: &Path, gen: &Generation) -> Result<(Vec<String>, Vec<String>)> {
    let scripts_dir = Path::new("/hammer/db/postinst");
    if !scripts_dir.exists() { return Ok((vec![], vec![])); }

    let mut ran    = Vec::new();
    let mut failed = Vec::new();

    // Build PATH that includes the profile's bin dirs + system dirs
    let profile_path_env = format!(
        "{}:{}:{}:/usr/bin:/usr/sbin:/bin:/sbin:/usr/local/bin:/usr/local/sbin",
        profile.join("usr/bin").display(),
        profile.join("usr/sbin").display(),
        profile.join("bin").display(),
    );

    // Packages whose postinst we SKIP entirely — they configure
    // low-level system state that's already correct in our environment.
    const SKIP_POSTINST: &[&str] = &[
        "libc6",           // triggers ldconfig/locale-gen, bash-syntax issues
        "libc-bin",        // same
        "locales",         // runs locale-gen which is slow + unnecessary
        "tzdata",          // runs dpkg-reconfigure
        "initramfs-tools", // runs update-initramfs
        "linux-image",     // runs update-grub etc
        "grub-pc",
        "grub-efi-amd64",
        "grub2-common",
    ];

    for pkg in &gen.packages {
        // Skip problematic postinsts
        if SKIP_POSTINST.contains(&pkg.name.as_str()) {
            log::info(&format!("activate: skipping postinst for {} (known-safe skip)", pkg.name));
            ran.push(pkg.name.clone()); // count as "ran OK"
            continue;
        }

        let script_path = scripts_dir.join(format!("{}.postinst", pkg.name));
        if !script_path.exists() { continue; }
        let content = std::fs::read_to_string(&script_path).unwrap_or_default();
        if content.trim().is_empty() { continue; }

        let patched = patch_maintainer_script(&content);
        let tmp     = format!("/tmp/hammer-postinst-{}", pkg.name);
        std::fs::write(&tmp, &patched)?;
        let mut perms = std::fs::metadata(&tmp)?.permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&tmp, perms)?;

        // Choose interpreter: prefer bash if script uses bash features
        let interpreter = if content.contains("#!/bin/bash") || content.contains("#!/usr/bin/bash")
            || content.contains("function ") || content.contains("local ")
        {
            "bash"
        } else {
            "sh"
        };

        log::info(&format!("activate: running postinst for {} ({})", pkg.name, interpreter));

        let status = std::process::Command::new(interpreter)
            .arg(&tmp)
            .arg("configure")
            .arg("")
            .env("DPKG_MAINTSCRIPT_PACKAGE",    &pkg.name)
            .env("DPKG_RUNNING_VERSION",        "1.23.5")
            .env("DEBIAN_FRONTEND",             "noninteractive")
            .env("DEBCONF_NONINTERACTIVE_SEEN", "true")
            .env("DPKG_NO_TSTP",                "1")
            .env("PATH",                        &profile_path_env)
            .env("DPKG",                        "/bin/true")
            .env("APT_GET",                     "/bin/true")
            // Tell update-alternatives where to look
            .env("ADMINDIR",                    "/var/lib/dpkg")
            .status();

        let _ = std::fs::remove_file(&tmp);

        match status {
            Ok(s) if s.success() => { log::info(&format!("activate: postinst {} ok", pkg.name)); ran.push(pkg.name.clone()); }
            Ok(s) => { log::warn(&format!("activate: postinst {} exited {:?}", pkg.name, s.code())); failed.push(pkg.name.clone()); }
            Err(e) => { log::warn(&format!("activate: postinst {} exec error: {}", pkg.name, e)); failed.push(pkg.name.clone()); }
        }
    }
    Ok((ran, failed))
}

fn patch_maintainer_script(script: &str) -> String {
    // These tokens in a line cause the whole line to be neutralised
    const NEUTRALISE_CONTAINS: &[&str] = &[
        "dpkg --configure",
        "dpkg-reconfigure",
        "apt-get",
        "apt ",
        "update-rc.d",
        "invoke-rc.d",
        "deb-systemd-invoke",
        "deb-systemd-helper",
        "dpkg-maintscript-helper",
        "dpkg-trigger",
        "update-initramfs",
        "update-grub",
        "grub-install",
        "locale-gen",
        "dpkg-statoverride",
    ];

    // These cause neutralisation only if the line STARTS with them
    const NEUTRALISE_STARTS: &[&str] = &[
        "run_ldconfig",
    ];

    let mut out = String::with_capacity(script.len() + 64);
    for line in script.lines() {
        let trimmed = line.trim();
        let neutralise =
            !trimmed.starts_with('#') && (
                NEUTRALISE_CONTAINS.iter().any(|pat| trimmed.contains(pat)) ||
                NEUTRALISE_STARTS.iter().any(|pat| trimmed.starts_with(pat))
            );
        if neutralise {
            out.push_str(&format!("# HAMMER-NEUTRALISED: {}\ntrue\n", line));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

pub fn patch_user_script(script: &str) -> String {
    const NEUTRALISE: &[&str] = &[
        "dpkg", "apt", "update-rc.d", "invoke-rc.d",
        "deb-systemd", "systemctl", "adduser", "useradd",
        "groupadd", "addgroup", "ldconfig", "update-alternatives",
    ];
    let mut out = String::new();
    for line in script.lines() {
        let t = line.trim();
        let neutralise = !t.starts_with('#') && NEUTRALISE.iter().any(|p| t.contains(p));
        if neutralise { out.push_str(&format!("# HAMMER-USER-NEUTRALISED: {}\ntrue\n", line)); }
        else          { out.push_str(line); out.push('\n'); }
    }
    out
}

// ─────────────────────────────────────────────────────────────
//  System users / groups
// ─────────────────────────────────────────────────────────────

fn ensure_system_users() -> Result<Vec<String>> {
    let scripts_dir = Path::new("/hammer/db/postinst");
    if !scripts_dir.exists() { return Ok(vec![]); }
    let mut created = Vec::new();
    for entry in std::fs::read_dir(scripts_dir)? {
        let entry = entry?;
        let fname = entry.file_name().to_string_lossy().to_string();
        if !fname.ends_with(".postinst") { continue; }
        let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
        for line in content.lines() {
            let t = line.trim();
            if t.starts_with('#') { continue; }
            if (t.contains("adduser") || t.contains("useradd")) && t.contains("--system") {
                if let Some(user) = extract_last_non_flag(t, &["adduser", "useradd"]) {
                    if !user_exists(&user) {
                        if create_system_user(&user, t).is_ok() {
                            created.push(user.clone());
                            log::info(&format!("activate: created user '{}'", user));
                        }
                    }
                }
            }
            if t.contains("groupadd") || (t.contains("addgroup") && t.contains("--system")) {
                if let Some(grp) = extract_last_non_flag(t, &["groupadd", "addgroup"]) {
                    if !group_exists(&grp) {
                        let _ = std::process::Command::new("groupadd").args(["--system", &grp]).status();
                        log::info(&format!("activate: created group '{}'", grp));
                    }
                }
            }
        }
    }
    Ok(created)
}

fn extract_last_non_flag(line: &str, skip: &[&str]) -> Option<String> {
    line.split_whitespace()
        .filter(|t| !t.starts_with('-') && !skip.contains(t))
        .last().map(|s| s.to_string())
}
fn user_exists(name: &str) -> bool {
    std::process::Command::new("id").arg(name).output()
        .map(|o| o.status.success()).unwrap_or(false)
}
fn group_exists(name: &str) -> bool {
    std::fs::read_to_string("/etc/group").unwrap_or_default()
        .lines().any(|l| l.starts_with(&format!("{}:", name)))
}
fn create_system_user(user: &str, original: &str) -> Result<()> {
    let tokens: Vec<&str> = original.split_whitespace().collect();
    let mut args = vec!["--system".to_string(), "--no-create-home".to_string()];
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i] {
            "--home"|"--home-dir"|"-d"|"--shell"|"-s"|"--gecos"|"-c" if i+1<tokens.len() => {
                args.push(tokens[i].to_string()); args.push(tokens[i+1].to_string()); i += 2;
            }
            "--group"|"--ingroup"|"--disabled-password" => { args.push(tokens[i].to_string()); i += 1; }
            _ => { i += 1; }
        }
    }
    args.push(user.to_string());
    let ok = std::process::Command::new("adduser").args(&args).status()
        .map(|s| s.success()).unwrap_or(false);
    if !ok {
        let status = std::process::Command::new("useradd").args(["-r", "-M", user]).status()?;
        if !status.success() { anyhow::bail!("Could not create user '{}'", user); }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  update-alternatives
//
//  FIX: Instead of just copying /etc/alternatives symlinks,
//  we now ALSO run update-alternatives --install properly
//  for known editors, so that `vim` appears as a command.
// ─────────────────────────────────────────────────────────────

fn update_alternatives(profile: &Path) -> Result<usize> {
    let mut count = 0usize;

    // 1. Copy /etc/alternatives symlinks from profile (existing behaviour)
    let alt_dir = profile.join("etc/alternatives");
    if alt_dir.exists() {
        let sys_alt = Path::new("/etc/alternatives");
        std::fs::create_dir_all(sys_alt)?;
        for entry in std::fs::read_dir(&alt_dir)? {
            let entry  = entry?;
            let name   = entry.file_name().to_string_lossy().to_string();
            let target = match std::fs::read_link(entry.path()) { Ok(t) => t, Err(_) => continue };
            let dest   = sys_alt.join(&name);
            if dest.symlink_metadata().is_ok() { std::fs::remove_file(&dest)?; }
            std::os::unix::fs::symlink(&target, &dest)?;
            count += 1;
        }
    }

    // 2. Run update-alternatives --install for editors found in profile
    let editor_alts: &[(&str, &str, u32)] = &[
        // (link,              name,    priority)
        ("/usr/bin/vim",       "vim",    100),
        ("/usr/bin/vi",        "vi",     100),
        ("/usr/bin/editor",    "editor", 100),
    ];

    let bin_dir = profile.join("usr/bin");

    // Prefer vim.basic, fallback to vim.tiny
    let vim_candidates = ["vim.basic", "vim.nox", "vim.tiny"];
    let mut vim_path: Option<PathBuf> = None;
    for cand in &vim_candidates {
        let p = bin_dir.join(cand);
        if p.exists() || p.symlink_metadata().is_ok() {
            vim_path = Some(p);
            break;
        }
    }

    if let Some(ref vpath) = vim_path {
        for &(link, name, priority) in editor_alts {
            // Only register if the link doesn't already point to the right thing
            let active_target = vpath.strip_prefix(profile)
                .map(|r| PathBuf::from("/hammer/active").join(r))
                .unwrap_or(vpath.clone());

            let _ = std::process::Command::new("update-alternatives")
                .args([
                    "--install",
                    link,
                    name,
                    &active_target.to_string_lossy(),
                    &priority.to_string(),
                ])
                .status();

            let _ = std::process::Command::new("update-alternatives")
                .args(["--set", name, &active_target.to_string_lossy()])
                .status();

            count += 1;
        }
        log::info(&format!("alternatives: registered vim editors → {}", vpath.display()));
    }

    // 3. Standard auto-update for other tools
    for name in &["awk", "python3", "pager", "x-www-browser"] {
        let _ = std::process::Command::new("update-alternatives").args(["--auto", name]).status();
    }

    Ok(count)
}

// ─────────────────────────────────────────────────────────────
//  Bin linking
//
//  FIX: We now walk the profile usr/bin and create symlinks in
//  BOTH /usr/local/bin AND /usr/bin (if the file doesn't exist
//  there already as a real file).  This ensures packages are
//  visible in PATH regardless of whether /usr/local/bin is in PATH.
//
//  Priority:
//    1. /usr/local/bin  (always try)
//    2. /usr/bin        (only if slot is empty — never overwrite system files)
// ─────────────────────────────────────────────────────────────

fn link_bins_to_system(profile: &Path, _old_gen: u32) -> Result<(usize, usize)> {
    // (profile subdir,      primary dest,         secondary dest if slot free)
    let bin_pairs: &[(PathBuf, PathBuf, Option<PathBuf>)] = &[
        (
            profile.join("usr/bin"),
            PathBuf::from("/usr/local/bin"),
            Some(PathBuf::from("/usr/bin")),
        ),
        (
            profile.join("usr/sbin"),
            PathBuf::from("/usr/local/sbin"),
            Some(PathBuf::from("/usr/sbin")),
        ),
        (
            profile.join("bin"),
            PathBuf::from("/usr/local/bin"),
            Some(PathBuf::from("/usr/bin")),
        ),
        (
            profile.join("sbin"),
            PathBuf::from("/usr/local/sbin"),
            Some(PathBuf::from("/usr/sbin")),
        ),
        (
            profile.join("usr/games"),
            PathBuf::from("/usr/local/games"),
            None,
        ),
    ];

    let mut unlinked = 0usize;
    let mut linked   = 0usize;

    // Step 1: remove stale hammer symlinks
    let all_dest_dirs = [
        "/usr/local/bin", "/usr/local/sbin", "/usr/local/games",
        // We only remove from /usr/bin if the symlink points into /hammer/
        "/usr/bin", "/usr/sbin",
    ];
    for dir_str in &all_dest_dirs {
        let dir = Path::new(dir_str);
        if !dir.exists() { continue; }
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if let Ok(target) = std::fs::read_link(&path) {
                let t = target.to_string_lossy();
                if t.contains("/hammer/active") || t.contains("/hammer/profiles") {
                    std::fs::remove_file(&path).ok();
                    unlinked += 1;
                }
            }
        }
    }

    // Step 2: link new binaries
    for (src_dir, primary_dest, secondary_dest) in bin_pairs {
        if !src_dir.exists() { continue; }
        std::fs::create_dir_all(primary_dest)?;

        for entry in std::fs::read_dir(src_dir).into_iter().flatten().flatten() {
            let src_path  = entry.path();
            let file_name = match src_path.file_name() { Some(n) => n.to_owned(), None => continue };

            let active_rel = src_path.strip_prefix(profile)
                .map(|r| PathBuf::from("/hammer/active").join(r))
                .unwrap_or_else(|_| src_path.clone());

            // Try primary dest (/usr/local/bin)
            let primary_path = primary_dest.join(&file_name);
            if link_one(&primary_path, &active_rel) { linked += 1; }

            // Try secondary dest (/usr/bin) only if slot is free
            if let Some(sec) = secondary_dest {
                std::fs::create_dir_all(sec).ok();
                let sec_path = sec.join(&file_name);
                // Only link if /usr/bin/<name> does not exist as a real file
                if !sec_path.exists() || sec_path.symlink_metadata()
                    .map(|m| m.file_type().is_symlink()).unwrap_or(false)
                {
                    if link_one(&sec_path, &active_rel) { linked += 1; }
                }
            }
        }
    }

    log::info(&format!("bin-link: linked={} unlinked={}", linked, unlinked));
    Ok((linked, unlinked))
}

/// Create one symlink dest → target. Returns true if created/updated.
fn link_one(dest: &Path, target: &Path) -> bool {
    if let Ok(existing) = std::fs::read_link(dest) {
        if existing == target { return true; } // already correct
        std::fs::remove_file(dest).ok();
    } else if dest.exists() {
        return false; // real system file, don't touch
    }
    match std::os::unix::fs::symlink(target, dest) {
        Ok(()) => {
            log::info(&format!("bin-link: {} → {}", dest.display(), target.display()));
            true
        }
        Err(e) => {
            log::warn(&format!("bin-link: cannot link {}: {}", dest.display(), e));
            false
        }
    }
}

pub fn unlink_all_bins_from_system() -> usize {
    let dirs = ["/usr/local/bin", "/usr/local/sbin", "/usr/local/games", "/usr/bin", "/usr/sbin"];
    let mut count = 0;
    for dir in &dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if let Ok(target) = std::fs::read_link(&path) {
                let t = target.to_string_lossy();
                if t.contains("/hammer/active") || t.contains("/hammer/profiles") {
                    std::fs::remove_file(&path).ok();
                    count += 1;
                }
            }
        }
    }
    count
}

fn switch_active_atomic(profile_path: &Path) -> Result<()> {
    let tmp = format!("{}.tmp", ACTIVE_LINK);
    if Path::new(&tmp).symlink_metadata().is_ok() { std::fs::remove_file(&tmp)?; }
    std::os::unix::fs::symlink(profile_path, &tmp)?;
    std::fs::rename(&tmp, ACTIVE_LINK).context("Atomic active switch failed")?;
    log::info(&format!("profile: active → {}", profile_path.display()));
    Ok(())
}

pub fn switch_active(gen: &Generation) -> Result<()> {
    let profile_path = gen.profile_path();
    switch_active_atomic(&profile_path)?;
    let _ = run_ldconfig(&profile_path);
    match link_bins_to_system(&profile_path, 0) {
        Ok((l, u)) => log::info(&format!("switch_active: bin-link linked={} unlinked={}", l, u)),
        Err(e)     => log::warn(&format!("switch_active: bin-link failed: {}", e)),
    }
    create_editor_wrappers(&profile_path);
    let _ = update_alternatives(&profile_path);
    Ok(())
}

pub fn read_active_gen() -> Option<u32> {
    let target = std::fs::read_link(ACTIVE_LINK).ok()?;
    let name   = target.file_name()?.to_string_lossy();
    name.strip_prefix("gen-")?.parse().ok()
}

pub fn delete_profile(gen: &Generation) -> anyhow::Result<()> {
    let path = gen.profile_path();
    if path.exists() {
        std::fs::remove_dir_all(&path)?;
        log::info(&format!("profile: deleted gen-{}", gen.number));
    }
    Ok(())
}

pub fn relink_bins(profile: &Path) -> Result<(usize, usize)> {
    let r = link_bins_to_system(profile, 0)?;
    create_editor_wrappers(profile);
    Ok(r)
}

// ─────────────────────────────────────────────────────────────
//  Postinst script storage
// ─────────────────────────────────────────────────────────────

pub fn save_postinst(pkg_name: &str, script: &str) -> Result<()> {
    let dir = Path::new("/hammer/db/postinst");
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(format!("{}.postinst", pkg_name)), script)?;
    Ok(())
}

pub fn remove_postinst(pkg_name: &str) {
    let _ = std::fs::remove_file(
        Path::new("/hammer/db/postinst").join(format!("{}.postinst", pkg_name))
    );
}

// ─────────────────────────────────────────────────────────────
//  Activation log
// ─────────────────────────────────────────────────────────────

fn write_activation_log(r: &ActivationResult) {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let mut s = format!("[{}] gen-{} activated\n", now, r.generation);
    s += &format!("  /etc merged:    {}\n", r.etc_merged);
    if !r.etc_conflicts.is_empty() {
        s += &format!("  conflicts:      {} (see *.hammer-new)\n", r.etc_conflicts.len());
    }
    s += &format!("  ldconfig:       {}\n", r.ldconfig_ran);
    s += &format!("  units:          {}\n", r.units_installed.join(", "));
    s += &format!("  postinst ok:    {}\n", r.scripts_ran.join(", "));
    if !r.scripts_failed.is_empty() {
        s += &format!("  postinst FAIL:  {}\n", r.scripts_failed.join(", "));
    }
    s += &format!("  users:          {}\n", r.users_created.join(", "));
    s += &format!("  alternatives:   {}\n", r.alternatives_updated);
    s += &format!("  bins linked:    {}  unlinked: {}\n", r.bins_linked, r.bins_unlinked);
    for w in &r.warnings { s += &format!("  warn: {}\n", w); }
    s += "\n";

    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true).append(true).open(ACTIVATION_LOG)
    {
        let _ = std::io::Write::write_all(&mut f, s.as_bytes());
    }
}
