use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────
//  Version constraint
// ─────────────────────────────────────────────────────────────

/// Operator in a version constraint (`>=`, `<=`, `=`, `>>`, `<<`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VersionOp {
    /// `>=`
    Gte,
    /// `<=`
    Lte,
    /// `=`
    Eq,
    /// `>>` (strictly greater)
    Gt,
    /// `<<` (strictly less)
    Lt,
}

impl VersionOp {
    /// Parse the operator string.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            ">=" => Some(Self::Gte),
            "<=" => Some(Self::Lte),
            "="  => Some(Self::Eq),
            ">>" => Some(Self::Gt),
            "<<" => Some(Self::Lt),
            _    => None,
        }
    }

    /// Return the operator as a string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Gte => ">=",
            Self::Lte => "<=",
            Self::Eq  => "=",
            Self::Gt  => ">>",
            Self::Lt  => "<<",
        }
    }
}

/// A version constraint: operator + version string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionConstraint {
    /// Comparison operator.
    pub op:      String,
    /// Required version string.
    pub version: String,
}

// ─────────────────────────────────────────────────────────────
//  Dependency types
// ─────────────────────────────────────────────────────────────

/// One alternative in an OR-group (e.g. `libssl3` or `libssl3 (>= 3.0)`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepAlternative {
    /// Package name (may be a virtual name).
    pub name:       String,
    /// Optional version constraint.
    pub constraint: Option<VersionConstraint>,
    /// Architecture qualifier (e.g. `:amd64`).
    pub arch:       Option<String>,
}

/// A dependency group: one or more OR-alternatives.
///
/// The group is satisfied when **any** alternative is satisfied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepGroup {
    /// The alternatives (at least one).
    pub alternatives: Vec<DepAlternative>,
}

// ─────────────────────────────────────────────────────────────
//  Package
// ─────────────────────────────────────────────────────────────

/// All metadata fields for a Debian package.
///
/// Parsed from a `Packages` index or a `.deb` control file.
/// Unknown fields are silently ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    /// Package name.
    pub name:              String,
    /// Debian-format version string.
    pub version:           String,
    /// Architecture (e.g. `amd64`, `all`).
    pub architecture:      String,
    /// Installed size in kilobytes.
    pub installed_size_kb: u64,
    /// Download size in bytes.
    pub download_size:     Option<u64>,
    /// Section (e.g. `utils`, `libs`).
    pub section:           Option<String>,
    /// Priority (`required`, `important`, `standard`, `optional`, `extra`).
    pub priority:          Option<String>,
    /// Maintainer name / email.
    pub maintainer:        Option<String>,
    /// Homepage URL.
    pub homepage:          Option<String>,
    /// One-line description.
    pub description_short: Option<String>,
    /// Full description (multi-line).
    pub description:       Option<String>,
    /// `Depends` field (raw string).
    pub depends:           Option<String>,
    /// `Pre-Depends` field.
    pub pre_depends:       Option<String>,
    /// `Recommends` field.
    pub recommends:        Option<String>,
    /// `Suggests` field.
    pub suggests:          Option<String>,
    /// `Enhances` field.
    pub enhances:          Option<String>,
    /// `Breaks` field.
    pub breaks:            Option<String>,
    /// `Conflicts` field.
    pub conflicts:         Option<String>,
    /// `Provides` field.
    pub provides:          Option<String>,
    /// `Replaces` field.
    pub replaces:          Option<String>,
    /// SHA-256 checksum of the `.deb` file.
    pub sha256:            Option<String>,
    /// Filename inside the repo (relative to pool root).
    pub filename:          Option<String>,
    /// Base URI of the repository this came from.
    pub repo_base_uri:     Option<String>,
    /// Whether this package is marked Essential.
    pub essential:         bool,
    /// Parsed `Provides` list (virtual names this package satisfies).
    pub provides_list:     Vec<String>,
}

impl Package {
    /// Parse one stanza from a `Packages` index (blank-line delimited).
    ///
    /// Returns `None` if the block is missing `Package:` or `Version:`.
    pub fn parse_block(block: &str) -> Option<Self> {
        let mut p = Package::default();
        let mut desc_lines: Vec<String> = Vec::new();
        let mut in_desc = false;

        for line in block.lines() {
            if line.starts_with(' ') || line.starts_with('\t') {
                if in_desc {
                    let stripped = line.trim();
                    if stripped == "." { desc_lines.push(String::new()); }
                    else               { desc_lines.push(stripped.to_string()); }
                }
                continue;
            }
            in_desc = false;

            let (key, val) = match line.split_once(':') {
                Some((k, v)) => (k.trim(), v.trim()),
                None         => continue,
            };

            match key {
                "Package"          => p.name              = val.into(),
                "Version"          => p.version           = val.into(),
                "Architecture"     => p.architecture      = val.into(),
                "Installed-Size"   => p.installed_size_kb = val.parse().unwrap_or(0),
                "Size"             => p.download_size     = val.parse().ok(),
                "Section"          => p.section           = Some(val.into()),
                "Priority"         => p.priority          = Some(val.into()),
                "Maintainer"       => p.maintainer        = Some(val.into()),
                "Homepage"         => p.homepage          = Some(val.into()),
                "SHA256"           => p.sha256            = Some(val.into()),
                "Filename"         => p.filename          = Some(val.into()),
                "Depends"          => p.depends           = Some(val.into()),
                "Pre-Depends"      => p.pre_depends       = Some(val.into()),
                "Recommends"       => p.recommends        = Some(val.into()),
                "Suggests"         => p.suggests          = Some(val.into()),
                "Enhances"         => p.enhances          = Some(val.into()),
                "Breaks"           => p.breaks            = Some(val.into()),
                "Conflicts"        => p.conflicts         = Some(val.into()),
                "Provides"         => {
                    p.provides = Some(val.into());
                    p.provides_list = val.split(',')
                        .map(|s| s.split('(').next().unwrap_or("").trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                "Replaces"         => p.replaces          = Some(val.into()),
                "Essential"        => p.essential         = val.eq_ignore_ascii_case("yes"),
                "Description"      => {
                    p.description_short = Some(val.into());
                    in_desc = true;
                }
                _ => {}
            }
        }

        if p.name.is_empty() || p.version.is_empty() { return None; }
        if !desc_lines.is_empty() {
            p.description = Some(desc_lines.join("\n"));
        }
        Some(p)
    }

    /// Parse a full `Packages` index file, returning all valid stanzas.
    pub fn parse_index(content: &str) -> Vec<Self> {
        content.split("\n\n")
            .filter_map(|b| Self::parse_block(b.trim()))
            .collect()
    }

    /// Return the short (first-line) description, or an empty string.
    pub fn short_description(&self) -> &str {
        self.description_short.as_deref().unwrap_or("")
    }

    /// Return the download URL given a repository base URI.
    pub fn download_url(&self) -> Option<String> {
        let base = self.repo_base_uri.as_deref()?;
        let path = self.filename.as_deref()?;
        // Debian convention: strip "dists/…" suffix, then append pool path
        let root = base.trim_end_matches('/');
        Some(format!("{}/{}", root, path))
    }
}
