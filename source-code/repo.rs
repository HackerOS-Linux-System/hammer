use anyhow::{Context, Result};
use hk_parser::{load_hk_file, resolve_interpolations};
use serde::Deserialize;
use std::path::Path;

pub const SOURCES_HK: &str = "/etc/hammer/sources-list.hk";

#[derive(Debug, Clone, PartialEq)]
pub enum EntryKind { Deb, DebSrc }

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

pub const KNOWN_SUITES: &[&str] = &[
    "bookworm", "bookworm-updates", "bookworm-security", "bookworm-backports",
"trixie",   "trixie-updates",   "trixie-security",   "trixie-backports",
"sid",      "unstable",         "forky",              "experimental",
"stable",   "stable-updates",   "stable-security",    "stable-backports",
"testing",  "testing-updates",  "testing-security",
"oldstable","oldstable-security",
];

// ─────────────────────────────────────────────────────────────
//  SourcesList
// ─────────────────────────────────────────────────────────────

impl SourcesList {
    pub fn entries(&self) -> &[SourceEntry] { &self.entries }
    pub fn entries_mut(&mut self) -> &mut Vec<SourceEntry> { &mut self.entries }

    pub fn add_entry(&mut self, entry: SourceEntry) {
        self.entries.push(entry);
    }

    pub fn remove_entry_by_index(&mut self, idx: usize) {
        if idx < self.entries.len() { self.entries.remove(idx); }
    }

    pub fn remove_entry_by_uri(&mut self, uri: &str) {
        self.entries.retain(|e| !e.uri.contains(uri));
    }

    pub fn set_enabled_by_index(&mut self, idx: usize, enabled: bool) {
        if let Some(e) = self.entries.get_mut(idx) { e.enabled = enabled; }
    }

    pub fn set_enabled_by_uri(&mut self, uri: &str, enabled: bool) {
        for e in &mut self.entries {
            if e.uri.contains(uri) { e.enabled = enabled; }
        }
    }

    pub fn set_default_by_index(&mut self, idx: usize) {
        // Move the entry at idx to position 0
        if idx < self.entries.len() {
            let entry = self.entries.remove(idx);
            self.entries.insert(0, entry);
        }
    }

    pub fn set_default_by_uri(&mut self, uri: &str) {
        if let Some(idx) = self.entries.iter().position(|e| e.uri.contains(uri)) {
            self.set_default_by_index(idx);
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(std::path::Path::new(SOURCES_HK))
    }

    pub fn save_to(&self, path: &std::path::Path) -> anyhow::Result<()> {
        use std::io::Write;
        let mut f = std::fs::File::create(path)?;
        for e in &self.entries {
            let kind = match e.kind {
                EntryKind::Deb    => "deb",
                EntryKind::DebSrc => "deb-src",
            };
            if !e.enabled { write!(f, "# ")?; }
            write!(f, "{} {} {}", kind, e.uri, e.suite)?;
            for comp in &e.components { write!(f, " {}", comp)?; }
            writeln!(f)?;
        }
        Ok(())
    }

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

        for (section_name, section_val) in &config {
            if section_name.starts_with('_') || section_name == "meta" { continue; }

            let map = match section_val.as_map() {
                Ok(m)  => m,
                Err(_) => continue,
            };

            let enabled = map.get("enabled")
            .and_then(|v| v.as_bool().ok())
            .unwrap_or(true);
            if !enabled { continue; }

            let kind = match map.get("type")
            .and_then(|v| v.as_string().ok())
            .as_deref()
            {
                Some("deb-src") => EntryKind::DebSrc,
                _               => EntryKind::Deb,
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
            let name = map.get("name")
            .and_then(|v| v.as_string().ok())
            .or_else(|| Some(section_name.clone()));

            entries.push(SourceEntry {
                kind, uri, suite, components, arches, signed_by,
                enabled: true, name,
            });
        }
        Ok(SourcesList { entries })
    }

    // ── legacy TOML fallback ──────────────────────────────────

    fn load_legacy_toml(path: &Path) -> Result<Self> {
        #[derive(Deserialize)]
        struct ReposFile { repo: Vec<HammerRepo> }
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
        fn bool_true() -> bool { true }

        let txt = std::fs::read_to_string(path)?;
        let file: ReposFile = toml::from_str(&txt)?;
        let entries = file.repo.into_iter().filter(|r| r.enabled).map(|r| SourceEntry {
            kind:       EntryKind::Deb,
            uri:        r.baseurl,
            suite:      r.suite,
            components: r.components,
            arches:     r.arch,
            signed_by:  r.gpgkey,
            enabled:    true,
            name:       Some(r.name),
        }).collect();
        Ok(SourcesList { entries })
    }

    // ── index URL builder ─────────────────────────────────────

    pub fn index_urls(&self, arch: &str) -> Vec<IndexUrl> {
        let mut out = Vec::new();
        for entry in &self.entries {
            if !entry.enabled || entry.kind != EntryKind::Deb { continue; }
            let arch_list: Vec<&str> = if entry.arches.is_empty() {
                vec![arch]
            } else {
                entry.arches.iter().map(|s| s.as_str()).collect()
            };
            for a in &arch_list {
                for comp in &entry.components {
                    let base = entry.uri.trim_end_matches('/');
                    let url  = format!("{}/dists/{}/{}/binary-{}/Packages",
                                       base, entry.suite, comp, a);
                    out.push(IndexUrl {
                        url,
                        inrelease_url: format!("{}/dists/{}/InRelease", base, entry.suite),
                             base_uri:  entry.uri.clone(),
                             suite:     entry.suite.clone(),
                             component: comp.clone(),
                             arch:      a.to_string(),
                             signed_by: entry.signed_by.clone(),
                             name:      entry.name.clone().unwrap_or_else(|| entry.suite.clone()),
                    });
                }
            }
        }
        out
    }

    // ── write default .hk ─────────────────────────────────────

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
                "\n[debian-{suite}-security]\n\
-> name       => debian-{suite}-security\n\
-> baseurl    => http://security.debian.org/debian-security\n\
-> suite      => {security_suite}\n\
-> components => [\"main\", \"contrib\", \"non-free\"]\n\
-> arch       => [\"{arch}\"]\n\
-> enabled    => true\n",
suite = suite, security_suite = security_suite, arch = arch
            )
        };

        let backports_block =
        if matches!(suite, "bookworm" | "trixie" | "stable" | "testing") {
            format!(
                "\n[debian-{suite}-backports]\n\
-> name       => debian-{suite}-backports\n\
-> baseurl    => http://deb.debian.org/debian\n\
-> suite      => {suite}-backports\n\
-> components => [\"main\", \"contrib\", \"non-free\"]\n\
-> arch       => [\"{arch}\"]\n\
-> enabled    => false\n",
suite = suite, arch = arch
            )
        } else {
            String::new()
        };

        let content = format!(
            "! Hammer sources list — format: .hk\n\
! Edit then run: hammer sync\n\n\
[debian-{suite}]\n\
-> name       => debian-{suite}\n\
-> baseurl    => http://deb.debian.org/debian\n\
-> suite      => {suite}\n\
-> components => [\"main\", \"contrib\", \"non-free\", \"non-free-firmware\"]\n\
-> arch       => [\"{arch}\"]\n\
-> enabled    => true\n\n\
[debian-{suite}-updates]\n\
-> name       => debian-{suite}-updates\n\
-> baseurl    => http://deb.debian.org/debian\n\
-> suite      => {suite}-updates\n\
-> components => [\"main\", \"contrib\", \"non-free\"]\n\
-> arch       => [\"{arch}\"]\n\
-> enabled    => true\n\
{security_block}{backports_block}",
suite = suite, arch = arch,
security_block = security_block,
backports_block = backports_block
        );

        std::fs::write(Path::new(SOURCES_HK), content)?;
        crate::log::info(&format!("repo: wrote {}", SOURCES_HK));
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────
//  apt sources.list fallback parser
// ─────────────────────────────────────────────────────────────

fn parse_apt_sources_list(content: &str) -> Vec<SourceEntry> {
    content.lines().filter_map(|l| {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') { return None; }
        parse_apt_deb822_line(l)
    }).collect()
}

fn parse_apt_deb822_line(line: &str) -> Option<SourceEntry> {
    let (kind, rest) = if line.starts_with("deb-src") {
        (EntryKind::DebSrc, line["deb-src".len()..].trim_start())
    } else if line.starts_with("deb") {
        (EntryKind::Deb, line["deb".len()..].trim_start())
    } else { return None; };
    let (options, rest) = if rest.starts_with('[') {
        let end = rest.find(']')?;
        (Some(&rest[1..end]), rest[end+1..].trim_start())
    } else { (None, rest) };
    let mut tokens     = rest.split_whitespace();
    let uri            = tokens.next()?.to_owned();
    let suite          = tokens.next()?.to_owned();
    let components: Vec<String> = tokens.map(|s| s.to_owned()).collect();
    let (arches, signed_by) = parse_apt_options(options);
    Some(SourceEntry { kind, uri, suite, components, arches, signed_by, enabled: true, name: None })
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

// ─────────────────────────────────────────────────────────────
//  cmd_repo — manage sources-list.hk
// ─────────────────────────────────────────────────────────────

/// `hammer repo <sub> [args…]`
pub fn cmd_repo(args: &[String]) -> anyhow::Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" | "ls" => repo_list(),
        "add"         => repo_add(&args[1..]),
        "remove" | "rm" | "delete" => repo_remove(&args[1..]),
        "enable"      => repo_enable(&args[1..], true),
        "disable"     => repo_enable(&args[1..], false),
        "info"        => repo_info(&args[1..]),
        "set-default" => repo_set_default(&args[1..]),
        other => anyhow::bail!(
            "Unknown repo subcommand '{}'\n  \
             Usage: hammer repo [list|add <url>|remove <id>|enable <id>|disable <id>|info <id>]",
            other
        ),
    }
}

fn repo_list() -> anyhow::Result<()> {
    use owo_colors::OwoColorize;
    let list = SourcesList::load()?;
    println!();
    println!("  {}  Configured repositories  ({})", "⬡".bright_cyan().bold(), SOURCES_HK.dimmed());
    println!("  {}", "─".repeat(70).dimmed());
    println!("  {:<4} {:<36} {:<14} {}",
             "#".bold(), "URI".bold(), "Suite".bold(), "Components".bold());
    println!("  {}", "─".repeat(70).dimmed());

    let entries = list.entries();
    if entries.is_empty() {
        println!("  {} No repositories configured.", "·".dimmed());
        println!("  Hint: {}", "hammer repo add <uri> <suite> <components>".cyan());
        return Ok(());
    }

    for (i, e) in entries.iter().enumerate() {
        let status = if e.enabled {
            "✔".bright_green().to_string()
        } else {
            "✘".red().to_string()
        };
        let uri_short = if e.uri.len() > 34 {
            format!("{}…", &e.uri[..33])
        } else {
            e.uri.clone()
        };
        println!("  {} {:<2} {:<36} {:<14} {}",
                 status,
                 (i + 1).to_string().dimmed(),
                 uri_short.cyan(),
                 e.suite.bold(),
                 e.components.join(" ").dimmed());
    }
    println!();
    println!("  {} entries. Toggle with {}.",
             entries.len(), "hammer repo enable/disable <#>".cyan());
    Ok(())
}

fn repo_add(args: &[String]) -> anyhow::Result<()> {
    use owo_colors::OwoColorize;
    // hammer repo add <uri> <suite> [components…]
    // or: hammer repo add "deb https://… suite comp1 comp2"  (apt-style)
    if args.is_empty() {
        anyhow::bail!("Usage: hammer repo add <uri> <suite> [main contrib non-free …]");
    }

    // Support apt-style "deb <uri> <suite> <comps>" as a single arg
    let (uri, suite, components) = if args[0].starts_with("deb ") || args[0].starts_with("deb-src ") {
        let parts: Vec<&str> = args[0].splitn(4, ' ').collect();
        if parts.len() < 3 {
            anyhow::bail!("Invalid apt-style source: '{}'", args[0]);
        }
        let comps: Vec<String> = if parts.len() >= 4 {
            parts[3].split_whitespace().map(|s| s.to_string()).collect()
        } else {
            vec!["main".to_string()]
        };
        (parts[1].to_string(), parts[2].to_string(), comps)
    } else if args.len() >= 2 {
        let uri   = args[0].clone();
        let suite = args[1].clone();
        let comps: Vec<String> = if args.len() >= 3 {
            args[2..].to_vec()
        } else {
            vec!["main".to_string()]
        };
        (uri, suite, comps)
    } else {
        anyhow::bail!("Usage: hammer repo add <uri> <suite> [main contrib non-free …]");
    };

    let mut list = SourcesList::load()?;
    // Check for duplicates
    if list.entries().iter().any(|e| e.uri == uri && e.suite == suite) {
        println!("  {} Repository already configured: {} {}", "·".yellow(), uri, suite);
        return Ok(());
    }

    list.add_entry(SourceEntry {
        kind:       EntryKind::Deb,
        uri:        uri.clone(),
        suite:      suite.clone(),
        components: components.clone(),
        arches:     vec![],
        signed_by:  None,
        enabled:    true,
        name:       None,
    });
    list.save()?;

    println!("  {} Added: {} {} {}", "✔".bright_green().bold(),
             uri.cyan(), suite.bold(), components.join(" ").dimmed());
    println!("  Run {} to update the package index.", "hammer sync".cyan());
    Ok(())
}

fn repo_remove(args: &[String]) -> anyhow::Result<()> {
    use owo_colors::OwoColorize;
    let id = args.first().ok_or_else(|| anyhow::anyhow!("Usage: hammer repo remove <# or uri>"))?;

    let mut list = SourcesList::load()?;
    let before = list.entries().len();

    // Try by index (1-based) first, then by URI substring
    if let Ok(idx) = id.parse::<usize>() {
        list.remove_entry_by_index(idx.saturating_sub(1));
    } else {
        list.remove_entry_by_uri(id);
    }

    let after = list.entries().len();
    if after == before {
        anyhow::bail!("No repository matched '{}'", id);
    }
    list.save()?;
    println!("  {} Removed repository {}.", "✔".bright_green(), id.cyan());
    Ok(())
}

fn repo_enable(args: &[String], enable: bool) -> anyhow::Result<()> {
    use owo_colors::OwoColorize;
    let id = args.first().ok_or_else(|| anyhow::anyhow!(
        "Usage: hammer repo {}/disable <# or uri>",
        if enable { "enable" } else { "disable" }
    ))?;

    let mut list = SourcesList::load()?;
    let verb = if enable { "Enabled" } else { "Disabled" };

    if let Ok(idx) = id.parse::<usize>() {
        list.set_enabled_by_index(idx.saturating_sub(1), enable);
    } else {
        list.set_enabled_by_uri(id, enable);
    }
    list.save()?;
    println!("  {} {} repository {}.", "✔".bright_green(), verb, id.cyan());
    Ok(())
}

fn repo_info(args: &[String]) -> anyhow::Result<()> {
    use owo_colors::OwoColorize;
    let id = args.first().ok_or_else(|| anyhow::anyhow!("Usage: hammer repo info <# or uri>"))?;
    let list = SourcesList::load()?;
    let entry = if let Ok(idx) = id.parse::<usize>() {
        list.entries().get(idx.saturating_sub(1)).cloned()
    } else {
        list.entries().iter().find(|e| e.uri.contains(id.as_str())).cloned()
    };
    match entry {
        None => anyhow::bail!("No repository matched '{}'", id),
        Some(e) => {
            println!();
            println!("  {:<20} {}", "URI:".bold(), e.uri.cyan());
            println!("  {:<20} {}", "Suite:".bold(), e.suite);
            println!("  {:<20} {}", "Components:".bold(), e.components.join(" "));
            println!("  {:<20} {}", "Enabled:".bold(),
                     if e.enabled { "yes".bright_green().to_string() }
                     else         { "no".red().to_string() });
        }
    }
    Ok(())
}

fn repo_set_default(args: &[String]) -> anyhow::Result<()> {
    use owo_colors::OwoColorize;
    let id = args.first().ok_or_else(|| anyhow::anyhow!("Usage: hammer repo set-default <# or uri>"))?;
    let mut list = SourcesList::load()?;
    if let Ok(idx) = id.parse::<usize>() {
        list.set_default_by_index(idx.saturating_sub(1));
    } else {
        list.set_default_by_uri(id);
    }
    list.save()?;
    println!("  {} Default repository set to {}.", "✔".bright_green(), id.cyan());
    Ok(())
}
