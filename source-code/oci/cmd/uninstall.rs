use anyhow::{bail, Result};
use owo_colors::OwoColorize;

use crate::oci::deb_layer::DebLayer;
use crate::oci::overlay::OverlayManager;
use crate::oci::ostree_repo::Repo;
use crate::oci::sysroot::Sysroot;
use crate::oci::transaction_lock::TransactionLock;
use crate::oci::types::Config;

pub fn run(args: &[String], cfg: &Config) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: hammer oci uninstall <pkg> [pkg...]");
    }
    let names: Vec<String> = args.to_vec();

    let lock = TransactionLock::acquire(&cfg.overlay_work_dir)?;

    let sysroot = Sysroot::open(&cfg.sysroot_path, &cfg.ostree_repo_path, &cfg.osname)?;
    let Some(current) = sysroot.booted_deployment()? else {
        bail!("No booted deployment.");
    };
    let (origin, mut existing_layers) = sysroot.read_layer_metadata(&current.checksum);
    let origin = if origin.is_empty() { current.origin_refspec.clone() } else { origin };

    for n in &names {
        if !existing_layers.iter().any(|l| &l.name == n) {
            eprintln!("  {} '{}' is not a hammer-oci layered package (it may be part of the base image and cannot be removed here).", "!".yellow().bold(), n);
        }
    }

    let lower_dir = cfg.overlay_work_dir.join("lower-checkout");
    let repo = Repo::open(&cfg.ostree_repo_path)?;
    repo.checkout_commit(&current.checksum, &lower_dir)?;

    let overlay_mgr = OverlayManager::new(cfg.overlay_work_dir.clone());
    let mut session = overlay_mgr.begin_session(&lower_dir)?;

    let deb_layer = DebLayer::new(cfg);
    let remove_result = deb_layer.remove_packages(&session, &names);
    if let Err(e) = remove_result {
        overlay_mgr.discard_session(&mut session)?;
        lock.mark_complete();
        return Err(e);
    }

    let flat_dir = cfg.overlay_work_dir.join("flat-commit");
    overlay_mgr.flatten_to(&session, &flat_dir)?;
    overlay_mgr.end_session(&mut session)?;

    let checksum = repo.commit_directory(
        &flat_dir,
        &format!("hammer-oci/{}", sanitize(&origin)),
        &format!("Uninstall: {}", names.join(", ")),
        "",
    )?;
    let _ = std::fs::remove_dir_all(&flat_dir);
    let _ = std::fs::remove_dir_all(&lower_dir);

    existing_layers.retain(|l| !names.contains(&l.name));
    let result = sysroot.deploy_commit(&checksum, &origin, &existing_layers)?;
    lock.mark_complete();

    if !result.success {
        bail!("Uninstall failed: {}", result.error_message);
    }
    println!("  {} Removed {} — reboot to activate.", "✔".bright_green().bold(), names.join(", ").bold());
    Ok(())
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect()
}
