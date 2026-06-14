use serde::{Deserialize, Serialize};

pub const JSON_SCHEMA_VERSION: &str = "1";

// ─────────────────────────────────────────────────────────────
//  Generic envelope
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonResponse<T: Serialize> {
    pub schema_version: &'static str,
    pub command:        String,
    pub ok:             bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error:          Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data:           Option<T>,
}

impl<T: Serialize> JsonResponse<T> {
    pub fn ok(command: &str, data: T) -> Self {
        JsonResponse {
            schema_version: JSON_SCHEMA_VERSION,
            command:        command.to_string(),
            ok:             true,
            error:          None,
            data:           Some(data),
        }
    }

    pub fn err(command: &str, msg: &str) -> JsonResponse<()> {
        JsonResponse {
            schema_version: JSON_SCHEMA_VERSION,
            command:        command.to_string(),
            ok:             false,
            error:          Some(msg.to_string()),
            data:           None,
        }
    }

    pub fn print(&self) {
        println!("{}", serde_json::to_string_pretty(self).unwrap_or_default());
    }
}

// ─────────────────────────────────────────────────────────────
//  Per-command output types
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonPackage {
    pub name:          String,
    pub version:       String,
    pub architecture:  String,
    pub installed:     bool,
    pub installed_ver: Option<String>,
    pub size_bytes:    Option<u64>,
    pub section:       Option<String>,
    pub description:   Option<String>,
    pub repo:          Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonTransactionPlan {
    pub to_install:     Vec<JsonPackage>,
    pub to_upgrade:     Vec<JsonUpgrade>,
    pub to_remove:      Vec<String>,
    pub to_autoremove:  Vec<String>,
    pub download_bytes: u64,
    pub install_bytes:  u64,
    pub freed_bytes:    u64,
    pub warnings:       Vec<String>,
    pub conflicts:      Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonUpgrade {
    pub name:        String,
    pub old_version: String,
    pub new_version: String,
    pub size_bytes:  Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonStatus {
    pub installed_packages: usize,
    pub user_packages:      usize,
    pub dep_packages:       usize,
    pub current_gen:        u32,
    pub pending_gen:        Option<u32>,
    pub total_gens:         usize,
    pub active_bins:        usize,
    pub store_entries:      usize,
    pub immutable_enabled:  bool,
    pub grub_status:        String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonGeneration {
    pub number:    u32,
    pub timestamp: String,
    pub packages:  usize,
    pub note:      Option<String>,
    pub active:    bool,
    pub pending:   bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonHistoryEntry {
    pub id:         i64,
    pub action:     String,
    pub package:    String,
    pub old_ver:    Option<String>,
    pub new_ver:    Option<String>,
    pub generation: u32,
    pub timestamp:  String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonSearchResult {
    pub packages: Vec<JsonPackage>,
    pub total:    usize,
    pub query:    String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonDiff {
    pub from:     u32,
    pub to:       u32,
    pub added:    Vec<JsonPackage>,
    pub removed:  Vec<String>,
    pub upgraded: Vec<JsonUpgrade>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonFileList {
    pub package: String,
    pub version: String,
    pub files:   Vec<JsonFileEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonFileEntry {
    pub path:      String,
    pub file_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target:    Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonStats {
    pub installed_packages:   usize,
    pub user_packages:        usize,
    pub store_entries:        usize,
    pub store_size_bytes:     u64,
    pub profiles_size_bytes:  u64,
    pub generations:          usize,
    pub current_gen:          u32,
    pub modified_conffiles:   usize,
    pub download_cache_bytes: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonWhyResult {
    pub package: String,
    pub reason:  String,
    pub chains:  Vec<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonWhyNotResult {
    pub package: String,
    pub reasons: Vec<String>,
    pub exists:  bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonConffileEntry {
    pub path:    String,
    pub package: String,
    pub status:  String,
}

// ─────────────────────────────────────────────────────────────
//  Conversion helpers
// ─────────────────────────────────────────────────────────────

pub fn package_to_json(
    pkg:       &crate::package::Package,
    installed: bool,
    inst_ver:  Option<&str>,
) -> JsonPackage {
    JsonPackage {
        name:          pkg.name.clone(),
        version:       pkg.version.clone(),
        architecture:  pkg.architecture.clone(),
        installed,
        installed_ver: inst_ver.map(|s| s.to_string()),
        size_bytes:    pkg.download_size,
        section:       pkg.section.clone(),
        description:   pkg.description_short.clone(),
        repo:          pkg.repo_base_uri.clone(),
    }
}

pub fn plan_to_json(plan: &crate::solver::TransactionPlan) -> JsonTransactionPlan {
    use crate::db::InstalledDb;
    let db = InstalledDb::open().ok();

    let to_install = plan.to_install.iter().map(|p| {
        let inst = db.as_ref().and_then(|d| d.get(&p.name));
        package_to_json(p, inst.is_some(), inst.as_ref().map(|i| i.version.as_str()))
    }).collect();

    let to_upgrade = plan.to_upgrade.iter().map(|p| JsonUpgrade {
        name:        p.name.clone(),
                                                old_version: plan.upgrade_from.get(&p.name).cloned().unwrap_or_default(),
                                                new_version: p.version.clone(),
                                                size_bytes:  p.download_size,
    }).collect();

    JsonTransactionPlan {
        to_install,
        to_upgrade,
        to_remove:      plan.to_remove.clone(),
        to_autoremove:  plan.to_autoremove.clone(),
        download_bytes: plan.download_bytes,
        install_bytes:  plan.install_bytes,
        freed_bytes:    plan.freed_bytes,
        warnings:       plan.warnings.clone(),
        conflicts:      plan.conflicts.clone(),
    }
}

pub fn history_to_json(entries: &[crate::db::HistoryEntry]) -> Vec<JsonHistoryEntry> {
    entries.iter().map(|e| JsonHistoryEntry {
        id:         e.id,
        action:     e.action.clone(),
                       package:    e.package.clone(),
                       old_ver:    e.old_ver.clone(),
                       new_ver:    e.new_ver.clone(),
                       generation: e.generation,
                       timestamp:  e.timestamp.to_rfc3339(),
    }).collect()
}

pub fn generations_to_json(gdb: &crate::profile::GenerationsDb) -> Vec<JsonGeneration> {
    gdb.generations.iter().map(|g| JsonGeneration {
        number:    g.number,
        timestamp: g.timestamp.to_rfc3339(),
                               packages:  g.packages.len(),
                               note:      g.note.clone(),
                               active:    g.number == gdb.current,
                               pending:   gdb.pending == Some(g.number),
    }).collect()
}

// ─────────────────────────────────────────────────────────────
//  Output helpers
// ─────────────────────────────────────────────────────────────

/// Check if --json flag is present in args.
pub fn is_json_mode(args: &[String]) -> bool {
    args.iter().any(|a| a == "--json")
}

pub fn print_json<T: serde::Serialize>(command: &str, data: T) {
    JsonResponse::ok(command, data).print();
}

pub fn print_json_error(command: &str, err: &str) {
    JsonResponse::<()>::err(command, err).print();
}

// ─────────────────────────────────────────────────────────────
//  Wired commands — search, list, status, history
//
//  These are the first concrete consumers of is_json_mode(): each checks
//  the flag up front and, if set, prints JSON and returns early instead
//  of falling through to the human-readable renderer.
// ─────────────────────────────────────────────────────────────

/// hammer search <query> [--json]
pub fn cmd_search(args: &[String]) -> anyhow::Result<()> {
    let query = args.iter().find(|a| !a.starts_with("--"))
    .ok_or_else(|| anyhow::anyhow!("Usage: hammer search <query> [--json]"))?;

    let cache = crate::cache::PackageCache::load()?;
    let db    = crate::db::InstalledDb::open()?;
    let results = cache.search(query);

    if is_json_mode(args) {
        let packages: Vec<JsonPackage> = results.iter().map(|p| {
            let inst = db.get(&p.name);
            package_to_json(p, inst.is_some(), inst.as_ref().map(|i| i.version.as_str()))
        }).collect();
        print_json("search", JsonSearchResult {
            total: packages.len(), packages, query: query.clone(),
        });
        return Ok(());
    }

    // Human-readable fallback
    use owo_colors::OwoColorize;
    println!();
    println!("  {}  Search results for '{}'", "⬡".bright_cyan().bold(), query.bold());
    println!("  {}", "─".repeat(60).dimmed());
    if results.is_empty() {
        println!("  {} No packages found.", "·".dimmed());
    }
    for pkg in results.iter().take(50) {
        let mark = if db.is_installed(&pkg.name) { "●".bright_green().to_string() }
        else { "○".dimmed().to_string() };
        println!("  {} {:<28} {:<12} {}",
                 mark, pkg.name.bold(), pkg.version.cyan(),
                 pkg.description_short.as_deref().unwrap_or("").dimmed());
    }
    if results.len() > 50 {
        println!("  … {} more results. Refine your query.", results.len() - 50);
    }
    Ok(())
}

/// hammer list [--installed] [--upgrades] [--json]
pub fn cmd_list(args: &[String]) -> anyhow::Result<()> {
    let installed_only = args.iter().any(|a| a == "--installed");
    let upgrades_only  = args.iter().any(|a| a == "--upgrades");

    let db    = crate::db::InstalledDb::open()?;
    let cache = crate::cache::PackageCache::load()?;

    let mut packages: Vec<JsonPackage> = Vec::new();

    if upgrades_only {
        for inst in db.list_all()? {
            if let Some(avail) = cache.get(&inst.name) {
                if crate::solver::version::compare(&avail.version, &inst.version)
                    == std::cmp::Ordering::Greater
                    {
                        packages.push(package_to_json(avail, true, Some(&inst.version)));
                    }
            }
        }
    } else if installed_only {
        for inst in db.list_all()? {
            let pkg = cache.get(&inst.name).cloned().unwrap_or_else(|| {
                crate::package::Package {
                    name: inst.name.clone(),
                                                                    version: inst.version.clone(),
                                                                    architecture: inst.architecture.clone(),
                                                                    ..crate::package::Package::default()
                }
            });
            packages.push(package_to_json(&pkg, true, Some(&inst.version)));
        }
    } else {
        for pkg in cache.all_packages() {
            let inst = db.get(&pkg.name);
            packages.push(package_to_json(pkg, inst.is_some(),
                                          inst.as_ref().map(|i| i.version.as_str())));
        }
    }

    if is_json_mode(args) {
        print_json("list", packages);
        return Ok(());
    }

    use owo_colors::OwoColorize;
    println!();
    let title = if upgrades_only { "Available upgrades" }
    else if installed_only { "Installed packages" }
    else { "All packages" };
    println!("  {}  {}", "⬡".bright_cyan().bold(), title);
    println!("  {}", "─".repeat(60).dimmed());

    for pkg in packages.iter().take(200) {
        let mark = if pkg.installed { "✔".bright_green().to_string() }
        else { "○".dimmed().to_string() };
        let ver_str = if let (true, Some(iv)) = (upgrades_only, &pkg.installed_ver) {
            format!("{} → {}", iv.dimmed(), pkg.version.cyan())
        } else {
            pkg.version.clone()
        };
        println!("  {} {:<28} {}", mark, pkg.name.bold(), ver_str);
    }
    if packages.len() > 200 {
        println!("  … {} more. Use --json for full output.", packages.len() - 200);
    }
    if packages.is_empty() {
        println!("  {} Nothing to show.", "·".dimmed());
    }
    Ok(())
}

/// hammer status [--json]
pub fn cmd_status(args: &[String]) -> anyhow::Result<()> {
    let db  = crate::db::InstalledDb::open()?;
    let all = db.list_all()?;

    let total = all.len();
    let user  = all.iter().filter(|p| p.reason == crate::db::InstallReason::User).count();
    let dep   = total - user;

    let gdb = crate::profile::GenerationsDb::load().unwrap_or_default();
    let store_entries = std::fs::read_dir(crate::store::STORE_DIR)
    .map(|d| d.flatten().count()).unwrap_or(0);
    // FIX E0425: `crate::immutable::is_enabled()` and
    // `crate::grub::status_summary()` don't exist in those modules.
    // Implement equivalent checks locally:
    //   - immutable /etc is detected by comparing the device ID of /etc
    //     vs / (a bind-mount or overlay gives /etc its own st_dev, which
    //     is how system/immutable.rs sets it up).
    //   - GRUB status is summarised from whether the config fragment that
    //     system/grub.rs writes exists and is non-empty.
    let immutable   = is_etc_immutable();
    let grub_status = grub_status_summary();

    let active_bins = std::fs::read_dir("/usr/local/bin")
    .map(|d| d.flatten().count()).unwrap_or(0);

    let status = JsonStatus {
        installed_packages: total,
        user_packages:      user,
        dep_packages:       dep,
        current_gen:        gdb.current,
        pending_gen:        gdb.pending,
        total_gens:         gdb.generations.len(),
        active_bins,
        store_entries,
        immutable_enabled:  immutable,
        grub_status:        grub_status.clone(),
    };

    if is_json_mode(args) {
        print_json("status", status);
        return Ok(());
    }

    use owo_colors::OwoColorize;
    println!();
    println!("  {}  hammer status", "⬡".bright_cyan().bold());
    println!("  {}", "─".repeat(50).dimmed());
    println!("  {:<24} {}", "Installed packages:".bold(), total.to_string().cyan());
    println!("  {:<24} {} ({} user, {} deps)", "  breakdown:".bold(),
             "".dimmed(), user, dep);
    println!("  {:<24} gen-{}", "Current generation:".bold(),
             gdb.current.to_string().bright_green());
    if let Some(p) = gdb.pending {
        println!("  {:<24} gen-{} {}", "Pending generation:".bold(),
                 p.to_string().yellow(), "(reboot to activate)".dimmed());
    }
    println!("  {:<24} {}", "Total generations:".bold(), gdb.generations.len());
    println!("  {:<24} {}", "Store entries:".bold(), store_entries.to_string().cyan());
    println!("  {:<24} {}", "Immutable /etc:".bold(),
             if immutable { "enabled".bright_green().to_string() }
             else          { "disabled".dimmed().to_string() });
    println!("  {:<24} {}", "GRUB:".bold(), grub_status.dimmed());
    println!();
    Ok(())
}

/// hammer history [-n N] [--json]
pub fn cmd_history(args: &[String]) -> anyhow::Result<()> {
    let limit: usize = args.iter()
    .find(|a| a.starts_with("-n"))
    .and_then(|a| a[2..].trim().parse().ok())
    .unwrap_or(20);

    let db = crate::db::InstalledDb::open()?;
    let entries = db.history(limit)?;

    if is_json_mode(args) {
        print_json("history", history_to_json(&entries));
        return Ok(());
    }

    use owo_colors::OwoColorize;
    println!();
    println!("  {}  Transaction history", "⬡".bright_cyan().bold());
    println!("  {}", "─".repeat(70).dimmed());
    println!("  {:<6} {:<22} {:<10} {:<28} {}",
             "Gen".bold(), "Timestamp".bold(), "Action".bold(),
             "Package".bold(), "Version".bold());
    println!("  {}", "─".repeat(70).dimmed());

    for e in &entries {
        let ts = e.timestamp.to_rfc3339().chars().take(19).collect::<String>();
        let action_col = match e.action.as_str() {
            "install" => e.action.bright_green().to_string(),
            "remove"  => e.action.red().to_string(),
            "upgrade" => e.action.yellow().to_string(),
            other     => other.cyan().to_string(),
        };
        let ver = match (&e.old_ver, &e.new_ver) {
            (Some(o), Some(n)) if o != n => format!("{} → {}", o.dimmed(), n.cyan()),
            (_, Some(n)) => n.cyan().to_string(),
            (Some(o), _) => o.dimmed().to_string(),
            _            => String::new(),
        };
        println!("  gen-{:<3} {:<22} {:<18} {:<28} {}",
                 e.generation, ts.dimmed(), action_col, e.package.bold(), ver);
    }
    if entries.is_empty() {
        println!("  {} No history recorded yet.", "·".dimmed());
    }
    println!();
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Local status helpers (cmd_status)
//
//  These replace the previously-referenced but non-existent
//  `crate::immutable::is_enabled()` and `crate::grub::status_summary()`.
// ─────────────────────────────────────────────────────────────

/// Returns true if /etc appears to be mounted separately from / —
/// i.e. it has a different st_dev, which is how a bind-mount or
/// overlay (as set up by `hammer immutable enable`) would present.
fn is_etc_immutable() -> bool {
    use std::os::unix::fs::MetadataExt;
    let root_dev = std::fs::metadata("/").map(|m| m.dev()).ok();
    let etc_dev  = std::fs::metadata("/etc").map(|m| m.dev()).ok();
    match (root_dev, etc_dev) {
        (Some(r), Some(e)) => r != e,
        _ => false,
    }
}

/// Summarise GRUB status from the presence of the config fragment that
/// `system/grub.rs` generates (e.g. /etc/grub.d/09_hammer or
/// /boot/grub/hammer.cfg, depending on layout). Falls back to "unknown"
/// if neither is found, and "not configured" if grub-mkconfig itself
/// isn't installed.
fn grub_status_summary() -> String {
    const CANDIDATES: &[&str] = &[
        "/etc/grub.d/09_hammer",
        "/boot/grub/hammer.cfg",
        "/boot/grub2/hammer.cfg",
    ];

    for path in CANDIDATES {
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.len() > 0 {
                return format!("configured ({})", path);
            }
        }
    }

    let has_grub_mkconfig = std::process::Command::new("which")
    .arg("grub-mkconfig")
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status()
    .map(|s| s.success())
    .unwrap_or(false);

    if has_grub_mkconfig {
        "not configured".to_string()
    } else {
        "unknown".to_string()
    }
}
