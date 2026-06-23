use owo_colors::OwoColorize;

// ──────────────────────────────────────────────────────────────────────────────
//  Compile-time constants
// ──────────────────────────────────────────────────────────────────────────────

/// `true`  → normal-mode build (atomicity disabled)
/// `false` → atomic build     (default)
pub const NORMAL_MODE: bool = cfg!(feature = "normal-mode");

/// Human-readable mode string shown in version output.
pub const MODE_LABEL: &str = if cfg!(feature = "normal-mode") {
    "normal"
} else {
    "atomic"
};

/// Binary name / description displayed to the user.
pub const BINARY_DESCRIPTION: &str = if cfg!(feature = "normal-mode") {
    "hammer (normal-mode) — HackerOS package manager"
} else {
    "hammer (atomic)      — HackerOS atomic package manager"
};

// ──────────────────────────────────────────────────────────────────────────────
//  Runtime guards
// ──────────────────────────────────────────────────────────────────────────────

/// Returns `true` if atomic features (immutable FS, generation store) are
/// active in this build.
#[inline]
pub fn atomic_enabled() -> bool {
    !NORMAL_MODE
}

/// Print a one-line build-mode banner (used in `hammer version`).
pub fn print_mode_banner() {
    if NORMAL_MODE {
        println!(
            "  {} Build mode : {}  (atomicity and immutable FS disabled)",
            "·".dimmed(),
            "normal".yellow().bold()
        );
    } else {
        println!(
            "  {} Build mode : {}  (immutable FS + generation rollback active)",
            "·".dimmed(),
            "atomic".bright_cyan().bold()
        );
    }
}

/// Called before any operation that requires the atomic store.
/// In normal-mode this is a no-op; in atomic mode it verifies the
/// store is properly initialised.
pub fn require_atomic(op: &str) -> anyhow::Result<()> {
    if NORMAL_MODE {
        // Normal mode: silently skip atomic-only prerequisites
        return Ok(());
    }
    // Atomic mode: ensure /hammer/store exists and is a directory
    let store_path = std::path::Path::new("/hammer/store");
    if !store_path.exists() {
        anyhow::bail!(
            "{} requires the hammer store to be initialised.\n  \
             Run: hammer init",
            op
        );
    }
    Ok(())
}

/// Returns the path to the package staging area.
/// Atomic build → /hammer/tmp  (on the read-only store filesystem, unlocked
///                               transiently during installs)
/// Normal build  → /tmp/hammer
pub fn staging_dir() -> std::path::PathBuf {
    if NORMAL_MODE {
        std::path::PathBuf::from("/tmp/hammer")
    } else {
        std::path::PathBuf::from("/hammer/tmp")
    }
}

/// Returns the path used for the package database.
pub fn db_path() -> std::path::PathBuf {
    if NORMAL_MODE {
        std::path::PathBuf::from("/var/lib/hammer/db.sqlite")
    } else {
        std::path::PathBuf::from("/hammer/db/packages.sqlite")
    }
}

/// Returns the path for the dpkg status file (normal-mode compat).
pub fn dpkg_status_path() -> &'static str {
    "/var/lib/dpkg/status"
}

// ──────────────────────────────────────────────────────────────────────────────
//  Feature availability checks (runtime, not compile-time)
// ──────────────────────────────────────────────────────────────────────────────

pub struct Features {
    pub atomicity:    bool,
    pub immutable_fs: bool,
    pub generations:  bool,
    pub ro_store:     bool,
    pub snapshots:    bool,
    pub sat_solver:   bool,   // always true
    pub gpg_verify:   bool,   // always true
    pub multi_arch:   bool,   // always true
    pub pinning:      bool,   // always true
    pub services:     bool,   // always true
    pub sandbox:      bool,   // always true
    pub doctor:       bool,   // always true
}

impl Features {
    pub fn current() -> Self {
        Features {
            atomicity:    !NORMAL_MODE,
            immutable_fs: !NORMAL_MODE,
            generations:  !NORMAL_MODE,
            ro_store:     !NORMAL_MODE,
            snapshots:    !NORMAL_MODE,
            sat_solver:   true,
            gpg_verify:   true,
            multi_arch:   true,
            pinning:      true,
            services:     true,
            sandbox:      true,
            doctor:       true,
        }
    }

    pub fn print(&self) {
        let yes  = "✔".bright_green().to_string();
        let no   = "✘".dimmed().to_string();
        let mark = |b: bool| if b { yes.clone() } else { no.clone() };

        println!("  {}", "Feature flags:".bold());
        println!("    {} Atomicity & generation rollback", mark(self.atomicity));
        println!("    {} Immutable filesystem (btrfs/zfs/remount)", mark(self.immutable_fs));
        println!("    {} Read-only hammer store", mark(self.ro_store));
        println!("    {} Btrfs/ZFS snapshots", mark(self.snapshots));
        println!("    {} CDCL SAT dependency solver", mark(self.sat_solver));
        println!("    {} GPG/Ed25519 signature verification", mark(self.gpg_verify));
        println!("    {} Multi-arch (amd64/arm64/…)", mark(self.multi_arch));
        println!("    {} Package pinning", mark(self.pinning));
        println!("    {} Service management (systemd)", mark(self.services));
        println!("    {} Namespace sandbox (seccomp + namespaces)", mark(self.sandbox));
        println!("    {} System doctor & self-repair", mark(self.doctor));
    }
}
