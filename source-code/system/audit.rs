use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use owo_colors::OwoColorize;
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

pub const AUDIT_LOG:  &str = "/hammer/db/audit.jsonl";
pub const AUDIT_PUB:  &str = "/etc/hammer/audit-key.pub";
pub const AUDIT_PRIV: &str = "/etc/hammer/audit-key.priv";

// ─────────────────────────────────────────────────────────────
//  AuditEntry
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub seq:        u64,
    pub timestamp:  String,
    pub action:     String,
    pub packages:   Vec<AuditPackage>,
    pub gen_before: Option<u32>,
    pub gen_after:  Option<u32>,
    pub gen_hash:   Option<String>,
    pub uid:        u32,
    pub prev_hash:  String,
    /// Ed25519 signature, hex-encoded (64 bytes -> 128 hex chars).
    /// Empty if no signing key available.
    pub signature:  String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditPackage {
    pub name:        String,
    pub old_version: Option<String>,
    pub new_version: Option<String>,
}

// ─────────────────────────────────────────────────────────────
//  AuditLog
// ─────────────────────────────────────────────────────────────

pub struct AuditLog;

impl AuditLog {
    // ── Append a new entry ────────────────────────────────────

    pub fn record(
        action:     &str,
        packages:   &[AuditPackage],
        gen_before: Option<u32>,
        gen_after:  Option<u32>,
    ) -> Result<()> {
        std::fs::create_dir_all("/hammer/db")?;

        let prev_hash = Self::last_entry_hash()?;
        let gen_hash  = gen_after.and_then(|g| crate::gpg::hash_generation(g).ok());
        let uid       = unsafe { libc::getuid() };
        let seq       = Self::next_seq()?;

        let mut entry = AuditEntry {
            seq,
            timestamp:  chrono::Utc::now().to_rfc3339(),
            action:     action.to_string(),
            packages:   packages.to_vec(),
            gen_before,
            gen_after,
            gen_hash,
            uid,
            prev_hash:  prev_hash.clone(),
            signature:  String::new(),
        };

        if Path::new(AUDIT_PRIV).exists() {
            match Self::sign_entry(&entry, &prev_hash) {
                Ok(sig) => entry.signature = sig,
                Err(e)  => crate::log::warn(&format!(
                    "audit: signing failed: {} (entry recorded unsigned)", e
                )),
            }
        }

        let line = serde_json::to_string(&entry)
        .context("Serialising audit entry")?;
        let mut file = std::fs::OpenOptions::new()
        .create(true).append(true)
        .open(AUDIT_LOG)
        .context("Opening audit log")?;
        use std::io::Write;
        writeln!(file, "{}", line)?;

        crate::log::info(&format!(
            "audit: seq={} action={} gen={:?}",
            seq, action, gen_after
        ));
        Ok(())
    }

    fn last_entry_hash() -> Result<String> {
        if !Path::new(AUDIT_LOG).exists() {
            return Ok(sha256_hex(b"genesis"));
        }
        let content = std::fs::read_to_string(AUDIT_LOG)?;
        let last_line = content.lines().filter(|l| !l.is_empty()).last().unwrap_or("");
        if last_line.is_empty() {
            return Ok(sha256_hex(b"genesis"));
        }
        Ok(sha256_hex(last_line.as_bytes()))
    }

    fn next_seq() -> Result<u64> {
        if !Path::new(AUDIT_LOG).exists() { return Ok(1); }
        let content = std::fs::read_to_string(AUDIT_LOG)?;
        let count = content.lines().filter(|l| !l.is_empty()).count();
        Ok(count as u64 + 1)
    }

    // ── Ed25519 signing ────────────────────────────────────────

    /// Build the signing payload: sha256(entry_json_without_sig || "|" || prev_hash)
    fn signing_payload(entry: &AuditEntry, prev_hash: &str) -> Result<[u8; 32]> {
        let mut tmp = entry.clone();
        tmp.signature = String::new();
        let json    = serde_json::to_string(&tmp)?;
        let payload = format!("{}|{}", json, prev_hash);
        let mut h = Sha256::new();
        h.update(payload.as_bytes());
        Ok(h.finalize().into())
    }

    fn load_signing_key() -> Result<SigningKey> {
        let hex_str = std::fs::read_to_string(AUDIT_PRIV)
        .context("Reading audit private key")?;
        let bytes = hex::decode(hex_str.trim())
        .context("Decoding private key hex")?;
        if bytes.len() != 32 {
            bail!("Audit private key must be 32 bytes (Ed25519 seed), got {}", bytes.len());
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        Ok(SigningKey::from_bytes(&seed))
    }

    fn load_verifying_key() -> Result<VerifyingKey> {
        let hex_str = std::fs::read_to_string(AUDIT_PUB)
        .context("Reading audit public key")?;
        let bytes = hex::decode(hex_str.trim())
        .context("Decoding public key hex")?;
        if bytes.len() != 32 {
            bail!("Audit public key must be 32 bytes, got {}", bytes.len());
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        VerifyingKey::from_bytes(&arr).context("Invalid Ed25519 public key")
    }

    fn sign_entry(entry: &AuditEntry, prev_hash: &str) -> Result<String> {
        let payload_hash = Self::signing_payload(entry, prev_hash)?;
        let signing_key  = Self::load_signing_key()?;
        let signature: Signature = signing_key.sign(&payload_hash);
        Ok(hex::encode(signature.to_bytes()))
    }

    fn verify_entry_sig(entry: &AuditEntry, prev_hash: &str) -> bool {
        if entry.signature.is_empty() { return true; } // unsigned = skip
        if !Path::new(AUDIT_PUB).exists() { return true; } // no key = skip

        let Ok(verifying_key) = Self::load_verifying_key() else { return false; };
        let Ok(payload_hash)  = Self::signing_payload(entry, prev_hash) else { return false; };

        let sig_bytes = match hex::decode(&entry.signature) {
            Ok(b) if b.len() == 64 => b,
            _ => return false,
        };
        let mut sig_arr = [0u8; 64];
        sig_arr.copy_from_slice(&sig_bytes);
        let signature = Signature::from_bytes(&sig_arr);

        verifying_key.verify(&payload_hash, &signature).is_ok()
    }

    // ── Load all entries ──────────────────────────────────────

    pub fn load_all() -> Result<Vec<AuditEntry>> {
        if !Path::new(AUDIT_LOG).exists() { return Ok(vec![]); }
        let content = std::fs::read_to_string(AUDIT_LOG)?;
        let entries = content.lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<AuditEntry>(l).ok())
        .collect();
        Ok(entries)
    }

    // ── Verify chain ──────────────────────────────────────────

    pub fn verify_chain() -> Result<ChainVerifyResult> {
        let entries = Self::load_all()?;
        let mut result = ChainVerifyResult {
            total:        entries.len(),
            valid:        0,
            invalid_sigs: vec![],
            broken_chain: vec![],
        };

        let mut prev_hash = sha256_hex(b"genesis");

        for entry in &entries {
            let computed_prev = if entry.seq == 1 {
                sha256_hex(b"genesis")
            } else {
                prev_hash.clone()
            };

            if entry.prev_hash != computed_prev {
                result.broken_chain.push(entry.seq);
            }

            if !Self::verify_entry_sig(entry, &entry.prev_hash) {
                result.invalid_sigs.push(entry.seq);
            } else {
                result.valid += 1;
            }

            let line = serde_json::to_string(entry).unwrap_or_default();
            prev_hash = sha256_hex(line.as_bytes());
        }

        Ok(result)
    }

    // ── Keygen — real Ed25519 key pair ─────────────────────────

    pub fn keygen() -> Result<()> {
        std::fs::create_dir_all("/etc/hammer")?;

        if Path::new(AUDIT_PRIV).exists() {
            bail!(
                "Audit key already exists at {}.\n  \
Remove it manually first if you really want to regenerate \
(this invalidates verification of all prior signed entries).",
                  AUDIT_PRIV
            );
        }

        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let verifying_key: VerifyingKey = signing_key.verifying_key();

        let priv_hex = hex::encode(signing_key.to_bytes());
        let pub_hex  = hex::encode(verifying_key.to_bytes());

        std::fs::write(AUDIT_PRIV, &priv_hex)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(AUDIT_PRIV, std::fs::Permissions::from_mode(0o600))?;

        std::fs::write(AUDIT_PUB, &pub_hex)?;
        std::fs::set_permissions(AUDIT_PUB, std::fs::Permissions::from_mode(0o644))?;

        println!("  {} Ed25519 audit key pair generated:", "✔".bright_green().bold());
        println!("    Private: {} (root-only)", AUDIT_PRIV.cyan());
        println!("    Public:  {}", AUDIT_PUB.cyan());
        println!();
        println!("  {} Back up {} securely — it cannot be recovered if lost.",
                 "!".yellow().bold(), AUDIT_PRIV.cyan());
        crate::log::info("audit: generated new Ed25519 key pair");
        Ok(())
    }
}

#[derive(Debug)]
pub struct ChainVerifyResult {
    pub total:        usize,
    pub valid:        usize,
    pub invalid_sigs: Vec<u64>,
    pub broken_chain: Vec<u64>,
}

impl ChainVerifyResult {
    pub fn is_ok(&self) -> bool {
        self.invalid_sigs.is_empty() && self.broken_chain.is_empty()
    }
}

// ─────────────────────────────────────────────────────────────
//  CLI
// ─────────────────────────────────────────────────────────────

pub fn cmd_audit(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" | "ls" => cmd_audit_list(args),
        "verify"      => cmd_audit_verify(),
        "export"      => cmd_audit_export(args),
        "keygen"      => AuditLog::keygen(),
        other => anyhow::bail!(
            "Unknown audit subcommand: '{}'. Try: list, verify, export, keygen", other
        ),
    }
}

fn cmd_audit_list(args: &[String]) -> Result<()> {
    let limit: usize = args.iter()
    .find(|a| a.starts_with("-n"))
    .and_then(|a| a[2..].trim().parse().ok())
    .unwrap_or(20);

    let entries = AuditLog::load_all()?;
    let total = entries.len();

    println!();
    println!("  {}  Audit trail ({} entries total)", "⬡".bright_cyan().bold(), total);
    println!("  {}", "─".repeat(75).dimmed());
    println!("  {:<6} {:<22} {:<12} {:<36} {}",
             "Seq".bold(), "Timestamp".bold(), "Action".bold(),
             "Packages".bold(), "Gen".bold());
    println!("  {}", "─".repeat(75).dimmed());

    for entry in entries.iter().rev().take(limit) {
        let ts = entry.timestamp.chars().take(19).collect::<String>();
        let pkg_summary = entry.packages.iter()
        .take(3)
        .map(|p| p.name.as_str())
        .collect::<Vec<_>>().join(", ");
        let pkg_summary = if entry.packages.len() > 3 {
            format!("{} +{}", pkg_summary, entry.packages.len() - 3)
        } else { pkg_summary };

        let gen_str = match (entry.gen_before, entry.gen_after) {
            (Some(b), Some(a)) if b != a => format!("{} → {}", b, a),
            (_, Some(a))                  => format!("{}", a),
            _                             => String::new(),
        };

        let action_col = match entry.action.as_str() {
            "install"   => entry.action.bright_green().to_string(),
            "remove"    => entry.action.red().to_string(),
            "upgrade"   => entry.action.yellow().to_string(),
            "rollback"  => entry.action.bright_red().to_string(),
            other       => other.cyan().to_string(),
        };

        let sig_ok = if entry.signature.is_empty() { String::new() }
        else { " ✓".bright_green().to_string() };

        println!("  {:<6} {:<22} {:<20} {:<36} {}{}",
                 entry.seq.to_string().dimmed(),
                 ts.dimmed(),
                 action_col,
                 pkg_summary,
                 gen_str.cyan(),
                 sig_ok);
    }

    if total > limit {
        println!("  … {} more entries. Use -n {} to see more.", total - limit, total);
    }
    println!();
    Ok(())
}

fn cmd_audit_verify() -> Result<()> {
    println!();
    println!("  {}  Verifying audit chain integrity (Ed25519)…", "⬡".bright_cyan().bold());

    let result = AuditLog::verify_chain()?;

    println!("  {:<28} {}", "Total entries:".bold(), result.total.to_string().cyan());
    println!("  {:<28} {}", "Valid:".bold(), result.valid.to_string().bright_green());

    if result.is_ok() {
        println!();
        println!("  {} Audit chain is intact and all Ed25519 signatures are valid.",
                 "✔".bright_green().bold());
    } else {
        if !result.invalid_sigs.is_empty() {
            println!("  {} Invalid signatures at entries: {}",
                     "✗".red().bold(),
                     result.invalid_sigs.iter().map(|n| n.to_string())
                     .collect::<Vec<_>>().join(", ").red());
        }
        if !result.broken_chain.is_empty() {
            println!("  {} Broken chain links at entries: {}",
                     "✗".red().bold(),
                     result.broken_chain.iter().map(|n| n.to_string())
                     .collect::<Vec<_>>().join(", ").red());
        }
        println!();
        println!("  {} AUDIT CHAIN TAMPERED OR CORRUPTED", "WARNING:".red().bold());
    }
    println!();
    Ok(())
}

fn cmd_audit_export(args: &[String]) -> Result<()> {
    let output = args.get(1).map(|s| s.as_str()).unwrap_or("hammer-audit.json");
    let entries = AuditLog::load_all()?;
    let json = serde_json::to_string_pretty(&entries)?;
    std::fs::write(output, &json)?;
    println!("  {} Exported {} entries to {}", "✔".bright_green(),
             entries.len(), output.bold());
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Convenience: record from transaction module
// ─────────────────────────────────────────────────────────────

pub fn record_install(
    packages:   &[crate::package::Package],
    gen_before: Option<u32>,
    gen_after:  Option<u32>,
) {
    let pkgs: Vec<AuditPackage> = packages.iter().map(|p| AuditPackage {
        name:        p.name.clone(),
                                                      old_version: None,
                                                      new_version: Some(p.version.clone()),
    }).collect();
    let _ = AuditLog::record("install", &pkgs, gen_before, gen_after);
}

pub fn record_remove(
    names:      &[String],
    gen_before: Option<u32>,
    gen_after:  Option<u32>,
    db:         &crate::db::InstalledDb,
) {
    let pkgs: Vec<AuditPackage> = names.iter().map(|name| AuditPackage {
        name:        name.clone(),
                                                   old_version: db.get(name).map(|p| p.version),
                                                   new_version: None,
    }).collect();
    let _ = AuditLog::record("remove", &pkgs, gen_before, gen_after);
}

pub fn record_upgrade(
    packages:     &[crate::package::Package],
    upgrade_from: &std::collections::HashMap<String, String>,
    gen_before:   Option<u32>,
    gen_after:    Option<u32>,
) {
    let pkgs: Vec<AuditPackage> = packages.iter().map(|p| AuditPackage {
        name:        p.name.clone(),
                                                      old_version: upgrade_from.get(&p.name).cloned(),
                                                      new_version: Some(p.version.clone()),
    }).collect();
    let _ = AuditLog::record("upgrade", &pkgs, gen_before, gen_after);
}

pub fn record_gen_switch(gen_before: u32, gen_after: u32) {
    let _ = AuditLog::record("gen-switch", &[], Some(gen_before), Some(gen_after));
}

pub fn record_rollback(gen_before: u32, gen_after: u32) {
    let _ = AuditLog::record("rollback", &[], Some(gen_before), Some(gen_after));
}

// ─────────────────────────────────────────────────────────────
//  Helpers
// ─────────────────────────────────────────────────────────────

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

// ─────────────────────────────────────────────────────────────
//  Package signature verification
// ─────────────────────────────────────────────────────────────

/// Verify the Ed25519 signature of a downloaded .deb file.
/// The signature is expected at `<path>.sig` (detached).
/// Falls back to GPG verification if no `.sig` file is found.
pub fn verify_package_signature(deb_path: &std::path::Path) -> Result<()> {
    // Try Ed25519 first
    let sig_path = deb_path.with_extension("deb.sig");
    if sig_path.exists() {
        return verify_ed25519_sig(deb_path, &sig_path);
    }

    // Try GPG (.deb.asc)
    let asc_path = deb_path.with_extension("deb.asc");
    if asc_path.exists() {
        return crate::gpg::verify_file_detached(deb_path, &asc_path);
    }

    // No signature file found — warn but don't block if repository is trusted
    let repo_trusted = crate::gpg::is_repo_trusted(deb_path);
    if repo_trusted {
        crate::log::info(&format!(
            "audit: no signature file for {} (repo trusted via InRelease)",
            deb_path.display()
        ));
        Ok(())
    } else {
        anyhow::bail!(
            "No signature found for package '{}' and repo is not GPG-verified.\n  \
             Import the repository key with: hammer key import <keyfile>",
            deb_path.display()
        )
    }
}

fn verify_ed25519_sig(data_path: &std::path::Path, sig_path: &std::path::Path) -> Result<()> {
    let pub_key_bytes = std::fs::read(AUDIT_PUB)
        .context("Reading Ed25519 public key from /etc/hammer/audit-key.pub")?;
    if pub_key_bytes.len() != 32 {
        anyhow::bail!("Invalid public key length in {}", AUDIT_PUB);
    }
    let pub_arr: [u8; 32] = pub_key_bytes[..32].try_into().unwrap();
    let verifying_key = VerifyingKey::from_bytes(&pub_arr)
        .context("Parsing Ed25519 verifying key")?;

    let data      = std::fs::read(data_path).context("Reading package data")?;
    let sig_bytes = std::fs::read(sig_path).context("Reading .sig file")?;
    if sig_bytes.len() != 64 {
        anyhow::bail!("Invalid signature length in {}", sig_path.display());
    }
    let sig_arr: [u8; 64] = sig_bytes[..64].try_into().unwrap();
    let sig = Signature::from_bytes(&sig_arr);

    verifying_key.verify(&data, &sig)
        .context("Ed25519 signature verification failed")?;

    crate::log::info(&format!(
        "audit: Ed25519 signature OK for {}", data_path.display()
    ));
    Ok(())
}

pub fn record_mark(pkg: &str, reason: &str) {
    let msg = format!("mark: {} set to {}", pkg, reason);
    crate::log::info(&msg);
}
