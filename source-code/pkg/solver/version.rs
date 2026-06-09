use std::cmp::Ordering;

pub fn compare(a: &str, b: &str) -> Ordering {
    let va = DebVersion::parse(a);
    let vb = DebVersion::parse(b);
    va.cmp_deb(&vb)
}

pub fn satisfies(installed_ver: &str, op: &str, constraint_ver: &str) -> bool {
    let ord = compare(installed_ver, constraint_ver);
    match op {
        "<<" | "<"  => ord == Ordering::Less,
        ">>" | ">"  => ord == Ordering::Greater,
        "<=" | "=<" => ord != Ordering::Greater,
        ">=" | "=>" => ord != Ordering::Less,
        "="         => ord == Ordering::Equal,
        _           => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebVersion {
    pub epoch:    u32,
    pub upstream: String,
    pub revision: String,
}

impl DebVersion {
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        let (epoch, rest) = if let Some(colon) = s.find(':') {
            let ep = s[..colon].parse::<u32>().unwrap_or(0);
            (ep, &s[colon+1..])
        } else {
            (0u32, s)
        };
        let (upstream, revision) = if let Some(dash) = rest.rfind('-') {
            (&rest[..dash], &rest[dash+1..])
        } else {
            (rest, "")
        };
        DebVersion {
            epoch,
            upstream: upstream.to_string(),
            revision: revision.to_string(),
        }
    }

    pub fn cmp_deb(&self, other: &DebVersion) -> Ordering {
        let ec = self.epoch.cmp(&other.epoch);
        if ec != Ordering::Equal { return ec; }
        let uc = compare_deb_string(&self.upstream, &other.upstream);
        if uc != Ordering::Equal { return uc; }
        compare_deb_string(&self.revision, &other.revision)
    }
}

impl PartialOrd for DebVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

impl Ord for DebVersion {
    fn cmp(&self, other: &Self) -> Ordering { self.cmp_deb(other) }
}

fn compare_deb_string(a: &str, b: &str) -> Ordering {
    let mut ai = 0usize;
    let mut bi = 0usize;
    let ab: Vec<char> = a.chars().collect();
    let bb: Vec<char> = b.chars().collect();
    loop {
        let a_non = read_non_digits(&ab, ai);
        let b_non = read_non_digits(&bb, bi);
        let cmp   = compare_non_digit_str(a_non, b_non);
        if cmp != Ordering::Equal { return cmp; }
        ai += a_non.len();
        bi += b_non.len();
        let a_dig = read_digits(&ab, ai);
        let b_dig = read_digits(&bb, bi);
        let an: u64 = if a_dig.is_empty() { 0 }
        else { a_dig.iter().collect::<String>().parse().unwrap_or(0) };
        let bn: u64 = if b_dig.is_empty() { 0 }
        else { b_dig.iter().collect::<String>().parse().unwrap_or(0) };
        let cmp = an.cmp(&bn);
        if cmp != Ordering::Equal { return cmp; }
        ai += a_dig.len();
        bi += b_dig.len();
        if ai >= ab.len() && bi >= bb.len() { return Ordering::Equal; }
    }
}

fn read_non_digits<'a>(s: &'a [char], from: usize) -> &'a [char] {
    let start = from;
    let mut i = from;
    while i < s.len() && !s[i].is_ascii_digit() { i += 1; }
    &s[start..i]
}

fn read_digits<'a>(s: &'a [char], from: usize) -> &'a [char] {
    let start = from;
    let mut i = from;
    while i < s.len() && s[i].is_ascii_digit() { i += 1; }
    &s[start..i]
}

fn deb_char_order(c: Option<char>) -> i32 {
    match c {
        None       => 0,
        Some('~')  => -1,
        Some(ch) if ch.is_alphabetic() => ch as i32,
        Some(ch)   => 256 + ch as i32,
    }
}

fn compare_non_digit_str(a: &[char], b: &[char]) -> Ordering {
    let len = a.len().max(b.len());
    for i in 0..len {
        let ao = deb_char_order(a.get(i).copied());
        let bo = deb_char_order(b.get(i).copied());
        match ao.cmp(&bo) {
            Ordering::Equal => continue,
            other           => return other,
        }
    }
    Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic() {
        assert_eq!(compare("1.0", "1.0"), Ordering::Equal);
        assert_eq!(compare("1.1", "1.0"), Ordering::Greater);
        assert_eq!(compare("0.9", "1.0"), Ordering::Less);
    }

    #[test]
    fn test_epoch() {
        assert_eq!(compare("2:1.0", "1:9.0"), Ordering::Greater);
        assert_eq!(compare("1.0",   "1:0.1"), Ordering::Less);
    }

    #[test]
    fn test_tilde() {
        assert_eq!(compare("1.0~rc1", "1.0"),     Ordering::Less);
        assert_eq!(compare("1.0~rc2", "1.0~rc1"), Ordering::Greater);
        assert_eq!(compare("1.0",     "1.0~rc1"), Ordering::Greater);
    }

    #[test]
    fn test_satisfies() {
        assert!(satisfies("2.0",  ">=", "1.5"));
        assert!(satisfies("1.0",  "=",  "1.0"));
        assert!(!satisfies("1.0", ">",  "1.0"));
        assert!(satisfies("1.0",  "<=", "1.0"));
        assert!(satisfies("0.9",  "<<", "1.0"));
        assert!(satisfies("1.1",  ">>", "1.0"));
    }
}
