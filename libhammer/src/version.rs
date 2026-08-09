use std::cmp::Ordering;

// ─────────────────────────────────────────────────────────────
//  Public API
// ─────────────────────────────────────────────────────────────

/// Compare two Debian version strings.
///
/// Returns `Ordering::Less | Equal | Greater`.
///
/// # Example
/// ```
/// use std::cmp::Ordering;
/// use libhammer::version::version_cmp;
/// assert_eq!(version_cmp("1.0~rc1", "1.0"), Ordering::Less);
/// assert_eq!(version_cmp("2:1.0",   "1:9"), Ordering::Greater);
/// ```
pub fn version_cmp(a: &str, b: &str) -> Ordering {
    let (ea, ua, ra) = split_version(a);
    let (eb, ub, rb) = split_version(b);

    // Epoch
    let epoch_a: u64 = ea.parse().unwrap_or(0);
    let epoch_b: u64 = eb.parse().unwrap_or(0);
    match epoch_a.cmp(&epoch_b) {
        Ordering::Equal => {}
        other           => return other,
    }

    // Upstream version
    match compare_version_string(ua, ub) {
        Ordering::Equal => {}
        other           => return other,
    }

    // Debian revision
    compare_version_string(ra.unwrap_or("0"), rb.unwrap_or("0"))
}

/// Returns `true` if `installed` satisfies the constraint `op required`.
///
/// # Example
/// ```
/// use libhammer::version::version_satisfies;
/// assert!(version_satisfies("3.1", ">=", "3.0"));
/// assert!(!version_satisfies("2.9", ">=", "3.0"));
/// ```
pub fn version_satisfies(installed: &str, op: &str, required: &str) -> bool {
    let ord = version_cmp(installed, required);
    match op {
        ">=" | "ge" => ord != Ordering::Less,
        "<=" | "le" => ord != Ordering::Greater,
        "="  | "eq" => ord == Ordering::Equal,
        ">>" | "gt" | ">" => ord == Ordering::Greater,
        "<<" | "lt" | "<" => ord == Ordering::Less,
        _            => false,
    }
}

// ─────────────────────────────────────────────────────────────
//  Internals
// ─────────────────────────────────────────────────────────────

/// Split into (epoch, upstream, revision).
fn split_version(v: &str) -> (&str, &str, Option<&str>) {
    // Epoch: optional leading `N:`
    let (epoch, rest) = if let Some(pos) = v.find(':') {
        (&v[..pos], &v[pos+1..])
    } else {
        ("0", v)
    };
    // Revision: optional trailing `-N`
    let (upstream, revision) = if let Some(pos) = rest.rfind('-') {
        (&rest[..pos], Some(&rest[pos+1..]))
    } else {
        (rest, None)
    };
    (epoch, upstream, revision)
}

/// Compare two version strings using Debian ordering rules.
/// Alternates between non-digit (alphabetical with special rules)
/// and digit (numeric) segments.
fn compare_version_string(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();

    loop {
        // Non-digit segment
        let (na, _ra) = take_nondigit(&mut ai);
        let (nb, _rb) = take_nondigit(&mut bi);
        match compare_nondigit_segs(&na, &nb) {
            Ordering::Equal => {}
            other           => return other,
        }

        // Digit segment
        let (da, _) = take_digit(&mut ai);
        let (db, _) = take_digit(&mut bi);
        let na: u64 = da.parse().unwrap_or(0);
        let nb: u64 = db.parse().unwrap_or(0);
        match na.cmp(&nb) {
            Ordering::Equal => {}
            other           => return other,
        }

        if ai.peek().is_none() && bi.peek().is_none() { return Ordering::Equal; }
    }
}

fn take_nondigit(iter: &mut std::iter::Peekable<std::str::Chars<'_>>) -> (String, ()) {
    let mut s = String::new();
    while let Some(&c) = iter.peek() {
        if c.is_ascii_digit() { break; }
        s.push(c);
        iter.next();
    }
    (s, ())
}

fn take_digit(iter: &mut std::iter::Peekable<std::str::Chars<'_>>) -> (String, ()) {
    let mut s = String::new();
    while let Some(&c) = iter.peek() {
        if !c.is_ascii_digit() { break; }
        s.push(c);
        iter.next();
    }
    (s, ())
}

/// Debian ordering for single non-digit characters:
///   `~` < (end of string) < digits < letters alphabetically < other
fn char_order(c: Option<char>) -> i32 {
    match c {
        None        => 0,
        Some('~')   => -1,
        Some(ch) if ch.is_ascii_alphabetic() => ch as i32,
        Some(ch)    => ch as i32 + 256,
    }
}

fn compare_nondigit_segs(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars();
    let mut bi = b.chars();
    loop {
        let ca = ai.next();
        let cb = bi.next();
        if ca.is_none() && cb.is_none() { return Ordering::Equal; }
        let oa = char_order(ca);
        let ob = char_order(cb);
        match oa.cmp(&ob) {
            Ordering::Equal => {}
            other           => return other,
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use Ordering::*;

    #[test]
    fn basic_numeric() {
        assert_eq!(version_cmp("1.0", "2.0"), Less);
        assert_eq!(version_cmp("2.0", "1.0"), Greater);
        assert_eq!(version_cmp("1.0", "1.0"), Equal);
    }

    #[test]
    fn epoch_wins() {
        assert_eq!(version_cmp("2:1.0", "1:9.9"), Greater);
        assert_eq!(version_cmp("1:1.0", "2:0.1"), Less);
    }

    #[test]
    fn tilde_sorts_before() {
        assert_eq!(version_cmp("1.0~rc1", "1.0"), Less);
        assert_eq!(version_cmp("1.0~beta", "1.0~alpha"), Greater);
    }

    #[test]
    fn revision_compared() {
        assert_eq!(version_cmp("1.0-1", "1.0-2"), Less);
        assert_eq!(version_cmp("1.0-10", "1.0-9"), Greater);
    }

    #[test]
    fn satisfies_operators() {
        assert!(version_satisfies("3.1", ">=", "3.0"));
        assert!(version_satisfies("3.0", ">=", "3.0"));
        assert!(!version_satisfies("2.9", ">=", "3.0"));
        assert!(version_satisfies("3.0", "=", "3.0"));
        assert!(version_satisfies("2.9", "<<", "3.0"));
        assert!(version_satisfies("3.1", ">>", "3.0"));
    }
}
