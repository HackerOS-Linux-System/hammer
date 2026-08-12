use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::package::Package;
use crate::build_mode;

/// Resolved at runtime so normal-mode uses /var/lib/hammer/
pub fn db_path() -> std::path::PathBuf {
    build_mode::db_path()
}

/// Legacy constant kept for compatibility — prefer `db_path()` fn.
#[cfg(not(feature = "normal-mode"))]
pub const DB_PATH: &str = "/hammer/db/hammer.db";
#[cfg(feature = "normal-mode")]
pub const DB_PATH: &str = "/var/lib/hammer/hammer.db";

// ─────────────────────────────────────────────────────────────
//  InstalledPackage
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPackage {
    pub name:              String,
    pub version:           String,
    pub architecture:      String,
    pub installed_size_kb: u64,
    pub section:           Option<String>,
    pub maintainer:        Option<String>,
    pub description_short: Option<String>,
    pub installed_at:      DateTime<Utc>,
    pub reason:            InstallReason,
    pub store_hash:        String,
    pub depends:           Option<String>,
    pub recommends:        Option<String>,
    /// `Multi-Arch:` value as published by the package (`same`, `foreign`,
    /// `allowed`, or `None` meaning "no"/absent). See `pkg::multi_arch`.
    pub multi_arch:        Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum InstallReason { User, Dependency }

impl InstallReason {
    pub fn as_str(&self) -> &'static str {
        match self { InstallReason::User => "user", InstallReason::Dependency => "dep" }
    }
    pub fn from_str(s: &str) -> Self {
        if s == "dep" { InstallReason::Dependency } else { InstallReason::User }
    }
}

// ─────────────────────────────────────────────────────────────
//  HistoryEntry
//
//  Field names used in ui.rs:
//    e.action, e.package, e.old_ver, e.new_ver, e.generation, e.timestamp
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub id:         i64,
    /// "install" | "remove" | "upgrade"
    pub action:     String,
    pub package:    String,
    pub old_ver:    Option<String>,
    pub new_ver:    Option<String>,
    /// Generation number when this action occurred
    pub generation: u32,
    pub timestamp:  DateTime<Utc>,
}

// ─────────────────────────────────────────────────────────────
//  InstalledDb
// ─────────────────────────────────────────────────────────────

pub struct InstalledDb {
    conn: Connection,
    /// Whether queries should also consult `/var/lib/dpkg/status` for
    /// packages hammer doesn't know about itself. `true` only for the
    /// real, on-disk database (`open()`/`open_at()`) — `open_in_memory()`
    /// is explicitly for isolated use (tests, and `hammer oci`'s
    /// per-rootfs CDCL resolver bridge, which seeds its own synthetic
    /// "installed" state and must not have the *host's* dpkg status
    /// silently mixed in on top of that).
    use_dpkg_fallback: bool,
}

impl InstalledDb {
    pub fn open() -> Result<Self> {
        let resolved = db_path();
        let path = resolved.as_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
            .with_context(|| format!("Cannot create {}", parent.display()))?;
        }
        let conn = Connection::open(path)
        .with_context(|| format!("Cannot open database {}", path.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let db = InstalledDb { conn, use_dpkg_fallback: true };
        db.migrate()?;
        Ok(db)
    }

    pub fn open_at(path: &str) -> Result<Self> {
        let p = std::path::Path::new(path);
        if let Some(parent) = p.parent() { std::fs::create_dir_all(parent)?; }
        let conn = rusqlite::Connection::open(p)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let db = InstalledDb { conn, use_dpkg_fallback: true };
        db.migrate()?;
        Ok(db)
    }

    /// Opens a private, in-memory database — never touches disk. Used by
    /// tests that need a real `InstalledDb` (migrations run, real SQL)
    /// without side effects on the host filesystem or interference between
    /// parallel test runs sharing a single on-disk DB path.
    pub fn open_in_memory() -> Result<Self> {
        let conn = rusqlite::Connection::open_in_memory()?;
        let db = InstalledDb { conn, use_dpkg_fallback: false };
        db.migrate()?;
        Ok(db)
    }


    // ── Queries ───────────────────────────────────────────────
    //
    // Every query here checks hammer's own sqlite `installed` table
    // first, then falls back to (or, for `list_all`, merges with) the
    // real system's `/var/lib/dpkg/status` via `dpkg_status` — see that
    // module's docs for why. Hammer's own record always wins when a
    // package is known to both, since it's the more authoritative source
    // for anything hammer itself is tracking (accurate `store_hash`,
    // `reason`, etc); dpkg only fills in packages hammer has never
    // touched itself.

    pub fn is_installed(&self, name: &str) -> bool {
        let in_own_db = self.conn.query_row(
            "SELECT 1 FROM installed WHERE name = ?1",
            params![name], |_| Ok(true),
        ).unwrap_or(false);
        in_own_db || (self.use_dpkg_fallback && crate::dpkg_status::is_installed(name))
    }

    pub fn get(&self, name: &str) -> Option<InstalledPackage> {
        let own: Option<InstalledPackage> = self.conn.query_row(
            "SELECT name,version,architecture,installed_size_kb,section,maintainer,
            description_short,installed_at,reason,store_hash,depends,recommends,multi_arch
            FROM installed WHERE name = ?1",
            params![name], row_to_installed,
        ).ok();
        own.or_else(|| if self.use_dpkg_fallback { crate::dpkg_status::get(name) } else { None })
    }

    pub fn list_all(&self) -> Result<Vec<InstalledPackage>> {
        let mut stmt = self.conn.prepare(
            "SELECT name,version,architecture,installed_size_kb,section,maintainer,
            description_short,installed_at,reason,store_hash,depends,recommends,multi_arch
            FROM installed ORDER BY name"
        )?;
        let rows = stmt.query_map([], row_to_installed)?;
        let mut own: Vec<InstalledPackage> = rows.filter_map(|r| r.ok()).collect();

        if self.use_dpkg_fallback {
            let known: std::collections::HashSet<String> = own.iter().map(|p| p.name.clone()).collect();
            own.extend(
                crate::dpkg_status::read_all()
                    .into_iter()
                    .filter(|p| !known.contains(&p.name))
            );
            own.sort_by(|a, b| a.name.cmp(&b.name));
        }
        Ok(own)
    }

    pub fn list_user_installed(&self) -> Result<Vec<InstalledPackage>> {
        let mut stmt = self.conn.prepare(
            "SELECT name,version,architecture,installed_size_kb,section,maintainer,
            description_short,installed_at,reason,store_hash,depends,recommends,multi_arch
            FROM installed WHERE reason = 'user' ORDER BY name"
        )?;
        let rows = stmt.query_map([], row_to_installed)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn count(&self) -> usize {
        self.conn.query_row("SELECT COUNT(*) FROM installed", [], |r| r.get::<_, i64>(0))
        .unwrap_or(0) as usize
    }

    // ── Mutations ─────────────────────────────────────────────

    pub fn record_install(
        &self,
        pkg:        &Package,
        reason:     InstallReason,
        store_hash: &str,
        gen:        u32,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT OR REPLACE INTO installed
            (name,version,architecture,installed_size_kb,section,maintainer,
                          description_short,installed_at,reason,store_hash,depends,recommends,multi_arch)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                          params![
                              pkg.name, pkg.version, pkg.architecture,
                          pkg.installed_size_kb.unwrap_or(0),
                          pkg.section, pkg.maintainer, pkg.description_short,
                          now, reason.as_str(), store_hash,
                          pkg.depends, pkg.recommends, pkg.multi_arch,
                          ],
        )?;
        self.conn.execute(
            "INSERT INTO history (action,package,old_ver,new_ver,generation,timestamp)
        VALUES ('install',?1,NULL,?2,?3,?4)",
                          params![pkg.name, pkg.version, gen, now],
        )?;
        Ok(())
    }

    pub fn record_upgrade(&self, old_ver: &str, pkg: &Package, store_hash: &str, gen: u32) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT OR REPLACE INTO installed
            (name,version,architecture,installed_size_kb,section,maintainer,
                          description_short,installed_at,reason,store_hash,depends,recommends,multi_arch)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,
                          COALESCE((SELECT reason FROM installed WHERE name=?1),'user'),
                          ?9,?10,?11,?12)",
                          params![
                              pkg.name, pkg.version, pkg.architecture,
                          pkg.installed_size_kb.unwrap_or(0),
                          pkg.section, pkg.maintainer, pkg.description_short,
                          now, store_hash, pkg.depends, pkg.recommends, pkg.multi_arch,
                          ],
        )?;
        self.conn.execute(
            "INSERT INTO history (action,package,old_ver,new_ver,generation,timestamp)
        VALUES ('upgrade',?1,?2,?3,?4,?5)",
                          params![pkg.name, old_ver, pkg.version, gen, now],
        )?;
        Ok(())
    }

    pub fn record_remove(&self, name: &str, version: &str, gen: u32) -> Result<()> {
        self.conn.execute("DELETE FROM installed WHERE name = ?1", params![name])?;
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO history (action,package,old_ver,new_ver,generation,timestamp)
        VALUES ('remove',?1,?2,NULL,?3,?4)",
                          params![name, version, gen, now],
        )?;
        Ok(())
    }

    pub fn history(&self, limit: usize) -> Result<Vec<HistoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,action,package,old_ver,new_ver,generation,timestamp
            FROM history ORDER BY id DESC LIMIT ?1"
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            let ts: String = row.get(6)?;
            Ok(HistoryEntry {
                id:         row.get(0)?,
               action:     row.get(1)?,
               package:    row.get(2)?,
               old_ver:    row.get(3)?,
               new_ver:    row.get(4)?,
               generation: row.get::<_, i64>(5)? as u32,
               timestamp:  DateTime::parse_from_rfc3339(&ts)
               .map(|d| d.with_timezone(&Utc))
               .unwrap_or_else(|_| Utc::now()),
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }
}

fn row_to_installed(row: &rusqlite::Row) -> rusqlite::Result<InstalledPackage> {
    let ts: String = row.get(7)?;
    Ok(InstalledPackage {
        name:              row.get(0)?,
       version:           row.get(1)?,
       architecture:      row.get(2)?,
       installed_size_kb: row.get::<_, i64>(3)? as u64,
       section:           row.get(4)?,
       maintainer:        row.get(5)?,
       description_short: row.get(6)?,
       installed_at:      DateTime::parse_from_rfc3339(&ts)
       .map(|d| d.with_timezone(&Utc))
       .unwrap_or_else(|_| Utc::now()),
       reason:            InstallReason::from_str(&row.get::<_, String>(8)?),
       store_hash:        row.get(9)?,
       depends:           row.get(10)?,
       recommends:        row.get(11)?,
       multi_arch:        row.get(12).ok(),
    })
}

// ─────────────────────────────────────────────────────────────
//  Schema migrations
// ─────────────────────────────────────────────────────────────

const CURRENT_SCHEMA_VERSION: u32 = 6;

impl InstalledDb {
    /// Run all pending schema migrations. Called on every open().
    pub fn migrate(&self) -> Result<()> {
        // Create schema_version table if missing (first-time setup)
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER NOT NULL
            );
        ")?;

        let version: u32 = self.conn
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap_or(0);

        if version < 1 { self.migrate_v1()?; }
        if version < 2 { self.migrate_v2()?; }
        if version < 3 { self.migrate_v3()?; }
        if version < 4 { self.migrate_v4()?; }
        if version < 5 { self.migrate_v5()?; }
        if version < 6 { self.migrate_v6()?; }

        // Write current version
        self.conn.execute_batch(&format!(
            "DELETE FROM schema_version;
             INSERT INTO schema_version VALUES ({});",
            CURRENT_SCHEMA_VERSION
        ))?;
        Ok(())
    }

    /// v1 — baseline tables (installed, history)
    fn migrate_v1(&self) -> Result<()> {
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS installed (
                name              TEXT PRIMARY KEY,
                version           TEXT NOT NULL,
                architecture      TEXT NOT NULL DEFAULT 'amd64',
                installed_size_kb INTEGER NOT NULL DEFAULT 0,
                section           TEXT,
                maintainer        TEXT,
                description_short TEXT,
                installed_at      TEXT NOT NULL,
                reason            TEXT NOT NULL DEFAULT 'user',
                store_hash        TEXT NOT NULL DEFAULT '',
                depends           TEXT,
                recommends        TEXT
            );
            CREATE TABLE IF NOT EXISTS history (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                action     TEXT NOT NULL,
                package    TEXT NOT NULL,
                old_ver    TEXT,
                new_ver    TEXT,
                generation INTEGER NOT NULL DEFAULT 0,
                timestamp  TEXT NOT NULL
            );
        ")?;
        Ok(())
    }

    /// v2 — add pins and holds tables
    fn migrate_v2(&self) -> Result<()> {
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS pins (
                name        TEXT PRIMARY KEY,
                \"constraint\" TEXT NOT NULL,
                priority    INTEGER NOT NULL DEFAULT 100,
                note        TEXT
            );
            CREATE TABLE IF NOT EXISTS holds (
                name       TEXT PRIMARY KEY,
                held_at    TEXT NOT NULL
            );
        ")?;
        Ok(())
    }

    /// v3 — add indexes for common query patterns
    fn migrate_v3(&self) -> Result<()> {
        self.conn.execute_batch("
            CREATE INDEX IF NOT EXISTS idx_installed_section ON installed(section);
            CREATE INDEX IF NOT EXISTS idx_installed_reason  ON installed(reason);
            CREATE INDEX IF NOT EXISTS idx_history_package   ON history(package);
            CREATE INDEX IF NOT EXISTS idx_history_action    ON history(action);
            CREATE INDEX IF NOT EXISTS idx_history_timestamp ON history(timestamp);
        ")?;
        Ok(())
    }

    /// v5 — add file_index table (hammer what <file>, 0.5)
    fn migrate_v5(&self) -> Result<()> {
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

    /// v4 — add conffiles tracking table
    fn migrate_v4(&self) -> Result<()> {
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS conffiles (
                package    TEXT NOT NULL,
                path       TEXT NOT NULL,
                orig_hash  TEXT NOT NULL,
                curr_hash  TEXT,
                modified   INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (package, path)
            );
            CREATE INDEX IF NOT EXISTS idx_conffiles_pkg  ON conffiles(package);
            CREATE INDEX IF NOT EXISTS idx_conffiles_path ON conffiles(path);
        ")?;
        Ok(())
    }

    /// v6 — add multi_arch column to `installed` (Multi-Arch: same/foreign/
    /// allowed support, see `pkg::multi_arch`).
    fn migrate_v6(&self) -> Result<()> {
        // SQLite has no `ADD COLUMN IF NOT EXISTS` on older versions still
        // in the wild, so probe `pragma_table_info` first — this migration
        // must stay idempotent like the others (schema_version is only
        // bumped after all migrations run, so a crash mid-migration could
        // otherwise re-run this and hit "duplicate column name").
        let has_column: bool = self.conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('installed') WHERE name = 'multi_arch'",
            [], |r| r.get::<_, i64>(0),
        ).map(|c| c > 0).unwrap_or(false);

        if !has_column {
            self.conn.execute_batch(
                "ALTER TABLE installed ADD COLUMN multi_arch TEXT;"
            )?;
        }
        Ok(())
    }

    // ── Pin management (via DB) ───────────────────────────────

    pub fn pin_package(&self, name: &str, constraint: &str, priority: i32) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO pins (name, \"constraint\", priority, note)
             VALUES (?1, ?2, ?3, NULL)",
            params![name, constraint, priority],
        )?;
        Ok(())
    }

    pub fn unpin_package(&self, name: &str) -> Result<()> {
        self.conn.execute("DELETE FROM pins WHERE name = ?1", params![name])?;
        Ok(())
    }

    pub fn get_pin(&self, name: &str) -> Option<(String, i32)> {
        self.conn.query_row(
            "SELECT \"constraint\", priority FROM pins WHERE name = ?1",
            params![name],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i32>(1)?)),
        ).ok()
    }

    pub fn list_pins(&self) -> Result<Vec<(String, String, i32)>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, \"constraint\", priority FROM pins ORDER BY name"
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?, r.get::<_,i32>(2)?))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    // ── Hold management ───────────────────────────────────────

    pub fn hold_package(&self, name: &str) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT OR REPLACE INTO holds (name, held_at) VALUES (?1, ?2)",
            params![name, now],
        )?;
        Ok(())
    }

    pub fn unhold_package(&self, name: &str) -> Result<()> {
        self.conn.execute("DELETE FROM holds WHERE name = ?1", params![name])?;
        Ok(())
    }

    pub fn is_held(&self, name: &str) -> bool {
        self.conn.query_row(
            "SELECT 1 FROM holds WHERE name = ?1",
            params![name], |_| Ok(true),
        ).unwrap_or(false)
    }

    pub fn list_holds(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT name FROM holds ORDER BY name")?;
        let rows = stmt.query_map([], |r| r.get::<_,String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    // ── Conffile tracking ─────────────────────────────────────

    pub fn register_conffile(&self, pkg: &str, path: &str, hash: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO conffiles (package, path, orig_hash, curr_hash, modified)
             VALUES (?1, ?2, ?3, ?3, 0)",
            params![pkg, path, hash],
        )?;
        Ok(())
    }

    pub fn update_conffile_hash(&self, path: &str, curr_hash: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE conffiles SET curr_hash = ?1, modified = (orig_hash != ?1)
             WHERE path = ?2",
            params![curr_hash, path],
        )?;
        Ok(())
    }

    pub fn list_modified_conffiles(&self, pkg: Option<&str>) -> Result<Vec<(String, String)>> {
        let (sql, param): (&str, Box<dyn rusqlite::types::ToSql>) = if let Some(p) = pkg {
            ("SELECT package, path FROM conffiles WHERE modified = 1 AND package = ?1",
             Box::new(p.to_string()))
        } else {
            ("SELECT package, path FROM conffiles WHERE modified = 1 ORDER BY package",
             Box::new(""))
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![param], |r| {
            Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Usuń wszystkie conffiles dla danej paczki (wywoływane przez hammer undo).
    pub fn remove_conffiles_for_package(&self, pkg: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM conffiles WHERE package = ?1",
            params![pkg],
        )?;
        Ok(())
    }

    // ── Maintenance ───────────────────────────────────────────

    pub fn vacuum(&self) -> Result<()> {
        self.conn.execute_batch("VACUUM; ANALYZE;")?;
        Ok(())
    }

    /// List packages installed explicitly by the user (reason = "user").

    /// Change the install reason for a package (user/dependency).
    pub fn set_reason(&self, name: &str, reason: InstallReason) -> Result<()> {
        let r = match reason {
            InstallReason::User       => "user",
            InstallReason::Dependency => "dependency",
        };
        self.conn.execute(
            "UPDATE installed SET reason = ?1 WHERE name = ?2",
            params![r, name],
        )?;
        Ok(())
    }

    fn row_to_pkg(&self, r: &rusqlite::Row<'_>) -> rusqlite::Result<InstalledPackage> {
        Ok(InstalledPackage {
            name:              r.get(0)?,
            version:           r.get(1)?,
            architecture:      r.get(2)?,
            installed_size_kb: r.get(3).unwrap_or(0),
            section:           r.get(4).ok(),
            maintainer:        r.get(5).ok(),
            description_short: r.get(6).ok(),
            installed_at:      r.get(7).and_then(|s: String| {
                                   chrono::DateTime::parse_from_rfc3339(&s)
                                       .map(|dt| dt.with_timezone(&chrono::Utc))
                                       .map_err(|_| rusqlite::Error::InvalidQuery)
                               }).unwrap_or_else(|_| chrono::Utc::now()),
            reason:            match r.get::<_, String>(8).as_deref() {
                                   Ok("dependency") => InstallReason::Dependency,
                                   _               => InstallReason::User,
                               },
            store_hash:        r.get(9).unwrap_or_default(),
            depends:           r.get(10).ok(),
            recommends:        r.get(11).ok(),
            multi_arch:        r.get(12).ok(),
        })
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        self.conn.execute("DELETE FROM installed WHERE name = ?1", params![name])?;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────
//  Unified source of truth: SQLite ↔ JSON
//
//  SQLite is authoritative.  The JSON snapshot is:
//    • Written atomically after every mutating operation
//    • Read on startup when the SQLite file is absent/corrupt
//      (disaster recovery + human-readable backup)
//    • Used by external tooling (jq, scripts, HammerStore UI)
//
//  Path: /hammer/db/installed.json
// ─────────────────────────────────────────────────────────────

pub const JSON_PATH: &str = "/hammer/db/installed.json";

/// Mode-aware counterpart of [`JSON_PATH`] — `/hammer/db/installed.json`
/// in atomic mode, `/var/lib/hammer/db/installed.json` in normal-mode
/// (via `build_mode::db_dir()`, the same directory the sqlite DB itself
/// lives in for that mode). [`JSON_PATH`] is kept as-is since it's part
/// of the public "used by external tooling" contract documented above
/// and atomic mode is the default; this is what [`InstalledDb::export_json`]
/// actually uses.
pub fn json_path() -> std::path::PathBuf {
    crate::build_mode::db_dir().join("installed.json")
}

/// Snapshot written to / read from the JSON file.
#[derive(Debug, Serialize, Deserialize)]
pub struct DbSnapshot {
    pub schema_version: u32,
    pub exported_at:    String,
    pub packages:       Vec<InstalledPackage>,
}

impl InstalledDb {
    // ── JSON export ───────────────────────────────────────────

    /// Export all installed packages to the JSON side-car file.
    /// Called automatically after every mutation.
    pub fn export_json(&self) -> Result<()> {
        let path = json_path();
        self.export_json_to(&path.to_string_lossy())
    }

    pub fn export_json_to(&self, path: &str) -> Result<()> {
        let packages = self.list_all()?;
        let snap = DbSnapshot {
            schema_version: CURRENT_SCHEMA_VERSION,
            exported_at:    Utc::now().to_rfc3339(),
            packages,
        };
        let json = serde_json::to_string_pretty(&snap)?;
        // Atomic write: write to *.tmp then rename. `JSON_PATH` defaults
        // to the atomic-mode layout ("/hammer/db/...") — in normal-mode
        // (or any mode where that directory was never created) the write
        // below would otherwise fail with "No such file or directory" on
        // every single call, since std::fs::write doesn't create parent
        // directories. This is a pure export/recovery side-car, so it's
        // safe and correct to just ensure its directory exists here
        // rather than making every one of export_json's callers reason
        // about which mode's directory layout applies.
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Creating {}", parent.display()))?;
        }
        let tmp = format!("{}.tmp", path);
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    // ── JSON import / recovery ────────────────────────────────

    /// Import a JSON snapshot into SQLite.  Used for disaster recovery
    /// when the SQLite file is absent or corrupt.  Safe to call on an
    /// empty database — it will not duplicate existing rows.
    pub fn import_json(&self, path: &str) -> Result<usize> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("Reading {}", path))?;
        let snap: DbSnapshot = serde_json::from_str(&raw)
            .context("Parsing JSON snapshot")?;
        let mut imported = 0usize;
        for ip in &snap.packages {
            // Skip packages already in SQLite
            if self.is_installed(&ip.name) { continue; }
            let ts = ip.installed_at.to_rfc3339();
            self.conn.execute(
                "INSERT OR IGNORE INTO installed
                (name,version,architecture,installed_size_kb,section,maintainer,
                 description_short,installed_at,reason,store_hash,depends,recommends)
                VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    ip.name, ip.version, ip.architecture, ip.installed_size_kb as i64,
                    ip.section, ip.maintainer, ip.description_short, ts,
                    ip.reason.as_str(), ip.store_hash, ip.depends, ip.recommends,
                ],
            )?;
            imported += 1;
        }
        Ok(imported)
    }

    /// Try to recover from a corrupt/missing SQLite by importing the JSON
    /// side-car.  Returns Ok(0) if nothing to recover.
    pub fn recover_from_json() -> Result<usize> {
        let json_path = std::path::Path::new(JSON_PATH);
        if !json_path.exists() { return Ok(0); }
        let db = Self::open()?;
        if db.count() > 0 {
            // SQLite is healthy — no recovery needed, just re-sync JSON
            db.export_json()?;
            return Ok(0);
        }
        let imported = db.import_json(JSON_PATH)?;
        eprintln!("[hammer] Recovered {} packages from JSON snapshot.", imported);
        Ok(imported)
    }

    /// Open the database, recovering from JSON if SQLite is empty/missing.
    pub fn open_with_recovery() -> Result<Self> {
        let db = Self::open()?;
        if db.count() == 0 {
            let json_path = std::path::Path::new(JSON_PATH);
            if json_path.exists() {
                let n = db.import_json(JSON_PATH)?;
                if n > 0 {
                    eprintln!("[hammer] Recovered {} packages from JSON backup.", n);
                }
            }
        }
        Ok(db)
    }

    // ── Auto-export wrappers ──────────────────────────────────
    //
    //  Wrap the three mutating methods so the JSON snapshot is always
    //  kept in sync.  Callers use these instead of the raw methods.

    pub fn record_install_and_sync(
        &self,
        pkg:        &Package,
        reason:     InstallReason,
        store_hash: &str,
        gen:        u32,
    ) -> Result<()> {
        self.record_install(pkg, reason, store_hash, gen)?;
        self.export_json().unwrap_or_else(|e| eprintln!("[db] JSON export failed: {}", e));
        Ok(())
    }

    pub fn record_upgrade_and_sync(
        &self,
        old_ver:    &str,
        pkg:        &Package,
        store_hash: &str,
        gen:        u32,
    ) -> Result<()> {
        self.record_upgrade(old_ver, pkg, store_hash, gen)?;
        self.export_json().unwrap_or_else(|e| eprintln!("[db] JSON export failed: {}", e));
        Ok(())
    }

    pub fn record_remove_and_sync(
        &self,
        name:    &str,
        version: &str,
        gen:     u32,
    ) -> Result<()> {
        self.record_remove(name, version, gen)?;
        self.export_json().unwrap_or_else(|e| eprintln!("[db] JSON export failed: {}", e));
        Ok(())
    }

    // ── Status ────────────────────────────────────────────────

    /// Return the mtime of the JSON snapshot (for cache-busting).
    pub fn json_mtime() -> Option<std::time::SystemTime> {
        std::fs::metadata(JSON_PATH).ok()?.modified().ok()
    }

    /// Validate that JSON snapshot agrees with SQLite.
    /// Returns (only_in_sqlite, only_in_json).
    pub fn validate_json_sync(&self) -> Result<(Vec<String>, Vec<String>)> {
        use std::collections::HashSet;
        let sqlite_names: HashSet<String> = self.list_all()?
            .into_iter().map(|p| p.name).collect();
        let json_path = std::path::Path::new(JSON_PATH);
        if !json_path.exists() {
            return Ok((sqlite_names.into_iter().collect(), vec![]));
        }
        let raw = std::fs::read_to_string(json_path)?;
        let snap: DbSnapshot = serde_json::from_str(&raw)?;
        let json_names: HashSet<String> = snap.packages.into_iter().map(|p| p.name).collect();
        let only_sqlite: Vec<String> = sqlite_names.difference(&json_names)
            .cloned().collect();
        let only_json: Vec<String> = json_names.difference(&sqlite_names)
            .cloned().collect();
        Ok((only_sqlite, only_json))
    }
}
