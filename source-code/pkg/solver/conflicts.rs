use crate::db::InstalledDb;
use crate::package::{parse_dep_field, Package};
use crate::solver::version::satisfies;

// ─────────────────────────────────────────────────────────────
//  Types
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ConflictInfo {
    pub message:  String,
    pub kind:     ConflictKind,
    pub pkg_name: String,
    pub with:     String,
    pub hard:     bool,
    /// Optionally, the specific file that caused the conflict
    pub detail:   Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConflictKind { Conflicts, Breaks, Replaces }

impl ConflictKind {
    pub fn label(&self) -> &'static str {
        match self {
            ConflictKind::Conflicts => "Conflicts",
            ConflictKind::Breaks    => "Breaks",
            ConflictKind::Replaces  => "Replaces",
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  check_install — what does installing `candidate` conflict with?
// ─────────────────────────────────────────────────────────────

pub fn check_install(candidate: &Package, db: &InstalledDb) -> Vec<ConflictInfo> {
    let mut out = Vec::new();
    if let Some(ref c_str) = candidate.conflicts {
        for group in parse_dep_field(c_str) {
            for alt in &group.alternatives {
                if let Some(inst) = db.get(&alt.name) {
                    let violates = alt.constraint.as_ref()
                        .map(|c| satisfies(&inst.version, c.op.as_str(), &c.version))
                        .unwrap_or(true);
                    if violates {
                        out.push(ConflictInfo {
                            message: format!(
                                "Package '{}' conflicts with installed '{}' {}",
                                candidate.name, inst.name, inst.version
                            ),
                            kind:     ConflictKind::Conflicts,
                            pkg_name: candidate.name.clone(),
                            with:     inst.name.clone(),
                            hard:     true,
                            detail:   None,
                        });
                    }
                }
            }
        }
    }

    // 2. Breaks: (soft — warn, but do not hard-block)
    if let Some(ref b_str) = candidate.breaks {
        for group in parse_dep_field(b_str) {
            for alt in &group.alternatives {
                if let Some(inst) = db.get(&alt.name) {
                    let breaks_it = alt.constraint.as_ref()
                        .map(|c| satisfies(&inst.version, c.op.as_str(), &c.version))
                        .unwrap_or(true);
                    if breaks_it {
                        out.push(ConflictInfo {
                            message: format!(
                                "Package '{}' breaks installed '{}' {} — \
                                 consider upgrading or removing it",
                                candidate.name, inst.name, inst.version
                            ),
                            kind:     ConflictKind::Breaks,
                            pkg_name: candidate.name.clone(),
                            with:     inst.name.clone(),
                            hard:     false,
                            detail:   None,
                        });
                    }
                }
            }
        }
    }

    // 3. Replaces: (informational — candidate can replace the installed pkg)
    if let Some(ref r_str) = candidate.replaces {
        for group in parse_dep_field(r_str) {
            for alt in &group.alternatives {
                if db.get(&alt.name).is_some() {
                    out.push(ConflictInfo {
                        message: format!(
                            "Package '{}' replaces installed '{}' — \
                             the old package will be removed",
                            candidate.name, alt.name
                        ),
                        kind:     ConflictKind::Replaces,
                        pkg_name: candidate.name.clone(),
                        with:     alt.name.clone(),
                        hard:     false,
                        detail:   None,
                    });
                }
            }
        }
    }

    // 3. Multi-arch conflicts (Conflicts: pkg:arch + Multi-Arch: Same version check)
    check_multi_arch(candidate, db, &mut out);

    out
}

// ─────────────────────────────────────────────────────────────
//  reverse_breaks — what currently-installed packages would be
//  broken if `candidate` were installed?
// ─────────────────────────────────────────────────────────────

pub fn check_reverse_breaks(candidate: &Package, db: &InstalledDb) -> Vec<ConflictInfo> {
    let mut out = Vec::new();
    let all = match db.list_all() { Ok(v) => v, Err(_) => return out };

    // Load cache to get dependency fields of installed packages
    let cache = match crate::cache::PackageCache::load() {
        Ok(c) => c,
        Err(_) => return out,
    };

    for inst in &all {
        // Get the cached package to access its Conflicts: field
        let cached_pkg = match cache.get(&inst.name) { Some(p) => p, None => continue };
        let conf_str = match &cached_pkg.conflicts { Some(s) => s, None => continue };
        for group in parse_dep_field(conf_str) {
            for alt in &group.alternatives {
                if alt.name == candidate.name {
                    // Does the version constraint match?
                    let applies = alt.constraint.as_ref()
                        .map(|c| satisfies(&candidate.version, c.op.as_str(), &c.version))
                        .unwrap_or(true);
                    if applies {
                        out.push(ConflictInfo {
                            message: format!(
                                "Installed package '{}' {} conflicts with '{}' {}",
                                inst.name, inst.version,
                                candidate.name, candidate.version
                            ),
                            kind:     ConflictKind::Conflicts,
                            pkg_name: inst.name.clone(),
                            with:     candidate.name.clone(),
                            hard:     true,
                            detail:   Some(format!(
                                "'{}' has Conflicts: {}",
                                inst.name, candidate.name
                            )),
                        });
                    }
                }
            }
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────
//  reverse_depends — what installed packages depend on `removing`?
//  (version-aware, ignores OR-alternates satisfied by other pkgs)
// ─────────────────────────────────────────────────────────────

pub fn reverse_depends(removing: &[String], db: &InstalledDb) -> Vec<String> {
    let remove_set: std::collections::HashSet<&str> =
        removing.iter().map(|s| s.as_str()).collect();

    let mut rdeps = Vec::new();
    let Ok(all) = db.list_all() else { return rdeps };

    for inst in &all {
        if remove_set.contains(inst.name.as_str()) { continue; }

        let dep_strs: Vec<&str> = inst.depends.iter().map(|s| s.as_str()).collect();

        let depends_on_removed = dep_strs.iter()
            .flat_map(|s| parse_dep_field(s))
            .any(|group| {
                // A dependency group is UNMET only if ALL alternatives are being removed
                // and no installed alternative remains outside the remove set
                let all_alts_removed = group.alternatives.iter()
                    .all(|alt| remove_set.contains(alt.name.as_str()));

                if !all_alts_removed { return false; }

                // Check version constraints: is there still a version that satisfies?
                group.alternatives.iter().any(|alt| {
                    if !remove_set.contains(alt.name.as_str()) { return false; }
                    // The dep is being removed — check if constraint was satisfied
                    alt.constraint.as_ref()
                        .and_then(|c| db.get(&alt.name)
                            .map(|i| satisfies(&i.version, c.op.as_str(), &c.version)))
                        .unwrap_or(true)
                })
            });

        if depends_on_removed {
            rdeps.push(inst.name.clone());
        }
    }
    rdeps
}

// ─────────────────────────────────────────────────────────────
//  resolve_replaces — given a list of candidates to install,
//  return the list of currently-installed packages that should
//  be auto-removed because a candidate Replaces them.
// ─────────────────────────────────────────────────────────────

pub fn resolve_replaces(candidates: &[Package], db: &InstalledDb) -> Vec<String> {
    let mut to_remove = Vec::new();
    for cand in candidates {
        let Some(ref r_str) = cand.replaces else { continue };
        for group in parse_dep_field(r_str) {
            for alt in &group.alternatives {
                if let Some(inst) = db.get(&alt.name) {
                    let applies = alt.constraint.as_ref()
                        .map(|c| satisfies(&inst.version, c.op.as_str(), &c.version))
                        .unwrap_or(true);
                    if applies && !to_remove.contains(&alt.name) {
                        to_remove.push(alt.name.clone());
                    }
                }
            }
        }
    }
    to_remove
}

// ─────────────────────────────────────────────────────────────
//  format_conflict_explanation — human-readable error block
// ─────────────────────────────────────────────────────────────

pub fn format_conflict_explanation(conflicts: &[ConflictInfo]) -> String {
    use owo_colors::OwoColorize;
    let mut lines = Vec::new();
    for c in conflicts {
        let prefix = if c.hard {
            format!("  {} {}", "✗".red().bold(), c.kind.label().red())
        } else {
            format!("  {} {}", "⚠".yellow(), c.kind.label().yellow())
        };
        lines.push(format!("{}: {}", prefix, c.message));
        if let Some(ref detail) = c.detail {
            lines.push(format!("     {}", detail.dimmed()));
        }
        if c.kind == ConflictKind::Replaces {
            lines.push(format!(
                "     {} '{}' will be auto-removed.",
                "→".dimmed(), c.with.cyan()
            ));
        }
    }
    lines.join("\n")
}

// ─────────────────────────────────────────────────────────────
//  Multi-arch conflict detection hooked into check_install (0.5)
//
//  Integrates crate::multi_arch::check_multi_arch_conflicts()
//  as an additional pass inside check_install so the SAT solver
//  sees multi-arch violations the same way as Conflicts:/Breaks:.
// ─────────────────────────────────────────────────────────────

/// Adapter: implement InstalledView for InstalledDb
struct DbInstalledView<'a>(&'a InstalledDb);

impl<'a> crate::multi_arch::InstalledView for DbInstalledView<'a> {
    fn installed_arches(&self, name: &str) -> Vec<(String, String)> {
        // Our DB stores one arch per row; for foreign-arch entries the name is
        // stored as "pkg:arch" (dpkg multi-arch convention).
        let native = self.0.get(name);
        let mut out = Vec::new();
        if let Some(ref inst) = native {
            out.push((inst.architecture.clone(), inst.version.clone()));
        }
        // Also check "name:i386", "name:arm64" etc.
        for foreign_arch in ["i386", "arm64", "armhf", "armel", "ppc64el", "s390x"] {
            let key = format!("{}:{}", name, foreign_arch);
            if let Some(inst) = self.0.get(&key) {
                out.push((inst.architecture.clone(), inst.version.clone()));
            }
        }
        out
    }
}

/// Run multi-arch conflict check and append results to `out`.
pub fn check_multi_arch(candidate: &Package, db: &InstalledDb, out: &mut Vec<ConflictInfo>) {
    let ma_db = crate::multi_arch::MultiArchDb::load();

    // Determine Multi-Arch mode: "all" architecture packages act as Foreign.
    let ma_mode = if candidate.architecture == "all" {
        crate::multi_arch::MultiArchMode::Foreign
    } else {
        ma_db.get_mode(&candidate.name)
            .unwrap_or(crate::multi_arch::MultiArchMode::No)
    };

    // Build conflict list from Conflicts: field (name, optional arch qualifier)
    let conflicts: Vec<(String, Option<String>)> = candidate.conflicts
        .as_deref()
        .map(|s| {
            crate::package::parse_dep_field(s)
                .into_iter()
                .flat_map(|g| g.alternatives)
                .map(|a| {
                    if let Some((n, arch)) = a.name.split_once(':') {
                        (n.to_string(), Some(arch.to_string()))
                    } else {
                        (a.name, None)
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let view = DbInstalledView(db);
    let ma_conflicts = crate::multi_arch::check_multi_arch_conflicts(
        &candidate.name,
        &candidate.architecture,
        &candidate.version,
        &ma_mode,
        &conflicts,
        &view,
    );

    for c in ma_conflicts {
        // Only report if the conflicting arch is actually enabled
        let arch_ok = c.conflicting_arch.as_deref()
            .map(|a| ma_db.supports_arch(a))
            .unwrap_or(true);
        if !arch_ok { continue; }

        out.push(ConflictInfo {
            message:  crate::multi_arch::format_conflicts(std::slice::from_ref(&c)),
            kind:     ConflictKind::Conflicts,
            pkg_name: c.requirer.clone(),
            with:     c.conflicting.clone(),
            hard:     true,
            detail:   c.conflicting_arch.map(|a| format!("Architecture: {}", a)),
        });
    }
}
