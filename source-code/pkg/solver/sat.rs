use std::collections::{HashMap, HashSet, VecDeque};
use anyhow::Result;

use crate::package::{parse_dep_field, Package};
use crate::solver::conflicts;
use crate::solver::dpll::PackageSatProblem;
use crate::solver::error::{SolverError, SolverProblem};
use crate::solver::provides::ProvidesMap;
use crate::solver::version::{compare, satisfies};
use super::{Solver, TransactionPlan};

pub(crate) fn arch_matches(pkg_arch: &str, sys_arch: &str) -> bool {
    matches!(pkg_arch, "all" | "any" | "") || pkg_arch == sys_arch
}

struct Pool<'a> {
    cache:    &'a crate::cache::PackageCache,
    db:       &'a crate::db::InstalledDb,
    provides: ProvidesMap,
    arch:     String,
}

impl<'a> Pool<'a> {
    fn new(cache: &'a crate::cache::PackageCache, db: &'a crate::db::InstalledDb) -> Self {
        let arch     = crate::cache::detect_arch();
        let provides = crate::solver::provides::build(cache);
        Pool { cache, db, provides, arch }
    }

    fn best(&self, name: &str) -> Option<&Package> {
        self.all_versions(name).into_iter().next()
    }

    fn all_versions(&self, name: &str) -> Vec<&Package> {
        let real = self.provides.resolve(name);
        let mut v: Vec<&Package> = self.cache.all_packages()
        .into_iter()
        .filter(|p| p.name == real && arch_matches(&p.architecture, &self.arch))
        .collect();
        v.sort_by(|a, b| compare(&b.version, &a.version));
        v
    }

    fn resolve<'b>(&'b self, name: &'b str) -> &'b str {
        self.provides.resolve(name)
    }
}

pub(super) fn resolve_install(
    solver:        &Solver<'_>,
    names:         &[String],
    no_recommends: bool,
) -> Result<TransactionPlan> {
    let pool = Pool::new(solver.cache, solver.db);
    let mut plan = TransactionPlan::default();

    // Validate root packages
    let mut problems = Vec::new();
    for name in names {
        let bare = name.split(':').next().unwrap_or(name);
        if pool.best(bare).is_none() && !pool.provides.is_virtual(bare) {
            problems.push(SolverProblem::NotFound {
                name:    bare.to_string(),
                          similar: solver.find_similar(bare),
            });
        }
    }
    if !problems.is_empty() {
        return Err(SolverError::new(problems).into());
    }

    // BFS: collect candidate packages
    let mut candidates: HashMap<String, Vec<Package>> = HashMap::new();
    let mut queue: VecDeque<String> = names.iter()
    .map(|n| n.split(':').next().unwrap_or(n).to_string())
    .collect();
    let mut visited: HashSet<String> = HashSet::new();

    const MAX_EXPLORE: usize = 4_000;

    while let Some(name) = queue.pop_front() {
        let real = pool.resolve(&name).to_string();
        if visited.contains(&real) { continue; }
        visited.insert(real.clone());
        if visited.len() > MAX_EXPLORE {
            plan.warnings.push(format!(
                "Dependency graph is very large (>{} nodes). Some optional deps may be skipped.",
                                       MAX_EXPLORE
            ));
            break;
        }

        // FIX E0502: build the set of versions to keep BEFORE calling retain,
        // so the closure captures only the pre-built set, not `all` itself.
        let all_versions = pool.all_versions(&real);

        let keep_versions: HashSet<String> = {
            let mut set = HashSet::new();
            // Always keep newest available
            if let Some(first) = all_versions.first() {
                set.insert(first.version.clone());
            }
            // Also keep currently installed version (needed for upgrade/keep decisions)
            if let Some(inst) = solver.db.get(&real) {
                set.insert(inst.version.clone());
            }
            set
        };

        // Filter without any self-borrow inside the closure
        let selected: Vec<Package> = all_versions
        .into_iter()
        .filter(|p| keep_versions.contains(&p.version))
        .cloned()
        .collect();

        for pkg in &selected {
            for field in dep_fields(pkg, no_recommends) {
                for group in parse_dep_field(field) {
                    for alt in &group.alternatives {
                        let dep_real = pool.resolve(&alt.name).to_string();
                        if !visited.contains(&dep_real) {
                            queue.push_back(dep_real);
                        }
                    }
                }
            }
        }

        if !selected.is_empty() {
            candidates.insert(real, selected);
        }
    }

    // Build SAT problem — newest version first → lower Var → DPLL prefers it
    let mut sat = PackageSatProblem::new();
    for (name, versions) in &candidates {
        for pkg in versions {
            sat.intern(name, &pkg.version);
        }
    }
    sat.build();

    // Require root packages
    for name in names {
        let bare = name.split(':').next().unwrap_or(name);
        let real = pool.resolve(bare).to_string();
        if let Some(vs) = candidates.get(&real) {
            if let Some(pkg) = vs.first() {
                if let Some(&v) = sat.pkg_to_var.get(&(real.clone(), pkg.version.clone())) {
                    sat.require(v);
                }
            }
        }
    }

    // At-most-one version per package name
    for (name, versions) in &candidates {
        let vars: Vec<u32> = versions.iter()
        .filter_map(|p| sat.pkg_to_var.get(&(name.clone(), p.version.clone())).copied())
        .collect();
        if vars.len() > 1 {
            sat.add_at_most_one(&vars);
        }
    }

    // Dependency and conflict clauses
    for (name, versions) in &candidates {
        for pkg in versions {
            let pkg_key = (name.clone(), pkg.version.clone());
            let pkg_var = match sat.pkg_to_var.get(&pkg_key).copied() {
                Some(v) => v,
                None    => continue,
            };

            for field in dep_fields(pkg, no_recommends) {
                for group in parse_dep_field(field) {
                    let already_sat = group.alternatives.iter().any(|alt| {
                        solver.db.get(&alt.name).map_or(false, |inst| {
                            alt.constraint.as_ref()
                            .map(|c| satisfies(&inst.version, c.op.as_str(), &c.version))
                            .unwrap_or(true)
                        })
                    });
                    if already_sat { continue; }

                    let mut dep_vars: Vec<u32> = Vec::new();
                    for alt in &group.alternatives {
                        let dep_real = pool.resolve(&alt.name).to_string();
                        if let Some(dep_versions) = candidates.get(&dep_real) {
                            for dv in dep_versions {
                                let ver_ok = alt.constraint.as_ref()
                                .map(|c| satisfies(&dv.version, c.op.as_str(), &c.version))
                                .unwrap_or(true);
                                if ver_ok {
                                    if let Some(&dvar) = sat.pkg_to_var
                                        .get(&(dep_real.clone(), dv.version.clone()))
                                        {
                                            dep_vars.push(dvar);
                                        }
                                }
                            }
                        }
                    }

                    if dep_vars.is_empty() {
                        let dep_names = group.alternatives.iter()
                        .map(|a| a.name.as_str()).collect::<Vec<_>>().join(" | ");
                        plan.warnings.push(format!(
                            "{}: dependency '{}' cannot be satisfied — skipped",
                            pkg.name, dep_names
                        ));
                    } else {
                        sat.add_dependency(pkg_var, &dep_vars);
                    }
                }
            }

            if let Some(ref c_str) = pkg.conflicts {
                for group in parse_dep_field(c_str) {
                    for alt in &group.alternatives {
                        let dep_real = pool.resolve(&alt.name).to_string();
                        if let Some(dep_vs) = candidates.get(&dep_real) {
                            for dv in dep_vs {
                                let matches = alt.constraint.as_ref()
                                .map(|c| satisfies(&dv.version, c.op.as_str(), &c.version))
                                .unwrap_or(true);
                                if matches {
                                    if let Some(&dvar) = sat.pkg_to_var
                                        .get(&(dep_real.clone(), dv.version.clone()))
                                        {
                                            sat.add_conflict(pkg_var, dvar);
                                        }
                                }
                            }
                        }
                    }
                }
            }

            if let Some(ref b_str) = pkg.breaks {
                for group in parse_dep_field(b_str) {
                    for alt in &group.alternatives {
                        if let Some(inst) = solver.db.get(&alt.name) {
                            let breaks_it = alt.constraint.as_ref()
                            .map(|c| satisfies(&inst.version, c.op.as_str(), &c.version))
                            .unwrap_or(true);
                            if breaks_it {
                                plan.warnings.push(format!(
                                    "'{}' breaks installed '{}' {}",
                                    pkg.name, inst.name, inst.version
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    // Run DPLL
    let solution = sat.solve().ok_or_else(|| {
        SolverError::single(SolverProblem::Generic(
            "Dependency resolution failed (SAT unsatisfiable).\n  \
There is an unresolvable conflict between packages.\n  \
Try: hammer fix-broken, or exclude conflicting packages."
.to_string(),
        ))
    })?;

    // Map solution → TransactionPlan
    let sys_arch = pool.arch.clone();

    for (name, version) in &solution {
        let pkg = match candidates.get(name)
        .and_then(|vs| vs.iter().find(|p| &p.version == version))
        {
            Some(p) => p.clone(),
            None    => continue,
        };

        if !arch_matches(&pkg.architecture, &sys_arch) { continue; }

        match solver.db.get(name) {
            Some(inst) if inst.version == *version => {
                // Already at this exact version — nothing to do
            }
            Some(inst) if compare(version, &inst.version) == std::cmp::Ordering::Greater => {
                plan.upgrade_from.insert(name.clone(), inst.version.clone());
                plan.download_bytes += pkg.download_size.unwrap_or(0);
                plan.install_bytes  += pkg.installed_size_kb.unwrap_or(0) * 1024;
                plan.to_upgrade.push(pkg);
            }
            Some(_) => {
                plan.download_bytes += pkg.download_size.unwrap_or(0);
                plan.install_bytes  += pkg.installed_size_kb.unwrap_or(0) * 1024;
                plan.to_install.push(pkg);
            }
            None => {
                plan.download_bytes += pkg.download_size.unwrap_or(0);
                plan.install_bytes  += pkg.installed_size_kb.unwrap_or(0) * 1024;
                plan.to_install.push(pkg);
            }
        }
    }

    plan.to_install.sort_by(|a, b| a.name.cmp(&b.name));
    plan.to_upgrade.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(plan)
}

pub(super) fn resolve_reinstall(solver: &Solver<'_>, names: &[String]) -> Result<TransactionPlan> {
    let pool = Pool::new(solver.cache, solver.db);
    let mut plan     = TransactionPlan::default();
    let mut problems = Vec::new();

    for name in names {
        let pkg = match pool.best(name) {
            Some(p) => p.clone(),
            None    => {
                problems.push(SolverProblem::NotFound {
                    name: name.clone(), similar: solver.find_similar(name),
                });
                continue;
            }
        };
        if !arch_matches(&pkg.architecture, &pool.arch) {
            plan.conflicts.push(format!("Package '{}' arch mismatch", pkg.name));
            continue;
        }
        plan.download_bytes += pkg.download_size.unwrap_or(0);
        plan.install_bytes  += pkg.installed_size_kb.unwrap_or(0) * 1024;
        if let Some(inst) = solver.db.get(name) {
            plan.upgrade_from.insert(name.clone(), inst.version.clone());
            plan.to_upgrade.push(pkg);
        } else {
            plan.to_install.push(pkg);
        }
    }

    if !problems.is_empty() { return Err(SolverError::new(problems).into()); }
    Ok(plan)
}

pub(super) fn resolve_remove(solver: &Solver<'_>, names: &[String]) -> Result<TransactionPlan> {
    let mut plan     = TransactionPlan::default();
    let mut problems = Vec::new();

    for name in names {
        match solver.db.get(name) {
            Some(inst) => {
                plan.freed_bytes += inst.installed_size_kb * 1024;
                plan.to_remove.push(name.clone());
            }
            None => problems.push(SolverProblem::Generic(
                format!("Package '{}' is not installed.", name)
            )),
        }
    }
    if !problems.is_empty() { return Err(SolverError::new(problems).into()); }

    for rdep in conflicts::reverse_depends(names, solver.db) {
        plan.warnings.push(format!(
            "Removing '{}' may break '{}' which depends on it",
            names.join(", "), rdep
        ));
    }
    Ok(plan)
}

pub(super) fn resolve_upgrade(solver: &Solver<'_>) -> Result<TransactionPlan> {
    let pool = Pool::new(solver.cache, solver.db);
    let mut plan = TransactionPlan::default();

    for inst in solver.db.list_all()? {
        if let Some(avail) = pool.best(&inst.name) {
            if compare(&avail.version, &inst.version) == std::cmp::Ordering::Greater
                && arch_matches(&avail.architecture, &pool.arch)
                {
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

pub(super) fn resolve_dist_upgrade(solver: &Solver<'_>) -> Result<TransactionPlan> {
    let installed_names: Vec<String> = solver.db.list_all()?
    .into_iter().map(|p| p.name).collect();
    let mut plan = resolve_install(solver, &installed_names, false)?;
    plan.warnings.insert(0,
                         "dist-upgrade: aggressive upgrade — review the package list carefully.".to_string()
    );
    Ok(plan)
}

pub(super) fn resolve_autoremove(solver: &Solver<'_>) -> Result<TransactionPlan> {
    let mut plan = TransactionPlan::default();
    let user_pkgs = solver.db.list_user_installed()?;
    let mut needed: HashSet<String> = user_pkgs.iter().map(|p| p.name.clone()).collect();
    let mut queue: VecDeque<String> = needed.iter().cloned().collect();

    const MAX: usize = 10_000;
    let mut itr = 0usize;

    while let Some(name) = queue.pop_front() {
        itr += 1;
        if itr > MAX { break; }
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

pub(super) fn resolve_fix_broken(solver: &Solver<'_>) -> Result<TransactionPlan> {
    let pool       = Pool::new(solver.cache, solver.db);
    let mut plan   = TransactionPlan::default();
    let mut to_install: HashSet<String> = HashSet::new();
    let mut broken: Vec<String>         = Vec::new();

    for inst in solver.db.list_all()? {
        if let Some(ref dep_str) = inst.depends {
            for group in parse_dep_field(dep_str) {
                let satisfied = group.alternatives.iter().any(|alt| {
                    solver.db.get(&alt.name).map_or(false, |i| {
                        alt.constraint.as_ref()
                        .map(|c| satisfies(&i.version, c.op.as_str(), &c.version))
                        .unwrap_or(true)
                    })
                });
                if !satisfied {
                    if let Some(dep) = group.alternatives.iter()
                        .find(|a| pool.best(&a.name)
                        .map_or(false, |p| arch_matches(&p.architecture, &pool.arch)))
                        {
                            to_install.insert(dep.name.clone());
                        } else {
                            let dep_str2 = group.alternatives.iter()
                            .map(|a| a.name.as_str()).collect::<Vec<_>>().join(" | ");
                            broken.push(format!("{}: cannot satisfy '{}'", inst.name, dep_str2));
                        }
                }
            }
        }
    }

    if !to_install.is_empty() {
        let names: Vec<String> = to_install.into_iter().collect();
        let sub = resolve_install(solver, &names, true)?;
        plan.to_install     = sub.to_install;
        plan.to_upgrade     = sub.to_upgrade;
        plan.upgrade_from   = sub.upgrade_from;
        plan.download_bytes = sub.download_bytes;
        plan.install_bytes  = sub.install_bytes;
        plan.warnings.extend(sub.warnings);
    }

    for msg in broken { plan.warnings.push(msg); }

    if plan.is_empty() && plan.warnings.is_empty() {
        plan.warnings.push("No broken dependencies found.".to_string());
    }

    plan.to_install.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(plan)
}

fn dep_fields<'a>(pkg: &'a Package, no_recommends: bool) -> Vec<&'a str> {
    let mut fields = Vec::new();
    if let Some(ref s) = pkg.pre_depends { fields.push(s.as_str()); }
    if let Some(ref s) = pkg.depends     { fields.push(s.as_str()); }
    if !no_recommends {
        if let Some(ref s) = pkg.recommends { fields.push(s.as_str()); }
    }
    fields
}
