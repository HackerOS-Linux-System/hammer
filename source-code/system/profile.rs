use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::log;
use crate::postinst::PostinstTranslator;
use crate::store::{StoreEntry, ACTIVE_LINK, PROFILES_DIR};

// ─────────────────────────────────────────────────────────────
//  Paths — resolved at runtime for normal-mode compatibility
// ─────────────────────────────────────────────────────────────

#[cfg(not(feature = "normal-mode"))]
pub const GENERATIONS_FILE: &str = "/hammer/db/generations.json";
#[cfg(not(feature = "normal-mode"))]
pub const PENDING_LINK:     &str = "/hammer/pending";
#[cfg(not(feature = "normal-mode"))]
pub const ACTIVATION_LOG:   &str = "/hammer/db/activation.log";

#[cfg(feature = "normal-mode")]
pub const GENERATIONS_FILE: &str = "/var/lib/hammer/db/generations.json";
#[cfg(feature = "normal-mode")]
pub const PENDING_LINK:     &str = "/var/lib/hammer/pending";
#[cfg(feature = "normal-mode")]
pub const ACTIVATION_LOG:   &str = "/var/lib/hammer/db/activation.log";

pub const BOOT_GEN_FILE: &str = "/boot/hammer-boot-gen";

const BIN_SOURCE_DIRS: &[&str] = &[
    "usr/bin", "usr/local/bin", "usr/sbin",
"usr/local/sbin", "bin", "sbin",
];
const LINK_TARGET_DIR: &str = "/usr/bin";

// ─────────────────────────────────────────────────────────────
//  Data structures
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenPackage {
    pub name:       String,
    pub version:    String,
    pub store_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum GenState { Active, Pending, Old, Failed }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Generation {
    pub number:    u32,
    pub timestamp: DateTime<Utc>,
    pub packages:  Vec<GenPackage>,
    pub note:      Option<String>,
    pub state:     Option<GenState>,
}

impl Generation {
    pub fn profile_path(&self) -> PathBuf {
        PathBuf::from(PROFILES_DIR).join(format!("gen-{}", self.number))
    }
    pub fn package_count(&self) -> usize { self.packages.len() }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GenerationsDb {
    pub current:     u32,
    pub pending:     Option<u32>,
    pub generations: Vec<Generation>,
}

impl GenerationsDb {
    /// Iterate all generations.
    pub fn all(&self) -> &[Generation] {
        &self.generations
    }

    /// Return the active generation number, if any.
    pub fn active_num(&self) -> Option<u32> {
        read_active_gen()
    }

    pub fn load() -> Result<Self> {
        let path = Path::new(GENERATIONS_FILE);
        if !path.exists() { return Ok(Self::default()); }
        let c = std::fs::read_to_string(path).context("Reading generations.json")?;
        serde_json::from_str(&c).context("Parsing generations.json")
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() { return Ok(Self::default()); }
        let c = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&c).unwrap_or_default())
    }

    pub fn save(&self) -> Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        let tmp     = format!("{}.tmp", GENERATIONS_FILE);
        std::fs::write(&tmp, &content)?;
        std::fs::rename(&tmp, GENERATIONS_FILE)?;
        Ok(())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
        let content = serde_json::to_string_pretty(self)?;
        let tmp     = path.with_extension("tmp");
        std::fs::write(&tmp, &content)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn get(&self, number: u32) -> Option<&Generation> {
        self.generations.iter().find(|g| g.number == number)
    }

    pub fn current_gen(&self) -> Option<&Generation> { self.get(self.current) }

    pub fn next_number(&self) -> u32 {
        self.generations.iter().map(|g| g.number).max().unwrap_or(0) + 1
    }
}

// ─────────────────────────────────────────────────────────────
//  ActivationResult — fields must match what ui.rs uses
// ─────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ActivationResult {
    pub gen_number:      u32,
    pub packages_linked: usize,
    pub scripts_failed:  Vec<String>,
    pub already_active:  bool,
}

// ─────────────────────────────────────────────────────────────
//  compose_profile
// ─────────────────────────────────────────────────────────────

pub fn compose_profile(
    gen_num:       u32,
    store_entries: &[StoreEntry],
    note:          Option<String>,
) -> Result<Generation> {
    let profile_dir = PathBuf::from(PROFILES_DIR).join(format!("gen-{}", gen_num));
    std::fs::create_dir_all(&profile_dir)?;
    for sub in &["usr/bin","usr/local/bin","usr/sbin","usr/lib","usr/share","etc","var","lib"] {
        std::fs::create_dir_all(profile_dir.join(sub))?;
    }
    let mut pkg_entries = Vec::new();
    for entry in store_entries {
        if entry.path.exists() { link_store_into_profile(&entry.path, &profile_dir)?; }
        pkg_entries.push(GenPackage {
            name:       entry.name.clone(),
                         version:    entry.version.clone(),
                         store_hash: entry.hash.clone(),
        });
    }
    Ok(Generation {
        number:    gen_num,
       timestamp: Utc::now(),
       packages:  pkg_entries,
       note,
       state:     Some(GenState::Pending),
    })
}

fn link_store_into_profile(store_path: &Path, profile_dir: &Path) -> Result<()> {
    for item in walkdir::WalkDir::new(store_path).min_depth(1)
        .into_iter().filter_map(|e| e.ok())
        {
            let rel = match item.path().strip_prefix(store_path) {
                Ok(r) => r, Err(_) => continue,
            };
            let dest = profile_dir.join(rel);
            if item.file_type().is_dir() {
                std::fs::create_dir_all(&dest)?;
            } else if item.file_type().is_symlink() || item.file_type().is_file() {
                if let Some(parent) = dest.parent() { std::fs::create_dir_all(parent)?; }
                if !dest.symlink_metadata().is_ok() {
                    let src = item.path().canonicalize().unwrap_or_else(|_| item.path().to_path_buf());
                    std::os::unix::fs::symlink(&src, &dest).ok();
                }
            }
        }
        Ok(())
}

// ─────────────────────────────────────────────────────────────
//  relink_bins — creates /usr/bin/<name> symlinks
// ─────────────────────────────────────────────────────────────

pub fn relink_bins(profile_path: &Path) -> Result<(usize, usize)> {
    let link_dir = Path::new(LINK_TARGET_DIR);
    std::fs::create_dir_all(link_dir)?;
    let mut linked  = 0usize;
    let mut removed = 0usize;
    let mut seen:   HashSet<String> = HashSet::new();

    for bin_subdir in BIN_SOURCE_DIRS {
        let src_dir = profile_path.join(bin_subdir);
        if !src_dir.exists() { continue; }
        for entry in std::fs::read_dir(&src_dir)
            .with_context(|| format!("Reading {}", src_dir.display()))?
            .flatten()
            {
                let src_path  = entry.path();
                let file_name = src_path.file_name()
                .and_then(|n| n.to_str()).unwrap_or("").to_string();
                if file_name.is_empty() || seen.contains(&file_name) { continue; }
                if src_path.is_dir() { continue; }
                let is_exec = std::fs::metadata(&src_path).map(|m| {
                    use std::os::unix::fs::PermissionsExt;
                    m.permissions().mode() & 0o111 != 0
                }).unwrap_or(false);
                if !is_exec && !src_path.is_symlink() { continue; }

                let link_path = link_dir.join(&file_name);
                if link_path.symlink_metadata().is_ok() {
                    if let Ok(target) = std::fs::read_link(&link_path) {
                        if target == src_path { seen.insert(file_name.clone()); continue; }
                    }
                    std::fs::remove_file(&link_path).ok();
                    removed += 1;
                }
                if std::os::unix::fs::symlink(&src_path, &link_path).is_ok() {
                    linked += 1;
                    seen.insert(file_name.clone());
                    log::info(&format!("relink: {} → {}", link_path.display(), src_path.display()));
                }
            }
    }

    // vim wrapper
    let vim_link  = link_dir.join("vim");
    let vim_basic = profile_path.join("usr/bin/vim.basic");
    if !vim_link.symlink_metadata().is_ok() && vim_basic.exists() {
        if std::os::unix::fs::symlink(&vim_basic, &vim_link).is_ok() {
            linked += 1;
        }
    }
    Ok((linked, removed))
}

// ─────────────────────────────────────────────────────────────
//  activate_pending
// ─────────────────────────────────────────────────────────────

pub fn activate_pending() -> Result<ActivationResult> {
    let pending_num = match read_pending_gen() {
        Some(n) => n,
        None => {
            let gdb = GenerationsDb::load()?;
            let cur = gdb.current_gen()
            .ok_or_else(|| anyhow::anyhow!("No current generation"))?;
            let (l, _) = relink_bins(&cur.profile_path())?;
            return Ok(ActivationResult {
                gen_number:      gdb.current,
                packages_linked: l,
                scripts_failed:  vec![],
                already_active:  true,
            });
        }
    };

    let mut gdb = GenerationsDb::load()?;
    let gen = gdb.get(pending_num)
    .ok_or_else(|| anyhow::anyhow!("Pending gen {} not in DB", pending_num))?
    .clone();

    let profile_path = gen.profile_path();
    if !profile_path.exists() {
        anyhow::bail!("Profile path missing: {}", profile_path.display());
    }

    switch_active(&gen)?;
    let (linked, _) = relink_bins(&profile_path)?;

    let prev_pkgs: HashSet<String> = gdb.current_gen()
    .map(|g| g.packages.iter().map(|p| p.name.clone()).collect())
    .unwrap_or_default();

    let mut scripts_failed = Vec::new();
    for pkg in &gen.packages {
        if prev_pkgs.contains(&pkg.name) { continue; }
        let script_path = PathBuf::from("/hammer/db/postinst")
        .join(format!("{}.postinst", pkg.name));
        if script_path.exists() {
            if let Err(e) = run_postinst(&pkg.name, &script_path) {
                log::warn(&format!("postinst {} failed: {}", pkg.name, e));
                scripts_failed.push(pkg.name.clone());
            }
        }
    }

    gdb.current = pending_num;
    gdb.pending = None;
    if let Some(g) = gdb.generations.iter_mut().find(|g| g.number == pending_num) {
        g.state = Some(GenState::Active);
    }
    gdb.save()?;
    clear_pending().ok();

    // Write activation log
    let ts  = chrono::Utc::now().to_rfc3339();
    let msg = format!("[{}] activated gen-{}, {} binaries linked\n", ts, pending_num, linked);
    if let Some(parent) = Path::new(ACTIVATION_LOG).parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let _ = std::fs::OpenOptions::new().create(true).append(true)
    .open(ACTIVATION_LOG).map(|mut f| { use std::io::Write; let _ = f.write_all(msg.as_bytes()); });

    log::info(&format!("activate: gen-{} activated, {} binaries linked", pending_num, linked));
    Ok(ActivationResult {
        gen_number:      pending_num,
       packages_linked: linked,
       scripts_failed,
       already_active:  false,
    })
}

// ─────────────────────────────────────────────────────────────
//  run_postinst — pub so transaction.rs can call it
// ─────────────────────────────────────────────────────────────

pub fn run_postinst(pkg_name: &str, script_path: &Path) -> Result<()> {
    let script  = std::fs::read_to_string(script_path)?;
    let trans   = PostinstTranslator::new(pkg_name);
    let actions = trans.translate(&script);
    let (results, summary) = trans.run(&actions);

    // Log services that were enabled/started
    if !summary.services_enabled.is_empty() {
        crate::log::info(&format!(
            "postinst [{}]: enabled services: {}",
            pkg_name,
            summary.services_enabled.join(", ")
        ));
    }
    if !summary.services_started.is_empty() {
        crate::log::info(&format!(
            "postinst [{}]: started services: {}",
            pkg_name,
            summary.services_started.join(", ")
        ));
    }
    if !summary.conffiles_created.is_empty() {
        crate::log::info(&format!(
            "postinst [{}]: created conffiles: {}",
            pkg_name,
            summary.conffiles_created.join(", ")
        ));
    }
    if !summary.warnings.is_empty() {
        for w in &summary.warnings {
            crate::log::warn(&format!("postinst [{}]: {}", pkg_name, w));
        }
    }

    let failed: Vec<_> = results.iter()
        .filter(|r| !r.success && r.action != "skip").collect();
    if !failed.is_empty() {
        let msgs: Vec<String> = failed.iter()
            .map(|r| format!("  {} failed: {}", r.action, r.message)).collect();
        anyhow::bail!("postinst actions failed:\n{}", msgs.join("\n"));
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  switch_active / pending helpers
// ─────────────────────────────────────────────────────────────

pub fn switch_active(gen: &Generation) -> Result<()> {
    let target = gen.profile_path();
    let active = Path::new(ACTIVE_LINK);
    let tmp    = PathBuf::from(format!("{}.tmp", ACTIVE_LINK));
    if !target.exists() { anyhow::bail!("Profile does not exist: {}", target.display()); }
    if tmp.symlink_metadata().is_ok() { std::fs::remove_file(&tmp)?; }
    std::os::unix::fs::symlink(&target, &tmp).context("Creating tmp active symlink")?;
    std::fs::rename(&tmp, active).context("Atomic rename of active symlink")?;
    log::info(&format!("profile: switched active to gen-{}", gen.number));
    Ok(())
}

pub fn set_pending(gen: &Generation) -> Result<()> {
    let pending = Path::new(PENDING_LINK);
    if pending.symlink_metadata().is_ok() { std::fs::remove_file(pending)?; }
    std::os::unix::fs::symlink(gen.profile_path(), pending)?;
    let mut gdb = GenerationsDb::load()?;
    gdb.pending = Some(gen.number);
    gdb.save()?;
    Ok(())
}

pub fn clear_pending() -> Result<()> {
    let pending = Path::new(PENDING_LINK);
    if pending.symlink_metadata().is_ok() { std::fs::remove_file(pending)?; }
    Ok(())
}

pub fn read_pending_gen() -> Option<u32> {
    GenerationsDb::load().ok()?.pending
}

pub fn read_active_gen() -> Option<u32> {
    let active = Path::new(ACTIVE_LINK);
    if !active.symlink_metadata().is_ok() { return None; }
    let target = std::fs::read_link(active).ok()?;
    let name   = target.file_name()?.to_str()?;
    name.strip_prefix("gen-")?.parse().ok()
}

pub fn install_activate_service() -> Result<()> {
    let hammer_bin = std::fs::read_link("/proc/self/exe")
    .unwrap_or_else(|_| PathBuf::from("/usr/bin/hammer"));
    let symlink_path = Path::new("/usr/bin/hammer");
    if !symlink_path.exists() && !symlink_path.symlink_metadata().is_ok() {
        std::os::unix::fs::symlink(&hammer_bin, symlink_path).ok();
    }
    let unit = format!(
        "[Unit]\n\
Description=Hammer Package Manager — Generation Activation\n\
DefaultDependencies=no\n\
Before=basic.target\n\
After=local-fs.target\n\
ConditionPathExists=/hammer/pending\n\
\n\
[Service]\n\
Type=oneshot\n\
RemainAfterExit=yes\n\
ExecStart={hammer} _activate\n\
StandardOutput=journal\n\
StandardError=journal\n\
\n\
[Install]\n\
WantedBy=sysinit.target\n",
hammer = hammer_bin.display()
    );
    std::fs::write("/etc/systemd/system/hammer-activate.service", &unit)?;
    let _ = std::process::Command::new("systemctl")
    .args(["enable", "hammer-activate.service", "--no-reload"]).status();
    log::info(&format!("profile: installed hammer-activate.service ({})", hammer_bin.display()));
    Ok(())
}

pub fn delete_profile(gen: &Generation) -> Result<()> {
    let pp = gen.profile_path();
    if pp.exists() {
        std::fs::remove_dir_all(&pp)
        .with_context(|| format!("Removing profile {}", pp.display()))?;
        log::info(&format!("profile: deleted gen-{}", gen.number));
    }
    Ok(())
}

// user mode
pub fn compose_user_profile(
    hammer_dir:    &Path,
    gen_num:       u32,
    store_entries: &[StoreEntry],
) -> Result<PathBuf> {
    let profile_dir = hammer_dir.join("profiles").join(format!("gen-{}", gen_num));
    std::fs::create_dir_all(profile_dir.join("bin"))?;
    for entry in store_entries {
        if entry.path.exists() { link_store_into_profile(&entry.path, &profile_dir)?; }
    }
    Ok(profile_dir)
}
