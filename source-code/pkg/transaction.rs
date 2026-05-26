use anyhow::{Context, Result};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use std::path::PathBuf;
use std::time::Duration;

use crate::db::{InstalledDb, InstallReason};
use crate::deb::DebPackage;
use crate::download::{self, HttpClient, UnpackSpinner};
use crate::log;
use crate::package::Package;
use crate::profile::{self, GenerationsDb, remove_postinst};
use crate::solver::TransactionPlan;
use crate::store::{Store, StoreEntry};

// ─────────────────────────────────────────────────────────────
//  TransactionContext
// ─────────────────────────────────────────────────────────────

pub struct TransactionContext<'a> {
    pub plan:           &'a TransactionPlan,
    pub db:             &'a InstalledDb,
    pub explicit:       &'a [String],
    pub is_upgrade:     bool,
    pub user_mode:      bool,
    pub store_override: Option<PathBuf>,
}

impl<'a> TransactionContext<'a> {
    pub fn system(
        plan:       &'a TransactionPlan,
        db:         &'a InstalledDb,
        explicit:   &'a [String],
        is_upgrade: bool,
    ) -> Self {
        Self { plan, db, explicit, is_upgrade, user_mode: false, store_override: None }
    }

    pub fn user(plan: &'a TransactionPlan, db: &'a InstalledDb, explicit: &'a [String]) -> Self {
        let user_env = crate::userenv::UserEnv::for_current_user().ok();
        Self {
            plan, db, explicit, is_upgrade: false, user_mode: true,
            store_override: user_env.map(|e| e.store_dir),
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  execute_transaction
// ─────────────────────────────────────────────────────────────

pub async fn execute_transaction(ctx: TransactionContext<'_>, note: &str) -> Result<u32> {
    let plan = ctx.plan;
    let db   = ctx.db;

    let effective_store_dir = ctx.store_override.as_deref()
    .unwrap_or_else(|| std::path::Path::new(crate::store::STORE_DIR));

    // ── 1. Download ───────────────────────────────────────────

    let all_pkgs: Vec<Package> = plan.to_install.iter()
    .chain(plan.to_upgrade.iter())
    .cloned().collect();

    let dl_results = if !all_pkgs.is_empty() {
        println!();
        println!(
            "  {}  {}",
            "⬡".bright_cyan().bold(),
                 format!(
                     "Fetching {} package{}…",
                     all_pkgs.len(),
                         if all_pkgs.len() == 1 { "" } else { "s" }
                 ).bold()
        );
        let client = HttpClient::new();
        download::download_packages(&client, &all_pkgs).await?
    } else {
        vec![]
    };

    // ── 2. Unpack ─────────────────────────────────────────────

    if !dl_results.is_empty() {
        println!();
        println!("  {}  {}", "⬡".bright_cyan().bold(), "Unpacking…".bold());
    }

    let mp = MultiProgress::new();
    let mut store_entries: Vec<StoreEntry> = Vec::new();

    for dl in &dl_results {
        let label   = format!("{} {}", dl.package.name, dl.package.version);
        let spinner = UnpackSpinner::new(&mp, &label);

        let deb_bytes = std::fs::read(&dl.path)
        .with_context(|| format!("Cannot read {}", dl.path.display()))?;
        let deb = DebPackage::parse(&deb_bytes)
        .with_context(|| format!("Parsing .deb for {}", dl.package.name))?;

        let entry = Store::install_deb_to(&dl.package, &deb, effective_store_dir)?;
        store_entries.push(entry);

        // Save maintainer scripts
        if let Some(script) = deb.extract_script("postinst") {
            let script: String = script;
            if !script.trim().is_empty() {
                let postinst_dir: PathBuf = if ctx.user_mode {
                    crate::userenv::UserEnv::for_current_user()
                    .map(|e| e.postinst_dir)
                    .unwrap_or_else(|_| PathBuf::from("/hammer/db/postinst"))
                } else {
                    PathBuf::from("/hammer/db/postinst")
                };
                std::fs::create_dir_all(&postinst_dir).ok();
                std::fs::write(
                    postinst_dir.join(format!("{}.postinst", dl.package.name)),
                               &script,
                ).ok();
            }
        }

        spinner.finish_ok(&label);
    }

    for name in &plan.to_remove { remove_postinst(name); }

    // ── 3. Compose generation ─────────────────────────────────

    println!();
    let compose_pb = {
        let pb = mp.add(ProgressBar::new_spinner());
        pb.set_style(
            ProgressStyle::with_template("  {spinner:.cyan}  {prefix:.bold}  {wide_msg}")
            .unwrap()
            .tick_strings(&["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏","·"]),
        );
        pb.enable_steady_tick(Duration::from_millis(80));
        pb
    };

    let mut gens_db = GenerationsDb::load()?;
    let gen_number  = gens_db.next_number();

    compose_pb.set_prefix("composing");
    compose_pb.set_message(
        format!("gen-{} from {} packages…", gen_number, store_entries.len())
            .dimmed().to_string(),
    );

    let mut new_packages: Vec<StoreEntry> = {
        let all_installed = db.list_all()?;
        let mut entries   = Vec::new();
        for inst in all_installed {
            let being_removed  = plan.to_remove.contains(&inst.name);
            let being_upgraded = plan.to_upgrade.iter().any(|p| p.name == inst.name);
            if being_removed || being_upgraded { continue; }
            let store_path = effective_store_dir
            .join(format!("{}-{}-{}", inst.name, inst.version, inst.store_hash));
            if store_path.exists() {
                entries.push(StoreEntry {
                    name: inst.name, version: inst.version,
                    hash: inst.store_hash, path: store_path,
                });
            }
        }
        entries
    };
    new_packages.extend(store_entries.iter().cloned());

    let gen = profile::compose_profile(gen_number, &new_packages, Some(note.to_string()))?;

    compose_pb.finish_with_message(format!(
        "{}  gen-{} ready ({} packages)",
                                           "✔".bright_green(), gen_number, new_packages.len()
    ));

    // ── 4. Live-patch or stage as pending ─────────────────────

    let all_new_files = crate::livepatch::collect_files(&store_entries);
    let analysis      = crate::livepatch::analyse(&all_new_files);
    let applied_live  = ctx.user_mode || (analysis.can_live_patch && !store_entries.is_empty());

    if applied_live {
        profile::switch_active(&gen)?;
        if ctx.user_mode {
            if let Ok(env) = crate::userenv::UserEnv::current() {
                env.apply_pending().ok();
            }
        }
        gens_db.generations.push(gen);
        gens_db.current = gen_number;
        gens_db.pending = None;
        gens_db.save()?;
    } else {
        profile::set_pending(&gen)
        .with_context(|| format!("Setting gen-{} as pending", gen_number))?;
        gens_db.generations.push(gen);
        gens_db.pending = Some(gen_number);
        gens_db.save()?;

        if !ctx.user_mode {
            if let Err(e) = crate::grub::update_grub(
                &GenerationsDb::load().unwrap_or_default(),
            ) {
                log::warn(&format!("grub update failed: {}", e));
            }
        }
    }

    // ── 5. Update DB ──────────────────────────────────────────

    for dl in &dl_results {
        let store_entry = store_entries.iter()
        .find(|e| e.name == dl.package.name)
        .expect("store entry must exist after unpack");

        let reason = if ctx.explicit.iter().any(|n| n == &dl.package.name) {
            InstallReason::User
        } else {
            InstallReason::Dependency
        };

        if plan.upgrade_from.contains_key(&dl.package.name) {
            let old_ver = plan.upgrade_from[&dl.package.name].clone();
            db.record_upgrade(&old_ver, &dl.package, &store_entry.hash, gen_number)?;
        } else {
            db.record_install(&dl.package, reason, &store_entry.hash, gen_number)?;
        }
    }

    for name in &plan.to_remove {
        if let Some(inst) = db.get(name) {
            db.record_remove(name, &inst.version, gen_number)?;
        }
    }

    // ── 6. Record integrity hash ──────────────────────────────

    if !ctx.user_mode {
        let _ = crate::gpg::record_gen_hash(gen_number);
    }

    log::info(&format!(
        "transaction complete: gen-{} (live={}, user={})",
                       gen_number, applied_live, ctx.user_mode
    ));

    Ok(gen_number)
}
