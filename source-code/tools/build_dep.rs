use crate::cli_types::has_flag;
use anyhow::{bail, Context, Result};
use owo_colors::OwoColorize;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::cache::PackageCache;
use crate::db::InstalledDb;
use crate::download::HttpClient;
use crate::repo::SourcesList;

pub const SOURCES_CACHE_DIR: &str = "/var/lib/hammer/sources";

// ─────────────────────────────────────────────────────────────
//  SourcePackage — one entry from Sources.xz
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct SourcePackage {
    pub name:          String,
    pub version:       String,
    pub binaries:      Vec<String>,   // "Binary:" field, comma-separated
    pub build_depends: Vec<String>,   // parsed package names only
    pub directory:     String,        // "Directory:" field (pool path)
    pub files:         Vec<(String, String)>, // (sha256, filename)
    pub repo_base_uri: String,
}

impl SourcePackage {
    /// Build the changelog URL for this source package.
    pub fn changelog_url(&self) -> String {
        if self.repo_base_uri.contains("debian.org") {
            format!(
                "https://metadata.ftp-master.debian.org/changelogs/{}/{}_changelog",
                self.directory.trim_end_matches('/'), self.name
            )
        } else if self.repo_base_uri.contains("ubuntu.com") {
            format!(
                "https://changelogs.ubuntu.com/changelogs/{}/{}-{}",
                self.directory.trim_end_matches('/'), self.name, self.version
            )
        } else {
            format!(
                "{}/{}/{}_changelog",
                self.repo_base_uri.trim_end_matches('/'),
                    self.directory.trim_end_matches('/'),
                    self.name
            )
        }
    }

    pub fn dsc_url(&self) -> Option<String> {
        let dsc_file = self.files.iter()
        .find(|(_, f)| f.ends_with(".dsc"))
        .map(|(_, f)| f.clone())?;
        Some(format!(
            "{}/{}/{}",
            self.repo_base_uri.trim_end_matches('/'),
                     self.directory.trim_end_matches('/'),
                     dsc_file
        ))
    }
}

// ─────────────────────────────────────────────────────────────
//  SourcesIndex
// ─────────────────────────────────────────────────────────────

pub struct SourcesIndex {
    /// source package name -> SourcePackage
    pub by_source: HashMap<String, SourcePackage>,
    /// binary package name -> source package name
    pub binary_to_source: HashMap<String, String>,
}

impl SourcesIndex {
    pub fn empty() -> Self {
        SourcesIndex { by_source: HashMap::new(), binary_to_source: HashMap::new() }
    }

    /// Load from on-disk cache if present, otherwise fetch fresh.
    pub async fn load_or_fetch(client: &HttpClient) -> Result<Self> {
        if let Ok(idx) = Self::load_cached() {
            if !idx.by_source.is_empty() {
                return Ok(idx);
            }
        }
        Self::fetch(client).await
    }

    pub fn load_cached() -> Result<Self> {
        let dir = Path::new(SOURCES_CACHE_DIR);
        if !dir.exists() { return Ok(Self::empty()); }

        let mut idx = Self::empty();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path  = entry.path();
            if path.extension().map_or(false, |e| e == "src") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let base_uri = content.lines()
                    .find(|l| l.starts_with("# hammer-base-uri:"))
                    .map(|l| l["# hammer-base-uri:".len()..].trim().to_string())
                    .unwrap_or_default();
                    idx.ingest_text(&content, &base_uri);
                }
            }
        }
        Ok(idx)
    }

    /// Fetch Sources.xz for every configured repo/component/suite and parse.
    ///
    /// `SourcesList` only exposes `index_urls(arch)`, which returns binary
    /// `Packages` index URLs (used by `pkg/cache.rs`). There is no separate
    /// `sources_index_urls()` method, so instead we take those binary index
    /// URLs and derive the corresponding source index URL by replacing the
    /// `binary-<arch>/Packages` path segment with `source/Sources`. This is
    /// the standard Debian repository layout:
    ///
    ///   dists/<suite>/<component>/binary-<arch>/Packages.xz
    ///   dists/<suite>/<component>/source/Sources.xz
    ///
    /// We call `index_urls(&native_arch)` once just to enumerate the
    /// configured (suite, component, base_uri) triples — the `<arch>`
    /// portion of each URL is then rewritten to `source` before fetching.
    pub async fn fetch(client: &HttpClient) -> Result<Self> {
        let sources = SourcesList::load()?;
        let mut idx = Self::empty();

        std::fs::create_dir_all(SOURCES_CACHE_DIR).ok();

        let native = crate::cache::detect_arch();
        let binary_urls = sources.index_urls(&native);
        if binary_urls.is_empty() {
            crate::log::warn("build_dep: no repositories configured for Sources index");
            return Ok(idx);
        }

        // Derive Sources.xz URLs from Packages.xz URLs, deduping so each
        // source index is fetched once even though it's shared across arches.
        let mut seen_sources: std::collections::HashSet<String> = std::collections::HashSet::new();

        for bin_url in &binary_urls {
            let Some(sources_url) = derive_sources_url(&bin_url.url) else {
                crate::log::warn(&format!(
                    "build_dep: could not derive Sources URL from {}", bin_url.url
                ));
                continue;
            };
            if !seen_sources.insert(sources_url.clone()) { continue; }

            let base_uri = bin_url.base_uri.clone();
            let text = match fetch_sources_text(client, &sources_url).await {
                Ok(t)  => t,
                Err(e) => {
                    crate::log::warn(&format!(
                        "build_dep: failed to fetch {}: {}", sources_url, e
                    ));
                    continue;
                }
            };

            let fname = url_to_cache_name(&sources_url);
            let dest  = PathBuf::from(SOURCES_CACHE_DIR).join(format!("{}.src", fname));
            let stored = format!("# hammer-base-uri: {}\n{}", base_uri, text);
            let _ = std::fs::write(&dest, &stored);

            idx.ingest_text(&text, &base_uri);
        }

        Ok(idx)
    }

    fn ingest_text(&mut self, text: &str, base_uri: &str) {
        for block in text.split("\n\n") {
            if block.trim().is_empty() { continue; }
            if let Some(src) = parse_source_block(block, base_uri) {
                for bin in &src.binaries {
                    self.binary_to_source.insert(bin.clone(), src.name.clone());
                }
                self.by_source.insert(src.name.clone(), src);
            }
        }
    }

    /// Find the source package that builds a given binary package
    /// (optionally matching a specific version of the binary).
    pub fn find_for_binary(&self, binary_name: &str, _binary_version: &str) -> Option<&SourcePackage> {
        // Direct binary name match first
        if let Some(src_name) = self.binary_to_source.get(binary_name) {
            return self.by_source.get(src_name);
        }
        // Fallback: source package with same name as binary
        self.by_source.get(binary_name)
    }

    pub fn get(&self, source_name: &str) -> Option<&SourcePackage> {
        self.by_source.get(source_name)
    }
}

// ─────────────────────────────────────────────────────────────
//  Sources.xz block parser
// ─────────────────────────────────────────────────────────────

fn parse_source_block(block: &str, base_uri: &str) -> Option<SourcePackage> {
    let mut src = SourcePackage::default();
    src.repo_base_uri = base_uri.to_string();

    let mut in_files     = false;
    let mut in_checksums = false;
    let mut in_builddeps = false;
    let mut builddeps_buf = String::new();

    for line in block.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            let trimmed = line.trim();
            if in_files {
                // Format: <md5> <size> <filename>
                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                if parts.len() == 3 {
                    let fname = parts[2].to_string();
                    if !src.files.iter().any(|(_, f)| f == &fname) {
                        src.files.push((String::new(), fname));
                    }
                }
            } else if in_checksums {
                // Format: <sha256> <size> <filename>
                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                if parts.len() == 3 {
                    let sha   = parts[0].to_string();
                    let fname = parts[2].to_string();
                    if let Some(entry) = src.files.iter_mut().find(|(_, f)| f == &fname) {
                        entry.0 = sha;
                    } else {
                        src.files.push((sha, fname));
                    }
                }
            } else if in_builddeps {
                builddeps_buf.push(' ');
                builddeps_buf.push_str(trimmed);
            }
            continue;
        }

        in_files     = false;
        in_checksums = false;
        in_builddeps = false;

        if let Some(v) = line.strip_prefix("Package:") {
            src.name = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("Version:") {
            src.version = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("Directory:") {
            src.directory = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("Binary:") {
            src.binaries = v.split(',').map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()).collect();
        } else if let Some(v) = line.strip_prefix("Build-Depends:") {
            builddeps_buf = v.to_string();
            in_builddeps  = true;
        } else if line.starts_with("Files:") {
            in_files = true;
        } else if line.starts_with("Checksums-Sha256:") {
            in_checksums = true;
        }
    }

    if !builddeps_buf.is_empty() {
        src.build_depends = parse_build_depends(&builddeps_buf);
    }

    if src.name.is_empty() { return None; }
    Some(src)
}

/// Parse a Build-Depends field into a flat list of package names.
/// Strips version constraints "(>= 1.0)" and architecture qualifiers "[amd64]",
/// and takes the first alternative of each "|" group.
fn parse_build_depends(field: &str) -> Vec<String> {
    field.split(',')
    .filter_map(|group| {
        // Take first alternative before any '|'
        let first_alt = group.split('|').next()?.trim();
        // Strip version constraint "(>= x)"
        let name_part = first_alt.split('(').next()?.trim();
        // Strip arch qualifier "[amd64 !i386]"
        let name_part = name_part.split('[').next()?.trim();
        if name_part.is_empty() { None } else { Some(name_part.to_string()) }
    })
    .collect()
}

// ─────────────────────────────────────────────────────────────
//  Fetch + decompress Sources index
// ─────────────────────────────────────────────────────────────

async fn fetch_sources_text(client: &HttpClient, base_url: &str) -> Result<String> {
    for suffix in &[".xz", ".gz", ".bz2", ""] {
        let url = format!("{}{}", base_url, suffix);
        if let Ok(bytes) = client.get_bytes(&url).await {
            return decompress(&bytes, suffix);
        }
    }
    bail!("No Sources index variant found at {}", base_url)
}

fn decompress(bytes: &[u8], suffix: &str) -> Result<String> {
    match suffix {
        ".xz"  => {
            let mut d = xz2::read::XzDecoder::new(bytes);
            let mut s = String::new(); d.read_to_string(&mut s)?; Ok(s)
        }
        ".gz"  => {
            let mut d = flate2::read::GzDecoder::new(bytes);
            let mut s = String::new(); d.read_to_string(&mut s)?; Ok(s)
        }
        ".bz2" => {
            let mut d = bzip2::read::BzDecoder::new(bytes);
            let mut s = String::new(); d.read_to_string(&mut s)?; Ok(s)
        }
        _      => Ok(String::from_utf8_lossy(bytes).to_string()),
    }
}

fn url_to_cache_name(url: &str) -> String {
    url.chars()
    .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
    .take(120)
    .collect()
}

/// Derive a Sources index URL from a binary Packages index URL by
/// replacing the trailing `binary-<arch>/Packages` path segment with
/// `source/Sources`.
///
/// Example:
///   .../dists/bookworm/main/binary-amd64/Packages
///   -> .../dists/bookworm/main/source/Sources
///
/// Returns None if the URL doesn't contain a recognisable `binary-*`
/// segment (e.g. flat repositories), since those don't have a separate
/// Sources index in the same layout.
fn derive_sources_url(packages_url: &str) -> Option<String> {
    // Strip a known compressed suffix if present (cache.rs's IndexUrl.url
    // is the *uncompressed* base path — compression suffixes are appended
    // separately when fetching — but be defensive just in case).
    let base = packages_url
    .trim_end_matches(".xz")
    .trim_end_matches(".gz")
    .trim_end_matches(".bz2");

    let idx = base.rfind("/binary-")?;
    let after_binary = &base[idx + "/binary-".len()..];
    // after_binary looks like "<arch>/Packages"
    let slash = after_binary.find('/')?;
    let tail  = &after_binary[slash + 1..]; // "Packages"
    if tail != "Packages" { return None; }

    Some(format!("{}/source/Sources", &base[..idx]))
}

// ─────────────────────────────────────────────────────────────
//  hammer build-dep <pkg>
// ─────────────────────────────────────────────────────────────

pub async fn cmd_build_dep(args: &[String]) -> Result<()> {
    let name = args.first()
    .ok_or_else(|| anyhow::anyhow!("Usage: hammer build-dep <package>"))?;

    let cache  = PackageCache::load()?;
    let db     = InstalledDb::open()?;
    let client = HttpClient::new();

    println!("  {} Loading Sources index…", "·".dimmed());
    let sources_idx = SourcesIndex::load_or_fetch(&client).await
    .unwrap_or_else(|_| SourcesIndex::empty());

    let pkg = cache.get(name)
    .ok_or_else(|| anyhow::anyhow!("Package '{}' not found. Run `hammer sync`.", name))?;

    let src = sources_idx.find_for_binary(name, &pkg.version);

    let build_deps: Vec<String> = match src {
        Some(s) if !s.build_depends.is_empty() => {
            println!("  {} Source package: {} {}", "·".dimmed(),
                     s.name.bold(), s.version.dimmed());
            s.build_depends.clone()
        }
        _ => {
            println!("  {} No Sources entry with Build-Depends found — using heuristics.",
                     "!".yellow());
            find_build_deps_heuristic(name, &cache)
        }
    };

    if build_deps.is_empty() {
        println!("  {} No build dependencies found for '{}'.", "·".dimmed(), name.bold());
        return Ok(());
    }

    println!();
    println!("  {}  Build dependencies for {}", "⬡".bright_cyan().bold(), name.bold());
    println!("  {}", "─".repeat(60).dimmed());

    let mut to_install = Vec::new();
    for dep in &build_deps {
        let installed = db.is_installed(dep);
        let available = cache.get(dep).is_some();
        let mark = if installed     { "✔".bright_green().to_string() }
        else if available { "○".cyan().to_string() }
        else              { "✗".red().to_string() };
        println!("  {} {}", mark, dep.bold());
        if !installed && available {
            to_install.push(dep.clone());
        }
    }

    if to_install.is_empty() {
        println!();
        println!("  {} All build dependencies already installed.", "✔".bright_green());
        return Ok(());
    }

    println!();
    println!("  Packages to install: {}", to_install.join(", ").cyan());

    if !crate::ui::confirm("Install build dependencies?")? {
        println!("  Aborted.");
        return Ok(());
    }

    let solver = crate::solver::Solver::new(&cache, &db);
    let plan   = solver.resolve_install(&to_install, false)?;
    let ctx    = crate::transaction::TransactionContext::system(&plan, &db, &to_install, false);
    crate::transaction::run_transaction(ctx, &format!("build-dep {}", name)).await?;
    Ok(())
}

/// Heuristic fallback when Sources index has no entry for this package.
fn find_build_deps_heuristic(name: &str, cache: &PackageCache) -> Vec<String> {
    let mut deps = Vec::new();
    for dep in &["build-essential", "dpkg-dev", "fakeroot"] {
        if cache.get(dep).is_some() { deps.push(dep.to_string()); }
    }
    let dev_name = format!("{}-dev", name);
    if cache.get(&dev_name).is_some() { deps.push(dev_name); }

    for pkg in cache.search(name).iter().take(20) {
        if pkg.name.ends_with("-dev") && !deps.contains(&pkg.name) {
            deps.push(pkg.name.clone());
        }
    }
    deps
}

// ─────────────────────────────────────────────────────────────
//  hammer source <pkg>
// ─────────────────────────────────────────────────────────────

pub async fn cmd_source(args: &[String]) -> Result<()> {
    let name = args.first()
    .ok_or_else(|| anyhow::anyhow!("Usage: hammer source <package>"))?;
    let download_only = args.iter().any(|a| a == "--download-only");
    let index_only    = args.iter().any(|a| a == "--index");
    let output_dir    = args.iter()
    .find(|a| a.starts_with("--dir="))
    .and_then(|a| a.strip_prefix("--dir="))
    .unwrap_or(".");

    let client = HttpClient::new();

    if index_only {
        println!("  {} Fetching Sources index for all configured repositories…", "·".dimmed());
        let idx = SourcesIndex::fetch(&client).await?;
        println!("  {} {} source packages indexed.", "✔".bright_green(), idx.by_source.len());
        return Ok(());
    }

    let cache = PackageCache::load()?;
    let pkg   = cache.get(name)
    .ok_or_else(|| anyhow::anyhow!("Package '{}' not found. Run `hammer sync`.", name))?;

    println!();
    println!("  {}  Fetching source for {} {}",
             "⬡".bright_cyan().bold(), name.bold(), pkg.version.dimmed());
    println!("  {}", "─".repeat(60).dimmed());

    let sources_idx = SourcesIndex::load_or_fetch(&client).await
    .unwrap_or_else(|_| SourcesIndex::empty());

    let (dsc_url, src_pkg) = match sources_idx.find_for_binary(name, &pkg.version) {
        Some(s) => {
            let url = s.dsc_url().unwrap_or_else(|| build_dsc_url_heuristic(
                name, &pkg.version, pkg.repo_base_uri.as_deref().unwrap_or("")
            ));
            (url, Some(s.clone()))
        }
        None => {
            let url = build_dsc_url_heuristic(
                name, &pkg.version, pkg.repo_base_uri.as_deref().unwrap_or("")
            );
            (url, None)
        }
    };

    println!("  {} Fetching DSC: {}", "·".dimmed(), dsc_url.dimmed());
    let dsc_content = match client.get_string(&dsc_url).await {
        Ok(s)  => s,
        Err(e) => {
            println!("  {} Could not fetch DSC: {}", "✗".red(), e);
            println!("  Try: {}", format!("https://packages.debian.org/source/{}", name).cyan());
            return Ok(());
        }
    };

    let out_dir = PathBuf::from(output_dir);
    std::fs::create_dir_all(&out_dir)?;

    let src_name = src_pkg.as_ref().map(|s| s.name.as_str()).unwrap_or(name);
    let dsc_path = out_dir.join(format!("{}_{}.dsc", src_name, pkg.version));
    std::fs::write(&dsc_path, &dsc_content)?;
    println!("  {} Saved {}", "✔".green(), dsc_path.display().to_string().bold());

    if download_only { return Ok(()); }

    // Download files listed in Sources index (preferred — has correct checksums)
    // or fall back to parsing the .dsc itself
    let files: Vec<(String, String)> = match &src_pkg {
        Some(s) if !s.files.is_empty() => s.files.clone(),
        _ => parse_dsc_files(&dsc_content),
    };

    let base = pkg.repo_base_uri.as_deref().unwrap_or("");
    let directory = src_pkg.as_ref().map(|s| s.directory.as_str()).unwrap_or("");

    for (sha256, filename) in &files {
        let file_url = if !directory.is_empty() {
            format!("{}/{}/{}", base.trim_end_matches('/'), directory.trim_end_matches('/'), filename)
        } else {
            let prefix = if name.starts_with("lib") {
                format!("lib{}", name.chars().nth(3).unwrap_or('a'))
            } else { name[..1].to_string() };
            format!("{}/pool/main/{}/{}/{}", base.trim_end_matches('/'), prefix, name, filename)
        };
        let dest = out_dir.join(filename);

        println!("  {} Downloading {}…", "·".dimmed(), filename);
        match client.get_bytes(&file_url).await {
            Ok(bytes) => {
                if !sha256.is_empty() {
                    use sha2::{Digest, Sha256};
                    let actual = hex::encode(Sha256::digest(&bytes));
                    if actual != *sha256 {
                        println!("  {} SHA256 mismatch for {} — skipping", "✗".red(), filename);
                        continue;
                    }
                }
                std::fs::write(&dest, &bytes)?;
                println!("  {} {} ({})", "✔".green(),
                         filename.bold(),
                         crate::ui::human_size(bytes.len() as u64).dimmed());
            }
            Err(e) => println!("  {} {}: {}", "✗".red(), filename, e.to_string().dimmed()),
        }
    }

    println!();
    println!("  Source files in: {}", out_dir.display().to_string().cyan());
    println!("  Build: {}", format!("cd {} && dpkg-buildpackage -us -uc", out_dir.display()).cyan());
    Ok(())
}

fn build_dsc_url_heuristic(name: &str, version: &str, base: &str) -> String {
    let prefix = if name.starts_with("lib") {
        format!("lib{}", name.chars().nth(3).unwrap_or('a'))
    } else {
        name[..1].to_string()
    };
    let ver_no_epoch = version.split_once(':').map(|(_, v)| v).unwrap_or(version);
    format!(
        "{}/pool/main/{}/{}/{}_{}.dsc",
        base.trim_end_matches('/'), prefix, name, name, ver_no_epoch
    )
}

fn parse_dsc_files(dsc: &str) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let mut in_section = "";
    for line in dsc.lines() {
        if line.starts_with("Files:") {
            in_section = "files"; continue;
        }
        if line.starts_with("Checksums-Sha256:") {
            in_section = "sha256"; continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() == 3 {
                let fname = parts[2].to_string();
                let sha   = if in_section == "sha256" { parts[0].to_string() } else { String::new() };
                if let Some(e) = files.iter_mut().find(|(_, f): &&mut (String,String)| f == &fname) {
                    if !sha.is_empty() { e.0 = sha; }
                } else {
                    files.push((sha, fname));
                }
            }
        } else {
            in_section = "";
        }
    }
    files
}

// ─────────────────────────────────────────────────────────────
//  hammer mock — clean build environment
// ─────────────────────────────────────────────────────────────

pub async fn cmd_mock(args: &[String]) -> Result<()> {
    let suite = args.iter()
    .find(|a| a.starts_with("--suite="))
    .and_then(|a| a.strip_prefix("--suite="))
    .unwrap_or("bookworm");

    // FIX E0515: bind detect_arch() result to an owned String first,
    // so we don't return a &str referencing a temporary.
    let detected_arch = crate::cache::detect_arch();
    let arch: &str = args.iter()
    .find(|a| a.starts_with("--arch="))
    .and_then(|a| a.strip_prefix("--arch="))
    .unwrap_or(detected_arch.as_str());

    let clean = args.iter().any(|a| a == "--clean");
    let shell = args.iter().any(|a| a == "--shell");

    let mock_root = PathBuf::from(format!("/var/cache/hammer/mock/{}-{}", suite, arch));

    if clean && mock_root.exists() {
        println!("  {} Cleaning mock root {}…", "·".dimmed(), mock_root.display());
        std::fs::remove_dir_all(&mock_root)?;
    }

    println!();
    println!("  {}  hammer mock — {} {}",
             "⬡".bright_cyan().bold(), suite.bold(), arch.dimmed());
    println!("  Root: {}", mock_root.display().to_string().dimmed());
    println!("  {}", "─".repeat(60).dimmed());

    if !mock_root.exists() {
        println!("  {} Bootstrapping {} environment…", "·".cyan(), suite);
        bootstrap_mock_root(&mock_root, suite, arch).await?;
    }

    if shell {
        println!("  {} Entering mock shell (exit to return)…", "·".cyan());
        enter_mock_shell(&mock_root)?;
    } else {
        println!("  {} Mock environment ready at {}", "✔".bright_green(),
                 mock_root.display().to_string().cyan());
        println!("  Shell: {}", format!("hammer mock --shell --suite={}", suite).cyan());
    }
    Ok(())
}

async fn bootstrap_mock_root(root: &Path, suite: &str, arch: &str) -> Result<()> {
    std::fs::create_dir_all(root)?;

    let has_debootstrap = std::process::Command::new("which")
    .arg("debootstrap")
    .stdout(std::process::Stdio::null())
    .status()
    .map(|s| s.success())
    .unwrap_or(false);

    if has_debootstrap {
        println!("  {} Running debootstrap…", "·".dimmed());
        let status = std::process::Command::new("debootstrap")
        .args([
            "--arch", arch,
            "--variant=minbase",
            suite,
            root.to_str().unwrap_or(""),
              "http://deb.debian.org/debian",
        ])
        .status()
        .context("Running debootstrap")?;

        if !status.success() {
            bail!("debootstrap failed with exit code {:?}", status.code());
        }
        println!("  {} Bootstrap complete.", "✔".bright_green());
    } else {
        println!("  {} debootstrap not found.", "!".yellow().bold());
        println!("  Install it first: {}", "hammer install debootstrap".cyan());
        bail!("debootstrap required for mock bootstrap");
    }
    Ok(())
}

fn enter_mock_shell(root: &Path) -> Result<()> {
    let root_str = root.to_str().unwrap_or("/");

    if std::process::Command::new("which").arg("systemd-nspawn")
        .stdout(std::process::Stdio::null()).status()
        .map(|s| s.success()).unwrap_or(false)
        {
            let status = std::process::Command::new("systemd-nspawn")
            .args(["--quiet", "--directory", root_str, "/bin/bash", "--login"])
            .status()
            .context("Entering nspawn shell")?;
            std::process::exit(status.code().unwrap_or(0));
        } else {
            println!("  {} Using chroot (limited isolation).", "!".yellow());
            let status = std::process::Command::new("chroot")
            .args([root_str, "/bin/bash"])
            .status()
            .context("chroot")?;
            std::process::exit(status.code().unwrap_or(0));
        }
}

#[cfg(test)]
mod sources_url_tests {
    use super::derive_sources_url;

    #[test]
    fn test_derive_sources_url() {
        assert_eq!(
            derive_sources_url("https://deb.debian.org/debian/dists/bookworm/main/binary-amd64/Packages"),
                   Some("https://deb.debian.org/debian/dists/bookworm/main/source/Sources".to_string())
        );
        assert_eq!(
            derive_sources_url("https://deb.debian.org/debian/dists/bookworm/main/binary-amd64/Packages.xz"),
                   Some("https://deb.debian.org/debian/dists/bookworm/main/source/Sources".to_string())
        );
        // Flat repo / unrecognised layout
        assert_eq!(derive_sources_url("file:///srv/repo/Packages"), None);
    }
}

// ─────────────────────────────────────────────────────────────
//  cmd_build — build a source package (item 8)
// ─────────────────────────────────────────────────────────────

/// `hammer build [--arch-only|--indep-only] [--sbuild] <dir>`
pub async fn cmd_build(args: &[String]) -> Result<()> {
    use owo_colors::OwoColorize;

    let arch_only  = has_flag(args, "--arch-only");
    let indep_only = has_flag(args, "--indep-only");
    let use_sbuild = has_flag(args, "--sbuild");
    let dir = args.iter().find(|a| !a.starts_with('-'))
        .map(|s| std::path::PathBuf::from(s))
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    if !dir.join("debian/control").exists() {
        anyhow::bail!(
            "'{}' does not look like a Debian source directory (no debian/control).",
            dir.display()
        );
    }

    // First install build-deps
    println!("  {}  Installing build-dependencies…", "⬡".bright_cyan().bold());
    let pkg_name = read_source_name(&dir)?;
    let install_args = vec![pkg_name.clone()];
    let _flags = crate::cli_types::GlobalFlags::default();
    cmd_build_dep(&install_args).await?;

    // Build
    println!();
    println!("  {}  Building {}…", "⬡".bright_cyan().bold(), pkg_name.bold());

    if use_sbuild {
        build_with_sbuild(&dir, arch_only, indep_only)?;
    } else {
        build_with_dpkg_buildpackage(&dir, arch_only, indep_only)?;
    }
    Ok(())
}

fn read_source_name(dir: &std::path::Path) -> Result<String> {
    let control = std::fs::read_to_string(dir.join("debian/control"))?;
    for line in control.lines() {
        if let Some(name) = line.strip_prefix("Source:") {
            return Ok(name.trim().to_string());
        }
        if let Some(name) = line.strip_prefix("Package:") {
            return Ok(name.trim().to_string());
        }
    }
    anyhow::bail!("Could not determine package name from debian/control")
}

fn build_with_dpkg_buildpackage(
    dir:        &std::path::Path,
    arch_only:  bool,
    indep_only: bool,
) -> Result<()> {
    use owo_colors::OwoColorize;
    let mut args = vec!["-us", "-uc"];  // unsigned build
    if arch_only  { args.push("-B"); }
    if indep_only { args.push("-A"); }

    println!("  {} dpkg-buildpackage {}", "→".cyan(), args.join(" ").dimmed());
    let status = std::process::Command::new("dpkg-buildpackage")
        .args(&args)
        .current_dir(dir)
        .status()
        .context("dpkg-buildpackage not found — install dpkg-dev")?;

    if !status.success() {
        anyhow::bail!("dpkg-buildpackage failed with exit {}", status);
    }
    println!("  {} Build complete.", "✔".bright_green().bold());
    Ok(())
}

fn build_with_sbuild(
    dir:        &std::path::Path,
    arch_only:  bool,
    indep_only: bool,
) -> Result<()> {
    use owo_colors::OwoColorize;
    let mut args = vec!["--no-clean-source"];
    if arch_only  { args.push("--arch-only"); }
    if indep_only { args.push("--indep-only"); }

    println!("  {} sbuild {}", "→".cyan(), args.join(" ").dimmed());
    let status = std::process::Command::new("sbuild")
        .args(&args)
        .current_dir(dir)
        .status()
        .context("sbuild not found — install sbuild")?;

    if !status.success() {
        anyhow::bail!("sbuild failed with exit {}", status);
    }
    println!("  {} sbuild complete.", "✔".bright_green().bold());
    Ok(())
}

// cmd_build_dep already defined but needs --arch-only / --indep-only flags check:
/// Override of cmd_build_dep to accept &GlobalFlags (for cli.rs compat)
pub async fn cmd_build_dep_with_flags(args: &[String], _flags: &crate::cli_types::GlobalFlags) -> Result<()> {
    cmd_build_dep(args).await
}
