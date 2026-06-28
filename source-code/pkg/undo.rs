use anyhow::Result;
use owo_colors::OwoColorize;
use std::path::Path;

use crate::profile::{GenerationsDb, activate_pending, set_pending};
use crate::ui;

// ─────────────────────────────────────────────────────────────
//  hammer undo [--to <N>] [-y/--yes]
// ─────────────────────────────────────────────────────────────

pub fn cmd_undo(args: &[String]) -> Result<()> {
    let yes = args.iter().any(|a| a == "-y" || a == "--yes");

    // Parse optional --to <N>
    let explicit_target: Option<u32> = args.windows(2)
        .find(|w| w[0] == "--to")
        .and_then(|w| w[1].strip_prefix("gen-").unwrap_or(&w[1]).parse().ok());

    let gdb     = GenerationsDb::load()?;
    let current = gdb.current;

    println!();
    println!("  {}  hammer undo", "⬡".bright_cyan().bold());
    println!("  {}", "─".repeat(65).dimmed());

    if current == 0 {
        println!("  {} Nie ma poprzedniej generacji do cofnięcia.", "·".dimmed());
        return Ok(());
    }

    // Resolve target generation
    let target_gen: u32 = match explicit_target {
        Some(n) => {
            if n >= current {
                anyhow::bail!(
                    "Cel cofnięcia gen-{} musi być mniejszy niż aktualna gen-{}",
                    n, current
                );
            }
            n
        }
        None => {
            if current == 0 {
                println!("  {} Nie ma poprzedniej generacji.", "·".dimmed());
                return Ok(());
            }
            current - 1
        }
    };

    // Validate target exists
    let current_gen = gdb.generations.iter().find(|g| g.number == current)
        .ok_or_else(|| anyhow::anyhow!("Nie znaleziono aktywnej generacji {}", current))?;
    let target = gdb.generations.iter().find(|g| g.number == target_gen)
        .ok_or_else(|| {
            // Show available generations to help user
            let available: Vec<String> = gdb.generations.iter()
                .map(|g| format!("gen-{} ({})", g.number, g.timestamp.format("%Y-%m-%d %H:%M")))
                .collect();
            anyhow::anyhow!(
                "Nie znaleziono generacji {}.\nDostępne: {}",
                target_gen,
                if available.is_empty() { "brak".to_string() } else { available.join(", ") }
            )
        })?;

    // Show plan
    if explicit_target.is_some() {
        println!(
            "  {} Cofanie do konkretnej generacji: {}",
            "·".dimmed(),
            format!("gen-{}", target_gen).bright_yellow()
        );
        if target_gen + 1 < current {
            println!(
                "  {} Pomijane generacje: {}",
                "⚠".yellow().bold(),
                ((target_gen + 1)..current)
                    .map(|n| format!("gen-{}", n))
                    .collect::<Vec<_>>()
                    .join(", ")
                    .dimmed()
            );
        }
    }

    println!(
        "  {} Aktualna generacja: {}  ({})",
        "·".dimmed(),
        format!("gen-{}", current).cyan(),
        current_gen.timestamp.format("%Y-%m-%d %H:%M").to_string().dimmed()
    );
    println!(
        "  {} Cel cofnięcia:      {}  ({})",
        "·".dimmed(),
        format!("gen-{}", target_gen).bright_green(),
        target.timestamp.format("%Y-%m-%d %H:%M").to_string().dimmed()
    );
    println!();

    // Diff
    let current_pkgs: std::collections::HashSet<&str> =
        current_gen.packages.iter().map(|s| s.name.as_str()).collect();
    let target_pkgs: std::collections::HashSet<&str> =
        target.packages.iter().map(|s| s.name.as_str()).collect();

    let will_remove: Vec<&str>  = current_pkgs.difference(&target_pkgs).copied().collect();
    let will_restore: Vec<&str> = target_pkgs.difference(&current_pkgs).copied().collect();

    if !will_remove.is_empty() {
        let mut sorted = will_remove.clone();
        sorted.sort();
        println!("  {} Zostaną cofnięte (usunięte z aktywnego profilu):", "✘".red());
        for pkg in &sorted {
            println!("    {} {}", "·".dimmed(), pkg.red());
        }
    }
    if !will_restore.is_empty() {
        let mut sorted = will_restore.clone();
        sorted.sort();
        println!("  {} Zostaną przywrócone:", "✔".bright_green());
        for pkg in &sorted {
            println!("    {} {}", "·".dimmed(), pkg.cyan());
        }
    }
    if will_remove.is_empty() && will_restore.is_empty() {
        println!("  {} Generacje są identyczne — nic do cofnięcia.", "·".dimmed());
        return Ok(());
    }

    // Conffiles warning
    println!();
    let conffiles_to_clean = collect_conffiles_for_packages(&will_remove);
    if !conffiles_to_clean.is_empty() {
        println!(
            "  {} Pliki konfiguracyjne stworzone przez cofnięte paczki:",
            "⚠".yellow().bold()
        );
        for cf in &conffiles_to_clean {
            let modified = is_conffile_modified(cf);
            let marker = if modified { "M".yellow().to_string() } else { " ".to_string() };
            println!("    {} {}", marker, cf.dimmed());
        }
        println!(
            "  {} Zmodyfikowane conffiles zostaną {} — oryginały zachowane w {}",
            "·".dimmed(),
            "przeniesione do .hammer-backup".yellow(),
            "/hammer/db/conffiles/".dimmed()
        );
        println!();
    }

    if !yes && !ui::confirm("Cofnąć operację?")? {
        println!("  Anulowano.");
        return Ok(());
    }

    // Clean conffiles
    let mut cleaned_confs = 0usize;
    for pkg_name in &will_remove {
        cleaned_confs += clean_postinst_conffiles(pkg_name);
    }
    if cleaned_confs > 0 {
        println!(
            "  {} Wyczyszczono {} conffiles postinst.",
            "✔".bright_green(), cleaned_confs
        );
    }

    // Activate target generation
    println!("  {} Przełączam na gen-{}…", "·".dimmed(), target_gen);

    let gdb_mut = GenerationsDb::load()?;
    let target_generation = gdb_mut.generations.iter()
        .find(|g| g.number == target_gen)
        .ok_or_else(|| anyhow::anyhow!("Generacja {} nie istnieje", target_gen))?
        .clone();

    set_pending(&target_generation)?;
    let result = activate_pending()?;

    if result.already_active {
        println!("  {} Generacja {} jest już aktywna.", "·".dimmed(), target_gen);
    } else {
        println!(
            "  {} Cofnięto do generacji {}.",
            "✔".bright_green(),
            format!("gen-{}", target_gen).cyan()
        );
        println!("  {} Zmiany wejdą w życie po restarcie.", "·".dimmed());
        if !result.scripts_failed.is_empty() {
            println!(
                "  {} Ostrzeżenie: postinst nieudane dla: {}",
                "⚠".yellow().bold(),
                result.scripts_failed.join(", ")
            );
        }
    }

    println!();
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  hammer rollback (alias for undo)
// ─────────────────────────────────────────────────────────────

pub fn cmd_rollback(args: &[String]) -> Result<()> {
    cmd_undo(args)
}

// ─────────────────────────────────────────────────────────────
//  Conffile cleanup helpers
// ─────────────────────────────────────────────────────────────

fn collect_conffiles_for_packages(pkg_names: &[&str]) -> Vec<String> {
    let conffiles_dir = Path::new("/hammer/db/conffiles");
    let mut result = Vec::new();
    for pkg in pkg_names {
        let list_path = conffiles_dir.join(pkg).join("list");
        if let Ok(content) = std::fs::read_to_string(&list_path) {
            for line in content.lines() {
                let l = line.trim();
                if !l.is_empty() { result.push(l.to_string()); }
            }
        }
    }
    result
}

fn is_conffile_modified(path: &str) -> bool {
    Path::new(path).exists()
}

fn clean_postinst_conffiles(pkg_name: &str) -> usize {
    let conffiles_dir = Path::new("/hammer/db/conffiles");
    let list_path = conffiles_dir.join(pkg_name).join("list");
    let content = match std::fs::read_to_string(&list_path) {
        Ok(c) => c,
        Err(_) => return 0,
    };
    let mut count = 0usize;
    for line in content.lines() {
        let path_str = line.trim();
        if path_str.is_empty() { continue; }
        let target = Path::new(path_str);
        if !target.exists() { continue; }
        if is_conffile_user_modified(pkg_name, path_str) {
            let backup_dir = Path::new("/hammer/db/conffiles-backup").join(pkg_name);
            let _ = std::fs::create_dir_all(&backup_dir);
            let backup_name = path_str.replace('/', "_").trim_start_matches('_').to_string();
            let backup_path = backup_dir.join(&backup_name);
            if std::fs::copy(target, &backup_path).is_ok() {
                crate::log::info(&format!(
                    "undo: backup zmodyfikowanego conffile {} → {}",
                    path_str, backup_path.display()
                ));
            }
        }
        if std::fs::remove_file(target).is_ok() {
            crate::log::info(&format!("undo: usunięto conffile {}", path_str));
            count += 1;
            if let Some(parent) = target.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }
    if let Ok(db) = crate::db::InstalledDb::open() {
        let _ = db.remove_conffiles_for_package(pkg_name);
    }
    count
}

fn is_conffile_user_modified(pkg_name: &str, path: &str) -> bool {
    use sha2::{Digest, Sha256};
    let orig_dir  = Path::new("/hammer/db/conffiles").join(pkg_name).join("orig");
    let path_key  = path.replace('/', "_").trim_start_matches('_').to_string();
    let orig_path = orig_dir.join(&path_key);
    let Ok(orig_bytes) = std::fs::read(&orig_path) else { return false };
    let Ok(curr_bytes) = std::fs::read(path)        else { return false };
    let orig_hash = format!("{:x}", Sha256::digest(&orig_bytes));
    let curr_hash = format!("{:x}", Sha256::digest(&curr_bytes));
    orig_hash != curr_hash
}
