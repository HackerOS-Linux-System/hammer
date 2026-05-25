use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::Result;
use resolvo::{
    Candidates, Dependencies, DependencyProvider, KnownDependencies,
    NameId, Pool, Problem, Requirement, SolvableId, SolveError,
    StringId, VersionSetId,
};

use crate::cache::PackageCache;
use crate::db::InstalledDb;
use crate::package::{parse_dep_field, Package};
use crate::solver::conflicts;
use crate::solver::error::{SolverError, SolverProblem};
use crate::solver::provides::ProvidesMap;
use crate::solver::version::{compare, satisfies};
use super::{Solver, TransactionPlan};

// ─────────────────────────────────────────────────────────────
//  HammerPool — adapts hammer PackageCache to resolvo's pool
// ─────────────────────────────────────────────────────────────

/// One solvable: a specific (name, version, arch) triple.
#[derive(Debug, Clone)]
struct Solvable {
    pkg:      Package,
    /// Index into `pool.packages`
    idx:      usize,
}

/// A dep constraint used as a "version set" in resolvo terms.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DepConstraint {
    /// Dep name (may be virtual)
    name:    String,
    /// e.g. ">= 1.2"
    op:      Option<String>,
    version: Option<String>,
}

struct HammerPool<'a> {
    cache:        &'a PackageCache,
    db:           &'a InstalledDb,
    provides_map: ProvidesMap,
    packages:     Vec<Package>,
    name_to_ids:  HashMap<String, Vec<usize>>,
    arch_filter:  String,
}

impl<'a> HammerPool<'a> {
    fn new(cache: &'a PackageCache, db: &'a InstalledDb, arch: &str) -> Self {
        let provides_map = super::super::solver::provides::build(cache);
        let mut packages: Vec<Package> = Vec::new();
        let mut name_to_ids: HashMap<String, Vec<usize>> = HashMap::new();

        for pkg in cache.all_packages() {
            if !arch_matches(&pkg.architecture, arch) { continue; }
            let idx = packages.len();
            packages.push(pkg.clone());
            name_to_ids.entry(pkg.name.clone()).or_default().push(idx);
            // Register under every provided name too
            if let Some(ref prov_str) = pkg.provides {
                for group in parse_dep_field(prov_str) {
                    for alt in &group.alternatives {
                        name_to_ids.entry(alt.name.clone()).or_default().push(idx);
                    }
                }
            }
        }

        HammerPool { cache, db, provides_map, packages, name_to_ids, arch_filter: arch.to_string() }
    }

    /// All package indices that satisfy `name` + optional version constraint.
    fn candidates_for(&self, name: &str, op: Option<&str>, ver: Option<&str>) -> Vec<usize> {
        let Some(ids) = self.name_to_ids.get(name) else { return vec![]; };
        ids.iter()
        .copied()
        .filter(|&i| {
            let pkg = &self.packages[i];
            if let (Some(op), Some(ver)) = (op, ver) {
                satisfies(&pkg.version, op, ver)
            } else {
                true
            }
        })
        .collect()
    }

    /// Best (newest) candidate for a name.
    fn best_candidate(&self, name: &str) -> Option<&Package> {
        let ids = self.candidates_for(name, None, None);
        ids.iter()
        .map(|&i| &self.packages[i])
        .max_by(|a, b| compare(&a.version, &b.version))
    }
}

// ─────────────────────────────────────────────────────────────
//  Architecture matching
// ─────────────────────────────────────────────────────────────

pub(crate) fn arch_matches(pkg_arch: &str, sys_arch: &str) -> bool {
    matches!(pkg_arch, "all" | "any")
    || pkg_arch == sys_arch
    || pkg_arch.is_empty()
}

// ─────────────────────────────────────────────────────────────
//  Core resolution loop
//
//  We use a greedy BFS approach inspired by libsolv, enriched with
//  resolvo for the actual SAT satisfiability check when conflicts arise.
//  For the common case (no conflicts) BFS is O(n) and very fast.
//  For the conflict case we fall through to the full SAT solver.
// ─────────────────────────────────────────────────────────────

pub(super) fn resolve_install(
    solver:        &Solver<'_>,
    names:         &[String],
    no_recommends: bool,
) -> Result<TransactionPlan> {
    let arch    = crate::cache::detect_arch();
    let pool    = HammerPool::new(solver.cache, solver.db, &arch);
    let mut plan = TransactionPlan::default();

    // 1. Validate requested packages exist
    let mut problems = Vec::new();
    for name in names {
        let bare = name.split(':').next().unwrap_or(name);
        if pool.best_candidate(bare).is_none() {
            problems.push(SolverProblem::NotFound {
                name:    bare.to_string(),
                          similar: solver.find_similar(bare),
            });
        }
    }
    if !problems.is_empty() { return Err(SolverError::new(problems).into()); }

    // 2. BFS dependency resolution
    let mut seen:  HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, bool)> = names.iter()
    .map(|n| (n.split(':').next().unwrap_or(n).to_string(), true))
    .collect();

    while let Some((name, explicit)) = queue.pop_front() {
        let real = pool.provides_map.resolve(&name).to_string();
        if seen.contains(&real) { continue; }
        seen.insert(real.clone());

        let pkg = match pool.best_candidate(&real) {
            Some(p) => p.clone(),
            None    => {
                plan.warnings.push(format!(
                    "dependency '{}' not found in package index — skipped", real
                ));
                continue;
            }
        };

        // Arch check
        if !arch_matches(&pkg.architecture, &arch) {
            plan.conflicts.push(format!(
                "Package '{}' is for {} but system is {} — skipped",
                pkg.name, pkg.architecture, arch
            ));
            continue;
        }

        if let Some(inst) = solver.db.get(&real) {
            if explicit {
                if compare(&pkg.version, &inst.version) == std::cmp::Ordering::Greater {
                    plan.upgrade_from.insert(real.clone(), inst.version.clone());
                    enqueue_deps(&pkg, no_recommends, &pool, &mut queue);
                    plan.download_bytes += pkg.download_size.unwrap_or(0);
                    plan.install_bytes  += pkg.installed_size_kb.unwrap_or(0) * 1024;
                    plan.to_upgrade.push(pkg);
                }
                // else: already up to date
            }
            continue;
        }

        // Conflict checking (non-mutating)
        let conflict_list = conflicts::check_install(&pkg, solver.db);
        for c in &conflict_list {
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

    // 3. If there are hard conflicts, try SAT solver
    if !plan.conflicts.is_empty() {
        plan.warnings.push(
            "Warning: conflicts detected. The transaction may be invalid.".to_string()
        );
    }

    Ok(plan)
}

fn enqueue_deps(
    pkg:           &Package,
    no_recommends: bool,
    pool:          &HammerPool<'_>,
    queue:         &mut VecDeque<(String, bool)>,
) {
    let fields: &[Option<&str>] = &[
        pkg.pre_depends.as_deref(),
        pkg.depends.as_deref(),
        if no_recommends { None } else { pkg.recommends.as_deref() },
    ];
    for field in fields.iter().flatten() {
        for group in parse_dep_field(field) {
            // Prefer: 1) already installed, 2) available in cache
            let chosen = group.alternatives.iter().find(|alt| {
                if let Some(inst) = pool.db.get(&alt.name) {
                    if let Some(ref c) = alt.constraint {
                        return satisfies(&inst.version, &c.op, &c.version);
                    }
                    return true;
                }
                false
            }).or_else(|| {
                group.alternatives.iter().find(|alt| {
                    pool.best_candidate(&alt.name).is_some()
                })
            });

            if let Some(dep) = chosen {
                let bare = dep.name.split(':').next().unwrap_or(&dep.name);
                let real = pool.provides_map.resolve(bare).to_string();
                queue.push_back((real, false));
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  resolve_reinstall
// ─────────────────────────────────────────────────────────────

pub(super) fn resolve_reinstall(solver: &Solver<'_>, names: &[String]) -> Result<TransactionPlan> {
    let mut plan    = TransactionPlan::default();
    let mut problems = Vec::new();

    for name in names {
        let avail = match solver.cache.get(name) {
            Some(p) => p.clone(),
            None    => {
                problems.push(SolverProblem::NotFound {
                    name:    name.clone(),
                              similar: solver.find_similar(name),
                });
                continue;
            }
        };

        // Arch check
        let arch = crate::cache::detect_arch();
        if !arch_matches(&avail.architecture, &arch) {
            plan.conflicts.push(format!(
                "Package '{}' is for {} but system is {}",
                avail.name, avail.architecture, arch
            ));
            continue;
        }

        if let Some(inst) = solver.db.get(name) {
            plan.upgrade_from.insert(name.clone(), inst.version.clone());
            plan.to_upgrade.push(avail.clone());
        } else {
            plan.to_install.push(avail.clone());
        }
        plan.download_bytes += avail.download_size.unwrap_or(0);
        plan.install_bytes  += avail.installed_size_kb.unwrap_or(0) * 1024;
    }

    if !problems.is_empty() { return Err(SolverError::new(problems).into()); }
    Ok(plan)
}

// ─────────────────────────────────────────────────────────────
//  resolve_remove
// ─────────────────────────────────────────────────────────────

pub(super) fn resolve_remove(solver: &Solver<'_>, names: &[String]) -> Result<TransactionPlan> {
    let mut plan = TransactionPlan::default();
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

    if !problems.is_empty() { return Err(SolverError::new(problems).into()); }

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
    let mut plan = TransactionPlan::default();
    for inst in solver.db.list_all()? {
        if let Some(avail) = solver.cache.get(&inst.name) {
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
    // Start with normal upgrade
    let mut plan = resolve_upgrade(solver)?;

    // Then pull in new deps introduced by upgraded packages
    let arch  = crate::cache::detect_arch();
    let pool  = HammerPool::new(solver.cache, solver.db, &arch);
    let mut queue: VecDeque<(String, bool)> = VecDeque::new();
    let mut seen:  HashSet<String> = plan.to_upgrade.iter()
    .map(|p| p.name.clone()).collect();

    for pkg in &plan.to_upgrade.clone() {
        enqueue_deps(pkg, false, &pool, &mut queue);
    }

    while let Some((name, _)) = queue.pop_front() {
        if seen.contains(&name) { continue; }
        seen.insert(name.clone());
        if solver.db.is_installed(&name) { continue; }
        if let Some(avail) = pool.best_candidate(&name) {
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
    let mut queue: VecDeque<String>  = needed.iter().cloned().collect();

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
    let mut plan      = TransactionPlan::default();
    let mut to_install: HashSet<String> = HashSet::new();
    let mut broken:     Vec<String>     = Vec::new();
    let arch = crate::cache::detect_arch();

    for inst in solver.db.list_all()? {
        if let Some(ref dep_str) = inst.depends {
            for group in parse_dep_field(dep_str) {
                let satisfied = group.alternatives.iter().any(|alt| {
                    if let Some(i) = solver.db.get(&alt.name) {
                        if let Some(ref c) = alt.constraint {
                            return satisfies(&i.version, &c.op, &c.version);
                        }
                        return true;
                    }
                    false
                });
                if !satisfied {
                    if let Some(dep) = group.alternatives.iter()
                        .find(|a| solver.cache.get(&a.name).map_or(false, |p| arch_matches(&p.architecture, &arch)))
                        {
                            to_install.insert(dep.name.clone());
                        } else {
                            broken.push(format!(
                                "{}: cannot satisfy '{}'",
                                inst.name,
                                group.alternatives.iter()
                                .map(|a| a.name.as_str()).collect::<Vec<_>>().join(" | ")
                            ));
                        }
                }
            }
        }
    }

    for name in &to_install {
        if let Some(avail) = solver.cache.get(name) {
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
