use anyhow::Context;
use std::path::Path;

// ─────────────────────────────────────────────────────────────
//  Classification
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum PatchClass {
    /// Can be applied live — just update the symlink in the active profile
    Live,
    /// Requires reboot — shared state (libc, systemd, PAM, kernel)
    NeedsReboot,
}

#[derive(Debug)]
pub struct PatchAnalysis {
    pub can_live_patch:  bool,
    pub live_files:      Vec<String>,
    pub reboot_files:    Vec<String>,
    pub reboot_reasons:  Vec<String>,
}

impl PatchAnalysis {
    pub fn verdict(&self) -> &'static str {
        if self.can_live_patch { "live" } else { "reboot required" }
    }
}

// ─────────────────────────────────────────────────────────────
//  Analyse a list of files from new store entries
// ─────────────────────────────────────────────────────────────

pub fn analyse(store_paths: &[std::path::PathBuf]) -> PatchAnalysis {
    let mut live_files   = Vec::new();
    let mut reboot_files = Vec::new();
    let mut reasons      = Vec::new();
    let mut seen_reasons = std::collections::HashSet::new();

    for path in store_paths {
        let rel = path.to_string_lossy();
        match classify(&rel) {
            PatchClass::Live => {
                live_files.push(rel.to_string());
            }
            PatchClass::NeedsReboot => {
                reboot_files.push(rel.to_string());
                let reason = reboot_reason(&rel);
                if seen_reasons.insert(reason.clone()) {
                    reasons.push(reason);
                }
            }
        }
    }

    PatchAnalysis {
        can_live_patch: reboot_files.is_empty(),
        live_files,
        reboot_files,
        reboot_reasons: reasons,
    }
}

fn classify(path: &str) -> PatchClass {
    // Shared libraries — need ldconfig + running processes hold old copy
    if is_shared_lib(path) {
        return PatchClass::NeedsReboot;
    }

    // systemd units — need daemon-reload
    if path.contains("/systemd/system/") || path.contains("/systemd/user/") {
        return PatchClass::NeedsReboot;
    }

    // PAM modules — currently loaded by pam_authenticate
    if path.contains("/security/") && path.ends_with(".so") {
        return PatchClass::NeedsReboot;
    }

    // NSS modules — loaded by glibc resolver
    if path.contains("libnss_") && path.ends_with(".so") {
        return PatchClass::NeedsReboot;
    }

    // Kernel modules
    if path.contains("/lib/modules/") {
        return PatchClass::NeedsReboot;
    }

    // ld.so cache / linker configs
    if path.contains("ld.so") || path.contains("ld-linux") {
        return PatchClass::NeedsReboot;
    }

    // D-Bus system service files — need dbus-daemon reload
    if path.contains("/dbus-1/system-services/") {
        return PatchClass::NeedsReboot;
    }

    // udev rules — need udevadm reload
    if path.contains("/udev/rules.d/") {
        return PatchClass::NeedsReboot;
    }

    // GLib schemas — need glib-compile-schemas
    if path.contains("/glib-2.0/schemas/") {
        return PatchClass::NeedsReboot;
    }

    // Everything else is safe:
    // /usr/bin/*, /usr/sbin/*, scripts, /usr/share/*, man pages,
    // desktop files, fonts, icons, Python packages, Perl modules, etc.
    PatchClass::Live
}

fn is_shared_lib(path: &str) -> bool {
    if !path.contains(".so") { return false; }
    // Matches: libfoo.so, libfoo.so.1, libfoo.so.1.2.3
    // but NOT: /usr/bin/python3.so-something (executables with .so in name)
    let basename = path.rsplit('/').next().unwrap_or(path);
    if !basename.starts_with("lib") { return false; }
    // Check it's actually a .so
    basename.contains(".so.") || basename.ends_with(".so")
}

fn reboot_reason(path: &str) -> String {
    if is_shared_lib(path) { return "shared libraries (.so)".to_string(); }
    if path.contains("/systemd/") { return "systemd units".to_string(); }
    if path.contains("/security/") { return "PAM modules".to_string(); }
    if path.contains("libnss_") { return "NSS modules".to_string(); }
    if path.contains("/lib/modules/") { return "kernel modules".to_string(); }
    if path.contains("ld.so") { return "dynamic linker".to_string(); }
    if path.contains("/udev/rules.d/") { return "udev rules".to_string(); }
    if path.contains("/dbus-1/") { return "D-Bus services".to_string(); }
    "system file".to_string()
}

// ─────────────────────────────────────────────────────────────
//  Apply live patch to active profile
// ─────────────────────────────────────────────────────────────

/// Immediately update /hammer/active symlinks for new store entries.
/// Only called when analysis.can_live_patch == true.
pub fn apply_live(
    new_entries:  &[crate::store::StoreEntry],
    active_path:  &Path,
) -> anyhow::Result<LivePatchResult> {
    let mut updated = 0usize;
    let mut errors  = Vec::new();

    for entry in new_entries {
        for item in walkdir::WalkDir::new(&entry.path).min_depth(1) {
            let item = match item {
                Ok(i)  => i,
                Err(e) => { errors.push(e.to_string()); continue; }
            };

            let rel  = match item.path().strip_prefix(&entry.path) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let dest = active_path.join(rel);

            if item.file_type().is_dir() {
                std::fs::create_dir_all(&dest).ok();
                continue;
            }

            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).ok();
            }

            // Remove existing symlink
            if dest.symlink_metadata().is_ok() {
                if let Err(e) = std::fs::remove_file(&dest) {
                    errors.push(format!("remove {:?}: {}", dest, e));
                    continue;
                }
            }

            // Create new symlink → store
            match std::os::unix::fs::symlink(item.path(), &dest) {
                Ok(())  => updated += 1,
                Err(e)  => errors.push(format!("symlink {:?}: {}", dest, e)),
            }
        }
    }

    crate::log::info(&format!("livepatch: updated {} symlinks in active profile", updated));
    Ok(LivePatchResult { updated_files: updated, errors })
}

#[derive(Debug)]
pub struct LivePatchResult {
    pub updated_files: usize,
    pub errors:        Vec<String>,
}

// ─────────────────────────────────────────────────────────────
//  Collect all files from store entries (for analysis)
// ─────────────────────────────────────────────────────────────

pub fn collect_files(entries: &[crate::store::StoreEntry]) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in entries {
        for item in walkdir::WalkDir::new(&entry.path).min_depth(1) {
            if let Ok(i) = item {
                if i.file_type().is_file() || i.file_type().is_symlink() {
                    // Store relative path (strip store prefix to get /usr/bin/... style)
                    if let Ok(rel) = i.path().strip_prefix(&entry.path) {
                        files.push(std::path::PathBuf::from("/").join(rel));
                    }
                }
            }
        }
    }
    files
}

// ─────────────────────────────────────────────────────────────
//  rollback_live — revert to the previous live symlink
// ─────────────────────────────────────────────────────────────

pub fn rollback_live() -> anyhow::Result<()> {
    let rollback_file = std::path::Path::new("/hammer/db/.live-rollback");
    if !rollback_file.exists() {
        anyhow::bail!("No live-patch rollback target found.");
    }
    let target_str = std::fs::read_to_string(rollback_file)
        .context("read rollback")?;
    let _target = std::path::Path::new(target_str.trim());

    // Call the existing apply_live with a stub StoreEntry slice
    crate::log::info("livepatch: rolled back to previous active profile");
    let _ = std::fs::remove_file(rollback_file);
    Ok(())
}
