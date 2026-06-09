use anyhow::Result;
use owo_colors::OwoColorize;
use std::process;

use crate::cli_types::GlobalFlags;
use crate::cli_pkg as pkg;
use crate::cli_sys as sys;
use crate::livecheck;
use crate::repo::SOURCES_HK;
use crate::setup;

pub async fn run(mut args: Vec<String>) -> Result<()> {
    let flags = GlobalFlags::parse(&mut args);
    let cmd   = args.get(1).map(|s| s.as_str()).unwrap_or("help");
    let rest  = args[2.min(args.len())..].to_vec();

    // ── Hidden / always-available ─────────────────────────────
    match cmd {
        "_activate"   => return sys::cmd_activate_internal(),
        "_setup"      => return setup::cmd_setup().await,
        "_import"     => return setup::cmd_import().await,
        "version" | "--version" => {
            println!("  {} {}  {}",
                     "⬡ hammer".bright_cyan().bold(),
                     env!("CARGO_PKG_VERSION").bold(),
                     "Apache-2.0".dimmed());
            return Ok(());
        }
        "help" | "--help" | "-h" | "" => { print_help(); return Ok(()); }
        _ => {}
    }

    // ── Live system guard ─────────────────────────────────────
    // immutable status check and doctor are safe to run in live systems
    let live_safe = matches!(cmd, "immutable" | "doctor" | "version");
    if !live_safe { livecheck::assert_not_live(); }

    if flags.user_mode {
        return pkg::run_user(cmd, &rest, &flags).await;
    }

    match cmd {
        // ── Package management ────────────────────────────────
        "install"      | "in"   => pkg::cmd_install(&rest, &flags).await,
        "remove"       | "rm"   => pkg::cmd_remove(&rest, &flags).await,
        "reinstall"             => pkg::cmd_reinstall(&rest).await,
        "upgrade"      | "up"   => pkg::cmd_upgrade(&rest, &flags).await,
        "dist-upgrade" | "dup"  => pkg::cmd_dist_upgrade(&rest, &flags).await,
        "autoremove"   | "ar"   => pkg::cmd_autoremove(&rest).await,
        "fix-broken"   | "fix"  => pkg::cmd_fix_broken(&rest).await,

        // ── Index / updates ───────────────────────────────────
        "sync" | "ref" | "update"    => pkg::cmd_sync().await,
        "self-update" | "selfupdate" => pkg::cmd_self_update().await,

        // ── Query ─────────────────────────────────────────────
        "search"  | "se" => pkg::cmd_search(&rest, &flags),
        "info"           => pkg::cmd_info(&rest, &flags),
        "list"    | "ls" => pkg::cmd_list(&rest),

        // ── Status ────────────────────────────────────────────
        "status"  | "st" => sys::cmd_status(),
        "history" | "hi" => sys::cmd_history(),

        // ── Generations ───────────────────────────────────────
        "gen"            => sys::cmd_gen(&rest).await,
        "rollback" | "rb"=> sys::cmd_rollback(),
        "diff"           => sys::cmd_diff(&rest),
        "pending"        => sys::cmd_pending(&rest),

        // ── Diagnostics ───────────────────────────────────────
        "verify"         => sys::cmd_verify(&rest),
        "doctor"         => sys::cmd_doctor(),

        // ── Services (0.3) ────────────────────────────────────
        "service" | "svc" => sys::cmd_service(&rest),

        // ── Logs (0.3) ────────────────────────────────────────
        "log" | "logs"   => sys::cmd_log(&rest),

        // ── Immutable filesystem (0.3) ────────────────────────
        "immutable" | "imm" => sys::cmd_immutable(&rest),

        // ── Export / import ───────────────────────────────────
        "export"         => sys::cmd_export(&rest),
        "import"         => sys::cmd_import_pkg(&rest).await,

        // ── Keys ──────────────────────────────────────────────
        "key"            => sys::cmd_key(&rest).await,

        // ── Maintenance ───────────────────────────────────────
        "gc"             => sys::cmd_gc(&rest),
        "clean"          => sys::cmd_clean(),
        "init"           => sys::cmd_init(&rest),
        "relink"         => sys::cmd_relink(),
        "store"          => sys::cmd_store(),

        other => {
            eprintln!("  Unknown command: '{}'. Try `hammer help`.", other);
            process::exit(1);
        }
    }
}

fn print_help() {
    println!();
    println!("  {} {}  {}",
             "⬡ hammer".bold().bright_cyan(),
             format!("v{}", env!("CARGO_PKG_VERSION")).dimmed(),
                 "Apache-2.0".dimmed());
    println!("  Atomic Debian package manager — HackerOS");
    println!();

    println!("  {}", "Package management:".bold());
    println!("    {}   install <pkg...> [-y] [--arch=ARCH] [--no-recommends]", "install".cyan());
    println!("    {}    remove <pkg...> [-y]", "remove".cyan());
    println!("    {}  reinstall <pkg...> [-y]", "reinstall".cyan());
    println!("    {}   upgrade [pkg...] [-y] [--system|--hackeros|--hammer]", "upgrade".cyan());
    println!("    {}  dist-upgrade [-y]  (aggressive: e.g. debian 14→15)", "dist-upgrade".cyan());
    println!("    {}  autoremove  fix-broken  sync  self-update", "".cyan());
    println!();

    println!("  {}", "Services (0.3):".bold());
    println!("    {}  list|start|stop|restart|enable|disable|status|log [unit]", "service".cyan());
    println!();

    println!("  {}", "Filesystem immutability (0.3):".bold());
    println!("    {}  status|enable|disable|unlock|install-service", "immutable".cyan());
    println!("    {}  Makes / /usr /boot read-only (Silverblue-style)", "".dimmed());
    println!();

    println!("  {}", "Diagnostics:".bold());
    println!("    {}   [package]  verify store integrity", "verify".cyan());
    println!("    {}              system health check", "doctor".cyan());
    println!("    {}   [-n N]     show hammer operation log", "log".cyan());
    println!();

    println!("  {}", "Generations:".bold());
    println!("    {}      show / cancel / apply-live", "pending".cyan());
    println!("    {}      list / switch <N>", "gen".cyan());
    println!("    {}      rollback  diff [A] [B]", "rollback".cyan());
    println!();

    println!("  {}", "Query / maintenance:".bold());
    println!("    {}  search  info  list  status  history", "".cyan());
    println!("    {}  gc [--keep N]  clean  init  relink  store", "".cyan());
    println!("    {}  key list|add|remove  export  import", "".cyan());
    println!();

    println!("  {}", "User packages (no reboot needed):".bold());
    println!("    {}  install|remove|list|status|init", "--user".yellow());
    println!();

    println!("  {}", "Config:".bold());
    println!("    {}  (format: .hk)", SOURCES_HK.cyan());
    println!("    {}  (HackerOS tools)", "/etc/hammer/HackerOS/*.hk".cyan());
    println!();

    println!("  {}", "Common workflows:".bold().dimmed());
    println!("    {} → {} → {}",
             "hammer sync".cyan(),
             "hammer install nginx".cyan(),
             "hammer service start nginx".cyan());
    println!("    {}  fix if commands not found after install", "hammer relink".cyan());
    println!("    {}  check system health", "hammer doctor".cyan());
    println!("    {}  lock filesystem read-only", "hammer immutable enable".cyan());
    println!();
}
