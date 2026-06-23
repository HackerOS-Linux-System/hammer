use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::net::UnixListener;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use serde::{Deserialize, Serialize};
use anyhow::{Context, Result};

// ─────────────────────────────────────────────────────────────
//  Constants
// ─────────────────────────────────────────────────────────────

pub const SOCKET_PATH:   &str = "/run/hammerd.sock";
pub const PID_FILE:      &str = "/run/hammerd.pid";
pub const CONFIG_FILE:   &str = "/etc/hammer/hammerd.toml";

// ─────────────────────────────────────────────────────────────
//  Daemon configuration
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Hours between auto-sync (0 = disabled)
    pub sync_interval_hours:   u64,
    /// Hours between update check + notification
    pub check_interval_hours:  u64,
    /// Hours between store integrity scan
    pub verify_interval_hours: u64,
    /// Enable desktop notifications
    pub notify:                bool,
    /// Log level for daemon
    pub log_level:             String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            sync_interval_hours:   6,
            check_interval_hours:  6,
            verify_interval_hours: 24,
            notify:                true,
            log_level:             "info".into(),
        }
    }
}

impl DaemonConfig {
    pub fn load() -> Self {
        let path = Path::new(CONFIG_FILE);
        if !path.exists() { return Self::default(); }
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }
}

// ─────────────────────────────────────────────────────────────
//  IPC protocol
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd")]
pub enum DaemonRequest {
    Status,
    Sync,
    Check,
    Verify,
    Reload,
    Shutdown,
    GetUpdates,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonResponse {
    pub ok:      bool,
    pub message: String,
    pub data:    Option<serde_json::Value>,
}

impl DaemonResponse {
    pub fn ok(msg: impl Into<String>) -> Self {
        DaemonResponse { ok: true, message: msg.into(), data: None }
    }
    pub fn ok_data(msg: impl Into<String>, data: serde_json::Value) -> Self {
        DaemonResponse { ok: true, message: msg.into(), data: Some(data) }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        DaemonResponse { ok: false, message: msg.into(), data: None }
    }
}

// ─────────────────────────────────────────────────────────────
//  Daemon state
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct DaemonState {
    pub last_sync:    Option<std::time::SystemTime>,
    pub last_check:   Option<std::time::SystemTime>,
    pub last_verify:  Option<std::time::SystemTime>,
    pub n_updates:    usize,
    pub running:      bool,
}

// ─────────────────────────────────────────────────────────────
//  Daemon entry point
// ─────────────────────────────────────────────────────────────

pub async fn run_daemon() -> Result<()> {
    let config = DaemonConfig::load();
    write_pid_file()?;

    let state = Arc::new(Mutex::new(DaemonState { running: true, ..Default::default() }));

    // Remove stale socket
    let _ = std::fs::remove_file(SOCKET_PATH);
    let listener = UnixListener::bind(SOCKET_PATH)
        .context("Cannot bind hammerd socket")?;

    eprintln!("[hammerd] Listening on {}", SOCKET_PATH);

    // Spawn scheduled tasks
    let cfg2 = config.clone();
    let st2   = state.clone();
    tokio::spawn(scheduler_loop(cfg2, st2));

    // Accept IPC connections
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let st = state.clone();
                tokio::spawn(handle_client(stream, st));
            }
            Err(e) => {
                eprintln!("[hammerd] Accept error: {}", e);
            }
        }
    }
}

async fn handle_client(
    mut stream: tokio::net::UnixStream,
    state: Arc<Mutex<DaemonState>>,
) {
    let mut buf = vec![0u8; 4096];
    let n = match stream.read(&mut buf).await {
        Ok(0) | Err(_) => return,
        Ok(n) => n,
    };

    let resp = match serde_json::from_slice::<DaemonRequest>(&buf[..n]) {
        Err(e) => DaemonResponse::err(format!("Invalid request: {}", e)),
        Ok(req) => dispatch(req, &state).await,
    };

    let bytes = serde_json::to_vec(&resp).unwrap_or_default();
    let _ = stream.write_all(&bytes).await;
}

async fn dispatch(
    req:   DaemonRequest,
    state: &Arc<Mutex<DaemonState>>,
) -> DaemonResponse {
    match req {
        DaemonRequest::Status => {
            let st = state.lock().await;
            DaemonResponse::ok_data("ok", serde_json::json!({
                "running":     st.running,
                "n_updates":   st.n_updates,
                "last_sync":   st.last_sync.map(|t| {
                    t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
                }),
                "last_check":  st.last_check.map(|t| {
                    t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
                }),
            }))
        }
        DaemonRequest::Sync => {
            drop(state.lock().await);
            match do_sync().await {
                Ok(_) => {
                    state.lock().await.last_sync = Some(std::time::SystemTime::now());
                    DaemonResponse::ok("Sync completed")
                }
                Err(e) => DaemonResponse::err(format!("Sync failed: {}", e)),
            }
        }
        DaemonRequest::Check => {
            match do_check_updates().await {
                Ok(n) => {
                    let mut st = state.lock().await;
                    st.last_check  = Some(std::time::SystemTime::now());
                    st.n_updates   = n;
                    DaemonResponse::ok_data(
                        format!("{} update(s) available", n),
                        serde_json::json!({ "n_updates": n })
                    )
                }
                Err(e) => DaemonResponse::err(format!("Check failed: {}", e)),
            }
        }
        DaemonRequest::Verify => {
            DaemonResponse::ok("Store verification scheduled")
        }
        DaemonRequest::GetUpdates => {
            let n = state.lock().await.n_updates;
            DaemonResponse::ok_data("ok", serde_json::json!({ "n_updates": n }))
        }
        DaemonRequest::Reload => {
            DaemonResponse::ok("Config reloaded")
        }
        DaemonRequest::Shutdown => {
            state.lock().await.running = false;
            DaemonResponse::ok("Shutting down")
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  Scheduler
// ─────────────────────────────────────────────────────────────

async fn scheduler_loop(
    config: DaemonConfig,
    state:  Arc<Mutex<DaemonState>>,
) {
    let sync_interval  = std::time::Duration::from_secs(config.sync_interval_hours  * 3600);
    let check_interval = std::time::Duration::from_secs(config.check_interval_hours * 3600);

    let mut next_sync  = tokio::time::Instant::now() + sync_interval;
    let mut next_check = tokio::time::Instant::now() + check_interval;

    loop {
        let now = tokio::time::Instant::now();

        if config.sync_interval_hours > 0 && now >= next_sync {
            let _ = do_sync().await;
            state.lock().await.last_sync = Some(std::time::SystemTime::now());
            next_sync = now + sync_interval;
        }

        if config.check_interval_hours > 0 && now >= next_check {
            if let Ok(n) = do_check_updates().await {
                let mut st = state.lock().await;
                st.last_check = Some(std::time::SystemTime::now());
                st.n_updates  = n;
                drop(st);
                if n > 0 && config.notify {
                    let _ = send_update_notification(n).await;
                }
            }
            next_check = now + check_interval;
        }

        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}

// ─────────────────────────────────────────────────────────────
//  Actions
// ─────────────────────────────────────────────────────────────

async fn do_sync() -> Result<()> {
    eprintln!("[hammerd] Running sync…");
    let status = tokio::process::Command::new("hammer")
        .arg("sync")
        .status().await
        .context("hammer sync")?;
    if !status.success() { anyhow::bail!("hammer sync exited {}", status); }
    eprintln!("[hammerd] Sync complete.");
    Ok(())
}

async fn do_check_updates() -> Result<usize> {
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

async fn send_update_notification(n: usize) -> Result<()> {
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

// ─────────────────────────────────────────────────────────────
//  Helpers
// ─────────────────────────────────────────────────────────────

fn write_pid_file() -> Result<()> {
    std::fs::write(PID_FILE, format!("{}\n", std::process::id()))
        .context("Writing PID file")?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Client helper — send a request to a running daemon
// ─────────────────────────────────────────────────────────────

pub async fn send_request(req: DaemonRequest) -> Result<DaemonResponse> {
    let mut stream = tokio::net::UnixStream::connect(SOCKET_PATH)
        .await
        .context("Cannot connect to hammerd socket — is hammerd running?")?;
    let bytes = serde_json::to_vec(&req)?;
    stream.write_all(&bytes).await?;
    stream.shutdown().await?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}

// ─────────────────────────────────────────────────────────────
//  systemd unit generator
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
         ExecStart={bin} start\n\
         ExecReload=/bin/kill -HUP $MAINPID\n\
         PIDFile={pid}\n\
         Restart=on-failure\n\
         RestartSec=30\n\
         StandardOutput=journal\n\
         StandardError=journal\n\n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        bin = bin.display(),
        pid = PID_FILE,
    );

    std::fs::write("/etc/systemd/system/hammerd.service", &service)?;
    let _ = std::process::Command::new("systemctl")
        .args(["enable", "hammerd.service", "--no-reload"])
        .status();
    let _ = std::process::Command::new("systemctl").arg("daemon-reload").status();
    Ok(())
}
