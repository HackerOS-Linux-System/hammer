use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::deb::DebPackage;
use crate::log;
use crate::package::Package;

pub const STORE_DIR:    &str = "/hammer/store";
pub const PROFILES_DIR: &str = "/hammer/profiles";
pub const ACTIVE_LINK:  &str = "/hammer/active";
pub const HAMMER_ROOT:  &str = "/hammer";

// ─────────────────────────────────────────────────────────────
//  Store entry
// ─────────────────────────────────────────────────────────────

/// Represents one entry in the store:
/// /hammer/store/<name>-<version>-<hash8>/
#[derive(Debug, Clone)]
pub struct StoreEntry {
    pub name:    String,
    pub version: String,
    pub hash:    String,   // 8-char prefix of sha256 of content
    pub path:    PathBuf,  // absolute path in store
}

impl StoreEntry {
    pub fn store_name(&self) -> String {
        format!("{}-{}-{}", self.name, self.version, self.hash)
    }
}

// ─────────────────────────────────────────────────────────────
//  Store
// ─────────────────────────────────────────────────────────────

pub struct Store;

impl Store {
    /// Create all required hammer directories
    pub fn init() -> Result<()> {
        for dir in &[STORE_DIR, PROFILES_DIR, "/hammer/db", "/var/cache/hammer/archives"] {
            std::fs::create_dir_all(dir)
            .with_context(|| format!("Cannot create {}", dir))?;
        }
        // Create gen-0 empty profile if no profiles exist
        let gen0 = PathBuf::from(PROFILES_DIR).join("gen-0");
        if !gen0.exists() {
            std::fs::create_dir_all(&gen0)?;
            // Create skeleton dirs in gen-0
            for d in &["usr/bin", "usr/lib", "usr/share", "usr/include", "etc", "var"] {
                std::fs::create_dir_all(gen0.join(d))?;
            }
            // Set active → gen-0
            let active = Path::new(ACTIVE_LINK);
            if active.symlink_metadata().is_ok() {
                std::fs::remove_file(active)?;
            }
            std::os::unix::fs::symlink(&gen0, active)?;
        }
        Ok(())
    }

    /// Install a .deb into the store.
    /// Returns the StoreEntry — if already present (same hash), returns existing.
    /// This is the core "content-addressed" operation.
    pub fn install_deb(pkg: &Package, deb: &DebPackage) -> Result<StoreEntry> {
        std::fs::create_dir_all(STORE_DIR)?;

        // Hash the raw .deb data bytes to get a content-addressed path
        let hash = hash_deb_content(&deb.data_bytes);
        let entry = StoreEntry {
            name:    pkg.name.clone(),
            version: pkg.version.clone(),
            hash:    hash.clone(),
            path:    PathBuf::from(STORE_DIR).join(format!("{}-{}-{}", pkg.name, pkg.version, hash)),
        };

        // Idempotent: if already in store, skip extraction
        if entry.path.exists() {
            log::info(&format!("store: {} already present ({})", pkg.name, hash));
            return Ok(entry);
        }

        log::info(&format!("store: extracting {} → {}", pkg.name, entry.path.display()));

        // Extract data.tar into a temp dir first, then rename atomically
        let tmp_path = PathBuf::from(STORE_DIR)
        .join(format!(".tmp-{}-{}-{}", pkg.name, pkg.version, hash));
        std::fs::create_dir_all(&tmp_path)?;

        deb.extract_data(&tmp_path)
        .with_context(|| format!("Extracting {} into store", pkg.name))?;

        // Run any post-install scripts that are safe
        // (we skip preinst/postinst — they expect a live root, not store)
        // Scripts that configure /etc are handled at profile-compose time

        // Rename temp → final (atomic on same filesystem)
        std::fs::rename(&tmp_path, &entry.path)
        .with_context(|| format!("Committing {} to store", pkg.name))?;

        log::info(&format!("store: committed {}", entry.store_name()));
        Ok(entry)
    }

    /// Install a .deb into a custom store directory (for --user mode).
    pub fn install_deb_to(pkg: &Package, deb: &DebPackage, store_dir: &std::path::Path) -> Result<StoreEntry> {
        std::fs::create_dir_all(store_dir)?;

        let hash  = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(&deb.data_bytes);
            hex::encode(&h.finalize()[..4])
        };
        let entry = StoreEntry {
            name:    pkg.name.clone(),
            version: pkg.version.clone(),
            hash:    hash.clone(),
            path:    store_dir.join(format!("{}-{}-{}", pkg.name, pkg.version, hash)),
        };

        if entry.path.exists() {
            log::info(&format!("store(user): {} already present", pkg.name));
            return Ok(entry);
        }

        let tmp_path = store_dir.join(format!(".tmp-{}-{}-{}", pkg.name, pkg.version, hash));
        std::fs::create_dir_all(&tmp_path)?;
        deb.extract_data(&tmp_path)
        .with_context(|| format!("Extracting {} into user store", pkg.name))?;
        std::fs::rename(&tmp_path, &entry.path)
        .with_context(|| format!("Committing {} to user store", pkg.name))?;

        log::info(&format!("store(user): committed {}", entry.store_name()));
        Ok(entry)
    }

    /// Remove a store entry (for gc)
    pub fn remove_entry(entry: &StoreEntry) -> Result<()> {
        if entry.path.exists() {
            std::fs::remove_dir_all(&entry.path)?;
            log::info(&format!("store: removed {}", entry.store_name()));
        }
        Ok(())
    }

    /// List all entries currently in the store
    pub fn list_entries() -> Result<Vec<StoreEntry>> {
        let store = Path::new(STORE_DIR);
        if !store.exists() { return Ok(Vec::new()); }

        let mut entries = Vec::new();
        for e in std::fs::read_dir(store)? {
            let e    = e?;
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') { continue; }

            // Parse name-ver-hash8
            if let Some(entry) = parse_store_name(&name, &e.path()) {
                entries.push(entry);
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    /// Total disk usage of store (bytes)
    pub fn disk_usage() -> u64 {
        WalkDir::new(STORE_DIR)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
    }

    /// Remove store entries not referenced by any generation.
    pub fn gc_unreferenced(referenced: &std::collections::HashSet<String>) -> anyhow::Result<()> {
        let store_dir = std::path::Path::new(STORE_DIR);
        if !store_dir.exists() { return Ok(()); }
        for entry in std::fs::read_dir(store_dir)? {
            let entry = entry?;
            let name  = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') { continue; }
            if !referenced.contains(&name) {
                std::fs::remove_dir_all(entry.path())?;
                crate::log::info(&format!("store: gc removed {}", name));
            }
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────
//  Helpers
// ─────────────────────────────────────────────────────────────

/// Hash the data.tar bytes → first 8 hex chars of sha256
fn hash_deb_content(data_bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data_bytes);
    let result = hasher.finalize();
    hex::encode(&result[..4])  // 8 hex chars
}

fn parse_store_name(name: &str, path: &Path) -> Option<StoreEntry> {
    // Format: pkgname-version-hash8
    // version can contain '-', hash is always 8 hex chars at the end
    let parts: Vec<&str> = name.rsplitn(2, '-').collect();
    if parts.len() < 2 { return None; }
    let hash    = parts[0].to_string();
    let rest    = parts[1];
    // Split rest into name + version at first '-' that's followed by a digit
    // or just use rsplit again
    let ver_parts: Vec<&str> = rest.rsplitn(2, '-').collect();
    if ver_parts.len() < 2 { return None; }
    let version = ver_parts[0].to_string();
    let pkg_name = ver_parts[1].to_string();

    Some(StoreEntry {
        name:    pkg_name,
         version,
         hash,
         path:    path.to_owned(),
    })
}
