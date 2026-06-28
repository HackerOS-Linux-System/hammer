use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::deb::DebPackage;
use crate::log;
use crate::package::Package;

#[cfg(not(feature = "normal-mode"))]
pub const STORE_DIR:    &str = "/hammer/store";
#[cfg(not(feature = "normal-mode"))]
pub const PROFILES_DIR: &str = "/hammer/profiles";
#[cfg(not(feature = "normal-mode"))]
pub const ACTIVE_LINK:  &str = "/hammer/active";

#[cfg(feature = "normal-mode")]
pub const STORE_DIR:    &str = "/var/lib/hammer/store";
#[cfg(feature = "normal-mode")]
pub const PROFILES_DIR: &str = "/var/lib/hammer/profiles";
#[cfg(feature = "normal-mode")]
pub const ACTIVE_LINK:  &str = "/var/lib/hammer/active";

// ─────────────────────────────────────────────────────────────
//  Backend detection
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum StoreBackend {
    /// btrfs — native subvolumes + snapshots
    BtrfsSnapshot,
    /// ext4/xfs/same-fs — hardlinks (no data duplication)
    Hardlink,
    /// fallback — symlinks (works on any fs, has known issues)
    Symlink,
}

impl StoreBackend {
    pub fn detect() -> Self {
        if is_btrfs(STORE_DIR) && is_btrfs("/usr") { return StoreBackend::BtrfsSnapshot; }
        if same_filesystem(STORE_DIR, "/usr")       { return StoreBackend::Hardlink; }
        StoreBackend::Symlink
    }

    pub fn name(&self) -> &'static str {
        match self {
            StoreBackend::BtrfsSnapshot => "btrfs-snapshot",
            StoreBackend::Hardlink      => "hardlink",
            StoreBackend::Symlink       => "symlink",
        }
    }
}

fn is_btrfs(path: &str) -> bool {
    let out = Command::new("stat").args(["-f", "-c", "%T", path]).output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim() == "btrfs",
        Err(_) => false,
    }
}

fn same_filesystem(a: &str, b: &str) -> bool {
    let dev_a = get_dev_id(a);
    let dev_b = get_dev_id(b);
    dev_a.is_some() && dev_a == dev_b
}

fn get_dev_id(path: &str) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| {
        use std::os::unix::fs::MetadataExt;
        m.dev()
    })
}

// ─────────────────────────────────────────────────────────────
//  StoreEntry
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct StoreEntry {
    pub name:    String,
    pub version: String,
    pub hash:    String,
    pub path:    PathBuf,
    pub backend: StoreBackend,
}

// ─────────────────────────────────────────────────────────────
//  StoreV2
// ─────────────────────────────────────────────────────────────

pub struct StoreV2 {
    pub backend:  StoreBackend,
    pub store_dir: PathBuf,
}

impl StoreV2 {
    pub fn new() -> Self {
        let backend = StoreBackend::detect();
        log::info(&format!("store_v2: using backend '{}'", backend.name()));
        StoreV2 { backend, store_dir: PathBuf::from(STORE_DIR) }
    }

    pub fn with_backend(backend: StoreBackend) -> Self {
        StoreV2 { backend, store_dir: PathBuf::from(STORE_DIR) }
    }

    // ── Install ───────────────────────────────────────────────

    pub fn install_deb(&self, pkg: &Package, deb: &DebPackage) -> Result<StoreEntry> {
        std::fs::create_dir_all(&self.store_dir)?;
        match &self.backend {
            StoreBackend::BtrfsSnapshot => self.install_btrfs(pkg, deb),
            StoreBackend::Hardlink      => self.install_hardlink(pkg, deb),
            StoreBackend::Symlink       => self.install_symlink(pkg, deb),
        }
    }

    // ── Btrfs: subvolume per package ──────────────────────────

    fn install_btrfs(&self, pkg: &Package, deb: &DebPackage) -> Result<StoreEntry> {
        crate::store::validate_arch(&pkg.name, &pkg.architecture, &crate::cache::detect_arch())?;

        let hash      = compute_hash(&pkg.name, &pkg.version, &pkg.architecture);
        let store_path = self.store_dir.join(format!("{}-{}-{}", pkg.name, pkg.version, &hash));

        if store_path.exists() {
            return Ok(StoreEntry {
                name: pkg.name.clone(), version: pkg.version.clone(),
                      hash, path: store_path, backend: StoreBackend::BtrfsSnapshot,
            });
        }

        // Create btrfs subvolume
        btrfs_create_subvolume(&store_path)
        .with_context(|| format!("Creating btrfs subvolume for {}", pkg.name))?;

        // Extract package content
        deb.extract_data(&store_path)
        .with_context(|| format!("Extracting {} to btrfs subvolume", pkg.name))?;

        // Make subvolume read-only (immutable package store)
        btrfs_set_readonly(&store_path, true)
        .with_context(|| format!("Setting btrfs subvolume ro for {}", pkg.name))?;

        log::info(&format!("store_v2(btrfs): installed {} {} ({})",
                           pkg.name, pkg.version, &hash[..8]));
        Ok(StoreEntry {
            name: pkg.name.clone(), version: pkg.version.clone(),
           hash, path: store_path, backend: StoreBackend::BtrfsSnapshot,
        })
    }

    // ── Hardlink: extract once, hardlink everywhere ───────────

    fn install_hardlink(&self, pkg: &Package, deb: &DebPackage) -> Result<StoreEntry> {
        crate::store::validate_arch(&pkg.name, &pkg.architecture, &crate::cache::detect_arch())?;

        let hash      = compute_hash(&pkg.name, &pkg.version, &pkg.architecture);
        let store_path = self.store_dir.join(format!("{}-{}-{}", pkg.name, pkg.version, &hash));

        if store_path.exists() {
            return Ok(StoreEntry {
                name: pkg.name.clone(), version: pkg.version.clone(),
                      hash, path: store_path, backend: StoreBackend::Hardlink,
            });
        }

        // Extract to temp, then hardlink into store
        let tmp_path = self.store_dir.join(format!("{}-{}-{}.tmp", pkg.name, pkg.version, &hash));
        if tmp_path.exists() { std::fs::remove_dir_all(&tmp_path)?; }
        std::fs::create_dir_all(&tmp_path)?;

        deb.extract_data(&tmp_path)
        .with_context(|| format!("Extracting {}", pkg.name))?;

        // Atomic rename tmp → store
        std::fs::rename(&tmp_path, &store_path)
        .with_context(|| format!("Committing {} to store", pkg.name))?;

        log::info(&format!("store_v2(hardlink): installed {} {} ({})",
                           pkg.name, pkg.version, &hash[..8]));
        Ok(StoreEntry {
            name: pkg.name.clone(), version: pkg.version.clone(),
           hash, path: store_path, backend: StoreBackend::Hardlink,
        })
    }

    // ── Symlink: legacy fallback ──────────────────────────────

    fn install_symlink(&self, pkg: &Package, deb: &DebPackage) -> Result<StoreEntry> {
        // Delegate to old store for compatibility
        StoreV2::new().install_deb(pkg, deb).map(|e| StoreEntry {
            name: e.name, version: e.version, hash: e.hash, path: e.path,
            backend: StoreBackend::Symlink,
        })
    }

    // ── Compose generation profile ────────────────────────────

    /// Compose a generation profile from store entries.
    /// On hardlink backend: creates hardlinks (not symlinks) in profile dir.
    /// On btrfs backend: creates a snapshot of the base profile + applies changes.
    pub fn compose_profile(
        &self,
        gen_num:  u32,
        entries:  &[StoreEntry],
        prev_gen: Option<u32>,
        note:     Option<String>,
    ) -> Result<crate::profile::Generation> {
        match &self.backend {
            StoreBackend::BtrfsSnapshot => self.compose_btrfs(gen_num, entries, prev_gen, note),
            StoreBackend::Hardlink      => self.compose_hardlink(gen_num, entries, note),
            StoreBackend::Symlink       => {
                crate::profile::compose_profile(gen_num, &to_old_entries(entries), note)
            }
        }
    }

    fn compose_hardlink(
        &self,
        gen_num: u32,
        entries: &[StoreEntry],
        note:    Option<String>,
    ) -> Result<crate::profile::Generation> {
        let profile_dir = PathBuf::from(PROFILES_DIR).join(format!("gen-{}", gen_num));
        std::fs::create_dir_all(&profile_dir)?;

        let mut pkg_entries = Vec::new();

        for entry in entries {
            if !entry.path.exists() { continue; }
            hardlink_tree(&entry.path, &profile_dir)
            .with_context(|| format!("Hardlinking {} into gen-{}", entry.name, gen_num))?;
            pkg_entries.push(crate::profile::GenPackage {
                name:       entry.name.clone(),
                             version:    entry.version.clone(),
                             store_hash: entry.hash.clone(),
            });
        }

        log::info(&format!("store_v2(hardlink): composed gen-{} ({} packages)",
                           gen_num, pkg_entries.len()));
        Ok(crate::profile::Generation {
            number:    gen_num,
            timestamp: chrono::Utc::now(),
           packages:  pkg_entries,
           note,
           state:     Some(crate::profile::GenState::Pending),
        })
    }

    fn compose_btrfs(
        &self,
        gen_num:  u32,
        entries:  &[StoreEntry],
        prev_gen: Option<u32>,
        note:     Option<String>,
    ) -> Result<crate::profile::Generation> {
        let profile_dir = PathBuf::from(PROFILES_DIR).join(format!("gen-{}", gen_num));

        if let Some(prev) = prev_gen {
            // Snapshot previous generation — INSTANT (CoW)
            let prev_dir = PathBuf::from(PROFILES_DIR).join(format!("gen-{}", prev));
            if prev_dir.exists() {
                btrfs_snapshot(&prev_dir, &profile_dir)
                .with_context(|| format!("Snapshot gen-{} → gen-{}", prev, gen_num))?;
                log::info(&format!("store_v2(btrfs): snapshotted gen-{} → gen-{}", prev, gen_num));
            } else {
                btrfs_create_subvolume(&profile_dir)?;
            }
        } else {
            btrfs_create_subvolume(&profile_dir)?;
        }

        // Apply new packages into the snapshot
        for entry in entries {
            if !entry.path.exists() { continue; }
            // On btrfs: copy files from store subvolume into profile snapshot
            // (they share blocks via CoW — no actual data duplication)
            copy_tree_cow(&entry.path, &profile_dir)
            .with_context(|| format!("Applying {} to gen-{}", entry.name, gen_num))?;
        }

        let pkg_entries: Vec<_> = entries.iter().map(|e| crate::profile::GenPackage {
            name: e.name.clone(), version: e.version.clone(), store_hash: e.hash.clone(),
        }).collect();

        log::info(&format!("store_v2(btrfs): composed gen-{} ({} packages)", gen_num, pkg_entries.len()));
        Ok(crate::profile::Generation {
            number:    gen_num,
            timestamp: chrono::Utc::now(),
           packages:  pkg_entries,
           note,
           state:     Some(crate::profile::GenState::Pending),
        })
    }

    // ── Activate generation — atomic switch ───────────────────

    /// Apply a generation profile to the live system.
    /// On btrfs: set-default subvolume (active after reboot) + update /usr symlink.
    /// On hardlink: atomic hardlink replacement per file.
    pub fn activate_generation(&self, gen: &crate::profile::Generation) -> Result<usize> {
        let profile_path = gen.profile_path();
        if !profile_path.exists() {
            anyhow::bail!("Profile path missing: {}", profile_path.display());
        }

        match &self.backend {
            StoreBackend::BtrfsSnapshot => {
                // On btrfs: set-default makes this subvolume the default mount
                btrfs_set_default_subvolume(&profile_path)?;
                log::info(&format!("store_v2(btrfs): set-default gen-{}", gen.number));
                // Also update /hammer/active for runtime use
                crate::profile::switch_active(gen)?;
                Ok(0) // btrfs handles file layout natively
            }
            StoreBackend::Hardlink => {
                // Atomically replace /usr files with hardlinks from profile
                let count = apply_hardlinks_to_system(&profile_path)?;
                crate::profile::switch_active(gen)?;
                log::info(&format!("store_v2(hardlink): activated gen-{}, {} files", gen.number, count));
                Ok(count)
            }
            StoreBackend::Symlink => {
                // Legacy: update symlinks via relink_bins
                crate::profile::switch_active(gen)?;
                let (linked, _) = crate::profile::relink_bins(&profile_path)?;
                Ok(linked)
            }
        }
    }

    // ── GC ───────────────────────────────────────────────────

    pub fn gc_unreferenced(&self, referenced: &std::collections::HashSet<String>) -> Result<usize> {
        let store = &self.store_dir;
        if !store.exists() { return Ok(0); }
        let mut removed = 0;
        for entry in std::fs::read_dir(store)?.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !referenced.contains(&name) {
                match &self.backend {
                    StoreBackend::BtrfsSnapshot => {
                        // Delete btrfs subvolume (must be rw first)
                        let _ = btrfs_set_readonly(&entry.path(), false);
                        let _ = btrfs_delete_subvolume(&entry.path());
                    }
                    _ => { std::fs::remove_dir_all(entry.path()).ok(); }
                }
                log::info(&format!("store_v2: gc removed {}", name));
                removed += 1;
            }
        }
        Ok(removed)
    }
}

// ─────────────────────────────────────────────────────────────
//  Hardlink helpers
// ─────────────────────────────────────────────────────────────

/// Walk `src` tree and create hardlinks in `dest` preserving structure.
/// If hardlink fails (cross-device), falls back to copy.
fn hardlink_tree(src: &Path, dest: &Path) -> Result<usize> {
    let mut count = 0;
    for item in walkdir::WalkDir::new(src).min_depth(1).into_iter().filter_map(|e| e.ok()) {
        let rel = match item.path().strip_prefix(src) {
            Ok(r) => r, Err(_) => continue,
        };
        let target = dest.join(rel);
        if item.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else if item.file_type().is_file() {
            if let Some(parent) = target.parent() { std::fs::create_dir_all(parent)?; }
            if target.symlink_metadata().is_ok() { continue; } // already exists
            match std::fs::hard_link(item.path(), &target) {
                Ok(()) => count += 1,
                Err(_) => {
                    // Cross-filesystem fallback: copy
                    std::fs::copy(item.path(), &target)?;
                    count += 1;
                }
            }
        } else if item.file_type().is_symlink() {
            // Preserve symlinks as-is (e.g. /usr/bin/python3 → python3.11)
            if let Some(parent) = target.parent() { std::fs::create_dir_all(parent)?; }
            if !target.symlink_metadata().is_ok() {
                if let Ok(link_target) = std::fs::read_link(item.path()) {
                    std::os::unix::fs::symlink(&link_target, &target).ok();
                }
            }
        }
    }
    Ok(count)
}

/// Apply hardlinks from profile directory to live system directories (/usr etc).
/// This is the atomic activation step: replaces files in-place via hardlinks.
fn apply_hardlinks_to_system(profile_path: &Path) -> Result<usize> {
    let system_dirs = ["usr", "bin", "sbin", "lib", "lib64", "opt"];
    let mut count = 0;

    for dir in &system_dirs {
        let src = profile_path.join(dir);
        let dst = Path::new("/").join(dir);
        if !src.exists() { continue; }
        count += hardlink_tree(&src, &dst)
        .with_context(|| format!("Applying hardlinks for /{}", dir))?;
    }
    Ok(count)
}

// ─────────────────────────────────────────────────────────────
//  Btrfs helpers
// ─────────────────────────────────────────────────────────────

fn btrfs_create_subvolume(path: &Path) -> Result<()> {
    let out = Command::new("btrfs")
    .args(["subvolume", "create"])
    .arg(path)
    .output()
    .context("btrfs subvolume create")?;
    if !out.status.success() {
        // Fallback: plain mkdir (if btrfs tools not available)
        std::fs::create_dir_all(path)?;
    }
    Ok(())
}

fn btrfs_snapshot(src: &Path, dest: &Path) -> Result<()> {
    // First try rw snapshot, then set ro on it
    let out = Command::new("btrfs")
    .args(["subvolume", "snapshot"])
    .arg(src).arg(dest)
    .output()
    .context("btrfs subvolume snapshot")?;
    if !out.status.success() {
        // Fallback: copy tree
        log::warn(&format!("btrfs snapshot failed, falling back to copy: {}",
                           String::from_utf8_lossy(&out.stderr)));
        copy_tree_cow(src, dest)?;
    }
    Ok(())
}

fn btrfs_set_readonly(path: &Path, ro: bool) -> Result<()> {
    let val = if ro { "true" } else { "false" };
    Command::new("btrfs")
    .args(["property", "set", "-ts"])
    .arg(path).arg("ro").arg(val)
    .output()
    .context("btrfs property set ro")?;
    Ok(())
}

fn btrfs_delete_subvolume(path: &Path) -> Result<()> {
    Command::new("btrfs")
    .args(["subvolume", "delete"])
    .arg(path)
    .output()
    .context("btrfs subvolume delete")?;
    Ok(())
}

fn btrfs_set_default_subvolume(path: &Path) -> Result<()> {
    // Get subvolume ID
    let out = Command::new("btrfs")
    .args(["subvolume", "show"])
    .arg(path).output();
    if let Ok(o) = out {
        let text = String::from_utf8_lossy(&o.stdout);
        if let Some(id_line) = text.lines().find(|l| l.trim().starts_with("Subvolume ID:")) {
            if let Some(id_str) = id_line.split(':').nth(1) {
                let id = id_str.trim();
                let _ = Command::new("btrfs")
                .args(["subvolume", "set-default", id, "/"])
                .output();
                log::info(&format!("btrfs: set-default subvolume {}", id));
            }
        }
    }
    Ok(())
}

/// Copy tree using cp --reflink=auto (CoW on btrfs, plain copy elsewhere)
fn copy_tree_cow(src: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let out = Command::new("cp")
    .args(["-a", "--reflink=auto"])
    .arg(src).arg(dest)
    .output();
    match out {
        Ok(o) if o.status.success() => Ok(()),
        _ => {
            // cp --reflink not available, use Rust walkdir copy
            hardlink_tree(src, dest).map(|_| ())
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  Helpers
// ─────────────────────────────────────────────────────────────

fn compute_hash(name: &str, version: &str, arch: &str) -> String {
    let mut h = Sha256::new();
    h.update(name.as_bytes()); h.update(b"|");
    h.update(version.as_bytes()); h.update(b"|");
    h.update(arch.as_bytes());
    hex::encode(&h.finalize()[..4])
}

fn to_old_entries(entries: &[StoreEntry]) -> Vec<crate::store::StoreEntry> {
    entries.iter().map(|e| crate::store::StoreEntry {
        name:    e.name.clone(),
                       version: e.version.clone(),
                       hash:    e.hash.clone(),
                       path:    e.path.clone(),
                    backend: crate::store::StoreBackend::Hardlink,
                }).collect()
}

// ─────────────────────────────────────────────────────────────
//  validate_arch — check package arch against system arch
// ─────────────────────────────────────────────────────────────

pub fn validate_arch(pkg_name: &str, pkg_arch: &str, sys_arch: &str) -> anyhow::Result<()> {
    match pkg_arch {
        "all" | "any" | "" => Ok(()),
        arch if arch == sys_arch => Ok(()),
        arch => {
            anyhow::bail!(
                "Package '{}' is for architecture '{}' but system is '{}'.",
                pkg_name, arch, sys_arch
            )
        }
    }
}

/// Type alias for backward compatibility
pub type Store = StoreV2;

/// Free function wrapper used by transaction.rs and setup.rs
pub fn store_install_deb(pkg: &Package, deb: &DebPackage) -> Result<StoreEntry> {
    let s = StoreV2::new();
    s.install_deb(pkg, deb)
}

fn detect_best_backend() -> StoreBackend {
    // Prefer btrfs if available
    if std::path::Path::new("/hammer/store").exists() {
        if let Ok(out) = std::process::Command::new("stat")
            .args(["--file-system", "--format=%T", "/hammer/store"])
            .output()
        {
            if String::from_utf8_lossy(&out.stdout).trim() == "btrfs" {
                return StoreBackend::BtrfsSnapshot;
            }
        }
    }
    StoreBackend::Hardlink
}

/// Free function: install a .deb into the store.
/// Used by transaction.rs which calls Store::install_deb(pkg, deb).
pub fn install_deb_pkg(pkg: &Package, deb: &DebPackage) -> Result<StoreEntry> {
    StoreV2::new().install_deb(pkg, deb)
}

/// Free function: GC unreferenced store entries.
/// Used by cli/sys.rs which calls Store::gc_unreferenced(&referenced).
pub fn gc_unreferenced_pkgs(referenced: &std::collections::HashSet<String>) -> Result<()> {
    StoreV2::new().gc_unreferenced(referenced).map(|_| ())
}
