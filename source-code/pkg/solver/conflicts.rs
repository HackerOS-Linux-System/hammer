use crate::db::InstalledDb;
use crate::package::{parse_dep_field, Package};
use crate::solver::version::satisfies;

// ─────────────────────────────────────────────────────────────
//  ConflictInfo
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ConflictInfo {
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
//  check_install
// ─────────────────────────────────────────────────────────────

pub fn check_install(candidate: &Package, db: &InstalledDb) -> Vec<ConflictInfo> {
    let mut out = Vec::new();

    // 1. Candidate's own Conflicts field
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
                        });
                    }
                }
            }
        }
    }

    // 2. Candidate's Breaks field
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

    // 3. Check if any installed package has a Conflicts referencing candidate.
    //    InstalledPackage doesn't carry the full control fields (only what
    //    db.rs stores), so we check via the Package struct from the cache
    //    if available. If not available, skip silently.
    //    This is handled externally in sat.rs using the cache.

    out
}

// ─────────────────────────────────────────────────────────────
//  reverse_depends
// ─────────────────────────────────────────────────────────────

pub fn reverse_depends(removing: &[String], db: &InstalledDb) -> Vec<String> {
    let remove_set: std::collections::HashSet<&str> =
    removing.iter().map(|s| s.as_str()).collect();
    let mut rdeps = Vec::new();
    let Ok(all) = db.list_all() else { return rdeps; };

    for inst in &all {
        if remove_set.contains(inst.name.as_str()) { continue; }
        // Only check Depends (InstalledPackage doesn't have pre_depends column)
        let depends_on_removed = inst.depends.iter()
        .flat_map(|s| parse_dep_field(s))
        .any(|group| {
            group.alternatives.iter()
            .any(|a| remove_set.contains(a.name.as_str()))
        });
        if depends_on_removed { rdeps.push(inst.name.clone()); }
    }
    rdeps
}
