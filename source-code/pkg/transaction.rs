use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use std::path::{Path, PathBuf};

use crate::cache::detect_arch;
use crate::db::{InstalledDb, InstallReason};
use crate::deb::DebPackage;
use crate::download::{download_packages, HttpClient};
use crate::log;
use crate::package::Package;
use crate::profile::{self, compose_profile, GenerationsDb};
use crate::solver::TransactionPlan;
use crate::store::Store;

// ─────────────────────────────────────────────────────────────
//  TransactionContext
// ─────────────────────────────────────────────────────────────

pub struct TransactionContext<'a> {
    pub plan:       &'a TransactionPlan,
    pub db:         &'a InstalledDb,
    pub explicit:   &'a [String],
    pub is_upgrade: bool,
}

impl<'a> TransactionContext<'a> {
    pub fn system(
        plan:       &'a TransactionPlan,
        db:         &'a InstalledDb,
        explicit:   &'a [String],
        is_upgrade: bool,
    ) -> Self {
        TransactionContext { plan, db, explicit, is_upgrade }
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

    // 1. Build list of packages to download
    let mut to_install: Vec<Package> = plan.to_install.clone();
    to_install.extend(plan.to_upgrade.clone());

    // 2. Download .deb files
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

    // 3. Unpack into store
    println!("  {}  Unpacking packages…", "⬡".bright_cyan().bold());

    let mut store_entries = Vec::new();

    for dl in &downloads {
        let deb_bytes = std::fs::read(&dl.path)
        .with_context(|| format!("Reading {}", dl.path.display()))?;

        let deb = DebPackage::parse(&deb_bytes)
        .with_context(|| format!("Parsing .deb for {}", dl.package.name))?;

        // FIX: deb.postinst is now a field (Option<String>), not a method.
        // Save postinst script for boot-time execution.
        if let Some(ref script) = deb.postinst {
            if let Err(e) = save_postinst_script(&dl.package.name, script) {
                log::warn(&format!("transaction: could not save postinst for {}: {}", dl.package.name, e));
            } else {
                log::info(&format!("transaction: saved postinst for {}", dl.package.name));
            }
        }

        let entry = Store::install_deb(&dl.package, &deb)
        .with_context(|| format!("Installing {} to store", dl.package.name))?;

        store_entries.push(entry);
        println!("  {} {} {}", "·".dimmed(), dl.package.name.bold(), dl.package.version.dimmed());
    }

    // 4. Handle removals
    if !plan.to_remove.is_empty() {
        let gdb = GenerationsDb::load()?;
        for name in &plan.to_remove {
            if let Some(inst) = db.get(name) {
                db.record_remove(name, &inst.version, gdb.current)
                .with_context(|| format!("Recording removal of {}", name))?;
            }
        }
    }

    // 5. Record installs in DB
    let mut gdb  = GenerationsDb::load()?;
    let gen_num  = gdb.next_number();

    for entry in &store_entries {
        let pkg = to_install.iter().find(|p| p.name == entry.name)
        .ok_or_else(|| anyhow::anyhow!("Internal: package {} not in install list", entry.name))?;

        let reason = if ctx.explicit.contains(&entry.name) {
            InstallReason::User
        } else {
            InstallReason::Dependency
        };

        db.record_install(pkg, reason, &entry.hash, gen_num)
        .with_context(|| format!("Recording install of {}", entry.name))?;
    }

    // 6. Include existing installed packages in profile
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

    // 7. Compose new generation profile
    println!("  {}  Composing gen-{}…", "⬡".bright_cyan().bold(), gen_num);
    let gen = compose_profile(gen_num, &all_entries, Some(note.to_string()))
    .context("Composing generation profile")?;

    // 8. Record generation
    gdb.generations.push(gen.clone());
    gdb.pending = Some(gen_num);
    gdb.save()?;

    // 9. Set pending symlink
    profile::set_pending(&gen)?;

    // 10. Update GRUB
    if let Err(e) = crate::grub::update_grub(&gdb) {
        log::warn(&format!("transaction: GRUB update failed: {}", e));
    }

    println!("  {}  Staged as gen-{} — reboot to activate",
             "✔".bright_green().bold(), gen_num);
    log::info(&format!("transaction: gen-{} staged — {}", gen_num, note));

    Ok(gen_num)
}

// ─────────────────────────────────────────────────────────────
//  Save postinst script
// ─────────────────────────────────────────────────────────────

fn save_postinst_script(pkg_name: &str, script: &str) -> Result<PathBuf> {
    let dir  = Path::new("/hammer/db/postinst");
    std::fs::create_dir_all(dir)?;
    let dest = dir.join(format!("{}.postinst", pkg_name));
    std::fs::write(&dest, script)?;
    Ok(dest)
}
