use anyhow::{bail, Context, Result};
use std::path::PathBuf;

use super::process::{run_capture, run_inherit_in};

pub struct OciPuller {
    work_dir: PathBuf,
}

impl OciPuller {
    /// `work_dir`: katalog na tymczasowe warstwy OCI i rootfs (powinien mieć
    /// co najmniej kilka GB wolnego miejsca, ten sam filesystem co `/ostree`
    /// żeby `ostree commit` mógł liczyć na hardlinki zamiast kopii).
    pub fn new(work_dir: PathBuf) -> Self {
        OciPuller { work_dir }
    }

    /// Sprawdza dostępność `skopeo` i `podman` w `$PATH`.
    pub fn check_tools_available() -> Result<()> {
        let missing: Vec<&str> = ["skopeo", "podman"]
            .into_iter()
            .filter(|t| !super::process::tool_available(t))
            .collect();
        if !missing.is_empty() {
            bail!(
                "hammer oci requires the following tool(s) which were not found in $PATH: {}.\n  \
                 Install them (e.g. `apt install skopeo podman`) before pulling OCI base images.",
                missing.join(", ")
            );
        }
        Ok(())
    }

    /// Ściąga `image_ref` (np. `registry.example.com/debian-bootc:bookworm`),
    /// scala warstwy OCI z poprawną obsługą whiteoutów i zwraca ścieżkę do
    /// katalogu z rozpakowanym rootfs, gotowym do `Repo::commit_directory`.
    /// Katalog zwrócony należy do wołającego — odpowiada za jego usunięcie.
    pub fn pull_and_unpack(&self, image_ref: &str) -> Result<PathBuf> {
        Self::check_tools_available()?;
        std::fs::create_dir_all(&self.work_dir)
            .with_context(|| format!("mkdir -p {}", self.work_dir.display()))?;

        let oci_layout_dir = self.work_dir.join("oci-layout");
        let rootfs_dir      = self.work_dir.join("rootfs");
        if oci_layout_dir.exists() { std::fs::remove_dir_all(&oci_layout_dir)?; }
        if rootfs_dir.exists() { std::fs::remove_dir_all(&rootfs_dir)?; }

        // 1) skopeo copy <src> oci:<oci_layout_dir>:<tag>
        let dest = format!("oci:{}:latest", oci_layout_dir.display());
        run_inherit_in(
            "skopeo",
            &["copy", &format!("docker://{image_ref}"), &dest],
            &self.work_dir,
        ).with_context(|| format!("skopeo copy docker://{image_ref} {dest}"))?;

        // 2) podman: create a throwaway container from the OCI layout and
        //    export its unified rootfs (handles OCI whiteouts correctly).
        let container_ref = format!("oci-archive-import-{}", std::process::id());
        let create_out = run_capture(
            "podman",
            &["create", "--name", &container_ref, &format!("oci-archive:{}", oci_layout_dir.display())],
        );
        // Fallback: some podman/skopeo combinations need `containers-storage:`
        // instead of `oci-archive:` for a plain-directory OCI layout — try
        // pulling into local storage directly if the archive path failed.
        let container_ref = if create_out.as_ref().map(|r| r.status_ok).unwrap_or(false) {
            container_ref
        } else {
            run_inherit_in(
                "skopeo",
                &["copy", &dest, &format!("containers-storage:localhost/hammer-oci-base:{}", std::process::id())],
                &self.work_dir,
            )?;
            let tag = format!("localhost/hammer-oci-base:{}", std::process::id());
            run_inherit_in("podman", &["create", "--name", &container_ref, &tag], &self.work_dir)?;
            container_ref
        };

        std::fs::create_dir_all(&rootfs_dir)?;
        // `podman export` streams a tar of the merged rootfs to stdout.
        let export = std::process::Command::new("podman")
            .args(["export", &container_ref])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .context("spawn podman export")?;
        let tar_status = std::process::Command::new("tar")
            .args(["-xpf", "-", "-C"])
            .arg(&rootfs_dir)
            .stdin(export.stdout.unwrap())
            .status()
            .context("tar -xpf (podman export | tar)")?;
        if !tar_status.success() {
            bail!("Extracting exported OCI rootfs failed (tar exit {tar_status})");
        }
        let _ = run_capture("podman", &["rm", "-f", &container_ref]);

        Ok(rootfs_dir)
    }
}
