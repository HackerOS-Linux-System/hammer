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
            } else {
                true
            }
        });
        if f.yes && !args.contains(&"-y".to_string()) {
            args.push("-y".to_string());
        }
        f
    }
}

// ─────────────────────────────────────────────────────────────
//  CLI entry point
// ─────────────────────────────────────────────────────────────

pub async fn run(mut args: Vec<String>) -> Result<()> {
    let flags = GlobalFlags::parse(&mut args);
    let cmd   = args.get(1).map(|s| s.as_str()).unwrap_or("help");
    let rest  = args[2.min(args.len())..].to_vec();

    // ── Internal / hidden commands (no live check) ────────────
    // These are only used from live-build hooks and must never
    // appear in `hammer help`.
    match cmd {
        "_activate"   => return cmd_activate_internal(),
        "_setup"      => return setup::cmd_setup().await,
        "_import"     => return setup::cmd_import().await,
        "version" | "--version" => {
            println!(
                "  {} {}  {}",
                "⬡ hammer".bright_cyan().bold(),
                env!("CARGO_PKG_VERSION").bold(),
                "Apache-2.0".dimmed()
            );
            return Ok(());
        }
        "help" | "--help" | "-h" | "" => { print_help(); return Ok(()); }
        _ => {}
    }

    // ── Live system guard ─────────────────────────────────────
    // All user-facing commands require an installed system.
    livecheck::assert_not_live();

    if flags.user_mode {
        return run_user(cmd, &rest, &flags).await;
    }

    match cmd {
        // ── Package management ────────────────────────────────
        "install"    | "in"              => cmd_install(&rest, false, &flags).await,
        "remove"     | "rm"              => cmd_remove(&rest, false).await,
        "upgrade"    | "up"              => cmd_upgrade(&rest, false, &flags).await,
        "autoremove" | "ar"              => cmd_autoremove(false).await,

        // ── Index ─────────────────────────────────────────────
        "sync"       | "ref" | "update"  => cmd_sync().await,

        // ── Self-update ───────────────────────────────────────
        "self-update" | "selfupdate"     => cmd_self_update().await,

        // ── Query ─────────────────────────────────────────────
        "search"     | "se"              => cmd_search(&rest, &flags),
        "info"                           => cmd_info(&rest, &flags),
        "list"       | "ls"              => cmd_list(&rest),

        // ── Status / history ──────────────────────────────────
        "status"     | "st"              => cmd_status(),
        "history"    | "hi"              => cmd_history(),

        // ── Generations ───────────────────────────────────────
        "gen"                            => cmd_gen(&rest).await,
        "rollback"   | "rb"              => cmd_rollback(),
        "diff"                           => cmd_diff(&rest),

        // ── Pending ───────────────────────────────────────────
        "pending"                        => cmd_pending(&rest),

        // ── Export / import ───────────────────────────────────
        "export"                         => cmd_export(&rest),
        "import"                         => cmd_import_pkg(&rest).await,

        // ── Key management ────────────────────────────────────
        "key"                            => cmd_key(&rest).await,

        // ── Maintenance ───────────────────────────────────────
        "gc"                             => cmd_gc(&rest),
        "clean"                          => cmd_clean(),
        "init"                           => cmd_init(&rest),
        "relink"                         => cmd_relink(),

        // ── Graphical store ───────────────────────────────────
        "store"                          => cmd_store(),

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
        "remove"  | "rm" => cmd_user_remove(&env, args),
        "list"    | "ls" => cmd_user_list(&env),
        "status"  | "st" => cmd_user_status(&env),
        "init"           => { env.init()?; cmd_user_shell_init(&env) }
        other => bail!(
            "hammer --user: unknown command '{}'. Try: install, remove, list, status, init",
            other
        ),
    }
}

// ─────────────────────────────────────────────────────────────
//  install
// ─────────────────────────────────────────────────────────────

async fn cmd_install(args: &[String], pre_confirmed: bool, flags: &GlobalFlags) -> Result<()> {
    let yes       = pre_confirmed || has_flag(args, "-y") || has_flag(args, "--yes");
    let no_recomm = has_flag(args, "--no-recommends");
    let names: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    if names.is_empty() {
        bail!("Usage: hammer install <package...> [-y] [--arch=ARCH] [--no-recommends]");
    }

    let target_arch = flags.arch.as_ref()
        .map(|a| userenv::normalise_arch(a)).transpose()?
        .unwrap_or_else(detect_arch);

    ui::print_header();
    let db    = InstalledDb::open()?;
    let cache = PackageCache::load_for_arch(&target_arch)?;

    if flags.arch.is_some() && !userenv::is_native_arch(&target_arch) {
        println!("  {} Cross-arch: installing {} packages on {} host",
                 "⚡".yellow().bold(), target_arch.bold(), detect_arch().dimmed());
    }

    let solver = Solver::new(&cache, &db);
    let plan   = solver.resolve_install(&names, no_recomm)?;
    if plan.is_empty() { ui::nothing_to_do(); return Ok(()); }

    ui::deps_resolved();
    ui::print_transaction_table(&plan, &target_arch);
    ui::print_transaction_summary(&plan);

    if !yes && !ui::confirm("Proceed with installation?")? {
        println!("  Aborted."); return Ok(());
    }

    let note    = format!("install {}", names.join(" "));
    let ctx     = TransactionContext::system(&plan, &db, &names, false);
    let gen_num = execute_transaction(ctx, &note).await?;

    if let Ok(gens_db) = GenerationsDb::load() { let _ = grub::update_grub(&gens_db); }
    ui::print_pending_notice(gen_num);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  remove
// ─────────────────────────────────────────────────────────────

async fn cmd_remove(args: &[String], pre_confirmed: bool) -> Result<()> {
    let yes   = pre_confirmed || has_flag(args, "-y");
    let names: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    if names.is_empty() { bail!("Usage: hammer remove <package...> [-y]"); }

    ui::print_header();
    let db     = InstalledDb::open()?;
    let cache  = PackageCache::load()?;
    let solver = Solver::new(&cache, &db);
    let plan   = solver.resolve_remove(&names)?;
    if plan.is_empty() { ui::nothing_to_do(); return Ok(()); }

    ui::print_transaction_table(&plan, &detect_arch());
    ui::print_transaction_summary(&plan);
    if !yes && !ui::confirm("Proceed with removal?")? {
        println!("  Aborted."); return Ok(());
    }

    let note    = format!("remove {}", names.join(" "));
    let ctx     = TransactionContext::system(&plan, &db, &names, false);
    let gen_num = execute_transaction(ctx, &note).await?;

    if let Ok(gens_db) = GenerationsDb::load() { let _ = grub::update_grub(&gens_db); }
    ui::print_pending_notice(gen_num);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  upgrade  — Debian packages + HackerOS .hk tools + self
// ─────────────────────────────────────────────────────────────

async fn cmd_upgrade(args: &[String], pre_confirmed: bool, flags: &GlobalFlags) -> Result<()> {
    let yes  = pre_confirmed || has_flag(args, "-y");
    let pkgs: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    ui::print_header();

    let arch   = flags.arch.as_ref()
        .map(|a| userenv::normalise_arch(a)).transpose()?
        .unwrap_or_else(detect_arch);
    let db     = InstalledDb::open()?;
    let cache  = PackageCache::load_for_arch(&arch)?;
    let solver = Solver::new(&cache, &db);
    let plan   = if pkgs.is_empty() { solver.resolve_upgrade()? }
                 else { solver.resolve_install(&pkgs, false)? };

    // ── Debian packages ───────────────────────────────────────
    if !plan.is_empty() {
        ui::deps_resolved();
        ui::print_transaction_table(&plan, &arch);
        ui::print_transaction_summary(&plan);
        if !yes && !ui::confirm("Proceed with upgrade?")? {
            println!("  Aborted."); return Ok(());
        }
        let explicit: Vec<String> = plan.to_upgrade.iter().map(|p| p.name.clone()).collect();
        let ctx     = TransactionContext::system(&plan, &db, &explicit, true);
        let gen_num = execute_transaction(ctx, "upgrade").await?;
        if let Ok(gens_db) = GenerationsDb::load() { let _ = grub::update_grub(&gens_db); }
        ui::print_pending_notice(gen_num);
    } else {
        println!("  {} All Debian packages up to date.", "✔".bright_green());
    }

    // ── HackerOS .hk tools ────────────────────────────────────
    let client = crate::download::HttpClient::new();
    hk_tools::update_all_tools(&client).await?;

    // ── Self-update check ─────────────────────────────────────
    println!(
        "  {}  Checking for hammer self-update…",
        "⬡".cyan().bold()
    );
    match selfupdate::check_for_update(&client).await {
        Ok(Some(new_ver)) => {
            println!(
                "  {} New hammer version available: {}",
                "↑".yellow().bold(), new_ver.bright_cyan()
            );
            println!(
                "  Run {} to update.",
                "hammer self-update".cyan()
            );
        }
        Ok(None) => {
            println!("  {} hammer is up to date.", "✔".bright_green());
        }
        Err(e) => {
            println!("  {} Self-update check failed: {}", "!".yellow(), e);
        }
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  autoremove
// ─────────────────────────────────────────────────────────────

async fn cmd_autoremove(pre_confirmed: bool) -> Result<()> {
    let db     = InstalledDb::open()?;
    let cache  = PackageCache::load()?;
    let solver = Solver::new(&cache, &db);
    let plan   = solver.resolve_autoremove()?;
    if plan.is_empty() { ui::nothing_to_do(); return Ok(()); }

    ui::print_transaction_table(&plan, &detect_arch());
    ui::print_transaction_summary(&plan);
    if !pre_confirmed && !ui::confirm("Remove unused dependencies?")? {
        println!("  Aborted."); return Ok(());
    }

    let removals: Vec<String> = plan.to_autoremove.clone();
    let ctx     = TransactionContext::system(&plan, &db, &removals, false);
    let gen_num = execute_transaction(ctx, "autoremove").await?;
    if let Ok(gens_db) = GenerationsDb::load() { let _ = grub::update_grub(&gens_db); }
    ui::print_pending_notice(gen_num);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  sync
// ─────────────────────────────────────────────────────────────

async fn cmd_sync() -> Result<()> {
    ui::print_header();
    crate::cache::sync_all().await?;
    println!("  {} Package index updated.", "✔".bright_green());
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  self-update
// ─────────────────────────────────────────────────────────────

async fn cmd_self_update() -> Result<()> {
    ui::print_header();
    let client = crate::download::HttpClient::new();
    selfupdate::self_update(&client).await
}

// ─────────────────────────────────────────────────────────────
//  search
// ─────────────────────────────────────────────────────────────

fn cmd_search(args: &[String], flags: &GlobalFlags) -> Result<()> {
    let installed_only = has_flag(args, "--installed");
    let query: String  = args.iter().filter(|a| !a.starts_with('-'))
        .cloned().collect::<Vec<_>>().join(" ");
    if query.is_empty() { bail!("Usage: hammer search <query> [--installed] [--arch=ARCH]"); }

    let arch  = flags.arch.as_ref()
        .map(|a| userenv::normalise_arch(a)).transpose()?
        .unwrap_or_else(detect_arch);
    let db    = InstalledDb::open()?;
    let cache = PackageCache::load_for_arch(&arch)?;

    let mut results: Vec<_> = cache.search(&query)
        .into_iter()
        .filter(|p| !installed_only || db.is_installed(&p.name))
        .collect();
    results.sort_by(|a, b| a.name.cmp(&b.name));

    ui::print_search_header(&query, results.len());
    for pkg in &results {
        ui::print_search_result(pkg, db.is_installed(&pkg.name));
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  info
// ─────────────────────────────────────────────────────────────

fn cmd_info(args: &[String], flags: &GlobalFlags) -> Result<()> {
    let name = args.first().ok_or_else(|| anyhow::anyhow!("Usage: hammer info <package>"))?;
    let arch  = flags.arch.as_ref()
        .map(|a| userenv::normalise_arch(a)).transpose()?
        .unwrap_or_else(detect_arch);
    let db    = InstalledDb::open()?;
    let cache = PackageCache::load_for_arch(&arch)?;

    let pkg = cache.get(name).ok_or_else(|| {
        anyhow::anyhow!("Package '{}' not found. Run `hammer sync` first.", name)
    })?;

    let inst_pkg = db.get(name);
    ui::print_package_info(pkg, inst_pkg.is_some(), inst_pkg.as_ref().map(|p| p.version.as_str()));
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  list
// ─────────────────────────────────────────────────────────────

fn cmd_list(args: &[String]) -> Result<()> {
    let installed_only = has_flag(args, "--installed") || has_flag(args, "-i");
    let upgrades_only  = has_flag(args, "--upgrades")  || has_flag(args, "-u");
    let db    = InstalledDb::open()?;
    let cache = PackageCache::load()?;

    if installed_only || upgrades_only {
        for inst in db.list_all()? {
            let new_ver = cache.get(&inst.name)
                .filter(|av| crate::package::version_cmp(&av.version, &inst.version)
                    == std::cmp::Ordering::Greater)
                .map(|av| av.version.as_str());
            if upgrades_only && new_ver.is_none() { continue; }
            let repo = cache.get(&inst.name)
                .and_then(|p| p.repo_base_uri.as_deref())
                .and_then(|u| u.split('/').last())
                .unwrap_or("installed");
            ui::print_list_entry(&inst.name, &inst.version, &inst.architecture, true, repo, new_ver);
        }
    } else {
        for pkg in cache.all_packages() {
            let inst = db.is_installed(&pkg.name);
            let repo = pkg.repo_base_uri.as_deref()
                .and_then(|u| u.split('/').last()).unwrap_or("");
            ui::print_list_entry(&pkg.name, &pkg.version, &pkg.architecture, inst, repo, None);
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  status
// ─────────────────────────────────────────────────────────────

fn cmd_status() -> Result<()> {
    let db = InstalledDb::open()?;
    ui::print_status(&db);
    println!("  {:<26} {}", "GRUB integration:".bold(), grub::grub_status().dimmed());
    if let Some(boot_gen) = grub::read_boot_gen() {
        println!("  {:<26} gen-{}", "Booted with:".bold(), boot_gen.to_string().cyan().bold());
    }

    // Show HackerOS .hk tools
    let tools = hk_tools::list_tools();
    if !tools.is_empty() {
        println!();
        println!("  {}", "HackerOS tools:".bold());
        for (name, ver, desc) in &tools {
            let desc_short: String = desc.chars().take(40).collect();
            println!("    {} {} {}  {}",
                     "·".dimmed(), name.bold(), ver.cyan(), desc_short.dimmed());
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  history
// ─────────────────────────────────────────────────────────────

fn cmd_history() -> Result<()> {
    let db      = InstalledDb::open()?;
    let entries = db.history(50)?;
    ui::print_history(&entries);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  diff
// ─────────────────────────────────────────────────────────────

fn cmd_diff(args: &[String]) -> Result<()> {
    let gens_db = GenerationsDb::load()?;
    let (a, b) = if args.is_empty() {
        let cur     = gens_db.current;
        let pending = gens_db.pending.ok_or_else(|| {
            anyhow::anyhow!("No pending generation. Use: hammer diff <A> <B>")
        })?;
        (cur, pending)
    } else if args.len() == 1 {
        let n: u32 = args[0].parse()
            .map_err(|_| anyhow::anyhow!("'{}' is not a generation number", args[0]))?;
        (n, gens_db.current)
    } else {
        let na: u32 = args[0].parse()
            .map_err(|_| anyhow::anyhow!("'{}' is not a generation number", args[0]))?;
        let nb: u32 = args[1].parse()
            .map_err(|_| anyhow::anyhow!("'{}' is not a generation number", args[1]))?;
        (na, nb)
    };
    let diff = compute_diff(a, b, &gens_db)?;
    ui::print_diff(&diff);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  gen
// ─────────────────────────────────────────────────────────────

async fn cmd_gen(args: &[String]) -> Result<()> {
    match args.first().map(|s| s.as_str()).unwrap_or("list") {
        "list" | "ls" => {
            let gens_db = GenerationsDb::load()?;
            ui::print_generations(&gens_db);
        }
        "switch" => {
            let n: u32 = args.get(1)
                .ok_or_else(|| anyhow::anyhow!("Usage: hammer gen switch <N>"))?
                .parse()?;
            cmd_gen_switch(n)?;
        }
        other => bail!("Unknown gen subcommand: '{}'. Try: list, switch", other),
    }
    Ok(())
}

fn cmd_gen_switch(n: u32) -> Result<()> {
    let gens_db = GenerationsDb::load()?;
    let gen = gens_db.get(n)
        .ok_or_else(|| anyhow::anyhow!("Generation {} not found.", n))?.clone();
    println!("  Switching to gen-{}…", n);
    profile::switch_active(&gen)?;
    let mut gens_db = GenerationsDb::load()?;
    gens_db.current = n;
    gens_db.pending = None;
    clear_pending().ok();
    gens_db.save()?;
    let _ = grub::update_grub(&gens_db);
    println!("  {} Now running gen-{}.", "✔".bright_green(), n);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  rollback
// ─────────────────────────────────────────────────────────────

fn cmd_rollback() -> Result<()> {
    let gens_db = GenerationsDb::load()?;
    let mut prev: Vec<_> = gens_db.generations.iter()
        .filter(|g| g.number < gens_db.current).collect();
    prev.sort_by(|a, b| b.number.cmp(&a.number));
    let p = prev.first().ok_or_else(|| anyhow::anyhow!("No previous generation."))?;
    println!("  Rolling back: gen-{} → gen-{}…", gens_db.current, p.number);
    profile::switch_active(p)?;
    let mut gens_db = GenerationsDb::load()?;
    gens_db.current = p.number;
    gens_db.pending = None;
    clear_pending().ok();
    gens_db.save()?;
    let _ = grub::update_grub(&gens_db);
    println!("  {} Rolled back to gen-{}.", "✔".bright_green(), p.number);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  pending
// ─────────────────────────────────────────────────────────────

fn cmd_pending(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("show");
    match sub {
        "show" | "status" => {
            let gens_db = GenerationsDb::load()?;
            match read_pending_gen() {
                None => println!("  {} No pending changes.", "·".dimmed()),
                Some(n) => {
                    println!("\n  {} Pending: {}",
                             "⬡".bright_yellow().bold(),
                             format!("gen-{}", n).bold().bright_yellow());
                    if let Some(gen) = gens_db.get(n) {
                        if let Some(ref note) = gen.note {
                            println!("  Operation : {}", note.cyan());
                        }
                        println!("  Packages  : {}", gen.package_count());
                    }
                    println!("\n  Will activate on next reboot.");
                    println!("  Diff:    {}", "hammer diff".cyan());
                    println!("  Cancel:  {}", "hammer pending cancel".cyan());
                }
            }
        }
        "cancel" => match read_pending_gen() {
            None => println!("  {} No pending changes to cancel.", "·".dimmed()),
            Some(n) => {
                clear_pending()?;
                let mut gens_db = GenerationsDb::load()?;
                gens_db.pending = None;
                if let Some(g) = gens_db.generations.iter_mut().find(|g| g.number == n) {
                    g.state = Some(crate::profile::GenState::Old);
                }
                gens_db.save()?;
                let _ = grub::update_grub(&gens_db);
                println!("  {} Pending gen-{} cancelled.", "✔".bright_green(), n);
            }
        },
        "apply-live" => cmd_pending_apply_live()?,
        other => bail!("Unknown pending subcommand: '{}'. Try: show, cancel, apply-live", other),
    }
    Ok(())
}

fn cmd_pending_apply_live() -> Result<()> {
    let pending_num = match read_pending_gen() {
        Some(n) => n,
        None => { println!("  {} No pending changes.", "·".dimmed()); return Ok(()); }
    };
    let gens_db = GenerationsDb::load()?;
    let gen = gens_db.get(pending_num)
        .ok_or_else(|| anyhow::anyhow!("Pending gen not in DB"))?;
    let current_pkgs: std::collections::HashSet<String> = gens_db.current_gen()
        .map(|g| g.packages.iter().map(|p| p.name.clone()).collect())
        .unwrap_or_default();
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

    let all_files = livepatch::collect_files(&new_entries);
    let analysis  = livepatch::analyse(&all_files);
    if !analysis.can_live_patch {
        println!("  {} Cannot apply live — reboot required.", "✗".red().bold());
        println!("  Reason(s): {}", analysis.reboot_reasons.join(", ").yellow());
        return Ok(());
    }
    let active = std::path::PathBuf::from(crate::store::ACTIVE_LINK);
    let result = livepatch::apply_live(&new_entries, &active)?;
    profile::switch_active(gen)?;
    let mut gens_db = GenerationsDb::load()?;
    gens_db.current = pending_num;
    gens_db.pending = None;
    gens_db.save()?;
    clear_pending()?;
    println!("  {} Live patch applied ({} files updated).",
             "✔".bright_green().bold(), result.updated_files.to_string().bold());
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  export / import (package store archives)
// ─────────────────────────────────────────────────────────────

fn cmd_export(args: &[String]) -> Result<()> {
    let output = args.first().map(|s| s.as_str()).unwrap_or("hammer-export.tar.gz");
    println!("  {} Exporting current generation to {}…", "::".bold().cyan(), output.bold());

    let gens_db = GenerationsDb::load()?;
    let gen_num = gens_db.current;
    let gen = gens_db.current_gen()
        .ok_or_else(|| anyhow::anyhow!("No current generation to export"))?;

    let manifest = serde_json::json!({
        "hammer_version": env!("CARGO_PKG_VERSION"),
        "exported_at":    chrono::Utc::now().to_rfc3339(),
        "generation":     gen_num,
        "packages":       gen.packages,
        "note":           gen.note,
    });
    let manifest_str = serde_json::to_string_pretty(&manifest)?;

    let out_file = std::fs::File::create(output)?;
    let enc      = flate2::write::GzEncoder::new(out_file, flate2::Compression::default());
    let mut tar  = tar::Builder::new(enc);

    let manifest_bytes = manifest_str.as_bytes();
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest_bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, "hammer-manifest.json", manifest_bytes)?;

    let mut added = 0usize;
    for pkg in &gen.packages {
        let store_path = std::path::PathBuf::from(crate::store::STORE_DIR)
            .join(format!("{}-{}-{}", pkg.name, pkg.version, pkg.store_hash));
        if store_path.exists() {
            tar.append_dir_all(
                format!("store/{}-{}-{}", pkg.name, pkg.version, pkg.store_hash),
                &store_path,
            )?;
            added += 1;
        }
    }
    tar.finish()?;
    println!("  {} Exported {} packages to {}", "✔".bright_green(), added.to_string().bold(), output.bold());
    println!("       {}", format!("hammer import {}", output).cyan());
    Ok(())
}

async fn cmd_import_pkg(args: &[String]) -> Result<()> {
    use std::io::Read;
    let input = args.first()
        .ok_or_else(|| anyhow::anyhow!("Usage: hammer import <file.tar.gz>"))?;
    println!("  {} Importing from {}…", "::".bold().cyan(), input.bold());

    let file    = std::fs::File::open(input)?;
    let dec     = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(dec);
    let mut manifest_json = String::new();

    for entry in tar.entries()? {
        let mut entry = entry?;
        if entry.path()?.to_string_lossy() == "hammer-manifest.json" {
            entry.read_to_string(&mut manifest_json)?;
        }
    }

    let file2    = std::fs::File::open(input)?;
    let dec2     = flate2::read::GzDecoder::new(file2);
    let mut tar2 = tar::Archive::new(dec2);
    std::fs::create_dir_all(crate::store::STORE_DIR)?;
    let mut extracted = 0usize;

    for entry in tar2.entries()? {
        let mut entry = entry?;
        let path      = entry.path()?.to_string_lossy().to_string();
        if path.starts_with("store/") {
            let dest = std::path::PathBuf::from(crate::store::STORE_DIR)
                .join(path.trim_start_matches("store/").trim_start_matches('/'));
            if let Some(parent) = dest.parent() { std::fs::create_dir_all(parent)?; }
            entry.unpack(&dest).ok();
            extracted += 1;
        }
    }

    if manifest_json.is_empty() {
        bail!("No manifest found in archive — not a hammer export?");
    }
    let manifest: serde_json::Value = serde_json::from_str(&manifest_json)?;
    println!("  {} Imported from gen-{} (hammer {})", "✔".bright_green(),
             manifest["generation"].as_u64().unwrap_or(0),
             manifest["hammer_version"].as_str().unwrap_or("?"));
    println!("  {} {} store entries extracted.", "·".dimmed(), extracted.to_string().bold());
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  key
// ─────────────────────────────────────────────────────────────

async fn cmd_key(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" | "ls" => {
            let db = crate::gpg::KeyringDb::load()?;
            if db.keys.is_empty() {
                println!("  {} No trusted keys.", "·".dimmed());
                println!("  Add a key: {}", "hammer key add <url|file>".cyan());
                return Ok(());
            }
            println!("\n  {}", "Trusted GPG keys:".bold());
            println!("  {}", "─".repeat(70).dimmed());
            for key in &db.keys {
                let fp = if key.fingerprint.len() >= 16 {
                    &key.fingerprint[key.fingerprint.len()-16..]
                } else { &key.fingerprint };
                println!("  {} {}", fp.cyan().bold(), key.name.bold());
                if let Some(ref email) = key.email { println!("    {}", email.dimmed()); }
                println!("    Added: {}  Source: {}", key.added_at.dimmed(), key.source.dimmed());
            }
            println!("  {}", "─".repeat(70).dimmed());
        }
        "add" => {
            let source = args.get(1)
                .ok_or_else(|| anyhow::anyhow!("Usage: hammer key add <url|/path/to/key.gpg>"))?;
            println!("  {} Importing key from {}…", "::".bold().cyan(), source.bold());
            let client = crate::download::HttpClient::new();
            let key    = crate::gpg::import_key(source, &client).await?;
            let fp = if key.fingerprint.len() >= 16 {
                &key.fingerprint[key.fingerprint.len()-16..]
            } else { &key.fingerprint };
            println!("  {} Imported: {} ({})", "✔".bright_green(), key.name.bold(), fp.cyan());
        }
        "remove" | "rm" => {
            let fp = args.get(1)
                .ok_or_else(|| anyhow::anyhow!("Usage: hammer key remove <fingerprint>"))?;
            let mut db = crate::gpg::KeyringDb::load()?;
            if db.remove(fp) {
                db.save()?;
                let dir = std::path::Path::new(crate::gpg::KEYRING_DIR);
                for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                    if entry.file_name().to_string_lossy().contains(&fp[..8.min(fp.len())]) {
                        std::fs::remove_file(entry.path()).ok();
                    }
                }
                println!("  {} Key {} removed.", "✔".bright_green(), fp.bold());
            } else {
                println!("  {} Key not found: {}", "·".dimmed(), fp);
            }
        }
        other => bail!("Unknown key subcommand: '{}'. Try: list, add, remove", other),
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  gc / clean / init / relink / store
// ─────────────────────────────────────────────────────────────

fn cmd_gc(args: &[String]) -> Result<()> {
    let keep: u32 = args.iter().find(|a| a.starts_with("--keep"))
        .and_then(|a| a.split('=').nth(1)).and_then(|v| v.parse().ok()).unwrap_or(3);
    let yes = has_flag(args, "-y");

    let mut gens_db = GenerationsDb::load()?;
    let active      = read_active_gen().unwrap_or(gens_db.current);
    let pending     = read_pending_gen();

    let mut candidates: Vec<_> = gens_db.generations.iter()
        .filter(|g| g.number != active && Some(g.number) != pending)
        .cloned().collect();
    candidates.sort_by(|a, b| b.number.cmp(&a.number));
    let to_delete = candidates[keep.min(candidates.len() as u32) as usize..].to_vec();

    if to_delete.is_empty() { println!("  {} Nothing to collect.", "·".dimmed()); return Ok(()); }

    println!("  Will delete {} old generation(s):", to_delete.len());
    for g in &to_delete {
        println!("    gen-{} ({})", g.number, g.timestamp.format("%Y-%m-%d").to_string().dimmed());
    }
    if !yes && !ui::confirm("Proceed?")? { println!("  Aborted."); return Ok(()); }

    let delete_nums: Vec<u32> = to_delete.iter().map(|g| g.number).collect();
    for g in &to_delete { profile::delete_profile(g)?; }
    gens_db.generations.retain(|g| !delete_nums.contains(&g.number));

    let referenced: std::collections::HashSet<String> = gens_db.generations.iter()
        .flat_map(|g| g.packages.iter()
            .map(|p| format!("{}-{}-{}", p.name, p.version, p.store_hash)))
        .collect();
    Store::gc_unreferenced(&referenced)?;
    gens_db.save()?;
    let _ = grub::update_grub(&gens_db);
    println!("  {} Garbage collection complete.", "✔".bright_green());
    Ok(())
}

fn cmd_clean() -> Result<()> {
    let removed = crate::download::clean_cache()?;
    println!("  {} Removed {} cached archive(s).", "✔".bright_green(), removed);
    Ok(())
}

fn cmd_init(args: &[String]) -> Result<()> {
    let user_mode = has_flag(args, "--user");
    let suite_arg = args.iter().find(|a| a.starts_with("--suite="))
        .and_then(|a| a.strip_prefix("--suite=").map(|s| s.to_string()));
    let suite = suite_arg.as_deref().unwrap_or("bookworm");

    if user_mode {
        let env      = UserEnv::current()?;
        env.init()?;
        let modified = userenv::install_shell_rc(&env)?;
        println!("  {} User hammer environment initialised.", "✔".bright_green());
        if !modified.is_empty() {
            for f in &modified { println!("       {}", f.as_str().dimmed()); }
            println!("       {}", "source ~/.hammer/env.sh".cyan());
        }
        return Ok(());
    }

    println!("  {} Initialising hammer {} (suite: {})…",
             "::".bold().cyan(), env!("CARGO_PKG_VERSION"), suite.bold());

    if !crate::repo::KNOWN_SUITES.contains(&suite) {
        bail!("Unknown suite '{}'. Supported: {}", suite, crate::repo::KNOWN_SUITES.join(", "));
    }

    for dir in &["/hammer/store", "/hammer/profiles", "/hammer/db", "/hammer/db/postinst",
                 "/etc/hammer", "/etc/hammer/HackerOS",
                 "/var/cache/hammer/archives", "/var/lib/hammer/lists",
                 "/usr/lib/hammer", "/usr/lib/HackerOS/hammer", "/var/log"] {
        std::fs::create_dir_all(dir)?;
        println!("  {} {}", "·".dimmed(), dir.dimmed());
    }

    let arch = detect_arch();
    let sources_path = std::path::Path::new(crate::repo::SOURCES_HK);
    if !sources_path.exists() {
        crate::repo::SourcesList::write_default(&arch, suite)?;
        println!("  {} Wrote {} ({})", "✔".green(), crate::repo::SOURCES_HK, suite.bold());
    }

    profile::install_activate_service()?;
    println!("  {} Installed hammer-activate.service", "✔".green());

    let gens_path = std::path::Path::new(profile::GENERATIONS_FILE);
    if !gens_path.exists() { GenerationsDb::default().save()?; }

    let gens_db = GenerationsDb::load()?;
    if let Err(e) = grub::update_grub(&gens_db) {
        println!("  {} GRUB: {} (non-fatal)", "·".yellow(), e.to_string().dimmed());
    } else {
        println!("  {} GRUB integration installed", "✔".green());
    }

    println!("\n  {} hammer {} initialised (suite: {}, arch: {}).",
             "✔".bright_green().bold(), env!("CARGO_PKG_VERSION"), suite.bold(), arch.bold());
    println!("  Next steps:");
    println!("    1. {} — add Debian archive key",
             "hammer key add https://ftp-master.debian.org/keys/archive-key-12.gpg".cyan());
    println!("    2. {} — refresh package index", "hammer sync".cyan());
    println!("    3. {} — install packages", "hammer install <pkg>".cyan());
    Ok(())
}

fn cmd_relink() -> Result<()> {
    let gens_db    = GenerationsDb::load()?;
    let active_num = read_active_gen().unwrap_or(gens_db.current);
    let gen = gens_db.get(active_num)
        .ok_or_else(|| anyhow::anyhow!("Active generation {} not found", active_num))?.clone();

    println!("  {} Relinking binaries for {}…",
             "⬡".bright_cyan().bold(), format!("gen-{}", active_num).bold());

    let profile_path = gen.profile_path();
    if !profile_path.exists() {
        anyhow::bail!("Profile path does not exist: {}\n  Try: hammer _activate",
                      profile_path.display());
    }

    match crate::profile::relink_bins(&profile_path) {
        Ok((linked, unlinked)) => {
            println!("  {} Linked: {}  Removed: {}",
                     "✔".bright_green().bold(),
                     linked.to_string().bold(),
                     unlinked.to_string().dimmed());
            if linked == 0 {
                println!("  {} No binaries to link.", "·".dimmed());
            } else {
                println!("  Packages are now available in PATH.");
                println!("  If shell still can't find them: {}", "hash -r".cyan());
            }
        }
        Err(e) => anyhow::bail!("relink failed: {}", e),
    }
    Ok(())
}

fn cmd_store() -> Result<()> {
    let store_bin = "/usr/share/hammer/store";
    if !std::path::Path::new(store_bin).exists() {
        bail!("Hammer Store not installed.\n  Expected: {}\n  Install: {}",
              store_bin.dimmed(), "hammer install hammer-store".cyan());
    }
    println!("  {} Launching Hammer Store…", "⬡".bright_cyan().bold());
    let status = std::process::Command::new(store_bin).status()?;
    if !status.success() { bail!("hammer-store exited: {}", status); }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  _activate  (boot-time, called by systemd)
// ─────────────────────────────────────────────────────────────

fn cmd_activate_internal() -> Result<()> {
    println!("hammer: boot activation starting…");
    if let Some(boot_gen) = grub::read_boot_gen() {
        println!("hammer: GRUB selected gen-{}", boot_gen);
        let gens_db = GenerationsDb::load()?;
        if let Some(gen) = gens_db.get(boot_gen) { profile::set_pending(gen)?; }
    }
    if let Some(pending_num) = read_pending_gen() {
        match crate::gpg::verify_boot_integrity(pending_num) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("hammer: BOOT INTEGRITY FAILED: {}", e);
                std::process::exit(1);
            }
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
    let yes = has_flag(args, "-y");
    let names: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    if names.is_empty() { bail!("Usage: hammer --user install <package...>"); }

    let arch   = flags.arch.as_ref()
        .map(|a| userenv::normalise_arch(a)).transpose()?
        .unwrap_or_else(detect_arch);
    let cache  = PackageCache::load_for_arch(&arch)?;
    let db_path = env.db_path.to_string_lossy().to_string();
    let db     = InstalledDb::open_at(&db_path)?;
    let solver = Solver::new(&cache, &db);
    let plan   = solver.resolve_install(&names, false)?;

    if plan.is_empty() { ui::nothing_to_do(); return Ok(()); }

    ui::print_transaction_table(&plan, &arch);
    if !yes && !ui::confirm("Install to user profile?")? { println!("  Aborted."); return Ok(()); }

    let client     = crate::download::HttpClient::new();
    let dl_results = crate::download::download_packages(&client, &plan.to_install).await?;
    let store      = userenv::UserStore::new(env);

    let mut store_entries = Vec::new();
    for dl in &dl_results {
        let deb_bytes = std::fs::read(&dl.path)?;
        let deb       = crate::deb::DebPackage::parse(&deb_bytes)?;
        let entry     = store.install_deb(&dl.package, &deb)?;
        store_entries.push(entry);
    }

    let mut gens_db    = crate::profile::GenerationsDb::load_from(&env.gens_file)?;
    let gen_num        = gens_db.next_number();
    let profile_path   = userenv::compose_user_profile(env, gen_num, &store_entries)?;

    let tmp = env.hammer_dir.join(".active.tmp");
    if tmp.symlink_metadata().is_ok() { std::fs::remove_file(&tmp)?; }
    std::os::unix::fs::symlink(&profile_path, &tmp)?;
    std::fs::rename(&tmp, &env.active_link)?;

    let note = format!("install {}", names.join(" "));
    let gen  = crate::profile::Generation {
        number:    gen_num,
        timestamp: chrono::Utc::now(),
        packages:  store_entries.iter().map(|e| crate::profile::GenPackage {
            name: e.name.clone(), version: e.version.clone(), store_hash: e.hash.clone(),
        }).collect(),
        note:  Some(note),
        state: Some(crate::profile::GenState::Active),
    };
    gens_db.generations.push(gen);
    gens_db.current = gen_num;
    gens_db.save_to(&env.gens_file)?;

    println!("  {} Installed {} package(s) to user profile.",
             "✔".bright_green(), store_entries.len().to_string().bold());
    println!("       {}", "source ~/.hammer/env.sh".cyan());
    Ok(())
}

fn cmd_user_remove(env: &UserEnv, _args: &[String]) -> Result<()> {
    println!("  {} hammer --user remove: not yet implemented", "·".yellow());
    println!("  User env: {}", env.hammer_dir.display());
    Ok(())
}

fn cmd_user_list(env: &UserEnv) -> Result<()> {
    println!("  {} User packages at {}", "⬡".cyan().bold(), env.hammer_dir.display());
    let gens_file = &env.gens_file;
    if !gens_file.exists() {
        println!("  {} No user packages installed.", "·".dimmed());
        println!("  Run: {}", "hammer init --user".cyan());
        return Ok(());
    }
    let gens_db = crate::profile::GenerationsDb::load_from(gens_file)?;
    if let Some(gen) = gens_db.current_gen() {
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
    if modified.is_empty() {
        println!("  {} Shell integration already installed.", "·".dimmed());
    } else {
        println!("  {} Shell integration added to:", "✔".bright_green());
        for f in &modified { println!("       {}", f.as_str().dimmed()); }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Help
// ─────────────────────────────────────────────────────────────

fn print_help() {
    println!();
    println!("  {} {}  {}",
             "⬡ hammer".bold().bright_cyan(),
             format!("v{}", env!("CARGO_PKG_VERSION")).dimmed(),
             "Apache-2.0".dimmed());
    println!("  Atomic Debian package manager — HackerOS");
    println!();
    println!("  {}", "Package management:".bold());
    println!("    {}  install <pkg...> [-y] [--arch=ARCH] [--no-recommends]", "install".cyan());
    println!("    {}   remove <pkg...> [-y]", "remove".cyan());
    println!("    {}  upgrade [pkg...] [-y]  (also updates .hk tools + checks self-update)", "upgrade".cyan());
    println!("    {}           remove unused auto-installed packages", "autoremove".cyan());
    println!("    {}               refresh package index", "sync".cyan());
    println!("    {}         update hammer itself", "self-update".cyan());
    println!();
    println!("  {}", "Pending changes (applied at next reboot):".bold());
    println!("    {}      show / cancel / apply-live", "pending".cyan());
    println!();
    println!("  {}", "Generations & diff:".bold());
    println!("    {}      list / switch <N>", "gen".cyan());
    println!("    {}              rollback to previous gen", "rollback".cyan());
    println!("    {}               diff current vs pending", "diff".cyan());
    println!();
    println!("  {}", "Export / import:".bold());
    println!("    {}      export current gen to archive", "export [file]".cyan());
    println!("    {}        import store from archive", "import <file>".cyan());
    println!();
    println!("  {}", "GPG keys:".bold());
    println!("    {}  list / add <url> / remove <fp>", "key".cyan());
    println!();
    println!("  {}", "Query / maintenance:".bold());
    println!("    {}       search <query> [--installed]", "search".cyan());
    println!("    {}         info <package>", "info".cyan());
    println!("    {}         list [--installed] [--upgrades]", "list".cyan());
    println!("    {}           show system status", "status".cyan());
    println!("    {}          show transaction history", "history".cyan());
    println!("    {}       gc [--keep N] [-y]", "gc".cyan());
    println!("    {}            clean download cache", "clean".cyan());
    println!("    {}     init [--suite=trixie] [--user]", "init".cyan());
    println!("    {}        relink binaries to PATH", "relink".cyan());
    println!();
    println!("  {}", "Graphical store:".bold());
    println!("    {}              open graphical package browser", "store".cyan());
    println!();
    println!("  {}", "User packages (--user):".bold());
    println!("    {} {}  install to ~/.hammer/", "--user".yellow(), "install <pkg>".cyan());
    println!();
    println!("  {}", "Sources list:".bold());
    println!("    Config: {}  (format: .hk)", SOURCES_HK.cyan());
    println!("    Tools:  {}", "/etc/hammer/HackerOS/*.hk".cyan());
    println!();
}

// ─────────────────────────────────────────────────────────────
//  Utility
// ─────────────────────────────────────────────────────────────

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}
