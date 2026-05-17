use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const KEYRING_DIR: &str = "/etc/hammer/trusted.gpg.d";
pub const KEYRING_DB:  &str = "/hammer/db/keyring.json";

// ─────────────────────────────────────────────────────────────
//  Key database
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedKey {
    pub fingerprint: String,
    pub name:        String,
    pub email:       Option<String>,
    pub added_at:    String,
    pub source:      String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct KeyringDb {
    pub keys: Vec<TrustedKey>,
}

impl KeyringDb {
    pub fn load() -> Result<Self> {
        let path = Path::new(KEYRING_DB);
        if !path.exists() { return Ok(Self::default()); }
        let txt = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&txt)?)
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all("/hammer/db")?;
        let txt = serde_json::to_string_pretty(self)?;
        let tmp = format!("{}.tmp", KEYRING_DB);
        std::fs::write(&tmp, &txt)?;
        std::fs::rename(&tmp, KEYRING_DB)?;
        Ok(())
    }

    pub fn find(&self, fingerprint: &str) -> Option<&TrustedKey> {
        let fp = fingerprint.to_uppercase().replace(' ', "");
        self.keys.iter().find(|k| k.fingerprint.replace(' ', "").ends_with(&fp))
    }

    pub fn remove(&mut self, fingerprint: &str) -> bool {
        let before = self.keys.len();
        let fp = fingerprint.to_uppercase().replace(' ', "");
        self.keys.retain(|k| !k.fingerprint.replace(' ', "").ends_with(&fp));
        self.keys.len() < before
    }
}

// ─────────────────────────────────────────────────────────────
//  Key import
// ─────────────────────────────────────────────────────────────

pub async fn import_key(source: &str, client: &crate::download::HttpClient) -> Result<TrustedKey> {
    std::fs::create_dir_all(KEYRING_DIR)?;

    let key_bytes: Vec<u8> = if source.starts_with("http://") || source.starts_with("https://") {
        client.get_bytes(source).await
        .with_context(|| format!("Fetching key from {}", source))?
    } else {
        std::fs::read(source).with_context(|| format!("Reading key file {}", source))?
    };

    let tmp_key = format!("/tmp/hammer-import-{}.gpg", std::process::id());
    std::fs::write(&tmp_key, &key_bytes)?;

    let info = gpg_key_info(&tmp_key).context("Cannot read key info — is gpg installed?")?;

    let dest = Path::new(KEYRING_DIR).join(format!("{}.gpg", info.fingerprint_short()));
    std::fs::copy(&tmp_key, &dest)?;
    let _ = std::fs::remove_file(&tmp_key);

    let _ = Command::new("gpg")
    .args(["--no-default-keyring",
          "--keyring", &format!("{}/hammer.gpg", KEYRING_DIR),
          "--import", dest.to_str().unwrap_or("")])
    .output();

    let key = TrustedKey {
        fingerprint: info.fingerprint.clone(),
        name:        info.name.clone(),
        email:       info.email.clone(),
        added_at:    chrono::Utc::now().to_rfc3339(),
        source:      source.to_string(),
    };

    let mut db = KeyringDb::load()?;
    db.remove(&info.fingerprint);
    db.keys.push(key.clone());
    db.save()?;

    crate::log::info(&format!("gpg: imported key {} ({})", info.fingerprint_short(), info.name));
    Ok(key)
}

// ─────────────────────────────────────────────────────────────
//  InRelease / Release.gpg verification
// ─────────────────────────────────────────────────────────────

pub fn verify_inrelease(inrelease_bytes: &[u8]) -> Result<String> {
    if !gpg_available() {
        crate::log::warn("gpg: not installed, skipping signature verification");
        return extract_inrelease_content(inrelease_bytes);
    }

    let keyring = build_gpg_keyring_args();
    let tmp_in  = format!("/tmp/hammer-inrelease-{}", std::process::id());
    let tmp_out = format!("/tmp/hammer-release-{}", std::process::id());
    std::fs::write(&tmp_in, inrelease_bytes)?;

    let mut cmd_args = vec![
        "--batch".to_string(), "--no-default-keyring".to_string(),
        "--trust-model".to_string(), "always".to_string(),
        "--output".to_string(), tmp_out.clone(),
        "--decrypt".to_string(), tmp_in.clone(),
    ];
    for k in &keyring { cmd_args.push(k.clone()); }

    let out = Command::new("gpg").args(&cmd_args).output().context("Failed to run gpg")?;
    let _ = std::fs::remove_file(&tmp_in);

    if out.status.success() {
        let content = std::fs::read_to_string(&tmp_out).context("Reading gpg output")?;
        let _ = std::fs::remove_file(&tmp_out);
        crate::log::info("gpg: InRelease signature OK");
        Ok(content)
    } else {
        let _ = std::fs::remove_file(&tmp_out);
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("GPG signature verification FAILED:\n{}", stderr)
    }
}

pub fn verify_release_gpg(release_bytes: &[u8], sig_bytes: &[u8]) -> Result<()> {
    if !gpg_available() {
        crate::log::warn("gpg: not installed, skipping signature verification");
        return Ok(());
    }

    let keyring = build_gpg_keyring_args();
    let tmp_rel = format!("/tmp/hammer-release-{}", std::process::id());
    let tmp_sig = format!("/tmp/hammer-release-gpg-{}", std::process::id());
    std::fs::write(&tmp_rel, release_bytes)?;
    std::fs::write(&tmp_sig, sig_bytes)?;

    let mut cmd_args = vec![
        "--batch".to_string(), "--no-default-keyring".to_string(),
        "--trust-model".to_string(), "always".to_string(),
        "--verify".to_string(), tmp_sig.clone(), tmp_rel.clone(),
    ];
    for k in &keyring { cmd_args.push(k.clone()); }

    let out = Command::new("gpg").args(&cmd_args).output()?;
    let _ = std::fs::remove_file(&tmp_rel);
    let _ = std::fs::remove_file(&tmp_sig);

    if out.status.success() {
        crate::log::info("gpg: Release.gpg signature OK");
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("GPG signature verification FAILED:\n{}", stderr)
    }
}

pub fn verify_packages_hash(packages_bytes: &[u8], release_content: &str, filename: &str) -> Result<()> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(packages_bytes);
    let actual_hash = hex::encode(hasher.finalize());

    let in_sha256 = release_content.lines()
    .skip_while(|l| !l.starts_with("SHA256:"))
    .skip(1)
    .take_while(|l| l.starts_with(' '))
    .find(|l| l.contains(filename));

    match in_sha256 {
        Some(line) => {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(expected) = parts.first() {
                if *expected != actual_hash.as_str() {
                    bail!("SHA256 mismatch for {}:\n  expected: {}\n  actual:   {}",
                          filename, expected, actual_hash);
                }
            }
            Ok(())
        }
        None => Ok(()),
    }
}

// ─────────────────────────────────────────────────────────────
//  Trusted Boot
// ─────────────────────────────────────────────────────────────

pub const BOOT_HASH_FILE: &str = "/hammer/db/boot-hashes.json";

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct BootHashDb {
    pub entries: Vec<BootHashEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootHashEntry {
    pub gen_number: u32,
    pub hash:       String,
    pub timestamp:  String,
    pub signed_by:  Option<String>,
}

impl BootHashDb {
    pub fn load() -> Result<Self> {
        let path = Path::new(BOOT_HASH_FILE);
        if !path.exists() { return Ok(Self::default()); }
        let txt = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&txt)?)
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all("/hammer/db")?;
        let txt = serde_json::to_string_pretty(self)?;
        let tmp = format!("{}.tmp", BOOT_HASH_FILE);
        std::fs::write(&tmp, &txt)?;
        std::fs::rename(&tmp, BOOT_HASH_FILE)?;
        Ok(())
    }

    pub fn get_gen(&self, gen: u32) -> Option<&BootHashEntry> {
        self.entries.iter().find(|e| e.gen_number == gen)
    }
}

pub fn hash_generation(gen_number: u32) -> Result<String> {
    use sha2::{Digest, Sha256};
    let profile_path = PathBuf::from(crate::store::PROFILES_DIR)
    .join(format!("gen-{}", gen_number));
    if !profile_path.exists() {
        bail!("Generation profile gen-{} not found", gen_number);
    }

    let mut entries: Vec<(String, String)> = Vec::new();
    for item in walkdir::WalkDir::new(&profile_path).sort_by_file_name() {
        let item = item?;
        if !item.file_type().is_file() && !item.file_type().is_symlink() { continue; }
        let rel = item.path().strip_prefix(&profile_path)
        .map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        let entry_hash = if item.file_type().is_symlink() {
            let target = std::fs::read_link(item.path())
            .map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            let mut h = Sha256::new();
            h.update(b"symlink:");
            h.update(target.as_bytes());
            hex::encode(h.finalize())
        } else {
            match std::fs::read(item.path()) {
                Ok(bytes) => { let mut h = Sha256::new(); h.update(&bytes); hex::encode(h.finalize()) }
                Err(_) => "000000".to_string(),
            }
        };
        entries.push((rel, entry_hash));
    }

    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut manifest = Sha256::new();
    for (path, hash) in &entries {
        manifest.update(format!("{}:{}\n", path, hash).as_bytes());
    }
    Ok(hex::encode(manifest.finalize()))
}

pub fn record_gen_hash(gen_number: u32) -> Result<String> {
    let hash = hash_generation(gen_number)?;
    let mut db = BootHashDb::load()?;
    db.entries.retain(|e| e.gen_number != gen_number);
    db.entries.push(BootHashEntry {
        gen_number,
        hash: hash.clone(),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    signed_by: None,
    });
    db.save()?;
    Ok(hash)
}

pub fn verify_boot_integrity(gen_number: u32) -> Result<()> {
    let db = BootHashDb::load()?;
    let entry = match db.get_gen(gen_number) {
        Some(e) => e.clone(),
        None => {
            crate::log::warn(&format!(
                "boot-integrity: no recorded hash for gen-{}, skipping", gen_number));
            return Ok(());
        }
    };
    let actual = hash_generation(gen_number)?;
    if actual != entry.hash {
        bail!("BOOT INTEGRITY VIOLATION: gen-{} hash mismatch!\n  recorded: {}\n  actual:   {}",
              gen_number, entry.hash, actual);
    }
    crate::log::info(&format!("boot-integrity: gen-{} OK ({}…)", gen_number, &actual[..16]));
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  GPG helpers
// ─────────────────────────────────────────────────────────────

fn gpg_available() -> bool {
    Command::new("gpg").arg("--version").output()
    .map(|o| o.status.success()).unwrap_or(false)
}

fn build_gpg_keyring_args() -> Vec<String> {
    let dir = Path::new(KEYRING_DIR);
    let mut args = Vec::new();
    if !dir.exists() { return args; }
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().map_or(false, |e| e == "gpg") {
            args.push("--keyring".to_string());
            args.push(path.to_string_lossy().to_string());
        }
    }
    args
}

struct GpgKeyInfo { fingerprint: String, name: String, email: Option<String> }

impl GpgKeyInfo {
    fn fingerprint_short(&self) -> &str {
        if self.fingerprint.len() >= 16 {
            &self.fingerprint[self.fingerprint.len() - 16..]
        } else { &self.fingerprint }
    }
}

fn gpg_key_info(keyfile: &str) -> Result<GpgKeyInfo> {
    let out = Command::new("gpg")
    .args(["--with-fingerprint", "--with-colons", "--import-options", "import-show",
          "--import", keyfile])
    .output().context("Running gpg --with-fingerprint")?;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut fingerprint = String::new();
    let mut name        = String::new();
    let mut email       = None;

    for line in stdout.lines() {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() < 10 { continue; }
        match parts[0] {
            "fpr" => { fingerprint = parts.get(9).unwrap_or(&"").to_string(); }
            "uid" => {
                let uid = parts.get(9).unwrap_or(&"");
                if let Some((n, e)) = parse_uid(uid) {
                    if name.is_empty() { name = n; }
                    if email.is_none() { email = e; }
                }
            }
            _ => {}
        }
    }
    if fingerprint.is_empty() {
        bail!("Could not read fingerprint from key — is it a valid GPG key?");
    }
    if name.is_empty() { name = "Unknown".to_string(); }
    Ok(GpgKeyInfo { fingerprint, name, email })
}

fn parse_uid(uid: &str) -> Option<(String, Option<String>)> {
    if let Some(bracket) = uid.find('<') {
        let name = uid[..bracket].trim().to_string();
        let email_end = uid.find('>').unwrap_or(uid.len());
        let email = uid[bracket+1..email_end].trim().to_string();
        Some((name, if email.is_empty() { None } else { Some(email) }))
    } else {
        Some((uid.to_string(), None))
    }
}

fn extract_inrelease_content(bytes: &[u8]) -> Result<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut in_content  = false;
    let mut past_header = false;
    let mut content     = String::new();

    for line in text.lines() {
        if line.starts_with("-----BEGIN PGP SIGNED MESSAGE-----") { in_content = true; continue; }
        if line.starts_with("-----BEGIN PGP SIGNATURE-----") { break; }
        if in_content {
            if !past_header && line.is_empty() { past_header = true; continue; }
            if past_header { content.push_str(line); content.push('\n'); }
        }
    }
    if content.is_empty() { return Ok(text.into_owned()); }
    Ok(content)
}
