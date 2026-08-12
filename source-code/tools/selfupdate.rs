use anyhow::{bail, Context, Result};
use owo_colors::OwoColorize;
use std::path::{Path, PathBuf};

use crate::download::HttpClient;
use crate::log;

pub const VERSION_FILE:  &str = "/usr/lib/HackerOS/hammer/version.hacker";
pub const HAMMER_BIN:    &str = "/usr/bin/hammer";
pub const ANVIL_BIN:     &str = "/usr/bin/anvil";
pub const STORE_BIN:     &str = "/usr/share/hammer/store";

const VERSION_URL: &str =
"https://raw.githubusercontent.com/HackerOS-Linux-System/hammer/main/version.hacker";
const RELEASE_BASE: &str =
"https://github.com/HackerOS-Linux-System/hammer/releases/download";

// version.hacker format: "[\n 0.1\n]"
fn parse_version_hacker(content: &str) -> Option<String> {
    let trimmed = content.trim();
    let inner   = trimmed.strip_prefix('[')?.strip_suffix(']')?;
    let ver     = inner.trim().to_string();
    if ver.is_empty() { None } else { Some(ver) }
}

fn read_local_version() -> Option<String> {
    std::fs::read_to_string(VERSION_FILE)
    .ok()
    .and_then(|s| parse_version_hacker(&s))
}

pub async fn check_for_update(client: &HttpClient) -> Result<Option<String>> {
    let remote_content = client.get_string(VERSION_URL).await
    .context("Fetching remote version.hacker")?;
    let remote_ver = parse_version_hacker(&remote_content)
    .ok_or_else(|| anyhow::anyhow!("Cannot parse remote version.hacker"))?;
    let local_ver = read_local_version()
    .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    if version_gt(&remote_ver, &local_ver) { Ok(Some(remote_ver)) }
    else { Ok(None) }
}

pub async fn self_update(client: &HttpClient) -> Result<()> {
    let new_version = match check_for_update(client).await? {
        Some(v) => v,
        None => {
            println!("  {} hammer is already up to date ({}).",
                     "✔".bright_green(), env!("CARGO_PKG_VERSION").cyan());
            return Ok(());
        }
    };

    let local_ver = read_local_version()
    .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    println!("  {}  Self-update: {} → {}",
             "⬡".bright_cyan().bold(), local_ver.dimmed(), new_version.bright_cyan().bold());

    let tag = format!("v{}", new_version);

    #[cfg(feature = "normal-mode")]
    { return self_update_normal_mode(client, &tag, &new_version).await; }
    #[cfg(not(feature = "normal-mode"))]
    { self_update_atomic(client, &tag, &new_version).await }
}

/// Normal-mode self-update: the release asset for this build is a
/// tar.gz archive (`hammer-normal-mode.tar.gz`) containing just the
/// `hammer` binary — no `anvil`/`hammer-store` companions (those are
/// atomic-mode-only tools that don't apply to a normal-mode install), and
/// no generations/GRUB integration to worry about, matching normal-mode's
/// "works like a classic package manager" design everywhere else. The
/// archive itself is what gets signature-checked (before extraction —
/// never unpack unverified content), not the binary inside it.
#[cfg(feature = "normal-mode")]
async fn self_update_normal_mode(client: &HttpClient, tag: &str, new_version: &str) -> Result<()> {
    let archive_url = format!("{}/{}/hammer-normal-mode.tar.gz", RELEASE_BASE, tag);
    println!("  {} Downloading hammer-normal-mode.tar.gz…", "·".dimmed());
    let archive_bytes = client.get_bytes(&archive_url).await
        .with_context(|| format!("Downloading {archive_url}"))?;
    println!("  {} Downloaded ({} bytes)", "✔".green(), archive_bytes.len());

    let tmp_archive = PathBuf::from("/tmp/hammer-update-normal-mode.tar.gz.tmp");
    std::fs::write(&tmp_archive, &archive_bytes)
        .context("Writing temp archive")?;

    // Signature verification — of the archive itself, before extraction.
    let sig_url = format!("{}.sig", archive_url);
    match client.get_bytes(&sig_url).await {
        Ok(sig_bytes) => {
            let sig_path = tmp_archive.with_extension("sig");
            std::fs::write(&sig_path, &sig_bytes)?;
            match crate::audit::verify_package_signature(&tmp_archive) {
                Ok(()) => println!("  {} Ed25519 OK: hammer-normal-mode.tar.gz", "✔".bright_green()),
                Err(e) => {
                    std::fs::remove_file(&tmp_archive).ok();
                    std::fs::remove_file(&sig_path).ok();
                    bail!("Signature verification FAILED for hammer-normal-mode.tar.gz: {e}\n  \
                           Self-update aborted — hammer was NOT updated.");
                }
            }
            std::fs::remove_file(&sig_path).ok();
        }
        Err(_) => {
            println!("  {} No .sig for hammer-normal-mode.tar.gz — skipping signature check (legacy release?)",
                     "⚠".yellow());
        }
    }

    // Extract just the `hammer` binary from the archive into a temp file
    // (never extract directly onto the live binary path — the atomic
    // rename below is what makes this safe against a crash mid-update).
    let extracted = extract_hammer_binary(&tmp_archive)
        .context("Extracting hammer binary from hammer-normal-mode.tar.gz")?;
    std::fs::remove_file(&tmp_archive).ok();

    let mut perms = std::fs::metadata(&extracted)?.permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&extracted, perms)?;

    if let Some(parent) = Path::new(HAMMER_BIN).parent() { std::fs::create_dir_all(parent)?; }
    std::fs::rename(&extracted, HAMMER_BIN)
        .with_context(|| format!("Replacing {HAMMER_BIN} (atomic rename)"))?;
    log::info(&format!("self-update: replaced {HAMMER_BIN}"));
    println!("  {} {} updated.", "✔".bright_green(), HAMMER_BIN);

    write_version_file(new_version)?;

    println!();
    println!("  {}  hammer updated to {}.", "⬡".bright_cyan().bold(), new_version.bright_cyan().bold());
    println!("  {}  Restart hammer for the new version to take effect.", "·".dimmed());
    Ok(())
}

/// Finds and extracts the `hammer` binary from a tar.gz archive, writing
/// it to a temp file and returning that path. Looks for an entry named
/// exactly `hammer` at any depth in the archive (tolerates being packed
/// either at the archive root or inside a subdirectory).
#[cfg(feature = "normal-mode")]
fn extract_hammer_binary(archive_path: &Path) -> Result<PathBuf> {
    let file = std::fs::File::open(archive_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    let out_path = PathBuf::from("/tmp/hammer-update-hammer.tmp");
    let mut found = false;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        if path.file_name().map(|n| n == "hammer").unwrap_or(false) {
            let mut out = std::fs::File::create(&out_path)?;
            std::io::copy(&mut entry, &mut out)?;
            found = true;
            break;
        }
    }
    if !found {
        bail!("hammer-normal-mode.tar.gz did not contain a 'hammer' binary");
    }
    Ok(out_path)
}

#[cfg(feature = "normal-mode")]
fn write_version_file(new_version: &str) -> Result<()> {
    std::fs::create_dir_all(Path::new(VERSION_FILE).parent().unwrap_or(Path::new("/")))?;
    let version_content = format!("[\n {}\n]\n", new_version);
    let tmp = format!("{}.tmp", VERSION_FILE);
    std::fs::write(&tmp, &version_content)?;
    std::fs::rename(&tmp, VERSION_FILE)?;
    Ok(())
}

#[cfg(not(feature = "normal-mode"))]
async fn self_update_atomic(client: &HttpClient, tag: &str, new_version: &str) -> Result<()> {

    struct Target<'a> { name: &'a str, url: String, dest: &'a str, required: bool }

    let targets = [
        Target { name: "hammer",       url: format!("{}/{}/hammer",       RELEASE_BASE, tag), dest: HAMMER_BIN, required: true  },
        Target { name: "anvil",        url: format!("{}/{}/anvil",        RELEASE_BASE, tag), dest: ANVIL_BIN,  required: false },
        Target { name: "hammer-store", url: format!("{}/{}/hammer-store", RELEASE_BASE, tag), dest: STORE_BIN,  required: false },
    ];

    // Phase 1: download all to tmp — all-or-nothing
    let mut downloaded: Vec<(PathBuf, &str)> = Vec::new();

    for target in &targets {
        println!("  {} Downloading {}…", "·".dimmed(), target.name);
        match client.get_bytes(&target.url).await {
            Ok(bytes) => {
                let tmp = PathBuf::from(format!("/tmp/hammer-update-{}.tmp", target.name));
                std::fs::write(&tmp, &bytes)
                .with_context(|| format!("Writing temp file for {}", target.name))?;
                let mut perms = std::fs::metadata(&tmp)?.permissions();
                std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
                std::fs::set_permissions(&tmp, perms)?;
                downloaded.push((tmp, target.dest));
                println!("  {} {} downloaded ({} bytes)", "✔".green(), target.name, bytes.len());
            }
            Err(e) => {
                if target.required {
                    for (tmp, _) in &downloaded { std::fs::remove_file(tmp).ok(); }
                    bail!("Failed to download {}: {}", target.name, e);
                } else {
                    println!("  {} {} not in this release, skipping.", "·".dimmed(), target.name);
                }
            }
        }
    }

    // Phase 1b: Ed25519 signature verification — verify ALL before installing ANY
    for (tmp_path, dest_name) in &downloaded {
        let sig_url = format!(
            "{}/{}/{}.sig",
            crate::selfupdate::RELEASE_BASE, tag, dest_name.rsplit('/').next().unwrap_or(dest_name)
        );
        match client.get_bytes(&sig_url).await {
            Ok(sig_bytes) => {
                let sig_path = tmp_path.with_extension("sig");
                std::fs::write(&sig_path, &sig_bytes)?;
                match crate::audit::verify_package_signature(tmp_path) {
                    Ok(()) => println!("  {} Ed25519 OK: {}", "✔".bright_green(), dest_name),
                    Err(e) => {
                        // Clean up all downloads
                        for (p, _) in &downloaded { std::fs::remove_file(p).ok(); }
                        bail!("Signature verification FAILED for {}: {}
                                 Self-update aborted — hammer was NOT updated.", dest_name, e);
                    }
                }
            }
            Err(_) => {
                println!("  {} No .sig for {} — skipping signature check (legacy release?)",
                         "⚠".yellow(), dest_name);
            }
        }
    }

    // Phase 2: atomic rename — point of no return
    for (tmp_path, dest_str) in &downloaded {
        let dest = Path::new(dest_str);
        if let Some(parent) = dest.parent() { std::fs::create_dir_all(parent)?; }
        std::fs::rename(tmp_path, dest)
        .with_context(|| format!("Replacing {} (atomic rename)", dest_str))?;
        log::info(&format!("self-update: replaced {}", dest_str));
        println!("  {} {} updated.", "✔".bright_green(), dest_str);
    }

    // Phase 3: update version file
    std::fs::create_dir_all(
        Path::new(VERSION_FILE).parent().unwrap_or(Path::new("/"))
    )?;
    let version_content = format!("[\n {}\n]\n", new_version);
    let tmp = format!("{}.tmp", VERSION_FILE);
    std::fs::write(&tmp, &version_content)?;
    std::fs::rename(&tmp, VERSION_FILE)?;

    println!();
    println!("  {}  hammer updated to {}.", "⬡".bright_cyan().bold(), new_version.bright_cyan().bold());
    println!("  {}  Restart hammer for the new version to take effect.", "·".dimmed());
    Ok(())
}

fn version_gt(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Vec<u32> {
        s.trim_start_matches('v').split('.')
        .filter_map(|p| p.parse().ok()).collect()
    };
    let av = parse(a);
    let bv = parse(b);
    let len = av.len().max(bv.len());
    for i in 0..len {
        let ai = av.get(i).copied().unwrap_or(0);
        let bi = bv.get(i).copied().unwrap_or(0);
        if ai > bi { return true; }
        if ai < bi { return false; }
    }
    false
}
