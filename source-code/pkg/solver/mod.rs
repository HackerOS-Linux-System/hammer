mod conflicts;
mod error;
mod provides;
mod sat;
mod version;

pub use error::{SolverError, SolverProblem};

use anyhow::Result;
use std::collections::HashMap;

use crate::cache::PackageCache;
use crate::db::InstalledDb;
use crate::package::Package;

// ─────────────────────────────────────────────────────────────
//  TransactionPlan  — result of solver run
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct TransactionPlan {
    /// New packages to download and install
    pub to_install:    Vec<Package>,
    /// Packages to upgrade (already installed, newer version available)
    pub to_upgrade:    Vec<Package>,
    /// Package names to remove
    pub to_remove:     Vec<String>,
    /// Packages that were auto-installed and are now unused
    pub to_autoremove: Vec<String>,
    /// Old version for each package being upgraded: name → old_version
    pub upgrade_from:  HashMap<String, String>,
    /// Total bytes to download
    pub download_bytes: u64,
    /// Total installed size after transaction
    pub install_bytes:  u64,
    /// Bytes freed by removals
    pub freed_bytes:    u64,
    /// Non-fatal warnings (conflict hints, downgrade notices, etc.)
    pub warnings:       Vec<String>,
    /// Conflict descriptions shown to user before confirmation
    pub conflicts:      Vec<String>,
}

impl TransactionPlan {
    pub fn is_empty(&self) -> bool {
        self.to_install.is_empty()
        && self.to_upgrade.is_empty()
        && self.to_remove.is_empty()
        && self.to_autoremove.is_empty()
    }
}

// ─────────────────────────────────────────────────────────────
//  Solver facade
// ─────────────────────────────────────────────────────────────

pub struct Solver<'a> {
    pub(crate) cache:       &'a PackageCache,
    pub(crate) db:          &'a InstalledDb,
}

impl<'a> Solver<'a> {
    pub fn new(cache: &'a PackageCache, db: &'a InstalledDb) -> Self {
        Solver { cache, db }
    }

    // ── Install ───────────────────────────────────────────────

    pub fn resolve_install(
        &self,
        names:         &[String],
        no_recommends: bool,
    ) -> Result<TransactionPlan> {
        sat::resolve_install(self, names, no_recommends)
    }

    // ── Reinstall ─────────────────────────────────────────────

    pub fn resolve_reinstall(&self, names: &[String]) -> Result<TransactionPlan> {
        sat::resolve_reinstall(self, names)
    }

    // ── Remove ────────────────────────────────────────────────

    pub fn resolve_remove(&self, names: &[String]) -> Result<TransactionPlan> {
        sat::resolve_remove(self, names)
    }

    // ── Upgrade ───────────────────────────────────────────────

    pub fn resolve_upgrade(&self) -> Result<TransactionPlan> {
        sat::resolve_upgrade(self)
    }

    // ── Dist-upgrade ──────────────────────────────────────────

    pub fn resolve_dist_upgrade(&self) -> Result<TransactionPlan> {
        sat::resolve_dist_upgrade(self)
    }

    // ── Autoremove ────────────────────────────────────────────

    pub fn resolve_autoremove(&self) -> Result<TransactionPlan> {
        sat::resolve_autoremove(self)
    }

    // ── Fix-broken ────────────────────────────────────────────

    pub fn resolve_fix_broken(&self) -> Result<TransactionPlan> {
        sat::resolve_fix_broken(self)
    }

    // ── Helpers exposed to sub-modules ───────────────────────

    pub(crate) fn provides_map(&self) -> provides::ProvidesMap {
        provides::build(self.cache)
    }

    pub(crate) fn find_similar(&self, name: &str) -> Vec<String> {
        let q = name.to_lowercase();
        let mut r: Vec<_> = self.cache.search(&q)
        .iter().take(6).map(|p| p.name.clone()).collect();
        r.truncate(6);
        r
    }
}
