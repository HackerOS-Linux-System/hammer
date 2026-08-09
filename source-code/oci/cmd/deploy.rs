use anyhow::{bail, Result};
use owo_colors::OwoColorize;

use crate::oci::oci_puller::OciPuller;
use crate::oci::ostree_repo::Repo;
use crate::oci::sysroot::Sysroot;
use crate::oci::transaction_lock::TransactionLock;
use crate::oci::types::Config;

pub async fn run(args: &[String], cfg: &Config) -> Result<()> {
    let Some(image_ref) = args.first() else {
        bail!("Usage: hammer oci deploy <image-ref>\n  e.g. hammer oci deploy ghcr.io/example/hackeros-base:trixie");
    };

    let lock = TransactionLock::acquire(&cfg.overlay_work_dir)?;
    if lock.found_incomplete() {
        eprintln!("  {} A previous 'hammer oci' transaction was interrupted.", "!".yellow().bold());
        eprintln!("    Run {} before continuing.", "hammer oci cleanup --repair".cyan());
    }

    println!("  {} Pulling base image {}…", "↓".bright_cyan().bold(), image_ref.bold());
    let puller = OciPuller::new(cfg.overlay_work_dir.join("pull"));
    let rootfs = puller.pull_and_unpack(image_ref)?;

    println!("  {} Committing to OSTree repo {}…", "·".dimmed(), cfg.ostree_repo_path.display());
    let repo = Repo::open_or_create(&cfg.ostree_repo_path)?;
    let refspec = format!("hammer-oci/{}", sanitize_ref(image_ref));
    let checksum = repo.commit_directory(
        &rootfs,
        &refspec,
        &format!("Initial deploy from {image_ref}"),
        "",
    )?;
    let _ = std::fs::remove_dir_all(&rootfs);

    println!("  {} Registering deployment…", "·".dimmed());
    let sysroot = Sysroot::open(&cfg.sysroot_path, &cfg.ostree_repo_path, &cfg.osname)?;
    let origin = format!("hammer-oci:{image_ref}");
    let result = sysroot.deploy_commit(&checksum, &origin, &[])?;

    lock.mark_complete();

    if !result.success {
        bail!("Deploy failed: {}", result.error_message);
    }
    println!("  {} Deployed {} as {}", "✔".bright_green().bold(), image_ref.bold(), &checksum[..12.min(checksum.len())]);
    println!("    Reboot to activate this deployment.");
    Ok(())
}

fn sanitize_ref(image_ref: &str) -> String {
    image_ref.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect()
}
