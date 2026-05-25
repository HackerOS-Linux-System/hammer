use std::collections::HashMap;

use crate::cache::PackageCache;
use crate::package::parse_dep_field;

// ─────────────────────────────────────────────────────────────
//  ProvidesMap
// ─────────────────────────────────────────────────────────────

/// Maps a virtual (or real) package name to a list of real package names
/// that satisfy it, in priority order.
#[derive(Debug, Default)]
pub struct ProvidesMap {
    /// virtual_name → [(real_pkg_name, optional_provided_version)]
    inner: HashMap<String, Vec<(String, Option<String>)>>,
}

impl ProvidesMap {
    /// Resolve a package name to a real package name.
    /// Returns the first real provider, or the name itself if not virtual.
    pub fn resolve<'a>(&'a self, name: &'a str) -> &'a str {
        if let Some(providers) = self.inner.get(name) {
            if let Some((real, _)) = providers.first() {
                return real.as_str();
            }
        }
        name
    }

    /// Returns all real packages that provide `name`.
    pub fn providers(&self, name: &str) -> &[(String, Option<String>)] {
        self.inner.get(name).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Returns true if any real package provides `name`.
    pub fn is_virtual(&self, name: &str) -> bool {
        self.inner.contains_key(name)
    }

    /// Check whether `installed_name` (with `installed_ver`) satisfies
    /// a virtual dependency `virtual_name` with optional version constraint.
    pub fn satisfies_virtual(
        &self,
        virtual_name:    &str,
        constraint_ver:  Option<&str>,
        constraint_op:   Option<&str>,
        installed_name:  &str,
        installed_ver:   &str,
    ) -> bool {
        if let Some(providers) = self.inner.get(virtual_name) {
            for (real, prov_ver) in providers {
                if real != installed_name { continue; }
                // If the dependency has a version constraint, check it
                if let (Some(op), Some(cv)) = (constraint_op, constraint_ver) {
                    // Use the provided version if available, else installed version
                    let ver_to_check = prov_ver.as_deref().unwrap_or(installed_ver);
                    return crate::solver::version::satisfies(ver_to_check, op, cv);
                }
                return true;
            }
        }
        false
    }
}

// ─────────────────────────────────────────────────────────────
//  Build the provides map from the package cache
// ─────────────────────────────────────────────────────────────

pub fn build(cache: &PackageCache) -> ProvidesMap {
    let mut inner: HashMap<String, Vec<(String, Option<String>)>> = HashMap::new();

    for pkg in cache.all_packages() {
        // Every real package "provides" itself (allows uniform lookup)
        inner.entry(pkg.name.clone())
        .or_default()
        .push((pkg.name.clone(), Some(pkg.version.clone())));

        // Parse the Provides field
        if let Some(ref provides_str) = pkg.provides {
            for group in parse_dep_field(provides_str) {
                for alt in &group.alternatives {
                    let prov_ver = alt.constraint.as_ref()
                    .filter(|c| c.op == "=")
                    .map(|c| c.version.clone());

                    inner.entry(alt.name.clone())
                    .or_default()
                    .push((pkg.name.clone(), prov_ver));
                }
            }
        }
    }

    // Sort each list: prefer exact name matches first, then alphabetically
    for (key, providers) in inner.iter_mut() {
        providers.sort_by(|(a, _), (b, _)| {
            let a_exact = a == key;
            let b_exact = b == key;
            b_exact.cmp(&a_exact).then(a.cmp(b))
        });
        providers.dedup_by(|(a, _), (b, _)| a == b);
    }

    ProvidesMap { inner }
}
