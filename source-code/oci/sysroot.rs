use anyhow::{Context, Result};
use std::path::PathBuf;

use super::ffi;
use super::ostree_repo::Repo;
use super::types::{Deployment, PackageLayer, TransactionResult};

pub struct Sysroot {
    pub path:   PathBuf,
    pub repo:   Repo,
    pub osname: String,
    inner:      ffi::Sysroot,
}

impl Sysroot {
    /// Otwiera sysroot w `path` (zwykle "/"). Ładuje też wbudowane repo OSTree.
    pub fn open(path: &std::path::Path, repo_path: &std::path::Path, osname: &str) -> Result<Self> {
        let repo = Repo::open_or_create(repo_path)?;
        let inner = ffi::Sysroot::load(path)
            .with_context(|| format!("Loading OSTree sysroot at {}", path.display()))?;
        Ok(Sysroot { path: path.to_path_buf(), repo, osname: osname.to_string(), inner })
    }

    /// Lista wszystkich zarejestrowanych deploymentów, od najnowszego
    /// (index 0) do najstarszego — jak `rpm-ostree status` i menu GRUB.
    pub fn list_deployments(&self) -> Result<Vec<Deployment>> {
        let infos = self.inner.deployments()
            .context("ostree_sysroot_get_deployments")?;
        Ok(infos.into_iter().enumerate().map(|(idx, d)| {
            let (_, layered_packages) = self.read_layer_metadata(&d.checksum);
            Deployment {
                id: format!("{}.{}", d.checksum, d.serial),
                timestamp: 0,
                osname: d.osname,
                checksum: d.checksum,
                serial: d.serial,
                booted: d.booted,
                staged: idx == 0 && !d.booted,
                pinned: d.pinned,
                origin_refspec: d.origin_refspec,
                layered_packages,
            }
        }).collect())
    }

    /// Aktualnie zabootowany deployment.
    pub fn booted_deployment(&self) -> Result<Option<Deployment>> {
        Ok(self.list_deployments()?.into_iter().find(|d| d.booted))
    }

    /// Tworzy nowy deployment z commita `checksum`, rejestruje jako staged
    /// (wchodzi w życie po reboot). Poprzedni deployment pozostaje dostępny
    /// do rollbacku — to jest domyślne zachowanie `ostree_sysroot_deploy_tree`
    /// + `ostree_sysroot_simple_write_deployment` (patrz `oci::ffi::Sysroot::deploy`).
    pub fn deploy_commit(
        &self,
        checksum:         &str,
        origin_refspec:   &str,
        layered_packages: &[PackageLayer],
    ) -> Result<TransactionResult> {
        match self.inner.deploy(&self.osname, checksum, origin_refspec) {
            Ok(()) => {
                if let Err(e) = self.write_layer_metadata(checksum, origin_refspec, layered_packages) {
                    crate::log::warn(&format!("oci: could not write layer metadata: {e:#}"));
                }
                Ok(TransactionResult::ok(checksum, true))
            }
            Err(e) => Ok(TransactionResult::err(format!("{e:#}"))),
        }
    }

    /// Rollback: usuwa deployment o indeksie 0 (najnowszy — ten, który
    /// właśnie chcemy cofnąć), przesuwając poprzedni na jego miejsce.
    /// W przeciwieństwie do wcześniejszej wersji CLI (która re-deployowała
    /// stary commit jako NOWY wpis), to jest prawdziwe, atomowe zdjęcie
    /// wpisu przez `ostree_sysroot_deployment_delete_index` — dokładnie to,
    /// co `ostree admin undeploy 0` robi wewnętrznie, i bliżej oczekiwanej
    /// semantyki "rollback" niż poprzednie obejście.
    pub fn rollback(&self) -> Result<TransactionResult> {
        let deployments = self.list_deployments()?;
        if deployments.len() < 2 {
            return Ok(TransactionResult::err(
                "No previous deployment available to roll back to.",
            ));
        }
        if deployments[0].pinned {
            return Ok(TransactionResult::err(
                "The current deployment is pinned; unpin it first if you really want to roll back past it.",
            ));
        }
        match self.inner.undeploy(0) {
            Ok(()) => {
                let new_top = self.list_deployments()?.into_iter().next();
                Ok(TransactionResult::ok(new_top.map(|d| d.checksum).unwrap_or_default(), true))
            }
            Err(e) => Ok(TransactionResult::err(format!("{e:#}"))),
        }
    }

    /// Usuwa deploymenty starsze niż `keep_last_n` i uruchamia prune na repo.
    pub fn cleanup(&self, keep_last_n: usize) -> Result<TransactionResult> {
        let deployments = self.list_deployments()?;
        if deployments.len() <= keep_last_n {
            self.repo.prune(None)?;
            return Ok(TransactionResult::ok("", false));
        }
        // Remove from the end (oldest) so earlier indices don't shift under
        // us mid-loop; skip pinned entries.
        for idx in (keep_last_n..deployments.len()).rev() {
            let dep = &deployments[idx];
            if dep.pinned {
                crate::log::info(&format!("oci cleanup: skipping pinned deployment {}", dep.id));
                continue;
            }
            if let Err(e) = self.inner.undeploy(idx) {
                return Ok(TransactionResult::err(format!("{e:#}")));
            }
        }
        self.repo.prune(None)?;
        Ok(TransactionResult::ok("", false))
    }

    /// Ustawia/zdejmuje pin na deploymencie o danym `checksum`/id.
    pub fn set_pinned(&self, deployment_id: &str, pinned: bool) -> Result<TransactionResult> {
        let deployments = self.list_deployments()?;
        let Some(idx) = deployments.iter().position(|d| d.id == deployment_id || d.checksum.starts_with(deployment_id)) else {
            return Ok(TransactionResult::err(format!("No such deployment: {deployment_id}")));
        };
        match self.inner.set_pinned(idx, pinned) {
            Ok(()) => Ok(TransactionResult::ok(deployments[idx].checksum.clone(), false)),
            Err(e) => Ok(TransactionResult::err(format!("{e:#}"))),
        }
    }

    /// Fizyczna ścieżka katalogu deploymentu na dysku:
    /// `/ostree/deploy/<osname>/deploy/<checksum>.<serial>`.
    pub fn deployment_path(&self, dep: &Deployment) -> PathBuf {
        self.path
            .join("ostree/deploy")
            .join(&self.osname)
            .join("deploy")
            .join(format!("{}.{}", dep.checksum, dep.serial))
    }

    /// Sprawdza integralność repo OSTree.
    pub fn check_repo_integrity(&self, slow_fsck: bool) -> Result<()> {
        self.repo.check_integrity(slow_fsck)
    }

    fn layer_metadata_path(&self) -> PathBuf {
        self.path
            .join("ostree/deploy")
            .join(&self.osname)
            .join("hammer-layers.json")
    }

    fn write_layer_metadata(
        &self,
        checksum: &str,
        origin_refspec: &str,
        layered_packages: &[PackageLayer],
    ) -> Result<()> {
        let path = self.layer_metadata_path();
        let mut all: std::collections::HashMap<String, serde_json::Value> =
            if path.exists() {
                serde_json::from_str(&std::fs::read_to_string(&path)?).unwrap_or_default()
            } else {
                std::collections::HashMap::new()
            };
        all.insert(
            checksum.to_string(),
            serde_json::json!({
                "origin_refspec": origin_refspec,
                "layered_packages": layered_packages,
            }),
        );
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
        std::fs::write(&path, serde_json::to_string_pretty(&all)?)?;
        Ok(())
    }

    /// Odczytuje warstwy pakietów zapisane dla danego commita (jeśli są).
    pub fn read_layer_metadata(&self, checksum: &str) -> (String, Vec<PackageLayer>) {
        let path = self.layer_metadata_path();
        let Ok(text) = std::fs::read_to_string(&path) else { return (String::new(), Vec::new()) };
        let Ok(all): Result<std::collections::HashMap<String, serde_json::Value>, _> =
            serde_json::from_str(&text) else { return (String::new(), Vec::new()) };
        let Some(entry) = all.get(checksum) else { return (String::new(), Vec::new()) };
        let origin = entry.get("origin_refspec").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let layers: Vec<PackageLayer> = entry.get("layered_packages")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        (origin, layers)
    }
}
