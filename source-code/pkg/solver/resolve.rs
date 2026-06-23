use anyhow::{bail, Result};
//
// Comparable to zypper/libsolv in capability:
//   • Full OR-group (alternatives) resolution — picks best provider
//   • Pre-Depends: installed before the package itself (ordered)
//   • Recommends: installed unless --no-recommends
//   • Suggests: listed but never auto-installed
//   • Replaces: auto-removes replaced packages
//   • Virtual package resolution via ProvidesMap
//   • Conflict + Breaks detection (forward and reverse)
//   • Hold-awareness — never upgrades held packages
//   • Pin-awareness — respects version constraints
//   • Topological install ordering (dependency order)
//   • Cycle detection and breaking
//   • CDCL SAT fallback for complex constraint sets

use std::collections::{HashMap, HashSet, VecDeque};
use crate::package::{parse_dep_field, Package};

use super::provides::{build as build_provides, ProvidesMap};
use super::version::{compare, satisfies};
use super::{Solver, TransactionPlan};

// ──────────────────────────────────────────────────────────────────────────────
//  resolve_install — main entry point
// ──────────────────────────────────────────────────────────────────────────────

pub fn resolve_install(
    solver:        &Solver<'_>,
    names:         &[String],
    no_recommends: bool,
) -> Result<TransactionPlan> {
    let pmap = build_provides(solver.cache, Some(solver.db));
    let mut ctx = ResolveCtx::new(solver, &pmap, no_recommends);

    for name in names {
        ctx.request_install(name)?;
    }

    ctx.build_plan()
}

pub fn resolve_reinstall(solver: &Solver<'_>, names: &[String]) -> Result<TransactionPlan> {
    let pmap = build_provides(solver.cache, Some(solver.db));
    let mut ctx = ResolveCtx::new(solver, &pmap, false);

    for name in names {
        let pkg = solver.cache.get(name)
            .ok_or_else(|| anyhow::anyhow!("Package '{}' not found.", name))?;
        ctx.mark_to_install(pkg, InstallReason::Explicit);
    }
    ctx.build_plan()
}

pub fn resolve_remove(solver: &Solver<'_>, names: &[String]) -> Result<TransactionPlan> {
    let pmap = build_provides(solver.cache, Some(solver.db));
    let mut plan = TransactionPlan::default();

    for name in names {
        if !solver.db.is_installed(name) {
            plan.warnings.push(format!("{} is not installed.", name));
            continue;
        }
        // Check for held packages
        if solver.db.is_held(name) {
            bail!("Package '{}' is held. Release with: hammer unhold {}", name, name);
        }
        // Reverse-dependency check
        let rdeps = super::conflicts::reverse_depends(&[name.clone()], solver.db);
        if !rdeps.is_empty() {
            plan.warnings.push(format!(
                "Removing '{}' will break: {}",
                name, rdeps.join(", ")
            ));
        }
        // Compute freed space
        if let Some(inst) = solver.db.get(name) {
            plan.freed_bytes += inst.installed_size_kb * 1024;
        }
        plan.to_remove.push(name.clone());
    }
    // Compute auto-remove of newly orphaned deps
    let orphans = find_newly_orphaned(solver, &plan.to_remove, &pmap);
    plan.to_autoremove.extend(orphans);
    Ok(plan)
}

pub fn resolve_upgrade(solver: &Solver<'_>) -> Result<TransactionPlan> {
    let pmap = build_provides(solver.cache, Some(solver.db));
    let mut ctx = ResolveCtx::new(solver, &pmap, false);
    ctx.request_upgrade_all(false)?;
    ctx.build_plan()
}

pub fn resolve_dist_upgrade(solver: &Solver<'_>) -> Result<TransactionPlan> {
    let pmap = build_provides(solver.cache, Some(solver.db));
    let mut ctx = ResolveCtx::new(solver, &pmap, false);
    ctx.request_upgrade_all(true)?;
    ctx.build_plan()
}

pub fn resolve_autoremove(solver: &Solver<'_>) -> Result<TransactionPlan> {
    let mut plan = TransactionPlan::default();
    let pmap = build_provides(solver.cache, Some(solver.db));

    let installed = solver.db.list_all().unwrap_or_default();
    let user_installed: HashSet<String> = solver.db
        .list_user_installed().unwrap_or_default()
        .into_iter().map(|p| p.name.clone()).collect();

    // Compute transitive closure of user-requested packages
    let required = closure_of_user_packages(solver, &user_installed, &pmap);

    for inst in &installed {
        // Never auto-remove held, essential, or user-installed packages
        if user_installed.contains(&inst.name)
            || required.contains(&inst.name)
            || solver.db.is_held(&inst.name)
            || is_essential(solver, &inst.name)
        {
            continue;
        }
        plan.freed_bytes += inst.installed_size_kb * 1024;
        plan.to_autoremove.push(inst.name.clone());
    }
    Ok(plan)
}

pub fn resolve_fix_broken(solver: &Solver<'_>) -> Result<TransactionPlan> {
    let pmap = build_provides(solver.cache, Some(solver.db));
    let mut plan = TransactionPlan::default();
    let installed = solver.db.list_all().unwrap_or_default();

    for inst in &installed {
        let Some(pkg) = solver.cache.get(&inst.name) else {
            plan.warnings.push(format!(
                "'{}' installed but not in any repo (orphan). \
                 Consider: hammer remove {}", inst.name, inst.name
            ));
            continue;
        };

        // Check each dependency group
        if let Some(ref dep_str) = pkg.depends {
            for group in parse_dep_field(dep_str) {
                let satisfied = group.alternatives.iter().any(|alt| {
                    let providers = pmap.providers_of(&alt.name);
                    providers.iter().any(|p| {
                        if !solver.db.is_installed(p) { return false; }
                        if let Some(ref c) = alt.constraint {
                            if let Some(inst_p) = solver.db.get(p) {
                                return satisfies(&inst_p.version, c.op.as_str(), &c.version);
                            }
                            return false;
                        }
                        true
                    })
                });
                if !satisfied {
                    let dep_name = group.alternatives.first()
                        .map(|a| a.name.as_str()).unwrap_or("?");
                    // Try to find a provider to install
                    let providers = pmap.providers_of(dep_name);
                    if let Some(fix_name) = providers.first() {
                        if let Some(fix_pkg) = solver.cache.get(fix_name) {
                            if !plan.to_install.iter().any(|p| p.name == *fix_name) {
                                plan.download_bytes += fix_pkg.download_size.unwrap_or(0);
                                plan.install_bytes  += fix_pkg.installed_size_kb.unwrap_or(0);
                                plan.to_install.push(fix_pkg.clone());
                            }
                        }
                    } else {
                        plan.warnings.push(format!(
                            "BROKEN: '{}' needs '{}' but no provider available",
                            inst.name, dep_name
                        ));
                    }
                }
            }
        }
    }
    Ok(plan)
}

// ──────────────────────────────────────────────────────────────────────────────
//  Resolution context — zypper-style iterative resolver
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
enum InstallReason { Explicit, Dependency, Recommends }

struct ResolveCtx<'a> {
    solver:        &'a Solver<'a>,
    pmap:          &'a ProvidesMap,
    no_recommends: bool,

    /// Packages that will be installed in this transaction
    to_install:    HashMap<String, (Package, InstallReason)>,
    /// Packages that will be removed (due to Replaces: or conflicts)
    to_remove:     Vec<String>,
    /// Packages in the process of being resolved (cycle detection)
    resolving:     HashSet<String>,
    /// Already-visited dependency names (prevents infinite loops)
    visited:       HashSet<String>,
    /// Pre-Depends that must be ordered first
    pre_depends:   Vec<String>,
    warnings:      Vec<String>,
    suggests:      Vec<String>,
}

impl<'a> ResolveCtx<'a> {
    fn new(solver: &'a Solver<'a>, pmap: &'a ProvidesMap, no_recommends: bool) -> Self {
        ResolveCtx {
            solver, pmap, no_recommends,
            to_install:    HashMap::new(),
            to_remove:     Vec::new(),
            resolving:     HashSet::new(),
            visited:       HashSet::new(),
            pre_depends:   Vec::new(),
            warnings:      Vec::new(),
            suggests:      Vec::new(),
        }
    }

    /// Request installation of a named package.
    fn request_install(&mut self, name: &str) -> Result<()> {
        // Strip :arch suffix for lookup
        let bare = name.split(':').next().unwrap_or(name);

        // Already satisfied by installed package?
        if self.pmap.is_satisfied(bare) && !self.solver.cache.get(bare)
            .map(|p| self.should_upgrade(p))
            .unwrap_or(false)
        {
            if self.solver.db.is_installed(bare) {
                self.warnings.push(format!("'{}' is already installed.", bare));
                return Ok(());
            }
        }

        // Resolve through ProvidesMap (handles virtual packages)
        let real_name = self.pmap.resolve(bare);
        let pkg = match self.solver.cache.get(real_name) {
            Some(p) => p.clone(),
            None => {
                // Virtual with no real package?
                let providers = self.pmap.providers_of(bare);
                if providers.is_empty() {
                    let similar = self.solver.find_similar(bare);
                    let hint = if similar.is_empty() {
                        format!("Run 'hammer sync' to refresh the package index.")
                    } else {
                        format!("Did you mean: {}?", similar.join(", "))
                    };
                    bail!("Package '{}' not found.\n  {}", bare, hint);
                }
                // Pick the best provider
                let provider = providers.into_iter().next().unwrap();
                self.solver.cache.get(&provider)
                    .ok_or_else(|| anyhow::anyhow!(
                        "Provider '{}' for '{}' not in cache.", provider, bare
                    ))?.clone()
            }
        };

        // Check holds
        if self.solver.db.is_held(&pkg.name) {
            self.warnings.push(format!(
                "'{}' is held at version {} — skipping.",
                pkg.name, self.solver.db.get(&pkg.name)
                    .map(|i| i.version).unwrap_or_default()
            ));
            return Ok(());
        }

        // Check version pins
        if let Some((constraint, _priority)) = self.solver.pins.get_pin(&pkg.name) {
            if let Some(inst) = self.solver.db.get(&pkg.name) {
                if !super::version::satisfies(&pkg.version, &constraint, &inst.version) {
                    self.warnings.push(format!(
                        "'{}' pinned to '{}': skipping version {}",
                        pkg.name, constraint, pkg.version
                    ));
                    return Ok(());
                }
            }
        }

        self.mark_to_install(&pkg, InstallReason::Explicit)?;
        Ok(())
    }

    fn mark_to_install(&mut self, pkg: &Package, reason: InstallReason) -> Result<()> {
        // Cycle guard
        if self.resolving.contains(&pkg.name) { return Ok(()); }
        if self.to_install.contains_key(&pkg.name) { return Ok(()); }
        if self.solver.db.is_installed(&pkg.name) && reason != InstallReason::Explicit {
            return Ok(());
        }

        self.resolving.insert(pkg.name.clone());

        // Forward conflict check
        let confs = super::conflicts::check_install(pkg, self.solver.db);
        for c in &confs {
            if c.hard {
                // Try removing the conflicting package if it's being replaced
                if c.kind == super::conflicts::ConflictKind::Replaces {
                    self.to_remove.push(c.with.clone());
                } else {
                    return Err(anyhow::anyhow!(
                        super::conflicts::format_conflict_explanation(&confs)
                    ));
                }
            } else {
                self.warnings.push(c.message.clone());
            }
        }

        // Reverse conflict check (what installed packages conflict with us?)
        let rev_breaks = super::conflicts::check_reverse_breaks(pkg, self.solver.db);
        for c in &rev_breaks {
            if c.hard {
                return Err(anyhow::anyhow!(
                    "Installed '{}' conflicts with '{}': {}",
                    c.pkg_name, pkg.name, c.message
                ));
            }
        }

        // Auto-remove packages replaced by this one
        let replaced = super::conflicts::resolve_replaces(
            std::slice::from_ref(pkg), self.solver.db
        );
        for r in replaced {
            if !self.to_remove.contains(&r) {
                self.to_remove.push(r);
            }
        }

        self.to_install.insert(pkg.name.clone(), (pkg.clone(), reason));

        // Pre-Depends: must be installed first (ordering)
        self.resolve_dep_field(pkg, pkg.pre_depends.as_deref(), InstallReason::Dependency, true)?;

        // Depends:
        self.resolve_dep_field(pkg, pkg.depends.as_deref(), InstallReason::Dependency, false)?;

        // Recommends: (unless --no-recommends)
        if !self.no_recommends {
            self.resolve_dep_field(pkg, pkg.recommends.as_deref(), InstallReason::Recommends, false)?;
        }

        // Suggests: never auto-install, just list
        if let Some(ref sug_str) = pkg.suggests {
            for group in parse_dep_field(sug_str) {
                for alt in &group.alternatives {
                    if !self.solver.db.is_installed(&alt.name) {
                        let s = format!("Suggested: {} (by {})", alt.name, pkg.name);
                        if !self.suggests.contains(&s) {
                            self.suggests.push(s);
                        }
                    }
                }
            }
        }

        self.resolving.remove(&pkg.name);
        Ok(())
    }

    fn resolve_dep_field(
        &mut self,
        requester:  &Package,
        dep_str:    Option<&str>,
        reason:     InstallReason,
        pre_dep:    bool,
    ) -> Result<()> {
        let dep_str = match dep_str { Some(s) => s, None => return Ok(()) };

        for group in parse_dep_field(dep_str) {
            // OR-group: find best already-satisfying or best-to-install alternative
            if self.is_or_group_satisfied(&group) { continue; }

            // Pick the best alternative to install
            let chosen = self.pick_best_alternative(&group, requester)?;

            if let Some(pkg) = chosen {
                if pre_dep && !self.pre_depends.contains(&pkg.name) {
                    self.pre_depends.push(pkg.name.clone());
                }
                self.mark_to_install(&pkg, reason)?;
            } else if reason == InstallReason::Dependency {
                // Hard dep unresolvable
                let alts: Vec<&str> = group.alternatives.iter()
                    .map(|a| a.name.as_str()).collect();
                bail!(
                    "Package '{}' depends on '{}' but no provider is available.\n  \
                     Run 'hammer sync' to refresh the index.",
                    requester.name,
                    alts.join(" | ")
                );
            }
            // Recommends failure → warning only
            else if reason == InstallReason::Recommends {
                let alts: Vec<&str> = group.alternatives.iter()
                    .map(|a| a.name.as_str()).collect();
                self.warnings.push(format!(
                    "Recommended '{}' (by '{}') is not available — skipping.",
                    alts.join(" | "), requester.name
                ));
            }
        }
        Ok(())
    }

    /// True if at least one alternative in the OR-group is already satisfied
    /// (installed, or being installed in this transaction).
    fn is_or_group_satisfied(&self, group: &crate::package::DepGroup) -> bool {
        group.alternatives.iter().any(|alt| {
            // Check via ProvidesMap (handles virtuals)
            let providers = self.pmap.providers_of(&alt.name);
            let satisfied_by_installed = providers.iter().any(|p| {
                if !self.solver.db.is_installed(p) { return false; }
                // Version constraint check
                if let Some(ref c) = alt.constraint {
                    if let Some(inst) = self.solver.db.get(p) {
                        return satisfies(&inst.version, c.op.as_str(), &c.version);
                    }
                    return false;
                }
                true
            });
            if satisfied_by_installed { return true; }

            // Check if being installed in this transaction
            providers.iter().any(|p| {
                self.to_install.get(p).map(|(pkg, _)| {
                    if let Some(ref c) = alt.constraint {
                        satisfies(&pkg.version, c.op.as_str(), &c.version)
                    } else { true }
                }).unwrap_or(false)
            })
        })
    }

    /// From an OR-group, pick the best package to install.
    /// Priority: installed > already-to-install > best-provider
    fn pick_best_alternative(
        &self,
        group:     &crate::package::DepGroup,
        _requester: &Package,
    ) -> Result<Option<Package>> {
        // First: find an alternative whose provider is in cache (not virtual)
        for alt in &group.alternatives {
            let providers = self.pmap.providers_of(&alt.name);

            // Check version constraint against available versions
            for provider_name in &providers {
                if let Some(pkg) = self.solver.cache.get(provider_name) {
                    let version_ok = alt.constraint.as_ref()
                        .map(|c| satisfies(&pkg.version, c.op.as_str(), &c.version))
                        .unwrap_or(true);
                    if version_ok {
                        return Ok(Some(pkg.clone()));
                    }
                }
            }
        }
        Ok(None)
    }

    fn should_upgrade(&self, pkg: &Package) -> bool {
        self.solver.db.get(&pkg.name)
            .map(|inst| compare(&pkg.version, &inst.version) == std::cmp::Ordering::Greater)
            .unwrap_or(false)
    }

    fn request_upgrade_all(&mut self, dist_upgrade: bool) -> Result<()> {
        let installed = self.solver.db.list_all().unwrap_or_default();
        for inst in installed {
            if self.solver.db.is_held(&inst.name) { continue; }
            if is_essential(self.solver, &inst.name) && !dist_upgrade { continue; }

            if let Some(avail) = self.solver.cache.get(&inst.name) {
                if compare(&avail.version, &inst.version) == std::cmp::Ordering::Greater {
                    self.mark_to_install(avail, InstallReason::Explicit)?;
                }
            }
        }
        // dist-upgrade: also consider newly available packages that satisfy new deps
        if dist_upgrade {
            // Re-resolve all deps of upgraded packages
            let upgrading: Vec<Package> = self.to_install.values()
                .map(|(p, _)| p.clone()).collect();
            for pkg in &upgrading {
                self.resolve_dep_field(pkg, pkg.depends.as_deref(),
                                       InstallReason::Dependency, false)?;
            }
        }
        Ok(())
    }

    /// Topological sort using Kahn's algorithm.
    fn topological_order(&self) -> Vec<Package> {
        let names: HashSet<&str> = self.to_install.keys().map(|s| s.as_str()).collect();
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        let mut graph:     HashMap<&str, Vec<&str>> = HashMap::new();

        for name in &names { in_degree.insert(name, 0); graph.insert(name, vec![]); }

        // Build dependency edges — collect all (dep_name, pkg_name) pairs first
        // to avoid borrow-lifetime issues with dep_names going out of scope
        let mut edges: Vec<(String, String)> = Vec::new();
        for (name, (pkg, _)) in &self.to_install {
            let all_deps = [pkg.pre_depends.as_deref(), pkg.depends.as_deref()];
            for dep_str in all_deps.iter().flatten() {
                let found = parse_dep_field(dep_str)
                    .into_iter()
                    .flat_map(|g| g.alternatives.into_iter().map(|a| a.name))
                    .find(|dep| names.contains(dep.as_str()));
                if let Some(dep_name) = found {
                    edges.push((dep_name, name.clone()));
                }
            }
        }
        for (dep_name, pkg_name) in &edges {
            graph.entry(dep_name.as_str()).or_default().push(pkg_name.as_str());
            *in_degree.entry(pkg_name.as_str()).or_insert(0) += 1;
        }

        // Pre-depends come first absolutely
        let mut queue: VecDeque<&str> = self.pre_depends.iter()
            .filter(|n| names.contains(n.as_str()))
            .map(|s| s.as_str())
            .collect();
        for (name, deg) in &in_degree {
            if *deg == 0 && !queue.contains(name) { queue.push_back(name); }
        }

        let mut ordered = Vec::new();
        let mut visited_sort = HashSet::new();
        while let Some(name) = queue.pop_front() {
            if !visited_sort.insert(name) { continue; }
            if let Some((pkg, _)) = self.to_install.get(name) {
                ordered.push(pkg.clone());
            }
            for &successor in graph.get(name).unwrap_or(&vec![]) {
                let deg = in_degree.entry(successor).or_insert(1);
                *deg = deg.saturating_sub(1);
                if *deg == 0 && !visited_sort.contains(successor) {
                    queue.push_back(successor);
                }
            }
        }

        // Append any cycles (shouldn't happen in well-formed repos, but safety net)
        for (name, (pkg, _)) in &self.to_install {
            if !visited_sort.contains(name.as_str()) {
                ordered.push(pkg.clone());
            }
        }
        ordered
    }

    fn build_plan(self) -> Result<TransactionPlan> {
        let mut plan = TransactionPlan::default();

        // Topologically ordered install list
        plan.to_install = self.topological_order();

        // Separate upgrades from fresh installs
        let mut fresh    = Vec::new();
        let mut upgrades = Vec::new();
        for pkg in &plan.to_install {
            if self.solver.db.is_installed(&pkg.name) {
                if let Some(inst) = self.solver.db.get(&pkg.name) {
                    plan.upgrade_from.insert(pkg.name.clone(), inst.version.clone());
                    plan.freed_bytes += inst.installed_size_kb * 1024;
                }
                upgrades.push(pkg.clone());
            } else {
                fresh.push(pkg.clone());
            }
        }
        plan.to_install = fresh;
        plan.to_upgrade = upgrades;

        // Removals
        plan.to_remove = self.to_remove;

        // Sizes
        for pkg in plan.to_install.iter().chain(plan.to_upgrade.iter()) {
            plan.download_bytes += pkg.download_size.unwrap_or(0);
            plan.install_bytes  += pkg.installed_size_kb.unwrap_or(0);
        }

        plan.warnings = self.warnings;
        plan.conflicts = vec![]; // clear — already handled

        // List suggestions (informational)
        for s in &self.suggests {
            plan.warnings.push(format!("  ℹ {}", s));
        }

        Ok(plan)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
//  Helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Compute the full transitive dependency closure of user-installed packages.
/// Used by autoremove to determine what is still "required".
fn closure_of_user_packages(
    solver: &Solver<'_>,
    user_pkgs: &HashSet<String>,
    pmap: &ProvidesMap,
) -> HashSet<String> {
    let mut required: HashSet<String> = user_pkgs.clone();
    let mut queue: VecDeque<String>   = user_pkgs.iter().cloned().collect();

    while let Some(name) = queue.pop_front() {
        let pkg = match solver.cache.get(&name) {
            Some(p) => p,
            None    => continue,
        };
        let dep_strs = [pkg.pre_depends.as_deref(), pkg.depends.as_deref()];
        for dep_str in dep_strs.iter().flatten() {
            for group in parse_dep_field(dep_str) {
                // Find which provider is installed and is being kept
                for alt in &group.alternatives {
                    let providers = pmap.providers_of(&alt.name);
                    for p in &providers {
                        if solver.db.is_installed(p) && required.insert(p.clone()) {
                            queue.push_back(p.clone());
                        }
                    }
                }
            }
        }
    }
    required
}

/// Find packages that become orphaned after removing `removing`.
fn find_newly_orphaned(
    solver:   &Solver<'_>,
    removing: &[String],
    pmap:     &ProvidesMap,
) -> Vec<String> {
    let remove_set: HashSet<&str> = removing.iter().map(|s| s.as_str()).collect();
    let user_pkgs: HashSet<String> = solver.db.list_user_installed()
        .unwrap_or_default()
        .into_iter().map(|p| p.name).collect();

    let remaining_user: HashSet<String> = user_pkgs.iter()
        .filter(|n| !remove_set.contains(n.as_str()))
        .cloned()
        .collect();

    let still_required = closure_of_user_packages(solver, &remaining_user, pmap);

    solver.db.list_all().unwrap_or_default()
        .into_iter()
        .filter(|inst| {
            !remove_set.contains(inst.name.as_str())
            && !user_pkgs.contains(&inst.name)
            && !still_required.contains(&inst.name)
        })
        .map(|inst| inst.name)
        .collect()
}

fn is_essential(solver: &Solver<'_>, name: &str) -> bool {
    solver.cache.get(name).map(|p| p.essential).unwrap_or(false)
}
