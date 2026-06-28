use crate::package::{DepAlternative, DepGroup, VersionConstraint};

/// Parse a Debian dependency field into a list of [`DepGroup`]s.
///
/// Each group is a comma-separated entry; within a group, alternatives
/// are separated by `|`.
pub fn parse_dep_field(s: &str) -> Vec<DepGroup> {
    s.split(',')
        .filter_map(|group_str| {
            let alts: Vec<DepAlternative> = group_str
                .split('|')
                .filter_map(|alt| parse_alternative(alt.trim()))
                .collect();
            if alts.is_empty() { None }
            else               { Some(DepGroup { alternatives: alts }) }
        })
        .collect()
}

/// Parse one alternative, e.g. `libssl3 (>= 3.0) [amd64]`.
pub fn parse_alternative(s: &str) -> Option<DepAlternative> {
    let s = s.trim();
    if s.is_empty() { return None; }

    // Architecture qualifier: `[amd64]` at end
    let (s, arch) = if let Some(start) = s.find('[') {
        let end   = s.find(']').unwrap_or(s.len());
        let arch  = s[start+1..end].trim().to_string();
        let rest  = s[..start].trim();
        (rest, Some(arch))
    } else {
        (s, None)
    };

    // Version constraint: `(>= 3.0)` at end
    let (name_part, constraint) = if let Some(start) = s.find('(') {
        let end  = s.find(')').unwrap_or(s.len());
        let con  = parse_constraint(s[start+1..end].trim());
        let name = s[..start].trim();
        (name, con)
    } else {
        (s.trim(), None)
    };

    // Architecture suffix: `name:arch`
    let (name, pkg_arch) = if let Some(pos) = name_part.find(':') {
        (&name_part[..pos], Some(name_part[pos+1..].to_string()))
    } else {
        (name_part, None)
    };

    if name.is_empty() { return None; }

    Some(DepAlternative {
        name:       name.trim().to_string(),
        constraint,
        arch:       arch.or(pkg_arch),
    })
}

fn parse_constraint(s: &str) -> Option<VersionConstraint> {
    // Formats: ">= 3.0"  "= 1.0"  ">> 2"  "<< 4"  "<= 5"
    for op in &[">=", "<=", ">>", "<<", "="] {
        if let Some(rest) = s.strip_prefix(op) {
            let version = rest.trim().to_string();
            if !version.is_empty() {
                return Some(VersionConstraint { op: op.to_string(), version });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_dep() {
        let gs = parse_dep_field("libssl3");
        assert_eq!(gs.len(), 1);
        assert_eq!(gs[0].alternatives[0].name, "libssl3");
        assert!(gs[0].alternatives[0].constraint.is_none());
    }

    #[test]
    fn versioned_dep() {
        let gs = parse_dep_field("libssl3 (>= 3.0)");
        let c  = gs[0].alternatives[0].constraint.as_ref().unwrap();
        assert_eq!(c.op,      ">=");
        assert_eq!(c.version, "3.0");
    }

    #[test]
    fn or_group() {
        let gs = parse_dep_field("libcurl4 | libcurl3");
        assert_eq!(gs[0].alternatives.len(), 2);
        assert_eq!(gs[0].alternatives[0].name, "libcurl4");
        assert_eq!(gs[0].alternatives[1].name, "libcurl3");
    }

    #[test]
    fn multi_group() {
        let gs = parse_dep_field("libssl3, libz1g | zlib1g");
        assert_eq!(gs.len(), 2);
        assert_eq!(gs[0].alternatives[0].name, "libssl3");
        assert_eq!(gs[1].alternatives.len(), 2);
    }

    #[test]
    fn arch_qualifier() {
        let gs = parse_dep_field("libfoo [amd64]");
        assert_eq!(gs[0].alternatives[0].arch, Some("amd64".into()));
    }

    #[test]
    fn empty_string() {
        assert_eq!(parse_dep_field("").len(), 0);
    }

    #[test]
    fn whitespace_only() {
        assert_eq!(parse_dep_field("   ").len(), 0);
    }
}
