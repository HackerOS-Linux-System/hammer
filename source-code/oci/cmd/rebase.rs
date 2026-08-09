use anyhow::{bail, Result};
use owo_colors::OwoColorize;

use crate::oci::deb_layer::DebLayer;
use crate::oci::oci_puller::OciPuller;
use crate::oci::overlay::OverlayManager;
use crate::oci::ostree_repo::Repo;
use crate::oci::sysroot::Sysroot;
use crate::oci::transaction_lock::TransactionLock;
use crate::oci::types::Config;

pub async fn run(args: &[String], cfg: &Config) -> Result<()> {
    let Some(new_image_ref) = args.first() else {
        bail!("Usage: hammer oci rebase <new-image-ref>");
    };

    let lock = TransactionLock::acquire(&cfg.overlay_work_dir)?;

    let sysroot = Sysroot::open(&cfg.sysroot_path, &cfg.ostree_repo_path, &cfg.osname)?;
    let Some(current) = sysroot.booted_deployment()? else {
        bail!("No booted deployment. Use 'hammer oci deploy' for the first install.");
    };
    let (_, existing_layers) = sysroot.read_layer_metadata(&current.checksum);
    let names: Vec<String> = existing_layers.iter().map(|l| l.name.clone()).collect();

    println!("  {} Pulling new base {}…", "↓".bright_cyan().bold(), new_image_ref.bold());
    let puller = OciPuller::new(cfg.overlay_work_dir.join("pull"));
    let new_base_rootfs = puller.pull_and_unpack(new_image_ref)?;

    let repo = Repo::open(&cfg.ostree_repo_path)?;
    let new_origin = format!("hammer-oci:{new_image_ref}");

    if names.is_empty() {
        let checksum = repo.commit_directory(
            &new_base_rootfs,
            &format!("hammer-oci/{}", sanitize(&new_origin)),
            &format!("Rebase to {new_image_ref}"),
            "",
        )?;
        let _ = std::fs::remove_dir_all(&new_base_rootfs);
        let result = sysroot.deploy_commit(&checksum, &new_origin, &[])?;
        lock.mark_complete();
        if !result.success { bail!("Rebase failed: {}", result.error_message); }
        println!("  {} Rebased to {} — reboot to activate.", "✔".bright_green().bold(), new_image_ref.bold());
        return Ok(());
    }

    println!("  {} Re-applying {} layered package(s): {}", "·".dimmed(), names.len(), names.join(", "));
    let overlay_mgr = OverlayManager::new(cfg.overlay_work_dir.clone());
    let mut session = overlay_mgr.begin_session(&new_base_rootfs)?;
    overlay_mgr.bind_mount_virtual_fs(&session)?;

    let deb_layer = DebLayer::new(cfg);
    let install_result = deb_layer.install_packages(&session, &names).await;
    overlay_mgr.unbind_virtual_fs(&session)?;

    let new_layers = match install_result {
        Ok(l) => l,
        Err(e) => {
            overlay_mgr.discard_session(&mut session)?;
            let _ = std::fs::remove_dir_all(&new_base_rootfs);
            lock.mark_complete();
            return Err(e);
        }
    };

    let flat_dir = cfg.overlay_work_dir.join("flat-commit");
    overlay_mgr.flatten_to(&session, &flat_dir)?;
    overlay_mgr.end_session(&mut session)?;
    let _ = std::fs::remove_dir_all(&new_base_rootfs);

    let checksum = repo.commit_directory(
        &flat_dir,
        &format!("hammer-oci/{}", sanitize(&new_origin)),
        &format!("Rebase to {new_image_ref} (+{} layers)", new_layers.len()),
        "",
    )?;
    let _ = std::fs::remove_dir_all(&flat_dir);

    let result = sysroot.deploy_commit(&checksum, &new_origin, &new_layers)?;
    lock.mark_complete();
    if !result.success { bail!("Rebase failed: {}", result.error_message); }
    println!("  {} Rebased to {} with {} layer(s) — reboot to activate.",
        "✔".bright_green().bold(), new_image_ref.bold(), new_layers.len());
    Ok(())
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect()
}
