use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::log;
use crate::store::{StoreEntry, ACTIVE_LINK, PROFILES_DIR};

pub const GENERATIONS_FILE: &str = "/hammer/db/generations.json";
/// Symlink that points to the generation staged for next boot.
pub const PENDING_LINK:     &str = "/hammer/pending";
/// Activation log written at boot by hammer _activate
pub const ACTIVATION_LOG:   &str = "/hammer/db/activation.log";

// ─────────────────────────────────────────────────────────────
//  Generation metadata
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum GenState {
    Active,
    Pending,
    Previous,
    Old,
}

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

    pub fn package_count(&self) -> usize {
        self.packages.len()
    }
}

// ─────────────────────────────────────────────────────────────
//  Generations DB
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct GenerationsDb {
    pub generations: Vec<Generation>,
    /// Currently booted generation
    pub current:     u32,
    /// Generation staged for next boot (None = no pending changes)
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

    pub fn current_gen(&self) -> Option<&Generation> {
        self.generations.iter().find(|g| g.number == self.current)
    }

    pub fn pending_gen(&self) -> Option<&Generation> {
        self.pending.and_then(|p| self.generations.iter().find(|g| g.number == p))
    }

    pub fn next_number(&self) -> u32 {
        self.generations.iter().map(|g| g.number).max().unwrap_or(0) + 1
    }

    pub fn get(&self, n: u32) -> Option<&Generation> {
        self.generations.iter().find(|g| g.number == n)
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }
}

// ─────────────────────────────────────────────────────────────
//  Profile composer
// ─────────────────────────────────────────────────────────────

/// Build a new generation profile from store entries.
/// Pure symlink work — extremely fast, no data is copied.
pub fn compose_profile(
    gen_number: u32,
    entries:    &[StoreEntry],
    note:       Option<String>,
) -> Result<Generation> {
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
           name:       e.name.clone(),
                                     version:    e.version.clone(),
                                     store_hash: e.hash.clone(),
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
//  Pending — stage for next boot
// ─────────────────────────────────────────────────────────────

/// Stage a generation as pending for next boot.
/// Sets /hammer/pending → gen-N profile path.
/// Does NOT touch /hammer/active.
pub fn set_pending(gen: &Generation) -> Result<()> {
    let profile_path = gen.profile_path();
    if !profile_path.exists() {
        anyhow::bail!("Profile path does not exist: {}", profile_path.display());
    }

    // Atomic symlink update: write to .tmp then rename
    let tmp = format!("{}.tmp", PENDING_LINK);
    if Path::new(&tmp).symlink_metadata().is_ok() { std::fs::remove_file(&tmp)?; }
    std::os::unix::fs::symlink(&profile_path, &tmp)?;
    std::fs::rename(&tmp, PENDING_LINK)
    .context("Cannot update /hammer/pending symlink")?;

    log::info(&format!("profile: pending = gen-{}", gen.number));

    // Ensure boot-time activation service is installed
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
//  Boot-time activation  (hammer _activate, called by systemd)
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

/// Full boot-time activation sequence.
/// Called by hammer-activate.service at early boot.
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

    // ── 1. Atomic switch ─────────────────────────────────────
    // This is the single atomic operation — after this, new gen is live
    switch_active_atomic(&profile_path)?;

    // ── 2. /etc merge ────────────────────────────────────────
    match merge_etc(&profile_path) {
        Ok((n, conflicts)) => {
            result.etc_merged    = n;
            result.etc_conflicts = conflicts;
        }
        Err(e) => result.warnings.push(format!("etc-merge: {}", e)),
    }

    // ── 3. ldconfig ──────────────────────────────────────────
    match run_ldconfig(&profile_path) {
        Ok(())  => result.ldconfig_ran = true,
        Err(e)  => result.warnings.push(format!("ldconfig: {}", e)),
    }

    // ── 3b. Link binaries into /usr/local/bin + /usr/local/sbin ─────
    // This is THE fix that makes installed packages actually visible in PATH.
    // We unlink old gen's bins first, then link new gen's bins.
    match link_bins_to_system(&profile_path, gens_db.current) {
        Ok((linked, unlinked)) => {
            result.bins_linked   = linked;
            result.bins_unlinked = unlinked;
        }
        Err(e) => result.warnings.push(format!("bin-link: {}", e)),
    }

    // ── 4. Sync systemd units ────────────────────────────────
    match sync_systemd_units(&profile_path) {
        Ok(units) => result.units_installed = units,
        Err(e)    => result.warnings.push(format!("systemd-sync: {}", e)),
    }

    // ── 5. System users / groups ─────────────────────────────
    match ensure_system_users() {
        Ok(users) => result.users_created = users,
        Err(e)    => result.warnings.push(format!("users: {}", e)),
    }

    // ── 6. postinst scripts ──────────────────────────────────
    match run_postinst_scripts(&profile_path, &gen) {
        Ok((ran, failed)) => {
            result.scripts_ran    = ran;
            result.scripts_failed = failed;
        }
        Err(e) => result.warnings.push(format!("postinst: {}", e)),
    }

    // ── 7. systemd daemon-reload + enable units ───────────────
    match systemd_reload_and_enable(&result.units_installed) {
        Ok(())  => {}
        Err(e)  => result.warnings.push(format!("daemon-reload: {}", e)),
    }

    // ── 8. update-alternatives ───────────────────────────────
    match update_alternatives(&profile_path) {
        Ok(n)  => result.alternatives_updated = n,
        Err(e) => result.warnings.push(format!("alternatives: {}", e)),
    }

    // ── 9. Finalise DB ───────────────────────────────────────
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
//  /etc merge
// ─────────────────────────────────────────────────────────────

/// Merge config files from new profile into /etc.
///
/// Rules (like pacman's .pacnew / Gentoo's etc-update):
///   - File absent from /etc  → copy from profile (no conflict)
///   - File identical          → no-op
///   - File differs            → keep user version, write profile as .hammer-new
///
/// Returns (merged_count, conflict_paths)
fn merge_etc(profile: &Path) -> Result<(usize, Vec<String>)> {
    let profile_etc = profile.join("etc");
    if !profile_etc.exists() { return Ok((0, vec![])); }

    let mut merged    = 0usize;
    let mut conflicts = Vec::new();

    for entry in walkdir::WalkDir::new(&profile_etc).min_depth(1) {
        let entry = entry?;
        let ft    = entry.file_type();
        if ft.is_dir() { continue; }

        let rel      = entry.path().strip_prefix(&profile_etc)?;
        let sys_path = Path::new("/etc").join(rel);

        if let Some(parent) = sys_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Resolve through symlink in store
        let src_content = match std::fs::read(entry.path()) {
            Ok(c) => c,
            Err(_) => continue,
        };

        if !sys_path.exists() {
            std::fs::write(&sys_path, &src_content)?;
            log::file_op("etc-install", &sys_path.to_string_lossy());
            merged += 1;
        } else {
            let existing = std::fs::read(&sys_path).unwrap_or_default();
            if existing != src_content {
                // User config wins — save new version as .hammer-new
                let new_path = {
                    let name = sys_path.file_name()
                    .unwrap_or_default().to_string_lossy();
                    sys_path.parent().unwrap_or(Path::new("/etc"))
                    .join(format!("{}.hammer-new", name))
                };
                std::fs::write(&new_path, &src_content)?;
                let conflict_str = sys_path.to_string_lossy().to_string();
                log::file_op("etc-conflict", &conflict_str);
                conflicts.push(conflict_str);
            }
        }
    }
    Ok((merged, conflicts))
}

// ─────────────────────────────────────────────────────────────
//  ldconfig
// ─────────────────────────────────────────────────────────────

fn run_ldconfig(profile: &Path) -> Result<()> {
    // Write ld.so.conf.d entry for our profile lib dirs
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

    let status = std::process::Command::new("ldconfig")
    .status()
    .context("ldconfig not found")?;

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

    let _ = std::process::Command::new("systemctl")
    .arg("daemon-reload").status();

    for unit in units {
        let unit_path = format!("/etc/systemd/system/{}", unit);
        let content   = std::fs::read_to_string(&unit_path).unwrap_or_default();
        if content.contains("[Install]") && content.contains("WantedBy=") {
            let _ = std::process::Command::new("systemctl")
            .args(["enable", "--no-reload", unit]).status();
            log::info(&format!("activate: enabled {}", unit));
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  postinst scripts
// ─────────────────────────────────────────────────────────────

fn run_postinst_scripts(
    profile: &Path,
    gen:     &Generation,
) -> Result<(Vec<String>, Vec<String>)> {
    let scripts_dir = Path::new("/hammer/db/postinst");
    if !scripts_dir.exists() { return Ok((vec![], vec![])); }

    let mut ran    = Vec::new();
    let mut failed = Vec::new();

    let profile_path = format!(
        "{}:{}:{}:/usr/bin:/usr/sbin:/bin:/sbin",
        profile.join("usr/bin").display(),
                               profile.join("usr/sbin").display(),
                               profile.join("bin").display(),
    );

    for pkg in &gen.packages {
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

        log::info(&format!("activate: running postinst for {}", pkg.name));

        let status = std::process::Command::new(&tmp)
        .args(["configure", ""])
        .env("DPKG_MAINTSCRIPT_PACKAGE",    &pkg.name)
        .env("DPKG_RUNNING_VERSION",        "1.23.5")
        .env("DEBIAN_FRONTEND",             "noninteractive")
        .env("DEBCONF_NONINTERACTIVE_SEEN", "true")
        .env("DPKG_NO_TSTP",                "1")
        .env("PATH",                        &profile_path)
        // Stub out apt/dpkg so scripts don't try to call them
        .env("DPKG",                        "/bin/true")
        .env("APT_GET",                     "/bin/true")
        .status();

        let _ = std::fs::remove_file(&tmp);

        match status {
            Ok(s) if s.success() => {
                log::info(&format!("activate: postinst {} ok", pkg.name));
                ran.push(pkg.name.clone());
            }
            Ok(s) => {
                log::warn(&format!("activate: postinst {} exited {:?}", pkg.name, s.code()));
                failed.push(pkg.name.clone());
            }
            Err(e) => {
                log::warn(&format!("activate: postinst {} exec error: {}", pkg.name, e));
                failed.push(pkg.name.clone());
            }
        }
    }
    Ok((ran, failed))
}

/// Patch a maintainer script: neutralise calls irrelevant in our env.
/// Kept: adduser, useradd, groupadd, ldconfig, fc-cache, update-alternatives
/// Neutralised: dpkg --configure, apt-get, update-rc.d, invoke-rc.d, deb-systemd-*
fn patch_maintainer_script(script: &str) -> String {
    const NEUTRALISE: &[&str] = &[
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
    ];

    let mut out = String::with_capacity(script.len() + 64);
    for line in script.lines() {
        let trimmed = line.trim();
        let neutralise = !trimmed.starts_with('#') &&
        NEUTRALISE.iter().any(|pat| trimmed.contains(pat));

        if neutralise {
            out.push_str(&format!("# HAMMER-NEUTRALISED: {}\ntrue\n", line));
        } else {
            out.push_str(line);
            out.push('\n');
        }
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
        let entry   = entry?;
        let fname   = entry.file_name().to_string_lossy().to_string();
        if !fname.ends_with(".postinst") { continue; }

        let content = std::fs::read_to_string(entry.path()).unwrap_or_default();

        for line in content.lines() {
            let t = line.trim();
            if t.starts_with('#') { continue; }

            if (t.contains("adduser") || t.contains("useradd")) && t.contains("--system") {
                if let Some(user) = extract_last_non_flag(t, &["adduser", "useradd"]) {
                    if !user_exists(&user) {
                        if let Ok(()) = create_system_user(&user, t) {
                            created.push(user.clone());
                            log::info(&format!("activate: created user '{}'", user));
                        }
                    }
                }
            }

            if t.contains("groupadd") || (t.contains("addgroup") && t.contains("--system")) {
                if let Some(grp) = extract_last_non_flag(t, &["groupadd", "addgroup"]) {
                    if !group_exists(&grp) {
                        let _ = std::process::Command::new("groupadd")
                        .args(["--system", &grp]).status();
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
    .last()
    .map(|s| s.to_string())
}

fn user_exists(name: &str) -> bool {
    std::process::Command::new("id").arg(name)
    .output().map(|o| o.status.success()).unwrap_or(false)
}

fn group_exists(name: &str) -> bool {
    std::fs::read_to_string("/etc/group").unwrap_or_default()
    .lines().any(|l| l.starts_with(&format!("{}:", name)))
}

fn create_system_user(user: &str, original: &str) -> Result<()> {
    // Parse safe flags: --home, --shell, --group, --no-create-home, --gecos
    let tokens: Vec<&str> = original.split_whitespace().collect();
    let mut args = vec!["--system".to_string(), "--no-create-home".to_string()];

    let mut i = 0;
    while i < tokens.len() {
        match tokens[i] {
            "--home" | "--home-dir" | "-d" | "--shell" | "-s" | "--gecos" | "-c" if i + 1 < tokens.len() => {
                args.push(tokens[i].to_string());
                args.push(tokens[i + 1].to_string());
                i += 2;
            }
            "--group" | "--ingroup" | "--disabled-password" => {
                args.push(tokens[i].to_string());
                i += 1;
            }
            _ => { i += 1; }
        }
    }
    args.push(user.to_string());

    // Try adduser first (Debian style), fall back to useradd
    let ok = std::process::Command::new("adduser")
    .args(&args).status()
    .map(|s| s.success()).unwrap_or(false);

    if !ok {
        let mut ua_args = vec!["-r".to_string(), "-M".to_string()];
        ua_args.push(user.to_string());
        let status = std::process::Command::new("useradd")
        .args(&ua_args).status()?;
        if !status.success() { anyhow::bail!("Could not create user '{}'", user); }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  update-alternatives
// ─────────────────────────────────────────────────────────────

fn update_alternatives(profile: &Path) -> Result<usize> {
    let alt_dir = profile.join("etc/alternatives");
    if !alt_dir.exists() { return Ok(0); }

    let sys_alt = Path::new("/etc/alternatives");
    std::fs::create_dir_all(sys_alt)?;
    let mut count = 0usize;

    for entry in std::fs::read_dir(&alt_dir)? {
        let entry  = entry?;
        let name   = entry.file_name().to_string_lossy().to_string();
        let target = match std::fs::read_link(entry.path()) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let dest = sys_alt.join(&name);
        if dest.symlink_metadata().is_ok() { std::fs::remove_file(&dest)?; }
        std::os::unix::fs::symlink(&target, &dest)?;
        count += 1;
    }

    // Run update-alternatives --auto for common ones
    for name in &["editor", "x-terminal-emulator", "x-www-browser", "vi", "awk", "python3"] {
        let _ = std::process::Command::new("update-alternatives")
        .args(["--auto", name]).status();
    }

    Ok(count)
}

// ─────────────────────────────────────────────────────────────
//  hammer-activate.service installer
// ─────────────────────────────────────────────────────────────

pub fn install_activate_service() -> Result<()> {
    const SERVICE: &str = r#"[Unit]
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
    std::fs::create_dir_all("/etc/systemd/system")?;

    let existing = std::fs::read_to_string(path).unwrap_or_default();
    if existing != SERVICE {
        std::fs::write(path, SERVICE)?;
        let _ = std::process::Command::new("systemctl").args(["daemon-reload"]).status();
        let _ = std::process::Command::new("systemctl")
        .args(["enable", "hammer-activate.service"]).status();
        log::info("activate: installed hammer-activate.service");
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  postinst script storage
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
    for w in &r.warnings { s += &format!("  warn: {}\n", w); }
    s += "\n";

    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true).append(true).open(ACTIVATION_LOG)
        {
            let _ = std::io::Write::write_all(&mut f, s.as_bytes());
        }
}

// ─────────────────────────────────────────────────────────────
//  Rollback / gen-switch helpers (immediate, no boot needed)
// ─────────────────────────────────────────────────────────────


// ─────────────────────────────────────────────────────────────
//  Bin linking — make hammer packages visible in PATH
//
//  Strategy: create symlinks in /usr/local/bin and /usr/local/sbin
//  pointing to /hammer/active/usr/bin/* etc.
//  These dirs are in PATH by default on every Debian/Ubuntu system.
//
//  We use /hammer/active (the symlink) as target so that after
//  future activations the symlinks stay valid automatically
//  (they point through the symlink, not to a specific gen path).
//
//  On each activation we:
//    1. Remove stale symlinks that point into any /hammer/profiles/ path
//    2. Create new symlinks for all binaries in the new profile
// ─────────────────────────────────────────────────────────────

/// Returns (linked, unlinked) counts
fn link_bins_to_system(profile: &Path, _old_gen: u32) -> Result<(usize, usize)> {
    let bin_pairs = [
        (profile.join("usr/bin"),   std::path::PathBuf::from("/usr/local/bin")),
        (profile.join("usr/sbin"),  std::path::PathBuf::from("/usr/local/sbin")),
        (profile.join("bin"),       std::path::PathBuf::from("/usr/local/bin")),
        (profile.join("sbin"),      std::path::PathBuf::from("/usr/local/sbin")),
        (profile.join("usr/games"), std::path::PathBuf::from("/usr/local/games")),
    ];

    let mut unlinked = 0usize;
    let mut linked   = 0usize;

    // ── Step 1: remove stale hammer symlinks ─────────────────
    for (_, dest_dir) in &bin_pairs {
        if !dest_dir.exists() { continue; }
        for entry in std::fs::read_dir(dest_dir).into_iter().flatten().flatten() {
            let path = entry.path();
            // Only touch symlinks that point into /hammer/
            if let Ok(target) = std::fs::read_link(&path) {
                let t = target.to_string_lossy();
                if t.contains("/hammer/") || t.contains("/hammer/active") {
                    std::fs::remove_file(&path).ok();
                    unlinked += 1;
                }
            }
        }
    }

    // ── Step 2: link new binaries via /hammer/active ─────────
    for (src_dir, dest_dir) in &bin_pairs {
        if !src_dir.exists() { continue; }
        std::fs::create_dir_all(dest_dir)?;

        for entry in std::fs::read_dir(src_dir).into_iter().flatten().flatten() {
            let src_path  = entry.path();
            let file_name = match src_path.file_name() {
                Some(n) => n.to_owned(),
                None    => continue,
            };
            let dest_path = dest_dir.join(&file_name);

            // Target points through /hammer/active (stays valid across gens)
            // Figure out which sub-dir we're in
            let active_rel = src_path.strip_prefix(profile)
            .map(|r| std::path::PathBuf::from("/hammer/active").join(r))
            .unwrap_or_else(|_| src_path.clone());

            // Skip if dest already points to the right place
            if let Ok(existing) = std::fs::read_link(&dest_path) {
                if existing == active_rel {
                    linked += 1;
                    continue;
                }
                std::fs::remove_file(&dest_path).ok();
            } else if dest_path.exists() {
                // Real file from system — don't overwrite
                continue;
            }

            match std::os::unix::fs::symlink(&active_rel, &dest_path) {
                Ok(()) => {
                    log::info(&format!(
                        "bin-link: {} → {}",
                        dest_path.display(), active_rel.display()
                    ));
                    linked += 1;
                }
                Err(e) => {
                    log::warn(&format!(
                        "bin-link: cannot link {}: {}", dest_path.display(), e
                    ));
                }
            }
        }
    }

    log::info(&format!("bin-link: linked={} unlinked={}", linked, unlinked));
    Ok((linked, unlinked))
}

/// Remove all hammer bin symlinks (called during uninstall/gc)
pub fn unlink_all_bins_from_system() -> usize {
    let dirs = ["/usr/local/bin", "/usr/local/sbin", "/usr/local/games"];
    let mut count = 0usize;
    for dir in &dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if let Ok(target) = std::fs::read_link(&path) {
                let t = target.to_string_lossy();
                if t.contains("/hammer/") {
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
    std::fs::rename(&tmp, ACTIVE_LINK)
    .context("Atomic active switch failed")?;
    log::info(&format!("profile: active → {}", profile_path.display()));
    Ok(())
}

/// Immediate switch — for rollback / gen switch only.
/// Triggers mini-activation (ldconfig, alternatives) without reboot.
pub fn switch_active(gen: &Generation) -> Result<()> {
    let profile_path = gen.profile_path();
    switch_active_atomic(&profile_path)?;
    // Fast subset: ldconfig, bin-linking, alternatives
    // (no postinst, no users — those need a full activate_pending)
    let _ = run_ldconfig(&profile_path);
    match link_bins_to_system(&profile_path, 0) {
        Ok((linked, unlinked)) => log::info(&format!(
            "switch_active: bin-link linked={} unlinked={}", linked, unlinked
        )),
        Err(e) => log::warn(&format!("switch_active: bin-link failed: {}", e)),
    }
    let _ = update_alternatives(&profile_path);
    Ok(())
}

pub fn read_active_gen() -> Option<u32> {
    let target = std::fs::read_link(ACTIVE_LINK).ok()?;
    let name   = target.file_name()?.to_string_lossy();
    name.strip_prefix("gen-")?.parse().ok()
}

/// Delete a generation's profile directory (for GC).
pub fn delete_profile(gen: &Generation) -> anyhow::Result<()> {
    let path = gen.profile_path();
    if path.exists() {
        std::fs::remove_dir_all(&path)?;
        log::info(&format!("profile: deleted gen-{}", gen.number));
    }
    Ok(())
}

/// Patch a postinst script for user-mode (no root, no systemd, no adduser)
pub fn patch_user_script(script: &str) -> String {
    const NEUTRALISE: &[&str] = &[
        "dpkg", "apt", "update-rc.d", "invoke-rc.d",
        "deb-systemd", "systemctl", "adduser", "useradd",
        "groupadd", "addgroup", "ldconfig", "update-alternatives",
    ];

    let mut out = String::new();
    for line in script.lines() {
        let t = line.trim();
        let neutralise = !t.starts_with('#') &&
        NEUTRALISE.iter().any(|p| t.contains(p));
        if neutralise {
            out.push_str(&format!("# HAMMER-USER-NEUTRALISED: {}\ntrue\n", line));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

impl GenerationsDb {
    pub fn load_from(path: &std::path::Path) -> anyhow::Result<Self> {
        if !path.exists() { return Ok(Self::default()); }
        let txt = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&txt)?)
    }

    pub fn save_to(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
        let txt = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &txt)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// Public wrapper — relink binaries for an already-active generation.
/// Called by `hammer relink` to fix PATH without reinstalling.
pub fn relink_bins(profile: &std::path::Path) -> Result<(usize, usize)> {
    link_bins_to_system(profile, 0)
}
