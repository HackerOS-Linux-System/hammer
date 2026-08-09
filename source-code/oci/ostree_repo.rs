use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use super::ffi;

#[derive(Debug, Clone, Default)]
pub struct CommitInfo {
    pub checksum:     String,
    pub subject:      String,
    pub body:         String,
    pub timestamp:    i64,
    pub content_size: u64,
}

pub struct Repo {
    pub path:   PathBuf,
    inner:      ffi::Repo,
}

impl Repo {
    /// Otwiera istniejące repo OSTree (błąd jeśli `path/config` nie istnieje).
    pub fn open(path: &Path) -> Result<Self> {
        let inner = ffi::Repo::open_at(path)
            .with_context(|| format!(
                "Opening OSTree repo at {}. If it doesn't exist yet, run \
                 'hammer oci deploy <image>' first (it will initialise it).",
                path.display()
            ))?;
        Ok(Repo { path: path.to_path_buf(), inner })
    }

    /// Tworzy nowe repo w trybie `bare-user` (przechowuje uid/gid/xattrs bez
    /// wymogu identycznych uprawnień na hoście — dokładnie jak oryginał).
    pub fn create(path: &Path) -> Result<Self> {
        let inner = ffi::Repo::create_at(path)
            .with_context(|| format!("Creating OSTree repo at {}", path.display()))?;
        Ok(Repo { path: path.to_path_buf(), inner })
    }

    /// Otwiera repo jeśli istnieje, w przeciwnym razie tworzy nowe.
    pub fn open_or_create(path: &Path) -> Result<Self> {
        let inner = ffi::Repo::open_or_create(path)?;
        Ok(Repo { path: path.to_path_buf(), inner })
    }

    /// Commituje `dir_path` jako nowy commit OSTree pod `refspec`.
    /// Zwraca checksum (sha256) nowego commita.
    pub fn commit_directory(
        &self,
        dir_path: &Path,
        refspec:  &str,
        subject:  &str,
        body:     &str,
    ) -> Result<String> {
        self.inner.commit_directory(dir_path, refspec, subject, body)
            .with_context(|| format!("Committing {} as {refspec}", dir_path.display()))
    }

    /// Rozwija ref na checksum najnowszego commita.
    pub fn resolve_ref(&self, refspec: &str) -> Result<Option<String>> {
        self.inner.resolve_rev(refspec)
    }

    /// Checkout commita `checksum` do `dest_dir`, `--union` + `--whiteouts`
    /// (obsługa OCI-style whiteouts przy warstwach nakładanych), tryb "user"
    /// (bare-user-only, bez wymogu roota na hoście dla checkout).
    pub fn checkout_commit(&self, checksum: &str, dest_dir: &Path) -> Result<()> {
        self.inner.checkout_at(checksum, dest_dir)
            .with_context(|| format!("Checking out {checksum} -> {}", dest_dir.display()))
    }

    /// Pełne metadane commita (subject/body/timestamp) — czytane przez
    /// `ostree_repo_load_commit` + rozpakowanie `GVariant`
    /// (`oci::ffi::Repo::load_commit_metadata`).
    pub fn read_commit_info(&self, checksum: &str) -> Result<CommitInfo> {
        let meta = self.inner.load_commit_metadata(checksum)
            .with_context(|| format!("Reading commit metadata for {checksum}"))?;
        Ok(CommitInfo {
            checksum:     meta.checksum,
            subject:      meta.subject,
            body:         meta.body,
            timestamp:    meta.timestamp,
            content_size: 0, // not exposed by the commit GVariant itself; would need ostree_repo_traverse_commit to compute
        })
    }

    /// Weryfikuje integralność repo. `slow_fsck=true` uruchamia pełny
    /// `ostree_repo_fsck` (wolne, pełne przejście po wszystkich obiektach).
    /// Weryfikuje integralność repo. `slow_fsck=true` uruchamia pełny
    /// przegląd (`Repo::fsck` — patrz dokumentacja w `oci/ffi/mod.rs` po
    /// dokładny opis, co realnie sprawdza).
    pub fn check_integrity(&self, slow_fsck: bool) -> Result<()> {
        if !self.path.join("config").exists() {
            anyhow::bail!("OSTree repo config missing at {}", self.path.join("config").display());
        }
        if slow_fsck {
            let (refs, objects) = self.inner.fsck()?;
            crate::log::info(&format!(
                "oci: fsck OK — {refs} ref(s), {objects} reachable object(s) verified"
            ));
        }
        Ok(())
    }

    /// Usuwa nieosiągalne obiekty (wywoływane przez cleanup).
    pub fn prune(&self, _keep_refs_older_than_days: Option<u32>) -> Result<()> {
        self.inner.prune()
    }
}
