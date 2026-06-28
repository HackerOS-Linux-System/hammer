use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::ipc::{DaemonRequest, DaemonResponse};
use super::server::SOCKET_PATH;

// ─────────────────────────────────────────────────────────────
//  Client helper — send a request to a running daemon
// ─────────────────────────────────────────────────────────────

pub async fn send_request(req: DaemonRequest) -> Result<DaemonResponse> {
    let mut stream = tokio::net::UnixStream::connect(SOCKET_PATH)
        .await
        .context("Cannot connect to hammerd socket — is hammerd running?\n  \
                  Start with: systemctl start hammerd")?;
    let bytes = serde_json::to_vec(&req)?;
    stream.write_all(&bytes).await?;
    stream.shutdown().await?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}

/// Convenience: check if the daemon is alive (Status ping).
pub async fn daemon_alive() -> bool {
    send_request(DaemonRequest::Status).await
        .map(|r| r.ok)
        .unwrap_or(false)
}
