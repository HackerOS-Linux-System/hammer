use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::log;

pub const KEYRING_DIR: &str = "/etc/hammer/trusted.gpg.d";

// ─────────────────────────────────────────────────────────────
//  InRelease parser
// ─────────────────────────────────────────────────────────────

/// Parsed content of an InRelease file.
#[derive(Debug, Default)]
pub struct InRelease {
    pub origin:        Option<String>,
    pub suite:         Option<String>,
    pub codename:      Option<String>,
    pub date:          Option<String>,
    pub valid_until:   Option<String>,
    /// SHA256: path → (size, hash)
    pub sha256:        HashMap<String, (u64, String)>,
    /// Raw signed content (for GPG verification)
    pub raw:           String,
}

impl InRelease {
    pub fn parse(content: &str) -> Result<Self> {
        let mut ir = InRelease { raw: content.to_string(), ..Default::default() };

        // Strip GPG header/footer if present (clear-signed format)
        let body = strip_pgp_armor(content);

        let mut in_sha256 = false;
        for line in body.lines() {
            if line.starts_with("Origin:")      { ir.origin      = Some(line[7..].trim().to_string()); }
            else if line.starts_with("Suite:")  { ir.suite       = Some(line[6..].trim().to_string()); }
            else if line.starts_with("Codename:") { ir.codename  = Some(line[9..].trim().to_string()); }
            else if line.starts_with("Date:")   { ir.date        = Some(line[5..].trim().to_string()); }
            else if line.starts_with("Valid-Until:") { ir.valid_until = Some(line[12..].trim().to_string()); }
            else if line.starts_with("SHA256:") { in_sha256 = true; }
            else if in_sha256 {
                if line.starts_with(' ') || line.starts_with('\t') {
                    // Format: " <hash> <size> <path>"
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() == 3 {
                        let hash = parts[0].to_string();
                        let size = parts[1].parse::<u64>().unwrap_or(0);
                        let path = parts[2].to_string();
                        ir.sha256.insert(path, (size, hash));
                    }
                } else {
                    in_sha256 = false;
                }
            }
        }
        Ok(ir)
    }

    /// Check if a file matches the declared SHA256 hash.
    pub fn verify_file(&self, rel_path: &str, data: &[u8]) -> Result<()> {
        let (expected_size, expected_hash) = self.sha256.get(rel_path)
        .ok_or_else(|| anyhow::anyhow!(
            "File '{}' not listed in InRelease SHA256 section", rel_path
        ))?;

        let actual_size = data.len() as u64;
        if actual_size != *expected_size {
            bail!(
                "Size mismatch for '{}': expected {} bytes, got {}",
                rel_path, expected_size, actual_size
            );
        }

        let actual_hash = sha256_hex(data);
        if &actual_hash != expected_hash {
            bail!(
                "SHA256 mismatch for '{}':\n  expected: {}\n  actual:   {}",
                rel_path, expected_hash, actual_hash
            );
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────
//  GPG verification
// ─────────────────────────────────────────────────────────────

/// Verify a clear-signed InRelease file against trusted keys.
/// Returns Ok(()) if valid, Err if signature is bad or no key matches.
pub fn verify_inrelease(content: &str, keyring_dir: &Path) -> Result<()> {
    if !keyring_dir.exists() {
        log::warn("gpg: no keyring directory — skipping signature verification");
        return Ok(());
    }

    // Collect all .gpg and .asc keyring files
    let keyfiles: Vec<PathBuf> = std::fs::read_dir(keyring_dir)
    .context("Reading keyring dir")?
    .flatten()
    .map(|e| e.path())
    .filter(|p| {
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        matches!(ext, "gpg" | "asc" | "pgp")
    })
    .collect();

    if keyfiles.is_empty() {
        log::warn("gpg: keyring is empty — skipping signature verification");
        return Ok(());
    }

    // Write content to a temp file
    let tmp_content = write_temp("hammer_inrelease_", content.as_bytes())?;
    let tmp_keyring = build_combined_keyring(&keyfiles)?;

    // Run gpgv to verify
    let output = Command::new("gpgv")
    .arg("--keyring")
    .arg(&tmp_keyring)
    .arg(&tmp_content)
    .output();

    let _ = std::fs::remove_file(&tmp_content);
    let _ = std::fs::remove_file(&tmp_keyring);

    match output {
        Ok(o) if o.status.success() => {
            log::info("gpg: InRelease signature OK");
            Ok(())
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            bail!("GPG signature verification failed:\n{}", stderr.trim())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // gpgv not installed — warn but don't fail (for development systems)
            log::warn("gpg: gpgv not found — install gnupg for signature verification");
            Ok(())
        }
        Err(e) => bail!("Failed to run gpgv: {}", e),
    }
}

// ─────────────────────────────────────────────────────────────
//  .deb file verification
// ─────────────────────────────────────────────────────────────

/// Verify a downloaded .deb file against the SHA256 declared in the package index.
pub fn verify_deb(path: &Path, expected_sha256: &str) -> Result<()> {
    let data = std::fs::read(path)
    .with_context(|| format!("Reading {}", path.display()))?;
    let actual = sha256_hex(&data);
    if actual != expected_sha256 {
        bail!(
            "SHA256 mismatch for {}:\n  expected: {}\n  actual:   {}",
            path.display(), expected_sha256, actual
        );
    }
    Ok(())
}

/// Verify a downloaded index file (Packages, Packages.gz etc.) against InRelease.
pub fn verify_index_file(rel_path: &str, data: &[u8], inrelease: &InRelease) -> Result<()> {
    inrelease.verify_file(rel_path, data)
}

// ─────────────────────────────────────────────────────────────
//  Key import
// ─────────────────────────────────────────────────────────────

/// Import a GPG key from bytes into the hammer keyring.
/// Accepts armored (.asc) or binary (.gpg) key data.
pub fn import_key_bytes(key_data: &[u8], key_name: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(KEYRING_DIR)?;

    // Determine extension
    let ext = if key_data.starts_with(b"-----") { "asc" } else { "gpg" };
    let safe_name = key_name.chars()
    .map(|c| if c.is_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
    .collect::<String>();
    let dest = PathBuf::from(KEYRING_DIR).join(format!("{}.{}", safe_name, ext));

    // Convert .asc to binary .gpg if gpg is available
    if ext == "asc" {
        let tmp = write_temp("hammer_key_import_", key_data)?;
        let out = Command::new("gpg")
        .args(["--dearmor", "--output"])
        .arg(&dest)
        .arg(&tmp)
        .output();
        let _ = std::fs::remove_file(&tmp);

        match out {
            Ok(o) if o.status.success() => {
                log::info(&format!("gpg: imported key to {}", dest.display()));
                return Ok(dest);
            }
            _ => {
                // gpg not available — save armored directly
                let asc_dest = PathBuf::from(KEYRING_DIR).join(format!("{}.asc", safe_name));
                std::fs::write(&asc_dest, key_data)?;
                log::info(&format!("gpg: saved armored key to {}", asc_dest.display()));
                return Ok(asc_dest);
            }
        }
    }

    std::fs::write(&dest, key_data)?;
    log::info(&format!("gpg: imported key to {}", dest.display()));
    Ok(dest)
}

// ─────────────────────────────────────────────────────────────
//  Key info extraction
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct KeyInfo {
    pub fingerprint: String,
    pub name:        String,
    pub email:       Option<String>,
    pub key_id:      String,
}

pub fn read_key_info(key_path: &Path) -> Result<KeyInfo> {
    let output = Command::new("gpg")
    .args(["--with-colons", "--import-options", "show-only", "--import"])
    .arg(key_path)
    .output();

    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            parse_gpg_colons(&stdout)
        }
        Err(_) => {
            // gpg not available — return placeholder
            let filename = key_path.file_stem()
            .and_then(|s| s.to_str()).unwrap_or("unknown");
            Ok(KeyInfo {
                fingerprint: "unavailable".to_string(),
               name:        filename.to_string(),
               email:       None,
               key_id:      "unavailable".to_string(),
            })
        }
    }
}

fn parse_gpg_colons(output: &str) -> Result<KeyInfo> {
    let mut fingerprint = String::new();
    let mut name        = String::new();
    let mut email       = None;
    let mut key_id      = String::new();

    for line in output.lines() {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() < 2 { continue; }
        match parts[0] {
            "pub" | "sec" => {
                if parts.len() > 4 { key_id = parts[4].to_string(); }
            }
            "fpr" => {
                if parts.len() > 9 { fingerprint = parts[9].to_string(); }
            }
            "uid" => {
                if parts.len() > 9 {
                    let uid = parts[9];
                    // UID format: "Name (comment) <email>"
                    if let Some(lt) = uid.find('<') {
                        name  = uid[..lt].trim().trim_end_matches(')').trim().to_string();
                        if let Some(gt) = uid.find('>') {
                            email = Some(uid[lt+1..gt].to_string());
                        }
                    } else {
                        name = uid.to_string();
                    }
                }
            }
            _ => {}
        }
    }

    if fingerprint.is_empty() && key_id.is_empty() {
        bail!("Could not parse GPG key info");
    }

    Ok(KeyInfo { fingerprint, name, email, key_id })
}

// ─────────────────────────────────────────────────────────────
//  Helpers
// ─────────────────────────────────────────────────────────────

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

fn strip_pgp_armor(content: &str) -> &str {
    // Clear-signed format:
    //   -----BEGIN PGP SIGNED MESSAGE-----
    //   Hash: SHA256
    //   <blank line>
    //   <body>
    //   -----BEGIN PGP SIGNATURE-----
    //   ...
    //   -----END PGP SIGNATURE-----
    if let Some(start) = content.find("\n\n") {
        if let Some(end) = content.find("-----BEGIN PGP SIGNATURE-----") {
            return &content[start+2..end];
        }
    }
    content
}

fn write_temp(prefix: &str, data: &[u8]) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("{}{}", prefix, std::process::id()));
    std::fs::write(&path, data)?;
    Ok(path)
}

fn build_combined_keyring(keyfiles: &[PathBuf]) -> Result<PathBuf> {
    // gpgv requires a combined keyring file
    let dest = std::env::temp_dir().join(format!("hammer_keyring_{}.gpg", std::process::id()));
    let mut combined = Vec::new();
    for kf in keyfiles {
        if let Ok(data) = std::fs::read(kf) {
            // If .asc, dearmor first
            if kf.extension().and_then(|e| e.to_str()) == Some("asc") {
                let tmp = write_temp("hammer_dearmor_", &data)?;
                if let Ok(out) = Command::new("gpg").args(["--dearmor"]).arg(&tmp).output() {
                    if out.status.success() { combined.extend_from_slice(&out.stdout); }
                }
                let _ = std::fs::remove_file(&tmp);
            } else {
                combined.extend_from_slice(&data);
            }
        }
    }
    std::fs::write(&dest, &combined)?;
    Ok(dest)
}
