use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use anyhow::{Context, Result};

use super::actions::{do_check_updates, do_gc_generations, do_security_upgrade, do_sync, do_verify};
use super::config::DaemonConfig;
use super::ipc::{DaemonRequest, DaemonResponse};
use super::service::write_pid_file;
use super::state::DaemonState;

pub const SOCKET_PATH: &str = "/run/hammerd.sock";

// ─────────────────────────────────────────────────────────────
//  Daemon entry point
// ─────────────────────────────────────────────────────────────

pub async fn run_daemon() -> Result<()> {
    let config = DaemonConfig::load();
    DaemonConfig::init_default().ok();
    write_pid_file()?;

    let state = Arc::new(Mutex::new(DaemonState::new()));

    // Remove stale socket
    let _ = std::fs::remove_file(SOCKET_PATH);
    let listener = UnixListener::bind(SOCKET_PATH)
        .context("Cannot bind hammerd socket")?;

    eprintln!("[hammerd] Listening on {}", SOCKET_PATH);

    // Spawn background scheduler under watchdog supervision
    let cfg2 = config.clone();
    let st2   = state.clone();
    tokio::spawn(super::scheduler::watchdog_scheduler(cfg2, st2));

    // Accept IPC connections
    loop {
        let st = state.clone();
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(handle_client(stream, st));
            }
            Err(e) => {
                eprintln!("[hammerd] Accept error: {}", e);
                // Back off briefly to avoid tight error loops
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
        // Shutdown check
        if !state.lock().await.running { break; }
    }

    eprintln!("[hammerd] Shutting down.");
    let _ = std::fs::remove_file(SOCKET_PATH);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Per-connection handler
// ─────────────────────────────────────────────────────────────

async fn handle_client(
    mut stream: tokio::net::UnixStream,
    state: Arc<Mutex<DaemonState>>,
) {
    let mut buf = vec![0u8; 8192];
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

// ─────────────────────────────────────────────────────────────
//  Dispatch table
// ─────────────────────────────────────────────────────────────

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
                "config_gen":  st.config_gen,
                "last_sync":   st.last_sync.map(|dt| dt.timestamp()),
                "last_check":  st.last_check.map(|dt| dt.timestamp()),
                "last_verify": st.last_verify.map(|dt| dt.timestamp()),
            }))
        }

        DaemonRequest::Sync => {
            match do_sync().await {
                Ok(_) => {
                    state.lock().await.last_sync = Some(chrono::Utc::now());
                    DaemonResponse::ok("Sync completed")
                }
                Err(e) => DaemonResponse::err(format!("Sync failed: {}", e)),
            }
        }

        DaemonRequest::Check => {
            match do_check_updates().await {
                Ok(n) => {
                    let mut st = state.lock().await;
                    st.last_check = Some(chrono::Utc::now());
                    st.n_updates  = n;
                    DaemonResponse::ok_data(
                        format!("{} update(s) available", n),
                        serde_json::json!({ "n_updates": n })
                    )
                }
                Err(e) => DaemonResponse::err(format!("Check failed: {}", e)),
            }
        }

        DaemonRequest::Verify => {
            match do_verify().await {
                Ok(()) => {
                    state.lock().await.last_verify = Some(chrono::Utc::now());
                    DaemonResponse::ok("Store verified OK")
                }
                Err(e) => DaemonResponse::err(format!("Verify failed: {}", e)),
            }
        }

        DaemonRequest::GetUpdates => {
            let n = state.lock().await.n_updates;
            DaemonResponse::ok_data("ok", serde_json::json!({ "n_updates": n }))
        }

        DaemonRequest::Reload => {
            let _new_cfg = DaemonConfig::load(); // re-reads from disk
            state.lock().await.config_gen += 1;
            DaemonResponse::ok("Config reloaded")
        }

        DaemonRequest::Shutdown => {
            state.lock().await.running = false;
            DaemonResponse::ok("Shutting down")
        }

        DaemonRequest::SecurityUpgrade => {
            match do_security_upgrade().await {
                Ok(()) => DaemonResponse::ok("Security upgrades applied"),
                Err(e) => DaemonResponse::err(format!("Security upgrade failed: {}", e)),
            }
        }

        DaemonRequest::GcGenerations { keep } => {
            match do_gc_generations(keep).await {
                Ok(()) => DaemonResponse::ok(format!("GC complete (kept {} generations)", keep)),
                Err(e) => DaemonResponse::err(format!("GC failed: {}", e)),
            }
        }
    }
}
