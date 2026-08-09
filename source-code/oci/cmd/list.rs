use anyhow::{bail, Result};
use owo_colors::OwoColorize;

use crate::oci::sysroot::Sysroot;
use crate::oci::types::Config;

pub fn run(_args: &[String], cfg: &Config) -> Result<()> {
    let sysroot = Sysroot::open(&cfg.sysroot_path, &cfg.ostree_repo_path, &cfg.osname)?;
    let Some(current) = sysroot.booted_deployment()? else {
        bail!("No booted deployment.");
    };
    let (_, layers) = sysroot.read_layer_metadata(&current.checksum);

    if layers.is_empty() {
        println!("  No layered packages on the booted deployment.");
        return Ok(());
    }
    println!();
    for l in &layers {
        println!("  {} {} {}", "●".bright_green(), l.name.bold(), l.version.dimmed());
    }
    println!();
    println!("  {} package(s) layered on top of the base image.", layers.len());
    Ok(())
}
