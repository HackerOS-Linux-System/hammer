use crate::db::InstalledDb;
use crate::package::{parse_dep_field, Package};
use crate::solver::version::satisfies;

// ─────────────────────────────────────────────────────────────
//  ConflictInfo
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ConflictInfo {
    /// e.g. "Package 'foo' conflicts with installed 'bar' (1.2-3)"
    pub message:  String,
    pub kind:     ConflictKind,
    pub pkg_name: String,
    pub with:     String,
    /// true = hard conflict (Conflicts), false = soft (Breaks)
    pub hard:     bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConflictKind { Conflicts, Breaks, Replaces }

// ─────────────────────────────────────────────────────────────
//  Check a package being considered for installation
// ─────────────────────────────────────────────────────────────

/// Check `candidate` against everything in `db`.
/// Returns a list of conflicts found.
pub fn check_install(candidate: &Package, db: &InstalledDb) -> Vec<ConflictInfo> {
    let mut out = Vec::new();

    // 1. Check candidate's own Conflicts
    if let Some(ref c_str) = candidate.conflicts {
        for group in parse_dep_field(c_str) {
            for alt in &group.alternatives {
                if let Some(inst) = db.get(&alt.name) {
                    let violates = alt.constraint.as_ref()
                    .map(|c| satisfies(&inst.version, &c.op, &c.version))
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
                        });
                    }
                }
            }
        }
    }

    // 2. Check candidate's Breaks
    if let Some(ref b_str) = candidate.breaks {
        for group in parse_dep_field(b_str) {
            for alt in &group.alternatives {
                if let Some(inst) = db.get(&alt.name) {
                    let breaks_it = alt.constraint.as_ref()
                    .map(|c| satisfies(&inst.version, &c.op, &c.version))
                    .unwrap_or(true);
                    if breaks_it {
                        out.push(ConflictInfo {
                            message: format!(
                                "Package '{}' breaks installed '{}' {}",
                                candidate.name, inst.name, inst.version
                            ),
                            kind:     ConflictKind::Breaks,
                            pkg_name: candidate.name.clone(),
                                 with:     inst.name.clone(),
                                 hard:     false,
                        });
                    }
                }
            }
        }
    }

    // 3. Check whether any installed package conflicts with the candidate
    if let Ok(all) = db.list_all() {
        for inst in &all {
            if let Some(ref c_str) = inst.conflicts {
                for group in parse_dep_field(c_str) {
                    for alt in &group.alternatives {
                        if alt.name != candidate.name { continue; }
                        let violates = alt.constraint.as_ref()
                        .map(|c| satisfies(&candidate.version, &c.op, &c.version))
                        .unwrap_or(true);
                        if violates {
                            out.push(ConflictInfo {
                                message: format!(
                                    "Installed '{}' {} conflicts with candidate '{}' {}",
                                    inst.name, inst.version,
                                    candidate.name, candidate.version
                                ),
                                kind:     ConflictKind::Conflicts,
                                pkg_name: inst.name.clone(),
                                     with:     candidate.name.clone(),
                                     hard:     true,
                            });
                        }
                    }
                }
            }
        }
    }

    out
}

// ─────────────────────────────────────────────────────────────
//  Check for reverse-dependency breaks on removal
// ─────────────────────────────────────────────────────────────

/// Return names of packages that depend on any of `removing`.
pub fn reverse_depends(removing: &[String], db: &InstalledDb) -> Vec<String> {
    let remove_set: std::collections::HashSet<&str> =
    removing.iter().map(|s| s.as_str()).collect();
    let mut rdeps = Vec::new();
    let Ok(all) = db.list_all() else { return rdeps; };

    for inst in &all {
        if remove_set.contains(inst.name.as_str()) { continue; }
        let depends_on_removed = [&inst.depends, &inst.pre_depends]
        .iter()
        .filter_map(|f| f.as_ref())
        .flat_map(|s| parse_dep_field(s))
        .any(|group| {
            group.alternatives.iter()
            .any(|a| remove_set.contains(a.name.as_str()))
        });
        if depends_on_removed { rdeps.push(inst.name.clone()); }
    }
    rdeps
}
