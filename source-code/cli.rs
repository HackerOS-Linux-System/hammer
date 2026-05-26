use anyhow::{bail, Result};
use owo_colors::OwoColorize;
use std::process;

use crate::cache::{detect_arch, PackageCache};
use crate::db::InstalledDb;
use crate::diff::compute_diff;
use crate::grub;
use crate::hk_tools;
use crate::livecheck;
use crate::livepatch;
use crate::lock;
use crate::profile::{
    self, activate_pending, clear_pending, GenerationsDb,
    read_active_gen, read_pending_gen,
};
use crate::repo::SOURCES_HK;
use crate::selfupdate;
use crate::setup;
use crate::solver::Solver;
use crate::store::Store;
use crate::transaction::{execute_transaction, TransactionContext};
use crate::ui;
use crate::userenv::{self, UserEnv};

// ─────────────────────────────────────────────────────────────
//  Global flags
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct GlobalFlags {
    pub user_mode: bool,
    pub arch:      Option<String>,
    pub yes:       bool,
}

impl GlobalFlags {
    pub fn parse(args: &mut Vec<String>) -> Self {
        let mut f = GlobalFlags::default();
        args.retain(|a| {
            if a == "--user" || a == "-U" {
                f.user_mode = true; false
            } else if let Some(arch) = a.strip_prefix("--arch=") {
                f.arch = Some(arch.to_string()); false
            } else if a == "-y" || a == "--yes" {
                f.yes = true; false
            } else { true }
        });
        f
    }
}

// ─────────────────────────────────────────────────────────────
//  Entry point
// ─────────────────────────────────────────────────────────────

pub async fn run(mut args: Vec<String>) -> Result<()> {
    let flags = GlobalFlags::parse(&mut args);
    let cmd   = args.get(1).map(|s| s.as_str()).unwrap_or("help");
    let rest  = args[2.min(args.len())..].to_vec();

    // ── Hidden / always-available ─────────────────────────────
    match cmd {
        "_activate"   => return cmd_activate_internal(),
        "_setup"      => return setup::cmd_setup().await,
        "_import"     => return setup::cmd_import().await,
        "version" | "--version" => {
            println!("  {} {}  {}", "⬡ hammer".bright_cyan().bold(),
                     env!("CARGO_PKG_VERSION").bold(), "Apache-2.0".dimmed());
            return Ok(());
        }
        "help" | "--help" | "-h" | "" => { print_help(); return Ok(()); }
        _ => {}
    }

    // ── Live system guard ─────────────────────────────────────
    livecheck::assert_not_live();

    if flags.user_mode {
        return run_user(cmd, &rest, &flags).await;
    }

    match cmd {
        "install"      | "in"   => cmd_install(&rest, &flags).await,
        "remove"       | "rm"   => cmd_remove(&rest, &flags).await,
        "upgrade"      | "up"   => cmd_upgrade(&rest, &flags).await,
        "dist-upgrade" | "dup"  => cmd_dist_upgrade(&rest, &flags).await,
        "autoremove"   | "ar"   => cmd_autoremove(&rest).await,
        "reinstall"             => cmd_reinstall(&rest, &flags).await,
        "fix-broken"   | "fix"  => cmd_fix_broken(&rest).await,
        "sync" | "ref" | "update" => cmd_sync().await,
        "self-update" | "selfupdate" => cmd_self_update().await,
        "verify"  => cmd_verify(&rest),
        "doctor"  => cmd_doctor(),
        "search"  | "se" => cmd_search(&rest, &flags),
        "info"           => cmd_info(&rest, &flags),
        "list"    | "ls" => cmd_list(&rest),
        "status"  | "st" => cmd_status(),
        "history" | "hi" => cmd_history(),
        "gen"            => cmd_gen(&rest).await,
        "rollback" | "rb"=> cmd_rollback(),
        "diff"           => cmd_diff(&rest),
        "pending"        => cmd_pending(&rest),
        "export"         => cmd_export(&rest),
        "import"         => cmd_import_pkg(&rest).await,
        "key"            => cmd_key(&rest).await,
        "gc"             => cmd_gc(&rest),
        "clean"          => cmd_clean(),
        "init"           => cmd_init(&rest),
        "relink"         => cmd_relink(),
        "store"          => cmd_store(),
        other => {
            eprintln!("  Unknown command: '{}'. Try `hammer help`.", other);
            process::exit(1);
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  User mode router
// ─────────────────────────────────────────────────────────────

async fn run_user(cmd: &str, args: &[String], flags: &GlobalFlags) -> Result<()> {
    let env = UserEnv::current()?;
    match cmd {
        "install" | "in" => cmd_user_install(&env, args, flags).await,
        "remove"  | "rm" => cmd_user_remove(&env, args).await,
        "list"    | "ls" => cmd_user_list(&env),
        "status"  | "st" => cmd_user_status(&env),
        "init"           => { env.init()?; cmd_user_shell_init(&env) }
        other => bail!("hammer --user: unknown command '{}'. Try: install, remove, list, status, init", other),
    }
}

// ─────────────────────────────────────────────────────────────
//  install
// ─────────────────────────────────────────────────────────────

pub(crate) async fn cmd_install(args: &[String], flags: &GlobalFlags) -> Result<()> {
    let yes       = has_flag(args, "-y") || has_flag(args, "--yes") || flags.yes;
    let no_recomm = has_flag(args, "--no-recommends");
    let names: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    if names.is_empty() {
        bail!("Usage: hammer install <package...> [-y] [--arch=ARCH] [--no-recommends]");
    }

    let arch  = flags.arch.as_ref()
    .map(|a| userenv::normalise_arch(a)).transpose()?
    .unwrap_or_else(detect_arch);

    // Acquire lock BEFORE opening DB
    let _lock = lock::system_lock()?;

    ui::print_header();
    let db    = InstalledDb::open()?;
    let cache = PackageCache::load_for_arch(&arch)?;
    let plan  = Solver::new(&cache, &db).resolve_install(&names, no_recomm)?;
    if plan.is_empty() { ui::nothing_to_do(); return Ok(()); }

    for c in &plan.conflicts { println!("  {} {}", "warn:".yellow().bold(), c.yellow()); }
    ui::deps_resolved();
    ui::print_transaction_table(&plan, &arch);
    ui::print_transaction_summary(&plan);
    if !yes && !ui::confirm("Proceed with installation?")? { println!("  Aborted."); return Ok(()); }

    let note    = format!("install {}", names.join(" "));
    let ctx     = TransactionContext::system(&plan, &db, &names, false);
    let gen_num = execute_transaction(ctx, &note).await?;
    if let Ok(gens_db) = GenerationsDb::load() { let _ = grub::update_grub(&gens_db); }
    ui::print_pending_notice(gen_num);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  reinstall  — FIX: _flags (suppress unused variable warning)
// ─────────────────────────────────────────────────────────────

async fn cmd_reinstall(args: &[String], _flags: &GlobalFlags) -> Result<()> {
    let yes   = has_flag(args, "-y");
    let names: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    if names.is_empty() { bail!("Usage: hammer reinstall <package...> [-y]"); }

    let _lock = lock::system_lock()?;

    ui::print_header();
    let db     = InstalledDb::open()?;
    let cache  = PackageCache::load_for_arch(&detect_arch())?;
    let plan   = Solver::new(&cache, &db).resolve_reinstall(&names)?;
    if plan.is_empty() { ui::nothing_to_do(); return Ok(()); }

    ui::print_transaction_table(&plan, &detect_arch());
    ui::print_transaction_summary(&plan);
    if !yes && !ui::confirm("Proceed with reinstallation?")? { println!("  Aborted."); return Ok(()); }

    let note    = format!("reinstall {}", names.join(" "));
    let ctx     = TransactionContext::system(&plan, &db, &names, false);
    let gen_num = execute_transaction(ctx, &note).await?;
    if let Ok(gens_db) = GenerationsDb::load() { let _ = grub::update_grub(&gens_db); }
    ui::print_pending_notice(gen_num);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  remove
// ─────────────────────────────────────────────────────────────

async fn cmd_remove(args: &[String], flags: &GlobalFlags) -> Result<()> {
    let yes   = has_flag(args, "-y") || flags.yes;
    let names: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    if names.is_empty() { bail!("Usage: hammer remove <package...> [-y]"); }

    let _lock = lock::system_lock()?;

    ui::print_header();
    let db     = InstalledDb::open()?;
    let cache  = PackageCache::load()?;
    let plan   = Solver::new(&cache, &db).resolve_remove(&names)?;
    if plan.is_empty() { ui::nothing_to_do(); return Ok(()); }

    for w in &plan.warnings { println!("  {} {}", "warn:".yellow().bold(), w.yellow()); }
    ui::print_transaction_table(&plan, &detect_arch());
    ui::print_transaction_summary(&plan);
    if !yes && !ui::confirm("Proceed with removal?")? { println!("  Aborted."); return Ok(()); }

    let note    = format!("remove {}", names.join(" "));
    let ctx     = TransactionContext::system(&plan, &db, &names, false);
    let gen_num = execute_transaction(ctx, &note).await?;
    if let Ok(gens_db) = GenerationsDb::load() { let _ = grub::update_grub(&gens_db); }
    ui::print_pending_notice(gen_num);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  upgrade  (--system / --hackeros / --hammer)
// ─────────────────────────────────────────────────────────────

async fn cmd_upgrade(args: &[String], flags: &GlobalFlags) -> Result<()> {
    let yes           = has_flag(args, "-y") || flags.yes;
    let only_system   = has_flag(args, "--system");
    let only_hackeros = has_flag(args, "--hackeros");
    let only_hammer   = has_flag(args, "--hammer");
    let pkgs: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();

    let do_system   = only_system   || (!only_hackeros && !only_hammer);
    let do_hackeros = only_hackeros || (!only_system   && !only_hammer);
    let do_hammer   = only_hammer   || (!only_system   && !only_hackeros);

    ui::print_header();

    if do_system {
        let _lock = lock::system_lock()?;
        let arch  = flags.arch.as_ref()
        .map(|a| userenv::normalise_arch(a)).transpose()?
        .unwrap_or_else(detect_arch);
        let db     = InstalledDb::open()?;
        let cache  = PackageCache::load_for_arch(&arch)?;
        let solver = Solver::new(&cache, &db);
        let plan   = if pkgs.is_empty() { solver.resolve_upgrade()? }
        else               { solver.resolve_install(&pkgs, false)? };

        if !plan.is_empty() {
            ui::deps_resolved();
            ui::print_transaction_table(&plan, &arch);
            ui::print_transaction_summary(&plan);
            if !yes && !ui::confirm("Proceed with upgrade?")? { println!("  Aborted."); return Ok(()); }
            let explicit: Vec<String> = plan.to_upgrade.iter().map(|p| p.name.clone()).collect();
            let ctx     = TransactionContext::system(&plan, &db, &explicit, true);
            let gen_num = execute_transaction(ctx, "upgrade").await?;
            if let Ok(gens_db) = GenerationsDb::load() { let _ = grub::update_grub(&gens_db); }
            ui::print_pending_notice(gen_num);
        } else {
            println!("  {} All Debian packages up to date.", "✔".bright_green());
        }
    }

    if do_hackeros {
        let client = crate::download::HttpClient::new();
        hk_tools::update_all_tools(&client).await?;
    }

    if do_hammer {
        println!("  {}  Checking for hammer self-update…", "⬡".cyan().bold());
        let client = crate::download::HttpClient::new();
        match selfupdate::check_for_update(&client).await {
            Ok(Some(v)) => {
                println!("  {} New hammer version: {}", "↑".yellow().bold(), v.bright_cyan());
                println!("  Run {} to update.", "hammer self-update".cyan());
            }
            Ok(None) => println!("  {} hammer is up to date.", "✔".bright_green()),
            Err(e)   => println!("  {} Self-update check failed: {}", "!".yellow(), e),
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  dist-upgrade
// ─────────────────────────────────────────────────────────────

async fn cmd_dist_upgrade(args: &[String], _flags: &GlobalFlags) -> Result<()> {
    let yes   = has_flag(args, "-y");
    let _lock = lock::system_lock()?;
    ui::print_header();
    println!("  {}  dist-upgrade — aggressive upgrade (may install/remove packages)", "⬡".bright_cyan().bold());
    println!();

    let db    = InstalledDb::open()?;
    let cache = PackageCache::load()?;
    let plan  = Solver::new(&cache, &db).resolve_dist_upgrade()?;
    if plan.is_empty() { println!("  {} System is already up to date.", "✔".bright_green()); return Ok(()); }

    for w in &plan.warnings { println!("  {} {}", "!".yellow().bold(), w.yellow()); }
    ui::print_transaction_table(&plan, &detect_arch());
    ui::print_transaction_summary(&plan);
    if !yes && !ui::confirm("Proceed with dist-upgrade? (This is destructive!)")? { println!("  Aborted."); return Ok(()); }

    let ctx     = TransactionContext::system(&plan, &db, &[], true);
    let gen_num = execute_transaction(ctx, "dist-upgrade").await?;
    if let Ok(gens_db) = GenerationsDb::load() { let _ = grub::update_grub(&gens_db); }
    ui::print_pending_notice(gen_num);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  autoremove
// ─────────────────────────────────────────────────────────────

async fn cmd_autoremove(args: &[String]) -> Result<()> {
    let yes   = has_flag(args, "-y");
    let _lock = lock::system_lock()?;
    let db    = InstalledDb::open()?;
    let cache = PackageCache::load()?;
    let plan  = Solver::new(&cache, &db).resolve_autoremove()?;
    if plan.is_empty() { ui::nothing_to_do(); return Ok(()); }

    ui::print_transaction_table(&plan, &detect_arch());
    ui::print_transaction_summary(&plan);
    if !yes && !ui::confirm("Remove unused dependencies?")? { println!("  Aborted."); return Ok(()); }

    let removals: Vec<String> = plan.to_autoremove.clone();
    let ctx     = TransactionContext::system(&plan, &db, &removals, false);
    let gen_num = execute_transaction(ctx, "autoremove").await?;
    if let Ok(gens_db) = GenerationsDb::load() { let _ = grub::update_grub(&gens_db); }
    ui::print_pending_notice(gen_num);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  fix-broken
// ─────────────────────────────────────────────────────────────

async fn cmd_fix_broken(args: &[String]) -> Result<()> {
    let yes   = has_flag(args, "-y");
    let _lock = lock::system_lock()?;
    ui::print_header();
    println!("  {}  Checking for broken dependencies…", "⬡".bright_cyan().bold());
    let db    = InstalledDb::open()?;
    let cache = PackageCache::load()?;
    let plan  = Solver::new(&cache, &db).resolve_fix_broken()?;

    for w in &plan.warnings { println!("  {} {}", "·".dimmed(), w.yellow()); }
    if plan.is_empty() { println!("  {} No broken dependencies found.", "✔".bright_green()); return Ok(()); }

    ui::print_transaction_table(&plan, &detect_arch());
    ui::print_transaction_summary(&plan);
    if !yes && !ui::confirm("Fix broken dependencies?")? { println!("  Aborted."); return Ok(()); }

    let ctx     = TransactionContext::system(&plan, &db, &[], false);
    let gen_num = execute_transaction(ctx, "fix-broken").await?;
    if let Ok(gens_db) = GenerationsDb::load() { let _ = grub::update_grub(&gens_db); }
    ui::print_pending_notice(gen_num);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  sync / self-update
// ─────────────────────────────────────────────────────────────

async fn cmd_sync() -> Result<()> {
    ui::print_header();
    crate::cache::sync_all().await?;
    println!("  {} Package index updated.", "✔".bright_green());
    Ok(())
}

async fn cmd_self_update() -> Result<()> {
    ui::print_header();
    let client = crate::download::HttpClient::new();
    selfupdate::self_update(&client).await
}

// ─────────────────────────────────────────────────────────────
//  verify
// ─────────────────────────────────────────────────────────────

fn cmd_verify(args: &[String]) -> Result<()> {
    let pkg_filter = args.first().map(|s| s.as_str());
    ui::print_header();
    println!("  {}  Verifying package integrity…", "⬡".bright_cyan().bold());
    println!("  {}", "─".repeat(60).dimmed());

    let db = InstalledDb::open()?;
    let mut ok = 0usize; let mut failed = 0usize; let mut missing = 0usize;
    let packages = db.list_all()?;
    let to_check: Vec<_> = packages.iter()
    .filter(|p| pkg_filter.map_or(true, |f| p.name == f)).collect();

    for pkg in &to_check {
        let store_path = std::path::Path::new(crate::store::STORE_DIR)
        .join(format!("{}-{}-{}", pkg.name, pkg.version, pkg.store_hash));
        if !store_path.exists() {
            println!("  {} {} {} — store entry missing", "✗".red().bold(), pkg.name.bold(), pkg.version.dimmed());
            missing += 1; continue;
        }
        let entry_ok = walkdir::WalkDir::new(&store_path).min_depth(1)
        .into_iter().flatten()
        .filter(|i| i.file_type().is_symlink())
        .all(|i| std::fs::read_link(i.path())
        .map(|t| t.exists() || t.symlink_metadata().is_ok()).unwrap_or(true));
        if entry_ok {
            ok += 1;
            if pkg_filter.is_some() {
                println!("  {} {} {} — OK", "✔".bright_green(), pkg.name.bold(), pkg.version.dimmed());
            }
        } else {
            println!("  {} {} {} — dangling symlinks", "✗".red().bold(), pkg.name.bold(), pkg.version.dimmed());
            failed += 1;
        }
    }

    println!();
    println!("  {}", "─".repeat(60).dimmed());
    println!("  {:<20} {}", "Checked:".bold(), to_check.len());
    println!("  {:<20} {}", "OK:".bold(),      ok.to_string().bright_green());
    if missing > 0 { println!("  {:<20} {}", "Missing:".bold(), missing.to_string().red()); }
    if failed  > 0 {
        println!("  {:<20} {}", "Failed:".bold(), failed.to_string().red());
        println!("  Run {} to fix.", "hammer fix-broken".cyan());
    } else { println!("\n  {} Store integrity OK.", "✔".bright_green()); }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  doctor
// ─────────────────────────────────────────────────────────────

fn cmd_doctor() -> Result<()> {
    ui::print_header();
    println!("  {}  hammer doctor — system health check", "⬡".bright_cyan().bold());
    println!("  {}", "─".repeat(60).dimmed());
    println!();

    let mut issues = 0usize;
    macro_rules! check {
        ($label:expr, $ok:expr, $fix:expr) => {{
            let ok: bool = $ok;
            if ok { println!("  {}  {}", "✔".bright_green(), $label); }
            else  { println!("  {}  {} — {}", "✗".red().bold(), $label, $fix.yellow()); issues += 1; }
        }};
    }

    check!("/hammer directory exists",         std::path::Path::new("/hammer").exists(), "run `hammer init`");
    let active_ok = std::fs::read_link("/hammer/active").map(|t| t.exists()).unwrap_or(false);
    check!("/hammer/active symlink valid",      active_ok, "run `hammer _activate` or `hammer relink`");
    check!("sources-list.hk exists",           std::path::Path::new(SOURCES_HK).exists(), "run `hammer init`");
    let lists_ok = std::path::Path::new(crate::cache::LISTS_DIR).exists()
    && std::fs::read_dir(crate::cache::LISTS_DIR).map(|mut d| d.next().is_some()).unwrap_or(false);
    check!("Package index populated",           lists_ok, "run `hammer sync`");
    let svc_ok = std::path::Path::new("/etc/systemd/system/hammer-activate.service").exists();
    check!("hammer-activate.service installed", svc_ok, "run `hammer init`");
    let path_env = std::env::var("PATH").unwrap_or_default();
    check!("/usr/bin in PATH",                  path_env.contains("/usr/bin"), "add to /etc/environment");
    check!("/usr/local/bin in PATH",            path_env.contains("/usr/local/bin"), "add to ~/.bashrc");
    check!("hammer database accessible",        InstalledDb::open().is_ok(), "run `hammer init`");
    if read_pending_gen().is_some() { println!("  {}  Pending changes staged — reboot to activate", "ℹ".cyan().bold()); }
    else { println!("  {}  No pending changes", "✔".bright_green()); }
    check!("GRUB generator installed",          std::path::Path::new(crate::grub::GRUB_GENERATOR).exists(), "run `hammer init`");
    check!("Running on installed system",       crate::livecheck::live_reason().is_none(), "hammer cannot run in a live system");

    let tools = hk_tools::list_tools();
    if !tools.is_empty() {
        let inst = tools.iter().filter(|(_, v, _)| v != "not installed").count();
        println!("  {}  HackerOS tools: {}/{} installed",
                 if inst == tools.len() { "✔".bright_green().to_string() } else { "ℹ".cyan().to_string() },
                     inst, tools.len());
    }

    println!();
    println!("  {}", "─".repeat(60).dimmed());
    if issues == 0 { println!("  {}  All checks passed. System is healthy.", "✔".bright_green().bold()); }
    else { println!("  {}  {} issue{} found.", "!".yellow().bold(), issues, if issues == 1 { "" } else { "s" }); }
    println!();
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  search / info / list
// ─────────────────────────────────────────────────────────────

fn cmd_search(args: &[String], flags: &GlobalFlags) -> Result<()> {
    let installed_only = has_flag(args, "--installed");
    let query = args.iter().filter(|a| !a.starts_with('-')).cloned().collect::<Vec<_>>().join(" ");
    if query.is_empty() { bail!("Usage: hammer search <query> [--installed]"); }
    let arch  = flags.arch.as_ref().map(|a| userenv::normalise_arch(a)).transpose()?
    .unwrap_or_else(detect_arch);
    let db    = InstalledDb::open()?;
    let cache = PackageCache::load_for_arch(&arch)?;
    let mut results: Vec<_> = cache.search(&query).into_iter()
    .filter(|p| !installed_only || db.is_installed(&p.name)).collect();
    results.sort_by(|a, b| a.name.cmp(&b.name));
    ui::print_search_header(&query, results.len());
    for pkg in &results { ui::print_search_result(pkg, db.is_installed(&pkg.name)); }
    Ok(())
}

fn cmd_info(args: &[String], flags: &GlobalFlags) -> Result<()> {
    let name = args.first().ok_or_else(|| anyhow::anyhow!("Usage: hammer info <package>"))?;
    let arch  = flags.arch.as_ref().map(|a| userenv::normalise_arch(a)).transpose()?
    .unwrap_or_else(detect_arch);
    let db    = InstalledDb::open()?;
    let cache = PackageCache::load_for_arch(&arch)?;
    let pkg   = cache.get(name).ok_or_else(|| anyhow::anyhow!("Package '{}' not found. Run `hammer sync`.", name))?;
    let inst  = db.get(name);
    ui::print_package_info(pkg, inst.is_some(), inst.as_ref().map(|p| p.version.as_str()));
    Ok(())
}

fn cmd_list(args: &[String]) -> Result<()> {
    let installed_only = has_flag(args, "--installed") || has_flag(args, "-i");
    let upgrades_only  = has_flag(args, "--upgrades")  || has_flag(args, "-u");
    let db    = InstalledDb::open()?;
    let cache = PackageCache::load()?;
    if installed_only || upgrades_only {
        for inst in db.list_all()? {
            let nv = cache.get(&inst.name)
            .filter(|av| crate::package::version_cmp(&av.version, &inst.version) == std::cmp::Ordering::Greater)
            .map(|av| av.version.as_str());
            if upgrades_only && nv.is_none() { continue; }
            let repo = cache.get(&inst.name).and_then(|p| p.repo_base_uri.as_deref())
            .and_then(|u| u.split('/').last()).unwrap_or("installed");
            ui::print_list_entry(&inst.name, &inst.version, &inst.architecture, true, repo, nv);
        }
    } else {
        for pkg in cache.all_packages() {
            let inst = db.is_installed(&pkg.name);
            let repo = pkg.repo_base_uri.as_deref().and_then(|u| u.split('/').last()).unwrap_or("");
            ui::print_list_entry(&pkg.name, &pkg.version, &pkg.architecture, inst, repo, None);
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  status / history
// ─────────────────────────────────────────────────────────────

fn cmd_status() -> Result<()> {
    let db = InstalledDb::open()?;
    ui::print_status(&db);
    println!("  {:<26} {}", "GRUB integration:".bold(), grub::grub_status().dimmed());
    if let Some(bg) = grub::read_boot_gen() {
        println!("  {:<26} gen-{}", "Booted with:".bold(), bg.to_string().cyan().bold());
    }
    let tools = hk_tools::list_tools();
    if !tools.is_empty() {
        println!(); println!("  {}", "HackerOS tools:".bold());
        for (n, v, d) in &tools {
            let ds: String = d.chars().take(40).collect();
            println!("    {} {} {}  {}", "·".dimmed(), n.bold(), v.cyan(), ds.dimmed());
        }
    }
    Ok(())
}

fn cmd_history() -> Result<()> {
    let db = InstalledDb::open()?;
    ui::print_history(&db.history(50)?);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  diff / gen / rollback / pending
// ─────────────────────────────────────────────────────────────

fn cmd_diff(args: &[String]) -> Result<()> {
    let gens_db = GenerationsDb::load()?;
    let (a, b) = if args.is_empty() {
        let p = gens_db.pending.ok_or_else(|| anyhow::anyhow!("No pending generation. Use: hammer diff <A> <B>"))?;
        (gens_db.current, p)
    } else if args.len() == 1 {
        (args[0].parse::<u32>()?, gens_db.current)
    } else {
        (args[0].parse::<u32>()?, args[1].parse::<u32>()?)
    };
    ui::print_diff(&compute_diff(a, b, &gens_db)?);
    Ok(())
}

async fn cmd_gen(args: &[String]) -> Result<()> {
    match args.first().map(|s| s.as_str()).unwrap_or("list") {
        "list" | "ls" => { ui::print_generations(&GenerationsDb::load()?); }
        "switch" => {
            let n: u32 = args.get(1).ok_or_else(|| anyhow::anyhow!("Usage: hammer gen switch <N>"))?.parse()?;
            let gens_db = GenerationsDb::load()?;
            let gen = gens_db.get(n).ok_or_else(|| anyhow::anyhow!("Generation {} not found.", n))?.clone();
            let _lock = lock::system_lock()?;
            profile::switch_active(&gen)?;
            let mut gdb = GenerationsDb::load()?;
            gdb.current = n; gdb.pending = None; clear_pending().ok(); gdb.save()?;
            let _ = grub::update_grub(&gdb);
            println!("  {} Now running gen-{}.", "✔".bright_green(), n);
        }
        other => bail!("Unknown gen subcommand: '{}'. Try: list, switch", other),
    }
    Ok(())
}

fn cmd_rollback() -> Result<()> {
    let _lock   = lock::system_lock()?;
    let gens_db = GenerationsDb::load()?;
    let mut prev: Vec<_> = gens_db.generations.iter()
    .filter(|g| g.number < gens_db.current).collect();
    prev.sort_by(|a, b| b.number.cmp(&a.number));
    let p = prev.first().ok_or_else(|| anyhow::anyhow!("No previous generation."))?;
    println!("  Rolling back: gen-{} → gen-{}…", gens_db.current, p.number);
    profile::switch_active(p)?;
    let mut gdb = GenerationsDb::load()?;
    gdb.current = p.number; gdb.pending = None; clear_pending().ok(); gdb.save()?;
    let _ = grub::update_grub(&gdb);
    println!("  {} Rolled back to gen-{}.", "✔".bright_green(), p.number);
    Ok(())
}

fn cmd_pending(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("show");
    match sub {
        "show" | "status" => {
            let gens_db = GenerationsDb::load()?;
            match read_pending_gen() {
                None => println!("  {} No pending changes.", "·".dimmed()),
                Some(n) => {
                    println!("\n  {} Pending: gen-{}", "⬡".bright_yellow().bold(), n.to_string().bold().bright_yellow());
                    if let Some(gen) = gens_db.get(n) {
                        if let Some(ref note) = gen.note { println!("  Operation : {}", note.cyan()); }
                        println!("  Packages  : {}", gen.package_count());
                    }
                    println!("\n  Will activate on next reboot.");
                    println!("  Cancel: {}", "hammer pending cancel".cyan());
                }
            }
        }
        "cancel" => match read_pending_gen() {
            None => println!("  {} No pending changes.", "·".dimmed()),
            Some(n) => {
                let _lock = lock::system_lock()?;
                clear_pending()?;
                let mut gdb = GenerationsDb::load()?;
                gdb.pending = None;
                if let Some(g) = gdb.generations.iter_mut().find(|g| g.number == n) {
                    g.state = Some(crate::profile::GenState::Old);
                }
                gdb.save()?;
                let _ = grub::update_grub(&gdb);
                println!("  {} Pending gen-{} cancelled.", "✔".bright_green(), n);
            }
        },
        "apply-live" => cmd_apply_live()?,
        other => bail!("Unknown pending subcommand: '{}'. Try: show, cancel, apply-live", other),
    }
    Ok(())
}

fn cmd_apply_live() -> Result<()> {
    let pending_num = match read_pending_gen() {
        Some(n) => n,
        None => { println!("  {} No pending changes.", "·".dimmed()); return Ok(()); }
    };
    let gens_db = GenerationsDb::load()?;
    let gen = gens_db.get(pending_num).ok_or_else(|| anyhow::anyhow!("Pending gen not in DB"))?;
    let current_pkgs: std::collections::HashSet<String> = gens_db.current_gen()
    .map(|g| g.packages.iter().map(|p| p.name.clone()).collect()).unwrap_or_default();
    let new_entries: Vec<crate::store::StoreEntry> = gen.packages.iter()
    .filter(|p| !current_pkgs.contains(&p.name))
    .filter_map(|p| {
        let path = std::path::PathBuf::from(crate::store::STORE_DIR)
        .join(format!("{}-{}-{}", p.name, p.version, p.store_hash));
        if path.exists() {
            Some(crate::store::StoreEntry {
                name: p.name.clone(), version: p.version.clone(),
                 hash: p.store_hash.clone(), path,
            })
        } else { None }
    }).collect();
    let analysis = livepatch::analyse(&livepatch::collect_files(&new_entries));
    if !analysis.can_live_patch {
        println!("  {} Cannot apply live — reboot required.", "✗".red().bold());
        println!("  Reason(s): {}", analysis.reboot_reasons.join(", ").yellow());
        return Ok(());
    }
    let active = std::path::PathBuf::from(crate::store::ACTIVE_LINK);
    let result = livepatch::apply_live(&new_entries, &active)?;
    let _lock  = lock::system_lock()?;
    profile::switch_active(gen)?;
    let mut gdb = GenerationsDb::load()?;
    gdb.current = pending_num; gdb.pending = None; gdb.save()?; clear_pending()?;
    println!("  {} Live patch applied ({} files updated).",
             "✔".bright_green().bold(), result.updated_files.to_string().bold());
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  export / import / key / gc / clean / init / relink / store
// ─────────────────────────────────────────────────────────────

fn cmd_export(args: &[String]) -> Result<()> {
    let output = args.first().map(|s| s.as_str()).unwrap_or("hammer-export.tar.gz");
    println!("  {} Exporting to {}…", "::".bold().cyan(), output.bold());
    let gens_db = GenerationsDb::load()?;
    let gen_num = gens_db.current;
    let gen = gens_db.current_gen().ok_or_else(|| anyhow::anyhow!("No current generation"))?;
    let manifest = serde_json::json!({
        "hammer_version": env!("CARGO_PKG_VERSION"),
                                     "generation": gen_num, "packages": gen.packages, "note": gen.note,
    });
    let out_file = std::fs::File::create(output)?;
    let enc      = flate2::write::GzEncoder::new(out_file, flate2::Compression::default());
    let mut tar  = tar::Builder::new(enc);
    let mb = serde_json::to_vec_pretty(&manifest)?;
    let mut hdr = tar::Header::new_gnu();
    hdr.set_size(mb.len() as u64); hdr.set_mode(0o644); hdr.set_cksum();
    tar.append_data(&mut hdr, "hammer-manifest.json", mb.as_slice())?;
    let mut added = 0usize;
    for pkg in &gen.packages {
        let sp = std::path::PathBuf::from(crate::store::STORE_DIR)
        .join(format!("{}-{}-{}", pkg.name, pkg.version, pkg.store_hash));
        if sp.exists() {
            tar.append_dir_all(format!("store/{}-{}-{}", pkg.name, pkg.version, pkg.store_hash), &sp)?;
            added += 1;
        }
    }
    tar.finish()?;
    println!("  {} Exported {} packages to {}", "✔".bright_green(), added.to_string().bold(), output.bold());
    Ok(())
}

async fn cmd_import_pkg(args: &[String]) -> Result<()> {
    use std::io::Read;
    let input = args.first().ok_or_else(|| anyhow::anyhow!("Usage: hammer import <file.tar.gz>"))?;
    let _lock = lock::system_lock()?;
    println!("  {} Importing from {}…", "::".bold().cyan(), input.bold());
    let file = std::fs::File::open(input)?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut manifest_json = String::new();
    for entry in tar.entries()? {
        let mut e = entry?;
        if e.path()?.to_string_lossy() == "hammer-manifest.json" { e.read_to_string(&mut manifest_json)?; }
    }
    let file2 = std::fs::File::open(input)?;
    let mut tar2 = tar::Archive::new(flate2::read::GzDecoder::new(file2));
    std::fs::create_dir_all(crate::store::STORE_DIR)?;
    let mut extracted = 0usize;
    for entry in tar2.entries()? {
        let mut e = entry?;
        let path = e.path()?.to_string_lossy().to_string();
        if path.starts_with("store/") {
            let dest = std::path::PathBuf::from(crate::store::STORE_DIR)
            .join(path.trim_start_matches("store/").trim_start_matches('/'));
            if let Some(p) = dest.parent() { std::fs::create_dir_all(p)?; }
            e.unpack(&dest).ok();
            extracted += 1;
        }
    }
    if manifest_json.is_empty() { bail!("No manifest found — not a hammer export?"); }
    let m: serde_json::Value = serde_json::from_str(&manifest_json)?;
    println!("  {} Imported from gen-{} (hammer {})", "✔".bright_green(),
             m["generation"].as_u64().unwrap_or(0), m["hammer_version"].as_str().unwrap_or("?"));
    println!("  {} {} store entries extracted.", "·".dimmed(), extracted.to_string().bold());
    Ok(())
}

async fn cmd_key(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" | "ls" => {
            let db = crate::gpg::KeyringDb::load()?;
            if db.keys.is_empty() { println!("  {} No trusted keys.", "·".dimmed()); return Ok(()); }
            println!("\n  {}", "Trusted GPG keys:".bold());
            for key in &db.keys {
                let fp = if key.fingerprint.len() >= 16 { &key.fingerprint[key.fingerprint.len()-16..] }
                else { &key.fingerprint };
                println!("  {} {}", fp.cyan().bold(), key.name.bold());
            }
        }
        "add" => {
            let source = args.get(1).ok_or_else(|| anyhow::anyhow!("Usage: hammer key add <url>"))?;
            let key = crate::gpg::import_key(source, &crate::download::HttpClient::new()).await?;
            println!("  {} Imported: {}", "✔".bright_green(), key.name.bold());
        }
        "remove" | "rm" => {
            let fp = args.get(1).ok_or_else(|| anyhow::anyhow!("Usage: hammer key remove <fp>"))?;
            let mut db = crate::gpg::KeyringDb::load()?;
            if db.remove(fp) { db.save()?; println!("  {} Key removed.", "✔".bright_green()); }
            else { println!("  {} Key not found.", "·".dimmed()); }
        }
        other => bail!("Unknown key subcommand: '{}'. Try: list, add, remove", other),
    }
    Ok(())
}

fn cmd_gc(args: &[String]) -> Result<()> {
    let keep: u32 = args.iter().find(|a| a.starts_with("--keep"))
    .and_then(|a| a.split('=').nth(1)).and_then(|v| v.parse().ok()).unwrap_or(3);
    let yes = has_flag(args, "-y");
    let _lock = lock::system_lock()?;
    let mut gdb = GenerationsDb::load()?;
    let active  = read_active_gen().unwrap_or(gdb.current);
    let pending = read_pending_gen();
    let mut cands: Vec<_> = gdb.generations.iter()
    .filter(|g| g.number != active && Some(g.number) != pending).cloned().collect();
    cands.sort_by(|a, b| b.number.cmp(&a.number));
    let to_del = cands[keep.min(cands.len() as u32) as usize..].to_vec();
    if to_del.is_empty() { println!("  {} Nothing to collect.", "·".dimmed()); return Ok(()); }
    println!("  Will delete {} old generation(s):", to_del.len());
    for g in &to_del { println!("    gen-{}", g.number); }
    if !yes && !ui::confirm("Proceed?")? { println!("  Aborted."); return Ok(()); }
    let del_nums: Vec<u32> = to_del.iter().map(|g| g.number).collect();
    for g in &to_del { profile::delete_profile(g)?; }
    gdb.generations.retain(|g| !del_nums.contains(&g.number));
    let referenced: std::collections::HashSet<String> = gdb.generations.iter()
    .flat_map(|g| g.packages.iter().map(|p| format!("{}-{}-{}", p.name, p.version, p.store_hash))).collect();
    Store::gc_unreferenced(&referenced)?;
    gdb.save()?;
    let _ = grub::update_grub(&gdb);
    println!("  {} Garbage collection complete.", "✔".bright_green());
    Ok(())
}

fn cmd_clean() -> Result<()> {
    let n = crate::download::clean_cache()?;
    println!("  {} Removed {} cached archive(s).", "✔".bright_green(), n);
    Ok(())
}

fn cmd_init(args: &[String]) -> Result<()> {
    if has_flag(args, "--user") {
        let env = UserEnv::current()?;
        env.init()?;
        let modified = userenv::install_shell_rc(&env)?;
        println!("  {} User hammer environment initialised.", "✔".bright_green());
        for f in &modified { println!("       {}", f.as_str().dimmed()); }
        return Ok(());
    }
    let suite = args.iter().find(|a| a.starts_with("--suite="))
    .and_then(|a| a.strip_prefix("--suite=")).unwrap_or("bookworm");
    println!("  {} Initialising hammer {} (suite: {})…", "::".bold().cyan(), env!("CARGO_PKG_VERSION"), suite.bold());
    for dir in &["/hammer/store", "/hammer/profiles", "/hammer/db", "/hammer/db/postinst",
        "/etc/hammer", "/etc/hammer/HackerOS", "/var/cache/hammer/archives",
        "/var/lib/hammer/lists", "/usr/lib/hammer", "/usr/lib/HackerOS/hammer"] {
            std::fs::create_dir_all(dir)?;
            println!("  {} {}", "·".dimmed(), dir.dimmed());
        }
        let arch = detect_arch();
    if !std::path::Path::new(crate::repo::SOURCES_HK).exists() {
        crate::repo::SourcesList::write_default(&arch, suite)?;
        println!("  {} Wrote {}", "✔".green(), crate::repo::SOURCES_HK);
    }
    profile::install_activate_service()?;
    if !std::path::Path::new(profile::GENERATIONS_FILE).exists() { GenerationsDb::default().save()?; }
    let gdb = GenerationsDb::load()?;
    if let Err(e) = grub::update_grub(&gdb) { println!("  {} GRUB: {} (non-fatal)", "·".yellow(), e.to_string().dimmed()); }
    println!("\n  {} hammer {} initialised.", "✔".bright_green().bold(), env!("CARGO_PKG_VERSION"));
    println!("    1. {} — add Debian archive key", "hammer key add https://ftp-master.debian.org/keys/archive-key-12.gpg".cyan());
    println!("    2. {} — refresh index", "hammer sync".cyan());
    println!("    3. {} — install packages", "hammer install <pkg>".cyan());
    Ok(())
}

fn cmd_relink() -> Result<()> {
    let gdb = GenerationsDb::load()?;
    let n   = read_active_gen().unwrap_or(gdb.current);
    let gen = gdb.get(n).ok_or_else(|| anyhow::anyhow!("Active generation {} not found", n))?.clone();
    let pp  = gen.profile_path();
    if !pp.exists() { bail!("Profile missing: {}\n  Try: hammer _activate", pp.display()); }
    match crate::profile::relink_bins(&pp) {
        Ok((l, u)) => {
            println!("  {} Linked: {}  Removed: {}", "✔".bright_green().bold(), l.to_string().bold(), u.to_string().dimmed());
            if l == 0 { println!("  {} No binaries to link.", "·".dimmed()); }
            else      { println!("  Packages now in PATH. Run {} if shell can't find them.", "hash -r".cyan()); }
        }
        Err(e) => bail!("relink failed: {}", e),
    }
    Ok(())
}

fn cmd_store() -> Result<()> {
    let s = "/usr/share/hammer/store";
    if !std::path::Path::new(s).exists() {
        bail!("Hammer Store not installed. Run: {}", "hammer install hammer-store".cyan());
    }
    println!("  {} Launching Hammer Store…", "⬡".bright_cyan().bold());
    if !std::process::Command::new(s).status()?.success() { bail!("hammer-store exited with error"); }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  _activate  (boot-time, called by systemd)
// ─────────────────────────────────────────────────────────────

fn cmd_activate_internal() -> Result<()> {
    println!("hammer: boot activation starting…");
    if let Some(bg) = grub::read_boot_gen() {
        println!("hammer: GRUB selected gen-{}", bg);
        let gdb = GenerationsDb::load()?;
        if let Some(gen) = gdb.get(bg) { profile::set_pending(gen)?; }
    }
    if let Some(pn) = read_pending_gen() {
        if let Err(e) = crate::gpg::verify_boot_integrity(pn) {
            eprintln!("hammer: BOOT INTEGRITY FAILED: {}", e);
            std::process::exit(1);
        }
    }
    let result = activate_pending()?;
    ui::print_activation_result(&result);
    if !result.scripts_failed.is_empty() {
        eprintln!("hammer: WARNING: postinst failed for: {}", result.scripts_failed.join(", "));
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  User-mode commands
// ─────────────────────────────────────────────────────────────

async fn cmd_user_install(env: &UserEnv, args: &[String], flags: &GlobalFlags) -> Result<()> {
    let yes   = has_flag(args, "-y") || flags.yes;
    let names: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    if names.is_empty() { bail!("Usage: hammer --user install <package...>"); }
    let arch  = flags.arch.as_ref().map(|a| userenv::normalise_arch(a)).transpose()?
    .unwrap_or_else(detect_arch);
    let _lock = lock::user_lock(&env.hammer_dir)?;
    let cache = PackageCache::load_for_arch(&arch)?;
    let db    = InstalledDb::open_at(&env.db_path.to_string_lossy())?;
    let plan  = Solver::new(&cache, &db).resolve_install(&names, false)?;
    if plan.is_empty() { ui::nothing_to_do(); return Ok(()); }
    ui::print_transaction_table(&plan, &arch);
    if !yes && !ui::confirm("Install to user profile?")? { println!("  Aborted."); return Ok(()); }
    let client = crate::download::HttpClient::new();
    let dl     = crate::download::download_packages(&client, &plan.to_install).await?;
    let store  = userenv::UserStore::new(env);
    let mut entries = Vec::new();
    for d in &dl {
        let bytes = std::fs::read(&d.path)?;
        let deb   = crate::deb::DebPackage::parse(&bytes)?;
        entries.push(store.install_deb(&d.package, &deb)?);
    }
    let mut gdb      = crate::profile::GenerationsDb::load_from(&env.gens_file)?;
    let gen_num      = gdb.next_number();
    let profile_path = userenv::compose_user_profile(env, gen_num, &entries)?;
    let tmp = env.hammer_dir.join(".active.tmp");
    if tmp.symlink_metadata().is_ok() { std::fs::remove_file(&tmp)?; }
    std::os::unix::fs::symlink(&profile_path, &tmp)?;
    std::fs::rename(&tmp, &env.active_link)?;
    let gen = crate::profile::Generation {
        number: gen_num, timestamp: chrono::Utc::now(),
        packages: entries.iter().map(|e| crate::profile::GenPackage {
            name: e.name.clone(), version: e.version.clone(), store_hash: e.hash.clone(),
        }).collect(),
        note: Some(format!("install {}", names.join(" "))),
        state: Some(crate::profile::GenState::Active),
    };
    gdb.generations.push(gen); gdb.current = gen_num; gdb.save_to(&env.gens_file)?;
    println!("  {} Installed {} package(s) to user profile.", "✔".bright_green(), entries.len().to_string().bold());
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  User remove  — FIX: was stub, now fully implemented
// ─────────────────────────────────────────────────────────────

async fn cmd_user_remove(env: &UserEnv, args: &[String]) -> Result<()> {
    let yes   = has_flag(args, "-y");
    let names: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    if names.is_empty() { bail!("Usage: hammer --user remove <package...> [-y]"); }

    let _lock  = lock::user_lock(&env.hammer_dir)?;
    let db_str = env.db_path.to_string_lossy().to_string();
    let db     = InstalledDb::open_at(&db_str)?;

    // Check all are installed
    let not_found: Vec<_> = names.iter().filter(|n| db.get(n).is_none()).cloned().collect();
    if !not_found.is_empty() {
        bail!("Not installed in user profile: {}", not_found.join(", "));
    }

    println!("  {} Removing from user profile: {}", "⬡".cyan().bold(), names.join(", ").bold());
    if !yes && !ui::confirm("Proceed?")? { println!("  Aborted."); return Ok(()); }

    // Remove from DB
    let mut gdb = crate::profile::GenerationsDb::load_from(&env.gens_file)?;
    for name in &names {
        if let Some(inst) = db.get(name) { db.record_remove(name, &inst.version, gdb.current)?; }
    }

    // Rebuild profile without removed packages
    let remaining: Vec<crate::store::StoreEntry> = db.list_all()?.iter()
    .filter(|p| !names.contains(&p.name))
    .filter_map(|p| {
        let path = std::path::PathBuf::from(crate::store::STORE_DIR)
        .join(format!("{}-{}-{}", p.name, p.version, p.store_hash));
        if path.exists() {
            Some(crate::store::StoreEntry {
                name: p.name.clone(), version: p.version.clone(),
                 hash: p.store_hash.clone(), path,
            })
        } else { None }
    })
    .collect();

    let gen_num = gdb.next_number();
    let profile = userenv::compose_user_profile(env, gen_num, &remaining)?;
    let tmp = env.hammer_dir.join(".active.tmp");
    if tmp.symlink_metadata().is_ok() { std::fs::remove_file(&tmp)?; }
    std::os::unix::fs::symlink(&profile, &tmp)?;
    std::fs::rename(&tmp, &env.active_link)?;

    let gen = crate::profile::Generation {
        number: gen_num, timestamp: chrono::Utc::now(),
        packages: remaining.iter().map(|e| crate::profile::GenPackage {
            name: e.name.clone(), version: e.version.clone(), store_hash: e.hash.clone(),
        }).collect(),
        note:  Some(format!("remove {}", names.join(" "))),
        state: Some(crate::profile::GenState::Active),
    };
    gdb.generations.push(gen); gdb.current = gen_num; gdb.save_to(&env.gens_file)?;

    // Remove wrappers
    for name in &names {
        let w = env.bin_dir.join(name);
        if w.symlink_metadata().is_ok() { std::fs::remove_file(&w).ok(); }
    }
    println!("  {} Removed {} package(s) from user profile.", "✔".bright_green(), names.len().to_string().bold());
    Ok(())
}

fn cmd_user_list(env: &UserEnv) -> Result<()> {
    println!("  {} User packages at {}", "⬡".cyan().bold(), env.hammer_dir.display());
    if !env.gens_file.exists() {
        println!("  {} No user packages installed. Run: {}", "·".dimmed(), "hammer init --user".cyan());
        return Ok(());
    }
    let gdb = crate::profile::GenerationsDb::load_from(&env.gens_file)?;
    if let Some(gen) = gdb.current_gen() {
        println!("  Active: gen-{} ({} packages)", gen.number, gen.package_count());
        for pkg in &gen.packages {
            println!("    {} {} {}", "·".dimmed(), pkg.name.bold(), pkg.version.dimmed());
        }
    }
    Ok(())
}

fn cmd_user_status(env: &UserEnv) -> Result<()> {
    println!("  {}", "User hammer environment".bold());
    println!("  {:<26} {}", "Location:".bold(), env.hammer_dir.display());
    let active = env.active_link.exists() || env.active_link.symlink_metadata().is_ok();
    println!("  {:<26} {}", "Initialised:".bold(),
             if active { "yes".bright_green().to_string() }
             else { "no — run hammer init --user".yellow().to_string() });
    Ok(())
}

fn cmd_user_shell_init(env: &UserEnv) -> Result<()> {
    let modified = userenv::install_shell_rc(env)?;
    if modified.is_empty() { println!("  {} Shell integration already installed.", "·".dimmed()); }
    else { for f in &modified { println!("       {}", f.as_str().dimmed()); } }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Help
// ─────────────────────────────────────────────────────────────

fn print_help() {
    println!();
    println!("  {} {}  {}", "⬡ hammer".bold().bright_cyan(),
             format!("v{}", env!("CARGO_PKG_VERSION")).dimmed(), "Apache-2.0".dimmed());
    println!("  Atomic Debian package manager — HackerOS");
    println!();
    println!("  {}", "Package management:".bold());
    println!("    {}  install <pkg...> [-y] [--arch=ARCH] [--no-recommends]", "install".cyan());
    println!("    {}   remove <pkg...> [-y]", "remove".cyan());
    println!("    {}  reinstall <pkg...> [-y]", "reinstall".cyan());
    println!("    {}  upgrade [pkg...] [-y] [--system|--hackeros|--hammer]", "upgrade".cyan());
    println!("    {}  dist-upgrade [-y]  (e.g. debian 14→15)", "dist-upgrade".cyan());
    println!("    {}  autoremove  fix-broken  sync  self-update", "".cyan());
    println!();
    println!("  {}", "Diagnostics:".bold());
    println!("    {}  [package]    verify store integrity", "verify".cyan());
    println!("    {}              system health check", "doctor".cyan());
    println!();
    println!("  {}", "Generations:".bold());
    println!("    {}  pending  gen  rollback  diff", "".cyan());
    println!();
    println!("  {}", "Query / maintenance:".bold());
    println!("    {}  search  info  list  status  history", "".cyan());
    println!("    {}  gc  clean  init  relink  store  key", "".cyan());
    println!("    {}  export  import", "".cyan());
    println!();
    println!("  {}", "User packages (no reboot needed):".bold());
    println!("    {} install|remove|list|status|init", "--user".yellow());
    println!();
    println!("  {}", "Config:".bold());
    println!("    {}  (format: .hk)", SOURCES_HK.cyan());
    println!("    {}", "/etc/hammer/HackerOS/*.hk".cyan());
    println!();
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}
