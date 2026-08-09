pub(crate) mod conflicts;
// `dpll` (a second, independent, unused DPLL-based solver implementation)
// was removed — nothing in the codebase referenced it, and it had its own
// unfixed correctness bugs (see ROADMAP.md). `sat` (CdclSolver) is the
// single, tested, actually-used SAT engine.
pub(crate) mod error;
pub(crate) mod provides;
pub(crate) mod resolve;
pub(crate) mod sat;
pub(crate) mod version;
#[cfg(test)]
pub(crate) mod tests;

use anyhow::Result;
use owo_colors::OwoColorize;
use std::collections::HashMap;

use crate::cache::PackageCache;
use crate::db::InstalledDb;
use crate::package::{parse_dep_field, Package};
use crate::pins::PinDb;
use crate::multi_arch::MultiArchDb;
use provides::build as build_provides;
use provides::ProvidesMap;

// ──────────────────────────────────────────────────────────────────────────────
//  TransactionPlan
// ──────────────────────────────────────────────────────────────────────────────

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
    pub sat_stats:      Option<sat::SolverStats>,
}

impl TransactionPlan {
    pub fn is_empty(&self) -> bool {
        self.to_install.is_empty()
            && self.to_upgrade.is_empty()
            && self.to_remove.is_empty()
            && self.to_autoremove.is_empty()
    }

    pub fn print_summary(&self, verbose: bool) {
        if !self.to_install.is_empty() {
            println!("  {} {} package(s) to install:",
                     "↓".bright_cyan().bold(), self.to_install.len());
            for p in &self.to_install {
                println!("    {} {}  ({})", "·".dimmed(), p.name.cyan(), p.version.dimmed());
            }
        }
        if !self.to_upgrade.is_empty() {
            println!("  {} {} package(s) to upgrade:",
                     "↑".bright_yellow().bold(), self.to_upgrade.len());
            for p in &self.to_upgrade {
                let from = self.upgrade_from.get(&p.name).map(|s| s.as_str()).unwrap_or("?");
                println!("    {} {}  {} → {}",
                         "·".dimmed(), p.name.cyan(),
                         from.dimmed(), p.version.bright_yellow());
            }
        }
        if !self.to_remove.is_empty() {
            println!("  {} {} package(s) to remove:",
                     "✘".red().bold(), self.to_remove.len());
            for name in &self.to_remove { println!("    {} {}", "·".dimmed(), name.red()); }
        }
        if !self.to_autoremove.is_empty() {
            println!("  {} {} package(s) to auto-remove:",
                     "✘".dimmed(), self.to_autoremove.len());
            for name in &self.to_autoremove {
                println!("    {} {}", "·".dimmed(), name.dimmed());
            }
        }
        // Warnings (includes suggestions)
        for w in &self.warnings {
            let icon = if w.starts_with("  ℹ") { "".to_string() }
                       else { format!("  {} ", "⚠".yellow().bold()) };
            println!("{}{}", icon, w.yellow());
        }
        if self.download_bytes > 0 {
            println!("  {} Download  : {}", "·".dimmed(), fmt_bytes(self.download_bytes));
        }
        if self.install_bytes > 0 {
            println!("  {} Disk usage: +{}", "·".dimmed(), fmt_bytes(self.install_bytes));
        }
        if self.freed_bytes > 0 {
            println!("  {} Freed     : -{}", "·".dimmed(), fmt_bytes(self.freed_bytes));
        }
        if verbose {
            if let Some(ref stats) = self.sat_stats {
                println!("  {} SAT: vars={} clauses={} conflicts={} decisions={} restarts={}",
                         "·".dimmed(),
                         stats.n_vars, stats.n_clauses,
                         stats.conflicts, stats.decisions, stats.restarts);
            }
        }
    }
}

fn fmt_bytes(b: u64) -> String {
    if b < 1024               { format!("{} B",   b) }
    else if b < 1024*1024     { format!("{:.1} KiB", b as f64 / 1024.0) }
    else if b < 1024*1024*1024{ format!("{:.1} MiB", b as f64 / (1024.0*1024.0)) }
    else                      { format!("{:.2} GiB", b as f64 / (1024.0*1024.0*1024.0)) }
}

// ──────────────────────────────────────────────────────────────────────────────
//  Solver backend enum
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default)]
pub enum SolverBackend {
    #[default]
    Cdcl,
    Dpll,
}

// ──────────────────────────────────────────────────────────────────────────────
//  Solver facade
// ──────────────────────────────────────────────────────────────────────────────

pub struct Solver<'a> {
    pub(crate) cache:      &'a PackageCache,
    pub(crate) db:         &'a InstalledDb,
    pub(crate) pins:       PinDb,
    pub(crate) multi_arch: MultiArchDb,
    pub(crate) backend:    SolverBackend,
}

impl<'a> Solver<'a> {
    pub fn new(cache: &'a PackageCache, db: &'a InstalledDb) -> Self {
        Solver {
            cache,
            db,
            pins:       PinDb::load().unwrap_or_default(),
            multi_arch: MultiArchDb::load(),
            backend:    SolverBackend::Cdcl,
        }
    }

    pub fn with_backend(mut self, b: SolverBackend) -> Self { self.backend = b; self }

    pub fn resolve_install(&self, names: &[String], no_recommends: bool)
        -> Result<TransactionPlan>
    { resolve::resolve_install(self, names, no_recommends) }

    pub fn resolve_reinstall(&self, names: &[String]) -> Result<TransactionPlan>
    { resolve::resolve_reinstall(self, names) }

    pub fn resolve_remove(&self, names: &[String]) -> Result<TransactionPlan>
    { resolve::resolve_remove(self, names) }

    pub fn resolve_upgrade(&self) -> Result<TransactionPlan>
    { resolve::resolve_upgrade(self) }

    pub fn resolve_dist_upgrade(&self) -> Result<TransactionPlan>
    { resolve::resolve_dist_upgrade(self) }

    pub fn resolve_autoremove(&self) -> Result<TransactionPlan>
    { resolve::resolve_autoremove(self) }

    pub fn resolve_fix_broken(&self) -> Result<TransactionPlan>
    { resolve::resolve_fix_broken(self) }

    // ── Diagnostics ───────────────────────────────────────────

    pub fn find_similar(&self, name: &str) -> Vec<String> {
        self.cache.search(&name.to_lowercase())
            .iter().take(6).map(|p| p.name.clone()).collect()
    }

    /// Full human-readable explanation of why a package can't be installed.
    /// Similar to `zypper why-not-installable`.
    pub fn explain_failure(&self, name: &str) -> String {
        let pmap = build_provides(self.cache, Some(self.db));
        let mut lines = Vec::new();

        match self.cache.get(name) {
            None => {
                lines.push(format!(
                    "  {} Package '{}' not found in any configured repository.",
                    "✗".red().bold(), name.bold()
                ));
                let similar = self.find_similar(name);
                if !similar.is_empty() {
                    lines.push(format!("  {} Did you mean: {}?",
                                       "ℹ".cyan(), similar.join(", ")));
                }
                lines.push(format!("  {} Run: {}", "→".dimmed(), "hammer sync".cyan()));
            }
            Some(pkg) => {
                lines.push(format!(
                    "  {} Cannot install '{}' {}:",
                    "✗".red().bold(), pkg.name.bold(), pkg.version.dimmed()
                ));

                // Check conflicts
                let confs = conflicts::check_install(pkg, self.db);
                for c in &confs {
                    let sym = if c.hard { "✗".red().bold().to_string() }
                              else      { "⚠".yellow().to_string() };
                    lines.push(format!("  {} {}", sym, c.message));
                }

                // Check reverse conflicts
                let rev = conflicts::check_reverse_breaks(pkg, self.db, self.cache);
                for c in &rev {
                    lines.push(format!("  {} {}", "✗".red().bold(), c.message));
                }

                // Check each dep group
                if let Some(ref dep_str) = pkg.depends {
                    for group in parse_dep_field(dep_str) {
                        let satisfied = group.alternatives.iter().any(|alt| {
                            pmap.providers_of(&alt.name).iter().any(|p| {
                                self.db.is_installed(p) || self.cache.get(p).is_some()
                            })
                        });
                        if !satisfied {
                            let alts: Vec<&str> = group.alternatives.iter()
                                .map(|a| a.name.as_str()).collect();
                            lines.push(format!(
                                "  {} Unmet dependency: {} (no provider available)",
                                "✗".red().bold(), alts.join(" | ").bold()
                            ));
                        }
                    }
                }

                // Hold check
                if self.db.is_held(name) {
                    lines.push(format!(
                        "  {} Package is held. Release with: {}",
                        "ℹ".cyan(), format!("hammer unhold {}", name).cyan()
                    ));
                }

                // Virtual package?
                if pmap.is_virtual(name) {
                    let providers = pmap.providers_of(name);
                    lines.push(format!(
                        "  {} '{}' is a virtual package. Providers: {}",
                        "ℹ".cyan(), name,
                        if providers.is_empty() { "none".to_string() }
                        else { providers.join(", ").cyan().to_string() }
                    ));
                }

                // If none of the direct (depth-1) checks above found
                // anything, the problem is very likely several levels
                // deep in the dependency tree — e.g. a package `vim`
                // depends on transitively conflicts with something else
                // already resolvable, which a depth-1 check can never
                // see. Walk the full transitive closure (same BFS as
                // `dependency_closure`, but with a parent pointer per
                // node so we can report the actual chain) and check
                // every package in it for the same conflict/reverse-
                // conflict problems checked above for the top-level
                // package.
                if lines.len() == 1 {
                    if let Some((path, problem)) = self.find_transitive_problem(name, &pmap) {
                        lines.push(format!(
                            "  {} Transitive problem found {} levels deep:",
                            "✗".red().bold(), path.len().saturating_sub(1)
                        ));
                        lines.push(format!("    {}", path.join(" → ").dimmed()));
                        lines.push(format!("  {} {}", "✗".red().bold(), problem));
                    } else {
                        lines.push(format!(
                            "  {} No single unmet dependency or direct conflict found, but the \
                             full dependency set is still unsatisfiable — this usually means two \
                             transitively-required packages conflict with each other rather than \
                             either one being individually broken.",
                            "ℹ".cyan()
                        ));
                    }
                }
            }
        }

        lines.join("\n")
    }

    /// BFS over the full transitive dependency closure of `root`,
    /// tracking a parent pointer per visited package so we can report the
    /// path from `root` down to whichever package first turns out to have
    /// a direct conflict, reverse-conflict, or unmet dependency of its
    /// own. Returns `None` if every package in the closure looks
    /// individually fine (meaning the real problem, if any, is a
    /// multi-package interaction the CDCL solver can see but this
    /// depth-first-style check cannot).
    fn find_transitive_problem(&self, root: &str, pmap: &ProvidesMap) -> Option<(Vec<String>, String)> {
        let mut visited: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut queue: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        queue.push_back(root.to_string());
        visited.insert(root.to_string(), String::new()); // root has no parent

        fn reconstruct(visited: &std::collections::HashMap<String, String>, mut node: String) -> Vec<String> {
            let mut path = vec![node.clone()];
            while let Some(parent) = visited.get(&node) {
                if parent.is_empty() { break; }
                path.push(parent.clone());
                node = parent.clone();
            }
            path.reverse();
            path
        }

        while let Some(name) = queue.pop_front() {
            let Some(pkg) = self.cache.get(&name) else { continue };

            if name != root {
                let confs = conflicts::check_install(pkg, self.db);
                if let Some(c) = confs.iter().find(|c| c.hard) {
                    return Some((reconstruct(&visited, name), c.message.clone()));
                }
                let rev = conflicts::check_reverse_breaks(pkg, self.db, self.cache);
                if let Some(c) = rev.first() {
                    return Some((reconstruct(&visited, name), c.message.clone()));
                }
            }

            let dep_strs = [pkg.pre_depends.as_deref(), pkg.depends.as_deref()];
            for dep_str in dep_strs.iter().flatten() {
                for group in parse_dep_field(dep_str) {
                    let mut any_provider_exists = false;
                    for alt in &group.alternatives {
                        let providers = pmap.providers_of(&alt.name);
                        if providers.is_empty() { continue; }
                        any_provider_exists = true;
                        for p in providers {
                            if self.db.is_installed(&p) { continue; }
                            if !visited.contains_key(&p) {
                                visited.insert(p.clone(), name.clone());
                                queue.push_back(p);
                            }
                        }
                    }
                    if !any_provider_exists {
                        let alts: Vec<&str> = group.alternatives.iter()
                            .map(|a| a.name.as_str()).collect();
                        return Some((
                            reconstruct(&visited, name.clone()),
                            format!("Unmet dependency: {} (no provider available)", alts.join(" | ")),
                        ));
                    }
                }
            }
        }
        None
    }

    /// Full dependency closure for a set of names.
    pub fn dependency_closure(&self, names: &[String]) -> Vec<String> {
        let pmap = build_provides(self.cache, Some(self.db));
        let mut visited = std::collections::HashSet::new();
        let mut queue   = std::collections::VecDeque::new();
        for n in names { queue.push_back(n.clone()); }
        while let Some(name) = queue.pop_front() {
            if !visited.insert(name.clone()) { continue; }
            let pkg = match self.cache.get(&name) { Some(p) => p, None => continue };
            let dep_strs = [pkg.pre_depends.as_deref(), pkg.depends.as_deref()];
            for dep_str in dep_strs.iter().flatten() {
                for group in parse_dep_field(dep_str) {
                    for alt in &group.alternatives {
                        let providers = pmap.providers_of(&alt.name);
                        for p in providers {
                            if !self.db.is_installed(&p) && !visited.contains(&p) {
                                queue.push_back(p);
                                break;
                            }
                        }
                    }
                }
            }
        }
        let mut result: Vec<String> = visited.into_iter()
            .filter(|n| !names.contains(n))
            .collect();
        result.sort();
        result
    }

    /// Why is a package installed? (reverse dep chain, like `zypper why`)
    pub fn why_installed(&self, name: &str) -> Vec<String> {
        let mut reasons = Vec::new();
        let installed = self.db.list_all().unwrap_or_default();
        for inst in &installed {
            if inst.name == name { continue; }
            let pkg = match self.cache.get(&inst.name) { Some(p) => p, None => continue };
            let dep_strs = [
                pkg.pre_depends.as_deref(),
                pkg.depends.as_deref(),
                pkg.recommends.as_deref(),
            ];
            for dep_str in dep_strs.iter().flatten() {
                for group in parse_dep_field(dep_str) {
                    if group.alternatives.iter().any(|a| a.name == name) {
                        reasons.push(format!("Required by: {}", inst.name));
                        break;
                    }
                }
            }
        }
        if reasons.is_empty() {
            if let Some(inst) = self.db.get(name) {
                let reason = match inst.reason {
                    crate::db::InstallReason::User       => "Explicitly installed by user",
                    crate::db::InstallReason::Dependency => "Installed as dependency",
                };
                reasons.push(reason.to_string());
            }
        }
        reasons
    }

    /// Check if upgrading a set of packages would break anything.
    pub fn check_upgrade_safety(&self, names: &[String]) -> Vec<String> {
        let mut issues = Vec::new();
        for name in names {
            if let Some(avail) = self.cache.get(name) {
                let rev_breaks = conflicts::check_reverse_breaks(avail, self.db, self.cache);
                for c in rev_breaks {
                    if c.hard {
                        issues.push(format!(
                            "Upgrading '{}' to {} would break '{}'",
                            name, avail.version, c.pkg_name
                        ));
                    }
                }
            }
        }
        issues
    }
}
