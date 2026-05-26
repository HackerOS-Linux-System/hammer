pub(crate) mod conflicts;
pub(crate) mod error;
pub(crate) mod provides;
pub(crate) mod sat;
pub(crate) mod version;

pub use error::SolverError;

use anyhow::Result;
use crate::cache::PackageCache;
use crate::db::InstalledDb;
use crate::package::Package;
use std::collections::HashMap;

// ─────────────────────────────────────────────────────────────
//  TransactionPlan
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct TransactionPlan {
    pub to_install:     Vec<Package>,
    pub to_upgrade:     Vec<Package>,
    pub to_remove:      Vec<String>,
    pub to_autoremove:  Vec<String>,
    pub upgrade_from:   HashMap<String, String>,
    pub download_bytes: u64,
    pub install_bytes:  u64,
    pub freed_bytes:    u64,
    pub warnings:       Vec<String>,
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
//  Solver
// ─────────────────────────────────────────────────────────────

pub struct Solver<'a> {
    pub(crate) cache: &'a PackageCache,
    pub(crate) db:    &'a InstalledDb,
}

impl<'a> Solver<'a> {
    pub fn new(cache: &'a PackageCache, db: &'a InstalledDb) -> Self {
        Solver { cache, db }
    }

    pub fn resolve_install(&self, names: &[String], no_recommends: bool) -> Result<TransactionPlan> {
        sat::resolve_install(self, names, no_recommends)
    }
    pub fn resolve_reinstall(&self, names: &[String]) -> Result<TransactionPlan> {
        sat::resolve_reinstall(self, names)
    }
    pub fn resolve_remove(&self, names: &[String]) -> Result<TransactionPlan> {
        sat::resolve_remove(self, names)
    }
    pub fn resolve_upgrade(&self) -> Result<TransactionPlan> {
        sat::resolve_upgrade(self)
    }
    pub fn resolve_dist_upgrade(&self) -> Result<TransactionPlan> {
        sat::resolve_dist_upgrade(self)
    }
    pub fn resolve_autoremove(&self) -> Result<TransactionPlan> {
        sat::resolve_autoremove(self)
    }
    pub fn resolve_fix_broken(&self) -> Result<TransactionPlan> {
        sat::resolve_fix_broken(self)
    }

    pub(crate) fn find_similar(&self, name: &str) -> Vec<String> {
        let q = name.to_lowercase();
        self.cache.search(&q).iter().take(6).map(|p| p.name.clone()).collect()
    }
}
