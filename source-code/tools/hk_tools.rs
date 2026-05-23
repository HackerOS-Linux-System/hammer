use anyhow::{bail, Context, Result};
use hk_parser::{load_hk_file, resolve_interpolations};
use owo_colors::OwoColorize;
use std::path::{Path, PathBuf};

use crate::download::HttpClient;
use crate::log;

// ─────────────────────────────────────────────────────────────
//  Paths
// ─────────────────────────────────────────────────────────────

pub const HK_TOOLS_DIR:     &str = "/etc/hammer/HackerOS";
pub const HK_VERSIONS_DIR:  &str = "/hammer/db/hk_tools";
pub const HK_STORE_DIR:     &str = "/hammer/hk_store";
pub const HK_HIDDEN_DIR:    &str = "/usr/lib/hammer";
pub const HK_PUBLIC_BIN:    &str = "/usr/local/bin";

// ─────────────────────────────────────────────────────────────
//  HkToolSpec — parsed from one .hk file
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct HkBinary {
    /// Filename of the binary inside the GitHub release assets
    pub asset_name: String,
    /// Name to give the wrapper (defaults to asset_name)
    pub wrapper_name: String,
}

#[derive(Debug, Clone)]
pub struct HkToolSpec {
    /// Tool name (derived from filename without .hk)
    pub name: String,
    /// Full path of the .hk file
    pub hk_path: PathBuf,
    /// GitHub releases URL — e.g. "https://github.com/Owner/Repo/releases"
    pub releases_url: String,
    /// Owner/Repo extracted from releases_url
    pub github_repo: String,
    /// Binaries to download from each release
    pub binaries: Vec<HkBinary>,
    /// If true, wrappers go to /usr/lib/hammer/ (not /usr/local/bin)
    pub hidden: bool,
    /// Optional description
    pub description: String,
}

impl HkToolSpec {
    /// Parse a single .hk file
    pub fn load(path: &Path) -> Result<Self> {
        let mut config = load_hk_file(path.to_str().unwrap_or(""))
            .with_context(|| format!("Parsing {}", path.display()))?;
        resolve_interpolations(&mut config)
            .with_context(|| format!("Resolving interpolations in {}", path.display()))?;

        let name = path.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();

        // Each .hk file is a flat section — no section header needed,
        // OR it can have a section [tool-name] wrapping all keys.
        // We support both flat and sectioned formats.

        let map = if let Some((_sec, val)) = config.iter().next() {
            // Try sectioned format first
            if let Ok(m) = val.as_map() {
                m
            } else {
                // Fall back to flat
                config.iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            }
        } else {
            config.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };

        let releases_url = map.get("releases")
            .or_else(|| map.get("releases_url"))
            .and_then(|v| v.as_string().ok())
            .ok_or_else(|| anyhow::anyhow!("{}: missing 'releases' key", path.display()))?;

        // Extract owner/repo from GitHub URL
        // "https://github.com/Owner/Repo/releases" → "Owner/Repo"
        let github_repo = extract_github_repo(&releases_url)
            .ok_or_else(|| anyhow::anyhow!(
                "{}: 'releases' must be a GitHub releases URL (got: {})",
                path.display(), releases_url
            ))?;

        // binaries — can be:
        //   binary => "tool-name"                     (single)
        //   binaries => ["tool-name", "other-name"]   (multiple)
        let mut binaries = Vec::new();

        if let Some(v) = map.get("binary").or_else(|| map.get("binaries")) {
            if let Ok(arr) = v.as_array() {
                for item in arr {
                    if let Ok(s) = item.as_string() {
                        binaries.push(parse_binary_spec(&s));
                    }
                }
            } else if let Ok(s) = v.as_string() {
                binaries.push(parse_binary_spec(&s));
            }
        }

        if binaries.is_empty() {
            // Default: use tool name as binary name
            binaries.push(HkBinary {
                asset_name:   name.clone(),
                wrapper_name: name.clone(),
            });
        }

        let hidden = map.get("hidden")
            .and_then(|v| v.as_bool().ok())
            .unwrap_or(false);

        let description = map.get("description")
            .or_else(|| map.get("desc"))
            .and_then(|v| v.as_string().ok())
            .unwrap_or_default();

        Ok(HkToolSpec {
            name,
            hk_path: path.to_path_buf(),
            releases_url,
            github_repo,
            binaries,
            hidden,
            description,
        })
    }

    pub fn version_file(&self) -> PathBuf {
        PathBuf::from(HK_VERSIONS_DIR).join(format!("{}.version", self.name))
    }

    pub fn installed_version(&self) -> Option<String> {
        std::fs::read_to_string(self.version_file())
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn store_dir(&self) -> PathBuf {
        PathBuf::from(HK_STORE_DIR).join(&self.name)
    }
}

// ─────────────────────────────────────────────────────────────
//  Load all .hk specs from /etc/hammer/HackerOS/
// ─────────────────────────────────────────────────────────────

pub fn load_all_specs() -> Vec<HkToolSpec> {
    let dir = Path::new(HK_TOOLS_DIR);
    if !dir.exists() { return vec![]; }

    let mut specs = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![]; };

    let mut paths: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |e| e == "hk"))
        .collect();
    paths.sort();

    for path in paths {
        match HkToolSpec::load(&path) {
            Ok(spec) => specs.push(spec),
            Err(e)   => log::warn(&format!("hk-tools: cannot parse {}: {}", path.display(), e)),
        }
    }
    specs
}

// ─────────────────────────────────────────────────────────────
//  GitHub release API
// ─────────────────────────────────────────────────────────────

/// Fetch the latest release tag from GitHub API.
/// Returns e.g. "v1.2.3" or "1.2.3"
pub async fn fetch_latest_release(client: &HttpClient, github_repo: &str) -> Result<String> {
    let api_url = format!(
        "https://api.github.com/repos/{}/releases/latest",
        github_repo
    );
    let body = client.inner
        .get(&api_url)
        .header("User-Agent", concat!("hammer/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .send().await
        .with_context(|| format!("GET {}", api_url))?
        .text().await?;

    // Parse "tag_name" from the JSON without a JSON library dependency
    extract_json_string(&body, "tag_name")
        .ok_or_else(|| anyhow::anyhow!("Cannot find tag_name in GitHub API response"))
}

/// Build a download URL for a release asset.
pub fn asset_url(github_repo: &str, tag: &str, asset_name: &str) -> String {
    format!(
        "https://github.com/{}/releases/download/{}/{}",
        github_repo, tag, asset_name
    )
}

// ─────────────────────────────────────────────────────────────
//  Install one tool
// ─────────────────────────────────────────────────────────────

pub async fn install_tool(spec: &HkToolSpec, client: &HttpClient) -> Result<()> {
    std::fs::create_dir_all(HK_VERSIONS_DIR)?;
    std::fs::create_dir_all(&spec.store_dir())?;
    std::fs::create_dir_all(HK_HIDDEN_DIR)?;
    std::fs::create_dir_all(HK_PUBLIC_BIN)?;

    let tag = fetch_latest_release(client, &spec.github_repo).await
        .with_context(|| format!("Fetching latest release for {}", spec.name))?;

    println!(
        "  {}  Installing {} {}…",
        "⬡".bright_cyan().bold(),
        spec.name.bold(),
        tag.dimmed()
    );

    for bin in &spec.binaries {
        let url  = asset_url(&spec.github_repo, &tag, &bin.asset_name);
        let dest = spec.store_dir().join(&bin.asset_name);

        println!("  {} Downloading {}…", "·".dimmed(), bin.asset_name);
        let bytes = client.get_bytes(&url).await
            .with_context(|| format!("Downloading {}", url))?;

        // Write atomically
        let tmp = dest.with_extension("tmp");
        std::fs::write(&tmp, &bytes)?;
        // Make executable
        let mut perms = std::fs::metadata(&tmp)?.permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&tmp, perms)?;
        std::fs::rename(&tmp, &dest)?;

        // Create wrapper
        create_wrapper(spec, &bin.wrapper_name, &dest)?;
    }

    // Record installed version
    std::fs::write(spec.version_file(), format!("{}\n", tag))?;

    println!(
        "  {} {} {} installed.",
        "✔".bright_green(),
        spec.name.bold(),
        tag.cyan()
    );
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Update one tool
// ─────────────────────────────────────────────────────────────

pub async fn update_tool(spec: &HkToolSpec, client: &HttpClient) -> Result<bool> {
    let latest = fetch_latest_release(client, &spec.github_repo).await?;
    let current = spec.installed_version().unwrap_or_default();

    if normalize_tag(&latest) == normalize_tag(&current) {
        return Ok(false); // already up to date
    }

    println!(
        "  {} Updating {} {} → {}",
        "↑".yellow().bold(),
        spec.name.bold(),
        current.dimmed(),
        latest.cyan()
    );

    install_tool(spec, client).await?;
    Ok(true)
}

// ─────────────────────────────────────────────────────────────
//  Update ALL .hk tools  (called from `hammer upgrade`)
// ─────────────────────────────────────────────────────────────

pub async fn update_all_tools(client: &HttpClient) -> Result<()> {
    let specs = load_all_specs();
    if specs.is_empty() { return Ok(()); }

    println!(
        "  {}  Checking {} HackerOS tool{}…",
        "⬡".cyan().bold(),
        specs.len(),
        if specs.len() == 1 { "" } else { "s" }
    );

    let mut updated  = 0usize;
    let mut failed   = 0usize;

    for spec in &specs {
        match update_tool(spec, client).await {
            Ok(true)  => updated += 1,
            Ok(false) => {}
            Err(e)    => {
                log::warn(&format!("hk-tools: update {} failed: {}", spec.name, e));
                failed += 1;
            }
        }
    }

    if updated > 0 {
        println!(
            "  {} {} tool{} updated.",
            "✔".bright_green(),
            updated,
            if updated == 1 { "" } else { "s" }
        );
    }
    if failed > 0 {
        println!(
            "  {} {} tool update{} failed — check logs.",
            "!".yellow().bold(),
            failed,
            if failed == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Wrapper creation
//
//  Public tool:  /usr/local/bin/<wrapper_name> → shell wrapper
//  Hidden tool:  /usr/lib/hammer/<wrapper_name> → shell wrapper
// ─────────────────────────────────────────────────────────────

fn create_wrapper(spec: &HkToolSpec, wrapper_name: &str, binary_path: &Path) -> Result<()> {
    let wrapper_dir = if spec.hidden {
        PathBuf::from(HK_HIDDEN_DIR)
    } else {
        PathBuf::from(HK_PUBLIC_BIN)
    };
    std::fs::create_dir_all(&wrapper_dir)?;

    let wrapper_path = wrapper_dir.join(wrapper_name);
    let binary_str   = binary_path.to_string_lossy();

    // Shell wrapper — thin passthrough that allows the binary path to be
    // updated atomically (we only update the binary, not the wrapper).
    let wrapper_content = format!(
        "#!/bin/sh\n\
         # HackerOS hammer tool wrapper — auto-generated, do not edit\n\
         # Tool: {name}  Source: {releases}\n\
         exec \"{binary}\" \"$@\"\n",
        name     = spec.name,
        releases = spec.releases_url,
        binary   = binary_str,
    );

    let tmp = wrapper_path.with_extension("tmp");
    std::fs::write(&tmp, &wrapper_content)?;
    let mut perms = std::fs::metadata(&tmp)?.permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&tmp, perms)?;
    std::fs::rename(&tmp, &wrapper_path)?;

    log::info(&format!(
        "hk-tools: wrapper {} → {}",
        wrapper_path.display(),
        binary_str
    ));
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  List installed tools (for `hammer status` / `hammer list`)
// ─────────────────────────────────────────────────────────────

pub fn list_tools() -> Vec<(String, String, String)> {
    // Returns (name, installed_version, description)
    load_all_specs()
        .into_iter()
        .map(|spec| {
            let ver  = spec.installed_version().unwrap_or_else(|| "not installed".to_string());
            let desc = spec.description.clone();
            (spec.name, ver, desc)
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────
//  Helpers
// ─────────────────────────────────────────────────────────────

fn extract_github_repo(url: &str) -> Option<String> {
    // "https://github.com/Owner/Repo/releases" → "Owner/Repo"
    // "https://github.com/Owner/Repo"           → "Owner/Repo"
    let url = url.trim_end_matches('/');
    let after_github = url.strip_prefix("https://github.com/")?;
    let parts: Vec<&str> = after_github.splitn(3, '/').collect();
    if parts.len() < 2 { return None; }
    Some(format!("{}/{}", parts[0], parts[1]))
}

fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\"", key);
    let pos = json.find(&needle)?;
    let after = &json[pos + needle.len()..];
    let colon = after.find(':')?;
    let after_colon = after[colon+1..].trim_start();
    if after_colon.starts_with('"') {
        let start = 1;
        let end   = after_colon[start..].find('"')?;
        Some(after_colon[start..start+end].to_string())
    } else {
        None
    }
}

fn parse_binary_spec(s: &str) -> HkBinary {
    // Format: "asset-name:wrapper-name" or just "asset-name"
    if let Some((asset, wrapper)) = s.split_once(':') {
        HkBinary {
            asset_name:   asset.trim().to_string(),
            wrapper_name: wrapper.trim().to_string(),
        }
    } else {
        HkBinary {
            asset_name:   s.trim().to_string(),
            wrapper_name: s.trim().to_string(),
        }
    }
}

/// Strip leading 'v' for comparison: "v1.2.3" == "1.2.3"
fn normalize_tag(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}
