use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use reqwest::{Client, StatusCode};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

use crate::package::Package;

pub const DL_DIR: &str = "/var/cache/hammer/archives";

const MAX_CONCURRENT: usize = 4;
const MAX_RETRIES:    usize = 3;
const RETRY_DELAY:    u64   = 2;

const USER_AGENT: &str = concat!(
    "hammer/", env!("CARGO_PKG_VERSION"),
                                 " (https://github.com/HackerOS-Linux-System/hammer)"
);

// Progress bar characters — filled / empty blocks (▰ ▱)
// These are U+25B0 and U+25B1
const FILLED: &str = "\u{25B0}";  // ▰
const EMPTY:  &str = "\u{25B1}";  // ▱

// ─────────────────────────────────────────────────────────────
//  HttpClient
// ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct HttpClient {
    pub inner: Client,
}

impl HttpClient {
    pub fn new() -> Self {
        let inner = Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(300))
        .connect_timeout(Duration::from_secs(20))
        .tcp_keepalive(Duration::from_secs(30))
        .pool_max_idle_per_host(4)
        .gzip(true)
        .deflate(true)
        .build()
        .expect("Failed to build HTTP client");
        HttpClient { inner }
    }

    pub async fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self.inner.get(url).send().await
        .with_context(|| format!("GET {}", url))?;
        if !resp.status().is_success() {
            bail!("HTTP {} for {}", resp.status(), url);
        }
        Ok(resp.bytes().await?.to_vec())
    }

    pub async fn get_string(&self, url: &str) -> Result<String> {
        let bytes = self.get_bytes(url).await?;
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }

    /// GET with conditional request (If-Modified-Since).
    /// Returns None if server returns 304 Not Modified.
    pub async fn get_bytes_if_modified(
        &self,
        url:            &str,
        last_modified:  Option<&str>,
    ) -> Result<Option<Vec<u8>>> {
        let mut req = self.inner.get(url);
        if let Some(lm) = last_modified {
            req = req.header("If-Modified-Since", lm);
        }
        let resp = req.send().await
            .with_context(|| format!("GET {}", url))?;

        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(None);  // cache is still valid
        }
        if !resp.status().is_success() {
            bail!("HTTP {} for {}", resp.status(), url);
        }
        Ok(Some(resp.bytes().await?.to_vec()))
    }

    /// Returns (bytes, Last-Modified header value).
    pub async fn get_bytes_with_meta(
        &self,
        url: &str,
    ) -> Result<(Vec<u8>, Option<String>)> {
        let resp = self.inner.get(url).send().await
            .with_context(|| format!("GET {}", url))?;
        if !resp.status().is_success() {
            bail!("HTTP {} for {}", resp.status(), url);
        }
        let lm = resp
            .headers()
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        Ok((resp.bytes().await?.to_vec(), lm))
    }
}

// ─────────────────────────────────────────────────────────────
//  DownloadResult
// ─────────────────────────────────────────────────────────────

pub struct DownloadResult {
    pub package: Package,
    pub path:    PathBuf,
}

// ─────────────────────────────────────────────────────────────
//  Progress bar styles
//
//  Overall header (spinner + total):
//    ⠙ Downloading  [▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱]  19.18 MiB/373.63 MiB  10.17 MiB/s  ETA 35s
//
//  Per-package line:
//    breeze-wallpaper 4:6.6.4-1           ▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱  4.09 MiB/38.31 MiB
//
//  The ▰ chars are printed in bright orange (\x1b[38;5;208m)
//  and ▱ chars in dim grey.
// ─────────────────────────────────────────────────────────────

/// Build a visual progress bar string using ▰/▱ with ANSI colour.
/// `filled_frac` is 0.0–1.0. `width` is number of characters.
fn block_bar(filled_frac: f64, width: usize) -> String {
    let filled = (filled_frac.clamp(0.0, 1.0) * width as f64).round() as usize;
    let empty  = width.saturating_sub(filled);
    // Orange for filled (256-colour code 208), dim white for empty
    let f_part = format!("\x1b[38;5;208m{}\x1b[0m", FILLED.repeat(filled));
    let e_part = format!("\x1b[2m{}\x1b[0m", EMPTY.repeat(empty));
    format!("{}{}", f_part, e_part)
}

fn overall_style() -> ProgressStyle {
    // indicatif template — we customise the bar chars via progress_chars
    // Orange filled ▰, empty ▱
    ProgressStyle::with_template(
        "  {spinner:.cyan}  {prefix:.bold}  [{wide_bar}]  {bytes}/{total_bytes}  {bytes_per_sec}  ETA {eta}"
    )
    .unwrap()
    .with_key("wide_bar", |state: &indicatif::ProgressState, w: &mut dyn std::fmt::Write| {
        let pct = if state.len().unwrap_or(1) == 0 { 0.0 }
        else { state.pos() as f64 / state.len().unwrap() as f64 };
        let width: usize = 38;
        let filled = (pct * width as f64).round() as usize;
        let empty  = width.saturating_sub(filled);
        let _ = write!(w, "\x1b[38;5;208m{}\x1b[0m\x1b[2m{}\x1b[0m",
                       "\u{25B0}".repeat(filled), "\u{25B1}".repeat(empty));
    })
    .tick_strings(&["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏","·"])
}

fn pkg_style() -> ProgressStyle {
    // Per-package: label  [bar]  bytes/total
    ProgressStyle::with_template(
        "    {prefix:<42}  [{wide_bar}]  {bytes:>9}/{total_bytes}"
    )
    .unwrap()
    .with_key("wide_bar", |state: &indicatif::ProgressState, w: &mut dyn std::fmt::Write| {
        let pct = if state.len().unwrap_or(1) == 0 { 0.0 }
        else { state.pos() as f64 / state.len().unwrap() as f64 };
        let width: usize = 28;
        let filled = (pct * width as f64).round() as usize;
        let empty  = width.saturating_sub(filled);
        let _ = write!(w, "\x1b[38;5;208m{}\x1b[0m\x1b[2m{}\x1b[0m",
                       "\u{25B0}".repeat(filled), "\u{25B1}".repeat(empty));
    })
}

// ─────────────────────────────────────────────────────────────
//  Public API
// ─────────────────────────────────────────────────────────────

pub async fn download_packages(
    client:   &HttpClient,
    packages: &[Package],
) -> Result<Vec<DownloadResult>> {
    if packages.is_empty() { return Ok(Vec::new()); }

    std::fs::create_dir_all(DL_DIR).context("Cannot create download cache dir")?;

    let total_bytes: u64 = packages.iter().filter_map(|p| p.download_size).sum();
    let mp = MultiProgress::new();

    // ── Overall bar ───────────────────────────────────────────
    let overall = mp.add(ProgressBar::new(total_bytes.max(1)));
    overall.set_style(overall_style());
    overall.set_prefix("Downloading");
    overall.enable_steady_tick(Duration::from_millis(100));

    // ── Per-package bars (inserted above overall) ─────────────
    let sty = pkg_style();
    let sem = Arc::new(Semaphore::new(MAX_CONCURRENT));
    let mut handles = Vec::new();

    for pkg in packages {
        let base_uri = match &pkg.repo_base_uri {
            Some(u) => u.clone(),
            None => bail!(
                "Package '{}' has no repository URI — run `hammer sync` first.", pkg.name),
        };
        let filename = match &pkg.filename {
            Some(f) => f.clone(),
            None => bail!(
                "Package '{}' has no filename in metadata — run `hammer sync` first.", pkg.name),
        };

        let url  = format!("{}/{}", base_uri.trim_end_matches('/'), filename);
        let dest   = pkg_dest_path(pkg);
        let size   = pkg.download_size.unwrap_or(0);
        let sha256 = pkg.sha256.clone();

        // Label: "name version" padded to 40 chars
        let label = format!("{} {}", pkg.name, pkg.version);

        let pb = mp.insert_before(&overall, ProgressBar::new(size.max(1)));
        pb.set_style(sty.clone());
        pb.set_prefix(label);

        let client_c  = client.clone();
        let overall_c = overall.clone();
        let pkg_c     = pkg.clone();
        let sem_c     = Arc::clone(&sem);

        let handle: tokio::task::JoinHandle<Result<DownloadResult>> =
        tokio::spawn(async move {
            let _permit = sem_c.acquire().await.expect("semaphore closed");
            let sha_ref = sha256.as_deref();
            let res = download_with_retry(&client_c, &url, &dest, &pb, &overall_c, sha_ref).await;
            match &res {
                Ok(()) => {
                    let sz = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
                    pb.finish_with_message(crate::ui::human_size(sz).green().to_string());
                }
                Err(e) => {
                    pb.finish_with_message(format!("\x1b[31m✗\x1b[0m {}", e));
                }
            }
            res.map(|_| DownloadResult { package: pkg_c, path: dest })
        });
        handles.push(handle);
    }

    let mut results  = Vec::new();
    let mut failures = Vec::new();

    for handle in handles {
        match handle.await {
            Ok(Ok(r))  => results.push(r),
            Ok(Err(e)) => failures.push(format!("{:#}", e)),
            Err(e)     => failures.push(format!("Task panic: {}", e)),
        }
    }

    overall.finish_and_clear();
    mp.clear().ok();

    if !failures.is_empty() {
        eprintln!();
        eprintln!("  {} {} download(s) failed:", "✗".red().bold(), failures.len());
        for f in &failures { eprintln!("    {} {}", "·".dimmed(), f); }
        eprintln!();
        bail!("{} package(s) could not be downloaded.\nTransaction aborted.", failures.len());
    }

    Ok(results)
}

// ─────────────────────────────────────────────────────────────
//  Internal helpers
// ─────────────────────────────────────────────────────────────

async fn download_with_retry(
    client:  &HttpClient,
    url:     &str,
    dest:    &Path,
    pb:      &ProgressBar,
    overall: &ProgressBar,
    expected_sha256: Option<&str>,
) -> Result<()> {
    // Already downloaded and SHA256 matches — skip entirely
    if let Ok(meta) = std::fs::metadata(dest) {
        if meta.len() > 0 {
            if let Some(sha) = expected_sha256 {
                if file_sha256_matches(dest, sha) {
                    pb.inc(meta.len());
                    overall.inc(meta.len());
                    return Ok(());
                }
                // Hash mismatch → re-download
                let _ = std::fs::remove_file(dest);
            } else {
                pb.inc(meta.len());
                overall.inc(meta.len());
                return Ok(());
            }
        }
    }
    let mut last_err = anyhow::anyhow!("No attempts made");
    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(RETRY_DELAY * attempt as u64)).await;
            pb.reset();
        }
        match download_one(client, url, dest, pb, overall).await {
            Ok(()) => {
                // Verify SHA256 after download
                if let Some(sha) = expected_sha256 {
                    if !file_sha256_matches(dest, sha) {
                        let _ = std::fs::remove_file(dest);
                        last_err = anyhow::anyhow!("SHA256 mismatch for {}", url);
                        continue;
                    }
                }
                return Ok(());
            }
            Err(e) => {
                last_err = e;
                if last_err.to_string().contains("404") { break; }
            }
        }
    }
    Err(last_err)
}

fn file_sha256_matches(path: &Path, expected: &str) -> bool {
    use sha2::{Digest, Sha256};
    let Ok(bytes) = std::fs::read(path) else { return false };
    let computed = format!("{:x}", Sha256::digest(&bytes));
    computed.eq_ignore_ascii_case(expected)
}

#[async_recursion::async_recursion]
async fn download_one(
    client:  &HttpClient,
    url:     &str,
    dest:    &Path,
    pb:      &ProgressBar,
    overall: &ProgressBar,
) -> Result<()> {
    use reqwest::header::{RANGE, CONTENT_RANGE};

    let tmp = dest.with_extension("part");

    // Check if partial file exists — attempt to resume
    let already: u64 = tokio::fs::metadata(&tmp).await
        .map(|m| m.len())
        .unwrap_or(0);

    let (resp, file) = if already > 0 {
        // Send Range request
        let range_val = format!("bytes={}-", already);
        let resp = client.inner.get(url)
            .header(RANGE, &range_val)
            .send().await
            .with_context(|| format!("GET (resume) {}", url))?;

        let status = resp.status();
        if status == StatusCode::RANGE_NOT_SATISFIABLE {
            // Server says range invalid — start fresh
            let _ = tokio::fs::remove_file(&tmp).await;
            return download_one(client, url, dest, pb, overall).await;
        }
        if status == StatusCode::NOT_FOUND { bail!("404 Not Found: {}", url); }

        // 206 Partial Content → resume; 200 OK → server ignored Range, restart
        let resume = status == reqwest::StatusCode::PARTIAL_CONTENT &&
            resp.headers().contains_key(CONTENT_RANGE);

        if resume {
            if let Some(total) = resp.content_length() {
                pb.set_length(already + total);
            }
            pb.set_position(already);
            overall.inc(already);
            let f = tokio::fs::OpenOptions::new().append(true).open(&tmp).await
                .with_context(|| format!("Cannot open partial file {:?}", tmp))?;
            (resp, f)
        } else {
            if !status.is_success() { bail!("HTTP {} for {}", status, url); }
            let _ = tokio::fs::remove_file(&tmp).await;
            if let Some(len) = resp.content_length() { pb.set_length(len); }
            let f = tokio::fs::File::create(&tmp).await
                .with_context(|| format!("Cannot create {:?}", tmp))?;
            (resp, f)
        }
    } else {
        let resp = client.inner.get(url).send().await
            .with_context(|| format!("GET {}", url))?;
        if resp.status() == StatusCode::NOT_FOUND { bail!("404 Not Found: {}", url); }
        if !resp.status().is_success() { bail!("HTTP {} for {}", resp.status(), url); }
        if let Some(len) = resp.content_length() { pb.set_length(len); }
        let f = tokio::fs::File::create(&tmp).await
            .with_context(|| format!("Cannot create {:?}", tmp))?;
        (resp, f)
    };

    let mut file   = file;
    let mut stream = resp.bytes_stream();
    use tokio::io::AsyncWriteExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("Stream error")?;
        file.write_all(&chunk).await?;
        let n = chunk.len() as u64;
        pb.inc(n);
        overall.inc(n);
    }
    file.flush().await?;
    drop(file);
    tokio::fs::rename(&tmp, dest).await
        .with_context(|| format!("Cannot rename {:?} → {:?}", tmp, dest))?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Utilities
// ─────────────────────────────────────────────────────────────

pub fn pkg_dest_path(pkg: &Package) -> PathBuf {
    let safe_ver = pkg.version.replace(':', "%3A").replace('/', "%2F");
    PathBuf::from(DL_DIR).join(format!("{}_{}_{}.deb", pkg.name, safe_ver, pkg.architecture))
}

pub fn clean_cache() -> anyhow::Result<usize> {
    let dir = Path::new(DL_DIR);
    if !dir.exists() { return Ok(0); }
    let mut count = 0;
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        if p.extension().map_or(false, |e| e == "deb") {
            std::fs::remove_file(&p)?;
            count += 1;
        }
    }
    Ok(count)
}

/// Evict least-recently-used cache entries until total size is under `max_bytes`.
/// Returns number of files removed.
pub fn evict_cache_lru(max_bytes: u64) -> anyhow::Result<usize> {
    let dir = Path::new(DL_DIR);
    if !dir.exists() { return Ok(0); }

    // Collect all .deb files with their atime + size
    struct Entry { path: std::path::PathBuf, atime: std::time::SystemTime, size: u64 }
    let mut entries: Vec<Entry> = Vec::new();
    let mut total: u64 = 0;

    for e in std::fs::read_dir(dir)? {
        let e    = e?;
        let path = e.path();
        if !path.extension().map_or(false, |x| x == "deb") { continue; }
        let meta  = std::fs::metadata(&path)?;
        let atime = meta.accessed().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let size  = meta.len();
        total += size;
        entries.push(Entry { path, atime, size });
    }

    if total <= max_bytes { return Ok(0); }

    // Sort by atime ascending (oldest first)
    entries.sort_by_key(|e| e.atime);

    let mut removed = 0;
    for entry in entries {
        if total <= max_bytes { break; }
        if let Ok(()) = std::fs::remove_file(&entry.path) {
            total   -= entry.size;
            removed += 1;
        }
    }
    Ok(removed)
}

// ─────────────────────────────────────────────────────────────
//  UnpackSpinner
// ─────────────────────────────────────────────────────────────

pub struct UnpackSpinner(ProgressBar);

impl UnpackSpinner {
    pub fn new(mp: &MultiProgress, label: &str) -> Self {
        let pb = mp.add(ProgressBar::new_spinner());
        pb.set_style(
            ProgressStyle::with_template("  {spinner:.cyan}  {prefix:.bold}  {wide_msg}")
            .unwrap()
            .tick_strings(&["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏","·"]),
        );
        pb.set_prefix("unpacking");
        pb.set_message(label.to_string());
        pb.enable_steady_tick(Duration::from_millis(80));
        UnpackSpinner(pb)
    }

    pub fn finish_ok(self, label: &str) {
        self.0.finish_with_message(
            format!("{} {}", "\x1b[32m✔\x1b[0m", label)
        );
    }

    pub fn finish(self) { self.0.finish_and_clear(); }
}

impl HttpClient {
    /// GET with an explicit timeout. Returns Err on timeout.
    pub async fn get_string_timeout(
        &self,
        url:      &str,
        duration: std::time::Duration,
    ) -> anyhow::Result<String> {
        let result = tokio::time::timeout(duration, self.get_bytes(url)).await
            .map_err(|_| anyhow::anyhow!("Request timed out after {}s: {}", duration.as_secs(), url))?;
        let bytes = result?;
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }

    /// GET with retry (up to `n` attempts, exponential backoff).
    pub async fn get_bytes_retry(&self, url: &str, attempts: u32) -> anyhow::Result<Vec<u8>> {
        let mut last_err = None;
        for i in 0..attempts {
            match self.get_bytes(url).await {
                Ok(b) => return Ok(b),
                Err(e) => {
                    last_err = Some(e);
                    if i + 1 < attempts {
                        let delay = std::time::Duration::from_secs(2u64.pow(i));
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("All {} attempts failed for {}", attempts, url)))
    }
}
