use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use owo_colors::OwoColorize;
use reqwest::StatusCode;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

use crate::download::HttpClient;
use crate::gpg_verify::{self, InRelease};
use crate::package::Package;
// FIX: removed unused `use crate::repo::SourcesList` — mirror builds its
// own MirrorConfig and does not need the full sources list.

// ─────────────────────────────────────────────────────────────
//  MirrorConfig
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MirrorConfig {
    pub source_url: String,
    pub local_path: PathBuf,
    pub arch:       String,
    pub suite:      String,
    pub components: Vec<String>,
    pub bw_limit:   u64,
    /// Skip GPG verification (dangerous — only for unsigned/private mirrors)
    pub insecure:   bool,
}

impl MirrorConfig {
    pub fn new(url: &str, local: &Path) -> Self {
        MirrorConfig {
            source_url: url.trim_end_matches('/').to_string(),
            local_path: local.to_path_buf(),
            arch:       crate::cache::detect_arch(),
            suite:      "bookworm".to_string(),
            components: vec!["main".to_string(), "contrib".to_string()],
            bw_limit:   0,
            insecure:   false,
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  Mirror command
// ─────────────────────────────────────────────────────────────

pub async fn cmd_mirror(args: &[String]) -> Result<()> {
    let url = args.first()
    .ok_or_else(|| anyhow::anyhow!(
        "Usage: hammer mirror <url> <local-path> [--arch=ARCH] [--suite=SUITE] [--limit=KBPS] [--insecure]"
    ))?;
    let local = args.get(1)
    .ok_or_else(|| anyhow::anyhow!("Usage: hammer mirror <url> <local-path>"))?;

    let mut cfg = MirrorConfig::new(url, Path::new(local));

    for arg in &args[2..] {
        if let Some(a) = arg.strip_prefix("--arch=")  { cfg.arch  = a.to_string(); }
        if let Some(s) = arg.strip_prefix("--suite=") { cfg.suite = s.to_string(); }
        if let Some(c) = arg.strip_prefix("--components=") {
            cfg.components = c.split(',').map(|s| s.trim().to_string()).collect();
        }
        if let Some(l) = arg.strip_prefix("--limit=") {
            cfg.bw_limit = l.parse::<u64>().unwrap_or(0) * 1024;
        }
        if arg == "--insecure" { cfg.insecure = true; }
    }

    println!();
    println!("  {}  Mirroring {} → {}",
             "⬡".bright_cyan().bold(),
             url.cyan(), local.bold());
    println!("  Suite: {}  Arch: {}  Components: {}",
             cfg.suite.bold(), cfg.arch.bold(), cfg.components.join(", ").dimmed());
    if cfg.insecure {
        println!("  {} --insecure: GPG verification DISABLED", "!".red().bold());
    }
    println!("  {}", "─".repeat(65).dimmed());

    let client = HttpClient::new();

    // Mirror InRelease + Packages index, verified
    let inrelease = mirror_index(&client, &cfg).await?;

    // Mirror package files listed in Packages index, with SHA256 per-file checks
    let pkgs = load_packages_from_mirror(&cfg)?;
    mirror_debs(&client, &cfg, &pkgs).await?;

    println!();
    println!("  {} Mirror complete at {}", "✔".bright_green().bold(), local.bold());
    if let Some(ir) = &inrelease {
        println!("  InRelease: {} (valid until {})",
                 if cfg.insecure { "unverified".yellow().to_string() }
                 else            { "GPG OK".bright_green().to_string() },
                     ir.valid_until.as_deref().unwrap_or("unknown").dimmed());
    }
    println!("  To use: add to /etc/hammer/sources-list.hk:");
    println!("    {} baseurl => \"file://{}\"", "->".cyan(), local);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  mirror_index — fetch InRelease + Packages, GPG/SHA256 verified
// ─────────────────────────────────────────────────────────────

async fn mirror_index(client: &HttpClient, cfg: &MirrorConfig) -> Result<Option<InRelease>> {
    let dists_base  = format!("{}/dists/{}", cfg.source_url, cfg.suite);
    let local_dists = cfg.local_path.join("dists").join(&cfg.suite);
    std::fs::create_dir_all(&local_dists)?;

    // ── Fetch + verify InRelease ──────────────────────────────
    let inrelease_url = format!("{}/InRelease", dists_base);
    let inrelease_bytes = client.get_bytes(&inrelease_url).await
    .context("Downloading InRelease")?;
    let inrelease_text  = String::from_utf8_lossy(&inrelease_bytes).to_string();

    let inrelease = if cfg.insecure {
        println!("  {} InRelease (unverified)", "!".yellow());
        InRelease::parse(&inrelease_text).ok()
    } else {
        let keyring_dir = Path::new(gpg_verify::KEYRING_DIR);
        match gpg_verify::verify_inrelease(&inrelease_text, keyring_dir) {
            Ok(()) => {
                println!("  {} InRelease (GPG verified)", "✔".bright_green());
                InRelease::parse(&inrelease_text).ok()
            }
            Err(e) => {
                bail!(
                    "InRelease GPG verification failed: {}\n  \
Add the repo's signing key with `hammer key add`, \
or pass --insecure to skip verification (NOT RECOMMENDED).",
                      e
                );
            }
        }
    };

    // Write InRelease to disk
    std::fs::write(local_dists.join("InRelease"), &inrelease_bytes)?;

    // ── Packages index per component, SHA256-checked against InRelease ──
    for comp in &cfg.components {
        let rel_path = format!("{}/binary-{}/Packages.xz", comp, cfg.arch);
        let pkg_url  = format!("{}/{}", dists_base, rel_path);
        let pkg_dir  = local_dists.join(comp).join(format!("binary-{}", cfg.arch));
        std::fs::create_dir_all(&pkg_dir)?;
        let dest = pkg_dir.join("Packages.xz");

        download_resumable(client, &pkg_url, &dest, 0).await
        .context(format!("Downloading {}", pkg_url))?;

        // Verify SHA256 against InRelease's checksum table
        let sha_status = match &inrelease {
            Some(ir) if !cfg.insecure => {
                let bytes = std::fs::read(&dest)?;
                match ir.verify_file(&rel_path, &bytes) {
                    Ok(())  => "sha256 ✔".bright_green().to_string(),
                    Err(e)  => {
                        // Remove the unverified file so it's not used downstream
                        let _ = std::fs::remove_file(&dest);
                        bail!("SHA256 mismatch for {}: {}", rel_path, e);
                    }
                }
            }
            _ => "unverified".yellow().to_string(),
        };

        println!("  {} {}/binary-{}/Packages.xz [{}]",
                 "✔".green(), comp, cfg.arch, sha_status);
    }

    Ok(inrelease)
}

fn load_packages_from_mirror(cfg: &MirrorConfig) -> Result<Vec<Package>> {
    use std::io::Read;
    let mut all = Vec::new();
    for comp in &cfg.components {
        let pkg_path = cfg.local_path
        .join("dists").join(&cfg.suite)
        .join(comp).join(format!("binary-{}", cfg.arch))
        .join("Packages.xz");
        if !pkg_path.exists() { continue; }
        let compressed = std::fs::read(&pkg_path)?;
        let mut dec = xz2::read::XzDecoder::new(compressed.as_slice());
        let mut text = String::new();
        dec.read_to_string(&mut text)?;
        let mut pkgs = Package::parse_index(&text);
        let local_uri = format!("file://{}", cfg.local_path.display());
        for p in pkgs.iter_mut() {
            p.repo_base_uri = Some(local_uri.clone());
        }
        all.extend(pkgs);
    }
    Ok(all)
}

// ─────────────────────────────────────────────────────────────
//  mirror_debs — verify each .deb against its Packages SHA256
// ─────────────────────────────────────────────────────────────

async fn mirror_debs(
    client: &HttpClient,
    cfg:    &MirrorConfig,
    pkgs:   &[Package],
) -> Result<()> {
    let total = pkgs.len();
    println!("  Mirroring {} .deb files…", total.to_string().cyan().bold());

    let mut done    = 0usize;
    let mut bytes   = 0u64;
    let mut sha_ok  = 0usize;
    let mut sha_bad = 0usize;

    for pkg in pkgs {
        let Some(ref filename) = pkg.filename else { continue };
        let url  = format!("{}/{}", cfg.source_url, filename);
        let dest = cfg.local_path.join(filename);
        if let Some(p) = dest.parent() { std::fs::create_dir_all(p)?; }

        let size = pkg.download_size.unwrap_or(0);
        match download_resumable(client, &url, &dest, cfg.bw_limit).await {
            Ok(()) => {
                // SHA256 verification against Packages index, if present
                if let Some(ref expected_sha) = pkg.sha256 {
                    match verify_file_sha256(&dest, expected_sha) {
                        Ok(true)  => sha_ok += 1,
                        Ok(false) => {
                            sha_bad += 1;
                            crate::log::warn(&format!(
                                "mirror: SHA256 mismatch for {} — removing", filename
                            ));
                            let _ = std::fs::remove_file(&dest);
                            continue;
                        }
                        Err(e) => crate::log::warn(&format!(
                            "mirror: could not verify {}: {}", filename, e
                        )),
                    }
                }

                done  += 1;
                bytes += size;
                if done % 100 == 0 || done == total {
                    println!("  [{}/{}] {} (sha256 ok: {}, bad: {})",
                             done, total,
                             crate::ui::human_size(bytes).cyan(),
                             sha_ok, sha_bad);
                }
            }
            Err(e) => {
                crate::log::warn(&format!("mirror: failed {}: {}", filename, e));
            }
        }
    }
    println!("  {} {} packages mirrored ({}) — {} verified, {} bad",
             "✔".bright_green(), done, crate::ui::human_size(bytes).cyan(),
             sha_ok.to_string().bright_green(), sha_bad.to_string().red());
    Ok(())
}

fn verify_file_sha256(path: &Path, expected_hex: &str) -> Result<bool> {
    use sha2::{Digest, Sha256};
    let data = std::fs::read(path)?;
    let actual = hex::encode(Sha256::digest(&data));
    Ok(actual.eq_ignore_ascii_case(expected_hex))
}

// ─────────────────────────────────────────────────────────────
//  Resumable download
// ─────────────────────────────────────────────────────────────

pub async fn download_resumable(
    client:   &HttpClient,
    url:      &str,
    dest:     &Path,
    bw_limit: u64,
) -> Result<()> {
    if dest.exists() {
        let meta = std::fs::metadata(dest)?;
        if meta.len() > 0 { return Ok(()); }
    }

    let part = dest.with_extension("part");
    let existing_size = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);

    let mut req = client.inner.get(url);
    if existing_size > 0 {
        req = req.header("Range", format!("bytes={}-", existing_size));
    }

    let resp = req.send().await.with_context(|| format!("GET {}", url))?;

    match resp.status() {
        StatusCode::NOT_FOUND => bail!("404 Not Found: {}", url),
        StatusCode::RANGE_NOT_SATISFIABLE => {
            if dest.exists() { return Ok(()); }
            bail!("Range not satisfiable for {}", url);
        }
        s if !s.is_success() => bail!("HTTP {} for {}", s, url),
        _ => {}
    }

    let is_partial = resp.status() == StatusCode::PARTIAL_CONTENT;

    let mut file = if is_partial && existing_size > 0 {
        tokio::fs::OpenOptions::new()
        .append(true)
        .open(&part).await
        .with_context(|| format!("Opening partial {:?}", part))?
    } else {
        let _ = tokio::fs::remove_file(&part).await;
        tokio::fs::File::create(&part).await
        .with_context(|| format!("Creating {:?}", part))?
    };

    let mut stream   = resp.bytes_stream();
    let mut interval = if bw_limit > 0 {
        Some(tokio::time::interval(std::time::Duration::from_millis(100)))
    } else { None };
    let mut window_bytes = 0u64;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("Stream error")?;
        file.write_all(&chunk).await?;

        if let Some(ref mut iv) = interval {
            window_bytes += chunk.len() as u64;
            let limit_per_100ms = bw_limit / 10;
            if window_bytes >= limit_per_100ms {
                iv.tick().await;
                window_bytes = 0;
            }
        }
    }

    file.flush().await?;
    drop(file);

    tokio::fs::rename(&part, dest).await
    .with_context(|| format!("Renaming {:?} → {:?}", part, dest))?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Flat repository support
// ─────────────────────────────────────────────────────────────

pub fn resolve_flat_repo_url(base: &str, filename: &str) -> String {
    let base = base.trim_end_matches('/');
    format!("{}/{}", base, filename.trim_start_matches('/'))
}

/// Fetch the Packages index for a flat repository (deb file:///path ./).
///
/// `arch` is currently unused because flat repos serve a single combined
/// Packages file regardless of architecture — kept in the signature for
/// API symmetry with the dists-based fetch and to allow future per-arch
/// flat layouts without changing the call sites.
pub async fn fetch_flat_packages(
    client:   &HttpClient,
    base_url: &str,
    _arch:    &str,
) -> Result<String> {
    for suffix in &["Packages.xz", "Packages.gz", "Packages"] {
        let url = format!("{}/{}", base_url.trim_end_matches('/'), suffix);
        match client.get_bytes(&url).await {
            Ok(bytes) => {
                let text = if suffix.ends_with(".xz") {
                    use std::io::Read;
                    let mut dec = xz2::read::XzDecoder::new(bytes.as_slice());
                    let mut s = String::new();
                    dec.read_to_string(&mut s)?;
                    s
                } else if suffix.ends_with(".gz") {
                    use std::io::Read;
                    let mut dec = flate2::read::GzDecoder::new(bytes.as_slice());
                    let mut s = String::new();
                    dec.read_to_string(&mut s)?;
                    s
                } else {
                    String::from_utf8_lossy(&bytes).to_string()
                };
                return Ok(text);
            }
            Err(_) => continue,
        }
    }
    bail!("No Packages index found at {}", base_url)
}
