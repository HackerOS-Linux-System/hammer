use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use rusqlite::{params, Connection};
use std::path::PathBuf;

use crate::db::DB_PATH;
use crate::store::STORE_DIR;

// ─────────────────────────────────────────────────────────────
//  FileIndex
// ─────────────────────────────────────────────────────────────

pub struct FileIndex {
    conn: Connection,
}

impl FileIndex {
    pub fn open() -> Result<Self> {
        let conn = Connection::open(DB_PATH)
            .with_context(|| format!("Nie można otworzyć bazy: {}", DB_PATH))?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        let idx = FileIndex { conn };
        idx.ensure_table()?;
        Ok(idx)
    }

    fn ensure_table(&self) -> Result<()> {
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS file_index (
                path    TEXT NOT NULL,
                package TEXT NOT NULL,
                PRIMARY KEY (path, package)
            );
            CREATE INDEX IF NOT EXISTS idx_file_index_pkg  ON file_index(package);
            CREATE INDEX IF NOT EXISTS idx_file_index_path ON file_index(path);
        ")?;
        Ok(())
    }

    // ── Budowanie indeksu przy instalacji ────────────────────

    /// Zindeksuj wszystkie pliki z katalogu store dla danej paczki.
    /// Wywołaj po udanym `execute_transaction` przy instalacji/upgrade.
    pub fn index_package(&self, pkg_name: &str, store_hash: &str, version: &str) -> Result<()> {
        let store_path = PathBuf::from(STORE_DIR)
            .join(format!("{}-{}-{}", pkg_name, version, store_hash));

        if !store_path.exists() {
            // Store nie jest jeszcze widoczny (pending generation) — pomiń
            return Ok(());
        }

        // Usuń stare wpisy tej paczki zanim dodasz nowe
        self.conn.execute(
            "DELETE FROM file_index WHERE package = ?1",
            params![pkg_name],
        )?;

        let mut count = 0usize;
        for entry in walkdir::WalkDir::new(&store_path)
            .min_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file() || e.file_type().is_symlink())
        {
            if let Ok(rel) = entry.path().strip_prefix(&store_path) {
                let installed_path = format!("/{}", rel.display());
                self.conn.execute(
                    "INSERT OR REPLACE INTO file_index (path, package) VALUES (?1, ?2)",
                    params![installed_path, pkg_name],
                )?;
                count += 1;
            }
        }

        crate::log::info(&format!(
            "file_index: zindeksowano {} plików dla {}",
            count, pkg_name
        ));
        Ok(())
    }

    /// Usuń wszystkie wpisy dla danej paczki z indeksu (przy remove).
    pub fn remove_package(&self, pkg_name: &str) -> Result<()> {
        let deleted = self.conn.execute(
            "DELETE FROM file_index WHERE package = ?1",
            params![pkg_name],
        )?;
        crate::log::info(&format!(
            "file_index: usunięto {} wpisów dla {}",
            deleted, pkg_name
        ));
        Ok(())
    }

    // ── Zapytania ─────────────────────────────────────────────

    /// Znajdź paczki posiadające dokładny plik `path`.
    pub fn lookup(&self, path: &str) -> Result<Vec<String>> {
        let norm = normalize_path(path);
        let mut stmt = self.conn.prepare(
            "SELECT package FROM file_index WHERE path = ?1 ORDER BY package",
        )?;
        let rows = stmt.query_map(params![norm], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Znajdź paczki przez fuzzy match (LIKE).
    pub fn lookup_fuzzy(&self, pattern: &str) -> Result<Vec<(String, String)>> {
        let like = format!("%{}%", pattern.trim_start_matches('/'));
        let mut stmt = self.conn.prepare(
            "SELECT path, package FROM file_index WHERE path LIKE ?1 ORDER BY path LIMIT 50",
        )?;
        let rows = stmt.query_map(params![like], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Czy indeks jest pusty? (świeża instalacja bez rebuild)
    pub fn is_empty(&self) -> bool {
        self.conn
            .query_row("SELECT COUNT(*) FROM file_index", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) == 0
    }

    /// Odbuduj cały indeks ze store — przydatne przy migracji z < 0.5.
    pub fn rebuild_all(&self) -> Result<usize> {
        self.conn.execute("DELETE FROM file_index", [])?;

        let db = crate::db::InstalledDb::open()?;
        let all = db.list_all()?;
        let mut total = 0usize;

        for inst in &all {
            let store_path = PathBuf::from(STORE_DIR)
                .join(format!("{}-{}-{}", inst.name, inst.version, inst.store_hash));
            if !store_path.exists() { continue; }

            for entry in walkdir::WalkDir::new(&store_path)
                .min_depth(1)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file() || e.file_type().is_symlink())
            {
                if let Ok(rel) = entry.path().strip_prefix(&store_path) {
                    let installed_path = format!("/{}", rel.display());
                    let _ = self.conn.execute(
                        "INSERT OR REPLACE INTO file_index (path, package) VALUES (?1, ?2)",
                        params![installed_path, inst.name],
                    );
                    total += 1;
                }
            }
        }

        crate::log::info(&format!("file_index: rebuild zakończony, {} plików", total));
        Ok(total)
    }
}

fn normalize_path(p: &str) -> String {
    if p.starts_with('/') {
        p.to_string()
    } else {
        format!("/{}", p)
    }
}

// ─────────────────────────────────────────────────────────────
//  hammer what <file>
// ─────────────────────────────────────────────────────────────

pub fn cmd_what(args: &[String]) -> Result<()> {
    let path_str = args
        .first()
        .ok_or_else(|| anyhow::anyhow!("Użycie: hammer what <ścieżka-pliku>"))?;

    println!();
    println!(
        "  {}  Która paczka dostarcza {}?",
        "⬡".bright_cyan().bold(),
        path_str.bold()
    );
    println!("  {}", "─".repeat(65).dimmed());

    let idx = FileIndex::open()?;

    // Przy pierwszym uruchomieniu na systemie < 0.5 odbuduj indeks
    if idx.is_empty() {
        println!(
            "  {} Indeks plików jest pusty — odbudowuję ze store (jednorazowo)…",
            "·".yellow()
        );
        let n = idx.rebuild_all()?;
        println!("  {} Zindeksowano {} plików.", "✔".bright_green(), n);
    }

    // 1. Dokładne dopasowanie
    let exact = idx.lookup(path_str)?;
    if !exact.is_empty() {
        for pkg in &exact {
            println!("  {} {} → {}", "✔".bright_green(), path_str.bold(), pkg.cyan().bold());
        }
        println!();
        return Ok(());
    }

    // 2. Fuzzy match jeśli brak dokładnego
    let fuzzy = idx.lookup_fuzzy(path_str)?;
    if fuzzy.is_empty() {
        println!(
            "  {} Żadna zainstalowana paczka nie dostarcza '{}'.",
            "·".dimmed(),
            path_str.bold()
        );
        println!(
            "  Wskazówka: spróbuj {} aby sprawdzić dostępne paczki.",
            "hammer search".cyan()
        );
    } else {
        println!("  {} Brak dokładnego trafienia, podobne pliki:", "·".yellow());
        println!();
        for (file, pkg) in &fuzzy {
            println!("  {} {}  ({})", "·".dimmed(), file.bold(), pkg.cyan());
        }
    }

    println!();
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  hammer what --rebuild
// ─────────────────────────────────────────────────────────────

pub fn cmd_what_rebuild() -> Result<()> {
    println!();
    println!("  {}  Odbudowywanie indeksu plików…", "⬡".bright_cyan().bold());
    let idx = FileIndex::open()?;
    let n   = idx.rebuild_all()?;
    println!("  {} Zindeksowano {} plików.", "✔".bright_green(), n);
    println!();
    Ok(())
}
