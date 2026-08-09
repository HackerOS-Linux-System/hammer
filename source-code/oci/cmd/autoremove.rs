use anyhow::{bail, Result};
use owo_colors::OwoColorize;

use crate::oci::sysroot::Sysroot;
use crate::oci::types::Config;
use crate::package::parse_dep_field;

pub fn run(args: &[String], cfg: &Config) -> Result<()> {
    let sysroot = Sysroot::open(&cfg.sysroot_path, &cfg.ostree_repo_path, &cfg.osname)?;
    let Some(current) = sysroot.booted_deployment()? else {
        bail!("No booted deployment.");
    };
    let (_, layers) = sysroot.read_layer_metadata(&current.checksum);
    if layers.is_empty() {
        println!("  Nothing to autoremove — no layered packages installed.");
        return Ok(());
    }

    let checkout = cfg.overlay_work_dir.join("autoremove-inspect");
    let repo = crate::oci::ostree_repo::Repo::open(&cfg.ostree_repo_path)?;
    repo.checkout_commit(&current.checksum, &checkout)?;
    let installed = crate::oci::status_db::load_all(&checkout)?;
    let auto_installed = crate::oci::status_db::load_auto_installed(&checkout);
    let _ = std::fs::remove_dir_all(&checkout);

    let mut referenced: std::collections::HashSet<String> = Default::default();
    for pkg in &installed {
        for field in [&pkg.depends, &pkg.pre_depends] {
            if field.is_empty() { continue; }
            for group in parse_dep_field(field) {
                for alt in group.alternatives {
                    referenced.insert(alt.name);
                }
            }
        }
    }

    let candidates: Vec<&str> = layers.iter()
        .map(|l| l.name.as_str())
        .filter(|n| auto_installed.contains(*n) && !referenced.contains(*n))
        .collect();

    if candidates.is_empty() {
        println!("  No auto-installed, unreferenced layered packages found.");
        return Ok(());
    }

    println!("  Candidates for removal (auto-installed, no longer required by anything):");
    for c in &candidates { println!("    - {c}"); }

    if !args.iter().any(|a| a == "--yes" || a == "-y") {
        println!();
        println!("  Re-run with {} to remove them.", "hammer oci autoremove --yes".cyan());
        return Ok(());
    }

    let names: Vec<String> = candidates.iter().map(|s| s.to_string()).collect();
    super::uninstall::run(&names, cfg)
}
