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
        if let Some(reason) = container_reason() {
            eprintln!();
            eprintln!("  Detected: {reason}.");
            eprintln!("  This binary was built for the \x1b[1matomic\x1b[0m mode (generations +");
            eprintln!("  immutable store), which needs a real installed root and doesn't apply");
            eprintln!("  inside a container. For classic apt-style package management that works");
            eprintln!("  anywhere — including containers — rebuild with:");
            eprintln!();
            eprintln!("      cargo build --release --features normal-mode");
            eprintln!();
            eprintln!("  Run 'hammer features' on a binary to check which mode it was built with.");
        }
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

// ─────────────────────────────────────────────────────────────
//  Container / virtualisation detection
// ─────────────────────────────────────────────────────────────

pub fn is_container() -> bool {
    check_docker()
    || check_podman()
    || check_wsl()
    || check_systemd_nspawn()
    || check_lxc()
}

pub fn container_reason() -> Option<&'static str> {
    if check_docker()         { return Some("running inside Docker container"); }
    if check_podman()         { return Some("running inside Podman container"); }
    if check_wsl()            { return Some("running inside WSL (Windows Subsystem for Linux)"); }
    if check_systemd_nspawn() { return Some("running inside systemd-nspawn container"); }
    if check_lxc()            { return Some("running inside LXC container"); }
    None
}

/// Warn (but don't abort) if running in a container.
pub fn warn_if_container() {
    if let Some(reason) = container_reason() {
        eprintln!(
            "\n  \x1b[1;33m⚠\x1b[0m  hammer in container: {}\n  \
             Some features (immutable FS, GRUB, systemd services) will not work.\n",
            reason
        );
    }
}

fn check_docker() -> bool {
    // /.dockerenv is the canonical Docker indicator
    Path::new("/.dockerenv").exists()
    // cgroup-based detection
    || std::fs::read_to_string("/proc/1/cgroup").unwrap_or_default().contains("docker")
    || std::env::var("container").as_deref() == Ok("docker")
}

fn check_podman() -> bool {
    Path::new("/run/.containerenv").exists()
    || std::env::var("container").as_deref() == Ok("podman")
}

fn check_wsl() -> bool {
    std::env::var("WSL_DISTRO_NAME").is_ok()
    || std::env::var("WSL_INTEROP").is_ok()
    || std::fs::read_to_string("/proc/version")
        .unwrap_or_default()
        .to_lowercase()
        .contains("microsoft")
}

fn check_systemd_nspawn() -> bool {
    std::env::var("container").as_deref() == Ok("systemd-nspawn")
    || Path::new("/run/host").exists()
}

fn check_lxc() -> bool {
    Path::new("/proc/1/environ").exists() && {
        std::fs::read_to_string("/proc/1/environ")
            .unwrap_or_default()
            .contains("container=lxc")
    }
}
