use anyhow::Result;
use owo_colors::OwoColorize;
use std::path::PathBuf;

use crate::db::InstalledDb;
use crate::store::STORE_DIR;
use crate::ui::human_size;

// ─────────────────────────────────────────────────────────────
//  hammer size [<pkg>...] [--all] [--sort=size|name] [--deps]
// ─────────────────────────────────────────────────────────────

pub fn cmd_size(args: &[String]) -> Result<()> {
    let show_all  = args.iter().any(|a| a == "--all");
    let sort_size = args.iter().any(|a| a == "--sort=size" || a == "--sort");
    let show_deps = args.iter().any(|a| a == "--deps");
    let names: Vec<&str> = args.iter()
        .filter(|a| !a.starts_with('-'))
        .map(|s| s.as_str())
        .collect();

    let db  = InstalledDb::open()?;
    let all = db.list_all()?;

    println!();
    println!("  {}  Rozmiary paczek", "⬡".bright_cyan().bold());
    println!("  {}", "─".repeat(65).dimmed());

    // Wybierz paczki do pokazania
    let selected: Vec<_> = if names.is_empty() || show_all {
        all.iter().collect()
    } else {
        all.iter().filter(|p| names.contains(&p.name.as_str())).collect()
    };

    if selected.is_empty() {
        println!("  {} Nie znaleziono żądanych paczek.", "·".dimmed());
        return Ok(());
    }

    // Pobierz faktyczny rozmiar na dysku ze store (jako fallback/weryfikacja)
    let mut rows: Vec<(String, u64, u64, String)> = selected.iter().map(|inst| {
        let db_size_bytes = inst.installed_size_kb * 1024;

        // Faktyczny rozmiar ze store (może się różnić od nagłówka deb)
        let store_path = PathBuf::from(STORE_DIR)
            .join(format!("{}-{}-{}", inst.name, inst.version, inst.store_hash));
        let actual_bytes = if store_path.exists() {
            dir_size_bytes(&store_path)
        } else {
            db_size_bytes // fallback na wartość z db
        };

        // Używaj wartości z db jeśli store nie istnieje, inaczej actual
        let display_bytes = if actual_bytes > 0 { actual_bytes } else { db_size_bytes };

        (inst.name.clone(), display_bytes, db_size_bytes, inst.version.clone())
    }).collect();

    // Sortowanie
    if sort_size {
        rows.sort_by(|a, b| b.1.cmp(&a.1));
    } else {
        rows.sort_by(|a, b| a.0.cmp(&b.0));
    }

    // Wyświetl tabelę
    let col_w = 36usize;
    println!(
        "  {:<width$} {:<12} {:<12} {}",
        "Paczka".bold(), "Rozmiar".bold(), "Wg deb".bold(), "Wersja".bold(),
        width = col_w
    );
    println!("  {}", "─".repeat(80).dimmed());

    let mut total_bytes = 0u64;
    for (name, actual, db_size, version) in &rows {
        let actual_str = human_size(*actual);
        let db_str     = if *db_size > 0 && *db_size != *actual {
            format!("({})", human_size(*db_size)).dimmed().to_string()
        } else {
            String::new()
        };

        println!(
            "  {:<width$} {:<12} {:<12} {}",
            name.cyan().bold(),
            actual_str.bright_white(),
            db_str,
            version.dimmed(),
            width = col_w
        );
        total_bytes += actual;
    }

    println!("  {}", "─".repeat(80).dimmed());
    println!(
        "  {:<width$} {}",
        format!("Razem ({} paczek):", rows.len()).bold(),
        human_size(total_bytes).bright_green().bold(),
        width = col_w
    );

    // Zależności — pokaż closure jeśli --deps
    if show_deps && !names.is_empty() {
        println!();
        println!("  {} Włącznie z zależnościami:", "·".dimmed());
        // Tutaj można by wywołać solver.dependency_closure() i zsumować
        // To jest placeholder — pełna implementacja wymaga PackageCache
        println!("  {} Użyj: {}", "→".dimmed(), "hammer size --deps --all".cyan());
    }

    println!();
    Ok(())
}

fn dir_size_bytes(path: &std::path::Path) -> u64 {
    if !path.exists() { return 0; }
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| std::fs::metadata(e.path()).ok())
        .map(|m| m.len())
        .sum()
}
