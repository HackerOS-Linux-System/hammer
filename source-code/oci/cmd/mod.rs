mod autoremove;
mod cleanup;
mod deploy;
mod diff;
mod fsck;
mod initramfs;
mod install;
mod list;
mod pin;
mod rebase;
mod rollback;
mod search;
mod status;
mod uninstall;
mod update;
mod upgrade;

use anyhow::Result;
use owo_colors::OwoColorize;

use super::config;

pub async fn run(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("help");
    let rest = args.get(1..).map(|s| s.to_vec()).unwrap_or_default();

    if matches!(sub, "help" | "--help" | "-h" | "") {
        print_help();
        return Ok(());
    }

    let cfg = config::load_config(None)?;

    match sub {
        "status"     | "st" => status::run(&rest, &cfg),
        "deploy"            => deploy::run(&rest, &cfg).await,
        "install"    | "in" => install::run(&rest, &cfg).await,
        "uninstall"  | "rm" => uninstall::run(&rest, &cfg),
        "upgrade"    | "up" => upgrade::run(&rest, &cfg).await,
        "rollback"   | "rb" => rollback::run(&rest, &cfg),
        "rebase"            => rebase::run(&rest, &cfg).await,
        "cleanup"           => cleanup::run(&rest, &cfg),
        "fsck"              => fsck::run(&rest, &cfg),
        "initramfs"         => initramfs::run(&rest, &cfg),
        "search"     | "se" => search::run(&rest, &cfg).await,
        "list"       | "ls" => list::run(&rest, &cfg),
        "pin"               => pin::run(&rest, &cfg, true),
        "unpin"             => pin::run(&rest, &cfg, false),
        "diff"              => diff::run(&rest, &cfg),
        "update"            => update::run(&rest, &cfg).await,
        "autoremove"        => autoremove::run(&rest, &cfg),
        other => {
            eprintln!("  {} Unknown 'hammer oci' subcommand: '{}'. Try {}.",
                      "!".red().bold(), other, "hammer oci help".cyan());
            std::process::exit(1);
        }
    }
}

fn print_help() {
    println!();
    println!("  {}  {}", "⬡ hammer oci".bright_cyan().bold(), "— image-based/immutable OCI+deb mode".dimmed());
    println!("  Native hammer replacement for the standalone 'deb-ostree' tool.");
    println!();
    println!("  {}", "Bootstrap:".bold());
    println!("    deploy <image-ref>        Initial deployment from an OCI base image");
    println!("    rebase <image-ref>        Switch to a different base image");
    println!();
    println!("  {}", "Layer management:".bold());
    println!("    install <pkg...>          Install package(s) as a layer");
    println!("    uninstall <pkg...>        Remove layered package(s)");
    println!("    upgrade                   Update base image + re-apply layers");
    println!("    update                    Refresh package indexes only");
    println!("    autoremove                Remove orphaned layered dependencies");
    println!();
    println!("  {}", "Deployments:".bold());
    println!("    status [--verbose]        Show all deployments (like 'rpm-ostree status')");
    println!("    rollback                  Return to the previous deployment");
    println!("    pin / unpin <id>          Protect / unprotect a deployment from cleanup");
    println!("    cleanup [--keep N]        Remove old deployments (default: keep 2)");
    println!("    fsck                      Verify OSTree repository integrity");
    println!("    diff                      Show package diff vs the booted deployment");
    println!();
    println!("  {}", "Query:".bold());
    println!("    search <term>             Search available packages");
    println!("    list                      List installed layered packages");
    println!();
    println!("  {}", "System:".bold());
    println!("    initramfs                 Regenerate initramfs inside the new deployment");
    println!();
}
