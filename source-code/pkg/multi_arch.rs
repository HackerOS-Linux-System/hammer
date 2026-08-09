use anyhow::{bail, Result};
use owo_colors::OwoColorize;
// FIX: removed unused `use std::collections::HashSet` — this module
// no longer constructs any HashSet directly (it was left over from an
// earlier draft of `all_arches`/dedup logic that now uses Vec only).

// ─────────────────────────────────────────────────────────────
//  MultiArchMode
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MultiArchMode {
    No,
    Same,
    Foreign,
    Allowed,
}

impl MultiArchMode {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "same"    => MultiArchMode::Same,
            "foreign" => MultiArchMode::Foreign,
            "allowed" => MultiArchMode::Allowed,
            _         => MultiArchMode::No,
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  Parse name:arch from user input
// ─────────────────────────────────────────────────────────────

pub fn parse_pkg_spec(spec: &str) -> (String, Option<String>) {
    if let Some((name, arch)) = spec.rsplit_once(':') {
        if arch.chars().all(|c| c.is_alphanumeric() || c == '_') && arch.len() <= 10 {
            return (name.to_string(), Some(arch.to_string()));
        }
    }
    (spec.to_string(), None)
}

// ─────────────────────────────────────────────────────────────
//  MultiArchDb
// ─────────────────────────────────────────────────────────────

const FOREIGN_ARCHES_FILE: &str = "/etc/hammer/foreign-arches";

#[derive(Debug, Default)]
pub struct MultiArchDb {
    pub foreign_arches: Vec<String>,
}

impl MultiArchDb {
    pub fn load() -> Self {
        let native = crate::cache::detect_arch();
        let mut db = MultiArchDb::default();
        if let Ok(content) = std::fs::read_to_string(FOREIGN_ARCHES_FILE) {
            for line in content.lines() {
                let arch = line.trim();
                if !arch.is_empty() && arch != &native {
                    db.foreign_arches.push(arch.to_string());
                }
            }
        }
        db
    }

    pub fn add_arch(&mut self, arch: &str) -> Result<()> {
        if crate::userenv::normalise_arch(arch).is_err() {
            bail!("Unknown architecture: '{}'", arch);
        }
        if !self.foreign_arches.contains(&arch.to_string()) {
            self.foreign_arches.push(arch.to_string());
            self.save()?;
        }
        Ok(())
    }

    pub fn remove_arch(&mut self, arch: &str) -> Result<()> {
        self.foreign_arches.retain(|a| a != arch);
        self.save()?;
        Ok(())
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all("/etc/hammer")?;
        std::fs::write(FOREIGN_ARCHES_FILE, self.foreign_arches.join("\n") + "\n")?;
        Ok(())
    }

    pub fn all_arches(&self) -> Vec<String> {
        let native = crate::cache::detect_arch();
        let mut arches = vec![native];
        arches.extend(self.foreign_arches.clone());
        arches
    }

    pub fn supports_arch(&self, arch: &str) -> bool {
        let native = crate::cache::detect_arch();
        arch == "all" || arch == native || self.foreign_arches.iter().any(|a| a == arch)
    }

    /// Return the Multi-Arch mode for a package name, read from its
    /// `multi_arch` DB column (populated from the `Multi-Arch:` control
    /// field at install time). Returns `None` if the package isn't
    /// installed or has no `Multi-Arch:` field (which means "no" per
    /// Debian policy — callers should treat `None` the same as
    /// `Some(MultiArchMode::No)`).
    pub fn get_mode(&self, pkg_name: &str) -> Option<MultiArchMode> {
        let db = crate::db::InstalledDb::open().ok()?;
        let installed = db.get(pkg_name)?;
        installed.multi_arch.map(|v| MultiArchMode::parse(&v))
    }
}

// ─────────────────────────────────────────────────────────────
//  Dependency satisfaction across architectures
// ─────────────────────────────────────────────────────────────

pub fn can_satisfy_dep(
    candidate_arch:       &str,
    candidate_multi_arch: &MultiArchMode,
    requirer_arch:        &str,
) -> bool {
    match candidate_arch {
        "all" | "any" => return true,
        _ => {}
    }
    match candidate_multi_arch {
        MultiArchMode::Foreign | MultiArchMode::Allowed => true,
        MultiArchMode::Same | MultiArchMode::No => candidate_arch == requirer_arch,
    }
}

// ─────────────────────────────────────────────────────────────
//  CLI: hammer arch list/add/remove
// ─────────────────────────────────────────────────────────────

pub fn cmd_arch(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" | "ls" => {
            let db     = MultiArchDb::load();
            let native = crate::cache::detect_arch();
            println!();
            println!("  {}  Configured architectures", "⬡".bright_cyan().bold());
            println!("  {}", "─".repeat(50).dimmed());
            println!("  {} {} (native)", "●".bright_green(), native.bold());
            if db.foreign_arches.is_empty() {
                println!("  {} No foreign architectures configured.", "·".dimmed());
            } else {
                for arch in &db.foreign_arches {
                    println!("  {} {} (foreign)", "○".cyan(), arch.bold());
                }
            }
            println!();
            println!("  Add:    {}", "hammer arch add i386".cyan());
            println!("  Remove: {}", "hammer arch remove i386".cyan());
        }
        "add" => {
            let arch = args.get(1)
            .ok_or_else(|| anyhow::anyhow!("Usage: hammer arch add <arch>"))?;
            let mut db = MultiArchDb::load();
            db.add_arch(arch)?;
            println!("  {} Added foreign architecture '{}'.", "✔".bright_green(), arch.bold());
            println!("  Run {} to fetch packages for this architecture.",
                     "hammer sync".cyan());
        }
        "remove" => {
            let arch = args.get(1)
            .ok_or_else(|| anyhow::anyhow!("Usage: hammer arch remove <arch>"))?;
            let mut db = MultiArchDb::load();
            db.remove_arch(arch)?;
            println!("  {} Removed foreign architecture '{}'.", "✔".bright_green(), arch.bold());
        }
        other => anyhow::bail!(
            "Unknown arch subcommand '{}'. Try: list, add, remove", other
        ),
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Store path for arch-qualified packages
// ─────────────────────────────────────────────────────────────

pub fn store_path_multi_arch(
    store_dir: &std::path::Path,
    name:      &str,
    version:   &str,
    arch:      &str,
    hash:      &str,
) -> std::path::PathBuf {
    let native = crate::cache::detect_arch();
    if arch == "all" || arch == native || arch.is_empty() {
        store_dir.join(format!("{}-{}-{}", name, version, hash))
    } else {
        store_dir.join(format!("{}-{}-{}-{}", name, arch, version, hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pkg_spec() {
        assert_eq!(parse_pkg_spec("curl"),       ("curl".to_string(), None));
        assert_eq!(parse_pkg_spec("curl:amd64"), ("curl".to_string(), Some("amd64".to_string())));
        assert_eq!(parse_pkg_spec("libc6:i386"), ("libc6".to_string(), Some("i386".to_string())));
        assert_eq!(parse_pkg_spec("python3.11"), ("python3.11".to_string(), None));
    }

    #[test]
    fn test_can_satisfy() {
        assert!(can_satisfy_dep("i386",  &MultiArchMode::Foreign, "amd64"));
        assert!(can_satisfy_dep("i386",  &MultiArchMode::Foreign, "i386"));
        assert!(can_satisfy_dep("amd64", &MultiArchMode::Same,    "amd64"));
        assert!(!can_satisfy_dep("i386", &MultiArchMode::Same,    "amd64"));
        assert!(can_satisfy_dep("all",   &MultiArchMode::No,      "amd64"));
        assert!(can_satisfy_dep("all",   &MultiArchMode::No,      "i386"));
    }
}

// ─────────────────────────────────────────────────────────────
//  cmd_dpkg_arch — hammer dpkg-arch (mimic dpkg --print-architecture)
// ─────────────────────────────────────────────────────────────

pub fn cmd_dpkg_arch(args: &[String]) -> anyhow::Result<()> {
    use owo_colors::OwoColorize;

    let sub = args.first().map(|s| s.as_str()).unwrap_or("print");
    match sub {
        "--print-architecture" | "print" | "" => {
            println!("{}", crate::cache::detect_arch());
        }
        "--print-foreign-architectures" | "foreign" => {
            let db = MultiArchDb::load();
            for arch in &db.foreign_arches { println!("{}", arch); }
        }
        "--add-architecture" | "add" => {
            let arch = args.get(1)
                .ok_or_else(|| anyhow::anyhow!("Usage: hammer dpkg-arch add <arch>"))?;
            let mut db = MultiArchDb::load();
            db.add_arch(arch)?;
            println!("  {} Added {}", "✔".bright_green(), arch.bold());
        }
        "--remove-architecture" | "remove" => {
            let arch = args.get(1)
                .ok_or_else(|| anyhow::anyhow!("Usage: hammer dpkg-arch remove <arch>"))?;
            let mut db = MultiArchDb::load();
            db.remove_arch(arch)?;
            println!("  {} Removed {}", "✔".bright_green(), arch.bold());
        }
        "--assert-multi-arch" | "assert" => {
            let db = MultiArchDb::load();
            if db.foreign_arches.is_empty() {
                anyhow::bail!("Multi-arch is not enabled");
            }
        }
        other => {
            anyhow::bail!(
                "Unknown dpkg-arch subcommand '{}'\n  \
                 Usage: hammer dpkg-arch [print|foreign|add <arch>|remove <arch>|assert]",
                other
            );
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Conflict resolution for multi-arch packages (0.6)
//
//  Debian multi-arch conflict semantics:
//    `Conflicts: pkg`         → conflicts with pkg of ANY architecture
//    `Conflicts: pkg:amd64`   → conflicts only with pkg:amd64
//    `Conflicts: pkg:any`     → conflicts with pkg of the same arch as requirer
//
//  `Multi-Arch: Same` packages additionally conflict with themselves
//  installed for a different architecture (co-install requires same version).
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct MultiArchConflict {
    /// Package that has the Conflicts: field
    pub requirer:      String,
    pub requirer_arch: String,
    /// Package that is conflicted with
    pub conflicting:      String,
    pub conflicting_arch: Option<String>,
    pub reason:           ConflictReason,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConflictReason {
    /// Conflicts: pkg (any-arch conflict)
    AnyArch,
    /// Conflicts: pkg:amd64 (specific arch)
    SpecificArch,
    /// Multi-Arch: Same, different version installed for another arch
    SameVersionRequired { installed_version: String, required_version: String },
}

/// Installed package view used for conflict checking (minimal interface).
pub trait InstalledView {
    fn installed_arches(&self, name: &str) -> Vec<(String, String)>; // (arch, version)
}

/// Check whether installing `new_pkg` (with its conflict list) would
/// violate any multi-arch constraint against `installed`.
///
/// Returns a list of conflicts found. Empty = no conflicts.
pub fn check_multi_arch_conflicts(
    new_name:     &str,
    new_arch:     &str,
    new_version:  &str,
    new_ma_mode:  &MultiArchMode,
    new_conflicts: &[(String, Option<String>)], // (pkg_name, optional_arch)
    installed:    &dyn InstalledView,
) -> Vec<MultiArchConflict> {
    let mut found = Vec::new();

    for (conf_name, conf_arch) in new_conflicts {
        let installed_for_pkg = installed.installed_arches(conf_name);
        for (inst_arch, _inst_ver) in &installed_for_pkg {
            let conflicts = match conf_arch.as_deref() {
                // `Conflicts: pkg` — conflicts with any arch
                None | Some("any") => true,
                // `Conflicts: pkg:amd64` — only that specific arch
                Some(specific) => inst_arch == specific,
            };
            if conflicts {
                found.push(MultiArchConflict {
                    requirer:         new_name.to_string(),
                    requirer_arch:    new_arch.to_string(),
                    conflicting:      conf_name.clone(),
                    conflicting_arch: Some(inst_arch.clone()),
                    reason: match conf_arch.as_deref() {
                        None        => ConflictReason::AnyArch,
                        Some("any") => ConflictReason::AnyArch,
                        _           => ConflictReason::SpecificArch,
                    },
                });
            }
        }
    }

    // Multi-Arch: Same — all installed arches must have the same version
    if *new_ma_mode == MultiArchMode::Same {
        let installed_arches = installed.installed_arches(new_name);
        for (inst_arch, inst_ver) in &installed_arches {
            if inst_arch == new_arch { continue; } // same arch, different install = upgrade
            if inst_ver != new_version {
                found.push(MultiArchConflict {
                    requirer:         new_name.to_string(),
                    requirer_arch:    new_arch.to_string(),
                    conflicting:      new_name.to_string(),
                    conflicting_arch: Some(inst_arch.clone()),
                    reason: ConflictReason::SameVersionRequired {
                        installed_version: inst_ver.clone(),
                        required_version:  new_version.to_string(),
                    },
                });
            }
        }
    }

    found
}

/// Format a conflict list for display (used by solver/transaction).
pub fn format_conflicts(conflicts: &[MultiArchConflict]) -> String {
    conflicts.iter().map(|c| {
        match &c.reason {
            ConflictReason::AnyArch | ConflictReason::SpecificArch => {
                format!(
                    "  {}:{} conflicts with {}:{}",
                    c.requirer,
                    c.requirer_arch,
                    c.conflicting,
                    c.conflicting_arch.as_deref().unwrap_or("*")
                )
            }
            ConflictReason::SameVersionRequired { installed_version, required_version } => {
                format!(
                    "  {}:{} (Multi-Arch: Same) requires version {} but {}:{} has {}",
                    c.requirer, c.requirer_arch,
                    required_version,
                    c.conflicting,
                    c.conflicting_arch.as_deref().unwrap_or("?"),
                    installed_version
                )
            }
        }
    }).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod multi_arch_conflict_tests {
    use super::*;

    struct MockInstalled(Vec<(String, String, String)>); // (name, arch, version)

    impl InstalledView for MockInstalled {
        fn installed_arches(&self, name: &str) -> Vec<(String, String)> {
            self.0.iter()
                .filter(|(n, _, _)| n == name)
                .map(|(_, a, v)| (a.clone(), v.clone()))
                .collect()
        }
    }

    #[test]
    fn test_any_arch_conflict() {
        let installed = MockInstalled(vec![
            ("libssl1.1".to_string(), "i386".to_string(), "1.1.1".to_string()),
        ]);
        let conflicts = check_multi_arch_conflicts(
            "libssl1.1", "amd64", "1.1.1",
            &MultiArchMode::No,
            &[("libssl1.1".to_string(), None)],
            &installed,
        );
        assert!(!conflicts.is_empty());
        assert_eq!(conflicts[0].reason, ConflictReason::AnyArch);
    }

    #[test]
    fn test_same_version_conflict() {
        let installed = MockInstalled(vec![
            ("curl".to_string(), "i386".to_string(), "7.88.0".to_string()),
        ]);
        let conflicts = check_multi_arch_conflicts(
            "curl", "amd64", "8.0.0",
            &MultiArchMode::Same,
            &[],
            &installed,
        );
        assert!(!conflicts.is_empty());
        match &conflicts[0].reason {
            ConflictReason::SameVersionRequired { installed_version, required_version } => {
                assert_eq!(installed_version, "7.88.0");
                assert_eq!(required_version, "8.0.0");
            }
            _ => panic!("Expected SameVersionRequired"),
        }
    }

    #[test]
    fn test_no_conflict_when_same_version() {
        let installed = MockInstalled(vec![
            ("curl".to_string(), "i386".to_string(), "8.0.0".to_string()),
        ]);
        let conflicts = check_multi_arch_conflicts(
            "curl", "amd64", "8.0.0",
            &MultiArchMode::Same,
            &[],
            &installed,
        );
        assert!(conflicts.is_empty());
    }
}
