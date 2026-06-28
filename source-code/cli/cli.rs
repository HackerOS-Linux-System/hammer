use anyhow::Result;
use owo_colors::OwoColorize;
use std::process;

use crate::build_mode;
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

    // ── Always-available ───────────────────────────────────────
    match cmd {
        "_activate"  => return sys::cmd_activate_internal(),
        "_setup"     => return setup::cmd_setup().await,
        "_import"    => return setup::cmd_import().await,
        "version" | "--version" | "-V" => { print_version(); return Ok(()); }
        "features"   => { build_mode::Features::current().print(); return Ok(()); }
        "help" | "--help" | "-h" | "" => { print_help(); return Ok(()); }
        _ => {}
    }

    // ── Live system guard ──────────────────────────────────────
    let live_safe = matches!(cmd,
        "immutable" | "doctor" | "version" | "features" | "log" | "logs"
    );
    if !live_safe { livecheck::assert_not_live(); }

    if flags.user_mode {
        return pkg::run_user(cmd, &rest, &flags).await;
    }

    match cmd {
        // ── Package management ────────────────────────────────
        "install"      | "in"  => pkg::cmd_install(&rest, &flags).await,
        "remove"       | "rm"  => pkg::cmd_remove(&rest, &flags).await,
        "reinstall"            => pkg::cmd_reinstall(&rest).await,
        "upgrade"      | "up"  => pkg::cmd_upgrade(&rest, &flags).await,
        "dist-upgrade" | "dup" => pkg::cmd_dist_upgrade(&rest, &flags).await,
        "autoremove"   | "ar"  => pkg::cmd_autoremove(&rest).await,
        "fix-broken"   | "fix" => pkg::cmd_fix_broken(&rest).await,

        // pins/holds — live in pins.rs, called directly
        "hold"   => crate::pins::cmd_hold(&rest),
        "unhold" => crate::pins::cmd_unhold(&rest),
        "pin"    => crate::pins::cmd_pin(&rest),
        "unpin"  => crate::pins::cmd_unpin(&rest),
        "mark"   => pkg::cmd_mark(&rest),

        // ── Multi-arch ────────────────────────────────────────
        "dpkg-arch" => crate::multi_arch::cmd_dpkg_arch(&rest),
        "arch"      => crate::multi_arch::cmd_arch(&rest),

        // ── Index / updates ───────────────────────────────────
        "sync"   | "ref" | "update"     => pkg::cmd_sync().await,
        "self-update" | "selfupdate"    => pkg::cmd_self_update().await,

        // ── Query ─────────────────────────────────────────────
        "search"  | "se"  => pkg::cmd_search(&rest, &flags),
        "info"            => pkg::cmd_info(&rest, &flags),
        "list"    | "ls"  => pkg::cmd_list(&rest),
        "show"            => crate::query::cmd_show(&rest, &flags),
        "depends" | "dep" => crate::query::cmd_depends(&rest),
        "rdepends"        => crate::query::cmd_rdepends(&rest),
        "files"           => crate::query::cmd_files(&rest),
        "which"           => crate::query::cmd_which(&rest),
        "changelog"       => { let client = crate::download::HttpClient::new(); crate::query::cmd_changelog(&rest, &client).await },
        "policy"          => crate::query::cmd_policy(&rest),

        // ── Sources / repos ───────────────────────────────────
        "source"    => crate::build_dep::cmd_source(&rest).await,
        "build-dep" => crate::build_dep::cmd_build_dep(&rest).await,
        "download"  => pkg::cmd_download(&rest).await,

        // ── .hk file tools ───────────────────────────────────
        "hk" => {
            let sub2  = rest.first().map(|s| s.as_str()).unwrap_or("help").to_string();
            let rest2 = rest.get(1..).map(|s| s.to_vec()).unwrap_or_default();
            match sub2.as_str() {
                "validate" => crate::hk_tools::cmd_validate_hk(&rest2),
                _ => {
                    eprintln!("  Unknown hk subcommand '{}'. Available: validate", sub2);
                    Ok(())
                }
            }
        }

        // ── Status ────────────────────────────────────────────
        "status"  | "st" => sys::cmd_status(),
        "history" | "hi" => sys::cmd_history(&rest),

        // ── Generations (atomic only) ─────────────────────────
        "gen" => {
            build_mode::require_atomic("hammer gen")?;
            sys::cmd_gen(&rest).await
        }
        "rollback" | "rb" => {
            build_mode::require_atomic("hammer rollback")?;
            sys::cmd_rollback(&rest)
        }
        "diff"    => sys::cmd_diff(&rest),
        "pending" => sys::cmd_pending(&rest),

        // ── Query v0.5 ────────────────────────────────────────
        "what"     => crate::file_index::cmd_what(&rest),
        "what-rebuild" => crate::file_index::cmd_what_rebuild(),
        "size"     => crate::size::cmd_size(&rest),
        "undo"     => crate::undo::cmd_undo(&rest),
        "show-deps" => crate::query::cmd_show_deps(&rest),
        "owns"     => crate::query::cmd_owns(&rest),
        "stats"    => crate::query::cmd_stats(),

        // ── Diagnostics ───────────────────────────────────────
        "verify"   => sys::cmd_verify(&rest),
        "why"      => sys::cmd_why(&rest),
        "why-not"  => sys::cmd_why_not(&rest),
        "doctor" => sys::cmd_doctor(),
        "fsck"   => sys::cmd_fsck(&rest),
        "check"  => sys::cmd_check(&rest),
        "audit"  => crate::audit::cmd_audit(&rest),

        // ── Services ──────────────────────────────────────────
        "service" | "svc" => sys::cmd_service(&rest),

        // ── Logs ──────────────────────────────────────────────
        "log" | "logs" => sys::cmd_log(&rest),

        // ── Immutable filesystem (atomic only) ────────────────
        "immutable" | "imm" => {
            #[cfg(feature = "normal-mode")]
            {
                eprintln!("  {} 'hammer immutable' is not available in normal-mode builds.",
                          "!".yellow().bold());
                return Ok(());
            }
            #[cfg(not(feature = "normal-mode"))]
            sys::cmd_immutable(&rest)
        }

        // ── Snapshots (atomic only) ───────────────────────────
        "snapshot" | "snap" => {
            build_mode::require_atomic("hammer snapshot")?;
            crate::immutable::create_snapshot(
                rest.first().map(|s| s.as_str()).unwrap_or("manual")
            )
        }

        // ── Export / import ───────────────────────────────────
        "export" => sys::cmd_export(&rest),
        "import" => sys::cmd_import_pkg(&rest).await,

        // ── Keys ─────────────────────────────────────────────
        "key" => sys::cmd_key(&rest).await,

        // ── Mirrors ──────────────────────────────────────────
        "mirror" => crate::mirror::cmd_mirror(&rest).await,
        "repo"   => crate::repo::cmd_repo(&rest),
        "build"  => crate::build_dep::cmd_build(&rest).await,

        // ── Maintenance ──────────────────────────────────────
        "gc"         => sys::cmd_gc(&rest),
        "clean"      => sys::cmd_clean(),
        "init"       => sys::cmd_init(&rest),
        "relink"     => sys::cmd_relink(),
        "store"      => sys::cmd_store(),
        "completion" => crate::completion::cmd_completion(&rest),
        "boot"       => sys::cmd_boot(&rest),

        // ── Daemon ────────────────────────────────────────────
        "daemon" => sys::cmd_daemon(&rest).await,

        // ── Database ──────────────────────────────────────────
        "db" => sys::cmd_db(&rest).await,

        other => {
            eprintln!("  {} Unknown command: '{}'. Try {}.",
                      "!".red().bold(), other, "hammer help".cyan());
            process::exit(1);
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  Version
// ─────────────────────────────────────────────────────────────

fn print_version() {
    println!();
    println!("  {}  {}",
             "⬡ hammer".bright_cyan().bold(),
             format!("v{}", env!("CARGO_PKG_VERSION")).bold());
    println!("  {} Atomic Debian package manager — HackerOS", "·".dimmed());
    build_mode::print_mode_banner();
    println!("  {} License : Apache-2.0", "·".dimmed());
    println!("  {} Source  : https://github.com/HackerOS-Linux-System/hammer", "·".dimmed());
    println!();
    println!("  Run {} to list all feature flags.", "hammer features".cyan());
    println!();
}

// ─────────────────────────────────────────────────────────────
//  Help
// ─────────────────────────────────────────────────────────────

fn print_help() {
    let atomic = !build_mode::NORMAL_MODE;
    println!();
    println!("  {}  {}  {}",
             "⬡ hammer".bold().bright_cyan(),
             format!("v{}", env!("CARGO_PKG_VERSION")).dimmed(),
             if atomic { "[atomic]".bright_cyan().to_string() }
             else      { "[normal]".yellow().to_string() });
    println!("  Debian package manager for HackerOS");
    println!();
    println!("  {}", "Package management:".bold());
    println!("    {}   <pkg…> [-y] [--arch=ARCH] [--no-recommends]", "install".cyan());
    println!("    {}    <pkg…> [-y]",                                 "remove".cyan());
    println!("    {}  <pkg…> [-y]",                                   "reinstall".cyan());
    println!("    {}   [pkg…] [-y]",                                  "upgrade".cyan());
    println!("    {}  [-y]",                                          "dist-upgrade".cyan());
    println!("    {} autoremove  fix-broken  sync  self-update",       "".cyan());
    println!();
    println!("  {}", "Query:".bold());
    println!("    {}   <q> [--installed] [--json]", "search".cyan());
    println!("    {} | {}  <pkg>",                  "info".cyan(), "show".cyan());
    println!("    {}    [--installed|--available|--upgradable]", "list".cyan());
    println!("    {} depends  rdepends  files  which  policy  changelog", "".cyan());
    println!();
    println!("  {}", "Query v0.5 (nowe):".bold());
    println!("    {}   <ścieżka>          — która paczka dostarcza ten plik", "what".cyan());
    println!("    {}    <pkg…>            — rozmiar na dysku paczek", "size".cyan());
    println!("    {}     [--yes]          — cofnij ostatnią operację", "undo".cyan());
    println!("    {}  <pkg>        — wizualizacja drzewa zależności", "show-deps".cyan());
    println!("    {}    <ścieżka>         — kto posiada plik (szybka, przez indeks)", "owns".cyan());
    println!("    {}   what-rebuild      — przebuduj indeks plik→paczka", "".cyan());
    println!();
    println!("  {}", "Pinning & holds:".bold());
    println!("    {} pin  unpin  hold  unhold  mark", "".cyan());
    println!();
    println!("  {}", "Sources & repos:".bold());
    println!("    {} source  build-dep  download  mirror  key", "".cyan());
    println!("    {}   {}", SOURCES_HK.cyan(), "(source list format)".dimmed());
    println!();
    println!("  {}", "Services (systemd):".bold());
    println!("    {}  list|start|stop|restart|enable|disable|status|log [unit]", "service".cyan());
    println!();
    if atomic {
        println!("  {}", "Immutable filesystem (atomic only):".bold());
        println!("    {}  status|enable|disable|lock|unlock|verify|snapshot|install-service", "immutable".cyan());
        println!();
        println!("  {}", "Generations (atomic only):".bold());
        println!("    {}  list|switch <N>|delete <N>   {}  {}",
                 "gen".cyan(), "rollback".cyan(), "diff".cyan());
        println!();
    }
    println!("  {}", "Diagnostics:".bold());
    println!("    {} doctor  check  verify  fsck  audit  log", "".cyan());
    println!();
    println!("  {}", "Maintenance:".bold());
    println!("    {} gc  clean  init  relink  store  completion  boot", "".cyan());
    println!();
    println!("  {}", "Daemon:".bold());
    println!("    {}  start|stop|status|reload|sync|check|verify|gc [--keep=N]", "daemon".cyan());
    println!();
    println!("  {}", "Database:".bold());
    println!("    {}  validate-json|export-json|import-json|recover", "db".cyan());
    println!();
    println!("  {}", "User packages (no root):".bold());
    println!("    {}  install|remove|list|status|init", "--user".yellow());
    println!();
    println!("  {}", "Build modes:".bold().dimmed());
    println!("    {}   cargo build --release",
             "atomic (default):".dimmed());
    println!("    {}  cargo build --release --features normal-mode",
             "normal (classic): ".dimmed());
    println!();
}
