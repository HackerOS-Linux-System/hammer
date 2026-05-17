use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─────────────────────────────────────────────────────────────
//  Package
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Package {
    pub name:              String,
    pub version:           String,
    pub architecture:      String,
    pub description_short: Option<String>,
    pub description_long:  Option<String>,
    pub section:           Option<String>,
    pub priority:          Option<String>,
    pub maintainer:        Option<String>,
    pub installed_size_kb: Option<u64>,
    pub download_size:     Option<u64>,
    pub filename:          Option<String>,
    pub sha256:            Option<String>,
    pub md5sum:            Option<String>,
    pub depends:           Option<String>,
    pub pre_depends:       Option<String>,
    pub recommends:        Option<String>,
    pub suggests:          Option<String>,
    pub conflicts:         Option<String>,
    pub replaces:          Option<String>,
    pub breaks:            Option<String>,
    pub provides:          Option<String>,
    pub homepage:          Option<String>,
    pub source:            Option<String>,
    pub repo_base_uri:     Option<String>,
}

impl Package {
    pub fn parse_index(content: &str) -> Vec<Package> {
        content
        .split("\n\n")
        .filter_map(|block| {
            let b = block.trim();
            if b.is_empty() { None } else { Package::parse_block(b) }
        })
        .collect()
    }

    pub fn parse_block(block: &str) -> Option<Package> {
        let mut map: HashMap<String, String> = HashMap::new();
        let mut cur_key = String::new();
        let mut cur_val = String::new();

        for line in block.lines() {
            if line.starts_with(' ') || line.starts_with('\t') {
                if !cur_key.is_empty() {
                    cur_val.push('\n');
                    cur_val.push_str(line.trim_start());
                }
            } else if let Some(idx) = line.find(':') {
                if !cur_key.is_empty() {
                    map.insert(cur_key.to_lowercase(), cur_val.trim().to_owned());
                }
                cur_key = line[..idx].trim().to_owned();
                cur_val = line[idx + 1..].trim().to_owned();
            }
        }
        if !cur_key.is_empty() {
            map.insert(cur_key.to_lowercase(), cur_val.trim().to_owned());
        }

        let name    = map.remove("package")?;
        let version = map.remove("version")?;
        let arch    = map.remove("architecture").unwrap_or_else(|| "all".into());
        let (desc_short, desc_long) = split_description(map.remove("description"));

        Some(Package {
            name,
            version,
            architecture:      arch,
            description_short: desc_short,
            description_long:  desc_long,
            section:           map.remove("section"),
             priority:          map.remove("priority"),
             maintainer:        map.remove("maintainer"),
             installed_size_kb: map.remove("installed-size").and_then(|v| v.parse().ok()),
             download_size:     map.remove("size").and_then(|v| v.parse().ok()),
             filename:          map.remove("filename"),
             sha256:            map.remove("sha256"),
             md5sum:            map.remove("md5sum"),
             depends:           map.remove("depends"),
             pre_depends:       map.remove("pre-depends"),
             recommends:        map.remove("recommends"),
             suggests:          map.remove("suggests"),
             conflicts:         map.remove("conflicts"),
             replaces:          map.remove("replaces"),
             breaks:            map.remove("breaks"),
             provides:          map.remove("provides"),
             homepage:          map.remove("homepage"),
             source:            map.remove("source"),
             repo_base_uri:     None,
        })
    }
}

fn split_description(raw: Option<String>) -> (Option<String>, Option<String>) {
    match raw {
        None => (None, None),
        Some(s) => {
            let mut lines = s.lines();
            let short = lines.next().map(|l| l.trim().to_owned()).filter(|l| !l.is_empty());
            let long: Vec<&str> = lines.collect();
            let long_str = long.join("\n");
            (short, if long_str.trim().is_empty() { None } else { Some(long_str) })
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  Dependency structures
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DepGroup {
    pub alternatives: Vec<SingleDep>,
}

#[derive(Debug, Clone)]
pub struct SingleDep {
    pub name:       String,
    pub constraint: Option<VersionConstraint>,
    pub arch_qual:  Option<String>,
}

#[derive(Debug, Clone)]
pub struct VersionConstraint {
    pub op:      VersionOp,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VersionOp { Eq, Lt, Le, Gt, Ge }

pub fn parse_dep_field(s: &str) -> Vec<DepGroup> {
    s.split(',')
    .filter_map(|chunk| {
        let chunk = chunk.trim();
        if chunk.is_empty() { return None; }
        let alts: Vec<SingleDep> = chunk
        .split('|')
        .filter_map(|alt| parse_single_dep(alt.trim()))
        .collect();
        if alts.is_empty() { None } else { Some(DepGroup { alternatives: alts }) }
    })
    .collect()
}

fn parse_single_dep(s: &str) -> Option<SingleDep> {
    let s = s.trim();
    if s.is_empty() { return None; }
    if let Some(paren) = s.find('(') {
        let (raw_name, rest) = s.split_at(paren);
        let name_part = raw_name.trim();
        let (name, arch_qual) = split_arch_qual(name_part);
        let inner = rest.trim_start_matches('(').trim_end_matches(')').trim();
        let constraint = parse_constraint(inner);
        Some(SingleDep { name, arch_qual, constraint })
    } else {
        let (name, arch_qual) = split_arch_qual(s);
        Some(SingleDep { name, arch_qual, constraint: None })
    }
}

fn split_arch_qual(s: &str) -> (String, Option<String>) {
    if let Some(colon) = s.find(':') {
        (s[..colon].trim().to_owned(), Some(s[colon..].to_owned()))
    } else {
        (s.trim().to_owned(), None)
    }
}

fn parse_constraint(s: &str) -> Option<VersionConstraint> {
    let (op, ver) = if s.starts_with(">=") {
        (VersionOp::Ge, s[2..].trim())
    } else if s.starts_with("<=") {
        (VersionOp::Le, s[2..].trim())
    } else if s.starts_with(">>") {
        (VersionOp::Gt, s[2..].trim())
    } else if s.starts_with("<<") {
        (VersionOp::Lt, s[2..].trim())
    } else if s.starts_with('=') {
        (VersionOp::Eq, s[1..].trim())
    } else {
        return None;
    };
    Some(VersionConstraint { op, version: ver.to_owned() })
}

// ─────────────────────────────────────────────────────────────
//  Version comparison (Debian algorithm)
// ─────────────────────────────────────────────────────────────

pub fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (ae, a_rest) = split_epoch(a);
    let (be, b_rest) = split_epoch(b);
    if ae != be { return ae.cmp(&be); }
    let (au, ar) = split_revision(a_rest);
    let (bu, br) = split_revision(b_rest);
    let uc = compare_upstream(au, bu);
    if uc != std::cmp::Ordering::Equal { return uc; }
    compare_upstream(ar, br)
}

fn split_epoch(v: &str) -> (u32, &str) {
    if let Some(c) = v.find(':') {
        if let Ok(e) = v[..c].parse::<u32>() {
            return (e, &v[c + 1..]);
        }
    }
    (0, v)
}

fn split_revision(v: &str) -> (&str, &str) {
    if let Some(d) = v.rfind('-') {
        (&v[..d], &v[d + 1..])
    } else {
        (v, "0")
    }
}

fn compare_upstream(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        let a_str: String = ai.by_ref().take_while(|c| !c.is_ascii_digit()).collect();
        let b_str: String = bi.by_ref().take_while(|c| !c.is_ascii_digit()).collect();
        let sc = compare_non_digit(&a_str, &b_str);
        if sc != std::cmp::Ordering::Equal { return sc; }
        let a_num: String = ai.by_ref().take_while(|c| c.is_ascii_digit()).collect();
        let b_num: String = bi.by_ref().take_while(|c| c.is_ascii_digit()).collect();
        if a_num.is_empty() && b_num.is_empty() { break; }
        let an: u64 = a_num.parse().unwrap_or(0);
        let bn: u64 = b_num.parse().unwrap_or(0);
        let nc = an.cmp(&bn);
        if nc != std::cmp::Ordering::Equal { return nc; }
    }
    std::cmp::Ordering::Equal
}

fn compare_non_digit(a: &str, b: &str) -> std::cmp::Ordering {
    let order = |c: char| -> i32 {
        if c == '~' { -1 }
        else if c.is_ascii_alphabetic() { c as i32 }
        else { c as i32 + 256 }
    };
    let mut ai = a.chars();
    let mut bi = b.chars();
    loop {
        match (ai.next(), bi.next()) {
            (None, None)       => return std::cmp::Ordering::Equal,
            (None, Some(bc))   => return if bc == '~' { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Less },
            (Some(ac), None)   => return if ac == '~' { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater },
            (Some(ac), Some(bc)) => {
                let cmp = order(ac).cmp(&order(bc));
                if cmp != std::cmp::Ordering::Equal { return cmp; }
            }
        }
    }
}

pub fn version_satisfies(installed: &str, op: &VersionOp, required: &str) -> bool {
    let c = version_cmp(installed, required);
    match op {
        VersionOp::Eq => c == std::cmp::Ordering::Equal,
        VersionOp::Ge => c != std::cmp::Ordering::Less,
        VersionOp::Le => c != std::cmp::Ordering::Greater,
        VersionOp::Gt => c == std::cmp::Ordering::Greater,
        VersionOp::Lt => c == std::cmp::Ordering::Less,
    }
}
