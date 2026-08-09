use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet, VecDeque};

use crate::gpg_verify::{self, InRelease};
use crate::package::{parse_dep_field, version_satisfies, Package};

use super::types::Config;

/// Pobiera i parsuje indeksy `Packages` dla wszystkich `cfg.apt_sources`,
/// zapisując surowy tekst do `cfg.apt_lists_path` (cache, jak `apt-get
/// update`). Zwraca połączoną pulę pakietów (najnowsza wersja per nazwa+arch
/// wygrywa gdy więcej niż jedno źródło ją dostarcza).
///
/// ## Weryfikacja GPG/InRelease
///
/// Dla każdego `(uri, suite)` pobierany jest najpierw `InRelease` (fallback:
/// `Release`) i weryfikowany podpisem (`crate::gpg_verify::verify_inrelease`,
/// ten sam mechanizm co przy `hammer sync` w trybie atomic/normal — patrz
/// `pkg/cache.rs`). Każdy plik `Packages` jest następnie sprawdzany wobec
/// checksumy zadeklarowanej w `InRelease` (`InRelease::verify_file`) —
/// **zanim** jego zawartość trafi do puli pakietów. Domyślnie (tak jak przy
/// `hammer sync`) brak klucza/podpisu tylko **ostrzega** (żeby nie
/// blokować pracy na repo bez skonfigurowanego keyringu — częsty przypadek
/// przy pierwszym uruchomieniu). Ustawienie `[apt] -> require_gpg = true`
/// w `oci.hk` zamienia to ostrzeżenie w twardy błąd, przerywając refresh
/// dla tego źródła — zalecane dla wdrożeń produkcyjnych.
pub async fn refresh_index(cfg: &Config) -> Result<Pool> {
    std::fs::create_dir_all(&cfg.apt_lists_path)
        .with_context(|| format!("mkdir -p {}", cfg.apt_lists_path.display()))?;

    let client = crate::download::HttpClient::new();
    let mut pool = Pool::default();

    for source_line in &cfg.apt_sources {
        // Format: "deb [options] <uri> <suite> <component...>"
        let parts: Vec<&str> = source_line.split_whitespace().collect();
        let mut it = parts.into_iter();
        let Some(kind) = it.next() else { continue };
        if kind != "deb" { continue; } // deb-src not needed for layer installs
        let mut uri = None;
        let mut rest = Vec::new();
        for tok in it {
            if tok.starts_with('[') { continue; } // skip [signed-by=...] options
            if uri.is_none() { uri = Some(tok.to_string()); } else { rest.push(tok.to_string()); }
        }
        let Some(uri) = uri else { continue };
        let Some(suite) = rest.first().cloned() else { continue };
        let components: Vec<String> = if rest.len() > 1 { rest[1..].to_vec() } else { vec!["main".to_string()] };

        let inrelease = match fetch_and_verify_release(&client, &uri, &suite, cfg).await {
            Ok(ir) => Some(ir),
            Err(e) if cfg.require_gpg => {
                return Err(e).with_context(|| format!(
                    "GPG verification required (require_gpg=true) but failed for {uri} {suite}"
                ));
            }
            Err(e) => {
                crate::log::warn(&format!(
                    "oci: could not verify {uri} {suite} InRelease ({e:#}) — \
                     proceeding WITHOUT signature verification for this source"
                ));
                None
            }
        };

        for component in &components {
            let rel_path = format!("{component}/binary-{}/Packages", cfg.arch);
            let index_url = format!("{}/dists/{}/{}", uri.trim_end_matches('/'), suite, rel_path);
            let bytes = match client.get_bytes(&index_url).await {
                Ok(b) => b,
                Err(_) => {
                    // Try compressed variant fallback path names are
                    // decoded server-side by most mirrors; if the plain
                    // Packages file 404s we simply skip this component
                    // rather than hard-failing the whole refresh.
                    crate::log::warn(&format!("oci: could not fetch {index_url}"));
                    continue;
                }
            };

            if let Some(ir) = &inrelease {
                if let Err(e) = ir.verify_file(&rel_path, &bytes) {
                    let msg = format!("oci: Packages index {rel_path} failed InRelease checksum check: {e:#}");
                    if cfg.require_gpg {
                        anyhow::bail!(msg);
                    }
                    crate::log::warn(&msg);
                    continue; // don't trust an index that doesn't match its declared checksum
                }
            }

            let text = String::from_utf8_lossy(&bytes).to_string();
            let cache_name = format!(
                "{}_{}_{}_{}_Packages",
                sanitize(&uri), suite, component, cfg.arch
            );
            let _ = std::fs::write(cfg.apt_lists_path.join(&cache_name), &text);

            for pkg in Package::parse_index(&text) {
                pool.add(pkg, &uri);
            }
        }
    }

    Ok(pool)
}

/// Pobiera `InRelease` (fallback: `Release`) dla `(uri, suite)` i weryfikuje
/// jego podpis wobec `cfg.keyring_dir`.
async fn fetch_and_verify_release(
    client: &crate::download::HttpClient,
    uri:    &str,
    suite:  &str,
    cfg:    &Config,
) -> Result<InRelease> {
    let inrelease_url = format!("{}/dists/{}/InRelease", uri.trim_end_matches('/'), suite);
    let content = match client.get_string(&inrelease_url).await {
        Ok(c) => c,
        Err(_) => {
            // Older/simpler mirrors only publish detached Release+Release.gpg;
            // gpg_verify::InRelease::parse works on either format (it just
            // strips PGP armor if present), but we can't verify a detached
            // signature with the same code path — treat as best-effort:
            // fetch the plain Release for checksums, skip signature check.
            let release_url = format!("{}/dists/{}/Release", uri.trim_end_matches('/'), suite);
            client.get_string(&release_url).await
                .with_context(|| format!("Fetching {inrelease_url} or {release_url}"))?
        }
    };

    let ir = InRelease::parse(&content)
        .with_context(|| format!("Parsing InRelease/Release for {uri} {suite}"))?;
    gpg_verify::verify_inrelease(&content, &cfg.keyring_dir)
        .with_context(|| format!("Verifying signature for {uri} {suite}"))?;
    Ok(ir)
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect()
}

/// Pula pakietów zbudowana z jednego lub więcej indeksów `Packages`.
#[derive(Default)]
pub struct Pool {
    /// name -> najlepsza znana wersja Package (+ base URI źródła, dla URL do .deb)
    by_name: HashMap<String, (Package, String)>,
    /// virtual/provides name -> lista realnych nazw pakietów, które to zapewniają
    provides: HashMap<String, Vec<String>>,
}

impl Pool {
    fn add(&mut self, pkg: Package, base_uri: &str) {
        for prov in &pkg.provides_list {
            self.provides.entry(prov.clone()).or_default().push(pkg.name.clone());
        }
        if let Some(provides_field) = &pkg.provides {
            for group in parse_dep_field(provides_field) {
                for alt in group.alternatives {
                    self.provides.entry(alt.name).or_default().push(pkg.name.clone());
                }
            }
        }
        match self.by_name.get(&pkg.name) {
            Some((existing, _)) if crate::package::version_cmp(&existing.version, &pkg.version) != std::cmp::Ordering::Less => {}
            _ => { self.by_name.insert(pkg.name.clone(), (pkg, base_uri.to_string())); }
        }
    }

    pub fn find(&self, name: &str) -> Option<&Package> {
        self.by_name.get(name).map(|(p, _)| p)
    }

    /// Iteruje po wszystkich pakietach w puli (dla `hammer oci search`).
    pub fn all(&self) -> impl Iterator<Item = &Package> {
        self.by_name.values().map(|(p, _)| p)
    }

    pub fn base_uri_for(&self, name: &str) -> Option<&str> {
        self.by_name.get(name).map(|(_, u)| u.as_str())
    }

    /// URL do pobrania `.deb` pliku danego pakietu (`Filename:` z indeksu
    /// jest relatywne do URI repozytorium).
    pub fn deb_url(&self, name: &str) -> Option<String> {
        let (pkg, base) = self.by_name.get(name)?;
        let filename = pkg.filename.as_ref()?;
        Some(format!("{}/{}", base.trim_end_matches('/'), filename.trim_start_matches('/')))
    }

    fn resolve_alt<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        if self.by_name.contains_key(name) { return Some(name); }
        self.provides.get(name).and_then(|v| v.first()).map(|s| s.as_str())
    }
}

/// Resolves `names` (+ transitive deps, respecting `Conflicts:`/alternatives
/// properly) using hammer's real CDCL SAT solver — the same
/// `Solver`/`resolve::resolve_install` the atomic and normal-mode backends
/// use — instead of the simplified BFS closure in [`resolve_closure`]
/// below.
///
/// ## Why this exists alongside `resolve_closure`
///
/// The real solver (`pkg::solver`) is built around `PackageCache`/
/// `InstalledDb`, which read from a single, global, on-disk location
/// (`build_mode::base_dir()`/`db_path()`) — appropriate for atomic/
/// normal-mode, where there is exactly one system. `hammer oci` can be
/// resolving packages for several independent rootfs (different
/// deployments, overlay sessions) in the same process, so it can't just
/// point the real solver at those global paths.
///
/// Rather than modifying `cache.rs`/`db.rs` themselves to be
/// rootfs-parameterised everywhere (a much larger change touching code
/// shared with every build mode), this builds **temporary, in-memory**
/// `PackageCache`/`InstalledDb` instances scoped to a single call —
/// `PackageCache::empty()` populated from this OCI `Pool`'s packages, and
/// `InstalledDb::open_in_memory()` seeded with whatever `status_db` says
/// is already installed in `rootfs`. Both of those constructors already
/// existed for exactly this kind of use (originally added for the solver
/// test suite) — no changes to shared cache/db code were needed at all.
///
/// `resolve_closure` remains as a fallback for anywhere that doesn't have
/// a `rootfs` to inspect yet (e.g. before the first checkout in a fresh
/// `deploy`), and as a safety net if bridging into the real solver fails
/// for some reason (logged, not silently swallowed).
pub fn resolve_with_real_solver(
    pool:   &Pool,
    rootfs: &std::path::Path,
    names:  &[String],
) -> Result<crate::solver::TransactionPlan> {
    let mut cache = crate::cache::PackageCache::empty();
    for pkg in pool.all() {
        cache.insert(pkg.clone());
    }

    let db = crate::db::InstalledDb::open_in_memory()
        .context("Building temporary in-memory InstalledDb for CDCL resolution")?;
    for installed in super::status_db::load_all(rootfs).unwrap_or_default() {
        // Prefer the full Package metadata from the pool (real Depends:/
        // Conflicts:/Provides: fields the solver needs); fall back to a
        // minimal, name+version-only Package for base-image packages that
        // aren't in the currently-configured apt sources at all (still
        // lets the solver treat them as "present" for satisfiability
        // purposes, even without full dependency data for them).
        let pkg = cache.get(&installed.name).cloned().unwrap_or_else(|| {
            let mut p = Package::default();
            p.name = installed.name.clone();
            p.version = installed.version.clone();
            p.architecture = installed.architecture.clone();
            p
        });
        db.record_install(&pkg, crate::db::InstallReason::User, "", 0)
            .with_context(|| format!("Seeding in-memory InstalledDb with '{}'", installed.name))?;
    }

    let solver = crate::solver::Solver::new(&cache, &db);
    crate::solver::resolve::resolve_install(&solver, names, false)
        .context("CDCL dependency resolution failed")
}

/// Domknięcie zależności metodą BFS: startując od `names`, dla każdego
/// pakietu bierzemy pierwszą alternatywę z `Depends:`/`Pre-Depends:` która
/// jest dostępna w puli i spełnia ograniczenie wersji (jeśli podane),
/// dodajemy do wyniku i kontynuujemy, aż domknięcie się ustabilizuje.
pub fn resolve_closure(pool: &Pool, names: &[String]) -> Result<Vec<Package>> {
    let mut resolved: HashMap<String, Package> = HashMap::new();
    let mut queue: VecDeque<String> = names.iter().cloned().collect();
    let mut seen: HashSet<String> = HashSet::new();

    while let Some(name) = queue.pop_front() {
        if !seen.insert(name.clone()) { continue; }

        let Some(real_name) = pool.resolve_alt(&name) else {
            anyhow::bail!("Package '{}' not found in any configured index", name);
        };
        let Some(pkg) = pool.find(real_name) else { continue };
        resolved.insert(pkg.name.clone(), pkg.clone());

        for field in [&pkg.depends, &pkg.pre_depends] {
            let Some(field) = field else { continue };
            for group in parse_dep_field(field) {
                // Pick first satisfiable alternative; if none currently
                // installed/resolved satisfies the constraint, take the
                // first alternative present in the pool at all (best effort,
                // matches typical "just install the newest" apt behaviour
                // for a fresh layer install).
                let mut chosen = None;
                for alt in &group.alternatives {
                    if let Some(real) = pool.resolve_alt(&alt.name) {
                        if let Some(candidate) = pool.find(real) {
                            let ok = match &alt.constraint {
                                Some(c) => version_satisfies(&candidate.version, c.op.as_str(), &c.version),
                                None => true,
                            };
                            if ok { chosen = Some(alt.name.clone()); break; }
                        }
                    }
                }
                if let Some(c) = chosen.or_else(|| group.alternatives.first().map(|a| a.name.clone())) {
                    if !seen.contains(&c) {
                        queue.push_back(c);
                    }
                }
            }
        }
    }

    Ok(resolved.into_values().collect())
}
