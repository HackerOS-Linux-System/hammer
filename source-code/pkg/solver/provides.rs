use std::collections::HashMap;
use crate::cache::PackageCache;
use crate::db::InstalledDb;
use crate::package::{parse_dep_field, VersionOp};

// ──────────────────────────────────────────────────────────────────────────────
//  Provider entry
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Provider {
    /// Real package name that provides the capability
    pub name:    String,
    /// Version it provides the capability at (None = any version)
    pub version: Option<String>,
    /// Repo priority (higher = preferred)
    pub priority: i32,
    /// Package architecture
    pub arch:    Option<String>,
    /// True if this package is currently installed
    pub installed: bool,
    /// True if this package is the exact name (not a virtual provide)
    pub exact:   bool,
}

// ──────────────────────────────────────────────────────────────────────────────
//  ProvidesMap
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct ProvidesMap {
    /// capability → list of providers (sorted: best first)
    provides:  HashMap<String, Vec<Provider>>,
    /// Replaces map: pkg → list of packages it can replace
    replaces:  HashMap<String, Vec<String>>,
    /// Packages that are purely virtual (no package with that exact name)
    virtuals:  std::collections::HashSet<String>,
}

impl ProvidesMap {
    // ── Resolution ────────────────────────────────────────────

    /// Best single provider for a capability (installed > pinned > priority > alpha).
    pub fn resolve<'a>(&'a self, name: &'a str) -> &'a str {
        if let Some(providers) = self.provides.get(name) {
            if let Some(p) = providers.first() {
                return p.name.as_str();
            }
        }
        name
    }

    /// All providers for a capability, best-first.
    pub fn providers_of(&self, name: &str) -> Vec<String> {
        // Also check multi-arch stripped name (foo:amd64 → foo)
        let bare = strip_arch_suffix(name);
        let key  = if bare != name { bare } else { name };

        self.provides.get(key)
            .map(|v| v.iter().map(|p| p.name.clone()).collect())
            .unwrap_or_else(|| {
                // Fallback: try stripping arch suffix
                if key != name {
                    self.provides.get(name)
                        .map(|v| v.iter().map(|p| p.name.clone()).collect())
                        .unwrap_or_default()
                } else {
                    vec![]
                }
            })
    }

    /// True if the capability has at least one provider installed right now.
    pub fn is_satisfied(&self, name: &str) -> bool {
        self.provides.get(strip_arch_suffix(name))
            .map(|v| v.iter().any(|p| p.installed))
            .unwrap_or(false)
    }

    /// True if `name` is a purely virtual package (no real package with that name).
    pub fn is_virtual(&self, name: &str) -> bool {
        self.virtuals.contains(strip_arch_suffix(name))
    }

    /// Packages that replace `name` (via `Replaces:` field).
    pub fn replacers_of(&self, name: &str) -> Vec<String> {
        self.replaces.get(name)
            .cloned()
            .unwrap_or_default()
    }

    /// Best provider satisfying a version constraint, or None.
    pub fn resolve_versioned(
        &self,
        name: &str,
        op:   &str,
        ver:  &str,
    ) -> Option<String> {
        let key = strip_arch_suffix(name);
        self.provides.get(key)?.iter().find(|p| {
            p.version.as_ref().map(|pv| {
                super::version::satisfies(pv, op, ver)
            }).unwrap_or(true)  // unversioned provide satisfies any constraint
        }).map(|p| p.name.clone())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
//  Builder
// ──────────────────────────────────────────────────────────────────────────────

/// Build a ProvidesMap from the package cache and installed DB.
/// Pass `installed_db = None` if the DB is not available (e.g. during sync).
pub fn build(cache: &PackageCache, installed_db: Option<&InstalledDb>) -> ProvidesMap {
    let mut provides: HashMap<String, Vec<Provider>> = HashMap::new();
    let mut replaces: HashMap<String, Vec<String>>   = HashMap::new();
    let mut virtuals = std::collections::HashSet::new();

    let installed_names: std::collections::HashSet<String> = installed_db
        .and_then(|db| db.list_all().ok())
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.name)
        .collect();

    for pkg in cache.all_packages() {
        let is_installed = installed_names.contains(&pkg.name);
        let base_priority = repo_priority_for(pkg.repo_base_uri.as_deref());

        // 1. Self-provides (pkg provides itself at its own version)
        provides.entry(pkg.name.clone()).or_default().push(Provider {
            name:      pkg.name.clone(),
            version:   Some(pkg.version.clone()),
            priority:  base_priority,
            arch:      Some(pkg.architecture.clone()),
            installed: is_installed,
            exact:     true,
        });

        // 2. Provides: field — may be versioned `foo (= 1.2)` or plain `foo`
        if let Some(ref prov_str) = pkg.provides {
            for group in parse_dep_field(prov_str) {
                for alt in &group.alternatives {
                    let cap_name = alt.name.clone();
                    let cap_ver  = alt.constraint.as_ref()
                        .filter(|c| c.op == VersionOp::Eq)
                        .map(|c| c.version.clone());

                    provides.entry(cap_name.clone()).or_default().push(Provider {
                        name:      pkg.name.clone(),
                        version:   cap_ver,
                        priority:  base_priority,
                        arch:      Some(pkg.architecture.clone()),
                        installed: is_installed,
                        exact:     false,
                    });

                    // If no real package has this name, mark it virtual
                    if !cache.get(&cap_name).map(|p| p.name == cap_name).unwrap_or(false) {
                        virtuals.insert(cap_name);
                    }
                }
            }
        }

        // 3. Replaces: field
        if let Some(ref repl_str) = pkg.replaces {
            for group in parse_dep_field(repl_str) {
                for alt in &group.alternatives {
                    replaces.entry(alt.name.clone())
                        .or_default()
                        .push(pkg.name.clone());
                }
            }
        }
    }

    // Sort providers: installed first, then by priority (desc), then alpha
    for providers in provides.values_mut() {
        providers.sort_by(|a, b| {
            b.installed.cmp(&a.installed)           // installed > not installed
                .then(b.exact.cmp(&a.exact))        // exact name > virtual provide
                .then(b.priority.cmp(&a.priority))  // higher priority repo first
                .then(a.name.cmp(&b.name))          // alphabetical tiebreak
        });
        providers.dedup_by(|a, b| a.name == b.name); // unique by package name
    }

    ProvidesMap { provides, replaces, virtuals }
}

/// Assign a numeric priority to a repository URI.
/// Mirrors apt's Pin-Priority concept for provider ordering.
fn repo_priority_for(uri: Option<&str>) -> i32 {
    match uri {
        None => 100,
        Some(u) if u.contains("hackeros") || u.contains("local") => 600,
        Some(u) if u.contains("security") => 500,
        Some(u) if u.contains("proposed") || u.contains("backports") => 200,
        Some(_) => 400,
    }
}

/// Strip `:arch` suffix from a package name.
fn strip_arch_suffix(name: &str) -> &str {
    if let Some(idx) = name.rfind(':') {
        // Only strip if what follows looks like an arch (alpha only, short)
        let suffix = &name[idx+1..];
        if suffix.len() <= 8 && suffix.chars().all(|c| c.is_alphabetic() || c == '_') {
            return &name[..idx];
        }
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_strip_arch() {
        assert_eq!(strip_arch_suffix("libfoo:amd64"), "libfoo");
        assert_eq!(strip_arch_suffix("libfoo"), "libfoo");
        assert_eq!(strip_arch_suffix("libfoo:arm64"), "libfoo");
        assert_eq!(strip_arch_suffix("python3:i386"), "python3");
    }
}
