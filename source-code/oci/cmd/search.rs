use anyhow::{bail, Result};
use owo_colors::OwoColorize;

use crate::oci::deb_layer::DebLayer;
use crate::oci::types::Config;

pub async fn run(args: &[String], cfg: &Config) -> Result<()> {
    let Some(term) = args.first() else {
        bail!("Usage: hammer oci search <term>");
    };
    let deb_layer = DebLayer::new(cfg);
    let pool = deb_layer.refresh_package_index().await?;

    let mut matches: Vec<_> = pool_iter(&pool)
        .filter(|p| p.name.contains(term.as_str()) || p.description_short.as_deref().unwrap_or("").to_lowercase().contains(&term.to_lowercase()))
        .collect();
    matches.sort_by(|a, b| a.name.cmp(&b.name));

    if matches.is_empty() {
        println!("  No packages found matching '{}'.", term);
        return Ok(());
    }
    println!();
    for p in matches {
        println!("  {} {} — {}", p.name.bold(), p.version.dimmed(), p.description_short.as_deref().unwrap_or(""));
    }
    println!();
    Ok(())
}

// `Pool` keeps its map private; expose a tiny read-only iterator helper
// here rather than widening `Pool`'s public API for a single call site.
fn pool_iter(pool: &crate::oci::repo_index::Pool) -> impl Iterator<Item = &crate::package::Package> {
    pool.all()
}
