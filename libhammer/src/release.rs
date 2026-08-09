use crate::control;
use std::collections::HashMap;

/// One entry in a `Release` file's checksum listing (`MD5Sum:`, `SHA1:`,
/// or `SHA256:` field) — one line per published index file, e.g.
/// `main/binary-amd64/Packages`, `main/binary-amd64/Packages.gz`, etc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseFileEntry {
    /// Hex-encoded checksum, as published (lowercase, no `0x` prefix).
    pub checksum: String,
    /// Size in bytes, as published.
    pub size: u64,
    /// Path relative to the repository suite root, e.g.
    /// `main/binary-amd64/Packages.xz`.
    pub path: String,
}

/// Parsed `Release`/`InRelease` metadata.
#[derive(Debug, Clone, Default)]
pub struct Release {
    /// `Origin:` — repository publisher name (e.g. `Debian`).
    pub origin: Option<String>,
    /// `Label:` — human-readable label.
    pub label: Option<String>,
    /// `Suite:` — e.g. `stable`, `testing`.
    pub suite: Option<String>,
    /// `Codename:` — e.g. `bookworm`, `trixie`.
    pub codename: Option<String>,
    /// `Version:` — release version, if published (e.g. `12.5`).
    pub version: Option<String>,
    /// `Date:` — publication timestamp, as a raw RFC 2822 string
    /// (parsing into a concrete date type is left to the caller to avoid
    /// forcing a `chrono`/`time` dependency on every consumer of this crate).
    pub date: Option<String>,
    /// `Architectures:` — space-separated in the source, split here.
    pub architectures: Vec<String>,
    /// `Components:` — space-separated in the source, split here.
    pub components: Vec<String>,
    /// `Description:`.
    pub description: Option<String>,
    /// Every file listed under `SHA256:`, keyed by `path` for O(1) lookup.
    /// Falls back to `SHA1:`/`MD5Sum:` (in that preference order) only if
    /// `SHA256:` is entirely absent from the file — real-world Debian/Ubuntu
    /// repositories always publish `SHA256:`.
    pub sha256_files: HashMap<String, ReleaseFileEntry>,
}

impl Release {
    /// Parses a `Release` or `InRelease` file's content. Strips PGP armor
    /// if present (does **not** verify the signature — see module docs).
    pub fn parse(content: &str) -> Self {
        let body = strip_pgp_armor(content);
        let fields = control::parse_block(body);

        let split_list = |k: &str| -> Vec<String> {
            fields
                .get(k)
                .map(|v| v.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default()
        };

        let mut release = Release {
            origin: fields.get("Origin").cloned(),
            label: fields.get("Label").cloned(),
            suite: fields.get("Suite").cloned(),
            codename: fields.get("Codename").cloned(),
            version: fields.get("Version").cloned(),
            date: fields.get("Date").cloned(),
            architectures: split_list("Architectures"),
            components: split_list("Components"),
            description: fields.get("Description").cloned(),
            sha256_files: HashMap::new(),
        };

        if let Some(block) = fields.get("SHA256") {
            release.sha256_files = parse_checksum_list(block);
        } else if let Some(block) = fields.get("SHA1").or_else(|| fields.get("MD5Sum")) {
            release.sha256_files = parse_checksum_list(block);
        }

        release
    }

    /// Looks up the expected checksum entry for a path relative to the
    /// suite root, e.g. `"main/binary-amd64/Packages.xz"`.
    pub fn file(&self, path: &str) -> Option<&ReleaseFileEntry> {
        self.sha256_files.get(path)
    }

    /// True if `arch` (e.g. `"amd64"`) is listed in `Architectures:`.
    pub fn supports_arch(&self, arch: &str) -> bool {
        self.architectures.iter().any(|a| a == arch)
    }
}

/// The `SHA256:`/`SHA1:`/`MD5Sum:` field body is itself a list, one entry
/// per line, indented by one space:
/// ```text
///  <checksum> <size> <path>
/// ```
fn parse_checksum_list(block: &str) -> HashMap<String, ReleaseFileEntry> {
    let mut out = HashMap::new();
    for line in block.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        let mut parts = line.split_whitespace();
        let (Some(checksum), Some(size_s), Some(path)) =
            (parts.next(), parts.next(), parts.next()) else { continue };
        let Ok(size) = size_s.parse::<u64>() else { continue };
        out.insert(
            path.to_string(),
            ReleaseFileEntry { checksum: checksum.to_string(), size, path: path.to_string() },
        );
    }
    out
}

/// Strips `-----BEGIN PGP SIGNED MESSAGE-----`/`Hash: ...`/
/// `-----BEGIN PGP SIGNATURE-----...-----END PGP SIGNATURE-----` armor from
/// an `InRelease` file, returning just the control-file body. Content
/// without any armor (a plain `Release` file) is returned unchanged.
fn strip_pgp_armor(content: &str) -> &str {
    const BEGIN: &str = "-----BEGIN PGP SIGNED MESSAGE-----";
    const SIG:   &str = "-----BEGIN PGP SIGNATURE-----";

    let body_start = match content.find(BEGIN) {
        Some(idx) => {
            // Skip the BEGIN line and any "Hash: ..." header lines that
            // follow it, up to the first blank line.
            let after_begin = &content[idx + BEGIN.len()..];
            match after_begin.find("\n\n") {
                Some(blank) => idx + BEGIN.len() + blank + 2,
                None => idx + BEGIN.len(),
            }
        }
        None => 0,
    };
    let body = &content[body_start..];
    match body.find(SIG) {
        Some(sig_idx) => body[..sig_idx].trim_end(),
        None => body.trim_end(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
Origin: Debian
Label: Debian
Suite: stable
Codename: bookworm
Architectures: amd64 arm64
Components: main contrib non-free
Description: Debian 12.5 Released 10 February 2024
SHA256:
 abcdef0123456789 1234 main/binary-amd64/Packages
 fedcba9876543210 5678 main/binary-amd64/Packages.xz
";

    #[test]
    fn parses_basic_fields() {
        let rel = Release::parse(SAMPLE);
        assert_eq!(rel.origin.as_deref(), Some("Debian"));
        assert_eq!(rel.codename.as_deref(), Some("bookworm"));
        assert_eq!(rel.architectures, vec!["amd64", "arm64"]);
        assert_eq!(rel.components, vec!["main", "contrib", "non-free"]);
        assert!(rel.supports_arch("amd64"));
        assert!(!rel.supports_arch("riscv64"));
    }

    #[test]
    fn parses_checksum_list() {
        let rel = Release::parse(SAMPLE);
        let entry = rel.file("main/binary-amd64/Packages").expect("entry present");
        assert_eq!(entry.checksum, "abcdef0123456789");
        assert_eq!(entry.size, 1234);
    }

    #[test]
    fn strips_inrelease_pgp_armor() {
        let signed = format!(
            "-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA512\n\n{}\n-----BEGIN PGP SIGNATURE-----\nabc123\n-----END PGP SIGNATURE-----\n",
            SAMPLE
        );
        let rel = Release::parse(&signed);
        assert_eq!(rel.codename.as_deref(), Some("bookworm"));
    }
}
