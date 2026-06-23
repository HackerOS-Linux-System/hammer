use anyhow::{Context, Result};
use fslock::LockFile;
use std::path::{Path, PathBuf};

/// Lock path adapts to build mode.
pub fn system_lock_path() -> &'static str {
    #[cfg(feature = "normal-mode")]
    { "/var/lib/hammer/.lock" }
    #[cfg(not(feature = "normal-mode"))]
    { "/hammer/db/.lock" }
}

pub struct HammerLock {
    _file:     LockFile,
    lock_path: PathBuf,
}

impl HammerLock {
    pub fn acquire_system() -> Result<Self> {
        Self::acquire(Path::new(system_lock_path()))
    }

    pub fn acquire_system_wait() -> Result<Self> {
        Self::acquire_wait(Path::new(system_lock_path()))
    }

    pub fn acquire_user(user_dir: &Path) -> Result<Self> {
        Self::acquire(&user_dir.join(".lock"))
    }

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
                "Another hammer instance is already running.\n  Lock: {}\n  \
                 If no other hammer is running, remove the lock file.",
                path.display()
            );
        }
        Ok(HammerLock { _file: file, lock_path: path.to_path_buf() })
    }

    fn acquire_wait(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = LockFile::open(path)
            .with_context(|| format!("Cannot open lock file {}", path.display()))?;
        if file.try_lock()? {
            return Ok(HammerLock { _file: file, lock_path: path.to_path_buf() });
        }
        eprintln!("  {}  Waiting for another hammer instance to finish…",
                  "\x1b[33m·\x1b[0m");
        file.lock()
            .with_context(|| format!("flock() blocking failed on {}", path.display()))?;
        Ok(HammerLock { _file: file, lock_path: path.to_path_buf() })
    }

    pub fn path(&self) -> &Path { &self.lock_path }
}

pub fn system_lock() -> Result<HammerLock> {
    HammerLock::acquire_system_wait()
}

pub fn user_lock(user_dir: &Path) -> Result<HammerLock> {
    HammerLock::acquire_wait(&user_dir.join(".lock"))
}
