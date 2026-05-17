use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use reqwest::{Client, StatusCode};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;

use crate::package::Package;

pub const DL_DIR: &str = "/var/cache/hammer/archives";

const MAX_CONCURRENT: usize = 4;
const MAX_RETRIES:    usize = 3;
const RETRY_DELAY:    u64   = 2;

const USER_AGENT: &str = concat!(
    "hammer/",
    env!("CARGO_PKG_VERSION"),
                                 " (https://github.com/HackerOS-Linux-System/hammer/)"
);

// ─────────────────────────────────────────────────────────────
//  HttpClient
// ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct HttpClient {
    inner: Client,
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
        let resp = self
        .inner
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {}", url))?;
        if !resp.status().is_success() {
            bail!("HTTP {} for {}", resp.status(), url);
        }
        Ok(resp.bytes().await?.to_vec())
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
//  Progress bar styles — yarn-inspired
//
//  Each package gets a spinner line:
//    ⠙ fetch  vim 9.1.0646-1             1.7 MiB/s
//    ✔ fetch  vim 9.1.0646-1             2.3 MiB  cached
//    ✗ fetch  vim 9.1.0646-1             404 Not Found
//
//  Below all spinners, one overall bytes bar:
//    ⬡  [━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━] 100%  38.4 MiB / 38.4 MiB
// ─────────────────────────────────────────────────────────────

fn pkg_spinner_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "  {spinner:.cyan}  {prefix:<38.bold}  {bytes_per_sec:>10}  {bytes:>8}",
    )
    .unwrap()
    .tick_strings(&[
        "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "·",
    ])
}

fn overall_bar_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "  {prefix:.bold.cyan}  [{bar:42.cyan/238}]  {percent:>3}%  {bytes:>9} / {total_bytes}  {elapsed}",
    )
    .unwrap()
    .progress_chars("━━─")
}

// ─────────────────────────────────────────────────────────────
//  Public API
// ─────────────────────────────────────────────────────────────

pub async fn download_packages(
    client:   &HttpClient,
    packages: &[Package],
) -> Result<Vec<DownloadResult>> {
    if packages.is_empty() {
        return Ok(Vec::new());
    }

    std::fs::create_dir_all(DL_DIR).context("Cannot create download cache dir")?;

    let total_bytes: u64 = packages.iter().filter_map(|p| p.download_size).sum();

    let mp = MultiProgress::new();

    // ── Overall progress bar (at the bottom) ──────────────────
    let overall = mp.add(ProgressBar::new(total_bytes.max(1)));
    overall.set_style(overall_bar_style());
    overall.set_prefix("downloading");

    // ── Per-package spinner (inserted above overall) ──────────
    let sty = pkg_spinner_style();
    let sem = Arc::new(Semaphore::new(MAX_CONCURRENT));
    let mut handles = Vec::new();

    for pkg in packages {
        let base_uri = match &pkg.repo_base_uri {
            Some(u) => u.clone(),
            None => bail!(
                "Package '{}' has no repository URI — run `hammer sync` first.",
                pkg.name
            ),
        };
        let filename = match &pkg.filename {
            Some(f) => f.clone(),
            None => bail!(
                "Package '{}' has no filename in metadata — run `hammer sync` first.",
                pkg.name
            ),
        };

        let url  = format!("{}/{}", base_uri.trim_end_matches('/'), filename);
        let dest = pkg_dest_path(pkg);

        let pb = mp.insert_before(&overall, ProgressBar::new_spinner());
        pb.set_style(sty.clone());
        pb.set_prefix(format!("{} {}", pkg.name, pkg.version));
        pb.set_message("pending".dimmed().to_string());
        pb.enable_steady_tick(Duration::from_millis(80));

        let client_c  = client.clone();
        let overall_c = overall.clone();
        let pkg_c     = pkg.clone();
        let sem_c     = Arc::clone(&sem);

        let handle: tokio::task::JoinHandle<Result<DownloadResult>> =
        tokio::spawn(async move {
            let _permit = sem_c.acquire().await.expect("Semaphore closed");
            let result =
            download_with_retry(&client_c, &url, &dest, &pb, &overall_c)
            .await;
            match result {
                Ok(()) => {
                    let size = std::fs::metadata(&dest)
                    .map(|m| m.len())
                    .unwrap_or(0);
                    pb.finish_with_message(format!(
                        "{}",
                        crate::ui::human_size(size).green()
                    ));
                    Ok(DownloadResult {
                        package: pkg_c,
                        path:    dest,
                    })
                }
                Err(e) => {
                    pb.finish_with_message(
                        format!("✗ {}", e).red().to_string(),
                    );
                    Err(e)
                }
            }
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
        for f in &failures {
            eprintln!("    {} {}", "·".dimmed(), f);
        }
        eprintln!();
        bail!(
            "{} package(s) could not be downloaded.\nTransaction aborted.",
              failures.len()
        );
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
) -> Result<()> {
    // Already cached?
    if let Ok(meta) = std::fs::metadata(dest) {
        if meta.len() > 0 {
            let n = meta.len();
            overall.inc(n);
            pb.set_message("cached".dimmed().to_string());
            return Ok(());
        }
    }

    let mut last_err = anyhow::anyhow!("No attempts made");
    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            let delay = RETRY_DELAY * attempt as u64;
            pb.set_message(
                format!("retry {}/{}", attempt, MAX_RETRIES - 1)
                    .yellow()
                    .to_string(),
            );
            tokio::time::sleep(Duration::from_secs(delay)).await;
        }
        match download_one(client, url, dest, pb, overall).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = e;
                if last_err.to_string().contains("404") {
                    break;
                }
            }
        }
    }
    Err(last_err)
}

async fn download_one(
    client:  &HttpClient,
    url:     &str,
    dest:    &Path,
    pb:      &ProgressBar,
    overall: &ProgressBar,
) -> Result<()> {
    let resp = client
    .inner
    .get(url)
    .send()
    .await
    .with_context(|| format!("GET {}", url))?;

    if resp.status() == StatusCode::NOT_FOUND {
        bail!("404 Not Found: {}", url);
    }
    if !resp.status().is_success() {
        bail!("HTTP {} for {}", resp.status(), url);
    }

    if let Some(len) = resp.content_length() {
        pb.set_length(len);
    }

    let tmp = dest.with_extension("part");
    let _ = tokio::fs::remove_file(&tmp).await;
    let mut file = tokio::fs::File::create(&tmp)
    .await
    .with_context(|| format!("Cannot create {:?}", tmp))?;

    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("Stream error")?;
        file.write_all(&chunk).await?;
        let n = chunk.len() as u64;
        pb.inc(n);
        overall.inc(n);
    }
    file.flush().await?;
    drop(file);

    tokio::fs::rename(&tmp, dest)
    .await
    .with_context(|| format!("Cannot rename {:?} → {:?}", tmp, dest))?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Utilities
// ─────────────────────────────────────────────────────────────

pub fn pkg_dest_path(pkg: &Package) -> PathBuf {
    let safe_ver = pkg.version.replace(':', "%3A").replace('/', "%2F");
    PathBuf::from(DL_DIR)
    .join(format!("{}_{}_{}.deb", pkg.name, safe_ver, pkg.architecture))
}

pub fn clean_cache() -> anyhow::Result<usize> {
    let dir = std::path::Path::new(DL_DIR);
    if !dir.exists() {
        return Ok(0);
    }
    let mut count = 0usize;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path  = entry.path();
        if path.extension().map_or(false, |e| e == "deb") {
            std::fs::remove_file(&path)?;
            count += 1;
        }
    }
    Ok(count)
}

// ─────────────────────────────────────────────────────────────
//  UnpackSpinner — yarn-style unpack indicator
//
//    ⠙  Unpacking  vim 9.1.0646-1…
// ─────────────────────────────────────────────────────────────

pub struct UnpackSpinner(ProgressBar);

impl UnpackSpinner {
    pub fn new(mp: &MultiProgress, label: &str) -> Self {
        let pb = mp.add(ProgressBar::new_spinner());
        pb.set_style(
            ProgressStyle::with_template(
                "  {spinner:.cyan}  {prefix:.bold}  {wide_msg}",
            )
            .unwrap()
            .tick_strings(&[
                "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "·",
            ]),
        );
        pb.set_prefix("unpacking");
        pb.set_message(label.to_string());
        pb.enable_steady_tick(Duration::from_millis(80));
        UnpackSpinner(pb)
    }

    pub fn finish_ok(self, label: &str) {
        self.0.finish_with_message(
            format!("{}  {}", "✔".bright_green(), label).to_string(),
        );
    }

    pub fn finish(self) {
        self.0.finish_and_clear();
    }
}
