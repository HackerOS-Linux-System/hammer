use std::collections::{HashMap, HashSet, VecDeque};
use anyhow::Result;

use crate::cache::PackageCache;
use crate::db::InstalledDb;
use crate::package::{parse_dep_field, Package};
use crate::solver::conflicts::{self, ConflictInfo};
use crate::solver::error::{SolverError, SolverProblem};
use crate::solver::version::{compare, satisfies};
use super::{Solver, TransactionPlan};

// ─────────────────────────────────────────────────────────────
//  Architecture helpers
// ─────────────────────────────────────────────────────────────

pub(crate) fn arch_matches(pkg_arch: &str, sys_arch: &str) -> bool {
    matches!(pkg_arch, "all" | "any" | "") || pkg_arch == sys_arch
}

// ─────────────────────────────────────────────────────────────
//  Internal pool (package lookup helpers)
// ─────────────────────────────────────────────────────────────

struct Pool<'a> {
    cache:    &'a PackageCache,
    db:       &'a InstalledDb,
    provides: crate::solver::provides::ProvidesMap,
    arch:     String,
}

impl<'a> Pool<'a> {
    fn new(cache: &'a PackageCache, db: &'a InstalledDb) -> Self {
        let arch     = crate::cache::detect_arch();
        let provides = crate::solver::provides::build(cache);
        Pool { cache, db, provides, arch }
    }

    fn best(&self, name: &str) -> Option<&Package> {
        let real = self.provides.resolve(name);
        self.cache.all_packages()
        .into_iter()
        .filter(|p| p.name == real && arch_matches(&p.architecture, &self.arch))
        .max_by(|a, b| compare(&a.version, &b.version))
    }

    fn resolve(&self, name: &str) -> String {
        self.provides.resolve(name).to_string()
    }
}

// ─────────────────────────────────────────────────────────────
//  resolve_install
// ─────────────────────────────────────────────────────────────

pub(super) fn resolve_install(
    solver:        &Solver<'_>,
    names:         &[String],
    no_recommends: bool,
) -> Result<TransactionPlan> {
    let pool = Pool::new(solver.cache, solver.db);
    let mut plan = TransactionPlan::default();

    // Validate requested packages
    let mut problems = Vec::new();
    for name in names {
        let bare = name.split(':').next().unwrap_or(name);
        if pool.best(bare).is_none() {
            problems.push(SolverProblem::NotFound {
                name:    bare.to_string(),
                          similar: solver.find_similar(bare),
            });
        }
    }
    if !problems.is_empty() {
        return Err(SolverError::new(problems).into());
    }

    let mut seen:  HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, bool)> = names.iter()
    .map(|n| (n.split(':').next().unwrap_or(n).to_string(), true))
    .collect();

    while let Some((name, explicit)) = queue.pop_front() {
        let real = pool.resolve(&name);
        if seen.contains(&real) { continue; }
        seen.insert(real.clone());

        let pkg = match pool.best(&real) {
            Some(p) => p.clone(),
            None    => {
                plan.warnings.push(format!(
                    "dependency '{}' not found in package index — skipped", real
                ));
                continue;
            }
        };

        if !arch_matches(&pkg.architecture, &pool.arch) {
            plan.conflicts.push(format!(
                "Package '{}' is for {} but system is {} — skipped",
                pkg.name, pkg.architecture, pool.arch
            ));
            continue;
        }

        if let Some(inst) = solver.db.get(&real) {
            if explicit && compare(&pkg.version, &inst.version) == std::cmp::Ordering::Greater {
                plan.upgrade_from.insert(real.clone(), inst.version.clone());
                enqueue_deps(&pkg, no_recommends, &pool, &mut queue);
                plan.download_bytes += pkg.download_size.unwrap_or(0);
                plan.install_bytes  += pkg.installed_size_kb.unwrap_or(0) * 1024;
                plan.to_upgrade.push(pkg);
            }
            continue;
        }

        // FIX E0502: collect conflicts first into a local Vec,
        // then extend plan — no simultaneous borrow of plan.
        let found_conflicts: Vec<ConflictInfo> =
        conflicts::check_install(&pkg, solver.db);
        for c in &found_conflicts {
            if c.hard { plan.conflicts.push(c.message.clone()); }
            else      { plan.warnings.push(c.message.clone()); }
        }

        plan.download_bytes += pkg.download_size.unwrap_or(0);
        plan.install_bytes  += pkg.installed_size_kb.unwrap_or(0) * 1024;
        enqueue_deps(&pkg, no_recommends, &pool, &mut queue);
        plan.to_install.push(pkg);
    }

    plan.to_install.sort_by(|a, b| a.name.cmp(&b.name));
    plan.to_upgrade.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(plan)
}

// ─────────────────────────────────────────────────────────────
//  resolve_reinstall
// ─────────────────────────────────────────────────────────────

pub(super) fn resolve_reinstall(solver: &Solver<'_>, names: &[String]) -> Result<TransactionPlan> {
    let pool = Pool::new(solver.cache, solver.db);
    let mut plan     = TransactionPlan::default();
    let mut problems = Vec::new();

    for name in names {
        let pkg = match pool.best(name) {
            Some(p) => p.clone(),
            None    => {
                problems.push(SolverProblem::NotFound {
                    name:    name.clone(),
                              similar: solver.find_similar(name),
                });
                continue;
            }
        };

        if !arch_matches(&pkg.architecture, &pool.arch) {
            plan.conflicts.push(format!(
                "Package '{}' is for {} but system is {}",
                pkg.name, pkg.architecture, pool.arch
            ));
            continue;
        }

        if let Some(inst) = solver.db.get(name) {
            plan.upgrade_from.insert(name.clone(), inst.version.clone());
            plan.to_upgrade.push(pkg.clone());
        } else {
            plan.to_install.push(pkg.clone());
        }
        plan.download_bytes += pkg.download_size.unwrap_or(0);
        plan.install_bytes  += pkg.installed_size_kb.unwrap_or(0) * 1024;
    }

    if !problems.is_empty() {
        return Err(SolverError::new(problems).into());
    }
    Ok(plan)
}

// ─────────────────────────────────────────────────────────────
//  resolve_remove
// ─────────────────────────────────────────────────────────────

pub(super) fn resolve_remove(solver: &Solver<'_>, names: &[String]) -> Result<TransactionPlan> {
    let mut plan     = TransactionPlan::default();
    let mut problems = Vec::new();

    for name in names {
        match solver.db.get(name) {
            Some(inst) => {
                plan.freed_bytes += inst.installed_size_kb * 1024;
                plan.to_remove.push(name.clone());
            }
            None => {
                problems.push(SolverProblem::Generic(
                    format!("Package '{}' is not installed.", name)
                ));
            }
        }
    }

    if !problems.is_empty() {
        return Err(SolverError::new(problems).into());
    }

    let rdeps = conflicts::reverse_depends(names, solver.db);
    for rdep in &rdeps {
        plan.warnings.push(format!(
            "Removing '{}' may break installed package '{}' which depends on it",
            names.join(", "), rdep
        ));
    }
    Ok(plan)
}

// ─────────────────────────────────────────────────────────────
//  resolve_upgrade
// ─────────────────────────────────────────────────────────────

pub(super) fn resolve_upgrade(solver: &Solver<'_>) -> Result<TransactionPlan> {
    let pool = Pool::new(solver.cache, solver.db);
    let mut plan = TransactionPlan::default();

    for inst in solver.db.list_all()? {
        if let Some(avail) = pool.best(&inst.name) {
            if compare(&avail.version, &inst.version) == std::cmp::Ordering::Greater {
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

// ─────────────────────────────────────────────────────────────
//  resolve_dist_upgrade
// ─────────────────────────────────────────────────────────────

pub(super) fn resolve_dist_upgrade(solver: &Solver<'_>) -> Result<TransactionPlan> {
    let pool  = Pool::new(solver.cache, solver.db);
    let mut plan = resolve_upgrade(solver)?;

    let mut queue: VecDeque<(String, bool)> = VecDeque::new();
    let mut seen: HashSet<String> = plan.to_upgrade.iter()
    .map(|p| p.name.clone()).collect();

    for pkg in &plan.to_upgrade.clone() {
        enqueue_deps(pkg, false, &pool, &mut queue);
    }

    while let Some((name, _)) = queue.pop_front() {
        if seen.contains(&name) { continue; }
        seen.insert(name.clone());
        if solver.db.is_installed(&name) { continue; }
        if let Some(avail) = pool.best(&name) {
            let avail = avail.clone();
            plan.download_bytes += avail.download_size.unwrap_or(0);
            plan.install_bytes  += avail.installed_size_kb.unwrap_or(0) * 1024;
            enqueue_deps(&avail, false, &pool, &mut queue);
            plan.to_install.push(avail);
        }
    }

    plan.to_install.sort_by(|a, b| a.name.cmp(&b.name));
    plan.warnings.push(
        "dist-upgrade: aggressive upgrade — review the package list carefully.".to_string()
    );
    Ok(plan)
}

// ─────────────────────────────────────────────────────────────
//  resolve_autoremove
// ─────────────────────────────────────────────────────────────

pub(super) fn resolve_autoremove(solver: &Solver<'_>) -> Result<TransactionPlan> {
    let mut plan = TransactionPlan::default();
    let user_pkgs = solver.db.list_user_installed()?;
    let mut needed: HashSet<String> = user_pkgs.iter().map(|p| p.name.clone()).collect();
    let mut queue: VecDeque<String> = needed.iter().cloned().collect();

    while let Some(name) = queue.pop_front() {
        if let Some(pkg) = solver.db.get(&name) {
            if let Some(ref dep_str) = pkg.depends {
                for group in parse_dep_field(dep_str) {
                    if let Some(dep) = group.alternatives.iter()
                        .find(|a| solver.db.is_installed(&a.name))
                        {
                            if needed.insert(dep.name.clone()) {
                                queue.push_back(dep.name.clone());
                            }
                        }
                }
            }
        }
    }

    for pkg in solver.db.list_all()? {
        if !needed.contains(&pkg.name) {
            plan.freed_bytes   += pkg.installed_size_kb * 1024;
            plan.to_autoremove.push(pkg.name.clone());
        }
    }
    plan.to_autoremove.sort();
    Ok(plan)
}

// ─────────────────────────────────────────────────────────────
//  resolve_fix_broken
// ─────────────────────────────────────────────────────────────

pub(super) fn resolve_fix_broken(solver: &Solver<'_>) -> Result<TransactionPlan> {
    let pool = Pool::new(solver.cache, solver.db);
    let mut plan      = TransactionPlan::default();
    let mut to_install: HashSet<String> = HashSet::new();
    let mut broken:     Vec<String>     = Vec::new();

    for inst in solver.db.list_all()? {
        if let Some(ref dep_str) = inst.depends {
            for group in parse_dep_field(dep_str) {
                let satisfied = group.alternatives.iter().any(|alt| {
                    if let Some(i) = solver.db.get(&alt.name) {
                        if let Some(ref c) = alt.constraint {
                            return satisfies(&i.version, c.op.as_str(), &c.version);
                        }
                        return true;
                    }
                    false
                });
                if !satisfied {
                    let found = group.alternatives.iter()
                    .find(|a| pool.best(&a.name).map_or(false, |p| arch_matches(&p.architecture, &pool.arch)));
                    if let Some(dep) = found {
                        to_install.insert(dep.name.clone());
                    } else {
                        broken.push(format!(
                            "{}: cannot satisfy '{}'",
                            inst.name,
                            group.alternatives.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(" | ")
                        ));
                    }
                }
            }
        }
    }

    for name in &to_install {
        if let Some(avail) = pool.best(name) {
            plan.download_bytes += avail.download_size.unwrap_or(0);
            plan.install_bytes  += avail.installed_size_kb.unwrap_or(0) * 1024;
            plan.to_install.push(avail.clone());
        }
    }

    for msg in broken { plan.warnings.push(msg); }

    if plan.is_empty() && plan.warnings.is_empty() {
        plan.warnings.push("No broken dependencies found.".to_string());
    }

    plan.to_install.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(plan)
}

// ─────────────────────────────────────────────────────────────
//  Helpers
// ─────────────────────────────────────────────────────────────

fn enqueue_deps(
    pkg:           &Package,
    no_recommends: bool,
    pool:          &Pool<'_>,
    queue:         &mut VecDeque<(String, bool)>,
) {
    let fields: &[Option<&str>] = &[
        pkg.pre_depends.as_deref(),
        pkg.depends.as_deref(),
        if no_recommends { None } else { pkg.recommends.as_deref() },
    ];
    for field in fields.iter().flatten() {
        for group in parse_dep_field(field) {
            let chosen = group.alternatives.iter().find(|alt| {
                if let Some(inst) = pool.db.get(&alt.name) {
                    if let Some(ref c) = alt.constraint {
                        return satisfies(&inst.version, c.op.as_str(), &c.version);
                    }
                    return true;
                }
                false
            }).or_else(|| {
                group.alternatives.iter().find(|alt| pool.best(&alt.name).is_some())
            });

            if let Some(dep) = chosen {
                let bare = dep.name.split(':').next().unwrap_or(&dep.name);
                let real = pool.resolve(bare);
                queue.push_back((real, false));
            }
        }
    }
}
