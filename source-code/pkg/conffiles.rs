use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const CONFFILES_DIR: &str = "/hammer/db/conffiles";

#[derive(Debug, Clone, PartialEq)]
pub enum ConffileStatus {
    Unchanged,
    Modified,
    Deleted,
    New,
}

#[derive(Debug, Clone)]
pub struct ConffileEntry {
    pub path:      PathBuf,
    pub package:   String,
    pub status:    ConffileStatus,
    pub orig_hash: Option<String>,
    pub curr_hash: Option<String>,
}

pub struct ConffileDb;

impl ConffileDb {
    pub fn record(pkg_name: &str, conffiles: &[(PathBuf, Vec<u8>)]) -> Result<()> {
        let base     = PathBuf::from(CONFFILES_DIR).join(pkg_name);
        let orig_dir = base.join("orig");
        std::fs::create_dir_all(&orig_dir)?;

        let mut list = String::new();
        for (path, content) in conffiles {
            list.push_str(&format!("{}\n", path.display()));
            let key = path_key(path);
            std::fs::write(orig_dir.join(&key), content)?;
        }
        std::fs::write(base.join("list"), &list)?;
        crate::log::info(&format!(
            "conffiles: recorded {} entries for {}", conffiles.len(), pkg_name
        ));
        Ok(())
    }

    /// Extract conffile originals from a deb package.
    pub fn extract_from_deb(deb: &crate::deb::DebPackage) -> Vec<(PathBuf, Vec<u8>)> {
        let conffiles_list = deb.extract_script("conffiles").unwrap_or_default();
        let mut result = Vec::new();

        let tmp = std::env::temp_dir()
        .join(format!("hammer_conffiles_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);

        // FIX E0308: deb.extract_data returns ExtractResult, not a tuple
        if let Ok(extract_result) = deb.extract_data(&tmp) {
            for line in conffiles_list.lines() {
                let path_str = line.trim();
                if path_str.is_empty() { continue; }
                let abs_path = PathBuf::from(path_str);
                let rel      = abs_path.strip_prefix("/").unwrap_or(&abs_path);
                let extracted = tmp.join(rel);
                if let Ok(content) = std::fs::read(&extracted) {
                    result.push((abs_path, content));
                }
                // suppress unused warning on extract_result fields
                let _ = &extract_result.regular_files;
            }
        }

        let _ = std::fs::remove_dir_all(&tmp);
        result
    }

    pub fn entries_for(pkg_name: &str) -> Vec<ConffileEntry> {
        let base      = PathBuf::from(CONFFILES_DIR).join(pkg_name);
        let list_path = base.join("list");
        let orig_dir  = base.join("orig");

        let Ok(list) = std::fs::read_to_string(&list_path) else { return vec![]; };

        list.lines()
        .filter(|l| !l.is_empty())
        .map(|line| {
            let path      = PathBuf::from(line.trim());
            let key       = path_key(&path);
            let orig_content = std::fs::read(orig_dir.join(&key)).ok();
            let orig_hash    = orig_content.as_ref().map(|c| sha256_hex(c));
            let curr_content = std::fs::read(&path).ok();
            let curr_hash    = curr_content.as_ref().map(|c| sha256_hex(c));

            let status = match (&orig_hash, &curr_hash) {
                (Some(o), Some(c)) if o == c => ConffileStatus::Unchanged,
             (Some(_), Some(_))            => ConffileStatus::Modified,
             (Some(_), None)               => ConffileStatus::Deleted,
             _                             => ConffileStatus::New,
            };

            ConffileEntry {
                path,
                package:   pkg_name.to_string(),
             status,
             orig_hash,
             curr_hash,
            }
        })
        .collect()
    }

    pub fn all_modified() -> Vec<ConffileEntry> {
        let base = Path::new(CONFFILES_DIR);
        if !base.exists() { return vec![]; }
        let Ok(dir) = std::fs::read_dir(base) else { return vec![]; };
        dir.flatten()
        .flat_map(|e| {
            let pkg = e.file_name().to_string_lossy().to_string();
            Self::entries_for(&pkg)
        })
        .filter(|e| e.status != ConffileStatus::Unchanged)
        .collect()
    }

    pub fn merge_upgrade(
        path:        &Path,
        old_content: &[u8],
        new_content: &[u8],
    ) -> Result<Option<MergeResult>> {
        if old_content == new_content { return Ok(None); }

        let curr_content = match std::fs::read(path) {
            Ok(c)  => c,
            Err(_) => return Ok(Some(MergeResult::UserDeleted)),
        };

        if sha256_hex(&curr_content) == sha256_hex(old_content) {
            return Ok(Some(MergeResult::AutoUpdated(new_content.to_vec())));
        }

        match three_way_merge(old_content, &curr_content, new_content) {
            Ok(merged) => Ok(Some(MergeResult::Merged(merged))),
            Err(_)     => Ok(Some(MergeResult::Conflict {
                current: curr_content,
                new:     new_content.to_vec(),
            })),
        }
    }

    pub fn apply_merge(path: &Path, result: &MergeResult) -> Result<()> {
        match result {
            MergeResult::AutoUpdated(content) => {
                if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
                std::fs::write(path, content)?;
                crate::log::info(&format!("conffiles: auto-updated {}", path.display()));
            }
            MergeResult::Merged(content) => {
                if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
                std::fs::write(path, content)?;
                crate::log::info(&format!("conffiles: merged {}", path.display()));
            }
            MergeResult::Conflict { new, .. } => {
                let new_path = path.with_extension("dpkg-new");
                std::fs::write(&new_path, new)?;
                crate::log::warn(&format!(
                    "conffiles: conflict in {} — new saved as {}",
                    path.display(), new_path.display()
                ));
            }
            MergeResult::UserDeleted => {
                crate::log::info(&format!(
                    "conffiles: {} was deleted by user, keeping deleted",
                    path.display()
                ));
            }
        }
        Ok(())
    }

    pub fn reset(path: &Path) -> Result<()> {
        let base = Path::new(CONFFILES_DIR);
        if !base.exists() {
            anyhow::bail!("No conffile database found.");
        }
        for entry in std::fs::read_dir(base)?.flatten() {
            let pkg       = entry.file_name().to_string_lossy().to_string();
            let list_path = entry.path().join("list");
            if let Ok(list) = std::fs::read_to_string(&list_path) {
                if list.lines().any(|l| PathBuf::from(l.trim()) == path) {
                    let key  = path_key(path);
                    let orig = entry.path().join("orig").join(&key);
                    if orig.exists() {
                        let content = std::fs::read(&orig)?;
                        if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
                        std::fs::write(path, &content)?;
                        println!("  {} Reset {} to original (from {})",
                                 "✔".bright_green(), path.display(), pkg.cyan());
                        crate::log::info(&format!(
                            "conffiles: reset {} to original from {}", path.display(), pkg
                        ));
                        return Ok(());
                    }
                }
            }
        }
        anyhow::bail!(
            "No conffile record found for {}.", path.display()
        )
    }
}

#[derive(Debug)]
pub enum MergeResult {
    AutoUpdated(Vec<u8>),
    Merged(Vec<u8>),
    Conflict { current: Vec<u8>, new: Vec<u8> },
    UserDeleted,
}

// ── CLI ───────────────────────────────────────────────────────

pub fn cmd_etc_diff() -> Result<()> {
    println!();
    println!("  {}  Modified conffiles", "⬡".bright_cyan().bold());
    println!("  {}", "─".repeat(70).dimmed());

    let modified = ConffileDb::all_modified();
    if modified.is_empty() {
        println!("  {} No conffiles modified.", "✔".bright_green());
        return Ok(());
    }

    println!("  {:<40} {:<12} {}",
             "File".bold(), "Status".bold(), "Package".bold());
    println!("  {}", "─".repeat(70).dimmed());

    for e in &modified {
        let status_col = match e.status {
            ConffileStatus::Modified => "modified".yellow().to_string(),
            ConffileStatus::Deleted  => "deleted".red().to_string(),
            ConffileStatus::New      => "new".bright_green().to_string(),
            ConffileStatus::Unchanged=> "unchanged".dimmed().to_string(),
        };
        println!("  {:<40} {:<20} {}",
                 e.path.display().to_string().bold(),
                 status_col,
                 e.package.cyan());
    }

    println!();
    println!("  {} file(s) modified.", modified.len().to_string().yellow().bold());
    println!("  Reset: {}", "hammer etc reset <file>".cyan());
    Ok(())
}

pub fn cmd_etc_reset(args: &[String]) -> Result<()> {
    let path_str = args.first().ok_or_else(|| anyhow::anyhow!(
        "Usage: hammer etc reset <file>"
    ))?;
    let path = PathBuf::from(path_str);
    if !path.is_absolute() {
        anyhow::bail!("Please provide an absolute path (e.g. /etc/ssh/sshd_config)");
    }
    ConffileDb::reset(&path)
}

pub fn cmd_etc_show(args: &[String]) -> Result<()> {
    let path_str = args.first().ok_or_else(|| anyhow::anyhow!(
        "Usage: hammer etc show <file>"
    ))?;
    let path = PathBuf::from(path_str);

    let base = Path::new(CONFFILES_DIR);
    for entry in std::fs::read_dir(base)?.flatten() {
        let list_path = entry.path().join("list");
        if let Ok(list) = std::fs::read_to_string(&list_path) {
            if list.lines().any(|l| PathBuf::from(l.trim()) == path) {
                let key       = path_key(&path);
                let orig_path = entry.path().join("orig").join(&key);
                let orig = std::fs::read_to_string(&orig_path)
                .unwrap_or_else(|_| "(binary or missing)".to_string());
                let curr = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| "(deleted)".to_string());

                println!("  {}  diff: {}", "⬡".bright_cyan().bold(), path.display());
                println!("  {}", "─".repeat(70).dimmed());

                let orig_lines: Vec<&str> = orig.lines().collect();
                let curr_lines: Vec<&str> = curr.lines().collect();
                print_simple_diff(&orig_lines, &curr_lines);
                return Ok(());
            }
        }
    }
    anyhow::bail!("File {} is not tracked as a conffile.", path.display())
}

// ── Helpers ───────────────────────────────────────────────────

fn path_key(path: &Path) -> String {
    sha256_hex(path.to_string_lossy().as_bytes())[..16].to_string()
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

fn three_way_merge(base: &[u8], ours: &[u8], theirs: &[u8]) -> Result<Vec<u8>> {
    let base_str   = std::str::from_utf8(base).context("base not UTF-8")?;
    let ours_str   = std::str::from_utf8(ours).context("current not UTF-8")?;
    let theirs_str = std::str::from_utf8(theirs).context("new not UTF-8")?;

    let base_lines:   Vec<&str> = base_str.lines().collect();
    let ours_lines:   Vec<&str> = ours_str.lines().collect();
    let theirs_lines: Vec<&str> = theirs_str.lines().collect();

    let mut result   = Vec::new();
    let mut conflict = false;
    let max = base_lines.len().max(ours_lines.len()).max(theirs_lines.len());

    for i in 0..max {
        let b = base_lines.get(i).copied();
        let o = ours_lines.get(i).copied();
        let t = theirs_lines.get(i).copied();

        match (b, o, t) {
            (_, Some(o), Some(t)) if o == t  => result.push(o.to_string()),
            (Some(b), Some(o), Some(t)) if o == b => result.push(t.to_string()),
            (Some(b), Some(o), Some(t)) if t == b => result.push(o.to_string()),
            (_, Some(o), None) => result.push(o.to_string()),
            (_, None, Some(t)) => result.push(t.to_string()),
            (_, Some(o), Some(t)) => {
                conflict = true;
                result.push("<<<<<<< current".to_string());
                result.push(o.to_string());
                result.push("=======".to_string());
                result.push(t.to_string());
                result.push(">>>>>>> package".to_string());
            }
            _ => {}
        }
    }

    if conflict {
        return Err(anyhow::anyhow!("conflict markers present"));
    }
    Ok((result.join("\n") + "\n").into_bytes())
}

fn print_simple_diff(orig: &[&str], curr: &[&str]) {
    let max = orig.len().max(curr.len());
    for i in 0..max {
        match (orig.get(i), curr.get(i)) {
            (Some(o), Some(c)) if o == c => println!("   {}", o),
            (Some(o), Some(c)) => {
                println!("  {} {}", "-".red(), o.red());
                println!("  {} {}", "+".bright_green(), c.bright_green());
            }
            (Some(o), None) => println!("  {} {}", "-".red(), o.red()),
            (None, Some(c)) => println!("  {} {}", "+".bright_green(), c.bright_green()),
            _ => {}
        }
    }
}
