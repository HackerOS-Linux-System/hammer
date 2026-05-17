use anyhow::{bail, Context, Result};
use hk_parser::{load_hk_file, resolve_interpolations, HkValue};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ─────────────────────────────────────────────────────────────
//  Paths — sources list uses .hk format
// ─────────────────────────────────────────────────────────────

pub const SOURCES_HK: &str = "/etc/hammer/sources-list.hk";

#[derive(Debug, Clone, PartialEq)]
pub enum EntryKind {
    Deb,
    DebSrc,
}

#[derive(Debug, Clone)]
pub struct SourceEntry {
    pub kind:       EntryKind,
    pub uri:        String,
    pub suite:      String,
    pub components: Vec<String>,
    pub arches:     Vec<String>,
    pub signed_by:  Option<String>,
    pub enabled:    bool,
    pub name:       Option<String>,
}

pub struct SourcesList {
    pub entries: Vec<SourceEntry>,
}

/// All known Debian suites for validation / completion
pub const KNOWN_SUITES: &[&str] = &[
    // Stable
    "bookworm",
"bookworm-updates",
"bookworm-security",
"bookworm-backports",
// Testing
"trixie",
"trixie-updates",
"trixie-security",
"trixie-backports",
// Unstable / experimental
"sid",
"unstable",
"forky",
"experimental",
// Release aliases
"stable",
"stable-updates",
"stable-security",
"stable-backports",
"testing",
"testing-updates",
"testing-security",
"oldstable",
"oldstable-security",
];

// ─────────────────────────────────────────────────────────────
//  SourcesList implementation
// ─────────────────────────────────────────────────────────────

impl SourcesList {
    /// Load sources from /etc/hammer/sources-list.hk (primary format).
    /// Falls back to legacy TOML if .hk not found.
    pub fn load() -> Result<Self> {
        let hk_path = Path::new(SOURCES_HK);
        if hk_path.exists() {
            return Self::load_hk(hk_path);
        }

        // Legacy TOML fallback
        let toml_path = Path::new("/etc/hammer/repos.toml");
        if toml_path.exists() {
            eprintln!(
                "  \x1b[33mwarn:\x1b[0m Using legacy repos.toml — \
migrate to /etc/hammer/sources-list.hk"
            );
            return Self::load_legacy_toml(toml_path);
        }

        // Last resort: /etc/apt/sources.list
        let mut entries = Vec::new();
        let main = Path::new("/etc/apt/sources.list");
        if main.exists() {
            let txt = std::fs::read_to_string(main)?;
            entries.extend(parse_apt_sources_list(&txt));
        }
        Ok(SourcesList { entries })
    }

    // ── .hk parser ────────────────────────────────────────────

    fn load_hk(path: &Path) -> Result<Self> {
        let mut config =
        load_hk_file(path.to_str().unwrap_or(SOURCES_HK))
        .with_context(|| format!("Parsing {}", path.display()))?;

        resolve_interpolations(&mut config)
        .with_context(|| format!("Resolving interpolations in {}", path.display()))?;

        let mut entries = Vec::new();

        // Each section in the .hk file is one repository block.
        // Expected structure:
        //
        //   [debian-bookworm]
        //   -> name      => debian-bookworm
        //   -> baseurl   => http://deb.debian.org/debian
        //   -> suite     => bookworm
        //   -> components => ["main", "contrib", "non-free"]
        //   -> arch      => ["amd64"]
        //   -> enabled   => true
        //   -> gpgkey    => https://...   (optional)
        //   -> type      => deb           (optional, default deb)

        for (section_name, section_val) in &config {
            // skip meta/comment sections
            if section_name.starts_with('_') || section_name == "meta" {
                continue;
            }

            let map = match section_val.as_map() {
                Ok(m) => m,
                Err(_) => continue,
            };

            let enabled = map
            .get("enabled")
            .and_then(|v| v.as_bool().ok())
            .unwrap_or(true);

            if !enabled {
                continue;
            }

            let kind = match map
            .get("type")
            .and_then(|v| v.as_string().ok())
            .as_deref()
            {
                Some("deb-src") => EntryKind::DebSrc,
                _ => EntryKind::Deb,
            };

            let uri = match map.get("baseurl").and_then(|v| v.as_string().ok()) {
                Some(u) => u,
                None => {
                    eprintln!(
                        "  \x1b[33mwarn:\x1b[0m Section [{}] missing 'baseurl', skipped.",
                        section_name
                    );
                    continue;
                }
            };

            let suite = match map.get("suite").and_then(|v| v.as_string().ok()) {
                Some(s) => s,
                None => {
                    eprintln!(
                        "  \x1b[33mwarn:\x1b[0m Section [{}] missing 'suite', skipped.",
                        section_name
                    );
                    continue;
                }
            };

            let components: Vec<String> = map
            .get("components")
            .and_then(|v| v.as_array().ok())
            .map(|arr| arr.iter().filter_map(|e| e.as_string().ok()).collect())
            .unwrap_or_else(|| vec!["main".to_string()]);

            let arches: Vec<String> = map
            .get("arch")
            .and_then(|v| v.as_array().ok())
            .map(|arr| arr.iter().filter_map(|e| e.as_string().ok()).collect())
            .unwrap_or_default();

            let signed_by = map.get("gpgkey").and_then(|v| v.as_string().ok());

            let name = map
            .get("name")
            .and_then(|v| v.as_string().ok())
            .or_else(|| Some(section_name.clone()));

            entries.push(SourceEntry {
                kind,
                uri,
                suite,
                components,
                arches,
                signed_by,
                enabled: true,
                name,
            });
        }

        Ok(SourcesList { entries })
    }

    // ── legacy TOML fallback ──────────────────────────────────

    fn load_legacy_toml(path: &Path) -> Result<Self> {
        #[derive(Deserialize)]
        struct ReposFile {
            repo: Vec<HammerRepo>,
        }
        #[derive(Deserialize)]
        struct HammerRepo {
            name:       String,
            baseurl:    String,
            suite:      String,
            components: Vec<String>,
            #[serde(default)]
            arch:       Vec<String>,
            #[serde(default = "bool_true")]
            enabled:    bool,
            gpgkey:     Option<String>,
        }
        fn bool_true() -> bool {
            true
        }

        let txt = std::fs::read_to_string(path)?;
        let file: ReposFile = toml::from_str(&txt)?;
        let entries = file
        .repo
        .into_iter()
        .filter(|r| r.enabled)
        .map(|r| SourceEntry {
            kind:       EntryKind::Deb,
            uri:        r.baseurl,
            suite:      r.suite,
            components: r.components,
            arches:     r.arch,
            signed_by:  r.gpgkey,
            enabled:    true,
            name:       Some(r.name),
        })
        .collect();
        Ok(SourcesList { entries })
    }

    // ── index URL builder ─────────────────────────────────────

    pub fn index_urls(&self, arch: &str) -> Vec<IndexUrl> {
        let mut out = Vec::new();
        for entry in &self.entries {
            if !entry.enabled || entry.kind != EntryKind::Deb {
                continue;
            }
            let arch_list: Vec<&str> = if entry.arches.is_empty() {
                vec![arch]
            } else {
                entry.arches.iter().map(|s| s.as_str()).collect()
            };
            for a in &arch_list {
                for comp in &entry.components {
                    let base = entry.uri.trim_end_matches('/');
                    let url = format!(
                        "{}/dists/{}/{}/binary-{}/Packages",
                        base, entry.suite, comp, a
                    );
                    out.push(IndexUrl {
                        url,
                        inrelease_url: format!(
                            "{}/dists/{}/InRelease",
                            base, entry.suite
                        ),
                        base_uri:  entry.uri.clone(),
                             suite:     entry.suite.clone(),
                             component: comp.clone(),
                             arch:      a.to_string(),
                             signed_by: entry.signed_by.clone(),
                             name:      entry
                             .name
                             .clone()
                             .unwrap_or_else(|| entry.suite.clone()),
                    });
                }
            }
        }
        out
    }

    // ── write default .hk file ────────────────────────────────

    pub fn write_default(arch: &str, suite: &str) -> Result<()> {
        std::fs::create_dir_all("/etc/hammer")?;

        let security_suite = match suite {
            "trixie" | "testing" => format!("{}-security", suite),
            "forky" | "sid" | "unstable" | "experimental" => String::new(),
            other => format!("{}-security", other),
        };

        let security_block = if security_suite.is_empty() {
            String::new()
        } else {
            format!(
                r#"
                [debian-{suite}-security]
                -> name       => debian-{suite}-security
                -> baseurl    => http://security.debian.org/debian-security
                -> suite      => {security_suite}
                -> components => ["main", "contrib", "non-free"]
                -> arch       => ["{arch}"]
                -> enabled    => true
                "#,
                suite = suite,
                security_suite = security_suite,
                arch = arch
            )
        };

        let backports_block =
        if matches!(suite, "bookworm" | "trixie" | "stable" | "testing") {
            format!(
                r#"
                [debian-{suite}-backports]
                -> name       => debian-{suite}-backports
                -> baseurl    => http://deb.debian.org/debian
                -> suite      => {suite}-backports
                -> components => ["main", "contrib", "non-free"]
                -> arch       => ["{arch}"]
                -> enabled    => false
                "#,
                suite = suite,
                arch = arch
            )
        } else {
            String::new()
        };

        let content = format!(
            r#"! Hammer sources list — format: .hk
            ! Edit this file to add/remove repositories.
            ! Run: hammer sync   — to refresh the package index
            !
            ! Syntax:
            !   [section-name]      — one repository per section
            !   -> key => value     — key/value pairs
            !   ! comment           — lines starting with ! are ignored

            [debian-{suite}]
            -> name       => debian-{suite}
            -> baseurl    => http://deb.debian.org/debian
            -> suite      => {suite}
            -> components => ["main", "contrib", "non-free", "non-free-firmware"]
            -> arch       => ["{arch}"]
            -> enabled    => true

            [debian-{suite}-updates]
            -> name       => debian-{suite}-updates
            -> baseurl    => http://deb.debian.org/debian
            -> suite      => {suite}-updates
            -> components => ["main", "contrib", "non-free"]
            -> arch       => ["{arch}"]
            -> enabled    => true
            {security_block}{backports_block}"#,
            suite = suite,
            arch = arch,
            security_block = security_block,
            backports_block = backports_block
        );

        let dest = Path::new(SOURCES_HK);
        std::fs::write(dest, content)?;
        crate::log::info(&format!("repo: wrote {}", SOURCES_HK));
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────
//  apt sources.list fallback parser
// ─────────────────────────────────────────────────────────────

fn parse_apt_sources_list(content: &str) -> Vec<SourceEntry> {
    content
    .lines()
    .filter_map(|l| {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            return None;
        }
        parse_apt_deb822_line(l)
    })
    .collect()
}

fn parse_apt_deb822_line(line: &str) -> Option<SourceEntry> {
    let (kind, rest) = if line.starts_with("deb-src") {
        (EntryKind::DebSrc, line["deb-src".len()..].trim_start())
    } else if line.starts_with("deb") {
        (EntryKind::Deb, line["deb".len()..].trim_start())
    } else {
        return None;
    };
    let (options, rest) = if rest.starts_with('[') {
        let end = rest.find(']')?;
        (Some(&rest[1..end]), rest[end + 1..].trim_start())
    } else {
        (None, rest)
    };
    let mut tokens = rest.split_whitespace();
    let uri = tokens.next()?.to_owned();
    let suite = tokens.next()?.to_owned();
    let components: Vec<String> = tokens.map(|s| s.to_owned()).collect();
    let (arches, signed_by) = parse_apt_options(options);
    Some(SourceEntry {
        kind,
         uri,
         suite,
         components,
         arches,
         signed_by,
         enabled: true,
         name: None,
    })
}

fn parse_apt_options(opts: Option<&str>) -> (Vec<String>, Option<String>) {
    let mut arches = Vec::new();
    let mut signed_by = None;
    if let Some(o) = opts {
        for tok in o.split_whitespace() {
            if let Some(v) = tok.strip_prefix("arch=") {
                arches = v.split(',').map(|s| s.to_owned()).collect();
            }
            if let Some(v) = tok.strip_prefix("signed-by=") {
                signed_by = Some(v.to_owned());
            }
        }
    }
    (arches, signed_by)
}

// ─────────────────────────────────────────────────────────────
//  IndexUrl
// ─────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct IndexUrl {
    pub url:           String,
    pub inrelease_url: String,
    pub base_uri:      String,
    pub suite:         String,
    pub component:     String,
    pub arch:          String,
    pub signed_by:     Option<String>,
    pub name:          String,
}
