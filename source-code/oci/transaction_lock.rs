use anyhow::{bail, Context, Result};
use fslock::LockFile;
use std::path::{Path, PathBuf};

pub struct TransactionLock {
    _file:            LockFile,
    lock_path:        PathBuf,
    incomplete_path:  PathBuf,
    found_incomplete: bool,
}

impl TransactionLock {
    /// Próbuje uzyskać wyłączną blokadę na `<lock_dir>/lock`. Zwraca błąd
    /// jeśli blokada jest zajęta. Sprawdza też `.incomplete` — jeśli
    /// istnieje, poprzednia transakcja została przerwana.
    pub fn acquire(lock_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(lock_dir)
            .with_context(|| format!("mkdir -p {}", lock_dir.display()))?;

        let lock_path = lock_dir.join("lock");
        let incomplete_path = lock_dir.join(".incomplete");
        let found_incomplete = incomplete_path.exists();

        let mut file = LockFile::open(&lock_path)
            .with_context(|| format!("Cannot open lock file {}", lock_path.display()))?;
        let locked = file.try_lock()
            .with_context(|| format!("flock() failed on {}", lock_path.display()))?;
        if !locked {
            bail!(
                "Another 'hammer oci' transaction is already running.\n  Lock: {}\n  \
                 If no other instance is running, remove the lock file.",
                lock_path.display()
            );
        }

        // Mark this transaction as in-progress.
        std::fs::write(&incomplete_path, format!("pid={}\n", std::process::id())).ok();

        Ok(TransactionLock { _file: file, lock_path, incomplete_path, found_incomplete })
    }

    /// Oznacza transakcję jako zakończoną (usuwa `.incomplete`). Wywoływać
    /// PO pomyślnym zakończeniu wszystkich operacji.
    pub fn mark_complete(&self) {
        let _ = std::fs::remove_file(&self.incomplete_path);
    }

    /// `true` jeśli wykryto `.incomplete` z poprzedniego, przerwanego
    /// uruchomienia. Caller powinien zaproponować `hammer oci cleanup --repair`.
    pub fn found_incomplete(&self) -> bool { self.found_incomplete }

    pub fn lock_path(&self) -> &Path { &self.lock_path }
}
