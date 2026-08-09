use anyhow::{bail, Result};
use owo_colors::OwoColorize;

use crate::oci::process::run_inherit;
use crate::oci::sysroot::Sysroot;
use crate::oci::types::Config;

pub fn run(_args: &[String], cfg: &Config) -> Result<()> {
    let sysroot = Sysroot::open(&cfg.sysroot_path, &cfg.ostree_repo_path, &cfg.osname)?;
    let Some(current) = sysroot.booted_deployment()? else {
        bail!("No booted deployment.");
    };
    let dep_path = sysroot.deployment_path(&current);
    if !dep_path.exists() {
        bail!("Deployment path {} does not exist on disk.", dep_path.display());
    }

    println!("  {} Regenerating initramfs in {}…", "·".dimmed(), dep_path.display());
    run_inherit("chroot", &[&dep_path.to_string_lossy(), "update-initramfs", "-u", "-k", "all"])?;
    println!("  {} initramfs regenerated. Note: OSTree deployments are content-addressed —", "✔".bright_green().bold());
    println!("    this changes files in place on the *existing* deployment checkout; run");
    println!("    'hammer oci install <noop-pkg>' or re-deploy to persist it as a new, committed generation.");
    Ok(())
}
