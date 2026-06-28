use std::path::PathBuf;
use anyhow::{Context, Result};

pub const PID_FILE: &str = "/run/hammerd.pid";

// ─────────────────────────────────────────────────────────────
//  PID file
// ─────────────────────────────────────────────────────────────

pub fn write_pid_file() -> Result<()> {
    std::fs::write(PID_FILE, format!("{}\n", std::process::id()))
        .context("Writing PID file")?;
    Ok(())
}

pub fn remove_pid_file() {
    let _ = std::fs::remove_file(PID_FILE);
}

pub fn read_pid() -> Option<u32> {
    std::fs::read_to_string(PID_FILE).ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Return true if a hammerd process is already running.
pub fn is_running() -> bool {
    let Some(pid) = read_pid() else { return false };
    // Check /proc/<pid>/cmdline exists (Linux)
    std::path::Path::new(&format!("/proc/{}/cmdline", pid)).exists()
}

// ─────────────────────────────────────────────────────────────
//  systemd unit installer
// ─────────────────────────────────────────────────────────────

pub fn install_hammerd_service() -> Result<()> {
    let bin = std::fs::read_link("/proc/self/exe")
        .unwrap_or_else(|_| PathBuf::from("/usr/bin/hammer-daemon"));

    let service = format!(
        "[Unit]\n\
         Description=Hammer Package Manager Daemon\n\
         Documentation=https://github.com/HackerOS-Linux-System/hammer\n\
         After=network-online.target\n\
         Wants=network-online.target\n\n\
         [Service]\n\
         Type=simple\n\
         ExecStart={bin} daemon start\n\
         ExecReload=/bin/kill -HUP $MAINPID\n\
         PIDFile={pid}\n\
         Restart=on-failure\n\
         RestartSec=30\n\
         StandardOutput=journal\n\
         StandardError=journal\n\
         # Security hardening\n\
         NoNewPrivileges=yes\n\
         ProtectSystem=strict\n\
         ReadWritePaths=/hammer /run /var/log\n\
         PrivateTmp=yes\n\n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        bin = bin.display(),
        pid = PID_FILE,
    );

    std::fs::write("/etc/systemd/system/hammerd.service", &service)?;
    let _ = std::process::Command::new("systemctl")
        .args(["enable", "hammerd.service", "--no-reload"])
        .status();
    let _ = std::process::Command::new("systemctl")
        .arg("daemon-reload")
        .status();
    Ok(())
}

pub fn uninstall_hammerd_service() -> Result<()> {
    let _ = std::process::Command::new("systemctl")
        .args(["disable", "hammerd.service"])
        .status();
    let _ = std::fs::remove_file("/etc/systemd/system/hammerd.service");
    let _ = std::process::Command::new("systemctl")
        .arg("daemon-reload")
        .status();
    Ok(())
}
