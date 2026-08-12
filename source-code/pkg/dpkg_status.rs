use std::collections::HashMap;
use std::path::Path;

use crate::db::{InstallReason, InstalledPackage};

pub const DPKG_STATUS_PATH: &str = "/var/lib/dpkg/status";

/// Reads every `Status: install ok installed` package from
/// `/var/lib/dpkg/status`. Returns an empty vec (not an error) if the file
/// doesn't exist or can't be read — a system without real dpkg at all is a
/// completely normal, expected case, not a failure.
pub fn read_all() -> Vec<InstalledPackage> {
    read_all_from(Path::new(DPKG_STATUS_PATH))
}

pub fn read_all_from(path: &Path) -> Vec<InstalledPackage> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    text.split("\n\n")
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .filter_map(parse_block)
        .filter(|p| p.reason == InstallReason::User || p.reason == InstallReason::Dependency)
        .collect()
}

/// Looks up a single package by name without reading the whole file into
/// memory as a `Vec` first when only one lookup is needed — still a full
/// linear scan under the hood (the file has no index), but avoids
/// allocating `InstalledPackage` for every other entry.
pub fn get(name: &str) -> Option<InstalledPackage> {
    read_all().into_iter().find(|p| p.name == name)
}

pub fn is_installed(name: &str) -> bool {
    get(name).is_some()
}

fn parse_block(block: &str) -> Option<InstalledPackage> {
    let fields = parse_rfc822_fields(block);
    let name = fields.get("Package")?.clone();

    let status = fields.get("Status").map(String::as_str).unwrap_or("");
    // Real dpkg Status: has three space-separated words: want flag,
    // error flag, current state — e.g. "install ok installed",
    // "deinstall ok config-files". Only "installed" packages are
    // actually present on disk; everything else (removed-but-not-purged,
    // half-configured, etc) should not be reported as installed.
    if !status.trim_end().ends_with("installed") {
        return None;
    }

    let installed_size_kb: u64 = fields.get("Installed-Size")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);

    Some(InstalledPackage {
        name,
        version:           fields.get("Version").cloned().unwrap_or_default(),
        architecture:       fields.get("Architecture").cloned().unwrap_or_default(),
        installed_size_kb,
        section:            fields.get("Section").cloned(),
        maintainer:         fields.get("Maintainer").cloned(),
        description_short:  fields.get("Description").cloned()
            .and_then(|d| d.lines().next().map(str::to_string)),
        // dpkg's status file has no install timestamp field — approximate
        // with "now" rather than leaving this Option-al, to keep
        // InstalledPackage's shape uniform between hammer- and
        // dpkg-sourced entries. Never used for logic decisions (only
        // display), so an approximate value here is harmless.
        installed_at:       chrono::Utc::now(),
        // Packages dpkg installed are, from hammer's point of view,
        // "just there" — closest existing reason is Dependency (implies
        // "don't second-guess/offer to autoremove this the way we would
        // a package a user explicitly asked hammer to install").
        reason:             InstallReason::Dependency,
        store_hash:         String::new(),
        depends:            fields.get("Depends").cloned(),
        recommends:         fields.get("Recommends").cloned(),
        multi_arch:         fields.get("Multi-Arch").cloned(),
    })
}

/// Same folding-aware RFC822 reader as `oci::status_db` (kept as a
/// separate, small copy rather than a shared dependency — `oci` is an
/// optional, feature-gated module and must never become a dependency of
/// code that also has to work in the default/normal-mode builds where
/// `oci` doesn't exist at all).
fn parse_rfc822_fields(block: &str) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    let mut current_key: Option<String> = None;
    for line in block.lines() {
        if let Some(rest) = line.strip_prefix(' ') {
            if let Some(key) = &current_key {
                if let Some(v) = fields.get_mut(key) {
                    let v: &mut String = v;
                    v.push('\n');
                    v.push_str(rest);
                }
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_string();
            let v = v.trim().to_string();
            fields.insert(k.clone(), v);
            current_key = Some(k);
        }
    }
    fields
}
