use anyhow::{bail, Result};
use owo_colors::OwoColorize;

use crate::oci::sysroot::Sysroot;
use crate::oci::transaction_lock::TransactionLock;
use crate::oci::types::Config;

pub fn run(_args: &[String], cfg: &Config) -> Result<()> {
    let lock = TransactionLock::acquire(&cfg.overlay_work_dir)?;
    let sysroot = Sysroot::open(&cfg.sysroot_path, &cfg.ostree_repo_path, &cfg.osname)?;
    let result = sysroot.rollback()?;
    lock.mark_complete();

    if !result.success {
        bail!("Rollback failed: {}", result.error_message);
    }
    println!("  {} Rolled back to {} — reboot to activate.",
        "✔".bright_green().bold(), &result.new_checksum[..12.min(result.new_checksum.len())]);
    Ok(())
}
