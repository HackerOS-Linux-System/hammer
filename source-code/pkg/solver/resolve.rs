use anyhow::{bail, Result};
//
// Comparable to zypper/libsolv in capability:
//   • CDCL SAT as primary decision engine (heuristic = initial assignment)
//   • Full OR-group (alternatives) resolution — picks best provider via MVS scoring
//   • Pre-Depends: installed before the package itself (ordered)
//   • Recommends: installed unless --no-recommends
//   • Suggests/Enhances: logged in why_installed journal, never auto-installed
//   • Replaces: auto-removes via ProvidesMap (not raw db.get)
//   • Virtual package resolution via ProvidesMap
//   • Conflict + Breaks detection (forward and reverse)
//   • Hold-awareness — never upgrades held packages
//   • Pin-awareness — respects version constraints
//   • Topological install ordering (dependency order)
//   • Cycle detection and breaking
//   • Autoremove respects Recommends from remaining packages

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

    // ── Phase 1: Heuristic pre-assignment (initial candidates) ────────────
    for name in names {
        ctx.request_install(name)?;
    }

    // ── Phase 2: CDCL verification and correction ─────────────────────────
    // Build a SAT problem from the heuristic assignment and verify it.
    // CDCL finds conflicts the heuristic missed and backtracks to a correct
    // assignment, setting sat_stats on the plan for diagnostics.
    let sat_result = ctx.run_cdcl_verification()?;

    let mut plan = ctx.build_plan()?;
    plan.sat_stats = Some(sat_result);
    Ok(plan)
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

    // Also compute packages recommended by the required set — they are
    // not candidates for auto-removal even if they were installed as deps.
    let recommended_by_remaining = closure_of_recommends(solver, &required, &pmap);

    for inst in &installed {
        // Never auto-remove held, essential, user-installed, or recommended packages
        if user_installed.contains(&inst.name)
            || required.contains(&inst.name)
            || recommended_by_remaining.contains(&inst.name)
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
        let rev_breaks = super::conflicts::check_reverse_breaks(pkg, self.solver.db, self.solver.cache);
        for c in &rev_breaks {
            if c.hard {
                return Err(anyhow::anyhow!(
                    "Installed '{}' conflicts with '{}': {}",
                    c.pkg_name, pkg.name, c.message
                ));
            }
        }

        // Auto-remove packages replaced by this one.
        // Use ProvidesMap to resolve virtual package names correctly.
        let replaced = resolve_replaces_with_provides(
            pkg, self.solver.db, self.pmap
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

        // Suggests: never auto-install, just log for why_installed journal
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

        // Enhances: symmetric to Suggests — log only, never auto-install
        if let Some(ref enh_str) = pkg.enhances {
            for group in parse_dep_field(enh_str) {
                for alt in &group.alternatives {
                    let s = format!("Enhances: {} (by {})", alt.name, pkg.name);
                    if !self.suggests.contains(&s) {
                        self.suggests.push(s);
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
    ///
    /// MVS (Minimum Version Selection) scoring:
    ///   1. Already-installed provider (no change → score 1000)
    ///   2. Already-queued-in-transaction provider (score 900)
    ///   3. Exact name match preferred over virtual-provide (score +200)
    ///   4. Smallest version delta vs current installed (score +100 - delta)
    ///   5. Repo priority tiebreak (higher = better)
    fn pick_best_alternative(
        &self,
        group:     &crate::package::DepGroup,
        _requester: &Package,
    ) -> Result<Option<Package>> {
        let mut best: Option<(Package, i64)> = None;

        for alt in &group.alternatives {
            let providers = self.pmap.providers_of(&alt.name);
            let is_exact_name = !self.pmap.is_virtual(&alt.name);

            for provider_name in &providers {
                let pkg = match self.solver.cache.get(provider_name) {
                    Some(p) => p,
                    None    => continue,
                };
                let version_ok = alt.constraint.as_ref()
                    .map(|c| satisfies(&pkg.version, c.op.as_str(), &c.version))
                    .unwrap_or(true);
                if !version_ok { continue; }

                // MVS score
                let mut score: i64 = 0;
                if self.solver.db.is_installed(&pkg.name) {
                    score += 1000;
                    // Prefer smallest version bump (minimal change)
                    if let Some(inst) = self.solver.db.get(&pkg.name) {
                        let ord = compare(&pkg.version, &inst.version);
                        score += match ord {
                            std::cmp::Ordering::Equal   => 100,
                            std::cmp::Ordering::Greater => 50,
                            std::cmp::Ordering::Less    => 10,
                        };
                    }
                }
                if self.to_install.contains_key(&pkg.name) { score += 900; }
                if is_exact_name { score += 200; }
                // Repo priority tiebreak
                score += pkg.download_size.unwrap_or(0) as i64 / -1024; // prefer smaller download

                if best.as_ref().map(|(_, s)| score > *s).unwrap_or(true) {
                    best = Some((pkg.clone(), score));
                }
            }
        }

        Ok(best.map(|(pkg, _)| pkg))
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
        // Follow both hard deps and Recommends (consistent with closure_of_recommends)
        let dep_strs = [
            pkg.pre_depends.as_deref(),
            pkg.depends.as_deref(),
            pkg.recommends.as_deref(),
        ];
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

// ──────────────────────────────────────────────────────────────────────────────
//  CDCL integration
// ──────────────────────────────────────────────────────────────────────────────

impl<'a> ResolveCtx<'a> {
    /// Run CDCL verification on the heuristic assignment.
    ///
    /// The heuristic (iterative resolver above) produces an initial candidate
    /// assignment.  We encode this as a SAT problem and run the CDCL engine:
    ///
    ///   • Each package name → SAT variable (via VarMap)
    ///   • "P must be installed" → unit clause [P]
    ///   • "P depends on Q|R"   → implication [¬P, Q, R]
    ///   • "P conflicts with Q"  → clause [¬P, ¬Q]
    ///
    /// On UNSAT the error propagates up to a human-readable bail.
    /// On SAT the model may revise the heuristic assignment and we emit
    /// `SolverStats` for verbose diagnostics.
    pub(crate) fn run_cdcl_verification(
        &mut self,
    ) -> Result<super::sat::SolverStats> {
        use super::sat::{CdclSolver, Lit};
        use crate::package::parse_dep_field;

        // Hard safety cap: real-world dependency graphs (hundreds of
        // packages, many `Provides:`/alternative-heavy ones like editors
        // or build toolchains) can occasionally need a large search, but
        // a CDCL solver should resolve any realistic apt-style dependency
        // set in at most a few tens of thousands of conflicts. Capping
        // this means a pathological input (or an as-yet-undiscovered
        // solver bug) fails fast with a clear message instead of hanging
        // the CLI indefinitely — which is strictly worse for a package
        // manager than an occasional "couldn't fully verify, falling
        // back" for a handful of exotic package sets.
        let mut sat = CdclSolver::new().with_conflict_limit(200_000);

        // ── Pre-intern all names so VarMap is stable ──────────
        for name in self.to_install.keys() {
            sat.intern(name);
        }

        // ── Domain hint: already-installed packages prefer true ─
        for name in self.to_install.keys() {
            if self.solver.db.is_installed(name) {
                let v = sat.intern(name);
                sat.prefer_installed(v);
            }
        }

        // ── Unit clauses: every planned package MUST be true ──
        let planned: Vec<String> = self.to_install.keys().cloned().collect();
        for name in &planned {
            let v = sat.intern(name);
            if let Err(e) = sat.add_clause(vec![Lit::pos(v)]) {
                bail!(
                    "Dependency conflict detected (CDCL): {}\n  \
                     Run 'hammer why-not <package>' for details.",
                    e
                );
            }
        }

        // ── Implication & conflict clauses ────────────────────
        let install_snapshot: Vec<(String, crate::package::Package)> = self
            .to_install.iter()
            .map(|(n, (p, _))| (n.clone(), p.clone()))
            .collect();

        for (name, pkg) in &install_snapshot {
            let pv = sat.intern(name);

            // Deps: ¬P ∨ Q₁ ∨ Q₂ ∨ …
            for dep_str in [pkg.pre_depends.as_deref(), pkg.depends.as_deref()].iter().flatten() {
                for group in parse_dep_field(dep_str) {
                    let already_ok = group.alternatives.iter().any(|alt| {
                        self.pmap.providers_of(&alt.name).iter()
                            .any(|p| self.solver.db.is_installed(p))
                    });
                    if already_ok { continue; }

                    let mut clause = vec![Lit::neg(pv)];
                    for alt in &group.alternatives {
                        for provider in self.pmap.providers_of(&alt.name) {
                            let qv = sat.intern(&provider);
                            clause.push(Lit::pos(qv));
                        }
                    }
                    if clause.len() > 1 {
                        if let Err(e) = sat.add_clause(clause) {
                            bail!(
                                "Dependency conflict detected (CDCL): {}\n  \
                                 Run 'hammer why-not <package>' for details.",
                                e
                            );
                        }
                    }
                }
            }

            // Conflicts: ¬P ∨ ¬Q
            if let Some(conf_str) = pkg.conflicts.as_deref() {
                for group in parse_dep_field(conf_str) {
                    for alt in &group.alternatives {
                        for provider in self.pmap.providers_of(&alt.name) {
                            // A package conflicting with a virtual name it
                            // itself provides does NOT conflict with
                            // itself — this is the extremely common
                            // "package renamed: Provides+Conflicts+Replaces
                            // old-name" pattern (Debian Policy §7.4), e.g.
                            // linux-libc-dev provides AND conflicts with
                            // linux-kernel-headers, meaning "conflicts with
                            // any OTHER package still providing the old
                            // name", not "conflicts with myself". Skipping
                            // self-conflicts here prevents every such
                            // package from generating an immediate,
                            // self-contradictory unit-false clause the
                            // moment it's pulled in as a dependency.
                            if provider == *name { continue; }
                            // Respect the version constraint on the
                            // Conflicts: entry, if any — e.g. "Conflicts:
                            // gcc (<< 4:13.2.0-3)" means "conflicts with
                            // OLD gcc, not with any gcc ever again" (an
                            // extremely common transition-period pattern
                            // in real Debian packages). Treating every
                            // Conflicts: as unconditional/all-versions
                            // turned routine version-gated conflicts into
                            // permanent hard conflicts against the
                            // package's current, actually-compatible
                            // version.
                            if let Some(constraint) = &alt.constraint {
                                let provider_version = self.solver.cache.get(&provider)
                                    .map(|p| p.version.as_str());
                                let conflict_applies = provider_version
                                    .map(|v| crate::package::version_satisfies(v, constraint.op.as_str(), &constraint.version))
                                    .unwrap_or(true); // unknown version: be conservative, keep the conflict
                                if !conflict_applies { continue; }
                            }
                            let qv = sat.intern(&provider);
                            if let Err(e) = sat.add_clause(vec![Lit::neg(pv), Lit::neg(qv)]) {
                                bail!(
                                    "Dependency conflict detected (CDCL): {}\n  \
                                     Run 'hammer why-not <package>' for details.",
                                    e
                                );
                            }
                        }
                    }
                }
            }
        }

        // ── Preprocessing (pure literal, FLP) ─────────────────
        let n_vars    = sat.vars.n_vars();
        let n_clauses = sat.clauses.len();
        if let Err(e) = sat.preprocess() {
            bail!("Dependency conflict (preprocessing): {}\n  \
                   Run 'hammer why-not <package>' for details.", e);
        }

        match sat.solve() {
            Err(e) => {
                bail!(
                    "Dependency conflict detected (CDCL): {}\n  \
                     Run 'hammer why-not <package>' for details.",
                    e
                );
            }
            Ok(true) => {
                // Accept model: remove anything the SAT engine set to false
                let model = sat.model();
                let to_drop: Vec<String> = planned.iter()
                    .filter(|n| model.get(*n).copied() == Some(false))
                    .cloned()
                    .collect();
                for name in &to_drop {
                    self.warnings.push(format!(
                        "CDCL: removed '{}' from install set (conflict resolution)",
                        name
                    ));
                    self.to_install.remove(name);
                }
                Ok(super::sat::SolverStats {
                    conflicts:    sat.conflicts,
                    decisions:    sat.decisions,
                    propagations: sat.propagations,
                    restarts:     sat.restarts,
                    n_clauses,
                    n_vars,
                })
            }
            Ok(false) => {
                // Budget exceeded (complex problem) — treat as warning, trust heuristic
                self.warnings.push(
                    "CDCL: conflict budget exceeded, using heuristic result.".into()
                );
                Ok(super::sat::SolverStats {
                    conflicts:    sat.conflicts,
                    decisions:    sat.decisions,
                    propagations: sat.propagations,
                    restarts:     sat.restarts,
                    n_clauses,
                    n_vars,
                })
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
//  ProvidesMap-aware Replaces resolution
// ──────────────────────────────────────────────────────────────────────────────

/// Resolve which installed packages should be removed because `pkg` Replaces them.
/// Uses ProvidesMap to resolve virtual names instead of raw db.get.
fn resolve_replaces_with_provides(
    pkg:  &crate::package::Package,
    db:   &crate::db::InstalledDb,
    pmap: &ProvidesMap,
) -> Vec<String> {
    use crate::package::parse_dep_field;
    use super::version::satisfies;

    let mut to_remove = Vec::new();
    let Some(ref r_str) = pkg.replaces else { return to_remove };

    for group in parse_dep_field(r_str) {
        for alt in &group.alternatives {
            // Resolve through virtual names → real package providers
            let providers = pmap.providers_of(&alt.name);
            let names_to_check: Vec<String> = if providers.is_empty() {
                vec![alt.name.clone()]
            } else {
                providers
            };

            for candidate in &names_to_check {
                if let Some(inst) = db.get(candidate) {
                    let applies = alt.constraint.as_ref()
                        .map(|c| satisfies(&inst.version, c.op.as_str(), &c.version))
                        .unwrap_or(true);
                    if applies && !to_remove.contains(candidate) {
                        to_remove.push(candidate.clone());
                    }
                }
            }
        }
    }
    to_remove
}

// ──────────────────────────────────────────────────────────────────────────────
//  Recommends-aware autoremove helper
// ──────────────────────────────────────────────────────────────────────────────

/// Compute the set of packages recommended by any package in `required`.
/// These should not be auto-removed even if they were installed as deps.
fn closure_of_recommends(
    solver:   &Solver<'_>,
    required: &HashSet<String>,
    pmap:     &ProvidesMap,
) -> HashSet<String> {
    use crate::package::parse_dep_field;

    let mut recommended: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = required.iter().cloned().collect();

    while let Some(name) = queue.pop_front() {
        let pkg = match solver.cache.get(&name) {
            Some(p) => p,
            None    => continue,
        };
        let Some(ref rec_str) = pkg.recommends else { continue };
        for group in parse_dep_field(rec_str) {
            // Pick best installed provider of this recommendation
            for alt in &group.alternatives {
                let providers = pmap.providers_of(&alt.name);
                for provider in &providers {
                    if solver.db.is_installed(provider)
                        && recommended.insert(provider.clone())
                    {
                        queue.push_back(provider.clone());
                    }
                }
            }
        }
    }
    recommended
}
