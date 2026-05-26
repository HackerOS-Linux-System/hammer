use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::deb::DebPackage;
use crate::package::Package;

pub const STORE_DIR:    &str = "/hammer/store";
pub const PROFILES_DIR: &str = "/hammer/profiles";
pub const ACTIVE_LINK:  &str = "/hammer/active";

// ─────────────────────────────────────────────────────────────
//  StoreEntry
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct StoreEntry {
    pub name:    String,
    pub version: String,
    pub hash:    String,
    pub path:    PathBuf,
}

// ─────────────────────────────────────────────────────────────
//  Store
// ─────────────────────────────────────────────────────────────

pub struct Store;

impl Store {
    /// Install a .deb into the content-addressed store.
    /// Returns the StoreEntry describing where it was stored.
    ///
    /// FIX: Now validates architecture before unpacking.
    pub fn install_deb_to(
        pkg:       &Package,
        deb:       &DebPackage,
        store_dir: &Path,
    ) -> Result<StoreEntry> {
        // ── Architecture validation ───────────────────────────
        let sys_arch = crate::cache::detect_arch();
        validate_arch(&pkg.name, &pkg.architecture, &sys_arch)?;

        // ── Compute store hash ────────────────────────────────
        let hash = compute_store_hash(&pkg.name, &pkg.version, &pkg.architecture);
        let store_path = store_dir.join(format!("{}-{}-{}", pkg.name, pkg.version, &hash));

        // Already unpacked?
        if store_path.exists() {
            return Ok(StoreEntry {
                name:    pkg.name.clone(),
                      version: pkg.version.clone(),
                      hash,
                      path:    store_path,
            });
        }

        // ── Unpack ───────────────────────────────────────────
        let tmp_path = store_dir.join(format!("{}-{}-{}.tmp", pkg.name, pkg.version, &hash));
        if tmp_path.exists() { std::fs::remove_dir_all(&tmp_path)?; }
        std::fs::create_dir_all(&tmp_path)?;

        deb.unpack_data(&tmp_path)
        .with_context(|| format!("Unpacking {} into store", pkg.name))?;

        // Atomic rename
        std::fs::rename(&tmp_path, &store_path)
        .with_context(|| format!("Committing {} to store", pkg.name))?;

        crate::log::info(&format!(
            "store: installed {} {} ({})", pkg.name, pkg.version, &hash[..8]
        ));

        Ok(StoreEntry {
            name:    pkg.name.clone(),
           version: pkg.version.clone(),
           hash,
           path:    store_path,
        })
    }

    /// Install to the default system store dir.
    pub fn install_deb(pkg: &Package, deb: &DebPackage) -> Result<StoreEntry> {
        std::fs::create_dir_all(STORE_DIR)?;
        Self::install_deb_to(pkg, deb, Path::new(STORE_DIR))
    }

    /// Disk usage of the entire store in bytes.
    pub fn disk_usage() -> u64 {
        dir_size(Path::new(STORE_DIR))
    }

    /// Remove store entries not referenced by any generation.
    pub fn gc_unreferenced(referenced: &std::collections::HashSet<String>) -> Result<usize> {
        let store = Path::new(STORE_DIR);
        if !store.exists() { return Ok(0); }
        let mut removed = 0usize;
        for entry in std::fs::read_dir(store)?.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !referenced.contains(&name) {
                std::fs::remove_dir_all(entry.path()).ok();
                crate::log::info(&format!("store: gc removed {}", name));
                removed += 1;
            }
        }
        Ok(removed)
    }
}

// ─────────────────────────────────────────────────────────────
//  Architecture validation
// ─────────────────────────────────────────────────────────────

/// Validate that `pkg_arch` is compatible with `sys_arch`.
/// Returns an error if not compatible.
pub fn validate_arch(pkg_name: &str, pkg_arch: &str, sys_arch: &str) -> Result<()> {
    // "all" and "any" are always compatible (architecture-independent)
    if matches!(pkg_arch, "all" | "any" | "") { return Ok(()); }

    // Direct match
    if pkg_arch == sys_arch { return Ok(()); }

    // Known equivalent pairs (e.g. arm64 == aarch64 in some repos)
    let equivalent: &[(&str, &str)] = &[
        ("amd64", "x86_64"),
        ("x86_64", "amd64"),
        ("arm64", "aarch64"),
        ("aarch64", "arm64"),
        ("armhf", "armv7l"),
        ("armv7l", "armhf"),
        ("i386", "i686"),
        ("i686", "i386"),
    ];
    if equivalent.iter().any(|(a, b)| (*a == pkg_arch && *b == sys_arch) || (*b == pkg_arch && *a == sys_arch)) {
        return Ok(());
    }

    bail!(
        "Package '{}' is for architecture '{}' but system is '{}'.\n  \
Use --arch={} to install cross-arch packages.",
pkg_name, pkg_arch, sys_arch, pkg_arch
    )
}

// ─────────────────────────────────────────────────────────────
//  Helpers
// ─────────────────────────────────────────────────────────────

fn compute_store_hash(name: &str, version: &str, arch: &str) -> String {
    let mut h = Sha256::new();
    h.update(name.as_bytes());
    h.update(b"|");
    h.update(version.as_bytes());
    h.update(b"|");
    h.update(arch.as_bytes());
    let digest = h.finalize();
    hex::encode(&digest[..4]) // 8-char prefix is enough for store dirs
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else { return 0; };
    entries.flatten().map(|e| {
        let p = e.path();
        if p.is_dir() { dir_size(&p) }
        else { std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0) }
    }).sum()
}
