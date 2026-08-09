use anyhow::{Context, Result};
use nix::mount::{mount, umount2, MntFlags, MsFlags};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct OverlaySession {
    pub lower_dir:  PathBuf,
    pub upper_dir:  PathBuf,
    pub work_dir:   PathBuf,
    pub merged_dir: PathBuf,
    pub mounted:    bool,
}

pub struct OverlayManager {
    work_root: PathBuf,
}

impl OverlayManager {
    /// `work_root`: katalog roboczy (np. `/var/lib/hammer/oci/overlay-work`).
    /// Wszystkie podkatalogi (`upper/work/merged`) tworzone są wewnątrz.
    pub fn new(work_root: PathBuf) -> Self {
        OverlayManager { work_root }
    }

    /// Tworzy `upper/work/merged` w `work_root`, montuje overlayfs z
    /// `lower_dir` jako bazę (read-only). Wymaga `CAP_SYS_ADMIN` (root).
    pub fn begin_session(&self, lower_dir: &Path) -> Result<OverlaySession> {
        let session_root = self.work_root.join("session");
        if session_root.exists() {
            std::fs::remove_dir_all(&session_root)
                .with_context(|| format!("Cleaning stale overlay session {}", session_root.display()))?;
        }
        let upper = session_root.join("upper");
        let work  = session_root.join("work");
        let merged = session_root.join("merged");
        for d in [&upper, &work, &merged] {
            std::fs::create_dir_all(d).with_context(|| format!("mkdir -p {}", d.display()))?;
        }

        let opts = format!(
            "lowerdir={},upperdir={},workdir={}",
            lower_dir.display(), upper.display(), work.display()
        );
        mount(
            Some("overlay"),
            &merged,
            Some("overlay"),
            MsFlags::empty(),
            Some(opts.as_str()),
        ).with_context(|| format!("mount overlay -> {} ({})", merged.display(), opts))?;

        Ok(OverlaySession {
            lower_dir: lower_dir.to_path_buf(),
            upper_dir: upper,
            work_dir: work,
            merged_dir: merged,
            mounted: true,
        })
    }

    /// Bind-mountuje `/proc`, `/sys`, `/dev`, `/dev/pts` do `merged_dir` tak,
    /// że skrypty maintainer (`postinst` itp.) mają działające środowisko.
    /// Kopiuje też `/etc/resolv.conf` na potrzeby ewentualnych DNS-lookupów.
    pub fn bind_mount_virtual_fs(&self, session: &OverlaySession) -> Result<()> {
        let binds: &[(&str, &str)] = &[
            ("/proc", "proc"),
            ("/sys",  "sys"),
            ("/dev",  "dev"),
            ("/dev/pts", "dev/pts"),
        ];
        for (src, rel) in binds {
            let dest = session.merged_dir.join(rel);
            std::fs::create_dir_all(&dest).ok();
            mount(
                Some(*src),
                &dest,
                None::<&str>,
                MsFlags::MS_BIND | MsFlags::MS_REC,
                None::<&str>,
            ).with_context(|| format!("bind-mount {} -> {}", src, dest.display()))?;
        }
        let resolv_dest = session.merged_dir.join("etc/resolv.conf");
        if let Some(parent) = resolv_dest.parent() { std::fs::create_dir_all(parent).ok(); }
        let _ = std::fs::copy("/etc/resolv.conf", &resolv_dest);
        Ok(())
    }

    /// Odmontowuje `/proc`,`/sys`,`/dev`,`/dev/pts` z `merged_dir`. Wywoływać
    /// PRZED `end_session`/`discard_session`.
    pub fn unbind_virtual_fs(&self, session: &OverlaySession) -> Result<()> {
        for rel in ["dev/pts", "dev", "sys", "proc"] {
            let dest = session.merged_dir.join(rel);
            if dest.exists() {
                let _ = umount2(&dest, MntFlags::MNT_DETACH);
            }
        }
        Ok(())
    }

    /// Odmontowuje `merged_dir`. `upper_dir` jest zachowany (do commit/copy).
    pub fn end_session(&self, session: &mut OverlaySession) -> Result<()> {
        if session.mounted {
            umount2(&session.merged_dir, MntFlags::MNT_DETACH)
                .with_context(|| format!("umount {}", session.merged_dir.display()))?;
            session.mounted = false;
        }
        Ok(())
    }

    /// Odmontowuje `merged_dir` i usuwa `upper_dir`/`work_dir` (porzuca
    /// zmiany). Wywoływać gdy instalacja pakietów się nie powiodła.
    pub fn discard_session(&self, session: &mut OverlaySession) -> Result<()> {
        self.end_session(session)?;
        let _ = std::fs::remove_dir_all(&session.upper_dir);
        let _ = std::fs::remove_dir_all(&session.work_dir);
        Ok(())
    }

    /// Scala `lower` + `upper` do jednego płaskiego katalogu `dest`, gotowego
    /// do `Repo::commit_directory`. Robimy to zamiast commitować bezpośrednio
    /// `merged_dir` (który wciąż ma aktywny mount overlay w trakcie sesji) —
    /// analog kroku "kopiowane na płasko" z diagramu architektury w README.
    pub fn flatten_to(&self, session: &OverlaySession, dest: &Path) -> Result<()> {
        if dest.exists() { std::fs::remove_dir_all(dest)?; }
        std::fs::create_dir_all(dest)?;
        // cp -a lower/* dest/, then cp -a upper/* dest/ (upper wins), and
        // apply OCI-style whiteouts (".wh.<name>" -> remove <name> in dest).
        copy_tree(&session.lower_dir, dest)?;
        apply_upper_with_whiteouts(&session.upper_dir, dest)?;
        Ok(())
    }
}

fn copy_tree(src: &Path, dest: &Path) -> Result<()> {
    if !src.exists() { return Ok(()); }
    for entry in walkdir::WalkDir::new(src).min_depth(1) {
        let entry = entry?;
        let rel = entry.path().strip_prefix(src).unwrap();
        let target = dest.join(rel);
        let ft = entry.file_type();
        if ft.is_dir() {
            std::fs::create_dir_all(&target)?;
        } else if ft.is_symlink() {
            if let Ok(link) = std::fs::read_link(entry.path()) {
                let _ = std::fs::remove_file(&target);
                let _ = std::os::unix::fs::symlink(link, &target);
            }
        } else {
            if let Some(parent) = target.parent() { std::fs::create_dir_all(parent)?; }
            std::fs::copy(entry.path(), &target)
                .with_context(|| format!("copy {} -> {}", entry.path().display(), target.display()))?;
        }
    }
    Ok(())
}

fn apply_upper_with_whiteouts(upper: &Path, dest: &Path) -> Result<()> {
    if !upper.exists() { return Ok(()); }
    for entry in walkdir::WalkDir::new(upper).min_depth(1) {
        let entry = entry?;
        let rel = entry.path().strip_prefix(upper).unwrap();
        let file_name = entry.file_name().to_string_lossy();

        if let Some(real_name) = file_name.strip_prefix(".wh.") {
            let target = dest.join(rel).with_file_name(real_name);
            let _ = std::fs::remove_file(&target);
            let _ = std::fs::remove_dir_all(&target);
            continue;
        }

        let target = dest.join(rel);
        let ft = entry.file_type();
        if ft.is_dir() {
            std::fs::create_dir_all(&target)?;
        } else if ft.is_symlink() {
            if let Ok(link) = std::fs::read_link(entry.path()) {
                let _ = std::fs::remove_file(&target);
                let _ = std::os::unix::fs::symlink(link, &target);
            }
        } else {
            if let Some(parent) = target.parent() { std::fs::create_dir_all(parent)?; }
            let _ = std::fs::remove_file(&target);
            std::fs::copy(entry.path(), &target)
                .with_context(|| format!("copy {} -> {}", entry.path().display(), target.display()))?;
        }
    }
    Ok(())
}

/// Wywoływana przy starcie `hammer oci` (lub `hammer oci status`) —
/// wykrywa osierocone sesje overlay po przerwanej transakcji (kill -9,
/// awaria zasilania) i sygnalizuje potrzebę `hammer oci cleanup --repair`.
pub fn detect_orphaned_session(work_root: &Path) -> Option<PathBuf> {
    let session = work_root.join("session");
    if session.join("merged").exists() || session.join("upper").exists() {
        Some(session)
    } else {
        None
    }
}

/// Usuwa osieroconą sesję (odmontowuje jeśli wciąż zamontowana, potem `rm -rf`).
pub fn repair_orphaned_session(session_dir: &Path) -> Result<()> {
    let merged = session_dir.join("merged");
    if merged.exists() {
        let _ = umount2(&merged, MntFlags::MNT_DETACH);
    }
    std::fs::remove_dir_all(session_dir)
        .with_context(|| format!("Removing orphaned overlay session {}", session_dir.display()))?;
    Ok(())
}
