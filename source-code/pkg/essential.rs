use anyhow::{bail, Result};
use owo_colors::OwoColorize;
use std::collections::HashSet;

/// Hardcoded always-essential packages regardless of control field.
/// These are required for the system to be minimally functional.
pub const HAMMER_CRITICAL: &[&str] = &[
    "bash", "dash", "coreutils", "util-linux", "grep", "sed", "awk",
"gawk", "findutils", "diffutils", "tar", "gzip", "bzip2", "xz-utils",
"libc6", "libgcc-s1", "libstdc++6",
"systemd", "systemd-sysv", "udev",
"login", "passwd", "shadow-utils",
"apt-utils", // keep for dpkg triggers
"hammer",    // self-protection
];

// ─────────────────────────────────────────────────────────────
//  EssentialDb
// ─────────────────────────────────────────────────────────────

pub struct EssentialDb {
    /// Package names marked Essential: yes
    essential: HashSet<String>,
    /// Package names marked Required priority
    required:  HashSet<String>,
}

impl EssentialDb {
    /// Build from the package cache.
    pub fn build(cache: &crate::cache::PackageCache) -> Self {
        let mut essential = HashSet::new();
        let mut required  = HashSet::new();

        for pkg in cache.all_packages() {
            if let Some(ref prio) = pkg.priority {
                if prio == "required" {
                    required.insert(pkg.name.clone());
                }
            }
            // Check Essential field via raw control — not in our Package struct yet,
            // but we can check the section/priority heuristically.
            // For now: mark anything in HAMMER_CRITICAL as essential too.
        }

        // Always mark hardcoded critical packages
        for &pkg in HAMMER_CRITICAL {
            essential.insert(pkg.to_string());
        }

        EssentialDb { essential, required }
    }

    pub fn is_essential(&self, name: &str) -> bool {
        self.essential.contains(name) || HAMMER_CRITICAL.contains(&name)
    }

    pub fn is_required(&self, name: &str) -> bool {
        self.required.contains(name)
    }

    pub fn is_protected(&self, name: &str) -> bool {
        self.is_essential(name) || self.is_required(name)
    }
}

// ─────────────────────────────────────────────────────────────
//  Guard function — call before any remove transaction
// ─────────────────────────────────────────────────────────────

/// Check if any package in `to_remove` is essential.
/// Returns Err with a clear message unless `force` is true.
pub fn guard_essential_removal(
    to_remove:       &[String],
    force_essential: bool,
        cache:           &crate::cache::PackageCache,
) -> Result<()> {
    let db       = EssentialDb::build(cache);
    let mut blocked = Vec::new();

    for name in to_remove {
        if db.is_protected(name) {
            blocked.push(name.clone());
        }
    }

    if blocked.is_empty() { return Ok(()); }

    if force_essential {
        eprintln!();
        eprintln!("  {} --force-essential: removing protected package(s): {}",
                  "!".red().bold(),
                  blocked.iter().map(|s| s.red().bold().to_string())
                  .collect::<Vec<_>>().join(", "));
        eprintln!("  {} This may make your system unbootable.",
                  "WARNING:".red().bold());
        eprintln!();
        return Ok(()); // allowed despite warning
    }

    bail!(
        "Cannot remove essential/required package(s): {}\n  \
These packages are critical for system operation.\n  \
Use --force-essential to override (DANGEROUS).",
          blocked.iter()
          .map(|s| format!("'{}'", s))
          .collect::<Vec<_>>().join(", ")
    )
}

/// List all essential packages currently installed.
pub fn cmd_list_essential() -> Result<()> {
    let db_inst = crate::db::InstalledDb::open()?;
    let cache   = crate::cache::PackageCache::load()?;
    let ess_db  = EssentialDb::build(&cache);

    println!();
    println!("  {}  Essential/Required packages", "⬡".bright_cyan().bold());
    println!("  {}", "─".repeat(60).dimmed());

    let all = db_inst.list_all()?;
    let mut shown = 0;

    for pkg in &all {
        if ess_db.is_protected(&pkg.name) {
            let reason = if ess_db.is_essential(&pkg.name) { "essential" }
            else { "required" };
            println!("  {} {:<32} {:<12} {}",
                     "●".bright_green(),
                     pkg.name.bold(),
                     pkg.version.cyan(),
                     reason.dimmed());
            shown += 1;
        }
    }

    // Also show critical packages not yet installed
    for &name in HAMMER_CRITICAL {
        if !db_inst.is_installed(name) {
            println!("  {} {:<32} {:<12} {}",
                     "○".yellow(),
                     name.bold(),
                     "NOT INSTALLED".red().bold(),
                     "critical".red());
        }
    }

    println!();
    println!("  {} essential/required package(s) installed.", shown.to_string().cyan());
    Ok(())
}
