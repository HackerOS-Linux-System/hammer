use crate::profile::GenerationsDb;

// ─────────────────────────────────────────────────────────────
//  Types
// ─────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct GenDiff {
    pub gen_a:     u32,
    pub gen_b:     u32,
    pub added:     Vec<DiffEntry>,
    pub removed:   Vec<DiffEntry>,
    pub upgraded:  Vec<DiffUpgrade>,
    pub unchanged: usize,
}

#[derive(Debug)]
pub struct DiffEntry {
    pub name:    String,
    pub version: String,
}

#[derive(Debug)]
pub struct DiffUpgrade {
    pub name:        String,
    pub version_old: String,
    pub version_new: String,
}

impl GenDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.upgraded.is_empty()
    }
    pub fn total_changes(&self) -> usize {
        self.added.len() + self.removed.len() + self.upgraded.len()
    }
}

// ─────────────────────────────────────────────────────────────
//  compute_diff
// ─────────────────────────────────────────────────────────────

pub fn compute_diff(a: u32, b: u32, gens_db: &GenerationsDb) -> anyhow::Result<GenDiff> {
    let gen_a = gens_db.get(a)
    .ok_or_else(|| anyhow::anyhow!("Generation {} not found", a))?;
    let gen_b = gens_db.get(b)
    .ok_or_else(|| anyhow::anyhow!("Generation {} not found", b))?;

    let map_a: std::collections::HashMap<&str, &str> = gen_a.packages.iter()
    .map(|p| (p.name.as_str(), p.version.as_str())).collect();
    let map_b: std::collections::HashMap<&str, &str> = gen_b.packages.iter()
    .map(|p| (p.name.as_str(), p.version.as_str())).collect();

    let mut added    = Vec::new();
    let mut removed  = Vec::new();
    let mut upgraded = Vec::new();
    let mut unchanged = 0usize;

    for (name, ver) in &map_b {
        match map_a.get(name) {
            None => added.push(DiffEntry { name: name.to_string(), version: ver.to_string() }),
            Some(old_ver) => {
                if *old_ver != *ver {
                    upgraded.push(DiffUpgrade {
                        name:        name.to_string(),
                                  version_old: old_ver.to_string(),
                                  version_new: ver.to_string(),
                    });
                } else {
                    unchanged += 1;
                }
            }
        }
    }
    for (name, ver) in &map_a {
        if !map_b.contains_key(name) {
            removed.push(DiffEntry { name: name.to_string(), version: ver.to_string() });
        }
    }

    added.sort_by(|a, b| a.name.cmp(&b.name));
    removed.sort_by(|a, b| a.name.cmp(&b.name));
    upgraded.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(GenDiff { gen_a: a, gen_b: b, added, removed, upgraded, unchanged })
}
