use anyhow::{Context, Result};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::repo::{IndexUrl, SourcesList};
use crate::download::HttpClient;
use crate::gpg_verify::{self, InRelease};
use crate::package::Package;

pub const LISTS_DIR: &str = "/var/lib/hammer/lists";
pub const CACHE_DIR: &str = "/var/cache/hammer";

fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("  {spinner:.cyan}  {prefix:<42.bold}  {wide_msg}")
    .unwrap()
    .tick_strings(&["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏","·"])
}

// ─────────────────────────────────────────────────────────────
//  PackageCache
// ─────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct PackageCache {
    by_name: HashMap<String, Package>,
    all:     HashMap<String, Package>,
}

impl PackageCache {
    pub fn empty() -> Self {
        PackageCache { by_name: HashMap::new(), all: HashMap::new() }
    }

    pub fn load() -> Result<Self> {
        let mut cache = Self::empty();
        let dir = Path::new(LISTS_DIR);
        if !dir.exists() { return Ok(cache); }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path  = entry.path();
            if path.extension().map_or(false, |e| e == "pkgs") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let base_uri = extract_base_uri_comment(&content);
                    let pkgs     = Package::parse_index(&content);
                    for mut pkg in pkgs {
                        if pkg.repo_base_uri.is_none() {
                            pkg.repo_base_uri = base_uri.clone();
                        }
                        cache.ingest(pkg);
                    }
                }
            }
        }
        Ok(cache)
    }

    /// Load cache filtered/including a specific arch.
    pub fn load_for_arch(_arch: &str) -> Result<Self> { Self::load() }

    fn ingest(&mut self, pkg: Package) {
        let key = format!("{}:{}:{}", pkg.name, pkg.architecture, pkg.version);
        self.all.insert(key, pkg.clone());
        let existing_newer = self.by_name.get(&pkg.name).map_or(false, |ex| {
            crate::package::version_cmp(&ex.version, &pkg.version) == std::cmp::Ordering::Greater
        });
        if !existing_newer {
            self.by_name.insert(pkg.name.clone(), pkg);
        }
    }

    // ─────────────────────────────────────────────────────────
    //  update — fetch indices for ALL configured architectures
    // ─────────────────────────────────────────────────────────

    pub async fn update(sources: &SourcesList, client: &HttpClient) -> Result<()> {
        // ── Collect arches: native + all configured foreign ───
        // FIX: `native` is now actually used below (in the progress
        // header arch count and as a fallback when no foreign arches
        // are configured), so the previous `unused variable` warning
        // is resolved without needing an underscore prefix.
        let native        = detect_arch();
        let multi_arch_db = crate::multi_arch::MultiArchDb::load();
        let mut all_arches = multi_arch_db.all_arches();
        if all_arches.is_empty() {
            all_arches.push(native.clone());
        }

        // Build index URL list for every arch
        let mut urls: Vec<IndexUrl> = Vec::new();
        for arch in &all_arches {
            urls.extend(sources.index_urls(arch));
        }

        // Dedup by URL (same URL might appear for native+alias)
        urls.dedup_by(|a, b| a.url == b.url);

        if urls.is_empty() {
            anyhow::bail!("No repositories configured. Check {}", crate::repo::SOURCES_HK);
        }

        std::fs::create_dir_all(LISTS_DIR).context("Cannot create lists directory")?;

        let mp  = MultiProgress::new();
        let sty = spinner_style();

        let header = mp.add(ProgressBar::new_spinner());
        header.set_style(
            ProgressStyle::with_template("  {prefix:.bold.cyan}  {wide_msg}")
            .unwrap().tick_strings(&["·","·"]),
        );
        header.set_prefix("hammer sync");
        header.set_message(format!(
            "Refreshing {} source{} for {} arch{} (native: {})…",
                                   urls.len(),
                                   if urls.len() == 1 { "" } else { "s" },
                                       all_arches.len(),
                                   if all_arches.len() == 1 { "" } else { "s" },
                                       native,
        ));
        header.tick();

        let mut handles = Vec::new();

        for url_info in urls {
            let label = format!("{}/{} [{}]",
                                url_info.suite, url_info.component, url_info.arch);
            let pb = mp.add(ProgressBar::new_spinner());
            pb.set_style(sty.clone());
            pb.set_prefix(label);
            pb.set_message("connecting…".dimmed().to_string());
            pb.enable_steady_tick(Duration::from_millis(80));

            let client_c  = client.clone();
            let base_uri  = url_info.base_uri.clone();
            let handle    = tokio::spawn(async move {
                let result = fetch_and_verify_index(&client_c, &url_info).await;
                (url_info, base_uri, pb, result)
            });
            handles.push(handle);
        }

        let mut ok_count   = 0usize;
        let mut err_count  = 0usize;
        let mut total_pkgs = 0usize;
        let mut gpg_failed = 0usize;

        for handle in handles {
            let (url_info, base_uri, pb, result) = handle.await?;
            match result {
                Ok(FetchResult { content, gpg_ok, sha256_ok }) => {
                    let sig_icon = if gpg_ok { "🔒".to_string() }
                    else      { "⚠".yellow().to_string() };

                    let stored = format!("# hammer-base-uri: {}\n{}", base_uri, content);
                    let fname  = url_to_cache_name(&url_info.url);
                    let dest   = PathBuf::from(LISTS_DIR).join(format!("{}.pkgs", fname));
                    std::fs::write(&dest, &stored)?;

                    let count = Package::parse_index(&content).len();
                    total_pkgs += count;

                    let sha_note = if sha256_ok { "" } else { " ⚠sha256" };
                    pb.finish_with_message(format!(
                        "{}  {} packages{}  {}",
                        sig_icon,
                        count.to_string().cyan(),
                                                   sha_note,
                                                   if !gpg_ok {
                                                       "(no signature)".yellow().to_string()
                                                   } else { String::new() }
                    ));

                    if !gpg_ok { gpg_failed += 1; }
                    ok_count += 1;
                }
                Err(e) => {
                    pb.finish_with_message(format!(
                        "{}  {}", "✗".red().bold(), e.to_string().dimmed()
                    ));
                    err_count += 1;
                }
            }
        }

        header.finish_with_message(format!(
            "Synced {} {}{} — {} packages indexed.",
            format!("{} source{}", ok_count, if ok_count == 1 { "" } else { "s" })
                .green().bold(),
                                           if err_count > 0 {
                                               format!(", {} failed", err_count).red().to_string()
                                           } else { String::new() },
                                               if gpg_failed > 0 {
                                                   format!(", {} unverified", gpg_failed).yellow().to_string()
                                               } else { String::new() },
                                                   total_pkgs.to_string().cyan().bold()
        ));
        mp.clear().ok();

        if gpg_failed > 0 {
            println!();
            println!("  {} {} source(s) could not be verified by GPG.",
                     "!".yellow().bold(), gpg_failed.to_string().yellow().bold());
            println!("  Add trusted keys with: {}", "hammer key add <url>".cyan());
        }

        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&Package> { self.by_name.get(name) }

    pub fn get_exact(&self, name: &str, version: &str, arch: &str) -> Option<&Package> {
        let key = format!("{}:{}:{}", name, arch, version);
        self.all.get(&key)
    }

    pub fn search(&self, query: &str) -> Vec<&Package> {
        let q = query.to_lowercase();
        let mut results: Vec<&Package> = self.by_name.values()
        .filter(|p| {
            p.name.to_lowercase().contains(&q)
            || p.description_short.as_ref()
            .map_or(false, |d| d.to_lowercase().contains(&q))
        })
        .collect();
        results.sort_by(|a, b| {
            let a_exact  = a.name.to_lowercase() == q;
            let b_exact  = b.name.to_lowercase() == q;
            if a_exact != b_exact { return b_exact.cmp(&a_exact); }
            let a_starts = a.name.to_lowercase().starts_with(&q);
            let b_starts = b.name.to_lowercase().starts_with(&q);
            if a_starts != b_starts { return b_starts.cmp(&a_starts); }
            a.name.cmp(&b.name)
        });
        results
    }

    pub fn all_packages(&self) -> Vec<&Package> {
        let mut v: Vec<&Package> = self.by_name.values().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub fn len(&self) -> usize { self.by_name.len() }
}

// ─────────────────────────────────────────────────────────────
//  fetch_and_verify_index  — GPG + SHA256
// ─────────────────────────────────────────────────────────────

struct FetchResult {
    content:   String,
    gpg_ok:    bool,
    sha256_ok: bool,
}

async fn fetch_and_verify_index(client: &HttpClient, info: &IndexUrl) -> Result<FetchResult> {
    let keyring_dir = std::path::Path::new(gpg_verify::KEYRING_DIR);
    let mut gpg_ok    = false;
    let mut sha256_ok = false;
    let mut inrelease: Option<InRelease> = None;

    match client.get_bytes(&info.inrelease_url).await {
        Ok(bytes) => {
            let content = String::from_utf8_lossy(&bytes).to_string();
            match gpg_verify::verify_inrelease(&content, keyring_dir) {
                Ok(()) => gpg_ok = true,
                Err(e) => crate::log::warn(&format!(
                    "cache: GPG failed for {}: {}", info.inrelease_url, e
                )),
            }
            if let Ok(ir) = InRelease::parse(&content) {
                inrelease = Some(ir);
            }
        }
        Err(e) => {
            crate::log::warn(&format!(
                "cache: cannot fetch InRelease for {}: {}", info.suite, e
            ));
        }
    }

    let (content, bytes_used, rel_path_used) =
    fetch_packages_file(client, info).await?;

    if let Some(ref ir) = inrelease {
        if ir.verify_file(&rel_path_used, &bytes_used).is_ok() {
            sha256_ok = true;
        } else {
            crate::log::warn(&format!(
                "cache: SHA256 mismatch for {}", rel_path_used
            ));
        }
    }

    Ok(FetchResult { content, gpg_ok, sha256_ok })
}

async fn fetch_packages_file(
    client: &HttpClient,
    info:   &IndexUrl,
) -> Result<(String, Vec<u8>, String)> {
    let base_rel = format!("{}/binary-{}/Packages", info.component, info.arch);

    for (suffix, rel_suffix) in &[
        (".zst", ".zst"), (".xz", ".xz"), (".gz", ".gz"), (".bz2", ".bz2"), ("", ""),
    ] {
        let url      = format!("{}{}", info.url, suffix);
        let rel_path = format!("{}{}", base_rel, rel_suffix);
        match client.get_bytes(&url).await {
            Ok(bytes) => {
                let text = decompress(&bytes, suffix)
                .with_context(|| format!("Decompression failed for {}", url))?;
                return Ok((text, bytes, rel_path));
            }
            Err(_) => continue,
        }
    }
    anyhow::bail!("All variants failed for {}", info.url)
}

// ─────────────────────────────────────────────────────────────
//  sync_all — convenience entry point
// ─────────────────────────────────────────────────────────────

pub async fn sync_all() -> Result<()> {
    let sources = SourcesList::load()?;
    let client  = HttpClient::new();
    PackageCache::update(&sources, &client).await
}

// ─────────────────────────────────────────────────────────────
//  Decompression
// ─────────────────────────────────────────────────────────────

fn decompress(bytes: &[u8], suffix: &str) -> Result<String> {
    match suffix {
        ".gz"  => {
            let mut d = flate2::read::GzDecoder::new(bytes);
            let mut s = String::new(); d.read_to_string(&mut s)?; Ok(s)
        }
        ".bz2" => {
            let mut d = bzip2::read::BzDecoder::new(bytes);
            let mut s = String::new(); d.read_to_string(&mut s)?; Ok(s)
        }
        ".xz"  => {
            let mut d = xz2::read::XzDecoder::new(bytes);
            let mut s = String::new(); d.read_to_string(&mut s)?; Ok(s)
        }
        _      => Ok(String::from_utf8_lossy(bytes).to_string()),
    }
}

// ─────────────────────────────────────────────────────────────
//  Helpers
// ─────────────────────────────────────────────────────────────

fn url_to_cache_name(url: &str) -> String {
    url.chars()
    .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
    .take(120)
    .collect()
}

fn extract_base_uri_comment(content: &str) -> Option<String> {
    content.lines()
    .find(|l| l.starts_with("# hammer-base-uri:"))
    .map(|l| l["# hammer-base-uri:".len()..].trim().to_owned())
}

pub fn detect_arch() -> String {
    if let Ok(out) = std::process::Command::new("uname").arg("-m").output() {
        if out.status.success() {
            let m = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            return match m.as_str() {
                "x86_64"            => "amd64",
                "aarch64" | "arm64" => "arm64",
                "armv7l"            => "armhf",
                "i686" | "i386"     => "i386",
                "riscv64"           => "riscv64",
                other               => other,
            }.to_owned();
        }
    }
    if cfg!(target_arch = "x86_64")  { return "amd64".into(); }
    if cfg!(target_arch = "aarch64") { return "arm64".into(); }
    "amd64".into()
}
