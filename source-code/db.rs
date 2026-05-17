use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::package::Package;

pub const DB_PATH: &str = "/hammer/db/hammer.db";

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
    /// Store hash for this package (links to /hammer/store/<name>-<ver>-<hash>/)
    pub store_hash:        String,
    pub depends:           Option<String>,
    pub recommends:        Option<String>,
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
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub id:         i64,
    pub action:     String,
    pub package:    String,
    pub old_ver:    Option<String>,
    pub new_ver:    Option<String>,
    pub generation: u32,
    pub timestamp:  DateTime<Utc>,
}

// ─────────────────────────────────────────────────────────────
//  InstalledDb
// ─────────────────────────────────────────────────────────────

pub struct InstalledDb {
    conn: Connection,
}

impl InstalledDb {
    pub fn open() -> Result<Self> {
        let path = Path::new(DB_PATH);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
            .with_context(|| format!("Cannot create {}", parent.display()))?;
        }
        let conn = Connection::open(path)
        .with_context(|| format!("Cannot open database {}", DB_PATH))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let db = InstalledDb { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch("
        CREATE TABLE IF NOT EXISTS installed (
            name              TEXT PRIMARY KEY,
            version           TEXT NOT NULL,
            architecture      TEXT NOT NULL,
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

        CREATE INDEX IF NOT EXISTS idx_history_ts ON history(timestamp DESC);
        ")?;
        Ok(())
    }

    // ──────────────────────────────────────────────────────────
    //  Queries
    // ──────────────────────────────────────────────────────────

    pub fn is_installed(&self, name: &str) -> bool {
        self.conn.query_row(
            "SELECT 1 FROM installed WHERE name = ?1",
            params![name], |_| Ok(true),
        ).unwrap_or(false)
    }

    pub fn get(&self, name: &str) -> Option<InstalledPackage> {
        self.conn.query_row(
            "SELECT name,version,architecture,installed_size_kb,section,maintainer,
            description_short,installed_at,reason,store_hash,depends,recommends
            FROM installed WHERE name = ?1",
            params![name], row_to_installed,
        ).ok()
    }

    pub fn list_all(&self) -> Result<Vec<InstalledPackage>> {
        let mut stmt = self.conn.prepare(
            "SELECT name,version,architecture,installed_size_kb,section,maintainer,
            description_short,installed_at,reason,store_hash,depends,recommends
            FROM installed ORDER BY name"
        )?;
        let rows = stmt.query_map([], row_to_installed)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn list_user_installed(&self) -> Result<Vec<InstalledPackage>> {
        let mut stmt = self.conn.prepare(
            "SELECT name,version,architecture,installed_size_kb,section,maintainer,
            description_short,installed_at,reason,store_hash,depends,recommends
            FROM installed WHERE reason = 'user' ORDER BY name"
        )?;
        let rows = stmt.query_map([], row_to_installed)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn count(&self) -> usize {
        self.conn.query_row("SELECT COUNT(*) FROM installed", [], |r| r.get::<_, i64>(0))
        .unwrap_or(0) as usize
    }

    // ──────────────────────────────────────────────────────────
    //  Mutations
    // ──────────────────────────────────────────────────────────

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
                          description_short,installed_at,reason,store_hash,depends,recommends)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                          params![
                              pkg.name, pkg.version, pkg.architecture,
                          pkg.installed_size_kb.unwrap_or(0),
                          pkg.section, pkg.maintainer, pkg.description_short,
                          now, reason.as_str(), store_hash,
                          pkg.depends, pkg.recommends,
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
                          description_short,installed_at,reason,store_hash,depends,recommends)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,
                          COALESCE((SELECT reason FROM installed WHERE name=?1),'user'),
                          ?9,?10,?11)",
                          params![
                              pkg.name, pkg.version, pkg.architecture,
                          pkg.installed_size_kb.unwrap_or(0),
                          pkg.section, pkg.maintainer, pkg.description_short,
                          now, store_hash, pkg.depends, pkg.recommends,
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
    })
}

impl InstalledDb {
    pub fn open_at(path: &str) -> anyhow::Result<Self> {
        let p = std::path::Path::new(path);
        if let Some(parent) = p.parent() { std::fs::create_dir_all(parent)?; }
        let conn = rusqlite::Connection::open(p)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let db = InstalledDb { conn };
        db.migrate()?;
        Ok(db)
    }
}
