use anyhow::Result;
use crate::package::Package;
use crate::profile::GenerationsDb;

// ─────────────────────────────────────────────────────────────
//  GenDiff
// ─────────────────────────────────────────────────────────────

pub struct GenDiff {
    pub from:     u32,
    pub to:       u32,
    pub added:    Vec<Package>,
    pub removed:  Vec<String>,
    pub upgraded: Vec<(Package, String)>, // (new_pkg, old_version)
}

pub fn compute_diff(from: u32, to: u32, gdb: &GenerationsDb) -> Result<GenDiff> {
    let gen_from = gdb.get(from);
    let gen_to   = gdb.get(to);

    let pkgs_from: std::collections::HashMap<&str, &str> = gen_from
    .map(|g| g.packages.iter().map(|p| (p.name.as_str(), p.version.as_str())).collect())
    .unwrap_or_default();

    let pkgs_to: std::collections::HashMap<&str, &str> = gen_to
    .map(|g| g.packages.iter().map(|p| (p.name.as_str(), p.version.as_str())).collect())
    .unwrap_or_default();

    let mut added    = Vec::new();
    let mut removed  = Vec::new();
    let mut upgraded = Vec::new();

    for (name, new_ver) in &pkgs_to {
        if let Some(old_ver) = pkgs_from.get(name) {
            if old_ver != new_ver {
                // Use Package::default() with struct update syntax
                let pkg = Package {
                    name:    name.to_string(),
                    version: new_ver.to_string(),
                    ..Package::default()
                };
                upgraded.push((pkg, old_ver.to_string()));
            }
        } else {
            let pkg = Package {
                name:    name.to_string(),
                version: new_ver.to_string(),
                ..Package::default()
            };
            added.push(pkg);
        }
    }

    for name in pkgs_from.keys() {
        if !pkgs_to.contains_key(name) {
            removed.push(name.to_string());
        }
    }

    added.sort_by(|a, b| a.name.cmp(&b.name));
    removed.sort();
    upgraded.sort_by(|a, b| a.0.name.cmp(&b.0.name));

    Ok(GenDiff { from, to, added, removed, upgraded })
}
