use anyhow::Result;
use owo_colors::OwoColorize;
use std::io::{self, Write};

use crate::db::{HistoryEntry, InstalledDb};
use crate::diff::GenDiff;
use crate::package::Package;
use crate::profile::{ActivationResult, GenerationsDb};
use crate::solver::TransactionPlan;

// ─────────────────────────────────────────────────────────────
//  Header / fatal / helpers
// ─────────────────────────────────────────────────────────────

pub fn print_header() {
    println!();
    println!("  {} {}",
             "⬡ hammer".bright_cyan().bold(),
             format!("v{}", env!("CARGO_PKG_VERSION")).dimmed());
    println!("  {}", "─".repeat(60).dimmed());
}

pub fn fatal(msg: &str) {
    eprintln!();
    eprintln!("  {} {}", "✗ error:".red().bold(), msg);
    eprintln!();
}

pub fn nothing_to_do() {
    println!("  {} Nothing to do.", "·".dimmed());
}

pub fn deps_resolved() {
    println!("  {} Dependencies resolved.", "✔".bright_green());
}

pub fn confirm(prompt: &str) -> Result<bool> {
    print!("\n  {} [Y/n] ", prompt.bold());
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let t = line.trim().to_lowercase();
    Ok(t.is_empty() || t == "y" || t == "yes")
}

pub fn human_size(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GiB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MiB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1_024 {
        format!("{:.0} KiB", bytes as f64 / 1_024.0)
    } else if bytes > 0 {
        format!("{} B", bytes)
    } else {
        String::new()
    }
}

// ─────────────────────────────────────────────────────────────
//  Transaction table
// ─────────────────────────────────────────────────────────────

pub fn print_transaction_table(plan: &TransactionPlan, _arch: &str) {
    let col_w = 36usize;

    if !plan.to_install.is_empty() {
        println!();
        println!("  {}", "Packages to install:".bold());
        println!("  {}", "─".repeat(80).dimmed());
        for pkg in &plan.to_install {
            let sz = human_size(pkg.download_size.unwrap_or(0));
            println!("  {:<width$} {:<16} {:<10} {}",
                     pkg.name.bright_green().bold(),
                     pkg.version.cyan(),
                     pkg.architecture.dimmed(),
                     sz.dimmed(),
                     width = col_w);
        }
    }

    if !plan.to_upgrade.is_empty() {
        println!();
        println!("  {}", "Packages to upgrade:".bold());
        println!("  {}", "─".repeat(80).dimmed());
        for pkg in &plan.to_upgrade {
            let old = plan.upgrade_from.get(&pkg.name).map(|v| v.as_str()).unwrap_or("?");
            let sz  = human_size(pkg.download_size.unwrap_or(0));
            println!("  {:<width$} {} → {} {}",
                     pkg.name.yellow().bold(),
                     old.dimmed(), pkg.version.bright_yellow(),
                     sz.dimmed(), width = col_w);
        }
    }

    if !plan.to_remove.is_empty() {
        println!();
        println!("  {}", "Packages to remove:".bold());
        println!("  {}", "─".repeat(80).dimmed());
        for name in &plan.to_remove {
            println!("  {}", name.red().bold());
        }
    }

    if !plan.to_autoremove.is_empty() {
        println!();
        println!("  {}", "Auto-remove (no longer needed):".bold());
        println!("  {}", "─".repeat(80).dimmed());
        println!("  {}", plan.to_autoremove.iter()
        .map(|s| s.dimmed().to_string()).collect::<Vec<_>>().join("  "));
    }
}

pub fn print_transaction_summary(plan: &TransactionPlan) {
    println!();
    println!("  {}", "─".repeat(60).dimmed());
    let install = plan.to_install.len() + plan.to_upgrade.len();
    let remove  = plan.to_remove.len()  + plan.to_autoremove.len();
    if install > 0 { println!("  {:<28} {}", "Install/upgrade:".bold(), install.to_string().bright_green()); }
    if remove  > 0 { println!("  {:<28} {}", "Remove:".bold(),          remove.to_string().red()); }
    if plan.download_bytes > 0 { println!("  {:<28} {}", "Download:".bold(),       human_size(plan.download_bytes).cyan()); }
    if plan.install_bytes  > 0 { println!("  {:<28} {}", "Installed size:".bold(), human_size(plan.install_bytes).cyan()); }
    if plan.freed_bytes    > 0 { println!("  {:<28} {}", "Freed:".bold(),          human_size(plan.freed_bytes).green()); }
    for w in &plan.warnings { println!("  {} {}", "warn:".yellow().bold(), w.yellow()); }
    println!();
}

pub fn print_pending_notice(gen_num: u32) {
    println!();
    println!("  {}", "─".repeat(60).dimmed());
    println!("  {}  Changes staged as {}",
             "⬡".bright_yellow().bold(),
             format!("gen-{}", gen_num).bold().bright_yellow());
    println!("  {}  Reboot to activate, or:", "·".dimmed());
    println!("    {}    show what changed",     "hammer diff".cyan());
    println!("    {}   cancel pending changes", "hammer pending cancel".cyan());
    println!("    {}  apply without reboot",    "hammer pending apply-live".cyan());
    println!();
}

// ─────────────────────────────────────────────────────────────
//  Activation result
// ─────────────────────────────────────────────────────────────

pub fn print_activation_result(r: &ActivationResult) {
    println!();
    println!("  {}  Boot activation complete", "⬡".bright_cyan().bold());
    println!("  {}", "─".repeat(60).dimmed());

    if r.already_active {
        println!("  {}  gen-{} already active — relinking binaries", "·".dimmed(), r.gen_number);
    } else {
        println!("  {:<28} gen-{}",
                 "Activated generation:".bold(),
                 r.gen_number.to_string().bright_green().bold());
    }

    if r.packages_linked > 0 {
        println!("  {:<28} {}", "Binaries linked to PATH:".bold(),
                 r.packages_linked.to_string().bright_green());
    } else {
        println!("  {:<28} {}",
                 "Binaries linked to PATH:".bold(),
                 "0 — run `hammer relink` if commands are missing".yellow());
    }

    if !r.scripts_failed.is_empty() {
        println!();
        println!("  {} {} postinst script(s) failed:", "!".yellow().bold(), r.scripts_failed.len());
        for pkg in &r.scripts_failed {
            println!("    {} {}", "·".yellow(), pkg.yellow());
        }
        println!("  Try: {}", "hammer fix-broken".cyan());
    }
    println!();
}

// ─────────────────────────────────────────────────────────────
//  Search results
// ─────────────────────────────────────────────────────────────

pub fn print_search_header(query: &str, count: usize) {
    println!();
    if count == 0 {
        println!("  {} No packages found matching '{}'.", "·".dimmed(), query.bold());
        println!("  Run {} to refresh the index.", "hammer sync".cyan());
    } else {
        println!("  {} {} package{} matching '{}'",
                 "✔".bright_green(), count.to_string().bold(),
                 if count == 1 { "" } else { "s" }, query.bold());
    }
    println!("  {}", "─".repeat(70).dimmed());
}

pub fn print_search_result(pkg: &Package, installed: bool) {
    let inst_mark = if installed { "✔".bright_green().to_string() } else { "·".dimmed().to_string() };
    let name_col  = if installed { pkg.name.bright_green().bold().to_string() } else { pkg.name.bold().to_string() };
    let desc = pkg.description_short.as_deref().unwrap_or("").chars().take(52).collect::<String>();
    println!("  {} {:<36} {:<16}  {}", inst_mark, name_col, pkg.version.cyan(), desc.dimmed());
}

// ─────────────────────────────────────────────────────────────
//  Package info
// ─────────────────────────────────────────────────────────────

pub fn print_package_info(pkg: &Package, installed: bool, installed_version: Option<&str>) {
    println!();
    println!("  {} {}", pkg.name.bold().bright_cyan(), pkg.version.cyan());
    println!("  {}", "─".repeat(60).dimmed());

    let status = if installed {
        if installed_version.map_or(true, |v| v == pkg.version) {
            "installed (up to date)".bright_green().to_string()
        } else {
            format!("installed ({}) — upgrade available", installed_version.unwrap_or("?")).yellow().to_string()
        }
    } else {
        "not installed".dimmed().to_string()
    };

    println!("  {:<20} {}", "Status:".bold(), status);
    println!("  {:<20} {}", "Architecture:".bold(), pkg.architecture.dimmed());
    if let Some(ref s) = pkg.section    { println!("  {:<20} {}", "Section:".bold(), s.dimmed()); }
    if let Some(ref m) = pkg.maintainer { println!("  {:<20} {}", "Maintainer:".bold(), m.dimmed()); }
    if let Some(sz) = pkg.installed_size_kb {
        println!("  {:<20} {}", "Installed size:".bold(), human_size(sz * 1024).cyan());
    }
    if let Some(ref d) = pkg.description_short {
        println!(); println!("  {}", d.bold());
    }
    if let Some(ref d) = pkg.description_long {
        println!();
        for line in d.lines().take(8) { println!("  {}", line.trim().dimmed()); }
    }
    if let Some(ref deps) = pkg.depends {
        println!(); println!("  {}", "Depends:".bold());
        println!("  {}", deps.dimmed());
    }
    println!();
}

// ─────────────────────────────────────────────────────────────
//  Status
// ─────────────────────────────────────────────────────────────

pub fn print_status(db: &InstalledDb) {
    println!();
    println!("  {}  hammer status", "⬡".bright_cyan().bold());
    println!("  {}", "─".repeat(60).dimmed());
    let count = db.list_all().map(|v| v.len()).unwrap_or(0);
    println!("  {:<26} {}", "Installed packages:".bold(), count.to_string().cyan().bold());
    if let Ok(gdb) = crate::profile::GenerationsDb::load() {
        println!("  {:<26} gen-{}", "Current generation:".bold(),
                 gdb.current.to_string().bright_green());
        if let Some(n) = gdb.pending {
            println!("  {:<26} gen-{} {}",
                     "Pending (on reboot):".bold(),
                     n.to_string().yellow(),
                     "(run `hammer diff` to see changes)".dimmed());
        }
        println!("  {:<26} {}", "Total generations:".bold(),
                 gdb.generations.len().to_string().dimmed());
    }
}

// ─────────────────────────────────────────────────────────────
//  History
//
//  HistoryEntry fields: id, action, package, old_ver, new_ver,
//                       generation, timestamp
// ─────────────────────────────────────────────────────────────

pub fn print_history(entries: &[HistoryEntry]) {
    println!();
    println!("  {}  Transaction history", "⬡".bright_cyan().bold());
    println!("  {}", "─".repeat(70).dimmed());
    if entries.is_empty() {
        println!("  {} No transaction history.", "·".dimmed());
        return;
    }
    for e in entries {
        let ts = e.timestamp.format("%Y-%m-%d %H:%M").to_string();
        let op_col = match e.action.as_str() {
            "install" => e.action.bright_green().to_string(),
            "remove"  => e.action.red().to_string(),
            "upgrade" => e.action.yellow().to_string(),
            other     => other.cyan().to_string(),
        };
        // FIX: use e.generation (not e.gen_number which doesn't exist)
        println!("  {}  {:<12} {:<36} gen-{}",
                 ts.dimmed(),
                 op_col,
                 e.package.bold(),
                 e.generation.to_string().cyan());
    }
}

// ─────────────────────────────────────────────────────────────
//  Generations
// ─────────────────────────────────────────────────────────────

pub fn print_generations(gdb: &GenerationsDb) {
    println!();
    println!("  {}  Generations", "⬡".bright_cyan().bold());
    println!("  {}", "─".repeat(70).dimmed());
    println!("  {:<8} {:<22} {:<8} {}", "Gen".bold(), "Date".bold(), "Pkgs".bold(), "Note".bold());
    println!("  {}", "─".repeat(70).dimmed());
    let mut gens = gdb.generations.clone();
    gens.sort_by(|a, b| b.number.cmp(&a.number));
    for gen in &gens {
        let active  = gen.number == gdb.current;
        let pending = gdb.pending == Some(gen.number);
        let marker = if active        { " ← active".bright_green().to_string() }
        else if pending  { " ← pending".yellow().to_string() }
        else             { String::new() };
        let ts   = gen.timestamp.format("%Y-%m-%d %H:%M").to_string();
        let note = gen.note.as_deref().unwrap_or("").chars().take(36).collect::<String>();
        let gen_str = if active {
            format!("gen-{}", gen.number).bright_green().bold().to_string()
        } else {
            format!("gen-{}", gen.number).dimmed().to_string()
        };
        println!("  {:<8} {:<22} {:<8} {}{}",
                 gen_str, ts.dimmed(), gen.packages.len().to_string().cyan(),
                 note.dimmed(), marker);
    }
    println!("  {}", "─".repeat(70).dimmed());
    println!();
    println!("  Rollback: {}     Switch: {}",
             "hammer rollback".cyan(), "hammer gen switch <N>".cyan());
}

// ─────────────────────────────────────────────────────────────
//  Diff
// ─────────────────────────────────────────────────────────────

pub fn print_diff(diff: &GenDiff) {
    println!();
    println!("  {}  Diff: gen-{} → gen-{}",
             "⬡".bright_cyan().bold(), diff.from, diff.to);
    println!("  {}", "─".repeat(60).dimmed());

    if diff.added.is_empty() && diff.removed.is_empty() && diff.upgraded.is_empty() {
        println!("  {} No differences.", "·".dimmed());
        return;
    }

    for pkg in &diff.added {
        println!("  {} {} {}", "+".bright_green().bold(), pkg.name.bright_green(), pkg.version.cyan());
    }
    for (pkg, old_ver) in &diff.upgraded {
        println!("  {} {} {} → {}", "↑".yellow().bold(), pkg.name.yellow(), old_ver.dimmed(), pkg.version.cyan());
    }
    for name in &diff.removed {
        println!("  {} {}", "-".red().bold(), name.red());
    }

    println!();
    println!("  {} added  {} upgraded  {} removed",
             diff.added.len().to_string().bright_green(),
             diff.upgraded.len().to_string().yellow(),
             diff.removed.len().to_string().red());
}

// ─────────────────────────────────────────────────────────────
//  List entry
// ─────────────────────────────────────────────────────────────

pub fn print_list_entry(
    name:      &str,
    version:   &str,
    arch:      &str,
    installed: bool,
    repo:      &str,
    new_ver:   Option<&str>,
) {
    let inst_mark = if installed { "✔".bright_green().to_string() } else { "·".dimmed().to_string() };
    let name_col  = if installed { name.bright_green().bold().to_string() } else { name.to_string() };
    let upgrade   = new_ver.map_or(String::new(), |v| format!(" → {}", v.bright_cyan()));
    println!("  {} {:<36} {:<20} {:<10} {}{}",
             inst_mark, name_col, version.cyan(), arch.dimmed(), repo.dimmed(), upgrade);
}
