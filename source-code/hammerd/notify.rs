use anyhow::Result;

// ─────────────────────────────────────────────────────────────
//  Desktop notification helpers
// ─────────────────────────────────────────────────────────────

pub async fn send_update_notification(n: usize) -> Result<()> {
    let summary = format!("{} update{} available", n, if n == 1 { "" } else { "s" });
    let _ = tokio::process::Command::new("notify-send")
        .args([
            "--app-name=hammer",
            "--icon=software-update-available",
            "--urgency=normal",
            &summary,
            "Run 'hammer upgrade' to install updates.",
        ])
        .status().await;
    Ok(())
}

pub async fn send_verify_failure_notification(msg: &str) -> Result<()> {
    let _ = tokio::process::Command::new("notify-send")
        .args([
            "--app-name=hammer",
            "--icon=dialog-warning",
            "--urgency=critical",
            "Store integrity problem",
            msg,
        ])
        .status().await;
    Ok(())
}

pub async fn send_security_upgrade_notification(n: usize) -> Result<()> {
    let summary = format!("{} security update{} applied", n, if n == 1 { "" } else { "s" });
    let _ = tokio::process::Command::new("notify-send")
        .args([
            "--app-name=hammer",
            "--icon=security-high",
            "--urgency=normal",
            &summary,
            "Reboot to activate.",
        ])
        .status().await;
    Ok(())
}
