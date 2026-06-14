use std::collections::{HashMap, HashSet, VecDeque};
use anyhow::Result;

use crate::multi_arch::{self, MultiArchMode};
use crate::package::{parse_dep_field, Package};
use crate::solver::conflicts;
use crate::solver::dpll::PackageSatProblem;
use crate::solver::error::{SolverError, SolverProblem};
use crate::solver::provides::ProvidesMap;
use crate::solver::version::{compare, satisfies};
use super::{Solver, TransactionPlan};

// ─────────────────────────────────────────────────────────────
//  Architecture helper
// ─────────────────────────────────────────────────────────────

pub(crate) fn arch_matches(pkg_arch: &str, sys_arch: &str) -> bool {
    matches!(pkg_arch, "all" | "any" | "") || pkg_arch == sys_arch
}

// ─────────────────────────────────────────────────────────────
//  Pool
// ─────────────────────────────────────────────────────────────

struct Pool<'a> {
    cache:      &'a crate::cache::PackageCache,
    db:         &'a crate::db::InstalledDb,
    provides:   ProvidesMap,
    /// Native + all configured foreign arches
    all_arches: Vec<String>,
    /// Native arch (primary)
    native:     String,
    /// Pin priorities for version sorting
    pins:       &'a crate::pins::PinDb,
}

impl<'a> Pool<'a> {
    fn new(solver: &'a Solver<'_>) -> Self {
        let native     = crate::cache::detect_arch();
        let all_arches = solver.multi_arch.all_arches();
        let provides   = crate::solver::provides::build(solver.cache);
        Pool {
            cache:      solver.cache,
            db:         solver.db,
            provides,
            all_arches,
            native,
            pins:       &solver.pins,
        }
    }

    /// Best (highest-priority, then newest) available version for a name,
    /// filtered to native arch (and arch-independent).
    fn best(&self, name: &str) -> Option<&Package> {
        self.all_versions_native(name).into_iter().next()
    }

    /// All available versions for native arch, sorted by pin priority desc
    /// then version desc. Forbidden versions (priority < 0) excluded.
    fn all_versions_native(&self, name: &str) -> Vec<&Package> {
        let real = self.provides.resolve(name);
        self.sorted_versions(real, &self.native)
    }

    /// All versions for a given arch, with pin filtering and priority sort.
    fn all_versions_for_arch(&self, name: &str, arch: &str) -> Vec<&Package> {
        let real = self.provides.resolve(name);
        self.sorted_versions(real, arch)
    }

    fn sorted_versions(&self, real: &str, arch: &str) -> Vec<&Package> {
        let inst_ver = self.db.get(real).map(|p| p.version);

        let mut v: Vec<&Package> = self.cache.all_packages()
        .into_iter()
        .filter(|p| {
            p.name == real
            && (arch_matches(&p.architecture, arch)
            || p.architecture == "all")
        })
        .filter(|p| !self.pins.is_forbidden(&p.name, &p.version))
        .collect();

        // Sort: pin priority desc, then version desc
        v.sort_by(|a, b| {
            let pa = self.pins.priority(&a.name, &a.version,
                                        inst_ver.as_deref());
            let pb = self.pins.priority(&b.name, &b.version,
                                        inst_ver.as_deref());
            pb.cmp(&pa)
            .then_with(|| compare(&b.version, &a.version))
        });
        v
    }

    fn resolve<'b>(&'b self, name: &'b str) -> &'b str {
        self.provides.resolve(name)
    }

    /// Check if a candidate can satisfy a dependency from `requirer_arch`.
    fn can_satisfy(
        &self,
        candidate:      &Package,
        requirer_arch:  &str,
    ) -> bool {
        // Determine Multi-Arch mode from package (not yet in our Package struct,
        // default to No which means same-arch only)
        let ma_mode = MultiArchMode::No; // TODO: parse Multi-Arch field when added to Package
        multi_arch::can_satisfy_dep(&candidate.architecture, &ma_mode, requirer_arch)
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
    let pool = Pool::new(solver);
    let mut plan = TransactionPlan::default();

    // ── Parse name:arch specs and validate roots ──────────────
    let mut problems = Vec::new();
    let mut root_specs: Vec<(String, String)> = Vec::new(); // (name, arch)

    for name in names {
        let (bare, arch_override) = multi_arch::parse_pkg_spec(name);
        let arch = arch_override.as_deref().unwrap_or(&pool.native).to_string();

        // Validate arch is configured
        if !pool.all_arches.iter().any(|a| a == &arch) && arch != pool.native {
            problems.push(SolverProblem::ArchMismatch {
                package:  bare.clone(),
                          pkg_arch: arch.clone(),
                          sys_arch: pool.native.clone(),
            });
            continue;
        }

        let real = pool.resolve(&bare).to_string();
        let candidates = pool.all_versions_for_arch(&real, &arch);

        if candidates.is_empty() && !pool.provides.is_virtual(&real) {
            problems.push(SolverProblem::NotFound {
                name:    bare.clone(),
                          similar: solver.find_similar(&bare),
            });
            continue;
        }

        // Check if best version is pinned forbidden
        if let Some(best) = candidates.first() {
            if solver.pins.is_forbidden(&best.name, &best.version) {
                problems.push(SolverProblem::Generic(format!(
                    "Package '{}' is pinned as forbidden (priority < 0). \
See: hammer pin list", bare
                )));
                continue;
            }
        }

        root_specs.push((real, arch));
    }

    if !problems.is_empty() {
        return Err(SolverError::new(problems).into());
    }

    // ── BFS: collect candidates across all arches ─────────────
    // Key: (real_name, arch) → sorted versions
    let mut candidates: HashMap<(String, String), Vec<Package>> = HashMap::new();
    let mut queue: VecDeque<(String, String)> = root_specs.iter().cloned().collect();
    let mut visited: HashSet<(String, String)> = HashSet::new();

    const MAX_EXPLORE: usize = 8_000; // higher limit for multi-arch

    while let Some((name, arch)) = queue.pop_front() {
        let key = (name.clone(), arch.clone());
        if visited.contains(&key) { continue; }
        visited.insert(key.clone());

        if visited.len() > MAX_EXPLORE {
            plan.warnings.push(format!(
                "Dependency graph very large (>{} nodes). Some optional deps skipped.",
                                       MAX_EXPLORE
            ));
            break;
        }

        let all_vers = pool.all_versions_for_arch(&name, &arch);

        // Keep: pin-preferred version + installed version
        let inst_ver = solver.db.get(&name).map(|p| p.version.clone());
        let keep: HashSet<String> = {
            let mut s = HashSet::new();
            if let Some(v) = all_vers.first() { s.insert(v.version.clone()); }
            if let Some(ref iv) = inst_ver     { s.insert(iv.clone()); }
            s
        };

        let selected: Vec<Package> = all_vers.into_iter()
        .filter(|p| keep.contains(&p.version))
        .cloned()
        .collect();

        // BFS: enqueue deps for all configured arches
        for pkg in &selected {
            for field in dep_fields(pkg, no_recommends) {
                for group in parse_dep_field(field) {
                    for alt in &group.alternatives {
                        let dep_real = pool.resolve(&alt.name).to_string();
                        // Determine which arch to fetch for this dep.
                        // If dep has arch qualifier (e.g. foo:i386), use that.
                        // Otherwise try native first, then foreign if Multi-Arch: foreign.
                        for dep_arch in &pool.all_arches {
                            let dk = (dep_real.clone(), dep_arch.clone());
                            if !visited.contains(&dk) {
                                queue.push_back(dk);
                            }
                        }
                    }
                }
            }
        }

        if !selected.is_empty() {
            candidates.insert(key, selected);
        }
    }

    // ── Build SAT problem ─────────────────────────────────────
    let mut sat = PackageSatProblem::new();

    // Intern all candidates — pin priority already encoded in ordering,
    // so lower Var index = preferred version for CDCL.
    for ((name, arch), versions) in &candidates {
        for pkg in versions {
            // Use "name:arch" as SAT name to distinguish foreign copies
            let sat_name = if arch == &pool.native {
                name.clone()
            } else {
                format!("{}:{}", name, arch)
            };
            sat.intern(&sat_name, &pkg.version);
        }
    }
    sat.build();

    // Require roots
    for (name, arch) in &root_specs {
        let sat_name = if arch == &pool.native {
            name.clone()
        } else {
            format!("{}:{}", name, arch)
        };
        let key = (name.clone(), arch.clone());
        if let Some(vs) = candidates.get(&key) {
            if let Some(pkg) = vs.first() {
                if let Some(&v) = sat.pkg_to_var.get(&(sat_name.clone(), pkg.version.clone())) {
                    sat.require(v);
                }
            }
        }
    }

    // At-most-one version per (name, arch) pair
    for ((name, arch), versions) in &candidates {
        let sat_name = if arch == &pool.native { name.clone() }
        else { format!("{}:{}", name, arch) };
        let vars: Vec<u32> = versions.iter()
        .filter_map(|p| sat.pkg_to_var.get(&(sat_name.clone(), p.version.clone())).copied())
        .collect();
        if vars.len() > 1 { sat.add_at_most_one(&vars); }
    }

    // Dependency and conflict clauses
    for ((name, arch), versions) in &candidates {
        let sat_name = if arch == &pool.native { name.clone() }
        else { format!("{}:{}", name, arch) };

        for pkg in versions {
            let pkg_var = match sat.pkg_to_var.get(&(sat_name.clone(), pkg.version.clone())).copied() {
                Some(v) => v,
                None    => continue,
            };

            for field in dep_fields(pkg, no_recommends) {
                for group in parse_dep_field(field) {
                    // Already satisfied by installed package of any compatible arch?
                    let already_sat = group.alternatives.iter().any(|alt| {
                        solver.db.get(&alt.name).map_or(false, |inst| {
                            let inst_pkg_dummy = Package {
                                architecture: inst.architecture.clone(),
                                                        ..Package::default()
                            };
                            // Check version constraint
                            let ver_ok = alt.constraint.as_ref()
                            .map(|c| satisfies(&inst.version, c.op.as_str(), &c.version))
                            .unwrap_or(true);
                            // Check arch compatibility (Multi-Arch: foreign can satisfy any)
                            let arch_ok = pool.can_satisfy(&inst_pkg_dummy, arch);
                            ver_ok && (arch_ok || inst.architecture == "all")
                        })
                    });
                    if already_sat { continue; }

                    let mut dep_vars: Vec<u32> = Vec::new();

                    for alt in &group.alternatives {
                        let dep_real = pool.resolve(&alt.name).to_string();

                        // Collect from all arches that can satisfy this dep
                        for dep_arch in &pool.all_arches {
                            let dep_key = (dep_real.clone(), dep_arch.clone());
                            if let Some(dep_versions) = candidates.get(&dep_key) {
                                let dep_sat_name = if dep_arch == &pool.native {
                                    dep_real.clone()
                                } else {
                                    format!("{}:{}", dep_real, dep_arch)
                                };
                                for dv in dep_versions {
                                    // Version constraint check
                                    let ver_ok = alt.constraint.as_ref()
                                    .map(|c| satisfies(&dv.version, c.op.as_str(), &c.version))
                                    .unwrap_or(true);
                                    // Multi-arch compatibility check
                                    let arch_ok = pool.can_satisfy(dv, arch);
                                    if ver_ok && arch_ok {
                                        if let Some(&dvar) = sat.pkg_to_var
                                            .get(&(dep_sat_name.clone(), dv.version.clone()))
                                            {
                                                dep_vars.push(dvar);
                                            }
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
                        // Dedup (same var may appear via multiple arches)
                        dep_vars.sort_unstable();
                        dep_vars.dedup();
                        sat.add_dependency(pkg_var, &dep_vars);
                    }
                }
            }

            // Conflicts
            if let Some(ref c_str) = pkg.conflicts {
                for group in parse_dep_field(c_str) {
                    for alt in &group.alternatives {
                        let dep_real = pool.resolve(&alt.name).to_string();
                        for dep_arch in &pool.all_arches {
                            let dep_key = (dep_real.clone(), dep_arch.clone());
                            if let Some(dep_vs) = candidates.get(&dep_key) {
                                let dep_sat_name = if dep_arch == &pool.native {
                                    dep_real.clone()
                                } else {
                                    format!("{}:{}", dep_real, dep_arch)
                                };
                                for dv in dep_vs {
                                    let matches = alt.constraint.as_ref()
                                    .map(|c| satisfies(&dv.version, c.op.as_str(), &c.version))
                                    .unwrap_or(true);
                                    if matches {
                                        if let Some(&dvar) = sat.pkg_to_var
                                            .get(&(dep_sat_name.clone(), dv.version.clone()))
                                            {
                                                sat.add_conflict(pkg_var, dvar);
                                            }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Breaks (advisory warnings)
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

    // ── Run CDCL SAT ─────────────────────────────────────────
    let solution = sat.solve().ok_or_else(|| {
        SolverError::single(SolverProblem::Generic(
            "Dependency resolution failed (SAT unsatisfiable).\n  \
Unresolvable conflict between packages or pins.\n  \
Try: hammer fix-broken, or adjust hammer pin list."
.to_string(),
        ))
    })?;

    // ── Map solution → TransactionPlan ────────────────────────
    for (sat_name, version) in &solution {
        // Decode sat_name back to (real_name, arch)
        let (real_name, real_arch) = if let Some((n, a)) = sat_name.rsplit_once(':') {
            (n.to_string(), a.to_string())
        } else {
            (sat_name.clone(), pool.native.clone())
        };

        let key = (real_name.clone(), real_arch.clone());
        let pkg = match candidates.get(&key)
        .and_then(|vs| vs.iter().find(|p| &p.version == version))
        {
            Some(p) => p.clone(),
            None    => continue,
        };

        match solver.db.get(&real_name) {
            Some(inst) if inst.version == *version => {
                // Already installed at this version
            }
            Some(inst) if compare(version, &inst.version) == std::cmp::Ordering::Greater => {
                plan.upgrade_from.insert(real_name.clone(), inst.version.clone());
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

// ─────────────────────────────────────────────────────────────
//  resolve_reinstall
// ─────────────────────────────────────────────────────────────

pub(super) fn resolve_reinstall(solver: &Solver<'_>, names: &[String]) -> Result<TransactionPlan> {
    let pool = Pool::new(solver);
    let mut plan = TransactionPlan::default();
    let mut problems = Vec::new();

    for name in names {
        let (bare, _) = multi_arch::parse_pkg_spec(name);
        let pkg = match pool.best(&bare) {
            Some(p) => p.clone(),
            None    => {
                problems.push(SolverProblem::NotFound {
                    name: bare.clone(), similar: solver.find_similar(&bare),
                });
                continue;
            }
        };
        plan.download_bytes += pkg.download_size.unwrap_or(0);
        plan.install_bytes  += pkg.installed_size_kb.unwrap_or(0) * 1024;
        if let Some(inst) = solver.db.get(&bare) {
            plan.upgrade_from.insert(bare.clone(), inst.version.clone());
            plan.to_upgrade.push(pkg);
        } else {
            plan.to_install.push(pkg);
        }
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
        let (bare, _) = multi_arch::parse_pkg_spec(name);
        match solver.db.get(&bare) {
            Some(inst) => {
                plan.freed_bytes += inst.installed_size_kb * 1024;
                plan.to_remove.push(bare.clone());
            }
            None => problems.push(SolverProblem::Generic(
                format!("Package '{}' is not installed.", bare)
            )),
        }
    }
    if !problems.is_empty() { return Err(SolverError::new(problems).into()); }

    for rdep in conflicts::reverse_depends(&plan.to_remove, solver.db) {
        plan.warnings.push(format!(
            "Removing '{}' may break '{}' which depends on it",
            names.join(", "), rdep
        ));
    }
    Ok(plan)
}

// ─────────────────────────────────────────────────────────────
//  resolve_upgrade
// ─────────────────────────────────────────────────────────────

pub(super) fn resolve_upgrade(solver: &Solver<'_>) -> Result<TransactionPlan> {
    let pool = Pool::new(solver);
    let mut plan = TransactionPlan::default();

    for inst in solver.db.list_all()? {
        // Skip held packages
        if solver.pins.is_held(&inst.name) { continue; }

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
    let installed_names: Vec<String> = solver.db.list_all()?
    .into_iter().map(|p| p.name).collect();
    let mut plan = resolve_install(solver, &installed_names, false)?;
    plan.warnings.insert(0,
                         "dist-upgrade: aggressive — review carefully.".to_string());
    Ok(plan)
}

// ─────────────────────────────────────────────────────────────
//  resolve_autoremove
// ─────────────────────────────────────────────────────────────

pub(super) fn resolve_autoremove(solver: &Solver<'_>) -> Result<TransactionPlan> {
    use std::collections::VecDeque;
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
    let pool = Pool::new(solver);
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
                        .find(|a| pool.best(&a.name).is_some())
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

// ─────────────────────────────────────────────────────────────
//  Helpers
// ─────────────────────────────────────────────────────────────

fn dep_fields<'a>(pkg: &'a Package, no_recommends: bool) -> Vec<&'a str> {
    let mut f = Vec::new();
    if let Some(ref s) = pkg.pre_depends { f.push(s.as_str()); }
    if let Some(ref s) = pkg.depends     { f.push(s.as_str()); }
    if !no_recommends {
        if let Some(ref s) = pkg.recommends { f.push(s.as_str()); }
    }
    f
}
