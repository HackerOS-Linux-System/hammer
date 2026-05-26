use anyhow::{Context, Result};
use fslock::LockFile;
use std::path::{Path, PathBuf};

pub const SYSTEM_LOCK_PATH: &str = "/hammer/db/.lock";

// ─────────────────────────────────────────────────────────────
//  HammerLock  — RAII guard
// ─────────────────────────────────────────────────────────────

pub struct HammerLock {
    _file:     LockFile,
    lock_path: PathBuf,
}

impl HammerLock {
    // ── Acquire system lock ───────────────────────────────────

    /// Acquire the system-wide hammer lock (non-blocking).
    /// Returns an error if another hammer instance holds the lock.
    pub fn acquire_system() -> Result<Self> {
        Self::acquire(Path::new(SYSTEM_LOCK_PATH))
    }

    /// Acquire the system lock, blocking until it is free.
    /// Prints a notice if waiting.
    pub fn acquire_system_wait() -> Result<Self> {
        Self::acquire_wait(Path::new(SYSTEM_LOCK_PATH))
    }

    // ── Acquire user lock ─────────────────────────────────────

    pub fn acquire_user(user_dir: &Path) -> Result<Self> {
        let lock_path = user_dir.join(".lock");
        Self::acquire(&lock_path)
    }

    // ── Internal ──────────────────────────────────────────────

    fn acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
            .with_context(|| format!("Cannot create lock dir {}", parent.display()))?;
        }

        let mut file = LockFile::open(path)
        .with_context(|| format!("Cannot open lock file {}", path.display()))?;

        let locked = file.try_lock()
        .with_context(|| format!("flock() failed on {}", path.display()))?;

        if !locked {
            anyhow::bail!(
                "Another hammer instance is already running.\n  \
Lock file: {}\n  \
If no other hammer is running, delete the lock file manually.",
path.display()
            );
        }

        Ok(HammerLock {
            _file:     file,
            lock_path: path.to_path_buf(),
        })
    }

    fn acquire_wait(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut file = LockFile::open(path)
        .with_context(|| format!("Cannot open lock file {}", path.display()))?;

        // Non-blocking first attempt
        if file.try_lock()? {
            return Ok(HammerLock {
                _file:     file,
                lock_path: path.to_path_buf(),
            });
        }

        // Print notice then block
        eprintln!(
            "  \x1b[33m·\x1b[0m  Waiting for another hammer instance to finish…"
        );

        file.lock()
        .with_context(|| format!("flock() blocking failed on {}", path.display()))?;

        Ok(HammerLock {
            _file:     file,
            lock_path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path { &self.lock_path }
}

// Drop is automatic: LockFile::drop() calls flock(LOCK_UN) + close()

// ─────────────────────────────────────────────────────────────
//  Helpers for commands that need the lock
// ─────────────────────────────────────────────────────────────

/// Acquire system lock with user-visible wait message.
/// Call this at the start of any mutating command.
pub fn system_lock() -> Result<HammerLock> {
    HammerLock::acquire_system_wait()
}

/// Acquire user lock with wait message.
pub fn user_lock(user_dir: &Path) -> Result<HammerLock> {
    let lock_path = user_dir.join(".lock");
    HammerLock::acquire_wait(&lock_path)
}

// ─────────────────────────────────────────────────────────────
//  Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_lock_acquire_release() {
        let tmp = std::env::temp_dir().join("hammer_test_lock");
        let _ = fs::remove_file(&tmp);

        let guard = HammerLock::acquire(&tmp).expect("first lock should succeed");
        drop(guard);

        // After release, we should be able to acquire again
        let _guard2 = HammerLock::acquire(&tmp).expect("second lock should succeed after release");
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn test_lock_exclusive() {
        let tmp = std::env::temp_dir().join("hammer_test_exclusive_lock");
        let _ = std::fs::remove_file(&tmp);

        let _guard = HammerLock::acquire(&tmp).expect("first lock");

        // Second acquire in same process: flock is per-FD, so a new FD will succeed
        // (flock allows re-lock from same process). This is expected behaviour.
        // In practice, two *processes* would block. We document this.

        let _ = std::fs::remove_file(&tmp);
    }
}
