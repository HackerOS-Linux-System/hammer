use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::deb::DebPackage;
use crate::package::Package;

pub const STORE_DIR:    &str = "/hammer/store";
pub const PROFILES_DIR: &str = "/hammer/profiles";
pub const ACTIVE_LINK:  &str = "/hammer/active";

#[derive(Debug, Clone)]
pub struct StoreEntry {
    pub name:    String,
    pub version: String,
    pub hash:    String,
    pub path:    PathBuf,
}

pub fn validate_arch(pkg_name: &str, pkg_arch: &str, sys_arch: &str) -> Result<()> {
    if matches!(pkg_arch, "all" | "any" | "") { return Ok(()); }
    if pkg_arch == sys_arch { return Ok(()); }
    let equiv: &[(&str, &str)] = &[
        ("amd64","x86_64"),("x86_64","amd64"),
        ("arm64","aarch64"),("aarch64","arm64"),
        ("armhf","armv7l"),("armv7l","armhf"),
        ("i386","i686"),("i686","i386"),
    ];
    if equiv.iter().any(|(a,b)| (*a==pkg_arch&&*b==sys_arch)||(*b==pkg_arch&&*a==sys_arch)) {
        return Ok(());
    }
    bail!("Package '{}' is for arch '{}' but system is '{}'.\n  Use --arch={} for cross-arch.",
          pkg_name, pkg_arch, sys_arch, pkg_arch)
}

pub struct Store;

impl Store {
    pub fn install_deb_to(pkg: &Package, deb: &DebPackage, store_dir: &Path) -> Result<StoreEntry> {
        let sys_arch = crate::cache::detect_arch();
        validate_arch(&pkg.name, &pkg.architecture, &sys_arch)?;

        let hash       = compute_store_hash(&pkg.name, &pkg.version, &pkg.architecture);
        let store_path = store_dir.join(format!("{}-{}-{}", pkg.name, pkg.version, &hash));

        if store_path.exists() {
            return Ok(StoreEntry { name: pkg.name.clone(), version: pkg.version.clone(), hash, path: store_path });
        }

        let tmp_path = store_dir.join(format!("{}-{}-{}.tmp", pkg.name, pkg.version, &hash));
        if tmp_path.exists() { std::fs::remove_dir_all(&tmp_path)?; }
        std::fs::create_dir_all(&tmp_path)?;

        // FIX: correct method is extract_data()
        deb.extract_data(&tmp_path)
        .with_context(|| format!("Unpacking {} into store", pkg.name))?;

        std::fs::rename(&tmp_path, &store_path)
        .with_context(|| format!("Committing {} to store", pkg.name))?;

        crate::log::info(&format!("store: installed {} {} ({})", pkg.name, pkg.version, &hash[..8]));
        Ok(StoreEntry { name: pkg.name.clone(), version: pkg.version.clone(), hash, path: store_path })
    }

    pub fn install_deb(pkg: &Package, deb: &DebPackage) -> Result<StoreEntry> {
        std::fs::create_dir_all(STORE_DIR)?;
        Self::install_deb_to(pkg, deb, Path::new(STORE_DIR))
    }

    pub fn disk_usage() -> u64 { dir_size(Path::new(STORE_DIR)) }

    pub fn gc_unreferenced(referenced: &std::collections::HashSet<String>) -> Result<usize> {
        let store = Path::new(STORE_DIR);
        if !store.exists() { return Ok(0); }
        let mut removed = 0;
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

fn compute_store_hash(name: &str, version: &str, arch: &str) -> String {
    let mut h = Sha256::new();
    h.update(name.as_bytes()); h.update(b"|");
    h.update(version.as_bytes()); h.update(b"|");
    h.update(arch.as_bytes());
    hex::encode(&h.finalize()[..4])
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else { return 0; };
    entries.flatten().map(|e| {
        let p = e.path();
        if p.is_dir() { dir_size(&p) } else { std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0) }
    }).sum()
}
