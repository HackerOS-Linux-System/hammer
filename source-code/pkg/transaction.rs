use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use std::path::{Path, PathBuf};

use crate::audit;
use crate::cache::detect_arch;
use crate::conffiles::ConffileDb;
use crate::db::{InstalledDb, InstallReason};
use crate::deb::DebPackage;
use crate::download::{download_packages, HttpClient};
use crate::essential;
use crate::log;
use crate::package::Package;
use crate::profile::{self, compose_profile, GenerationsDb};
use crate::sandbox::PostinstSandbox;
use crate::solver::TransactionPlan;
use crate::store::Store;

// ─────────────────────────────────────────────────────────────
//  TransactionContext
// ─────────────────────────────────────────────────────────────

pub struct TransactionContext<'a> {
    pub plan:            &'a TransactionPlan,
    pub db:              &'a InstalledDb,
    pub explicit:        &'a [String],
    pub is_upgrade:      bool,
    /// Pass --force-essential to allow removing essential packages
    pub force_essential: bool,
}

impl<'a> TransactionContext<'a> {
    pub fn system(
        plan:       &'a TransactionPlan,
        db:         &'a InstalledDb,
        explicit:   &'a [String],
        is_upgrade: bool,
    ) -> Self {
        TransactionContext { plan, db, explicit, is_upgrade, force_essential: false }
    }
}

// ─────────────────────────────────────────────────────────────
//  execute_transaction
// ─────────────────────────────────────────────────────────────

pub async fn execute_transaction(ctx: TransactionContext<'_>, note: &str) -> Result<u32> {
    let client    = HttpClient::new();
    let plan      = ctx.plan;
    let db        = ctx.db;
    let _sys_arch = detect_arch();

    // ── Guard: check essential packages before removal ────────
    if !plan.to_remove.is_empty() {
        let cache = crate::cache::PackageCache::load()?;
        essential::guard_essential_removal(
            &plan.to_remove,
            ctx.force_essential,
            &cache,
        ).context("Essential package protection")?;
    }

    // ── Build download list ───────────────────────────────────
    let mut to_install: Vec<Package> = plan.to_install.clone();
    to_install.extend(plan.to_upgrade.clone());

    // ── Download ──────────────────────────────────────────────
    if !to_install.is_empty() {
        println!("  {}  Downloading {} package{}…",
                 "⬡".bright_cyan().bold(),
                 to_install.len(),
                 if to_install.len() == 1 { "" } else { "s" });
    }

    let downloads = if !to_install.is_empty() {
        download_packages(&client, &to_install).await
        .context("Download failed")?
    } else {
        vec![]
    };

    // ── Unpack into store ─────────────────────────────────────
    println!("  {}  Unpacking packages…", "⬡".bright_cyan().bold());

    let mut store_entries = Vec::new();
    let sandbox = PostinstSandbox::new();

    for dl in &downloads {
        let deb_bytes = std::fs::read(&dl.path)
        .with_context(|| format!("Reading {}", dl.path.display()))?;

        let deb = DebPackage::parse(&deb_bytes)
        .with_context(|| format!("Parsing .deb for {}", dl.package.name))?;

        // Save postinst script for boot-time execution (legacy path)
        // and run it sandboxed if available
        if let Some(ref script) = deb.postinst {
            if let Err(e) = save_postinst_script(&dl.package.name, script) {
                log::warn(&format!(
                    "transaction: could not save postinst for {}: {}", dl.package.name, e
                ));
            }
        }

        // Extract to store — returns conffiles alongside regular files
        let entry = Store::install_deb(&dl.package, &deb)
        .with_context(|| format!("Installing {} to store", dl.package.name))?;

        // Record conffiles (three-way merge support on upgrade)
        // We need to extract to a temp dir to get original conffile content
        let tmp = std::env::temp_dir().join(format!(
            "hammer_conffiles_{}_{}", dl.package.name, std::process::id()
        ));
        if std::fs::create_dir_all(&tmp).is_ok() {
            if let Ok(result) = deb.extract_data(&tmp) {
                if !result.conffiles.is_empty() {
                    if let Err(e) = ConffileDb::record(&dl.package.name, &result.conffiles) {
                        log::warn(&format!(
                            "transaction: conffiles record failed for {}: {}", dl.package.name, e
                        ));
                    }
                }
            }
            let _ = std::fs::remove_dir_all(&tmp);
        }

        store_entries.push(entry);
        println!("  {} {} {}",
                 "·".dimmed(), dl.package.name.bold(), dl.package.version.dimmed());
    }

    // ── Handle removals ───────────────────────────────────────
    let mut gdb = GenerationsDb::load()?;

    if !plan.to_remove.is_empty() {
        for name in &plan.to_remove {
            if let Some(inst) = db.get(name) {
                db.record_remove(name, &inst.version, gdb.current)
                .with_context(|| format!("Recording removal of {}", name))?;
            }
        }
        // Audit: record removal
        audit::record_remove(&plan.to_remove, Some(gdb.current), None, db);
    }

    // ── Record installs in DB ─────────────────────────────────
    let gen_num = gdb.next_number();

    for entry in &store_entries {
        let pkg = to_install.iter().find(|p| p.name == entry.name)
        .ok_or_else(|| anyhow::anyhow!(
            "Internal: package {} not in install list", entry.name
        ))?;

        let reason = if ctx.explicit.contains(&entry.name) {
            InstallReason::User
        } else {
            InstallReason::Dependency
        };

        db.record_install(pkg, reason, &entry.hash, gen_num)
        .with_context(|| format!("Recording install of {}", entry.name))?;
    }

    // ── Audit: record install/upgrade ─────────────────────────
    let upgrades: Vec<&Package> = to_install.iter()
    .filter(|p| plan.upgrade_from.contains_key(&p.name))
    .collect();
    let installs: Vec<&Package> = to_install.iter()
    .filter(|p| !plan.upgrade_from.contains_key(&p.name))
    .collect();

    if !installs.is_empty() {
        let pkgs: Vec<Package> = installs.into_iter().cloned().collect();
        audit::record_install(&pkgs, Some(gdb.current), Some(gen_num));
    }
    if !upgrades.is_empty() {
        let pkgs: Vec<Package> = upgrades.into_iter().cloned().collect();
        audit::record_upgrade(&pkgs, &plan.upgrade_from, Some(gdb.current), Some(gen_num));
    }

    // ── Include existing installed packages in new profile ────
    let existing_entries: Vec<crate::store::StoreEntry> = db.list_all()
    .unwrap_or_default()
    .iter()
    .filter(|p| !to_install.iter().any(|i| i.name == p.name))
    .filter(|p| !plan.to_remove.contains(&p.name))
    .filter_map(|p| {
        let path = PathBuf::from(crate::store::STORE_DIR)
        .join(format!("{}-{}-{}", p.name, p.version, p.store_hash));
        if path.exists() {
            Some(crate::store::StoreEntry {
                name:    p.name.clone(),
                 version: p.version.clone(),
                 hash:    p.store_hash.clone(),
                 path,
            })
        } else { None }
    })
    .collect();

    let mut all_entries = existing_entries;
    all_entries.extend(store_entries);

    // ── Compose new generation profile ───────────────────────
    println!("  {}  Composing gen-{}…", "⬡".bright_cyan().bold(), gen_num);
    let gen = compose_profile(gen_num, &all_entries, Some(note.to_string()))
    .context("Composing generation profile")?;

    // ── Record generation ─────────────────────────────────────
    gdb.generations.push(gen.clone());
    gdb.pending = Some(gen_num);
    gdb.save()?;

    // ── Set pending symlink ───────────────────────────────────
    profile::set_pending(&gen)?;

    // ── Run sandboxed postinst scripts (live installs) ────────
    // Only for packages that are new (not just upgraded) and have postinst
    for dl in &downloads {
        let is_new = !plan.upgrade_from.contains_key(&dl.package.name);
        if !is_new { continue; }

        let script_path = Path::new("/hammer/db/postinst")
        .join(format!("{}.postinst", dl.package.name));
        if !script_path.exists() { continue; }

        if let Ok(script) = std::fs::read_to_string(&script_path) {
            log::info(&format!(
                "transaction: running postinst for {} via sandbox",
                dl.package.name
            ));
            match sandbox.run_postinst(&dl.package.name, &script) {
                Ok(result) if result.success => {
                    log::info(&format!(
                        "transaction: postinst {} OK", dl.package.name
                    ));
                }
                Ok(result) => {
                    log::warn(&format!(
                        "transaction: postinst {} failed (exit {}): {}",
                                       dl.package.name, result.exit_code,
                                       result.stderr.lines().next().unwrap_or("")
                    ));
                    println!("  {} postinst {} failed — system may need manual config",
                             "!".yellow().bold(), dl.package.name.bold());
                }
                Err(e) => {
                    log::warn(&format!(
                        "transaction: could not run postinst {}: {}", dl.package.name, e
                    ));
                }
            }
        }
    }

    // ── Update GRUB ───────────────────────────────────────────
    if let Err(e) = crate::grub::update_grub(&gdb) {
        log::warn(&format!("transaction: GRUB update failed: {}", e));
    }

    println!("  {}  Staged as gen-{} — reboot to activate",
             "✔".bright_green().bold(), gen_num);
    log::info(&format!("transaction: gen-{} staged — {}", gen_num, note));

    Ok(gen_num)
}

// ─────────────────────────────────────────────────────────────
//  Save postinst script (legacy path for boot-time activation)
// ─────────────────────────────────────────────────────────────

fn save_postinst_script(pkg_name: &str, script: &str) -> Result<PathBuf> {
    let dir  = Path::new("/hammer/db/postinst");
    std::fs::create_dir_all(dir)?;
    let dest = dir.join(format!("{}.postinst", pkg_name));
    std::fs::write(&dest, script)?;
    Ok(dest)
}
