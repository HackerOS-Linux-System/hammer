use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use std::io::{self, Write};
use std::time::Duration;

use crate::db::HistoryEntry;
use crate::package::Package;
use crate::profile::{ActivationResult, Generation, GenerationsDb, GenState};
use crate::solver::TransactionPlan;
use crate::store::Store;

// ─────────────────────────────────────────────────────────────
//  Terminal helpers
// ─────────────────────────────────────────────────────────────

pub fn term_width() -> usize {
    terminal_size::terminal_size()
    .map(|(w, _)| w.0 as usize)
    .unwrap_or(80)
    .min(120)
}

fn rule(ch: char) -> String {
    ch.to_string().repeat(term_width())
}
fn thin()  -> String { rule('─') }
fn thick() -> String { rule('━') }

// ─────────────────────────────────────────────────────────────
//  Header
// ─────────────────────────────────────────────────────────────

pub fn print_header() {
    println!();
    println!(
        "  {}  {}",
        "⬡ hammer".bold().bright_cyan(),
             format!("v{}", env!("CARGO_PKG_VERSION")).dimmed()
    );
    println!("  {}", thin().dimmed());
    println!();
}

// ─────────────────────────────────────────────────────────────
//  Section / step markers
// ─────────────────────────────────────────────────────────────

pub fn section(title: &str) {
    println!("  {} {}", "::".bold().cyan(), title.bold());
}

pub fn step_minor(action: &str, name: &str, version: &str) {
    if version.is_empty() {
        println!("  {} {:<18} {}", "→".dimmed(), action.bold(), name.cyan());
    } else {
        println!(
            "  {} {:<18} {} {}",
            "→".dimmed(),
                 action.bold(),
                 name.cyan(),
                 version.dimmed()
        );
    }
}

pub fn step_pending(old: u32, new: u32) {
    println!(
        "  {} Staging     {} {} {}",
        "⬡".yellow().bold(),
             format!("gen-{}", old).dimmed(),
                 "→".bold(),
             format!("gen-{}", new).bright_yellow().bold()
    );
}

pub fn step_switch(old: u32, new: u32) {
    println!(
        "  {} Switching   {} {} {}",
        "⬡".bright_cyan().bold(),
             format!("gen-{}", old).dimmed(),
                 "→".bold(),
             format!("gen-{}", new).bright_cyan().bold()
    );
}

// ─────────────────────────────────────────────────────────────
//  Pending notice
// ─────────────────────────────────────────────────────────────

pub fn print_pending_notice(gen_number: u32) {
    println!();
    println!("  {}", thick().dimmed());
    println!(
        "  {}  Generation {} has been staged.",
        "⬡".bright_yellow().bold(),
             format!("gen-{}", gen_number).bold()
    );
    println!();
    println!(
        "  Changes will take effect {}",
        "after the next reboot.".bold()
    );
    println!();
    println!(
        "  {:<28} {}",
        "See what's staged:".dimmed(),
             "hammer status".cyan()
    );
    println!(
        "  {:<28} {}",
        "Cancel pending changes:".dimmed(),
             "hammer pending cancel".cyan()
    );
    println!(
        "  {:<28} {}",
        "Rollback after reboot:".dimmed(),
             "hammer rollback".cyan()
    );
    println!("  {}", thick().dimmed());
    println!();
}

// ─────────────────────────────────────────────────────────────
//  Activation result
// ─────────────────────────────────────────────────────────────

pub fn print_activation_result(r: &ActivationResult) {
    if r.nothing_to_do {
        println!(
            "  {} hammer: no pending generation, nothing to activate.",
            "·".dimmed()
        );
        return;
    }

    println!();
    println!(
        "  {} hammer: activated {}",
        "✔".bright_green().bold(),
             format!("gen-{}", r.generation).bold()
    );

    if r.etc_merged > 0 {
        println!(
            "  {:<30} {}",
            "config files installed:",
            r.etc_merged.to_string().cyan()
        );
    }
    if !r.etc_conflicts.is_empty() {
        println!(
            "  {:<30} {} (saved as *.hammer-new)",
                 "config conflicts:".yellow(),
                 r.etc_conflicts.len().to_string().yellow()
        );
        for path in &r.etc_conflicts {
            println!("    {} {}", "·".yellow(), path.dimmed());
        }
    }
    if r.ldconfig_ran {
        println!(
            "  {:<30} {}",
            "shared libraries:",
            "updated".green()
        );
    }
    if !r.units_installed.is_empty() {
        println!(
            "  {:<30} {}",
            "systemd units:",
            r.units_installed.join(", ").cyan()
        );
    }
    if !r.scripts_ran.is_empty() {
        println!(
            "  {:<30} {}",
            "postinst scripts ok:",
            r.scripts_ran.join(", ").green()
        );
    }
    if !r.scripts_failed.is_empty() {
        println!(
            "  {:<30} {}",
            "postinst FAILED:".red().bold(),
                 r.scripts_failed.join(", ").red()
        );
    }
    if !r.users_created.is_empty() {
        println!(
            "  {:<30} {}",
            "users created:",
            r.users_created.join(", ").cyan()
        );
    }
    if r.alternatives_updated > 0 {
        println!(
            "  {:<30} {}",
            "alternatives updated:",
            r.alternatives_updated.to_string().cyan()
        );
    }
    if r.bins_linked > 0 {
        println!(
            "  {:<30} {} linked, {} removed",
            "binaries in PATH:".bold(),
                 r.bins_linked.to_string().bright_green(),
                 r.bins_unlinked.to_string().dimmed()
        );
    }
    if r.bins_linked == 0 && !r.nothing_to_do {
        println!(
            "  {} No binaries were linked to PATH — check /usr/local/bin",
            "warn:".yellow().bold()
        );
    }
    if !r.warnings.is_empty() {
        println!();
        for w in &r.warnings {
            println!("  {} {}", "warn:".yellow().bold(), w.yellow());
        }
    }
    println!();
}

// ─────────────────────────────────────────────────────────────
//  Status
// ─────────────────────────────────────────────────────────────

pub fn print_status(db: &crate::db::InstalledDb) {
    use crate::profile::{read_active_gen, read_pending_gen};

    print_header();

    let gens_db = GenerationsDb::load().unwrap_or_default();
    let active  = read_active_gen().unwrap_or(gens_db.current);
    let pending = read_pending_gen();
    let pkg_cnt = db.count();
    let store_b = Store::disk_usage();
    let gen_cnt = gens_db.generations.len();

    println!(
        "  {:<26} {}",
        "Active generation:".bold(),
             format!("gen-{}", active).bright_cyan().bold()
    );

    if let Some(gen) = gens_db.current_gen() {
        println!(
            "  {:<26} {}",
            "Since:".bold(),
                 gen.timestamp
                 .format("%Y-%m-%d %H:%M UTC")
                 .to_string()
                 .dimmed()
        );
        if let Some(ref note) = gen.note {
            println!(
                "  {:<26} {}",
                "Last operation:".bold(),
                     note.dimmed()
            );
        }
    }

    if let Some(pnum) = pending {
        println!();
        println!("  {}", thin().dimmed());
        println!(
            "  {}  {} {} {}",
            "⬡".bright_yellow().bold(),
                 "Pending:".bold().yellow(),
                 format!("gen-{}", pnum).bold().bright_yellow(),
                     "— will activate on next reboot".dimmed()
        );
        if let Some(gen) = gens_db.get(pnum) {
            if let Some(ref note) = gen.note {
                println!(
                    "  {:<26} {}",
                    "  Staged operation:".bold(),
                         note.cyan()
                );
            }
            println!(
                "  {:<26} {}",
                "  Staged packages:".bold(),
                     gen.package_count().to_string().bold()
            );
        }
        println!();
        println!(
            "  {:<26} {}",
            "  Cancel pending:".dimmed(),
                 "hammer pending cancel".cyan()
        );
        println!("  {}", thin().dimmed());
    } else {
        println!(
            "  {:<26} {}",
            "Pending:".bold(),
                 "none".dimmed()
        );
    }

    println!();
    println!(
        "  {:<26} {}",
        "Packages installed:".bold(),
             pkg_cnt.to_string().bold()
    );
    println!(
        "  {:<26} {}",
        "Store usage:".bold(),
             human_size(store_b).yellow().bold().to_string()
    );
    println!(
        "  {:<26} {}",
        "Generations:".bold(),
             gen_cnt.to_string().bold()
    );

    if let Ok(log) = std::fs::read_to_string(crate::profile::ACTIVATION_LOG) {
        let last = log.lines().filter(|l| l.contains("activated")).last();
        if let Some(line) = last {
            println!(
                "  {:<26} {}",
                "Last activation:".bold(),
                     line.trim().dimmed()
            );
        }
    }

    println!();

    // /etc conflicts
    let conflicts = find_hammer_new_files();
    if !conflicts.is_empty() {
        println!("  {}", thin().dimmed());
        println!(
            "  {} {} config file conflict(s) need review:",
                 "!".yellow().bold(),
                 conflicts.len().to_string().yellow().bold()
        );
        for p in &conflicts {
            println!("    {} {}", "·".yellow(), p.dimmed());
        }
        println!(
            "  Review:  original is saved as {}",
            "*.hammer-new".cyan()
        );
        println!("  {}", thin().dimmed());
        println!();
    }
}

fn find_hammer_new_files() -> Vec<String> {
    let mut out = Vec::new();
    fn walk(dir: &std::path::Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.to_string_lossy().ends_with(".hammer-new") {
                out.push(p.to_string_lossy().to_string());
            }
        }
    }
    walk(std::path::Path::new("/etc"), &mut out);
    out
}

// ─────────────────────────────────────────────────────────────
//  Transaction table
// ─────────────────────────────────────────────────────────────

pub fn print_transaction_table(plan: &TransactionPlan, _arch: &str) {
    let name_w = 30usize;
    let ver_w  = 22usize;
    let arch_w = 8usize;
    let repo_w = 18usize;

    println!();
    println!("  {}", thick().dimmed());
    println!(
        "  {:<name_w$} {:<ver_w$} {:<arch_w$} {:<repo_w$} {}",
        "Package".bold(),
             "Version".bold(),
             "Arch".bold(),
             "Repository".bold(),
             "Size".bold()
    );
    println!("  {}", thick().dimmed());

    if !plan.to_install.is_empty() {
        println!("{}", "  Installing:".bold().bright_green());
        for pkg in &plan.to_install {
            print_pkg_row(pkg, '+', name_w, ver_w, arch_w, repo_w);
        }
    }
    if !plan.to_upgrade.is_empty() {
        println!("{}", "  Upgrading:".bold().yellow());
        for pkg in &plan.to_upgrade {
            let old = plan.upgrade_from.get(&pkg.name).map(|s| s.as_str()).unwrap_or("?");
            print_upgrade_row(pkg, old, name_w, ver_w, arch_w, repo_w);
        }
    }
    if !plan.to_remove.is_empty() {
        println!("{}", "  Removing:".bold().red());
        for name in &plan.to_remove {
            println!("  {} {}", "-".red().bold(), name.red());
        }
    }
    if !plan.to_autoremove.is_empty() {
        println!("{}", "  Autoremove:".bold().red());
        for name in &plan.to_autoremove {
            println!(
                "  {} {}",
                "-".red().dimmed(),
                     name.red().dimmed()
            );
        }
    }
    println!("  {}", thin().dimmed());
}

fn repo_short(pkg: &Package) -> &str {
    pkg.repo_base_uri
    .as_deref()
    .unwrap_or("unknown")
    .trim_end_matches('/')
    .split('/')
    .last()
    .unwrap_or("unknown")
}

fn print_pkg_row(
    pkg: &Package,
    prefix: char,
    nw: usize,
    vw: usize,
    aw: usize,
    rw: usize,
) {
    let repo     = repo_short(pkg);
    let size_str = pkg.download_size.map(human_size).unwrap_or_else(|| "?".to_string());
    println!(
        "  {} {:<nw$} {:<vw$} {:<aw$} {:<rw$} {}",
        prefix.to_string().bright_green().bold(),
             pkg.name.bold(),
             pkg.version.bright_white(),
             pkg.architecture.dimmed(),
             repo.dimmed(),
             size_str.yellow(),
    );
}

fn print_upgrade_row(
    pkg: &Package,
    old: &str,
    nw: usize,
    vw: usize,
    aw: usize,
    rw: usize,
) {
    let repo     = repo_short(pkg);
    let size_str = pkg.download_size.map(human_size).unwrap_or_else(|| "?".to_string());
    let ver_disp = format!("{} → {}", old.dimmed(), pkg.version.bright_white());
    println!(
        "  {} {:<nw$} {:<vw$} {:<aw$} {:<rw$} {}",
        "↑".yellow().bold(),
             pkg.name.yellow().bold(),
             ver_disp,
             pkg.architecture.dimmed(),
             repo.dimmed(),
             size_str.yellow(),
    );
}

// ─────────────────────────────────────────────────────────────
//  Transaction summary
// ─────────────────────────────────────────────────────────────

pub fn print_transaction_summary(plan: &TransactionPlan) {
    println!();
    println!("  {}", "Transaction Summary".bold());
    println!("  {}", thin().dimmed());

    if !plan.to_install.is_empty() {
        println!(
            "  {:<14} {}",
            "Install:".bright_green().bold(),
                 plan.to_install.len().to_string().bold()
        );
    }
    if !plan.to_upgrade.is_empty() {
        println!(
            "  {:<14} {}",
            "Upgrade:".yellow().bold(),
                 plan.to_upgrade.len().to_string().bold()
        );
    }
    if !plan.to_remove.is_empty() {
        println!(
            "  {:<14} {}",
            "Remove:".red().bold(),
                 plan.to_remove.len().to_string().bold()
        );
    }
    if !plan.to_autoremove.is_empty() {
        println!(
            "  {:<14} {}",
            "Autoremove:".red().bold(),
                 plan.to_autoremove.len().to_string().bold()
        );
    }

    println!();
    if plan.download_bytes > 0 {
        println!(
            "  {:<26} {}",
            "Total download:",
            human_size(plan.download_bytes).yellow().bold()
        );
    }
    if plan.install_bytes > 0 {
        println!(
            "  {:<26} {}",
            "After install:",
            human_size(plan.install_bytes).yellow().bold()
        );
    }
    if plan.freed_bytes > 0 {
        println!(
            "  {:<26} {}",
            "Freed:",
            human_size(plan.freed_bytes).green().bold()
        );
    }
    if !plan.warnings.is_empty() {
        println!();
        for w in &plan.warnings {
            warn(w);
        }
    }
    println!();
}

// ─────────────────────────────────────────────────────────────
//  Confirm
// ─────────────────────────────────────────────────────────────

pub fn confirm(prompt: &str) -> io::Result<bool> {
    print!("  {} [{}]: ", prompt.bold(), "Y/n".bold().cyan());
    io::stdout().flush()?;
    let mut s = String::new();
    io::stdin().read_line(&mut s)?;
    let t = s.trim().to_lowercase();
    Ok(t.is_empty() || t == "y" || t == "yes")
}

// ─────────────────────────────────────────────────────────────
//  Misc
// ─────────────────────────────────────────────────────────────

pub fn nothing_to_do() {
    println!("  {} Nothing to do.", "·".dimmed());
}

pub fn deps_resolved() {
    println!("  {} Dependencies resolved.", "✔".green());
}

pub fn warn(msg: &str) {
    eprintln!("  {} {}", "Warning:".yellow().bold(), msg.yellow());
}

pub fn fatal(msg: &str) {
    eprintln!("  {} {}", "Error:".red().bold(), msg.bold());
}

// ─────────────────────────────────────────────────────────────
//  Generations list
// ─────────────────────────────────────────────────────────────

pub fn print_generations(gens_db: &GenerationsDb) {
    use crate::profile::{read_active_gen, read_pending_gen};
    let active  = read_active_gen().unwrap_or(gens_db.current);
    let pending = read_pending_gen();

    print_header();
    println!("  {}", "Generation History".bold());
    println!("  {}", thick().dimmed());
    println!(
        "  {:<6} {:<14} {:<24} {:<8} {}",
        "#".bold(),
             "State".bold(),
             "Date".bold(),
             "Pkgs".bold(),
             "Note".bold()
    );
    println!("  {}", thin().dimmed());

    let mut gens: Vec<_> = gens_db.generations.iter().collect();
    gens.sort_by(|a, b| b.number.cmp(&a.number));

    for gen in &gens {
        let state_str = if Some(gen.number) == pending {
            "⬡ pending".bright_yellow().bold().to_string()
        } else if gen.number == active {
            "● active ".bright_cyan().bold().to_string()
        } else {
            "  old    ".dimmed().to_string()
        };

        let date = gen.timestamp.format("%Y-%m-%d %H:%M").to_string();
        let note = gen
        .note
        .as_deref()
        .unwrap_or("")
        .chars()
        .take(36)
        .collect::<String>();

        println!(
            "  {:<6} {:<22} {:<24} {:<8} {}",
            format!("#{}", gen.number).bold(),
                state_str,
                 date.dimmed(),
                 gen.package_count().to_string().bold(),
                 note.dimmed()
        );
    }

    println!("  {}", thin().dimmed());
    println!(
        "  Rollback: {}   Switch: {}   Cancel pending: {}",
        "hammer rollback".cyan(),
             "hammer gen switch <N>".cyan(),
             "hammer pending cancel".cyan(),
    );
    println!();
}

// ─────────────────────────────────────────────────────────────
//  Search
// ─────────────────────────────────────────────────────────────

pub fn print_search_header(query: &str, count: usize) {
    println!();
    println!(
        "  {} results for {}:",
        count.to_string().bold().cyan(),
             query.bold()
    );
    println!("  {}", thin().dimmed());
    println!(
        "  {:<40} {:<22} {:<18} {}",
        "Name".bold(),
             "Version".bold(),
             "Repository".bold(),
             "Description".bold()
    );
    println!("  {}", thin().dimmed());
}

pub fn print_search_result(pkg: &Package, is_installed: bool) {
    let mark = if is_installed {
        "  ✔".bright_green().bold().to_string()
    } else {
        "   ".to_string()
    };
    let desc = pkg
    .description_short
    .as_deref()
    .unwrap_or("")
    .chars()
    .take(36)
    .collect::<String>();
    let repo = repo_short(pkg);
    let name = if is_installed {
        pkg.name.bold().bright_white().to_string()
    } else {
        pkg.name.clone()
    };

    println!(
        "{} {:<40} {:<22} {:<18} {}",
        mark,
        format!("{}.{}", name, pkg.architecture.dimmed()),
            pkg.version.cyan(),
             repo.dimmed(),
             desc.dimmed()
    );
}

// ─────────────────────────────────────────────────────────────
//  Package info
// ─────────────────────────────────────────────────────────────

pub fn print_package_info(
    pkg:           &Package,
    is_installed:  bool,
    installed_ver: Option<&str>,
) {
    println!();
    println!("  {}", thick().dimmed());
    let field = |label: &str, value: &str| {
        println!("  {:<22} {}", format!("{}:", label).bold(), value);
    };

    field("Name",         &pkg.name);
    field("Version",      &pkg.version);
    field("Architecture", &pkg.architecture);

    let status = if is_installed {
        if installed_ver.map(|v| v != pkg.version).unwrap_or(false) {
            format!(
                "{} (installed: {})",
                    "Upgrade available".yellow().bold(),
                    installed_ver.unwrap_or("?").dimmed()
            )
        } else {
            "Installed".bright_green().bold().to_string()
        }
    } else {
        "Available".dimmed().to_string()
    };
    field("Status", &status);

    if let Some(s) = pkg.section.as_deref()    { field("Section",    s); }
    if let Some(m) = pkg.maintainer.as_deref() { field("Maintainer", m); }
    if let Some(sz) = pkg.installed_size_kb    { field("Size",     &human_size(sz * 1024)); }
    if let Some(sz) = pkg.download_size        { field("Download", &human_size(sz)); }
    if let Some(h) = pkg.homepage.as_deref()   { field("Homepage",   h); }
    field("Repository", repo_short(pkg));
    if let Some(d) = pkg.depends.as_deref()    { field("Requires",   d); }
    if let Some(r) = pkg.recommends.as_deref() { field("Recommends", r); }
    if let Some(c) = pkg.conflicts.as_deref()  { field("Conflicts",  c); }

    println!("  {}", thin().dimmed());
    if let Some(d) = pkg.description_short.as_deref() {
        field("Summary", d);
    }
    if let Some(ref d_owned) = pkg.description_long {
        let d: &str = d_owned.as_str();
        println!("  {:<22}", "Description:".bold());
        for line in d.lines() {
            let l = line.trim();
            if l == "." {
                println!();
            } else {
                println!("    {}", l.dimmed());
            }
        }
    }
    println!("  {}", thick().dimmed());
    println!();
}

// ─────────────────────────────────────────────────────────────
//  List
// ─────────────────────────────────────────────────────────────

pub fn print_list_entry(
    name:        &str,
    version:     &str,
    arch:        &str,
    is_inst:     bool,
    repo:        &str,
    new_version: Option<&str>,
) {
    let status = if is_inst {
        if let Some(nv) = new_version {
            format!("  ↑ {}", nv.yellow())
        } else {
            "  ✔".bright_green().to_string()
        }
    } else {
        "   ".to_string()
    };

    println!(
        "{} {:<38} {:<22} {}",
        status,
        format!("{}.{}", name.bold(), arch.dimmed()),
            version.cyan(),
             repo.dimmed(),
    );
}

// ─────────────────────────────────────────────────────────────
//  History
// ─────────────────────────────────────────────────────────────

pub fn print_history(entries: &[HistoryEntry]) {
    println!();
    println!("  {}", "Transaction History".bold());
    println!("  {}", thick().dimmed());
    println!(
        "  {:<6} {:<12} {:<10} {:<30} {}",
        "ID".bold(),
             "Action".bold(),
             "Gen".bold(),
             "Package".bold(),
             "Date".bold()
    );
    println!("  {}", thin().dimmed());

    for e in entries {
        let action_str = match e.action.as_str() {
            "install" => format!("{:<10}", "install".bright_green().bold()),
            "remove"  => format!("{:<10}", "remove".red().bold()),
            "upgrade" => format!("{:<10}", "upgrade".yellow().bold()),
            other     => format!("{:<10}", other),
        };
        let pkg_ver = match (&e.old_ver, &e.new_ver) {
            (None, Some(nv))     => nv.clone(),
            (Some(ov), None)     => ov.clone(),
            (Some(ov), Some(nv)) => {
                format!("{} → {}", ov.dimmed(), nv.bright_cyan())
            }
            _ => String::new(),
        };
        println!(
            "  {:<6} {} {:<10} {:<30} {}",
            e.id.to_string().dimmed(),
                 action_str,
                 format!("gen-{}", e.generation).cyan().dimmed(),
                     format!("{} {}", e.package.bold(), pkg_ver),
                         e.timestamp.format("%Y-%m-%d %H:%M").to_string().dimmed()
        );
    }
    println!("  {}", thick().dimmed());
    println!();
}

// ─────────────────────────────────────────────────────────────
//  Repo spinner (used by cache.rs)
// ─────────────────────────────────────────────────────────────

pub fn make_repo_spinner(label: &str, mp: &MultiProgress) -> ProgressBar {
    let pb = mp.add(ProgressBar::new_spinner());
    pb.set_style(
        ProgressStyle::with_template(
            "  {spinner:.cyan}  {prefix:<40.bold}  {wide_msg}",
        )
        .unwrap()
        .tick_strings(&[
            "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏",
        ]),
    );
    pb.set_prefix(label.to_owned());
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}

// ─────────────────────────────────────────────────────────────
//  Diff output
// ─────────────────────────────────────────────────────────────

pub fn print_diff(diff: &crate::diff::GenDiff) {
    println!();
    println!(
        "  {} diff {} → {}",
        "⬡".cyan().bold(),
             format!("gen-{}", diff.gen_a).dimmed(),
                 format!("gen-{}", diff.gen_b).bold()
    );
    println!("  {}", "─".repeat(60).dimmed());

    if diff.is_empty() {
        println!(
            "  {} No changes between gen-{} and gen-{}.",
            "·".dimmed(),
                 diff.gen_a,
                 diff.gen_b
        );
        return;
    }

    if !diff.added.is_empty() {
        println!(
            "  {} Added ({}):",
                 "+".bright_green().bold(),
                 diff.added.len()
        );
        for e in &diff.added {
            println!(
                "    {} {}  {}",
                "+".green(),
                     e.name.bold(),
                     e.version.cyan()
            );
        }
    }
    if !diff.removed.is_empty() {
        println!(
            "  {} Removed ({}):",
                 "-".red().bold(),
                 diff.removed.len()
        );
        for e in &diff.removed {
            println!(
                "    {} {}  {}",
                "-".red(),
                     e.name.bold(),
                     e.version.dimmed()
            );
        }
    }
    if !diff.upgraded.is_empty() {
        println!(
            "  {} Upgraded ({}):",
                 "↑".yellow().bold(),
                 diff.upgraded.len()
        );
        for e in &diff.upgraded {
            println!(
                "    {} {}  {} → {}",
                "↑".yellow(),
                     e.name.bold(),
                     e.version_old.as_str().dimmed(),
                     e.version_new.as_str().bright_cyan()
            );
        }
    }

    println!("  {}", "─".repeat(60).dimmed());
    println!(
        "  Total: {} change(s)",
             diff.total_changes().to_string().bold()
    );
    println!();
}

// ─────────────────────────────────────────────────────────────
//  Helpers — public so download.rs can use human_size
// ─────────────────────────────────────────────────────────────

pub fn human_size(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GiB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MiB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1_024 {
        format!("{:.0} KiB", bytes as f64 / 1_024.0)
    } else {
        format!("{} B", bytes)
    }
}
