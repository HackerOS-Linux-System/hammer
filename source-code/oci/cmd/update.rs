use anyhow::Result;
use owo_colors::OwoColorize;

use crate::oci::deb_layer::DebLayer;
use crate::oci::types::Config;

pub async fn run(_args: &[String], cfg: &Config) -> Result<()> {
    let deb_layer = DebLayer::new(cfg);
    let pool = deb_layer.refresh_package_index().await?;
    println!("  {} Refreshed package index ({} package(s) available across {} source(s)).",
        "✔".bright_green().bold(), pool.all().count(), cfg.apt_sources.len());
    Ok(())
}
