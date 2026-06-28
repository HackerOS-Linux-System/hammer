use anyhow::{Context, Result};

// ─────────────────────────────────────────────────────────────
//  Actions — run as background tasks or in response to IPC
// ─────────────────────────────────────────────────────────────

/// Refresh the package index (`hammer sync`).
pub async fn do_sync() -> Result<()> {
    eprintln!("[hammerd] Running sync…");
    let status = tokio::process::Command::new("hammer")
        .arg("sync")
        .status().await
        .context("hammer sync")?;
    if !status.success() {
        anyhow::bail!("hammer sync exited {}", status);
    }
    eprintln!("[hammerd] Sync complete.");
    Ok(())
}

/// Return the number of upgradable packages.
pub async fn do_check_updates() -> Result<usize> {
    eprintln!("[hammerd] Checking for updates…");
    let out = tokio::process::Command::new("hammer")
        .args(["list", "--upgradable", "--json"])
        .output().await
        .context("hammer list --upgradable")?;
    if !out.status.success() { return Ok(0); }
    let text = String::from_utf8_lossy(&out.stdout);
    let count = text.lines().filter(|l| !l.trim().is_empty()).count();
    eprintln!("[hammerd] {} update(s) available.", count);
    Ok(count)
}

/// Run store integrity verification (`hammer verify`).
pub async fn do_verify() -> Result<()> {
    eprintln!("[hammerd] Running store verify…");
    let status = tokio::process::Command::new("hammer")
        .args(["verify", "--quiet"])
        .status().await
        .context("hammer verify")?;
    if !status.success() {
        anyhow::bail!("hammer verify exited {}", status);
    }
    eprintln!("[hammerd] Verify complete.");
    Ok(())
}

/// Apply security-only upgrades non-interactively.
pub async fn do_security_upgrade() -> Result<()> {
    eprintln!("[hammerd] Applying security upgrades…");
    let status = tokio::process::Command::new("hammer")
        .args(["upgrade", "--security-only", "--yes"])
        .status().await
        .context("hammer upgrade --security-only")?;
    if !status.success() {
        anyhow::bail!("hammer upgrade --security-only exited {}", status);
    }
    eprintln!("[hammerd] Security upgrade complete.");
    Ok(())
}

/// GC old generations (keep last `keep`).
pub async fn do_gc_generations(keep: u32) -> Result<()> {
    eprintln!("[hammerd] GC generations (keep={})…", keep);
    let status = tokio::process::Command::new("hammer")
        .args(["gen", "gc", "--keep", &keep.to_string()])
        .status().await
        .context("hammer gen gc")?;
    if !status.success() {
        anyhow::bail!("hammer gen gc exited {}", status);
    }
    eprintln!("[hammerd] GC complete.");
    Ok(())
}
