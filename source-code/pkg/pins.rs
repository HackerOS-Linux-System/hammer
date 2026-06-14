use anyhow::Result;
use owo_colors::OwoColorize;
// FIX: removed unused `use std::collections::HashMap`
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const PINS_FILE: &str = "/etc/hammer/pins.hk";

// ─────────────────────────────────────────────────────────────
//  PinEntry
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinEntry {
    pub package:  String,
    pub version:  String,
    pub priority: i32,
    pub note:     Option<String>,
}

impl PinEntry {
    pub fn matches_version(&self, version: &str, installed_version: Option<&str>) -> bool {
        match self.version.as_str() {
            "*"         => true,
            "installed" => installed_version.map_or(false, |iv| iv == version),
            spec if spec.contains('*') => {
                let prefix = spec.trim_end_matches('*');
                version.starts_with(prefix)
            }
            spec if spec.starts_with(">=") => {
                let req = spec[2..].trim();
                crate::solver::version::compare(version, req) != std::cmp::Ordering::Less
            }
            spec if spec.starts_with("<=") => {
                let req = spec[2..].trim();
                crate::solver::version::compare(version, req) != std::cmp::Ordering::Greater
            }
            spec if spec.starts_with('>') => {
                let req = spec[1..].trim();
                crate::solver::version::compare(version, req) == std::cmp::Ordering::Greater
            }
            spec if spec.starts_with('<') => {
                let req = spec[1..].trim();
                crate::solver::version::compare(version, req) == std::cmp::Ordering::Less
            }
            exact => exact == version,
        }
    }

    pub fn matches_package(&self, name: &str) -> bool {
        if self.package.contains('*') {
            let prefix = self.package.trim_end_matches('*');
            name.starts_with(prefix)
        } else {
            self.package == name
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  PinDb
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct PinDb {
    pub entries: Vec<PinEntry>,
}

impl PinDb {
    pub fn load() -> Result<Self> {
        let path = Path::new(PINS_FILE);
        if !path.exists() { return Ok(Self::default()); }
        let content = std::fs::read_to_string(path)?;
        let mut db  = PinDb::default();
        db.parse_hk(&content);
        Ok(db)
    }

    fn parse_hk(&mut self, content: &str) {
        let mut current_pkg  = String::new();
        let mut current_ver  = "*".to_string();
        let mut current_prio = 500i32;
        let mut current_note = None::<String>;
        let mut in_section   = false;

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('!') { continue; }

            if line.starts_with('[') && line.ends_with(']') {
                if in_section && !current_pkg.is_empty() {
                    self.entries.push(PinEntry {
                        package:  current_pkg.clone(),
                                      version:  current_ver.clone(),
                                      priority: current_prio,
                                      note:     current_note.clone(),
                    });
                }
                current_pkg  = line[1..line.len()-1].trim().to_string();
                current_ver  = "*".to_string();
                current_prio = 500;
                current_note = None;
                in_section   = true;
                continue;
            }

            if in_section && line.starts_with("->") {
                let kv = line[2..].trim();
                if let Some((k, v)) = kv.split_once("=>") {
                    let k = k.trim();
                    let v = v.trim().trim_matches('"');
                    match k {
                        "version"  => current_ver  = v.to_string(),
                        "priority" => current_prio = v.parse().unwrap_or(500),
                        "note"     => current_note = Some(v.to_string()),
                        _          => {}
                    }
                }
            }
        }

        if in_section && !current_pkg.is_empty() {
            self.entries.push(PinEntry {
                package:  current_pkg,
                version:  current_ver,
                priority: current_prio,
                note:     current_note,
            });
        }
    }

    pub fn priority(&self, name: &str, version: &str, installed_version: Option<&str>) -> i32 {
        self.entries.iter()
        .filter(|e| e.matches_package(name) && e.matches_version(version, installed_version))
        .map(|e| e.priority)
        .max()
        .unwrap_or(500)
    }

    pub fn is_forbidden(&self, name: &str, version: &str) -> bool {
        self.priority(name, version, None) < 0
    }

    pub fn is_held(&self, name: &str) -> bool {
        self.entries.iter()
        .any(|e| e.matches_package(name) && e.priority >= 1000)
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all("/etc/hammer")?;
        let mut out = String::new();
        out.push_str("! hammer pins — auto-generated\n\n");
        for e in &self.entries {
            out.push_str(&format!("[{}]\n", e.package));
            out.push_str(&format!("-> version  => \"{}\"\n", e.version));
            out.push_str(&format!("-> priority => {}\n", e.priority));
            if let Some(ref note) = e.note {
                out.push_str(&format!("-> note     => \"{}\"\n", note));
            }
            out.push('\n');
        }
        let tmp = format!("{}.tmp", PINS_FILE);
        std::fs::write(&tmp, &out)?;
        std::fs::rename(&tmp, PINS_FILE)?;
        crate::log::info(&format!("pins: saved {} entries to {}", self.entries.len(), PINS_FILE));
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────
//  CLI
// ─────────────────────────────────────────────────────────────

pub fn cmd_pin(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");

    match sub {
        "list" | "ls" => {
            let db = PinDb::load()?;
            if db.entries.is_empty() {
                println!("  {} No pins configured.", "·".dimmed());
                println!("  Add: {}", "hammer pin add <pkg> <version> [priority]".cyan());
                return Ok(());
            }
            println!();
            println!("  {}  Package pins", "⬡".bright_cyan().bold());
            println!("  {}", "─".repeat(70).dimmed());
            println!("  {:<24} {:<20} {:<8} {}",
                     "Package".bold(), "Version".bold(), "Priority".bold(), "Note".bold());
            println!("  {}", "─".repeat(70).dimmed());
            for e in &db.entries {
                let prio_col = match e.priority {
                    p if p < 0    => p.to_string().red().to_string(),
                    p if p >= 1000=> p.to_string().bright_green().bold().to_string(),
                    p if p >= 500 => p.to_string().yellow().to_string(),
                    p             => p.to_string().dimmed().to_string(),
                };
                println!("  {:<24} {:<20} {:<12} {}",
                         e.package.bold(), e.version.cyan(), prio_col,
                         e.note.as_deref().unwrap_or("").dimmed());
            }
        }

        "add" => {
            let pkg  = args.get(1).ok_or_else(|| anyhow::anyhow!(
                "Usage: hammer pin add <package> <version> [priority] [note]"
            ))?;
            let ver  = args.get(2).cloned().unwrap_or_else(|| "*".to_string());
            let prio = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(1001i32);
            let note = args.get(4).cloned();
            let mut db = PinDb::load()?;
            db.entries.retain(|e| e.package != *pkg);
            db.entries.push(PinEntry { package: pkg.clone(), version: ver.clone(), priority: prio, note });
            db.save()?;
            println!("  {} Pinned {} to {} (priority {})",
                     "✔".bright_green(), pkg.bold(), ver.cyan(), prio);
        }

        "remove" | "rm" | "unpin" => {
            let pkg = args.get(1).ok_or_else(|| anyhow::anyhow!(
                "Usage: hammer pin remove <package>"
            ))?;
            let mut db = PinDb::load()?;
            let before = db.entries.len();
            db.entries.retain(|e| e.package != *pkg);
            if db.entries.len() < before {
                db.save()?;
                println!("  {} Unpinned {}.", "✔".bright_green(), pkg.bold());
            } else {
                println!("  {} No pin found for '{}'.", "·".dimmed(), pkg);
            }
        }

        "hold" => {
            let pkg = args.get(1).ok_or_else(|| anyhow::anyhow!(
                "Usage: hammer pin hold <package>"
            ))?;
            let mut db      = PinDb::load()?;
            let inst_ver    = crate::db::InstalledDb::open()?
            .get(pkg).map(|p| p.version);
            let ver         = inst_ver.as_deref().unwrap_or("installed").to_string();
            db.entries.retain(|e| e.package != *pkg);
            db.entries.push(PinEntry {
                package: pkg.clone(), version: ver.clone(),
                            priority: 1001, note: Some("held".to_string()),
            });
            db.save()?;
            println!("  {} {} held at version {}",
                     "✔".bright_green(), pkg.bold(), ver.cyan());
        }

        "unhold" => {
            let pkg = args.get(1).ok_or_else(|| anyhow::anyhow!(
                "Usage: hammer pin unhold <package>"
            ))?;
            let mut db = PinDb::load()?;
            db.entries.retain(|e| !(e.package == *pkg && e.note.as_deref() == Some("held")));
            db.save()?;
            println!("  {} {} released from hold.", "✔".bright_green(), pkg.bold());
        }

        other => anyhow::bail!(
            "Unknown pin subcommand '{}'. Try: list, add, remove, hold, unhold", other
        ),
    }
    Ok(())
}
