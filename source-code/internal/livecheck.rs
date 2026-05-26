use std::path::Path;

pub fn is_live_system() -> bool {
    check_cmdline()
    || check_live_dirs()
    || check_root_filesystem()
    || check_fstab()
    || check_hostname()
}

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

pub fn live_reason() -> Option<String> {
    if check_cmdline()         { return Some("kernel cmdline contains live boot parameters".into()); }
    if check_live_dirs()       { return Some("/run/live or /etc/live/config detected".into()); }
    if check_root_filesystem() { return Some("root filesystem is overlay/tmpfs (live layer)".into()); }
    if check_fstab()           { return Some("no persistent root entry in /etc/fstab".into()); }
    if check_hostname()        { return Some("hostname matches live system default".into()); }
    None
}

fn check_cmdline() -> bool {
    let Ok(cmdline) = std::fs::read_to_string("/proc/cmdline") else { return false; };
    let live_markers = [
        "boot=live", "live-media", "toram", "findiso",
        "fromiso", "live_dir=", "live-config", "USERNAME=user",
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
        if parts[1] == "/" {
            return matches!(parts[2], "overlay" | "overlayfs" | "aufs" | "tmpfs");
        }
    }
    false
}

fn check_fstab() -> bool {
    let Ok(fstab) = std::fs::read_to_string("/etc/fstab") else { return true; };
    let has_real_root = fstab.lines()
    .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
    .any(|line| {
        let parts: Vec<&str> = line.split_whitespace().collect();
        parts.len() >= 3
        && parts[1] == "/"
        && !matches!(parts[2], "tmpfs" | "overlay" | "none")
    });
    !has_real_root
}

fn check_hostname() -> bool {
    let Ok(h) = std::fs::read_to_string("/etc/hostname") else { return false; };
    let h = h.trim().to_lowercase();
    matches!(h.as_str(), "debian-live" | "live" | "hackeros-live" | "kali" | "ubuntu-live")
    || h.contains("-live")
    || h.starts_with("live-")
}
