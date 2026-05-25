use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet, VecDeque};

use crate::cache::PackageCache;
use crate::db::InstalledDb;
use crate::package::{parse_dep_field, version_cmp, version_satisfies, Package};

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
    /// Conflict descriptions (non-fatal warnings shown to user)
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
    cache:        &'a PackageCache,
    db:           &'a InstalledDb,
    /// provides_map: virtual_name → list of real package names
    provides_map: HashMap<String, Vec<String>>,
}

impl<'a> Solver<'a> {
    pub fn new(cache: &'a PackageCache, db: &'a InstalledDb) -> Self {
        let provides_map = build_provides_map(cache);
        Solver { cache, db, provides_map }
    }

    // ── resolve_install ───────────────────────────────────────

    pub fn resolve_install(
        &self,
        names:         &[String],
        no_recommends: bool,
    ) -> Result<TransactionPlan> {
        let mut plan  = TransactionPlan::default();
        let mut seen: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, bool)> = VecDeque::new();

        for name in names {
            let name = name.split(':').next().unwrap_or(name).to_owned();
            // Resolve virtual package names
            let real = self.resolve_name(&name);
            if self.cache.get(&real).is_none() {
                // Try search for similar names
                let similar = self.find_similar(&name);
                if similar.is_empty() {
                    bail!(
                        "No match for package: '{}'\n  \
                         Hint: run `hammer sync` to refresh the package index.",
                        name
                    );
                } else {
                    bail!(
                        "No match for package: '{}'\n  \
                         Did you mean one of: {}?\n  \
                         Hint: run `hammer sync` to refresh the package index.",
                        name,
                        similar.join(", ")
                    );
                }
            }
            queue.push_back((real, true));
        }

        while let Some((name, explicit)) = queue.pop_front() {
            if seen.contains(&name) { continue; }
            seen.insert(name.clone());

            let avail = match self.cache.get(&name) {
                Some(p) => p.clone(),
                None => {
                    // Try virtual resolution
                    if let Some(real) = self.provides_map.get(&name)
                        .and_then(|v| v.first())
                    {
                        let real = real.clone();
                        if !seen.contains(&real) {
                            queue.push_back((real, false));
                        }
                    } else {
                        plan.warnings.push(format!(
                            "dependency '{}' not found in package index — skipped", name
                        ));
                    }
                    continue;
                }
            };

            let priority = avail.priority.as_deref().unwrap_or("");
            if !explicit && matches!(priority, "required" | "important" | "standard") {
                if !self.db.is_installed(&name) {
                    self.enqueue_deps(&avail, true, &mut queue);
                }
                continue;
            }

            if let Some(inst) = self.db.get(&name) {
                if explicit {
                    match version_cmp(&avail.version, &inst.version) {
                        std::cmp::Ordering::Greater => {
                            plan.upgrade_from.insert(name.clone(), inst.version.clone());
                            plan.download_bytes += avail.download_size.unwrap_or(0);
                            plan.install_bytes  += avail.installed_size_kb.unwrap_or(0) * 1024;
                            self.enqueue_deps(&avail, no_recommends, &mut queue);
                            plan.to_upgrade.push(avail);
                        }
                        _ => {} // up to date
                    }
                }
                continue;
            }

            // Check conflicts BEFORE adding to plan
            self.check_conflicts(&avail, &plan, &mut plan.conflicts);

            plan.download_bytes += avail.download_size.unwrap_or(0);
            plan.install_bytes  += avail.installed_size_kb.unwrap_or(0) * 1024;
            self.enqueue_deps(&avail, no_recommends, &mut queue);
            plan.to_install.push(avail);
        }

        plan.to_install.sort_by(|a, b| a.name.cmp(&b.name));
        plan.to_upgrade.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(plan)
    }

    // ── resolve_reinstall ─────────────────────────────────────

    pub fn resolve_reinstall(&self, names: &[String]) -> Result<TransactionPlan> {
        let mut plan = TransactionPlan::default();
        for name in names {
            let avail = self.cache.get(name).ok_or_else(|| {
                anyhow::anyhow!(
                    "Package '{}' not found in index. Run `hammer sync` first.", name
                )
            })?;
            // Force reinstall regardless of installed version
            if let Some(inst) = self.db.get(name) {
                plan.upgrade_from.insert(name.clone(), inst.version.clone());
                plan.to_upgrade.push(avail.clone());
            } else {
                plan.to_install.push(avail.clone());
            }
            plan.download_bytes += avail.download_size.unwrap_or(0);
            plan.install_bytes  += avail.installed_size_kb.unwrap_or(0) * 1024;
        }
        Ok(plan)
    }

    // ── resolve_remove ────────────────────────────────────────

    pub fn resolve_remove(&self, names: &[String]) -> Result<TransactionPlan> {
        let mut plan = TransactionPlan::default();
        for name in names {
            match self.db.get(name) {
                Some(inst) => {
                    plan.freed_bytes += inst.installed_size_kb * 1024;
                    plan.to_remove.push(name.clone());
                }
                None => bail!("Package '{}' is not installed.", name),
            }
        }
        // Check for reverse dependencies that will break
        let rdeps = self.find_reverse_deps(names);
        for rdep in &rdeps {
            plan.warnings.push(format!(
                "Removing '{}' may break installed package '{}'",
                names.join(", "), rdep
            ));
        }
        Ok(plan)
    }

    // ── resolve_upgrade ───────────────────────────────────────

    pub fn resolve_upgrade(&self) -> Result<TransactionPlan> {
        let mut plan = TransactionPlan::default();
        for inst in self.db.list_all()? {
            if let Some(avail) = self.cache.get(&inst.name) {
                if version_cmp(&avail.version, &inst.version) == std::cmp::Ordering::Greater {
                    plan.upgrade_from.insert(inst.name.clone(), inst.version.clone());
                    plan.download_bytes += avail.download_size.unwrap_or(0);
                    plan.install_bytes  += avail.installed_size_kb.unwrap_or(0) * 1024;
                    plan.to_upgrade.push(avail.clone());
                }
            }
        }
        plan.to_upgrade.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(plan)
    }

    // ── resolve_dist_upgrade ──────────────────────────────────
    //
    // Aggressive upgrade: upgrades everything including packages
    // that normal upgrade would skip (e.g. version downgrades,
    // held packages). Also resolves new dependencies introduced
    // by the new versions.

    pub fn resolve_dist_upgrade(&self) -> Result<TransactionPlan> {
        let mut plan = self.resolve_upgrade()?;

        // Additionally: find packages in the new index that are
        // not yet installed but are required by packages being upgraded.
        let mut queue: VecDeque<(String, bool)> = VecDeque::new();
        let mut seen:  HashSet<String> = plan.to_upgrade.iter()
            .map(|p| p.name.clone()).collect();

        for pkg in &plan.to_upgrade.clone() {
            self.enqueue_deps(pkg, false, &mut queue);
        }

        while let Some((name, _)) = queue.pop_front() {
            if seen.contains(&name) { continue; }
            seen.insert(name.clone());

            if self.db.is_installed(&name) { continue; }

            if let Some(avail) = self.cache.get(&name) {
                plan.download_bytes += avail.download_size.unwrap_or(0);
                plan.install_bytes  += avail.installed_size_kb.unwrap_or(0) * 1024;
                self.enqueue_deps(avail, false, &mut queue);
                plan.to_install.push(avail.clone());
            }
        }

        plan.to_install.sort_by(|a, b| a.name.cmp(&b.name));
        plan.warnings.push(
            "dist-upgrade: this is an aggressive upgrade — \
             review the package list carefully before proceeding."
                .to_string(),
        );
        Ok(plan)
    }

    // ── resolve_autoremove ────────────────────────────────────

    pub fn resolve_autoremove(&self) -> Result<TransactionPlan> {
        let mut plan   = TransactionPlan::default();
        let user_pkgs  = self.db.list_user_installed()?;
        let mut needed: HashSet<String> = user_pkgs.iter()
            .map(|p| p.name.clone()).collect();

        let mut queue: VecDeque<String> = needed.iter().cloned().collect();
        while let Some(name) = queue.pop_front() {
            if let Some(pkg) = self.db.get(&name) {
                if let Some(ref dep_str) = pkg.depends {
                    for group in parse_dep_field(dep_str) {
                        if let Some(dep) = group.alternatives.iter()
                            .find(|a| self.db.is_installed(&a.name))
                        {
                            if needed.insert(dep.name.clone()) {
                                queue.push_back(dep.name.clone());
                            }
                        }
                    }
                }
            }
        }

        for pkg in self.db.list_all()? {
            if !needed.contains(&pkg.name) {
                plan.freed_bytes += pkg.installed_size_kb * 1024;
                plan.to_autoremove.push(pkg.name.clone());
            }
        }
        plan.to_autoremove.sort();
        Ok(plan)
    }

    // ── resolve_fix_broken ────────────────────────────────────
    //
    // Find installed packages with unsatisfied dependencies and
    // build a plan to install the missing ones.

    pub fn resolve_fix_broken(&self) -> Result<TransactionPlan> {
        let mut plan    = TransactionPlan::default();
        let mut to_install: HashSet<String> = HashSet::new();
        let mut broken:     Vec<String>     = Vec::new();

        for inst in self.db.list_all()? {
            let mut pkg_broken = false;
            // Check Depends
            if let Some(ref dep_str) = inst.depends {
                for group in parse_dep_field(dep_str) {
                    let satisfied = group.alternatives.iter().any(|alt| {
                        if let Some(inst_dep) = self.db.get(&alt.name) {
                            if let Some(ref c) = alt.constraint {
                                version_satisfies(&inst_dep.version, &c.op, &c.version)
                            } else { true }
                        } else { false }
                    });
                    if !satisfied {
                        // Try to find a package to satisfy this dep
                        if let Some(dep) = group.alternatives.iter()
                            .find(|a| self.cache.get(&a.name).is_some())
                        {
                            to_install.insert(dep.name.clone());
                        } else {
                            broken.push(format!(
                                "{}: unsatisfied dependency '{}'",
                                inst.name,
                                group.alternatives.iter()
                                    .map(|a| a.name.as_str())
                                    .collect::<Vec<_>>().join(" | ")
                            ));
                        }
                        pkg_broken = true;
                    }
                }
            }
            if pkg_broken { broken.push(inst.name.clone()); }
        }

        if broken.is_empty() && to_install.is_empty() {
            plan.warnings.push("No broken dependencies found.".to_string());
            return Ok(plan);
        }

        for name in &to_install {
            if let Some(avail) = self.cache.get(name) {
                plan.download_bytes += avail.download_size.unwrap_or(0);
                plan.install_bytes  += avail.installed_size_kb.unwrap_or(0) * 1024;
                plan.to_install.push(avail.clone());
            }
        }

        if !broken.is_empty() {
            plan.warnings.push(format!(
                "Broken packages detected: {}",
                broken.join(", ")
            ));
        }

        plan.to_install.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(plan)
    }

    // ── Internal helpers ──────────────────────────────────────

    fn enqueue_deps(
        &self,
        pkg:           &Package,
        no_recommends: bool,
        queue:         &mut VecDeque<(String, bool)>,
    ) {
        let fields: &[Option<&str>] = &[
            pkg.pre_depends.as_deref(),
            pkg.depends.as_deref(),
            if no_recommends { None } else { pkg.recommends.as_deref() },
        ];
        for field in fields.iter().flatten() {
            for group in parse_dep_field(field) {
                // First: prefer already-installed alternative
                let chosen = group.alternatives.iter().find(|alt| {
                    if let Some(inst) = self.db.get(&alt.name) {
                        if let Some(ref c) = alt.constraint {
                            return version_satisfies(&inst.version, &c.op, &c.version);
                        }
                        return true;
                    }
                    false
                // Second: prefer alternative available in cache
                }).or_else(|| {
                    group.alternatives.iter().find(|alt| {
                        self.cache.get(&alt.name).is_some()
                        || self.provides_map.contains_key(&alt.name)
                    })
                });

                if let Some(dep) = chosen {
                    let dep_name = self.resolve_name(
                        dep.name.split(':').next().unwrap_or(&dep.name)
                    );
                    queue.push_back((dep_name, false));
                }
            }
        }
    }

    /// Resolve a virtual/provides name to its real package name.
    fn resolve_name(&self, name: &str) -> String {
        // If direct package exists, use it
        if self.cache.get(name).is_some() { return name.to_string(); }
        // Try provides map
        if let Some(providers) = self.provides_map.get(name) {
            if let Some(real) = providers.first() {
                return real.clone();
            }
        }
        name.to_string()
    }

    /// Check if pkg conflicts with anything in the current plan or db.
    fn check_conflicts(
        &self,
        pkg:       &Package,
        _plan:     &TransactionPlan,
        conflicts: &mut Vec<String>,
    ) {
        // Check Conflicts field
        if let Some(ref c_str) = pkg.conflicts {
            for group in parse_dep_field(c_str) {
                for alt in &group.alternatives {
                    if self.db.is_installed(&alt.name) {
                        conflicts.push(format!(
                            "Package '{}' conflicts with installed '{}'",
                            pkg.name, alt.name
                        ));
                    }
                }
            }
        }
        // Check Breaks field
        if let Some(ref b_str) = pkg.breaks {
            for group in parse_dep_field(b_str) {
                for alt in &group.alternatives {
                    if let Some(inst) = self.db.get(&alt.name) {
                        let breaks_it = if let Some(ref c) = alt.constraint {
                            version_satisfies(&inst.version, &c.op, &c.version)
                        } else { true };
                        if breaks_it {
                            conflicts.push(format!(
                                "Package '{}' breaks installed '{}' ({})",
                                pkg.name, alt.name, inst.version
                            ));
                        }
                    }
                }
            }
        }
    }

    /// Find packages that depend on any of `names`.
    fn find_reverse_deps(&self, names: &[String]) -> Vec<String> {
        let name_set: HashSet<&str> = names.iter().map(|s| s.as_str()).collect();
        let mut rdeps = Vec::new();
        let Ok(all) = self.db.list_all() else { return rdeps; };
        for inst in all {
            if names.contains(&inst.name) { continue; }
            if let Some(ref dep_str) = inst.depends {
                for group in parse_dep_field(dep_str) {
                    if group.alternatives.iter().any(|a| name_set.contains(a.name.as_str())) {
                        rdeps.push(inst.name.clone());
                        break;
                    }
                }
            }
        }
        rdeps
    }

    /// Find package names similar to `name` (for better error messages).
    fn find_similar(&self, name: &str) -> Vec<String> {
        let name_lower = name.to_lowercase();
        let mut results = self.cache.search(&name_lower)
            .iter().take(5).map(|p| p.name.clone()).collect::<Vec<_>>();
        results.truncate(5);
        results
    }
}

// ─────────────────────────────────────────────────────────────
//  Build provides map from cache
// ─────────────────────────────────────────────────────────────

fn build_provides_map(cache: &PackageCache) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for pkg in cache.all_packages() {
        if let Some(ref provides_str) = pkg.provides {
            // Provides field: "libfoo (= 1.2), virtual-pkg, ..."
            for group in parse_dep_field(provides_str) {
                for alt in &group.alternatives {
                    map.entry(alt.name.clone())
                        .or_default()
                        .push(pkg.name.clone());
                }
            }
        }
    }
    map
}
