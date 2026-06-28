use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────
//  IPC protocol
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd")]
pub enum DaemonRequest {
    /// Query daemon status and counters
    Status,
    /// Trigger an immediate package index sync
    Sync,
    /// Check for upgradable packages
    Check,
    /// Run store integrity verification
    Verify,
    /// Reload configuration from disk
    Reload,
    /// Graceful shutdown
    Shutdown,
    /// Get the current upgrade count without triggering a check
    GetUpdates,
    /// Apply pending security-only upgrades
    SecurityUpgrade,
    /// Trigger GC of old generations (keep last N)
    GcGenerations { keep: u32 },
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
