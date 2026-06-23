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
        let entry = crate::store::install_deb_pkg(&dl.package, &deb)
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
                    backend: crate::store::StoreBackend::Hardlink,
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

// ─────────────────────────────────────────────────────────────
//  Normal-mode transaction (no atomic store, no generations)
//  cargo build --release --features normal-mode
// ─────────────────────────────────────────────────────────────

#[cfg(feature = "normal-mode")]
pub async fn execute_transaction_normal(ctx: TransactionContext<'_>, note: &str) -> Result<()> {
    use crate::build_mode::staging_dir;

    let plan   = ctx.plan;
    let db     = ctx.db;
    let client = crate::download::HttpClient::new();

    if plan.is_empty() {
        println!("  {} Nothing to do.", "·".dimmed());
        return Ok(());
    }

    plan.print_summary(false);

    // ── Dry-run: stop here ────────────────────────────────────
    // (caller checks flags.dry_run before calling, but guard here too)

    // ── Pre-transaction hook ──────────────────────────────────
    run_hook("pre-transaction", note).await;

    // ── Download ──────────────────────────────────────────────
    let mut to_install = plan.to_install.clone();
    to_install.extend(plan.to_upgrade.clone());

    if !to_install.is_empty() {
        println!("  {}  Downloading {} package(s)…", "⬡".cyan().bold(), to_install.len());
        let downloads = crate::download::download_packages(&client, &to_install)
            .await.context("Download")?;

        // ── Verify signatures ─────────────────────────────────
        for (pkg, path) in &downloads {
            if let Err(e) = crate::audit::verify_package_signature(path) {
                eprintln!("  {} Signature verification failed for {}: {}", "!".red().bold(), pkg.name, e);
                anyhow::bail!("Aborting: signature verification failed for '{}'", pkg.name);
            }
        }

        // ── Install directly to / ─────────────────────────────
        let staging = staging_dir();
        std::fs::create_dir_all(&staging)?;

        let sandbox = crate::sandbox::PostinstSandbox::new();
        for (pkg, deb_path) in downloads {
            println!("  {} Unpacking {}…", "↓".cyan(), pkg.name.bold());
            let deb  = DebPackage::read(&deb_path).context("Read .deb")?;

            // prerm for upgrades
            if db.is_installed(&pkg.name) {
                if let Some(prerm) = deb.prerm() {
                    let _ = crate::sandbox::run_prerm_script(&sandbox, &pkg.name, prerm);
                }
            }

            // Unpack to /
            deb.unpack(Path::new("/")).context("Unpack")?;
            log::pkg("install-normal", &pkg.name, &pkg.version);

            // postinst
            if let Some(postinst) = deb.postinst() {
                let result = sandbox.run_postinst(&pkg.name, postinst)?;
                if result.exit_code != 0 {
                    log::warn(&format!("postinst {} exit {}", pkg.name, result.exit_code));
                }
            }

            // Update conffiles
            let mut confdb = ConffileDb::open()?;
            confdb.register_package_conffiles(&pkg.name, &deb)?;

            // Record in DB
            let entry = crate::db::InstalledPackage {
                name:              pkg.name.clone(),
                version:           pkg.version.clone(),
                architecture:      pkg.architecture.clone().unwrap_or_else(|| "amd64".into()),
                installed_size_kb: pkg.installed_size_kb.unwrap_or(0),
                section:           pkg.section.clone(),
                maintainer:        pkg.maintainer.clone(),
                description_short: pkg.description.as_ref()
                                      .and_then(|d| d.lines().next().map(|l| l.to_string())),
                installed_at:      chrono::Utc::now(),
                reason:            if ctx.explicit.contains(&pkg.name) {
                                       crate::db::InstallReason::User
                                   } else {
                                       crate::db::InstallReason::Dependency
                                   },
                store_hash:        String::new(),  // no store in normal mode
                depends:           pkg.depends.clone(),
                recommends:        pkg.recommends.clone(),
            };
            db.upsert(&entry)?;
        }
    }

    // ── Removals ──────────────────────────────────────────────
    for name in plan.to_remove.iter().chain(plan.to_autoremove.iter()) {
        println!("  {} Removing {}…", "✘".red(), name.bold());
        let pkg_root = Path::new("/");

        // prerm
        if let Some(script_path) = find_maintainer_script(name, "prerm") {
            let sandbox = crate::sandbox::PostinstSandbox::new();
            let _ = crate::sandbox::run_prerm_script(
                &sandbox, name,
                &std::fs::read_to_string(&script_path).unwrap_or_default()
            );
        }

        // Remove files listed in dpkg info
        remove_package_files(name, pkg_root)?;

        // postrm
        if let Some(script_path) = find_maintainer_script(name, "postrm") {
            let sandbox = crate::sandbox::PostinstSandbox::new();
            let _ = crate::sandbox::run_postrm_script(
                &sandbox, name,
                &std::fs::read_to_string(&script_path).unwrap_or_default(),
                "remove"
            );
        }

        db.remove(name)?;
        log::pkg("remove-normal", name, "");
    }

    // ── Post-transaction hook ─────────────────────────────────
    run_hook("post-transaction", note).await;

    println!("  {} Transaction complete.", "✔".bright_green().bold());
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Pre/Post transaction hooks
// ─────────────────────────────────────────────────────────────

async fn run_hook(hook: &str, note: &str) {
    let hook_path = format!("/etc/hammer/hooks.d/{}", hook);
    let path = Path::new(&hook_path);
    if !path.exists() { return; }

    log::info(&format!("hook: running {} ({})", hook, note));
    let _ = tokio::process::Command::new("/bin/sh")
        .arg(&hook_path)
        .env("HAMMER_TRANSACTION", note)
        .status()
        .await;
}

// ─────────────────────────────────────────────────────────────
//  Partial-install rollback
// ─────────────────────────────────────────────────────────────

pub struct RollbackGuard {
    /// Packages that were successfully installed before a failure
    installed: Vec<String>,
    db:        std::sync::Arc<InstalledDb>,
    activated: bool,
}

impl RollbackGuard {
    pub fn new(db: std::sync::Arc<InstalledDb>) -> Self {
        RollbackGuard { installed: Vec::new(), db, activated: false }
    }
    pub fn record(&mut self, name: &str) { self.installed.push(name.to_string()); }
    pub fn commit(mut self)               { self.activated = true; }
}

impl Drop for RollbackGuard {
    fn drop(&mut self) {
        if !self.activated && !self.installed.is_empty() {
            log::warn(&format!(
                "transaction: rolling back partial install: {}",
                self.installed.join(", ")
            ));
            for name in &self.installed {
                let _ = crate::db::InstalledDb::open().and_then(|db| db.remove(name));
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  Helpers
// ─────────────────────────────────────────────────────────────

fn find_maintainer_script(pkg: &str, kind: &str) -> Option<std::path::PathBuf> {
    let path = std::path::PathBuf::from(format!("/var/lib/dpkg/info/{}.{}", pkg, kind));
    if path.exists() { Some(path) } else { None }
}

fn remove_package_files(pkg: &str, root: &Path) -> Result<()> {
    let list_path = format!("/var/lib/dpkg/info/{}.list", pkg);
    let list = match std::fs::read_to_string(&list_path) {
        Ok(l) => l,
        Err(_) => {
            log::warn(&format!("remove: no dpkg file list for {}", pkg));
            return Ok(());
        }
    };
    let mut paths: Vec<&str> = list.lines().collect();
    // Remove files before directories, and deeper paths first
    paths.sort_by(|a, b| b.len().cmp(&a.len()));
    for rel in paths {
        let full = root.join(rel.trim_start_matches('/'));
        if full.is_file() || full.is_symlink() {
            let _ = std::fs::remove_file(&full);
            log::file_op("remove", &full.to_string_lossy());
        } else if full.is_dir() {
            let _ = std::fs::remove_dir(&full); // only removes empty dirs
        }
    }
    Ok(())
}
