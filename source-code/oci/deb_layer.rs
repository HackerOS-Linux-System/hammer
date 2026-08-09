use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::Path;

use crate::deb::DebPackage;

use super::types::Config;
use super::overlay::OverlaySession;
use super::process::run_inherit;
use super::repo_index::{self, Pool};
use super::status_db::{self, InstalledPackage};
use super::types::PackageLayer;

pub struct DebLayer<'a> {
    cfg: &'a Config,
}

impl<'a> DebLayer<'a> {
    pub fn new(cfg: &'a Config) -> Self {
        DebLayer { cfg }
    }

    /// Pobiera i parsuje indeksy `Packages` dla wszystkich skonfigurowanych
    /// `apt_sources` — odpowiednik `apt-get update`, ale bez apt.
    pub async fn refresh_package_index(&self) -> Result<Pool> {
        repo_index::refresh_index(self.cfg).await
    }

    /// Rozwiązuje zależności, pobiera wszystkie potrzebne `.deb` (z
    /// weryfikacją SHA256) i instaluje je (rozpakowanie + skrypty
    /// maintainer) w `session.merged_dir`. Zwraca listę WSZYSTKICH
    /// pakietów faktycznie zainstalowanych (podane + transytywne zależności).
    pub async fn install_packages(
        &self,
        session: &OverlaySession,
        names:   &[String],
    ) -> Result<Vec<PackageLayer>> {
        let pool = self.refresh_package_index().await?;
        let to_install = match repo_index::resolve_with_real_solver(&pool, &session.lower_dir, names) {
            Ok(plan) => {
                // A fresh install puts everything in `to_install`; if any
                // target (or transitive dep) is already present in the
                // rootfs at an older version, the solver correctly routes
                // it through `to_upgrade` instead — both need fetching.
                let mut pkgs = plan.to_install;
                pkgs.extend(plan.to_upgrade);
                pkgs
            }
            Err(e) => {
                crate::log::warn(&format!(
                    "oci: CDCL resolution failed ({e:#}), falling back to the \
                     simpler BFS dependency closure — this loses proper \
                     Conflicts:/alternative handling for this install"
                ));
                repo_index::resolve_closure(&pool, names)
                    .context("Resolving dependency closure (fallback)")?
            }
        };

        let client = crate::download::HttpClient::new();
        let mut installed = Vec::new();

        for pkg in &to_install {
            if status_db::is_installed(&session.merged_dir, &pkg.name) {
                continue;
            }
            let Some(url) = pool.deb_url(&pkg.name) else {
                bail!("No download URL (Filename:) for package '{}'", pkg.name);
            };

            crate::log::info(&format!("oci: fetching {} {}", pkg.name, pkg.version));
            let bytes = client.get_bytes(&url).await
                .with_context(|| format!("Downloading {}", url))?;

            if let Some(expected_sha256) = &pkg.sha256 {
                let mut hasher = Sha256::new();
                hasher.update(&bytes);
                let got = hex::encode(hasher.finalize());
                if &got != expected_sha256 {
                    bail!(
                        "SHA256 mismatch for {}: expected {}, got {}",
                        pkg.name, expected_sha256, got
                    );
                }
            }

            let deb = DebPackage::parse(&bytes)
                .with_context(|| format!("Parsing {}.deb", pkg.name))?;
            let extract = deb.unpack(&session.merged_dir)
                .with_context(|| format!("Unpacking {} into {}", pkg.name, session.merged_dir.display()))?;

            if let Some(script) = &deb.postinst {
                self.run_maintainer_script(session, &pkg.name, script, "postinst")?;
            }

            status_db::upsert(&session.merged_dir, &InstalledPackage {
                name:           pkg.name.clone(),
                version:        pkg.version.clone(),
                architecture:   pkg.architecture.clone(),
                maintainer:     pkg.maintainer.clone().unwrap_or_default(),
                description:    pkg.description_short.clone().unwrap_or_default(),
                depends:        pkg.depends.clone().unwrap_or_default(),
                pre_depends:    pkg.pre_depends.clone().unwrap_or_default(),
                provides:       pkg.provides.clone().unwrap_or_default(),
                section:        pkg.section.clone().unwrap_or_default(),
                priority:       pkg.priority.clone().unwrap_or_default(),
                installed_size: pkg.installed_size_kb.unwrap_or(0) * 1024,
                files:          extract.all_files.iter().map(|p| p.display().to_string()).collect(),
                status:         "install ok installed".to_string(),
                installed_by:   "hammer-oci".to_string(),
            })?;

            // apt-compatible bookkeeping: packages the caller explicitly
            // asked for by name are "manual"; anything pulled in only to
            // satisfy a dependency is "auto" — this is what makes
            // `hammer oci autoremove` (and, if someone chroots in, real
            // `apt autoremove`) able to tell the two apart later.
            let explicitly_requested = names.iter().any(|n| n == &pkg.name);
            status_db::set_auto_installed(
                &session.merged_dir, &pkg.name, &pkg.architecture, !explicitly_requested,
            )?;

            installed.push(PackageLayer {
                name:    pkg.name.clone(),
                version: pkg.version.clone(),
                op:      super::types::LayerOp::Install,
            });
        }

        Ok(installed)
    }

    /// Usuwa pliki należące do `names` z `session.merged_dir` według listy
    /// plików zapisanej w `status_db` przy instalacji. Wykonuje `prerm`/
    /// `postrm` jeśli pakiet je ma (odczytane z `dpkg/info/<pkg>.{prerm,postrm}`
    /// zapisanych przy instalacji — patrz uwaga w `run_maintainer_script`).
    pub fn remove_packages(&self, session: &OverlaySession, names: &[String]) -> Result<()> {
        for name in names {
            let all = status_db::load_all(&session.merged_dir)?;
            let Some(pkg) = all.iter().find(|p| &p.name == name) else {
                crate::log::warn(&format!("oci: '{}' not installed, skipping removal", name));
                continue;
            };
            for file in pkg.files.iter().rev() {
                let path = session.merged_dir.join(file.trim_start_matches('/'));
                if path.is_dir() {
                    let _ = std::fs::remove_dir(&path); // only if empty
                } else {
                    let _ = std::fs::remove_file(&path);
                }
            }
            status_db::remove(&session.merged_dir, name)?;
            let _ = status_db::set_auto_installed(&session.merged_dir, name, "", false);
        }
        Ok(())
    }

    /// Sprawdza przez `status_db` czy pakiet jest zainstalowany w danym rootfs.
    pub fn is_installed(&self, rootfs_path: &Path, package_name: &str) -> bool {
        status_db::is_installed(rootfs_path, package_name)
    }

    /// Wykonuje skrypt maintainer wewnątrz `chroot` względem
    /// `session.merged_dir`. Jedyne miejsce w `DebLayer`, gdzie wywoływany
    /// jest proces zewnętrzny ("chroot") — bo skrypty maintainer `.deb` są
    /// dowolnym kodem powłoki zakładającym wykonanie wewnątrz docelowego
    /// systemu i nie da się ich bezpiecznie "zinterpretować" bez realnego
    /// chroot.
    fn run_maintainer_script(
        &self,
        session:        &OverlaySession,
        package_name:   &str,
        script_content: &str,
        script_name:    &str,
    ) -> Result<()> {
        let info_dir = session.merged_dir.join("var/lib/dpkg/info");
        std::fs::create_dir_all(&info_dir)?;
        let script_path_host = info_dir.join(format!("{package_name}.{script_name}"));
        std::fs::write(&script_path_host, script_content)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path_host)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path_host, perms)?;
        }

        let script_path_chroot = format!("/var/lib/dpkg/info/{package_name}.{script_name}");
        run_inherit(
            "chroot",
            &[&session.merged_dir.to_string_lossy(), &script_path_chroot, "configure"],
        ).with_context(|| format!("Running {script_name} for {package_name} in chroot"))?;
        Ok(())
    }
}
