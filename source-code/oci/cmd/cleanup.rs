use anyhow::Result;
use owo_colors::OwoColorize;

use crate::oci::overlay;
use crate::oci::sysroot::Sysroot;
use crate::oci::types::Config;

pub fn run(args: &[String], cfg: &Config) -> Result<()> {
    if args.iter().any(|a| a == "--repair") {
        if let Some(orphan) = overlay::detect_orphaned_session(&cfg.overlay_work_dir) {
            overlay::repair_orphaned_session(&orphan)?;
            println!("  {} Removed orphaned overlay session at {}.", "✔".bright_green().bold(), orphan.display());
        } else {
            println!("  Nothing to repair.");
        }
        let incomplete = cfg.overlay_work_dir.join(".incomplete");
        let _ = std::fs::remove_file(&incomplete);
    }

    let keep = args.iter().position(|a| a == "--keep")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(2);

    let sysroot = Sysroot::open(&cfg.sysroot_path, &cfg.ostree_repo_path, &cfg.osname)?;
    let result = sysroot.cleanup(keep)?;
    if !result.success {
        anyhow::bail!("Cleanup failed: {}", result.error_message);
    }
    println!("  {} Cleaned up old deployments (kept last {}).", "✔".bright_green().bold(), keep);
    Ok(())
}
