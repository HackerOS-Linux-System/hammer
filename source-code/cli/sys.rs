use anyhow::{bail, Result};
use owo_colors::OwoColorize;

use crate::cache::detect_arch;
use crate::cli_types::has_flag;
use crate::db::InstalledDb;
use crate::diff::compute_diff;
use crate::grub;
use crate::hk_tools;
use crate::immutable;
use crate::lock;
use crate::livepatch;
use crate::profile::{
    self, activate_pending, clear_pending, GenerationsDb,
    read_active_gen, read_pending_gen,
};
use crate::repo::SOURCES_HK;
use crate::service;
use crate::store::Store;
use crate::ui;
use crate::userenv::{self, UserEnv};

// ─────────────────────────────────────────────────────────────
//  status / history
// ─────────────────────────────────────────────────────────────

pub fn cmd_status() -> Result<()> {
    let db = InstalledDb::open()?;
    ui::print_status(&db);
    println!("  {:<26} {}", "GRUB integration:".bold(), grub::grub_status().dimmed());
    if let Some(bg) = grub::read_boot_gen() {
        println!("  {:<26} gen-{}", "Booted with:".bold(), bg.to_string().cyan().bold());
    }
    if let Some(active) = read_active_gen() {
        println!("  {:<26} gen-{}", "Active profile:".bold(), active.to_string().bright_green());
        let bins = count_active_bins();
        println!("  {:<26} {}", "Binaries in PATH:".bold(), bins.to_string().cyan());
    }
    // Immutable status
    let imm = immutable::is_immutable_enabled();
    println!("  {:<26} {}",
             "Immutable filesystem:".bold(),
             if imm { "enabled".bright_green().to_string() }
             else   { "disabled (run `hammer immutable enable`)".yellow().to_string() });

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

fn count_active_bins() -> usize {
    let active = std::path::Path::new(crate::store::ACTIVE_LINK);
    if !active.exists() { return 0; }
    let mut count = 0;
    for sub in &["usr/bin","usr/sbin","usr/local/bin","bin"] {
        let dir = active.join(sub);
        if let Ok(entries) = std::fs::read_dir(&dir) {
            count += entries.filter_map(|e| e.ok()).count();
        }
    }
    count
}

pub fn cmd_history() -> Result<()> {
    let db = InstalledDb::open()?;
    ui::print_history(&db.history(50)?);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  service — wraps service::cmd_service
// ─────────────────────────────────────────────────────────────

pub fn cmd_service(args: &[String]) -> Result<()> {
    service::cmd_service(args)
}

// ─────────────────────────────────────────────────────────────
//  log — show hammer operation log
// ─────────────────────────────────────────────────────────────

pub fn cmd_log(args: &[String]) -> Result<()> {
    let lines: usize = args.iter()
    .find(|a| a.starts_with("-n"))
    .and_then(|a| a[2..].trim().parse().ok())
    .unwrap_or(50);

    let log_path = crate::log::LOG_FILE;
    if !std::path::Path::new(log_path).exists() {
        println!("  {} No log file at {}", "·".dimmed(), log_path);
        return Ok(());
    }

    println!("  {}  hammer log  (last {} lines from {})", "⬡".bright_cyan().bold(), lines, log_path);
    println!("  {}", "─".repeat(70).dimmed());

    let content = std::fs::read_to_string(log_path)?;
    let all_lines: Vec<&str> = content.lines().collect();
    let start = all_lines.len().saturating_sub(lines);

    for line in &all_lines[start..] {
        if      line.contains("ERROR") { println!("  {}", line.red()); }
        else if line.contains("WARN")  { println!("  {}", line.yellow()); }
        else if line.starts_with("──") { println!("  {}", line.dimmed()); }
        else                           { println!("  {}", line); }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  immutable — new in 0.3
// ─────────────────────────────────────────────────────────────

pub fn cmd_immutable(args: &[String]) -> Result<()> {
    immutable::cmd_immutable(args)
}

// ─────────────────────────────────────────────────────────────
//  diff / gen / rollback / pending
// ─────────────────────────────────────────────────────────────

pub fn cmd_diff(args: &[String]) -> Result<()> {
    let gdb = GenerationsDb::load()?;
    let (a, b) = if args.is_empty() {
        let p = gdb.pending.ok_or_else(|| anyhow::anyhow!("No pending generation. Use: hammer diff <A> <B>"))?;
        (gdb.current, p)
    } else if args.len() == 1 {
        (args[0].parse::<u32>()?, gdb.current)
    } else {
        (args[0].parse::<u32>()?, args[1].parse::<u32>()?)
    };
    ui::print_diff(&compute_diff(a, b, &gdb)?);
    Ok(())
}

pub async fn cmd_gen(args: &[String]) -> Result<()> {
    match args.first().map(|s| s.as_str()).unwrap_or("list") {
        "list" | "ls" => { ui::print_generations(&GenerationsDb::load()?); }
        "switch" => {
            let n: u32 = args.get(1).ok_or_else(|| anyhow::anyhow!("Usage: hammer gen switch <N>"))?.parse()?;
            let gdb   = GenerationsDb::load()?;
            let gen   = gdb.get(n).ok_or_else(|| anyhow::anyhow!("Generation {} not found.", n))?.clone();
            let _lock = lock::system_lock()?;
            profile::switch_active(&gen)?;
            profile::relink_bins(&gen.profile_path())?;
            let mut gdb2 = GenerationsDb::load()?;
            gdb2.current = n; gdb2.pending = None;
            clear_pending().ok(); gdb2.save()?;
            let _ = grub::update_grub(&gdb2);
            println!("  {} Now running gen-{}.", "✔".bright_green(), n);
        }
        other => bail!("Unknown gen subcommand: '{}'. Try: list, switch", other),
    }
    Ok(())
}

pub fn cmd_rollback() -> Result<()> {
    let _lock = lock::system_lock()?;
    let gdb   = GenerationsDb::load()?;
    let mut prev: Vec<_> = gdb.generations.iter()
    .filter(|g| g.number < gdb.current).collect();
    prev.sort_by(|a, b| b.number.cmp(&a.number));
    let p = prev.first().ok_or_else(|| anyhow::anyhow!("No previous generation."))?;
    println!("  Rolling back: gen-{} → gen-{}…", gdb.current, p.number);
    profile::switch_active(p)?;
    profile::relink_bins(&p.profile_path())?;
    let mut gdb2 = GenerationsDb::load()?;
    gdb2.current = p.number; gdb2.pending = None;
    clear_pending().ok(); gdb2.save()?;
    let _ = grub::update_grub(&gdb2);
    println!("  {} Rolled back to gen-{}. Binaries relinked.", "✔".bright_green(), p.number);
    Ok(())
}

pub fn cmd_pending(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("show");
    match sub {
        "show" | "status" => {
            let gdb = GenerationsDb::load()?;
            match read_pending_gen() {
                None => println!("  {} No pending changes.", "·".dimmed()),
                Some(n) => {
                    println!("\n  {} Pending: gen-{}", "⬡".bright_yellow().bold(),
                             n.to_string().bold().bright_yellow());
                    if let Some(gen) = gdb.get(n) {
                        if let Some(ref note) = gen.note { println!("  Operation : {}", note.cyan()); }
                        println!("  Packages  : {}", gen.package_count());
                    }
                    println!("\n  Will activate on next reboot.");
                    println!("  Cancel:     {}", "hammer pending cancel".cyan());
                    println!("  Apply now:  {}", "hammer pending apply-live".cyan());
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
    let gdb = GenerationsDb::load()?;
    let gen = gdb.get(pending_num).ok_or_else(|| anyhow::anyhow!("Pending gen not in DB"))?;
    let current_pkgs: std::collections::HashSet<String> = gdb.current_gen()
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
    profile::relink_bins(&gen.profile_path())?;
    let mut gdb2 = GenerationsDb::load()?;
    gdb2.current = pending_num; gdb2.pending = None;
    gdb2.save()?; clear_pending()?;
    println!("  {} Live patch applied ({} files, binaries relinked).",
             "✔".bright_green().bold(), result.updated_files.to_string().bold());
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  verify
// ─────────────────────────────────────────────────────────────

pub fn cmd_verify(args: &[String]) -> Result<()> {
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
            println!("  {} {} {} — store entry missing",
                     "✗".red().bold(), pkg.name.bold(), pkg.version.dimmed());
            missing += 1; continue;
        }
        let entry_ok = walkdir::WalkDir::new(&store_path).min_depth(1)
        .into_iter().flatten()
        .filter(|i| i.file_type().is_symlink())
        .all(|i| std::fs::read_link(i.path())
        .map(|t| t.exists() || t.symlink_metadata().is_ok()).unwrap_or(true));
        if entry_ok { ok += 1; if pkg_filter.is_some() {
            println!("  {} {} {} — OK", "✔".bright_green(), pkg.name.bold(), pkg.version.dimmed());
        }} else {
            println!("  {} {} {} — dangling symlinks",
                     "✗".red().bold(), pkg.name.bold(), pkg.version.dimmed());
            failed += 1;
        }
    }
    println!(); println!("  {}", "─".repeat(60).dimmed());
    println!("  {:<20} {}", "Checked:".bold(), to_check.len());
    println!("  {:<20} {}", "OK:".bold(), ok.to_string().bright_green());
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

pub fn cmd_doctor() -> Result<()> {
    ui::print_header();
    println!("  {}  hammer doctor — system health check", "⬡".bright_cyan().bold());
    println!("  {}", "─".repeat(65).dimmed()); println!();
    let mut issues = 0usize;
    macro_rules! check {
        ($label:expr, $ok:expr, $fix:expr) => {{
            let ok: bool = $ok;
            if ok { println!("  {}  {}", "✔".bright_green(), $label); }
            else  { println!("  {}  {} — {}", "✗".red().bold(), $label, $fix.yellow()); issues += 1; }
        }};
    }
    macro_rules! warn_check {
        ($label:expr, $ok:expr, $fix:expr) => {{
            let ok: bool = $ok;
            if ok { println!("  {}  {}", "✔".bright_green(), $label); }
            else  { println!("  {}  {} — {}", "!".yellow().bold(), $label, $fix.yellow()); }
        }};
    }

    check!("/hammer directory exists",           std::path::Path::new("/hammer").exists(), "run `hammer init`");
    check!("/hammer/active symlink valid",        std::fs::read_link("/hammer/active").map(|t| t.exists()).unwrap_or(false), "run `hammer _activate` or `hammer relink`");
    let bins = count_active_bins();
    if bins > 1 { println!("  {}  {} binaries linked to PATH", "✔".bright_green(), bins.to_string().cyan()); }
    else { println!("  {}  Only {} binary in PATH — run {}",
        "✗".red().bold(), bins, "hammer relink".cyan()); issues += 1; }
        check!("sources-list.hk exists",             std::path::Path::new(SOURCES_HK).exists(), "run `hammer init`");
        let lists_ok = std::path::Path::new(crate::cache::LISTS_DIR).exists()
        && std::fs::read_dir(crate::cache::LISTS_DIR).map(|mut d| d.next().is_some()).unwrap_or(false);
        check!("Package index populated",            lists_ok, "run `hammer sync`");
        check!("hammer-activate.service installed",  std::path::Path::new("/etc/systemd/system/hammer-activate.service").exists(), "run `hammer init`");
        let path_env = std::env::var("PATH").unwrap_or_default();
        check!("/usr/bin in PATH",                   path_env.contains("/usr/bin"), "add to /etc/environment");
        check!("hammer database accessible",         InstalledDb::open().is_ok(), "run `hammer init`");
        if read_pending_gen().is_some() { println!("  {}  Pending changes staged — reboot to activate", "ℹ".cyan().bold()); }
        else { println!("  {}  No pending changes", "✔".bright_green()); }
        check!("GRUB generator installed",           std::path::Path::new(crate::grub::GRUB_GENERATOR).exists(), "run `hammer init`");
        check!("Running on installed system",        crate::livecheck::live_reason().is_none(), "hammer cannot run in a live system");
        warn_check!("Immutable filesystem enabled",  immutable::is_immutable_enabled(), "run `hammer immutable enable`");
        let has_keys = std::path::Path::new(crate::gpg_verify::KEYRING_DIR).exists()
        && std::fs::read_dir(crate::gpg_verify::KEYRING_DIR).map(|mut d| d.next().is_some()).unwrap_or(false);
        warn_check!("Trusted GPG keys configured",   has_keys, "add keys: `hammer key add <url>`");
        let gpgv_ok = std::process::Command::new("gpgv").arg("--version")
        .output().map(|o| o.status.success()).unwrap_or(false);
        warn_check!("gpgv available",                gpgv_ok, "hammer install gnupg");

        let tools = hk_tools::list_tools();
        if !tools.is_empty() {
            let inst = tools.iter().filter(|(_, v, _)| v != "not installed").count();
            println!("  {}  HackerOS tools: {}/{} installed",
                     if inst == tools.len() { "✔".bright_green().to_string() } else { "ℹ".cyan().to_string() },
                         inst, tools.len());
        }

        println!(); println!("  {}", "─".repeat(65).dimmed());
        if issues == 0 { println!("  {}  All checks passed.", "✔".bright_green().bold()); }
        else {
            println!("  {}  {} issue{} found.", "!".yellow().bold(), issues, if issues == 1 { "" } else { "s" });
            if bins <= 1 { println!("\n  Quick fix: {}", "hammer relink".cyan().bold()); }
        }
        println!();
        Ok(())
}

// ─────────────────────────────────────────────────────────────
//  export / import / key / gc / clean / init / relink / store
// ─────────────────────────────────────────────────────────────

pub fn cmd_export(args: &[String]) -> Result<()> {
    let output = args.first().map(|s| s.as_str()).unwrap_or("hammer-export.tar.gz");
    let gdb = GenerationsDb::load()?;
    let gen = gdb.current_gen().ok_or_else(|| anyhow::anyhow!("No current generation"))?;
    let manifest = serde_json::json!({
        "hammer_version": env!("CARGO_PKG_VERSION"), "generation": gdb.current,
                                     "packages": gen.packages, "note": gen.note,
    });
    let out_file = std::fs::File::create(output)?;
    let enc      = flate2::write::GzEncoder::new(out_file, flate2::Compression::default());
    let mut tar  = tar::Builder::new(enc);
    let mb  = serde_json::to_vec_pretty(&manifest)?;
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
    println!("  {} Exported {} packages to {}", "✔".bright_green(), added, output.bold());
    Ok(())
}

pub async fn cmd_import_pkg(args: &[String]) -> Result<()> {
    use std::io::Read;
    let input = args.first().ok_or_else(|| anyhow::anyhow!("Usage: hammer import <file.tar.gz>"))?;
    let _lock = lock::system_lock()?;
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
            e.unpack(&dest).ok(); extracted += 1;
        }
    }
    if manifest_json.is_empty() { bail!("No manifest found — not a hammer export?"); }
    let m: serde_json::Value = serde_json::from_str(&manifest_json)?;
    println!("  {} Imported from gen-{} (hammer {})", "✔".bright_green(),
             m["generation"].as_u64().unwrap_or(0), m["hammer_version"].as_str().unwrap_or("?"));
    println!("  {} {} store entries extracted.", "·".dimmed(), extracted);
    Ok(())
}

pub async fn cmd_key(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" | "ls" => {
            let db = crate::gpg::KeyringDb::load()?;
            if db.keys.is_empty() {
                println!("  {} No trusted keys.", "·".dimmed());
                println!("  Add: {}", "hammer key add https://ftp-master.debian.org/keys/archive-key-12.gpg".cyan());
                return Ok(());
            }
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

pub fn cmd_gc(args: &[String]) -> Result<()> {
    let keep: u32 = args.iter().find(|a| a.starts_with("--keep"))
    .and_then(|a| a.split('=').nth(1)).and_then(|v| v.parse().ok()).unwrap_or(3);
    let yes   = has_flag(args, "-y");
    let _lock = lock::system_lock()?;
    let mut gdb = GenerationsDb::load()?;
    let active  = read_active_gen().unwrap_or(gdb.current);
    let pending = read_pending_gen();
    let mut cands: Vec<_> = gdb.generations.iter()
    .filter(|g| g.number != active && Some(g.number) != pending).cloned().collect();
    cands.sort_by(|a, b| b.number.cmp(&a.number));
    let to_del = cands[keep.min(cands.len() as u32) as usize..].to_vec();
    if to_del.is_empty() { println!("  {} Nothing to collect.", "·".dimmed()); return Ok(()); }
    for g in &to_del { println!("    gen-{}", g.number); }
    if !yes && !ui::confirm("Proceed?")? { println!("  Aborted."); return Ok(()); }
    let del_nums: Vec<u32> = to_del.iter().map(|g| g.number).collect();
    for g in &to_del { profile::delete_profile(g)?; }
    gdb.generations.retain(|g| !del_nums.contains(&g.number));
    let referenced: std::collections::HashSet<String> = gdb.generations.iter()
    .flat_map(|g| g.packages.iter().map(|p| format!("{}-{}-{}", p.name, p.version, p.store_hash))).collect();
    Store::gc_unreferenced(&referenced)?;
    gdb.save()?; let _ = grub::update_grub(&gdb);
    println!("  {} Garbage collection complete.", "✔".bright_green());
    Ok(())
}

pub fn cmd_clean() -> Result<()> {
    let n = crate::download::clean_cache()?;
    println!("  {} Removed {} cached archive(s).", "✔".bright_green(), n);
    Ok(())
}

pub fn cmd_init(args: &[String]) -> Result<()> {
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
    println!("  {} Initialising hammer {} (suite: {})…",
             "::".bold().cyan(), env!("CARGO_PKG_VERSION"), suite.bold());
    for dir in &["/hammer/store","/hammer/profiles","/hammer/db","/hammer/db/postinst",
        "/etc/hammer","/etc/hammer/HackerOS","/var/cache/hammer/archives",
        "/var/lib/hammer/lists","/usr/lib/hammer","/usr/lib/HackerOS/hammer"] {
            std::fs::create_dir_all(dir)?;
            println!("  {} {}", "·".dimmed(), dir.dimmed());
        }
        let arch = detect_arch();
    if !std::path::Path::new(crate::repo::SOURCES_HK).exists() {
        crate::repo::SourcesList::write_default(&arch, suite)?;
        println!("  {} Wrote {}", "✔".green(), crate::repo::SOURCES_HK);
    }
    profile::install_activate_service()?;
    immutable::install_immutable_service()?;
    if !std::path::Path::new(profile::GENERATIONS_FILE).exists() { GenerationsDb::default().save()?; }
    let gdb = GenerationsDb::load()?;
    if let Err(e) = grub::update_grub(&gdb) {
        println!("  {} GRUB: {} (non-fatal)", "·".yellow(), e.to_string().dimmed());
    }
    println!("\n  {} hammer {} initialised.", "✔".bright_green().bold(), env!("CARGO_PKG_VERSION"));
    println!("    1. {}", "hammer key add https://ftp-master.debian.org/keys/archive-key-12.gpg".cyan());
    println!("    2. {}", "hammer sync".cyan());
    println!("    3. {}", "hammer install <pkg>".cyan());
    Ok(())
}

pub fn cmd_relink() -> Result<()> {
    let gdb = GenerationsDb::load()?;
    let n   = read_active_gen().unwrap_or(gdb.current);
    let gen = gdb.get(n).ok_or_else(|| anyhow::anyhow!("Active generation {} not found", n))?.clone();
    let pp  = gen.profile_path();
    if !pp.exists() { bail!("Profile missing: {}\n  Try: hammer _activate", pp.display()); }
    println!("  {}  Relinking binaries for gen-{}…", "⬡".bright_cyan().bold(), n);
    match profile::relink_bins(&pp) {
        Ok((l, u)) => {
            println!("  {} Linked: {}  Removed: {}",
                     "✔".bright_green().bold(), l.to_string().bold(), u.to_string().dimmed());
            if l == 0 { println!("  {} No binaries to link.", "·".dimmed()); }
            else { println!("  Packages in PATH. Run {} if shell can't find them.", "hash -r".cyan()); }
        }
        Err(e) => bail!("relink failed: {}", e),
    }
    Ok(())
}

pub fn cmd_store() -> Result<()> {
    let s = "/usr/share/hammer/store";
    if !std::path::Path::new(s).exists() {
        bail!("Hammer Store not installed. Run: {}", "hammer install hammer-store".cyan());
    }
    if !std::process::Command::new(s).status()?.success() { bail!("hammer-store exited with error"); }
    Ok(())
}

pub fn cmd_activate_internal() -> Result<()> {
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

