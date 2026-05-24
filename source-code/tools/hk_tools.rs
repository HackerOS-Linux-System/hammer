use anyhow::{Context, Result};
use hk_parser::{load_hk_file, resolve_interpolations};
use indexmap::IndexMap;
use owo_colors::OwoColorize;
use std::path::{Path, PathBuf};

use crate::download::HttpClient;
use crate::log;

// ─────────────────────────────────────────────────────────────
//  Paths
// ─────────────────────────────────────────────────────────────

pub const HK_TOOLS_DIR:    &str = "/etc/hammer/HackerOS";
pub const HK_VERSIONS_DIR: &str = "/hammer/db/hk_tools";
pub const HK_STORE_DIR:    &str = "/hammer/hk_store";
pub const HK_HIDDEN_DIR:   &str = "/usr/lib/hammer";
pub const HK_PUBLIC_BIN:   &str = "/usr/bin";
pub const DESKTOP_DIR:     &str = "/usr/share/applications";

// ─────────────────────────────────────────────────────────────
//  HkBinary — one binary from a release
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct HkBinary {
    /// Filename of the asset in the GitHub release
    pub asset_name:   String,
    /// Name of the wrapper / symlink to create
    pub wrapper_name: String,
    /// Override symlink directory (None = use spec default)
    pub custom_dir:   Option<PathBuf>,
}

// ─────────────────────────────────────────────────────────────
//  HkToolSpec — parsed from one .hk file
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct HkToolSpec {
    pub name:          String,
    pub hk_path:       PathBuf,
    pub releases_url:  String,
    pub github_repo:   String,
    pub binaries:      Vec<HkBinary>,
    pub hidden:        bool,
    pub is_gui:        bool,
    /// URL to .desktop file (raw or blob GitHub URL)
    pub desktop_file:  Option<String>,
    /// Default dirs for all binaries (unless overridden per-binary)
    pub default_dirs:  Vec<PathBuf>,
    pub description:   String,
}

impl HkToolSpec {
    pub fn load(path: &Path) -> Result<Self> {
        let mut config = load_hk_file(path.to_str().unwrap_or(""))
            .with_context(|| format!("Parsing {}", path.display()))?;
        resolve_interpolations(&mut config)
            .with_context(|| format!("Resolving interpolations in {}", path.display()))?;

        let name = path.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();

        // FIX: hk_parser returns IndexMap<String, HkValue>.
        // config.iter() yields (&String, &HkValue) — we can use it
        // directly without collecting into a new map.
        // We look for either a sectioned format [section] or a flat format.

        // Try to find the first section that matches the tool name,
        // or just use the first section, or fall back to the root map.
        let map: &IndexMap<String, hk_parser::HkValue> = {
            // Check if the first value is a map (sectioned format)
            if let Some((_sec_name, sec_val)) = config.iter().next() {
                if sec_val.as_map().is_ok() {
                    // Sectioned: use the first section's map
                    // We work with config directly, get the inner map
                    // by finding a section that has "releases" key
                    &config
                } else {
                    &config
                }
            } else {
                &config
            }
        };

        // Helper: get a string value from the config, trying both
        // flat format and sectioned format.
        let get_str = |key: &str| -> Option<String> {
            // Try flat
            if let Some(v) = map.get(key) {
                if let Ok(s) = v.as_string() { return Some(s); }
            }
            // Try inside first section
            for (_sec, sec_val) in map.iter() {
                if let Ok(inner) = sec_val.as_map() {
                    if let Some(v) = inner.get(key) {
                        if let Ok(s) = v.as_string() { return Some(s); }
                    }
                }
            }
            None
        };

        let get_bool = |key: &str| -> bool {
            if let Some(v) = map.get(key) {
                if let Ok(b) = v.as_bool() { return b; }
            }
            for (_sec, sec_val) in map.iter() {
                if let Ok(inner) = sec_val.as_map() {
                    if let Some(v) = inner.get(key) {
                        if let Ok(b) = v.as_bool() { return b; }
                    }
                }
            }
            false
        };

        let get_array = |key: &str| -> Vec<String> {
            let try_val = |v: &hk_parser::HkValue| -> Vec<String> {
                if let Ok(arr) = v.as_array() {
                    arr.iter().filter_map(|e| e.as_string().ok()).collect()
                } else if let Ok(s) = v.as_string() {
                    vec![s]
                } else {
                    vec![]
                }
            };
            if let Some(v) = map.get(key) {
                let r = try_val(v);
                if !r.is_empty() { return r; }
            }
            for (_sec, sec_val) in map.iter() {
                if let Ok(inner) = sec_val.as_map() {
                    if let Some(v) = inner.get(key) {
                        let r = try_val(v);
                        if !r.is_empty() { return r; }
                    }
                }
            }
            vec![]
        };

        let releases_url = get_str("releases")
            .or_else(|| get_str("releases_url"))
            .ok_or_else(|| anyhow::anyhow!("{}: missing 'releases' key", path.display()))?;

        let github_repo = extract_github_repo(&releases_url)
            .ok_or_else(|| anyhow::anyhow!(
                "{}: 'releases' must be a GitHub releases URL (got: {})",
                path.display(), releases_url
            ))?;

        let hidden     = get_bool("hidden");
        let is_gui     = get_bool("is_gui");
        let desktop_file = get_str("desktop_file");
        let description  = get_str("description").or_else(|| get_str("desc")).unwrap_or_default();

        // Parse dirs
        let default_dirs: Vec<PathBuf> = get_array("dirs")
            .iter()
            .filter_map(|s| {
                // "dir:binary" format means per-binary — skip at this level
                if s.contains(':') { None } else { Some(PathBuf::from(s)) }
            })
            .collect();

        // Parse per-binary dir overrides from dirs array
        let dir_overrides: std::collections::HashMap<String, PathBuf> = get_array("dirs")
            .iter()
            .filter_map(|s| {
                if let Some((dir, bin)) = s.split_once(':') {
                    Some((bin.trim().to_string(), PathBuf::from(dir.trim())))
                } else {
                    None
                }
            })
            .collect();

        // Parse binaries
        let mut binaries: Vec<HkBinary> = Vec::new();

        let bin_specs: Vec<String> = {
            let mut v = get_array("binaries");
            if v.is_empty() {
                if let Some(s) = get_str("binary") { v = vec![s]; }
            }
            v
        };

        for spec in &bin_specs {
            let (asset, wrapper) = if let Some((a, w)) = spec.split_once(':') {
                (a.trim().to_string(), w.trim().to_string())
            } else {
                (spec.trim().to_string(), spec.trim().to_string())
            };
            let custom_dir = dir_overrides.get(&wrapper).cloned()
                .or_else(|| dir_overrides.get(&asset).cloned());
            binaries.push(HkBinary { asset_name: asset, wrapper_name: wrapper, custom_dir });
        }

        if binaries.is_empty() {
            binaries.push(HkBinary {
                asset_name:   name.clone(),
                wrapper_name: name.clone(),
                custom_dir:   None,
            });
        }

        Ok(HkToolSpec {
            name, hk_path: path.to_path_buf(),
            releases_url, github_repo, binaries,
            hidden, is_gui, desktop_file, default_dirs, description,
        })
    }

    pub fn version_file(&self) -> PathBuf {
        PathBuf::from(HK_VERSIONS_DIR).join(format!("{}.version", self.name))
    }

    pub fn installed_version(&self) -> Option<String> {
        std::fs::read_to_string(self.version_file()).ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn store_dir(&self) -> PathBuf {
        PathBuf::from(HK_STORE_DIR).join(&self.name)
    }

    /// Effective symlink directory for a given binary
    pub fn bin_dir_for(&self, bin: &HkBinary) -> PathBuf {
        if let Some(ref d) = bin.custom_dir { return d.clone(); }
        if let Some(d) = self.default_dirs.first() { return d.clone(); }
        if self.hidden {
            PathBuf::from(HK_HIDDEN_DIR)
        } else {
            PathBuf::from(HK_PUBLIC_BIN)
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  Load all specs
// ─────────────────────────────────────────────────────────────

pub fn load_all_specs() -> Vec<HkToolSpec> {
    let dir = Path::new(HK_TOOLS_DIR);
    if !dir.exists() { return vec![]; }
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![]; };
    let mut paths: Vec<_> = entries.flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |e| e == "hk"))
        .collect();
    paths.sort();
    paths.iter().filter_map(|p| {
        HkToolSpec::load(p).map_err(|e| {
            log::warn(&format!("hk-tools: cannot parse {}: {}", p.display(), e));
        }).ok()
    }).collect()
}

// ─────────────────────────────────────────────────────────────
//  GitHub release API
// ─────────────────────────────────────────────────────────────

pub async fn fetch_latest_release(client: &HttpClient, github_repo: &str) -> Result<String> {
    let api_url = format!("https://api.github.com/repos/{}/releases/latest", github_repo);
    let body = client.inner
        .get(&api_url)
        .header("User-Agent", concat!("hammer/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .send().await
        .with_context(|| format!("GET {}", api_url))?
        .text().await?;
    extract_json_string(&body, "tag_name")
        .ok_or_else(|| anyhow::anyhow!("Cannot find tag_name in GitHub API response for {}", github_repo))
}

pub fn asset_url(github_repo: &str, tag: &str, asset_name: &str) -> String {
    format!("https://github.com/{}/releases/download/{}/{}", github_repo, tag, asset_name)
}

// ─────────────────────────────────────────────────────────────
//  Install one tool  (atomically)
// ─────────────────────────────────────────────────────────────

pub async fn install_tool(spec: &HkToolSpec, client: &HttpClient) -> Result<()> {
    std::fs::create_dir_all(HK_VERSIONS_DIR)?;
    std::fs::create_dir_all(&spec.store_dir())?;
    std::fs::create_dir_all(HK_HIDDEN_DIR)?;
    std::fs::create_dir_all(HK_PUBLIC_BIN)?;

    let tag = fetch_latest_release(client, &spec.github_repo).await
        .with_context(|| format!("Fetching latest release for {}", spec.name))?;

    println!("  {}  Installing {} {}…",
             "⬡".bright_cyan().bold(), spec.name.bold(), tag.dimmed());

    // Download all binaries to tmp files FIRST (atomic: all-or-nothing)
    let mut tmp_files: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new(); // (tmp, dest, wrapper)

    for bin in &spec.binaries {
        let url  = asset_url(&spec.github_repo, &tag, &bin.asset_name);
        let dest = spec.store_dir().join(&bin.asset_name);

        println!("  {} Downloading {}…", "·".dimmed(), bin.asset_name);
        let bytes = client.get_bytes(&url).await
            .with_context(|| format!("Downloading {}", url))?;

        let tmp = dest.with_extension("tmp");
        std::fs::write(&tmp, &bytes)?;
        set_executable(&tmp)?;
        tmp_files.push((tmp, dest, spec.store_dir().join(&bin.wrapper_name)));
    }

    // Download desktop file if GUI
    let mut desktop_tmp: Option<(PathBuf, PathBuf)> = None;
    if spec.is_gui {
        if let Some(ref df_url) = spec.desktop_file {
            let raw_url = github_blob_to_raw(df_url);
            println!("  {} Downloading desktop file…", "·".dimmed());
            match client.get_bytes(&raw_url).await {
                Ok(bytes) => {
                    let filename = raw_url.split('/').last().unwrap_or("app.desktop").to_string();
                    let dest = PathBuf::from(DESKTOP_DIR).join(&filename);
                    let tmp  = dest.with_extension("tmp");
                    std::fs::create_dir_all(DESKTOP_DIR)?;
                    std::fs::write(&tmp, &bytes)?;
                    desktop_tmp = Some((tmp, dest));
                }
                Err(e) => {
                    log::warn(&format!("hk-tools: desktop file download failed for {}: {}", spec.name, e));
                }
            }
        }
    }

    // ── Atomic commit phase ───────────────────────────────────
    // At this point all downloads succeeded. Start replacing.

    for (i, bin) in spec.binaries.iter().enumerate() {
        let (ref tmp, ref dest, _) = tmp_files[i];
        std::fs::rename(tmp, dest)?;
        // Create wrapper / symlink
        let bin_dir = spec.bin_dir_for(bin);
        std::fs::create_dir_all(&bin_dir)?;
        create_wrapper(spec, &bin.wrapper_name, dest, &bin_dir)?;
    }

    if let Some((tmp, dest)) = desktop_tmp {
        std::fs::rename(&tmp, &dest)?;
        log::info(&format!("hk-tools: installed desktop file {}", dest.display()));
    }

    // Record version
    std::fs::write(spec.version_file(), format!("{}\n", tag))?;

    println!("  {} {} {} installed.", "✔".bright_green(), spec.name.bold(), tag.cyan());
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Update one tool
// ─────────────────────────────────────────────────────────────

pub async fn update_tool(spec: &HkToolSpec, client: &HttpClient) -> Result<bool> {
    let latest  = fetch_latest_release(client, &spec.github_repo).await?;
    let current = spec.installed_version().unwrap_or_default();
    if normalize_tag(&latest) == normalize_tag(&current) { return Ok(false); }

    println!("  {} Updating {} {} → {}",
             "↑".yellow().bold(), spec.name.bold(), current.dimmed(), latest.cyan());
    install_tool(spec, client).await?;
    Ok(true)
}

// ─────────────────────────────────────────────────────────────
//  Update ALL .hk tools
// ─────────────────────────────────────────────────────────────

pub async fn update_all_tools(client: &HttpClient) -> Result<()> {
    let specs = load_all_specs();
    if specs.is_empty() { return Ok(()); }

    println!("  {}  Checking {} HackerOS tool{}…",
             "⬡".cyan().bold(), specs.len(), if specs.len() == 1 { "" } else { "s" });

    let mut updated = 0usize;
    let mut failed  = 0usize;

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
        println!("  {} {} tool{} updated.", "✔".bright_green(),
                 updated, if updated == 1 { "" } else { "s" });
    }
    if failed > 0 {
        println!("  {} {} tool update{} failed — check logs.", "!".yellow().bold(),
                 failed, if failed == 1 { "" } else { "s" });
    }
    Ok(())
}

pub fn list_tools() -> Vec<(String, String, String)> {
    load_all_specs().into_iter().map(|spec| {
        let ver  = spec.installed_version().unwrap_or_else(|| "not installed".to_string());
        let desc = spec.description.clone();
        (spec.name, ver, desc)
    }).collect()
}

// ─────────────────────────────────────────────────────────────
//  Wrapper creation
// ─────────────────────────────────────────────────────────────

fn create_wrapper(spec: &HkToolSpec, wrapper_name: &str, binary_path: &Path, wrapper_dir: &Path) -> Result<()> {
    let wrapper_path = wrapper_dir.join(wrapper_name);
    let binary_str   = binary_path.to_string_lossy();

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
    set_executable(&tmp)?;
    std::fs::rename(&tmp, &wrapper_path)?;
    log::info(&format!("hk-tools: wrapper {} → {}", wrapper_path.display(), binary_str));
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Helpers
// ─────────────────────────────────────────────────────────────

fn set_executable(path: &Path) -> Result<()> {
    let mut perms = std::fs::metadata(path)?.permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

fn extract_github_repo(url: &str) -> Option<String> {
    let url = url.trim_end_matches('/');
    let after = url.strip_prefix("https://github.com/")?;
    let parts: Vec<&str> = after.splitn(3, '/').collect();
    if parts.len() < 2 { return None; }
    Some(format!("{}/{}", parts[0], parts[1]))
}

/// Convert github.com blob URL to raw.githubusercontent.com URL
fn github_blob_to_raw(url: &str) -> String {
    if url.contains("raw.githubusercontent.com") { return url.to_string(); }
    url.replace("https://github.com/", "https://raw.githubusercontent.com/")
       .replace("/blob/", "/")
}

fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\"", key);
    let pos    = json.find(&needle)?;
    let after  = json[pos + needle.len()..].trim_start();
    let after  = after.strip_prefix(':')?.trim_start();
    if after.starts_with('"') {
        let end = after[1..].find('"')?;
        Some(after[1..1+end].to_string())
    } else {
        None
    }
}

fn normalize_tag(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}
