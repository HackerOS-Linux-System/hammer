use anyhow::Result;
use owo_colors::OwoColorize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::profile::GenerationsDb;

// ─────────────────────────────────────────────────────────────
//  Types
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PkgChange {
    pub name:     String,
    pub kind:     ChangeKind,
    pub from_ver: Option<String>,
    pub to_ver:   Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChangeKind { Added, Removed, Upgraded, Downgraded }

impl ChangeKind {
    pub fn label(&self) -> &'static str {
        match self {
            ChangeKind::Added      => "added",
            ChangeKind::Removed    => "removed",
            ChangeKind::Upgraded   => "upgraded",
            ChangeKind::Downgraded => "downgraded",
        }
    }
}

#[derive(Debug)]
pub struct FileChange {
    pub path:  String,
    pub kind:  FileChangeKind,
}

#[derive(Debug, PartialEq)]
pub enum FileChangeKind { Added, Removed, Modified }

#[derive(Debug)]
pub struct GenDiff {
    pub from:         u32,
    pub to:           u32,
    pub pkg_changes:  Vec<PkgChange>,
    pub file_changes: Vec<FileChange>,
    pub conf_changes: Vec<FileChange>,
}

impl GenDiff {
    pub fn is_empty(&self) -> bool {
        self.pkg_changes.is_empty() && self.file_changes.is_empty()
    }

    pub fn n_added(&self)    -> usize { self.pkg_changes.iter().filter(|c| c.kind == ChangeKind::Added).count() }
    pub fn n_removed(&self)  -> usize { self.pkg_changes.iter().filter(|c| c.kind == ChangeKind::Removed).count() }
    pub fn n_upgraded(&self) -> usize { self.pkg_changes.iter().filter(|c| c.kind == ChangeKind::Upgraded || c.kind == ChangeKind::Downgraded).count() }
}

// ─────────────────────────────────────────────────────────────
//  Compute diff
// ─────────────────────────────────────────────────────────────

pub fn compute_diff(from: u32, to: u32, gdb: &GenerationsDb) -> Result<GenDiff> {
    let gen_from = gdb.get(from);
    let gen_to   = gdb.get(to);

    let pkgs_from: HashMap<&str, &str> = gen_from
        .map(|g| g.packages.iter().map(|p| (p.name.as_str(), p.version.as_str())).collect())
        .unwrap_or_default();

    let pkgs_to: HashMap<&str, &str> = gen_to
        .map(|g| g.packages.iter().map(|p| (p.name.as_str(), p.version.as_str())).collect())
        .unwrap_or_default();

    let mut pkg_changes = Vec::new();

    // Added / upgraded / downgraded
    for (name, new_ver) in &pkgs_to {
        if let Some(old_ver) = pkgs_from.get(name) {
            if old_ver != new_ver {
                let ord = crate::solver::version::compare(new_ver, old_ver);
                pkg_changes.push(PkgChange {
                    name:     name.to_string(),
                    kind:     if ord == std::cmp::Ordering::Greater {
                        ChangeKind::Upgraded
                    } else {
                        ChangeKind::Downgraded
                    },
                    from_ver: Some(old_ver.to_string()),
                    to_ver:   Some(new_ver.to_string()),
                });
            }
        } else {
            pkg_changes.push(PkgChange {
                name:     name.to_string(),
                kind:     ChangeKind::Added,
                from_ver: None,
                to_ver:   Some(new_ver.to_string()),
            });
        }
    }

    // Removed
    for (name, old_ver) in &pkgs_from {
        if !pkgs_to.contains_key(name) {
            pkg_changes.push(PkgChange {
                name:     name.to_string(),
                kind:     ChangeKind::Removed,
                from_ver: Some(old_ver.to_string()),
                to_ver:   None,
            });
        }
    }

    pkg_changes.sort_by(|a, b| a.name.cmp(&b.name));

    // File-level diff (compare store contents)
    let file_changes = compute_file_diff(from, to, gdb);
    let conf_changes = compute_conf_diff(from, to);

    Ok(GenDiff { from, to, pkg_changes, file_changes, conf_changes })
}

fn compute_file_diff(from: u32, to: u32, gdb: &GenerationsDb) -> Vec<FileChange> {
    let store = crate::store::STORE_DIR;
    let mut changes = Vec::new();

    let files_from = collect_store_files(from, gdb, store);
    let files_to   = collect_store_files(to,   gdb, store);

    let all_paths: HashSet<&String> = files_from.keys().chain(files_to.keys()).collect();

    for path in all_paths {
        match (files_from.get(path), files_to.get(path)) {
            (None, Some(_))    => changes.push(FileChange { path: path.clone(), kind: FileChangeKind::Added }),
            (Some(_), None)    => changes.push(FileChange { path: path.clone(), kind: FileChangeKind::Removed }),
            (Some(h1), Some(h2)) if h1 != h2
                               => changes.push(FileChange { path: path.clone(), kind: FileChangeKind::Modified }),
            _                  => {}
        }
    }
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    changes
}

fn collect_store_files(gen_num: u32, gdb: &GenerationsDb, store: &str) -> HashMap<String, u64> {
    let mut map = HashMap::new();
    let Some(gen) = gdb.get(gen_num) else { return map; };
    for pkg in &gen.packages {
        let dir = Path::new(store).join(format!("{}-{}-{}", pkg.name, pkg.version, pkg.store_hash));
        collect_files_recursive(&dir, &dir, &mut map);
    }
    map
}

fn collect_files_recursive(base: &Path, dir: &Path, map: &mut HashMap<String, u64>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return; };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            collect_files_recursive(base, &path, map);
        } else {
            let rel = path.strip_prefix(base).unwrap_or(&path)
                .to_string_lossy().to_string();
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            map.insert(rel, size);
        }
    }
}

fn compute_conf_diff(_from: u32, _to: u32) -> Vec<FileChange> {
    // Compare /etc changes tracked by conffile DB
    // Stub: real impl would use ConffileDb hashes
    Vec::new()
}

// ─────────────────────────────────────────────────────────────
//  Print diff
// ─────────────────────────────────────────────────────────────

pub fn print_diff(diff: &GenDiff, full: bool, json: bool) -> Result<()> {
    if json {
        return print_diff_json(diff);
    }

    println!();
    println!("  {}  Diff: gen-{} → gen-{}", "⬡".bright_cyan().bold(), diff.from, diff.to);
    println!("  {}", "─".repeat(60).dimmed());

    if diff.is_empty() {
        println!("  {} No changes between gen-{} and gen-{}.", "·".dimmed(), diff.from, diff.to);
        return Ok(());
    }

    // Package changes
    for c in &diff.pkg_changes {
        let (sym, line) = match c.kind {
            ChangeKind::Added =>
                ("++".bright_green().to_string(),
                 format!("{} → {}", c.name.bold(), c.to_ver.as_deref().unwrap_or("?").green())),
            ChangeKind::Removed =>
                ("--".red().to_string(),
                 format!("{} {}", c.name.bold(), c.from_ver.as_deref().unwrap_or("?").dimmed())),
            ChangeKind::Upgraded =>
                ("~~".bright_yellow().to_string(),
                 format!("{}: {} → {}",
                     c.name.bold(),
                     c.from_ver.as_deref().unwrap_or("?").dimmed(),
                     c.to_ver.as_deref().unwrap_or("?").bright_yellow())),
            ChangeKind::Downgraded =>
                ("↓↓".red().to_string(),
                 format!("{}: {} → {}",
                     c.name.bold(),
                     c.from_ver.as_deref().unwrap_or("?").dimmed(),
                     c.to_ver.as_deref().unwrap_or("?").red())),
        };
        println!("  {}  {}", sym, line);
    }

    // File-level summary
    if full && !diff.file_changes.is_empty() {
        println!();
        println!("  {}", "File changes:".bold());
        for fc in &diff.file_changes {
            let (sym, col): (&str, _) = match fc.kind {
                FileChangeKind::Added    => ("+", fc.path.green().to_string()),
                FileChangeKind::Removed  => ("-", fc.path.red().to_string()),
                FileChangeKind::Modified => ("~", fc.path.yellow().to_string()),
            };
            println!("  {} {}", sym.dimmed(), col);
        }
    }

    // Summary stats
    println!();
    println!("  {} +{} upgraded:{} removed:{}",
             "Summary:".bold(),
             diff.n_added().to_string().green(),
             diff.n_upgraded().to_string().yellow(),
             diff.n_removed().to_string().red());

    if !full && !diff.file_changes.is_empty() {
        println!("  {} {} file change(s). Use {} for details.",
                 "·".dimmed(), diff.file_changes.len(), "--full".cyan());
    }
    Ok(())
}

fn print_diff_json(diff: &GenDiff) -> Result<()> {
    let pkg_changes: Vec<serde_json::Value> = diff.pkg_changes.iter().map(|c| {
        serde_json::json!({
            "name":     c.name,
            "kind":     c.kind.label(),
            "from_ver": c.from_ver,
            "to_ver":   c.to_ver,
        })
    }).collect();

    let file_changes: Vec<serde_json::Value> = diff.file_changes.iter().map(|c| {
        let kind = match c.kind {
            FileChangeKind::Added    => "added",
            FileChangeKind::Removed  => "removed",
            FileChangeKind::Modified => "modified",
        };
        serde_json::json!({ "path": c.path, "kind": kind })
    }).collect();

    let out = serde_json::json!({
        "from": diff.from,
        "to":   diff.to,
        "packages": pkg_changes,
        "files":    file_changes,
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
