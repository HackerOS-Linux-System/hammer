use anyhow::{bail, Result};
use owo_colors::OwoColorize;

use crate::oci::sysroot::Sysroot;
use crate::oci::types::Config;

pub fn run(args: &[String], cfg: &Config, pinned: bool) -> Result<()> {
    let Some(id) = args.first() else {
        bail!("Usage: hammer oci {} <deployment-id>", if pinned { "pin" } else { "unpin" });
    };
    let sysroot = Sysroot::open(&cfg.sysroot_path, &cfg.ostree_repo_path, &cfg.osname)?;
    let result = sysroot.set_pinned(id, pinned)?;
    if !result.success {
        bail!("{}", result.error_message);
    }
    println!("  {} {} deployment {}", "✔".bright_green().bold(),
        if pinned { "Pinned" } else { "Unpinned" }, id);
    Ok(())
}
