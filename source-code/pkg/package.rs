use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

// ─────────────────────────────────────────────────────────────
//  VersionOp
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VersionOp {
    Eq,  // =
    Lt,  // <<
    Le,  // <=
    Gt,  // >>
    Ge,  // >=
}

impl VersionOp {
    pub fn as_str(&self) -> &'static str {
        match self {
            VersionOp::Eq => "=",
            VersionOp::Lt => "<<",
            VersionOp::Le => "<=",
            VersionOp::Gt => ">>",
            VersionOp::Ge => ">=",
        }
    }
}

impl std::fmt::Display for VersionOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ─────────────────────────────────────────────────────────────
//  VersionConstraint
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionConstraint {
    pub op:      VersionOp,
    pub version: String,
}

impl std::fmt::Display for VersionConstraint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.op, self.version)
    }
}

// ─────────────────────────────────────────────────────────────
//  DepAlternative / DepGroup
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DepAlternative {
    pub name:       String,
    pub constraint: Option<VersionConstraint>,
    pub arch:       Option<String>,
}

#[derive(Debug, Clone)]
pub struct DepGroup {
    pub alternatives: Vec<DepAlternative>,
}

// ─────────────────────────────────────────────────────────────
//  Package
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub name:              String,
    pub version:           String,
    pub architecture:      String,

    pub depends:           Option<String>,
    pub pre_depends:       Option<String>,
    pub recommends:        Option<String>,
    pub suggests:          Option<String>,
    pub conflicts:         Option<String>,
    pub breaks:            Option<String>,
    pub provides:          Option<String>,
    pub replaces:          Option<String>,
    pub enhances:          Option<String>,

    pub section:           Option<String>,
    pub priority:          Option<String>,
    pub maintainer:        Option<String>,
    pub homepage:          Option<String>,
    pub description_short: Option<String>,
    pub description_long:  Option<String>,

    pub installed_size_kb: Option<u64>,
    pub download_size:     Option<u64>,

    pub repo_base_uri:     Option<String>,
    pub filename:          Option<String>,
    pub sha256:            Option<String>,

    pub provides_list:     Vec<String>,
}

// ── Default ─────────────────────────────────────────────────
// Used in diff.rs for constructing placeholder Package values.

impl Default for Package {
    fn default() -> Self {
        Package {
            name:              String::new(),
            version:           String::new(),
            architecture:      String::new(),
            depends:           None,
            pre_depends:       None,
            recommends:        None,
            suggests:          None,
            conflicts:         None,
            breaks:            None,
            provides:          None,
            replaces:          None,
            enhances:          None,
            section:           None,
            priority:          None,
            maintainer:        None,
            homepage:          None,
            description_short: None,
            description_long:  None,
            installed_size_kb: None,
            download_size:     None,
            repo_base_uri:     None,
            filename:          None,
            sha256:            None,
            provides_list:     Vec::new(),
        }
    }
}

impl Package {
    /// Parse a raw Debian control stanza.
    pub fn parse_block(block: &str) -> Option<Self> {
        let mut p = Package::default();

        let mut lines     = block.lines().peekable();
        let mut cur_field = String::new();
        let mut cur_value = String::new();

        macro_rules! flush {
            () => {
                if !cur_field.is_empty() {
                    p.set_field(&cur_field, &cur_value);
                    cur_field.clear();
                    cur_value.clear();
                }
            };
        }

        while let Some(line) = lines.next() {
            if line.starts_with(' ') || line.starts_with('\t') {
                cur_value.push('\n');
                cur_value.push_str(line.trim_start());
            } else if let Some(colon) = line.find(':') {
                flush!();
                cur_field = line[..colon].trim().to_lowercase();
                cur_value = line[colon+1..].trim().to_string();
            }
        }
        flush!();

        if p.name.is_empty() || p.version.is_empty() { return None; }
        Some(p)
    }

    fn set_field(&mut self, field: &str, value: &str) {
        let v = value.trim().to_string();
        match field {
            "package"        => self.name              = v,
            "version"        => self.version            = v,
            "architecture"   => self.architecture       = v,
            "depends"        => self.depends             = Some(v),
            "pre-depends"    => self.pre_depends         = Some(v),
            "recommends"     => self.recommends          = Some(v),
            "suggests"       => self.suggests            = Some(v),
            "conflicts"      => self.conflicts           = Some(v),
            "breaks"         => self.breaks              = Some(v),
            "provides"       => self.provides            = Some(v),
            "replaces"       => self.replaces            = Some(v),
            "enhances"       => self.enhances            = Some(v),
            "section"        => self.section             = Some(v),
            "priority"       => self.priority            = Some(v),
            "maintainer"     => self.maintainer          = Some(v),
            "homepage"       => self.homepage            = Some(v),
            "filename"       => self.filename            = Some(v),
            "sha256"         => self.sha256              = Some(v),
            "installed-size" => self.installed_size_kb   = v.parse().ok(),
            "size"           => self.download_size        = v.parse().ok(),
            "description"    => {
                let mut parts = v.splitn(2, '\n');
                self.description_short = Some(parts.next().unwrap_or("").trim().to_string());
                self.description_long  = parts.next().map(|s| s.to_string());
            }
            _ => {}
        }
    }

    pub fn parse_index(content: &str) -> Vec<Self> {
        content.split("\n\n")
        .filter_map(|block| {
            let b = block.trim();
            if b.is_empty() { None } else { Self::parse_block(b) }
        })
        .collect()
    }
}

// ─────────────────────────────────────────────────────────────
//  Dependency parser
// ─────────────────────────────────────────────────────────────

pub fn parse_dep_field(s: &str) -> Vec<DepGroup> {
    s.split(',')
    .map(|group| {
        let alternatives = group.split('|').map(|alt| parse_dep_alt(alt.trim())).collect();
        DepGroup { alternatives }
    })
    .collect()
}

fn parse_dep_alt(s: &str) -> DepAlternative {
    let (name_part, rest) = match s.find('(') {
        Some(i) => (&s[..i], Some(&s[i..])),
        None    => (s, None),
    };

    let (pkg_name, arch) = if let Some(colon) = name_part.find(':') {
        (name_part[..colon].trim().to_string(), Some(name_part[colon+1..].trim().to_string()))
    } else {
        (name_part.trim().to_string(), None)
    };

    let constraint = rest.and_then(|r| {
        let inner = r.trim_start_matches('(').split(')').next()?;
        let mut parts = inner.splitn(2, ' ');
        let op_str = parts.next()?.trim();
        let ver    = parts.next()?.trim().to_string();
        let op = parse_version_op(op_str)?;
        Some(VersionConstraint { op, version: ver })
    });

    DepAlternative { name: pkg_name, constraint, arch }
}

fn parse_version_op(s: &str) -> Option<VersionOp> {
    match s {
        "="  | "==" => Some(VersionOp::Eq),
        "<<" | "<"  => Some(VersionOp::Lt),
        "<=" | "=<" => Some(VersionOp::Le),
        ">>" | ">"  => Some(VersionOp::Gt),
        ">=" | "=>" => Some(VersionOp::Ge),
        _           => None,
    }
}

// ─────────────────────────────────────────────────────────────
//  Version comparison
// ─────────────────────────────────────────────────────────────

pub fn version_cmp(a: &str, b: &str) -> Ordering {
    crate::solver::version::compare(a, b)
}

pub fn version_satisfies(installed: &str, op: &str, required: &str) -> bool {
    crate::solver::version::satisfies(installed, op, required)
}
