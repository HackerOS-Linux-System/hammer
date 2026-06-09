use anyhow::{bail, Result};
use owo_colors::OwoColorize;

use crate::cache::{detect_arch, PackageCache};
use crate::cli_types::{GlobalFlags, has_flag};
use crate::db::InstalledDb;
use crate::grub;
use crate::hk_tools;
use crate::lock;
use crate::profile::GenerationsDb;
use crate::selfupdate;
use crate::solver::Solver;
use crate::transaction::{execute_transaction, TransactionContext};
use crate::ui;
use crate::userenv::{self, UserEnv};

// ─────────────────────────────────────────────────────────────
//  User mode router
// ─────────────────────────────────────────────────────────────

pub async fn run_user(cmd: &str, args: &[String], flags: &GlobalFlags) -> Result<()> {
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

pub async fn cmd_install(args: &[String], flags: &GlobalFlags) -> Result<()> {
    let yes       = has_flag(args, "-y") || has_flag(args, "--yes") || flags.yes;
    let no_recomm = has_flag(args, "--no-recommends");
    let names: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    if names.is_empty() {
        bail!("Usage: hammer install <package...> [-y] [--arch=ARCH] [--no-recommends]");
    }

    let arch  = flags.arch.as_ref()
    .map(|a| userenv::normalise_arch(a)).transpose()?
    .unwrap_or_else(detect_arch);
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
    if let Ok(gdb) = GenerationsDb::load() { let _ = grub::update_grub(&gdb); }
    ui::print_pending_notice(gen_num);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  reinstall
// ─────────────────────────────────────────────────────────────

pub async fn cmd_reinstall(args: &[String]) -> Result<()> {
    let yes   = has_flag(args, "-y");
    let names: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    if names.is_empty() { bail!("Usage: hammer reinstall <package...> [-y]"); }

    let _lock = lock::system_lock()?;
    ui::print_header();
    let db    = InstalledDb::open()?;
    let cache = PackageCache::load_for_arch(&detect_arch())?;
    let plan  = Solver::new(&cache, &db).resolve_reinstall(&names)?;
    if plan.is_empty() { ui::nothing_to_do(); return Ok(()); }

    ui::print_transaction_table(&plan, &detect_arch());
    ui::print_transaction_summary(&plan);
    if !yes && !ui::confirm("Proceed with reinstallation?")? { println!("  Aborted."); return Ok(()); }

    let note    = format!("reinstall {}", names.join(" "));
    let ctx     = TransactionContext::system(&plan, &db, &names, false);
    let gen_num = execute_transaction(ctx, &note).await?;
    if let Ok(gdb) = GenerationsDb::load() { let _ = grub::update_grub(&gdb); }
    ui::print_pending_notice(gen_num);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  remove
// ─────────────────────────────────────────────────────────────

pub async fn cmd_remove(args: &[String], flags: &GlobalFlags) -> Result<()> {
    let yes   = has_flag(args, "-y") || flags.yes;
    let names: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    if names.is_empty() { bail!("Usage: hammer remove <package...> [-y]"); }

    let _lock = lock::system_lock()?;
    ui::print_header();
    let db    = InstalledDb::open()?;
    let cache = PackageCache::load()?;
    let plan  = Solver::new(&cache, &db).resolve_remove(&names)?;
    if plan.is_empty() { ui::nothing_to_do(); return Ok(()); }

    for w in &plan.warnings { println!("  {} {}", "warn:".yellow().bold(), w.yellow()); }
    ui::print_transaction_table(&plan, &detect_arch());
    ui::print_transaction_summary(&plan);
    if !yes && !ui::confirm("Proceed with removal?")? { println!("  Aborted."); return Ok(()); }

    let note    = format!("remove {}", names.join(" "));
    let ctx     = TransactionContext::system(&plan, &db, &names, false);
    let gen_num = execute_transaction(ctx, &note).await?;
    if let Ok(gdb) = GenerationsDb::load() { let _ = grub::update_grub(&gdb); }
    ui::print_pending_notice(gen_num);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  upgrade
// ─────────────────────────────────────────────────────────────

pub async fn cmd_upgrade(args: &[String], flags: &GlobalFlags) -> Result<()> {
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
        let db    = InstalledDb::open()?;
        let cache = PackageCache::load_for_arch(&arch)?;
        let plan  = if pkgs.is_empty() { Solver::new(&cache, &db).resolve_upgrade()? }
        else               { Solver::new(&cache, &db).resolve_install(&pkgs, false)? };

        if !plan.is_empty() {
            ui::deps_resolved();
            ui::print_transaction_table(&plan, &arch);
            ui::print_transaction_summary(&plan);
            if !yes && !ui::confirm("Proceed with upgrade?")? { println!("  Aborted."); return Ok(()); }
            let explicit: Vec<String> = plan.to_upgrade.iter().map(|p| p.name.clone()).collect();
            let ctx     = TransactionContext::system(&plan, &db, &explicit, true);
            let gen_num = execute_transaction(ctx, "upgrade").await?;
            if let Ok(gdb) = GenerationsDb::load() { let _ = grub::update_grub(&gdb); }
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

pub async fn cmd_dist_upgrade(args: &[String], _flags: &GlobalFlags) -> Result<()> {
    let yes   = has_flag(args, "-y");
    let _lock = lock::system_lock()?;
    ui::print_header();
    println!("  {}  dist-upgrade — aggressive upgrade", "⬡".bright_cyan().bold());
    let db    = InstalledDb::open()?;
    let cache = PackageCache::load()?;
    let plan  = Solver::new(&cache, &db).resolve_dist_upgrade()?;
    if plan.is_empty() { println!("  {} System is up to date.", "✔".bright_green()); return Ok(()); }
    for w in &plan.warnings { println!("  {} {}", "!".yellow().bold(), w.yellow()); }
    ui::print_transaction_table(&plan, &detect_arch());
    ui::print_transaction_summary(&plan);
    if !yes && !ui::confirm("Proceed with dist-upgrade?")? { println!("  Aborted."); return Ok(()); }
    let ctx     = TransactionContext::system(&plan, &db, &[], true);
    let gen_num = execute_transaction(ctx, "dist-upgrade").await?;
    if let Ok(gdb) = GenerationsDb::load() { let _ = grub::update_grub(&gdb); }
    ui::print_pending_notice(gen_num);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  autoremove / fix-broken
// ─────────────────────────────────────────────────────────────

pub async fn cmd_autoremove(args: &[String]) -> Result<()> {
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
    if let Ok(gdb) = GenerationsDb::load() { let _ = grub::update_grub(&gdb); }
    ui::print_pending_notice(gen_num);
    Ok(())
}

pub async fn cmd_fix_broken(args: &[String]) -> Result<()> {
    let yes   = has_flag(args, "-y");
    let _lock = lock::system_lock()?;
    ui::print_header();
    println!("  {}  Checking for broken dependencies…", "⬡".bright_cyan().bold());
    let db    = InstalledDb::open()?;
    let cache = PackageCache::load()?;
    let plan  = Solver::new(&cache, &db).resolve_fix_broken()?;
    for w in &plan.warnings { println!("  {} {}", "·".dimmed(), w.yellow()); }
    if plan.is_empty() { println!("  {} No broken dependencies.", "✔".bright_green()); return Ok(()); }
    ui::print_transaction_table(&plan, &detect_arch());
    ui::print_transaction_summary(&plan);
    if !yes && !ui::confirm("Fix broken dependencies?")? { println!("  Aborted."); return Ok(()); }
    let ctx     = TransactionContext::system(&plan, &db, &[], false);
    let gen_num = execute_transaction(ctx, "fix-broken").await?;
    if let Ok(gdb) = GenerationsDb::load() { let _ = grub::update_grub(&gdb); }
    ui::print_pending_notice(gen_num);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  sync / self-update
// ─────────────────────────────────────────────────────────────

pub async fn cmd_sync() -> Result<()> {
    ui::print_header();
    crate::cache::sync_all().await?;
    println!("  {} Package index updated.", "✔".bright_green());
    Ok(())
}

pub async fn cmd_self_update() -> Result<()> {
    ui::print_header();
    selfupdate::self_update(&crate::download::HttpClient::new()).await
}

// ─────────────────────────────────────────────────────────────
//  search / info / list
// ─────────────────────────────────────────────────────────────

pub fn cmd_search(args: &[String], flags: &GlobalFlags) -> Result<()> {
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

pub fn cmd_info(args: &[String], flags: &GlobalFlags) -> Result<()> {
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

pub fn cmd_list(args: &[String]) -> Result<()> {
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
//  User-mode commands
// ─────────────────────────────────────────────────────────────

pub async fn cmd_user_install(env: &UserEnv, args: &[String], flags: &GlobalFlags) -> Result<()> {
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
    let tmp          = env.hammer_dir.join(".active.tmp");
    if tmp.symlink_metadata().is_ok() { std::fs::remove_file(&tmp)?; }
    std::os::unix::fs::symlink(&profile_path, &tmp)?;
    std::fs::rename(&tmp, &env.active_link)?;
    let gen = crate::profile::Generation {
        number:    gen_num, timestamp: chrono::Utc::now(),
        packages:  entries.iter().map(|e| crate::profile::GenPackage {
            name: e.name.clone(), version: e.version.clone(), store_hash: e.hash.clone(),
        }).collect(),
        note:  Some(format!("install {}", names.join(" "))),
        state: Some(crate::profile::GenState::Active),
    };
    gdb.generations.push(gen); gdb.current = gen_num; gdb.save_to(&env.gens_file)?;
    println!("  {} Installed {} package(s) to user profile.", "✔".bright_green(), entries.len().to_string().bold());
    Ok(())
}

pub async fn cmd_user_remove(env: &UserEnv, args: &[String]) -> Result<()> {
    let yes   = has_flag(args, "-y");
    let names: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    if names.is_empty() { bail!("Usage: hammer --user remove <package...> [-y]"); }
    let _lock  = lock::user_lock(&env.hammer_dir)?;
    let db_str = env.db_path.to_string_lossy().to_string();
    let db     = InstalledDb::open_at(&db_str)?;
    let not_found: Vec<_> = names.iter().filter(|n| db.get(n).is_none()).cloned().collect();
    if !not_found.is_empty() { bail!("Not installed in user profile: {}", not_found.join(", ")); }
    println!("  {} Removing: {}", "⬡".cyan().bold(), names.join(", ").bold());
    if !yes && !ui::confirm("Proceed?")? { println!("  Aborted."); return Ok(()); }
    let mut gdb = crate::profile::GenerationsDb::load_from(&env.gens_file)?;
    for name in &names {
        if let Some(inst) = db.get(name) { db.record_remove(name, &inst.version, gdb.current)?; }
    }
    let remaining: Vec<crate::store::StoreEntry> = db.list_all()?.iter()
    .filter(|p| !names.contains(&p.name))
    .filter_map(|p| {
        let path = std::path::PathBuf::from(crate::store::STORE_DIR)
        .join(format!("{}-{}-{}", p.name, p.version, p.store_hash));
        if path.exists() {
            Some(crate::store::StoreEntry {
                name: p.name.clone(), version: p.version.clone(), hash: p.store_hash.clone(), path,
            })
        } else { None }
    }).collect();
    let gen_num = gdb.next_number();
    let profile = userenv::compose_user_profile(env, gen_num, &remaining)?;
    let tmp     = env.hammer_dir.join(".active.tmp");
    if tmp.symlink_metadata().is_ok() { std::fs::remove_file(&tmp)?; }
    std::os::unix::fs::symlink(&profile, &tmp)?;
    std::fs::rename(&tmp, &env.active_link)?;
    let gen = crate::profile::Generation {
        number: gen_num, timestamp: chrono::Utc::now(),
        packages: remaining.iter().map(|e| crate::profile::GenPackage {
            name: e.name.clone(), version: e.version.clone(), store_hash: e.hash.clone(),
        }).collect(),
        note: Some(format!("remove {}", names.join(" "))),
        state: Some(crate::profile::GenState::Active),
    };
    gdb.generations.push(gen); gdb.current = gen_num; gdb.save_to(&env.gens_file)?;
    // Remove wrappers from hammer_dir/bin/
    let bin_dir = env.hammer_dir.join("bin");
    for name in &names {
        let w = bin_dir.join(name);
        if w.symlink_metadata().is_ok() { std::fs::remove_file(&w).ok(); }
    }
    println!("  {} Removed {} package(s) from user profile.", "✔".bright_green(), names.len().to_string().bold());
    Ok(())
}

pub fn cmd_user_list(env: &UserEnv) -> Result<()> {
    println!("  {} User packages at {}", "⬡".cyan().bold(), env.hammer_dir.display());
    if !env.gens_file.exists() {
        println!("  {} No user packages. Run: {}", "·".dimmed(), "hammer init --user".cyan());
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

pub fn cmd_user_status(env: &UserEnv) -> Result<()> {
    println!("  {}", "User hammer environment".bold());
    println!("  {:<26} {}", "Location:".bold(), env.hammer_dir.display());
    let active = env.active_link.exists() || env.active_link.symlink_metadata().is_ok();
    // FIX: separate String branches
    let active_str = if active { "yes".bright_green().to_string() }
    else { "no — run hammer init --user".yellow().to_string() };
    println!("  {:<26} {}", "Initialised:".bold(), active_str);
    Ok(())
}

pub fn cmd_user_shell_init(env: &UserEnv) -> Result<()> {
    let modified = userenv::install_shell_rc(env)?;
    if modified.is_empty() { println!("  {} Shell integration already installed.", "·".dimmed()); }
    else { for f in &modified { println!("       {}", f.as_str().dimmed()); } }
    Ok(())
}
