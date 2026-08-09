use anyhow::{bail, Result};
use owo_colors::OwoColorize;

use crate::oci::deb_layer::DebLayer;
use crate::oci::overlay::OverlayManager;
use crate::oci::ostree_repo::Repo;
use crate::oci::sysroot::Sysroot;
use crate::oci::transaction_lock::TransactionLock;
use crate::oci::types::Config;

pub async fn run(args: &[String], cfg: &Config) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: hammer oci install <pkg> [pkg...]");
    }
    let names: Vec<String> = args.to_vec();

    let lock = TransactionLock::acquire(&cfg.overlay_work_dir)?;

    let sysroot = Sysroot::open(&cfg.sysroot_path, &cfg.ostree_repo_path, &cfg.osname)?;
    let Some(current) = sysroot.booted_deployment()? else {
        bail!("No booted deployment. Run 'hammer oci deploy <image-ref>' first.");
    };
    let (origin, mut existing_layers) = sysroot.read_layer_metadata(&current.checksum);
    let origin = if origin.is_empty() { current.origin_refspec.clone() } else { origin };

    println!("  {} Checking out current deployment…", "·".dimmed());
    let lower_dir = cfg.overlay_work_dir.join("lower-checkout");
    let repo = Repo::open(&cfg.ostree_repo_path)?;
    repo.checkout_commit(&current.checksum, &lower_dir)?;

    let overlay_mgr = OverlayManager::new(cfg.overlay_work_dir.clone());
    let mut session = overlay_mgr.begin_session(&lower_dir)?;
    overlay_mgr.bind_mount_virtual_fs(&session)?;

    println!("  {} Resolving and installing: {}", "↓".bright_cyan().bold(), names.join(", ").bold());
    let deb_layer = DebLayer::new(cfg);
    let install_result = deb_layer.install_packages(&session, &names).await;

    overlay_mgr.unbind_virtual_fs(&session)?;

    let new_layers = match install_result {
        Ok(layers) => layers,
        Err(e) => {
            overlay_mgr.discard_session(&mut session)?;
            lock.mark_complete();
            return Err(e);
        }
    };

    println!("  {} Committing new layer to OSTree…", "·".dimmed());
    let flat_dir = cfg.overlay_work_dir.join("flat-commit");
    overlay_mgr.flatten_to(&session, &flat_dir)?;
    overlay_mgr.end_session(&mut session)?;

    let checksum = repo.commit_directory(
        &flat_dir,
        &format!("hammer-oci/{}", sanitize(&origin)),
        &format!("Install: {}", names.join(", ")),
        "",
    )?;
    let _ = std::fs::remove_dir_all(&flat_dir);
    let _ = std::fs::remove_dir_all(&lower_dir);

    for l in new_layers {
        if !existing_layers.iter().any(|e| e.name == l.name) {
            existing_layers.push(l);
        }
    }

    let result = sysroot.deploy_commit(&checksum, &origin, &existing_layers)?;
    lock.mark_complete();

    if !result.success {
        bail!("Install failed: {}", result.error_message);
    }
    println!("  {} Installed {} — reboot to activate.", "✔".bright_green().bold(), names.join(", ").bold());
    Ok(())
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect()
}
