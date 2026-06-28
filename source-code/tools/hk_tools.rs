use anyhow::{Context, Result};
use hk_parser::{load_hk_file, resolve_interpolations};
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
//  HkBinary
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct HkBinary {
    pub asset_name:   String,
    pub wrapper_name: String,
    /// Per-binary custom symlink directory override
    pub custom_dir:   Option<PathBuf>,
}

// ─────────────────────────────────────────────────────────────
//  HkToolSpec
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct HkToolSpec {
    pub name:         String,
    pub hk_path:      PathBuf,
    pub releases_url: String,
    pub github_repo:  String,
    pub binaries:     Vec<HkBinary>,
    pub hidden:       bool,
    pub is_gui:       bool,
    pub desktop_file: Option<String>,
    /// Default dirs for all binaries (unless overridden per-binary)
    pub default_dirs: Vec<PathBuf>,
    pub description:  String,
}

impl HkToolSpec {
    pub fn load(path: &Path) -> Result<Self> {
        let mut config = load_hk_file_full(path)
        .with_context(|| format!("Parsing {}", path.display()))?;
        resolve_interpolations(&mut config)
        .with_context(|| format!("Resolving interpolations in {}", path.display()))?;

        let name = path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

        // ── Helper closures that work directly on the config IndexMap ──
        //
        // FIX: We do NOT collect config.iter() into a new IndexMap
        // (that caused E0277 — can't collect &IndexMap).
        // Instead we search the config directly.
        //
        // The .hk file may be:
        //   (a) flat:     key => value pairs at top level
        //   (b) sectioned: [SectionName]  key => value
        //
        // We try flat first, then scan inside each section.

        let get_str = |key: &str| -> Option<String> {
            // Flat
            if let Some(v) = config.get(key) {
                if let Ok(s) = v.as_string() { return Some(s); }
            }
            // Sectioned
            for (_sec, sec_val) in config.iter() {
                if let Ok(inner) = sec_val.as_map() {
                    if let Some(v) = inner.get(key) {
                        if let Ok(s) = v.as_string() { return Some(s); }
                    }
                }
            }
            None
        };

        let get_bool = |key: &str| -> bool {
            if let Some(v) = config.get(key) {
                if let Ok(b) = v.as_bool() { return b; }
            }
            for (_sec, sec_val) in config.iter() {
                if let Ok(inner) = sec_val.as_map() {
                    if let Some(v) = inner.get(key) {
                        if let Ok(b) = v.as_bool() { return b; }
                    }
                }
            }
            false
        };

        let get_array = |key: &str| -> Vec<String> {
            let extract_from = |v: &hk_parser::HkValue| -> Vec<String> {
                if let Ok(arr) = v.as_array() {
                    arr.iter().filter_map(|e| e.as_string().ok()).collect()
                } else if let Ok(s) = v.as_string() {
                    vec![s]
                } else {
                    vec![]
                }
            };
            if let Some(v) = config.get(key) {
                let r = extract_from(v);
                if !r.is_empty() { return r; }
            }
            for (_sec, sec_val) in config.iter() {
                if let Ok(inner) = sec_val.as_map() {
                    if let Some(v) = inner.get(key) {
                        let r = extract_from(v);
                        if !r.is_empty() { return r; }
                    }
                }
            }
            vec![]
        };

        // ── Required fields ────────────────────────────────────
        let releases_url = get_str("releases")
        .or_else(|| get_str("releases_url"))
        .ok_or_else(|| anyhow::anyhow!("{}: missing 'releases' key", path.display()))?;

        let github_repo = extract_github_repo(&releases_url)
        .ok_or_else(|| anyhow::anyhow!(
            "{}: 'releases' must be a GitHub releases URL (got: {})",
                                       path.display(), releases_url
        ))?;

        // ── Optional fields ────────────────────────────────────
        let hidden       = get_bool("hidden");
        let is_gui       = get_bool("is_gui");
        let desktop_file = get_str("desktop_file");
        let description  = get_str("description")
        .or_else(|| get_str("desc")).unwrap_or_default();

        // Parse dirs — global and per-binary
        let dirs_raw = get_array("dirs");
        let default_dirs: Vec<PathBuf> = dirs_raw.iter()
        .filter(|s| !s.contains(':'))
        .map(|s| PathBuf::from(s))
        .collect();

        let dir_overrides: std::collections::HashMap<String, PathBuf> = dirs_raw.iter()
        .filter_map(|s| {
            if let Some((dir, bin)) = s.split_once(':') {
                Some((bin.trim().to_string(), PathBuf::from(dir.trim())))
            } else {
                None
            }
        })
        .collect();

        // Parse binary/binaries
        let mut bin_specs: Vec<String> = get_array("binaries");
        if bin_specs.is_empty() {
            if let Some(s) = get_str("binary") { bin_specs = vec![s]; }
        }

        let mut binaries: Vec<HkBinary> = bin_specs.iter().map(|spec| {
            let (asset, wrapper) = if let Some((a, w)) = spec.split_once(':') {
                (a.trim().to_string(), w.trim().to_string())
            } else {
                (spec.trim().to_string(), spec.trim().to_string())
            };
            let custom_dir = dir_overrides.get(&wrapper).cloned()
            .or_else(|| dir_overrides.get(&asset).cloned());
            HkBinary { asset_name: asset, wrapper_name: wrapper, custom_dir }
        }).collect();

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
        if let Some(ref d) = bin.custom_dir  { return d.clone(); }
        if let Some(d) = self.default_dirs.first() { return d.clone(); }
        if self.hidden { PathBuf::from(HK_HIDDEN_DIR) }
        else           { PathBuf::from(HK_PUBLIC_BIN) }
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
    .ok_or_else(|| anyhow::anyhow!(
        "Cannot find tag_name in GitHub API response for {}", github_repo
    ))
}

pub fn asset_url(github_repo: &str, tag: &str, asset_name: &str) -> String {
    format!("https://github.com/{}/releases/download/{}/{}", github_repo, tag, asset_name)
}

// ─────────────────────────────────────────────────────────────
//  Install one tool  (all-or-nothing atomic)
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

    // Phase 1: download everything to tmp files
    let mut tmp_map: Vec<(PathBuf, PathBuf, &HkBinary)> = Vec::new();

    for bin in &spec.binaries {
        let url  = asset_url(&spec.github_repo, &tag, &bin.asset_name);
        let dest = spec.store_dir().join(&bin.asset_name);
        println!("  {} Downloading {}…", "·".dimmed(), bin.asset_name);
        let bytes = client.get_bytes(&url).await
        .with_context(|| format!("Downloading {}", url))?;
        let tmp = dest.with_extension("tmp");
        std::fs::write(&tmp, &bytes)?;
        set_executable(&tmp)?;
        tmp_map.push((tmp, dest, bin));
    }

    // Download desktop file (GUI apps)
    let mut desktop_tmp: Option<(PathBuf, PathBuf)> = None;
    if spec.is_gui {
        if let Some(ref df_url) = spec.desktop_file {
            let raw_url = github_blob_to_raw(df_url);
            println!("  {} Downloading desktop file…", "·".dimmed());
            match client.get_bytes(&raw_url).await {
                Ok(bytes) => {
                    let fname = raw_url.split('/').last().unwrap_or("app.desktop").to_string();
                    let dest  = PathBuf::from(DESKTOP_DIR).join(&fname);
                    let tmp   = dest.with_extension("tmp");
                    std::fs::create_dir_all(DESKTOP_DIR)?;
                    std::fs::write(&tmp, &bytes)?;
                    desktop_tmp = Some((tmp, dest));
                }
                Err(e) => {
                    log::warn(&format!(
                        "hk-tools: desktop file download failed for {}: {}", spec.name, e
                    ));
                }
            }
        }
    }

    // Phase 2: atomic rename (all downloads succeeded)
    for (tmp, dest, bin) in &tmp_map {
        std::fs::rename(tmp, dest)?;
        let bin_dir = spec.bin_dir_for(bin);
        std::fs::create_dir_all(&bin_dir)?;
        create_wrapper(spec, &bin.wrapper_name, dest, &bin_dir)?;
    }

    if let Some((tmp, dest)) = desktop_tmp {
        std::fs::rename(&tmp, &dest)?;
        log::info(&format!("hk-tools: installed desktop file {}", dest.display()));
    }

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
//  Update ALL tools
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

fn create_wrapper(
    spec:        &HkToolSpec,
    wrapper_name: &str,
    binary_path: &Path,
    wrapper_dir: &Path,
) -> Result<()> {
    let wrapper_path = wrapper_dir.join(wrapper_name);
    let binary_str   = binary_path.to_string_lossy();
    let content = format!(
        "#!/bin/sh\n\
# HackerOS hammer tool wrapper — auto-generated, do not edit\n\
# Tool: {name}  Source: {releases}\n\
exec \"{binary}\" \"$@\"\n",
name     = spec.name,
releases = spec.releases_url,
binary   = binary_str,
    );
    let tmp = wrapper_path.with_extension("tmp");
    std::fs::write(&tmp, &content)?;
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
    let url   = url.trim_end_matches('/');
    let after = url.strip_prefix("https://github.com/")?;
    let parts: Vec<&str> = after.splitn(3, '/').collect();
    if parts.len() < 2 { return None; }
    Some(format!("{}/{}", parts[0], parts[1]))
}

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

// ─────────────────────────────────────────────────────────────
//  Multiline .hk preprocessor (0.6)
//
//  The `hk-parser` crate parses single-line values like:
//    components => ["main", "contrib"]
//
//  But real files often span multiple lines:
//    components => ["main",
//                   "contrib",
//                   "non-free"]
//
//  AND use line-continuation with trailing backslash:
//    description => This is a very \
//                   long description
//
//  This preprocessor normalises the file to single-line values
//  before handing it to `hk_parser::load_hk_file`.
//  It writes a temp file, returns its path.
// ─────────────────────────────────────────────────────────────

use std::io::Write;

/// Join multiline array literals and backslash-continued lines.
/// Returns the normalised source as a String.
pub fn preprocess_hk_source(source: &str) -> String {
    let mut out  = String::with_capacity(source.len());
    let mut buf  = String::new();
    let mut depth = 0i32;       // bracket nesting depth
    let mut in_continuation = false;

    for raw_line in source.lines() {
        let line = raw_line.trim_end();

        // Line continuation with trailing backslash
        if in_continuation {
            buf.push(' ');
            if line.ends_with('\\') {
                buf.push_str(line.trim_start().trim_end_matches('\\'));
                // stay in continuation
            } else {
                buf.push_str(line.trim_start());
                in_continuation = false;
                depth = 0;
                out.push_str(&buf);
                out.push('\n');
                buf.clear();
            }
            continue;
        }

        // Count bracket depth changes in this line
        for ch in line.chars() {
            match ch {
                '[' | '(' => depth += 1,
                ']' | ')' => { depth -= 1; if depth < 0 { depth = 0; } }
                _ => {}
            }
        }

        // Trailing backslash continuation (non-array)
        if depth == 0 && line.ends_with('\\') {
            buf.push_str(line.trim_end_matches('\\'));
            in_continuation = true;
            continue;
        }

        if depth > 0 {
            // We're inside an unclosed bracket — accumulate
            buf.push_str(line.trim_start());
            buf.push(' ');
        } else {
            // Bracket closed (depth <= 0) or never opened
            if !buf.is_empty() {
                buf.push_str(line.trim_start());
                out.push_str(&buf);
                out.push('\n');
                buf.clear();
                depth = 0;
            } else {
                out.push_str(line);
                out.push('\n');
            }
        }
    }

    // Flush any remaining buffer (file ended inside brackets)
    if !buf.is_empty() {
        out.push_str(&buf);
        out.push('\n');
    }

    out
}

/// Write preprocessed source to a temp file and return its path.
/// Caller is responsible for deleting the file.
pub fn write_preprocessed_hk(source: &str, suffix: &str) -> Result<std::path::PathBuf> {
    let normalised = preprocess_hk_source(source);
    let tmp_path = std::env::temp_dir().join(format!("hammer_hk_{}.hk", suffix));
    let mut f = std::fs::File::create(&tmp_path)?;
    f.write_all(normalised.as_bytes())?;
    Ok(tmp_path)
}

/// Drop-in for `hk_parser::load_hk_file` with multiline support.
pub fn load_hk_file_multiline(
    path: &Path
) -> Result<indexmap::IndexMap<String, hk_parser::HkValue>> {
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("Reading {}", path.display()))?;
    let normalised = preprocess_hk_source(&source);
    let tmp = write_preprocessed_hk(&normalised, &path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tmp".to_string()))?;
    let result = load_hk_file(tmp.to_str().unwrap_or(""))
        .with_context(|| format!("Parsing (preprocessed) {}", path.display()))?;
    let _ = std::fs::remove_file(&tmp);
    Ok(result)
}

#[cfg(test)]
mod multiline_tests {
    use super::*;

    #[test]
    fn test_single_line_unchanged() {
        let src = "components => [\"main\", \"contrib\"]\n";
        assert_eq!(preprocess_hk_source(src), src);
    }

    #[test]
    fn test_multiline_array() {
        let src = "components => [\"main\",\n               \"contrib\",\n               \"non-free\"]\n";
        let out = preprocess_hk_source(src);
        assert_eq!(out.lines().count(), 1);
        assert!(out.contains("non-free"));
    }

    #[test]
    fn test_backslash_continuation() {
        let src = "description => very long \\\n              value here\n";
        let out = preprocess_hk_source(src);
        assert_eq!(out.lines().count(), 1);
        assert!(out.contains("value here"));
    }

    #[test]
    fn test_comments_preserved() {
        let src = "! comment line\nname => hello\n";
        let out = preprocess_hk_source(src);
        assert!(out.contains("! comment line"));
        assert!(out.contains("name => hello"));
    }

    #[test]
    fn test_nested_brackets() {
        let src = "opts => {\"a\": [1,\n2,\n3]}\n";
        let out = preprocess_hk_source(src);
        assert_eq!(out.lines().filter(|l| !l.trim().is_empty()).count(), 1);
    }
}

// ─────────────────────────────────────────────────────────────
//  .hk schema validation (0.5)
//
//  Every .hk file for a tool spec is validated against the
//  HkToolSchema: required keys must be present and non-empty,
//  unknown keys produce a warning (never a hard error so that
//  future keys don't break older hammer versions).
// ─────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct SchemaError {
    pub file:    std::path::PathBuf,
    pub errors:  Vec<String>,
    pub warnings: Vec<String>,
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Schema errors in {}:\n{}", self.file.display(),
               self.errors.iter()
                   .map(|e| format!("  ERROR: {}", e))
                   .chain(self.warnings.iter().map(|w| format!("  WARN:  {}", w)))
                   .collect::<Vec<_>>().join("\n"))
    }
}

/// Known keys for a tool .hk file (used for unknown-key warnings).
const KNOWN_TOOL_KEYS: &[&str] = &[
    "name", "description", "github_repo", "releases_url",
    "binary", "binary_glob", "hidden", "is_gui", "desktop_file",
    "default_install_dir", "verify_sha256", "post_install",
    "pre_install", "depends_on",
];

/// Known keys for a sources-list .hk entry.
const KNOWN_SOURCES_KEYS: &[&str] = &[
    "name", "baseurl", "suite", "components", "arch", "enabled",
    "gpgkey", "type", "priority",
];

/// Required keys for a tool .hk file.
const REQUIRED_TOOL_KEYS: &[&str] = &["name", "github_repo"];

/// Required keys per source entry.
const REQUIRED_SOURCES_KEYS: &[&str] = &["name", "baseurl", "suite", "components"];

/// Validate a tool .hk file. Returns Ok if no hard errors, Err otherwise.
pub fn validate_tool_hk(path: &Path) -> Result<SchemaError> {
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("Reading {}", path.display()))?;
    let normalised = preprocess_hk_source(&source);
    let tmp = write_preprocessed_hk(&normalised, "validate")?;
    let config = load_hk_file(tmp.to_str().unwrap_or(""))
        .with_context(|| "Parsing .hk")?;
    let _ = std::fs::remove_file(&tmp);

    let mut errors   = Vec::new();
    let mut warnings = Vec::new();
    let keys: Vec<&str> = config.keys().map(String::as_str).collect();

    for req in REQUIRED_TOOL_KEYS {
        let missing = match config.get(*req) {
            None => true,
            Some(v) => match v {
                hk_parser::HkValue::String(s) => s.trim().is_empty(),
                _ => false,
            },
        };
        if missing {
            errors.push(format!("Missing required key: '{}'", req));
        }
    }

    for key in &keys {
        if !KNOWN_TOOL_KEYS.contains(key) {
            warnings.push(format!("Unknown key '{}' (will be ignored)", key));
        }
    }

    Ok(SchemaError { file: path.to_path_buf(), errors, warnings })
}

/// Validate a sources-list .hk file (may contain multiple sections).
pub fn validate_sources_hk(path: &Path) -> Result<SchemaError> {
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("Reading {}", path.display()))?;

    let mut errors   = Vec::new();
    let mut warnings = Vec::new();
    let mut current_section: Option<String> = None;
    let mut section_keys: std::collections::HashSet<String> = Default::default();

    let flush = |section: &str,
                 keys: &std::collections::HashSet<String>,
                 errors: &mut Vec<String>,
                 warnings: &mut Vec<String>| {
        for req in REQUIRED_SOURCES_KEYS {
            if !keys.contains(*req) {
                errors.push(format!("[{}] missing required key '{}'", section, req));
            }
        }
        for key in keys {
            if !KNOWN_SOURCES_KEYS.contains(&key.as_str()) {
                warnings.push(format!("[{}] unknown key '{}' (ignored)", section, key));
            }
        }
    };

    for raw in source.lines() {
        let line = raw.trim();
        if line.starts_with('!') || line.is_empty() { continue; }
        if line.starts_with('[') && line.ends_with(']') {
            if let Some(ref sec) = current_section.clone() {
                flush(sec, &section_keys, &mut errors, &mut warnings);
            }
            current_section = Some(line[1..line.len()-1].to_string());
            section_keys.clear();
            continue;
        }
        if line.contains("=>") {
            let key = line.split("=>").next().unwrap_or("").trim()
                .trim_start_matches("->").trim().to_string();
            section_keys.insert(key);
        }
    }
    if let Some(ref sec) = current_section {
        flush(sec, &section_keys, &mut errors, &mut warnings);
    }

    Ok(SchemaError { file: path.to_path_buf(), errors, warnings })
}

/// Hammer CLI: `hammer hk validate <file.hk>`
pub fn cmd_validate_hk(args: &[String]) -> anyhow::Result<()> {
    use owo_colors::OwoColorize;
    let path_str = args.first()
        .ok_or_else(|| anyhow::anyhow!("Usage: hammer hk validate <file.hk>"))?;
    let path = Path::new(path_str);

    // Detect file type by content (look for section headers → sources-list)
    let source = std::fs::read_to_string(path)?;
    let result = if source.lines().any(|l| l.trim().starts_with("->")) {
        validate_sources_hk(path)?
    } else {
        validate_tool_hk(path)?
    };

    if result.errors.is_empty() && result.warnings.is_empty() {
        println!("  {}  {} — valid", "✔".bright_green().bold(), path.display());
        return Ok(());
    }

    for w in &result.warnings {
        println!("  {}  {}", "⚠".yellow(), w.yellow());
    }
    for e in &result.errors {
        println!("  {}  {}", "✗".red().bold(), e.red());
    }
    if !result.errors.is_empty() {
        anyhow::bail!("Schema validation failed for {}", path.display());
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  !include support for .hk files (0.5)
//
//  Allows composable configs:
//    !include base.hk
//    !include ../common/gpg.hk
//
//  Includes are relative to the including file.
//  Circular include detection via a visited set.
// ─────────────────────────────────────────────────────────────

/// Resolve all `!include` directives recursively and return merged source.
pub fn resolve_includes(path: &Path) -> anyhow::Result<String> {
    let mut visited = std::collections::HashSet::new();
    resolve_includes_inner(path, &mut visited)
}

fn resolve_includes_inner(
    path:    &Path,
    visited: &mut std::collections::HashSet<std::path::PathBuf>,
) -> anyhow::Result<String> {
    let canonical = path.canonicalize()
        .with_context(|| format!("Resolving path {}", path.display()))?;

    if visited.contains(&canonical) {
        anyhow::bail!("Circular !include detected: {}", canonical.display());
    }
    visited.insert(canonical.clone());

    let dir    = canonical.parent().unwrap_or(Path::new("."));
    let source = std::fs::read_to_string(&canonical)
        .with_context(|| format!("Reading {}", canonical.display()))?;

    let mut out = String::with_capacity(source.len());
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("!include ") {
            let include_path = dir.join(rest.trim());
            let included = resolve_includes_inner(&include_path, visited)
                .with_context(|| format!("In !include from {}", canonical.display()))?;
            out.push_str(&included);
            out.push('\n');
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }

    visited.remove(&canonical);
    Ok(out)
}

/// Load a .hk file with both !include resolution AND multiline preprocessing.
pub fn load_hk_file_full(path: &Path) -> anyhow::Result<indexmap::IndexMap<String, hk_parser::HkValue>> {
    let source    = resolve_includes(path)?;
    let normalised = preprocess_hk_source(&source);
    let tmp = write_preprocessed_hk(&normalised, "full")?;
    let result = load_hk_file(tmp.to_str().unwrap_or(""))
        .with_context(|| format!("Parsing (full) {}", path.display()))?;
    let _ = std::fs::remove_file(&tmp);
    Ok(result)
}

#[cfg(test)]
mod include_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_no_includes() {
        let src = "name => hello\nversion => 1.0\n";
        let tmp = std::env::temp_dir().join("test_no_include.hk");
        let mut f = std::fs::File::create(&tmp).unwrap();
        f.write_all(src.as_bytes()).unwrap();
        let out = resolve_includes(&tmp).unwrap();
        assert!(out.contains("name => hello"));
        std::fs::remove_file(tmp).ok();
    }

    #[test]
    fn test_include_resolves() {
        let dir = std::env::temp_dir();
        let base = dir.join("base_inc.hk");
        let main = dir.join("main_inc.hk");
        std::fs::write(&base, "base_key => base_value\n").unwrap();
        std::fs::write(&main, "!include base_inc.hk\nname => main\n").unwrap();
        let out = resolve_includes(&main).unwrap();
        assert!(out.contains("base_key => base_value"));
        assert!(out.contains("name => main"));
        std::fs::remove_file(base).ok();
        std::fs::remove_file(main).ok();
    }

    #[test]
    fn test_validate_tool_missing_required() {
        let tmp = std::env::temp_dir().join("test_schema.hk");
        std::fs::write(&tmp, "description => Something\n").unwrap();
        let res = validate_tool_hk(&tmp).unwrap();
        assert!(res.errors.iter().any(|e| e.contains("name")));
        std::fs::remove_file(tmp).ok();
    }
}
