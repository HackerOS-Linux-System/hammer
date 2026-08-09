use anyhow::Result;
use owo_colors::OwoColorize;

use crate::oci::sysroot::Sysroot;
use crate::oci::types::Config;

pub fn run(_args: &[String], cfg: &Config) -> Result<()> {
    let sysroot = Sysroot::open(&cfg.sysroot_path, &cfg.ostree_repo_path, &cfg.osname)?;
    let deployments = sysroot.list_deployments()?;

    let Some(current) = deployments.iter().find(|d| d.booted) else {
        println!("  No booted deployment.");
        return Ok(());
    };
    let Some(previous) = deployments.iter().find(|d| !d.booted) else {
        println!("  Only one deployment exists — nothing to diff against.");
        return Ok(());
    };

    let (_, cur_layers) = sysroot.read_layer_metadata(&current.checksum);
    let (_, prev_layers) = sysroot.read_layer_metadata(&previous.checksum);

    let added: Vec<_> = cur_layers.iter().filter(|c| !prev_layers.iter().any(|p| p.name == c.name)).collect();
    let removed: Vec<_> = prev_layers.iter().filter(|p| !cur_layers.iter().any(|c| c.name == p.name)).collect();
    let changed: Vec<_> = cur_layers.iter().filter_map(|c| {
        prev_layers.iter().find(|p| p.name == c.name && p.version != c.version).map(|p| (p, c))
    }).collect();

    if added.is_empty() && removed.is_empty() && changed.is_empty() {
        println!("  No package-level differences between the two most recent deployments.");
        return Ok(());
    }

    println!();
    for a in &added   { println!("  {} {} {}", "+".bright_green().bold(), a.name.bold(), a.version.dimmed()); }
    for r in &removed { println!("  {} {}", "-".red().bold(), r.name.bold()); }
    for (p, c) in &changed {
        println!("  {} {} {} {} {}", "~".yellow().bold(), c.name.bold(), p.version.dimmed(), "->".dimmed(), c.version.dimmed());
    }
    println!();
    Ok(())
}
