use anyhow::Result;
use owo_colors::OwoColorize;

use crate::oci::ostree_repo::Repo;
use crate::oci::types::Config;

pub fn run(_args: &[String], cfg: &Config) -> Result<()> {
    println!("  {} Verifying OSTree repo at {}…", "·".dimmed(), cfg.ostree_repo_path.display());
    let repo = Repo::open(&cfg.ostree_repo_path)?;
    repo.check_integrity(true)?;
    println!("  {} Repository integrity verified — every ref's commit history is intact.",
        "✔".bright_green().bold());
    Ok(())
}
