use anyhow::{bail, Context, Result};
use reqwest::Client;
use std::io::Read;

use crate::package::Package;

// ─────────────────────────────────────────────────────────────
//  RepoFetcher
// ─────────────────────────────────────────────────────────────

/// Fetches package indexes from a Debian-format repository.
pub struct RepoFetcher {
    base_url: String,
    client:   Client,
}

impl RepoFetcher {
    /// Create a fetcher for `base_url` (e.g. `https://deb.debian.org/debian`).
    pub fn new(base_url: impl Into<String>) -> Self {
        RepoFetcher {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client:   Client::builder()
                .user_agent(concat!("libhammer/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("HTTP client"),
        }
    }

    /// Fetch and parse the `Packages` index for `(suite, component, arch)`.
    ///
    /// Tries `.xz`, `.gz`, and uncompressed in that order.
    pub async fn fetch_packages(
        &self,
        suite:     &str,
        component: &str,
        arch:      &str,
    ) -> Result<Vec<Package>> {
        let base = format!(
            "{}/dists/{}/{}/binary-{}",
            self.base_url, suite, component, arch
        );

        // Try compressed variants in preference order
        for (suffix, decompress) in &[
            ("Packages.xz",  "xz"   as &str),
            ("Packages.gz",  "gz"         ),
            ("Packages",     "none"        ),
        ] {
            let url = format!("{}/{}", base, suffix);
            match self.fetch_bytes(&url).await {
                Ok(bytes) => {
                    let raw  = decompress_bytes(&bytes, decompress)?;
                    let text = String::from_utf8_lossy(&raw);
                    let mut pkgs = Package::parse_index(&text);
                    // Annotate with repo origin
                    for p in &mut pkgs {
                        p.repo_base_uri = Some(self.base_url.clone());
                    }
                    return Ok(pkgs);
                }
                Err(_) => continue,
            }
        }
        bail!("Could not fetch Packages index from {}", base);
    }

    /// Fetch the `Release` file and return it as a string.
    pub async fn fetch_release(&self, suite: &str) -> Result<String> {
        let url = format!("{}/dists/{}/Release", self.base_url, suite);
        let bytes = self.fetch_bytes(&url).await?;
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }

    /// Fetch raw bytes from `url`.
    pub async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self.client.get(url)
            .send().await
            .with_context(|| format!("GET {}", url))?;
        if !resp.status().is_success() {
            bail!("HTTP {} for {}", resp.status(), url);
        }
        let bytes = resp.bytes().await
            .with_context(|| format!("Reading body from {}", url))?;
        Ok(bytes.to_vec())
    }
}

// ─────────────────────────────────────────────────────────────
//  Decompression
// ─────────────────────────────────────────────────────────────

fn decompress_bytes(data: &[u8], ext: &str) -> Result<Vec<u8>> {
    use std::io::Cursor;
    match ext {
        "gz" => {
            let mut d   = flate2::read::GzDecoder::new(Cursor::new(data));
            let mut out = Vec::new();
            d.read_to_end(&mut out)?;
            Ok(out)
        }
        "xz" => {
            let mut d   = xz2::read::XzDecoder::new(Cursor::new(data));
            let mut out = Vec::new();
            d.read_to_end(&mut out)?;
            Ok(out)
        }
        _ => Ok(data.to_vec()),
    }
}
