use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use sha2::{Digest, Sha256};
// FIX: removed unused `use std::collections::HashMap` and `use std::sync::{Arc, Mutex}`
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::store::STORE_DIR;

// ─────────────────────────────────────────────────────────────
//  IntegrityEntry
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct IntegrityEntry {
    pub path:         PathBuf,
    pub expected_sha: Option<String>,
    pub actual_sha:   Option<String>,
    pub status:       IntegrityStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IntegrityStatus {
    Ok,
    Missing,
    Corrupted,
    DanglingSymlink,
    Unknown,
}

impl IntegrityStatus {
    pub fn is_ok(&self) -> bool { *self == IntegrityStatus::Ok }
}

// ─────────────────────────────────────────────────────────────
//  Verify store for one package
// ─────────────────────────────────────────────────────────────

pub fn verify_package(
    name:       &str,
    version:    &str,
    store_hash: &str,
    deep:       bool,
) -> Vec<IntegrityEntry> {
    let store_path = PathBuf::from(STORE_DIR)
    .join(format!("{}-{}-{}", name, version, store_hash));

    if !store_path.exists() {
        return vec![IntegrityEntry {
            path:         store_path,
            expected_sha: None,
            actual_sha:   None,
            status:       IntegrityStatus::Missing,
        }];
    }

    let mut entries = Vec::new();

    for item in walkdir::WalkDir::new(&store_path)
        .min_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        {
            let path = item.path().to_path_buf();

            let status = if item.file_type().is_symlink() {
                match std::fs::read_link(&path) {
                    Ok(target) => {
                        if target.exists() || target.symlink_metadata().is_ok() {
                            IntegrityStatus::Ok
                        } else {
                            IntegrityStatus::DanglingSymlink
                        }
                    }
                    Err(_) => IntegrityStatus::DanglingSymlink,
                }
            } else if item.file_type().is_file() {
                if deep {
                    match hash_file(&path) {
                        Ok(hash) => {
                            entries.push(IntegrityEntry {
                                path:         path.clone(),
                                         expected_sha: None,
                                         actual_sha:   Some(hash),
                                         status:       IntegrityStatus::Ok,
                            });
                            continue;
                        }
                        Err(_) => IntegrityStatus::Corrupted,
                    }
                } else if path.exists() {
                    IntegrityStatus::Ok
                } else {
                    IntegrityStatus::Missing
                }
            } else {
                IntegrityStatus::Ok
            };

            if status != IntegrityStatus::Ok {
                entries.push(IntegrityEntry {
                    path,
                    expected_sha: None,
                    actual_sha:   None,
                    status,
                });
            }
        }

        entries
}

// ─────────────────────────────────────────────────────────────
//  Full store scan
// ─────────────────────────────────────────────────────────────

pub struct StoreScanResult {
    pub checked:   usize,
    pub ok:        usize,
    pub missing:   usize,
    pub corrupted: usize,
    pub dangling:  usize,
    pub issues:    Vec<(String, IntegrityEntry)>,
    pub elapsed:   Duration,
}

pub fn scan_full_store(deep: bool) -> Result<StoreScanResult> {
    let start = Instant::now();
    let db    = crate::db::InstalledDb::open()?;
    let pkgs  = db.list_all()?;

    let mut result = StoreScanResult {
        checked: 0, ok: 0, missing: 0, corrupted: 0, dangling: 0,
        issues: Vec::new(), elapsed: Duration::ZERO,
    };

    for pkg in &pkgs {
        let issues = verify_package(&pkg.name, &pkg.version, &pkg.store_hash, deep);
        result.checked += 1;

        if issues.is_empty() {
            result.ok += 1;
        } else {
            for entry in issues {
                match &entry.status {
                    IntegrityStatus::Missing         => result.missing   += 1,
                    IntegrityStatus::Corrupted       => result.corrupted += 1,
                    IntegrityStatus::DanglingSymlink  => result.dangling  += 1,
                    _ => {}
                }
                result.issues.push((pkg.name.clone(), entry));
            }
        }
    }

    result.elapsed = start.elapsed();
    Ok(result)
}

// ─────────────────────────────────────────────────────────────
//  Background daemon
// ─────────────────────────────────────────────────────────────

pub fn start_integrity_daemon() {
    std::thread::Builder::new()
    .name("hammer-integrity".into())
    .spawn(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all().build().unwrap();
        rt.block_on(async {
            loop {
                crate::log::info("store-integrity: starting background scan");
                match scan_full_store(false) {
                    Ok(result) => {
                        crate::log::info(&format!(
                            "store-integrity: {}/{} ok, {} issues in {:.1}s",
                            result.ok, result.checked,
                            result.issues.len(),
                                                  result.elapsed.as_secs_f64()
                        ));
                        if !result.issues.is_empty() {
                            let msg = format!(
                                "{} store integrity issue(s) found",
                                              result.issues.len()
                            );
                            let detail = result.issues.iter().take(3)
                            .map(|(pkg, e)| format!("{}: {:?}", pkg, e.status))
                            .collect::<Vec<_>>().join("\n");
                            let _ = crate::notify::send_notification(
                                &msg, &detail,
                                crate::notify::Urgency::Critical,
                                "dialog-error",
                            );
                            crate::log::error(&format!(
                                "store-integrity: {}: {}", msg, detail
                            ));
                        }
                    }
                    Err(e) => {
                        crate::log::warn(&format!("store-integrity scan failed: {}", e));
                    }
                }
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
    })
    .expect("Failed to spawn integrity daemon thread");
}

// ─────────────────────────────────────────────────────────────
//  CLI: hammer verify [<pkg>] [--deep] [--fix] [--json]
// ─────────────────────────────────────────────────────────────

pub fn cmd_verify_extended(args: &[String]) -> Result<()> {
    let pkg_filter = args.iter().find(|a| !a.starts_with('-')).map(|s| s.as_str());
    let deep       = args.iter().any(|a| a == "--deep");
    let fix        = args.iter().any(|a| a == "--fix");

    // FIX: json_mode is now used to switch output format below
    let json_mode  = crate::json_output::is_json_mode(args);

    let db   = crate::db::InstalledDb::open()?;
    let pkgs = db.list_all()?;
    let to_check: Vec<_> = pkgs.iter()
    .filter(|p| pkg_filter.map_or(true, |f| p.name == f))
    .collect();

    let mut ok       = 0usize;
    let mut problems: Vec<(String, Vec<IntegrityEntry>)> = Vec::new();

    for pkg in &to_check {
        let issues = verify_package(&pkg.name, &pkg.version, &pkg.store_hash, deep);
        if issues.is_empty() {
            ok += 1;
        } else {
            problems.push((pkg.name.clone(), issues));
        }
    }

    if json_mode {
        return print_verify_json(&to_check, ok, &problems, deep, fix);
    }

    println!();
    println!("  {}  Verifying store integrity{}",
             "⬡".bright_cyan().bold(),
             if deep { " (deep — hashing all files)" } else { "" });
    println!("  {}", "─".repeat(65).dimmed());

    for pkg in &to_check {
        let issues = verify_package(&pkg.name, &pkg.version, &pkg.store_hash, deep);
        if issues.is_empty() {
            if pkg_filter.is_some() {
                println!("  {} {} {} — OK",
                         "✔".bright_green(), pkg.name.bold(), pkg.version.dimmed());
            }
        } else {
            for entry in &issues {
                let status_str = match &entry.status {
                    IntegrityStatus::Missing         => "missing".red().to_string(),
                    IntegrityStatus::Corrupted       => "CORRUPTED".red().bold().to_string(),
                    IntegrityStatus::DanglingSymlink  => "dangling symlink".yellow().to_string(),
                    _                                 => "unknown".dimmed().to_string(),
                };
                println!("  {} {} — {} — {}",
                         "✗".red().bold(), pkg.name.bold(),
                         status_str,
                         entry.path.display().to_string().dimmed());
            }
        }
    }

    println!();
    println!("  {}", "─".repeat(65).dimmed());
    println!("  {:<22} {}", "Checked:".bold(), to_check.len());
    println!("  {:<22} {}", "OK:".bold(), ok.to_string().bright_green());
    println!("  {:<22} {}", "Problems:".bold(),
             if problems.is_empty() { "0".bright_green().to_string() }
             else { problems.len().to_string().red().bold().to_string() });

    if !problems.is_empty() {
        if fix {
            println!();
            println!("  {}  Attempting to fix…", "⬡".yellow().bold());
            let affected: Vec<String> = problems.iter().map(|(n, _)| n.clone()).collect();
            let cache  = crate::cache::PackageCache::load()?;
            let solver = crate::solver::Solver::new(&cache, &db);
            match solver.resolve_reinstall(&affected) {
                Ok(plan) => {
                    println!("  Reinstalling {} package(s)…", plan.to_upgrade.len());
                }
                Err(e) => {
                    println!("  {} Could not resolve fix: {}", "✗".red(), e);
                }
            }
        } else {
            println!();
            println!("  Fix: {}", "hammer verify --fix".cyan());
            println!("  Or:  {}", "hammer fix-broken".cyan());
        }
    } else {
        println!();
        println!("  {} Store integrity OK.", "✔".bright_green().bold());
    }

    Ok(())
}

fn print_verify_json(
    to_check: &[&crate::db::InstalledPackage],
    ok:       usize,
    problems: &[(String, Vec<IntegrityEntry>)],
                     deep:     bool,
                     fix:      bool,
) -> Result<()> {
    use crate::json_output::{print_json, print_json_error};
    use serde::Serialize;

    #[derive(Serialize)]
    struct JsonIssue { path: String, status: String }

    #[derive(Serialize)]
    struct JsonProblem { package: String, issues: Vec<JsonIssue> }

    #[derive(Serialize)]
    struct JsonVerifyResult {
        checked:  usize,
        ok:       usize,
        problems: Vec<JsonProblem>,
        deep:     bool,
        fix:      bool,
    }

    let json_problems: Vec<JsonProblem> = problems.iter().map(|(name, issues)| {
        JsonProblem {
            package: name.clone(),
                                                              issues: issues.iter().map(|e| JsonIssue {
                                                                  path:   e.path.display().to_string(),
                                                                                        status: format!("{:?}", e.status),
                                                              }).collect(),
        }
    }).collect();

    if to_check.is_empty() {
        print_json_error("verify", "No matching packages found");
        return Ok(());
    }

    print_json("verify", JsonVerifyResult {
        checked: to_check.len(), ok, problems: json_problems, deep, fix,
    });
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Helpers
// ─────────────────────────────────────────────────────────────

fn hash_file(path: &Path) -> Result<String> {
    let data = std::fs::read(path)
    .with_context(|| format!("Reading {}", path.display()))?;
    let mut h = Sha256::new();
    h.update(&data);
    Ok(hex::encode(h.finalize()))
}
