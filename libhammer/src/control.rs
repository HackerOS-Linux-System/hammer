use indexmap::IndexMap;

/// One `Key: Value` block, in the order fields appeared in the source text.
pub type Fields = IndexMap<String, String>;

/// Parses a single control-file block (no blank lines inside it) into an
/// ordered map of fields. Folded continuation lines (starting with a space
/// or tab) are un-folded and appended to the previous field's value,
/// joined with `\n` — this matches how `dpkg`/`apt` treat multi-line
/// `Description:`-style fields. A continuation line containing only a
/// single `.` is treated as an explicit blank line within the field
/// (the standard convention for blank paragraph separators in long
/// descriptions).
pub fn parse_block(block: &str) -> Fields {
    let mut fields = Fields::new();
    let mut current_key: Option<String> = None;

    for line in block.lines() {
        if line.is_empty() {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            let Some(key) = &current_key else { continue };
            let cont = line.trim_start();
            let cont = if cont == "." { "" } else { cont };
            if let Some(v) = fields.get_mut(key) {
                v.push('\n');
                v.push_str(cont);
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_string();
            let v = v.trim().to_string();
            fields.insert(k.clone(), v);
            current_key = Some(k);
        }
    }

    fields
}

/// Parses a multi-block control file (blocks separated by one or more
/// blank lines) — the format of `Packages`, `/var/lib/dpkg/status`, and
/// `Sources` files. Empty blocks (extra blank lines) are skipped.
pub fn parse_blocks(content: &str) -> Vec<Fields> {
    content
        .split("\n\n")
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .map(parse_block)
        .collect()
}

/// Serialises `fields` back into control-file format, preserving insertion
/// order. Multi-line values (containing `\n`) are folded back with a
/// leading space on continuation lines, and empty continuation lines are
/// written as a single `.` — the inverse of [`parse_block`]'s folding.
pub fn write_block(fields: &Fields) -> String {
    let mut out = String::new();
    for (k, v) in fields {
        let mut lines = v.split('\n');
        if let Some(first) = lines.next() {
            out.push_str(k);
            out.push_str(": ");
            out.push_str(first);
            out.push('\n');
        }
        for cont in lines {
            out.push(' ');
            if cont.is_empty() {
                out.push('.');
            } else {
                out.push_str(cont);
            }
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_continuation_lines() {
        let block = "Package: curl\nDescription: short\n long line one\n .\n long line two\n";
        let fields = parse_block(block);
        assert_eq!(fields.get("Package").unwrap(), "curl");
        assert_eq!(
            fields.get("Description").unwrap(),
            "short\nlong line one\n\nlong line two"
        );
    }

    #[test]
    fn splits_multiple_blocks() {
        let content = "Package: a\nVersion: 1\n\nPackage: b\nVersion: 2\n";
        let blocks = parse_blocks(content);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].get("Package").unwrap(), "a");
        assert_eq!(blocks[1].get("Package").unwrap(), "b");
    }

    #[test]
    fn roundtrips_simple_fields() {
        let mut fields = Fields::new();
        fields.insert("Package".to_string(), "curl".to_string());
        fields.insert("Version".to_string(), "8.5.0-2".to_string());
        let text = write_block(&fields);
        let reparsed = parse_block(&text);
        assert_eq!(fields, reparsed);
    }
}
