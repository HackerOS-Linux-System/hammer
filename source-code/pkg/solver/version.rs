use std::cmp::Ordering;

// ──────────────────────────────────────────────────────────────────────────────
//  VersionReq
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionReq {
    pub op:      String,
    pub version: String,
}

impl VersionReq {
    pub fn new(op: impl Into<String>, version: impl Into<String>) -> Self {
        VersionReq { op: op.into(), version: version.into() }
    }

    /// Check whether `ver` satisfies this requirement.
    pub fn satisfies(&self, ver: &str) -> bool {
        satisfies(ver, &self.op, &self.version)
    }

    /// Parse `"(>= 1.2)"` or `">= 1.2"` style strings.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().trim_matches(|c| c == '(' || c == ')').trim();
        for op in &[">=", "<=", ">>", "<<", "!=", "=", ">", "<"] {
            if let Some(rest) = s.strip_prefix(op) {
                return Some(VersionReq {
                    op:      op.to_string(),
                    version: rest.trim().to_string(),
                });
            }
        }
        None
    }
}

impl std::fmt::Display for VersionReq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({} {})", self.op, self.version)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
//  Public API
// ──────────────────────────────────────────────────────────────────────────────

/// Compare two Debian version strings. Returns Ordering.
pub fn compare(a: &str, b: &str) -> Ordering {
    let (ea, ua) = split_epoch(a);
    let (eb, ub) = split_epoch(b);

    // 1. Compare epochs (numeric)
    match ea.cmp(&eb) {
        Ordering::Equal => {}
        other => return other,
    }

    // 2. Split upstream+revision
    let (ua_up, ua_rev) = split_revision(ua);
    let (ub_up, ub_rev) = split_revision(ub);

    // 3. Compare upstream version
    match compare_deb_str(ua_up, ub_up) {
        Ordering::Equal => {}
        other => return other,
    }

    // 4. Compare Debian revision (empty = "0")
    compare_deb_str(
        if ua_rev.is_empty() { "0" } else { ua_rev },
        if ub_rev.is_empty() { "0" } else { ub_rev },
    )
}

/// Check `installed_ver <op> constraint_ver`.
pub fn satisfies(installed: &str, op: &str, constraint: &str) -> bool {
    let ord = compare(installed, constraint);
    match op {
        "="  | "==" => ord == Ordering::Equal,
        "!=" | "<>" => ord != Ordering::Equal,
        "<<" | "<"  => ord == Ordering::Less,
        "<=" | "=<" => ord != Ordering::Greater,
        ">>" | ">"  => ord == Ordering::Greater,
        ">=" | "=>" => ord != Ordering::Less,
        _           => false,
    }
}

/// Parse `"2:1.0-1"` → `(2, "1.0-1")`.
fn split_epoch(v: &str) -> (u32, &str) {
    if let Some(idx) = v.find(':') {
        let epoch: u32 = v[..idx].parse().unwrap_or(0);
        (epoch, &v[idx + 1..])
    } else {
        (0, v)
    }
}

/// Split `"1.0-1"` → `("1.0", "1")`.  No `-` → revision = `""`.
fn split_revision(v: &str) -> (&str, &str) {
    // Only the LAST `-` is the revision separator
    if let Some(idx) = v.rfind('-') {
        (&v[..idx], &v[idx + 1..])
    } else {
        (v, "")
    }
}

// ──────────────────────────────────────────────────────────────────────────────
//  Core dpkg string comparison
// ──────────────────────────────────────────────────────────────────────────────

/// Compare two version component strings using dpkg's alternating
/// non-digit / digit segment algorithm with tilde support.
fn compare_deb_str(a: &str, b: &str) -> Ordering {
    let mut ai = a.as_bytes();
    let mut bi = b.as_bytes();

    loop {
        // Non-digit segment
        let (a_alpha, a_rest) = take_non_digit(ai);
        let (b_alpha, b_rest) = take_non_digit(bi);
        match compare_alpha_segment(a_alpha, b_alpha) {
            Ordering::Equal => {}
            other => return other,
        }
        ai = a_rest;
        bi = b_rest;

        // Digit segment
        let (a_num, a_rest2) = take_digit(ai);
        let (b_num, b_rest2) = take_digit(bi);
        match compare_numeric_segment(a_num, b_num) {
            Ordering::Equal => {}
            other => return other,
        }
        ai = a_rest2;
        bi = b_rest2;

        if ai.is_empty() && bi.is_empty() { return Ordering::Equal; }
    }
}

/// Split bytes into leading non-digit run and remainder.
fn take_non_digit(s: &[u8]) -> (&[u8], &[u8]) {
    let n = s.iter().take_while(|&&c| !c.is_ascii_digit()).count();
    (&s[..n], &s[n..])
}

/// Split bytes into leading digit run and remainder.
fn take_digit(s: &[u8]) -> (&[u8], &[u8]) {
    let n = s.iter().take_while(|&&c| c.is_ascii_digit()).count();
    (&s[..n], &s[n..])
}

/// dpkg's character order for the non-digit segment:
///   `~` < end-of-string < letters < everything-else (sorted by ASCII)
fn char_order(c: u8) -> i32 {
    match c {
        b'~'                   => -1,
        b if b.is_ascii_alphabetic() => b as i32,
        b                      => (b as i32) + 256, // non-alpha sorts after alpha
    }
}

/// Compare two non-digit (alpha/special) segments character by character.
fn compare_alpha_segment(a: &[u8], b: &[u8]) -> Ordering {
    let mut ai = a.iter().peekable();
    let mut bi = b.iter().peekable();
    loop {
        match (ai.next(), bi.next()) {
            (None, None)           => return Ordering::Equal,
            (Some(&ac), None)      => {
                if ac == b'~' { return Ordering::Less; }
                return Ordering::Greater;
            }
            (None, Some(&bc))      => {
                if bc == b'~' { return Ordering::Greater; }
                return Ordering::Less;
            }
            (Some(&ac), Some(&bc)) => {
                let oa = char_order(ac);
                let ob = char_order(bc);
                match oa.cmp(&ob) {
                    Ordering::Equal => {}
                    other => return other,
                }
            }
        }
    }
}

/// Compare two numeric (digit) segments as integers.
/// Empty string = 0.
fn compare_numeric_segment(a: &[u8], b: &[u8]) -> Ordering {
    // Trim leading zeros
    let a = trim_leading_zeros(a);
    let b = trim_leading_zeros(b);
    // Longer number = larger (since leading zeros stripped)
    match a.len().cmp(&b.len()) {
        Ordering::Equal => a.cmp(b), // same length: lexicographic = numeric
        other           => other,
    }
}

fn trim_leading_zeros(s: &[u8]) -> &[u8] {
    let n = s.iter().take_while(|&&c| c == b'0').count();
    let trimmed = &s[n..];
    if trimmed.is_empty() { &s[s.len().saturating_sub(1)..] } else { trimmed }
}

// ──────────────────────────────────────────────────────────────────────────────
//  Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use Ordering::*;

    fn cmp(a: &str, b: &str) -> Ordering { compare(a, b) }

    // Basic ordering
    #[test] fn test_simple()    { assert_eq!(cmp("1.0",   "2.0"),   Less); }
    #[test] fn test_eq()        { assert_eq!(cmp("1.0",   "1.0"),   Equal); }
    #[test] fn test_gt()        { assert_eq!(cmp("2.0",   "1.0"),   Greater); }

    // Epoch
    #[test] fn test_epoch_gt()  { assert_eq!(cmp("2:1.0", "1:9.9"), Greater); }
    #[test] fn test_epoch_eq()  { assert_eq!(cmp("1:1.0", "1:1.0"), Equal); }
    #[test] fn test_no_epoch()  { assert_eq!(cmp("1.0",   "0:1.0"), Equal); }

    // Tilde
    #[test] fn test_tilde_lt()  { assert_eq!(cmp("1.0~rc1", "1.0"),      Less); }
    #[test] fn test_tilde_lt2() { assert_eq!(cmp("1.0~rc1", "1.0~rc2"),  Less); }
    #[test] fn test_tilde_gt()  { assert_eq!(cmp("1.0",     "1.0~rc1"),  Greater); }

    // Revision
    #[test] fn test_rev()       { assert_eq!(cmp("1.0-1",  "1.0-2"),  Less); }
    #[test] fn test_rev_eq()    { assert_eq!(cmp("1.0-1",  "1.0-1"),  Equal); }
    #[test] fn test_no_rev()    { assert_eq!(cmp("1.0",    "1.0-0"),  Equal); }

    // Padding zeros
    #[test] fn test_zeros()     { assert_eq!(cmp("1.0",    "1.0.0"),  Less); }
    #[test] fn test_leading0()  { assert_eq!(cmp("1.01",   "1.1"),    Equal); }

    // satisfies
    #[test] fn test_sat_ge()    { assert!(satisfies("2.0", ">=", "1.0")); }
    #[test] fn test_sat_lt()    { assert!(!satisfies("1.0", ">>", "1.0")); }
    #[test] fn test_sat_eq()    { assert!(satisfies("1.0", "=",  "1.0")); }
    #[test] fn test_sat_ne()    { assert!(satisfies("1.1", "!=", "1.0")); }
    #[test] fn test_tilde_dep() { assert!(satisfies("1.0~beta", "<=", "1.0")); }

    // Real-world examples
    #[test] fn test_real1()     { assert_eq!(cmp("4.14.1-1",    "4.14.1-2"),  Less); }
    #[test] fn test_real2()     { assert_eq!(cmp("1:7.4.052-1", "8.0.1220-1"), Greater); }
    #[test] fn test_real3()     { assert_eq!(cmp("0.9.8zh-1",   "1.0.0-1"),   Less); }
    #[test] fn test_real4()     { assert_eq!(cmp("2.1.1+dfsg-1","2.1.1-1"),   Greater); }
}
