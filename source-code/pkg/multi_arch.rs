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
