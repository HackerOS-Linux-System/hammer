use anyhow::Result;
use owo_colors::OwoColorize;

use crate::oci::overlay;
use crate::oci::ostree_repo::Repo;
use crate::oci::sysroot::Sysroot;
use crate::oci::types::Config;

pub fn run(args: &[String], cfg: &Config) -> Result<()> {
    let verbose = args.iter().any(|a| a == "--verbose" || a == "-v");
    let sysroot = Sysroot::open(&cfg.sysroot_path, &cfg.ostree_repo_path, &cfg.osname)?;
    let deployments = sysroot.list_deployments()?;

    if let Some(orphan) = overlay::detect_orphaned_session(&cfg.overlay_work_dir) {
        println!("  {} Found an interrupted transaction at {}.", "!".yellow().bold(), orphan.display());
        println!("    Run {} to clean it up.", "hammer oci cleanup --repair".cyan());
        println!();
    }

    if deployments.is_empty() {
        println!("  No deployments yet. Run {} to bootstrap.", "hammer oci deploy <image-ref>".cyan());
        return Ok(());
    }

    let repo = if verbose { Repo::open(&cfg.ostree_repo_path).ok() } else { None };

    println!();
    for dep in &deployments {
        let marker = if dep.booted { "●".bright_green().to_string() } else { "○".dimmed().to_string() };
        let (origin, layers) = sysroot.read_layer_metadata(&dep.checksum);
        let origin = if origin.is_empty() { dep.origin_refspec.clone() } else { origin };

        println!("  {} {} {}", marker, dep.osname.bold(), format!("{}.{}", &dep.checksum[..12.min(dep.checksum.len())], dep.serial).dimmed());
        if !origin.is_empty() {
            println!("      Origin: {}", origin);
        }
        if dep.pinned {
            println!("      {}", "Pinned".yellow());
        }
        if !layers.is_empty() {
            let names: Vec<&str> = layers.iter().map(|l| l.name.as_str()).collect();
            println!("      Layered packages ({}): {}", layers.len(), names.join(", "));
        }
        if let Some(repo) = &repo {
            match repo.read_commit_info(&dep.checksum) {
                Ok(info) if !info.subject.is_empty() || info.timestamp > 0 => {
                    if !info.subject.is_empty() {
                        println!("      {}: {}", "Subject".dimmed(), info.subject);
                    }
                    if info.timestamp > 0 {
                        println!("      {}: {}", "Committed".dimmed(), format_epoch(info.timestamp));
                    }
                }
                Ok(_) => {}
                Err(e) => println!("      {} could not read commit metadata: {e:#}", "!".yellow()),
            }
        }
        println!();
    }
    if !verbose {
        println!("  {} Run with {} for commit subject/timestamp.", "ℹ".cyan(), "hammer oci status --verbose".cyan());
    }
    Ok(())
}

fn format_epoch(ts: i64) -> String {
    // Minimal, dependency-free UTC formatting (avoids pulling chrono into
    // the oci module just for one display line) — good enough for a
    // status line; not used for any logic decisions.
    let secs_since_epoch = ts;
    let days = secs_since_epoch.div_euclid(86400);
    let secs_of_day = secs_since_epoch.rem_euclid(86400);
    let (h, m, s) = (secs_of_day / 3600, (secs_of_day % 3600) / 60, secs_of_day % 60);
    // Days since epoch -> Y-M-D (civil_from_days, Howard Hinnant's algorithm)
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m_num = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m_num <= 2 { y + 1 } else { y };
    format!("{y:04}-{m_num:02}-{d:02} {h:02}:{m:02}:{s:02} UTC")
}
