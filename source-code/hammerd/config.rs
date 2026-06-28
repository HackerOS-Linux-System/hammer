use std::path::Path;
use serde::{Deserialize, Serialize};

pub const CONFIG_FILE: &str = "/etc/hammer/hammerd.toml";

// ─────────────────────────────────────────────────────────────
//  DaemonConfig
// ─────────────────────────────────────────────────────────────

fn default_sync_interval()   -> u64    { 6 }
fn default_check_interval()  -> u64    { 6 }
fn default_verify_interval() -> u64    { 24 }
fn default_notify()          -> bool   { true }
fn default_log_level()       -> String { "info".into() }
fn default_auto_security()   -> bool   { false }
fn default_max_generations() -> u32    { 20 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Hours between auto-sync (0 = disabled)
    #[serde(default = "default_sync_interval")]
    pub sync_interval_hours:   u64,
    /// Hours between update check + notification
    #[serde(default = "default_check_interval")]
    pub check_interval_hours:  u64,
    /// Hours between store integrity scan
    #[serde(default = "default_verify_interval")]
    pub verify_interval_hours: u64,
    /// Enable desktop notifications
    #[serde(default = "default_notify")]
    pub notify:                bool,
    /// Log level: "trace" | "debug" | "info" | "warn" | "error"
    #[serde(default = "default_log_level")]
    pub log_level:             String,
    /// Auto-apply security-only upgrades without prompting
    #[serde(default = "default_auto_security")]
    pub auto_security_upgrade: bool,
    /// Maximum number of generations to keep before GC
    #[serde(default = "default_max_generations")]
    pub max_generations:       u32,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            sync_interval_hours:   6,
            check_interval_hours:  6,
            verify_interval_hours: 24,
            notify:                true,
            log_level:             "info".into(),
            auto_security_upgrade: false,
            max_generations:       20,
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

    pub fn save(&self) -> anyhow::Result<()> {
        let dir = std::path::Path::new(CONFIG_FILE).parent().unwrap();
        std::fs::create_dir_all(dir)?;
        let toml_str = toml::to_string_pretty(self)?;
        std::fs::write(CONFIG_FILE, toml_str)?;
        Ok(())
    }

    /// Write a default config file if none exists.
    pub fn init_default() -> anyhow::Result<()> {
        if !std::path::Path::new(CONFIG_FILE).exists() {
            Self::default().save()?;
        }
        Ok(())
    }
}
