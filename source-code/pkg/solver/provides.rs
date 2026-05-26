use std::collections::HashMap;
use crate::cache::PackageCache;
use crate::package::parse_dep_field;

#[derive(Debug, Default)]
pub struct ProvidesMap {
    inner: HashMap<String, Vec<(String, Option<String>)>>,
}

impl ProvidesMap {
    pub fn resolve<'a>(&'a self, name: &'a str) -> &'a str {
        if let Some(providers) = self.inner.get(name) {
            if let Some((real, _)) = providers.first() {
                return real.as_str();
            }
        }
        name
    }

    pub fn providers(&self, name: &str) -> &[(String, Option<String>)] {
        self.inner.get(name).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn is_virtual(&self, name: &str) -> bool {
        self.inner.contains_key(name)
    }
}

pub fn build(cache: &PackageCache) -> ProvidesMap {
    let mut inner: HashMap<String, Vec<(String, Option<String>)>> = HashMap::new();

    for pkg in cache.all_packages() {
        // Self-provides
        inner.entry(pkg.name.clone())
        .or_default()
        .push((pkg.name.clone(), Some(pkg.version.clone())));

        if let Some(ref provides_str) = pkg.provides {
            for group in parse_dep_field(provides_str) {
                for alt in &group.alternatives {
                    // FIX: c.op is VersionOp — compare via as_str()
                    let prov_ver = alt.constraint.as_ref()
                    .filter(|c| c.op.as_str() == "=")
                    .map(|c| c.version.clone());

                    inner.entry(alt.name.clone())
                    .or_default()
                    .push((pkg.name.clone(), prov_ver));
                }
            }
        }
    }

    // Sort: exact name matches first
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
