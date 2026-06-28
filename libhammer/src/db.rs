use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::package::Package;

// ─────────────────────────────────────────────────────────────
//  Types
// ─────────────────────────────────────────────────────────────

/// Why a package was installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstallReason {
    /// Explicitly requested by the user.
    User,
    /// Pulled in as a dependency.
    Dependency,
}

impl InstallReason {
    /// String representation stored in the database.
    pub fn as_str(self) -> &'static str {
        match self { Self::User => "user", Self::Dependency => "dep" }
    }

    /// Parse from DB string.
    pub fn from_str(s: &str) -> Self {
        if s == "user" { Self::User } else { Self::Dependency }
    }
}

/// A package as recorded in the installed DB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPackage {
    /// Package name.
    pub name:              String,
    /// Installed version.
    pub version:           String,
    /// Architecture.
    pub architecture:      String,
    /// Installed size in KB.
    pub installed_size_kb: u64,
    /// Section (optional).
    pub section:           Option<String>,
    /// Maintainer (optional).
    pub maintainer:        Option<String>,
    /// One-line description (optional).
    pub description_short: Option<String>,
    /// Timestamp of installation.
    pub installed_at:      DateTime<Utc>,
    /// Install reason.
    pub reason:            InstallReason,
    /// Store hash (Hammer-store path, empty in normal-mode installs).
    pub store_hash:        String,
    /// `Depends` field (raw).
    pub depends:           Option<String>,
    /// `Recommends` field (raw).
    pub recommends:        Option<String>,
}

// ─────────────────────────────────────────────────────────────
//  InstalledDb
// ─────────────────────────────────────────────────────────────

/// SQLite database of installed packages.
pub struct InstalledDb {
    conn: Connection,
}

const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS installed (
    name               TEXT    NOT NULL PRIMARY KEY,
    version            TEXT    NOT NULL,
    architecture       TEXT    NOT NULL DEFAULT 'amd64',
    installed_size_kb  INTEGER NOT NULL DEFAULT 0,
    section            TEXT,
    maintainer         TEXT,
    description_short  TEXT,
    installed_at       TEXT    NOT NULL,
    reason             TEXT    NOT NULL DEFAULT 'dep',
    store_hash         TEXT    NOT NULL DEFAULT '',
    depends            TEXT,
    recommends         TEXT
);
CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);
"#;

impl InstalledDb {
    /// Open (or create) the database at `path`.
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("Opening InstalledDb at {}", path))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        Ok(InstalledDb { conn })
    }

    /// Open an in-memory database (useful for testing).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Ok(InstalledDb { conn })
    }

    /// Apply schema migrations.
    pub fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(SCHEMA_V1)?;
        Ok(())
    }

    // ── Queries ───────────────────────────────────────────────

    /// Return `true` if `name` is in the installed set.
    pub fn is_installed(&self, name: &str) -> bool {
        self.conn.query_row(
            "SELECT 1 FROM installed WHERE name = ?1",
            params![name], |_| Ok(()))
            .is_ok()
    }

    /// Fetch the installed record for `name`, or `None`.
    pub fn get(&self, name: &str) -> Option<InstalledPackage> {
        self.conn.query_row(
            "SELECT name,version,architecture,installed_size_kb,section,maintainer,
                    description_short,installed_at,reason,store_hash,depends,recommends
             FROM installed WHERE name = ?1",
            params![name],
            |r| Self::row_to_pkg(r),
        ).ok()
    }

    /// List all installed packages.
    pub fn list_all(&self) -> Result<Vec<InstalledPackage>> {
        let mut stmt = self.conn.prepare(
            "SELECT name,version,architecture,installed_size_kb,section,maintainer,
                    description_short,installed_at,reason,store_hash,depends,recommends
             FROM installed ORDER BY name"
        )?;
        let rows = stmt.query_map([], |r| Self::row_to_pkg(r))?;
        rows.map(|r| r.map_err(anyhow::Error::from)).collect()
    }

    /// Total number of installed packages.
    pub fn count(&self) -> usize {
        self.conn.query_row("SELECT COUNT(*) FROM installed", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as usize
    }

    // ── Mutations ─────────────────────────────────────────────

    /// Record a fresh package installation.
    pub fn record_install(
        &self,
        pkg:        &Package,
        reason:     InstallReason,
        store_hash: &str,
        _gen:       u32,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT OR REPLACE INTO installed
             (name,version,architecture,installed_size_kb,section,maintainer,
              description_short,installed_at,reason,store_hash,depends,recommends)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                pkg.name, pkg.version, pkg.architecture, pkg.installed_size_kb as i64,
                pkg.section, pkg.maintainer, pkg.description_short,
                now, reason.as_str(), store_hash, pkg.depends, pkg.recommends,
            ],
        )?;
        Ok(())
    }

    /// Record a package upgrade.
    pub fn record_upgrade(
        &self,
        _old_version: &str,
        pkg:          &Package,
        store_hash:   &str,
        gen:          u32,
    ) -> Result<()> {
        // Re-use record_install (INSERT OR REPLACE handles the update)
        self.record_install(pkg, InstallReason::User, store_hash, gen)
    }

    /// Record a package removal.
    pub fn record_remove(&self, name: &str, _version: &str, _gen: u32) -> Result<()> {
        self.conn.execute("DELETE FROM installed WHERE name = ?1", params![name])?;
        Ok(())
    }

    /// Change the install reason for a package.
    pub fn set_reason(&self, name: &str, reason: InstallReason) -> Result<()> {
        self.conn.execute(
            "UPDATE installed SET reason = ?1 WHERE name = ?2",
            params![reason.as_str(), name],
        )?;
        Ok(())
    }

    // ── Internal helpers ──────────────────────────────────────

    fn row_to_pkg(r: &rusqlite::Row<'_>) -> rusqlite::Result<InstalledPackage> {
        let ts: String = r.get(7)?;
        let installed_at = ts.parse::<DateTime<Utc>>()
            .unwrap_or_else(|_| Utc::now());
        Ok(InstalledPackage {
            name:              r.get(0)?,
            version:           r.get(1)?,
            architecture:      r.get(2)?,
            installed_size_kb: r.get::<_, i64>(3)? as u64,
            section:           r.get(4)?,
            maintainer:        r.get(5)?,
            description_short: r.get(6)?,
            installed_at,
            reason: InstallReason::from_str(&r.get::<_, String>(8)?),
            store_hash:  r.get(9)?,
            depends:     r.get(10)?,
            recommends:  r.get(11)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::Package;

    fn test_pkg(name: &str) -> Package {
        Package {
            name:    name.into(),
            version: "1.0".into(),
            ..Default::default()
        }
    }

    #[test]
    fn basic_install_query() {
        let db = InstalledDb::open_in_memory().unwrap();
        db.migrate().unwrap();

        let pkg = test_pkg("curl");
        db.record_install(&pkg, InstallReason::User, "", 0).unwrap();

        assert!(db.is_installed("curl"));
        assert!(!db.is_installed("wget"));

        let got = db.get("curl").unwrap();
        assert_eq!(got.name, "curl");
        assert_eq!(got.reason, InstallReason::User);
    }

    #[test]
    fn list_all_ordered() {
        let db = InstalledDb::open_in_memory().unwrap();
        db.migrate().unwrap();
        db.record_install(&test_pkg("zsh"),  InstallReason::User, "", 0).unwrap();
        db.record_install(&test_pkg("bash"), InstallReason::Dependency, "", 0).unwrap();
        let all = db.list_all().unwrap();
        assert_eq!(all[0].name, "bash"); // alphabetical
        assert_eq!(all[1].name, "zsh");
    }

    #[test]
    fn remove() {
        let db = InstalledDb::open_in_memory().unwrap();
        db.migrate().unwrap();
        db.record_install(&test_pkg("vim"), InstallReason::User, "", 0).unwrap();
        assert!(db.is_installed("vim"));
        db.record_remove("vim", "1.0", 0).unwrap();
        assert!(!db.is_installed("vim"));
    }

    #[test]
    fn count() {
        let db = InstalledDb::open_in_memory().unwrap();
        db.migrate().unwrap();
        assert_eq!(db.count(), 0);
        db.record_install(&test_pkg("a"), InstallReason::User, "", 0).unwrap();
        db.record_install(&test_pkg("b"), InstallReason::Dependency, "", 0).unwrap();
        assert_eq!(db.count(), 2);
    }
}
