use std::path::Path;

// ─────────────────────────────────────────────────────────────
//  Public API
// ─────────────────────────────────────────────────────────────

/// Returns true if we are running inside a live (non-installed) system.
/// If true, hammer should refuse to operate.
pub fn is_live_system() -> bool {
    check_cmdline()
        || check_live_dirs()
        || check_root_filesystem()
        || check_fstab()
        || check_hostname()
}

/// Call this early in main(). Prints an error and exits if live.
pub fn assert_not_live() {
    if is_live_system() {
        eprintln!();
        eprintln!("  \x1b[1;31m✗\x1b[0m  hammer cannot be used in a live system.");
        eprintln!();
        eprintln!("  HackerOS must be \x1b[1minstalled to disk\x1b[0m before using hammer.");
        eprintln!("  To install HackerOS, use the installer from the live environment.");
        eprintln!();
        std::process::exit(2);
    }
}

/// Returns a human-readable reason why we think this is a live system.
/// Returns None if we think it's installed.
pub fn live_reason() -> Option<String> {
    if check_cmdline()         { return Some("kernel cmdline contains live boot parameters".into()); }
    if check_live_dirs()       { return Some("/run/live or /etc/live/config detected".into()); }
    if check_root_filesystem() { return Some("root filesystem is overlay/tmpfs (live layer)".into()); }
    if check_fstab()           { return Some("no persistent root entry in /etc/fstab".into()); }
    if check_hostname()        { return Some("hostname matches live system default".into()); }
    None
}

// ─────────────────────────────────────────────────────────────
//  Individual checks
// ─────────────────────────────────────────────────────────────

fn check_cmdline() -> bool {
    let Ok(cmdline) = std::fs::read_to_string("/proc/cmdline") else { return false; };
    let live_markers = [
        "boot=live",
        "live-media",
        "toram",
        "findiso",
        "fromiso",
        "live_dir=",
        "live-config",
        "USERNAME=user",      // Debian live default
        "LIVE_HOSTNAME=",
    ];
    live_markers.iter().any(|m| cmdline.contains(m))
}

fn check_live_dirs() -> bool {
    Path::new("/run/live").exists()
        || Path::new("/etc/live").exists()
        || Path::new("/lib/live").exists()
        || Path::new("/run/initramfs/live").exists()
}

fn check_root_filesystem() -> bool {
    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else { return false; };
    for line in mounts.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 { continue; }
        let mountpoint = parts[1];
        let fstype     = parts[2];
        if mountpoint == "/" {
            // overlay / overlayfs / aufs / tmpfs all indicate live
            return matches!(fstype, "overlay" | "overlayfs" | "aufs" | "tmpfs");
        }
    }
    false
}

fn check_fstab() -> bool {
    let Ok(fstab) = std::fs::read_to_string("/etc/fstab") else {
        // No fstab at all → almost certainly live
        return true;
    };
    // In a properly installed system, /etc/fstab has a non-tmpfs root entry.
    // We look for any line that mounts "/" with a real block device.
    let has_root_entry = fstab.lines()
        .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
        .any(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 3 { return false; }
            let mountpoint = parts[1];
            let fstype     = parts[2];
            mountpoint == "/" && !matches!(fstype, "tmpfs" | "overlay" | "none")
        });
    !has_root_entry
}

fn check_hostname() -> bool {
    let Ok(hostname) = std::fs::read_to_string("/etc/hostname") else { return false; };
    let h = hostname.trim().to_lowercase();
    // Typical live system hostnames
    matches!(
        h.as_str(),
        "debian-live" | "live" | "hackeros-live" | "kali" | "ubuntu-live"
    ) || h.contains("-live")
      || h.starts_with("live-")
}
