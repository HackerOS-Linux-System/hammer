use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ─────────────────────────────────────────────────────────────
//  LayerOp / PackageLayer
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayerOp {
    Install,
    Uninstall,
    Override,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageLayer {
    pub name:    String,
    pub version: String,
    #[serde(default = "default_op")]
    pub op:      LayerOp,
}

fn default_op() -> LayerOp { LayerOp::Install }

// ─────────────────────────────────────────────────────────────
//  Deployment
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deployment {
    pub id:               String,
    pub timestamp:        u64,
    pub osname:           String,
    pub checksum:         String,
    pub serial:           i32,
    #[serde(default)]
    pub booted:           bool,
    #[serde(default)]
    pub staged:           bool,
    #[serde(default)]
    pub pinned:           bool,
    pub origin_refspec:   String,
    #[serde(default)]
    pub layered_packages: Vec<PackageLayer>,
}

// ─────────────────────────────────────────────────────────────
//  TransactionResult
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct TransactionResult {
    pub success:         bool,
    pub new_checksum:    String,
    pub error_message:   String,
    pub requires_reboot: bool,
}

impl TransactionResult {
    pub fn ok(new_checksum: impl Into<String>, requires_reboot: bool) -> Self {
        TransactionResult {
            success: true,
            new_checksum: new_checksum.into(),
            error_message: String::new(),
            requires_reboot,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        TransactionResult {
            success: false,
            new_checksum: String::new(),
            error_message: msg.into(),
            requires_reboot: false,
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  Config
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Config {
    /// Ścieżka sysroot OSTree, zwykle "/".
    pub sysroot_path:     PathBuf,
    /// Ścieżka repo OSTree, zwykle "/ostree/repo".
    pub ostree_repo_path: PathBuf,
    /// Nazwa "systemu operacyjnego" w sensie OSTree (osname), np. "hackeros".
    pub osname:           String,
    /// Katalog roboczy sesji overlay (jedyna własna ścieżka hammer-oci).
    pub overlay_work_dir: PathBuf,
    /// Cache indeksów Packages — odpowiednik /var/lib/apt/lists.
    pub apt_lists_path:   PathBuf,
    /// Plik sources.list (czytany, nie zarządzany przez hammer-oci).
    pub apt_sources_list: PathBuf,
    /// Katalog sources.list.d/.
    pub apt_sources_dir:  PathBuf,
    /// Katalog kluczy GPG zaufanych.
    pub keyring_dir:      PathBuf,
    /// Architektura docelowa, domyślnie "amd64".
    pub arch:              String,
    /// Tryb confext dla plików /etc.
    pub confext_mode:      String,
    /// Zrodła apt — wypełniane z apt_sources_list/apt_sources_dir lub
    /// nadpisywane przez [apt] -> source_N w pliku .hk.
    pub apt_sources:       Vec<String>,
    /// Refspec obrazu bazowego OCI (origin), np.
    /// "hammer-oci:ghcr.io/example/image:trixie".
    pub origin_refspec:    Option<String>,
    /// `[apt] -> require_gpg` — jeśli `true`, brak/zły podpis GPG na
    /// `InRelease` lub niezgodność checksumy `Packages` z `InRelease`
    /// przerywa `hammer oci update`/`install`/itd. z twardym błędem
    /// zamiast tylko ostrzegać. Domyślnie `false` (spójne z zachowaniem
    /// `hammer sync` w pozostałych trybach), zalecane `true` w produkcji.
    pub require_gpg:       bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            sysroot_path:     PathBuf::from("/"),
            ostree_repo_path: PathBuf::from("/ostree/repo"),
            osname:           "hackeros".to_string(),
            overlay_work_dir: PathBuf::from("/var/lib/hammer/oci/overlay-work"),
            apt_lists_path:   PathBuf::from("/var/lib/hammer/oci/apt-cache"),
            apt_sources_list: PathBuf::from("/etc/apt/sources.list"),
            apt_sources_dir:  PathBuf::from("/etc/apt/sources.list.d"),
            keyring_dir:      PathBuf::from("/etc/apt/trusted.gpg.d"),
            arch:             "amd64".to_string(),
            confext_mode:     "none".to_string(),
            apt_sources:      Vec::new(),
            origin_refspec:   None,
            require_gpg:      false,
        }
    }
}
