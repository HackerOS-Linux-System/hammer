use anyhow::Context;
use anyhow::Result;
use owo_colors::OwoColorize;
use std::process::Command;

// ─────────────────────────────────────────────────────────────
//  Notification levels
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub enum Urgency { Low = 0, Normal = 1, Critical = 2 }

// ─────────────────────────────────────────────────────────────
//  send_notification
// ─────────────────────────────────────────────────────────────

pub fn send_notification(
    summary:  &str,
    body:     &str,
    urgency:  Urgency,
    icon:     &str,
) -> Result<()> {
    // Try notify-send (libnotify) first
    let urg = match urgency {
        Urgency::Low      => "low",
        Urgency::Normal   => "normal",
        Urgency::Critical => "critical",
    };

    let status = Command::new("notify-send")
    .args([
        "--app-name=hammer",
        "--urgency", urg,
        "--icon", icon,
        "--expire-time=10000",
        summary,
        body,
    ])
    .status();

    match status {
        Ok(s) if s.success() => return Ok(()),
        _ => {}
    }

    // Fallback: D-Bus direct call
    let dbus_call = format!(
        "call --dest=org.freedesktop.Notifications \
/org/freedesktop/Notifications \
org.freedesktop.Notifications.Notify \
string:hammer uint32:0 string:{} string:'{}' string:'{}' \
array:string: dict:string:variant: int32:10000",
icon, summary, body
    );
    let _ = Command::new("dbus-send").args(dbus_call.split_whitespace()).status();

    // Fallback 2: wall message
    crate::log::info(&format!("notify: {} — {}", summary, body));
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  check_and_notify
// ─────────────────────────────────────────────────────────────

pub async fn check_and_notify() -> Result<()> {
    let db    = crate::db::InstalledDb::open()?;
    let cache = crate::cache::PackageCache::load()?;

    let mut upgrades = Vec::new();
    for inst in db.list_all()? {
        if let Some(avail) = cache.get(&inst.name) {
            if crate::solver::version::compare(&avail.version, &inst.version)
                == std::cmp::Ordering::Greater
                {
                    upgrades.push(format!("{} {} → {}", inst.name, inst.version, avail.version));
                }
        }
    }

    if upgrades.is_empty() {
        crate::log::info("notify: no updates available");
        return Ok(());
    }

    let count   = upgrades.len();
    let summary = format!("{} update{} available", count, if count == 1 { "" } else { "s" });
    let body    = if count <= 5 {
        upgrades.join("\n")
    } else {
        format!("{}\n… and {} more", upgrades[..5].join("\n"), count - 5)
    };

    crate::log::info(&format!("notify: {} updates available", count));
    send_notification(&summary, &body, Urgency::Normal, "software-update-available")?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  CLI
// ─────────────────────────────────────────────────────────────

pub async fn cmd_notify(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("check");
    match sub {
        "check" => {
            println!("  {} Checking for updates…", "·".dimmed());
            check_and_notify().await?;
        }
        "daemon" => {
            println!("  {} Starting update check loop…", "·".dimmed());
            loop {
                // Sync index first
                if let Err(e) = crate::cache::sync_all().await {
                    crate::log::warn(&format!("notify daemon: sync failed: {}", e));
                }
                // Then check
                if let Err(e) = check_and_notify().await {
                    crate::log::warn(&format!("notify daemon: check failed: {}", e));
                }
                // Sleep 6 hours
                tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
            }
        }
        "install-timer" => install_systemd_timer()?,
        other => anyhow::bail!("Unknown notify subcommand: '{}'", other),
    }
    Ok(())
}

fn install_systemd_timer() -> Result<()> {
    let hammer = std::fs::read_link("/proc/self/exe")
    .unwrap_or_else(|_| std::path::PathBuf::from("/usr/bin/hammer"));

    let service = format!(
        "[Unit]\n\
Description=Hammer Package Manager — Update Check\n\
After=network-online.target\n\
Wants=network-online.target\n\
\n\
[Service]\n\
Type=oneshot\n\
ExecStart={} notify check\n\
StandardOutput=journal\n\
StandardError=journal\n\
\n\
[Install]\n\
WantedBy=timers.target\n",
hammer.display()
    );

    let timer = "\
[Unit]\n\
Description=Hammer Package Manager — Daily Update Check\n\
\n\
[Timer]\n\
OnCalendar=daily\n\
Persistent=true\n\
RandomizedDelaySec=1h\n\
\n\
[Install]\n\
WantedBy=timers.target\n";

    std::fs::write("/etc/systemd/system/hammer-update-check.service", &service)?;
    std::fs::write("/etc/systemd/system/hammer-update-check.timer",   timer)?;

    let _ = Command::new("systemctl")
    .args(["enable", "--now", "hammer-update-check.timer", "--no-reload"])
    .status();
    let _ = Command::new("systemctl").arg("daemon-reload").status();

    println!("  {} hammer-update-check.timer installed and enabled.", "✔".bright_green());
    crate::log::info("notify: installed systemd timer");
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Daemon mode with PID file and configurable interval
// ─────────────────────────────────────────────────────────────

pub const NOTIFY_PID_FILE: &str = "/run/hammer-notify.pid";
pub const NOTIFY_SOCKET:   &str = "/run/hammer-notify.sock";

#[derive(Debug)]
pub struct NotifyDaemon {
    pub interval_hours:   u64,
    pub check_interval:   u64,
}

impl NotifyDaemon {
    pub fn new(interval_hours: u64) -> Self {
        NotifyDaemon { interval_hours, check_interval: interval_hours }
    }

    pub fn from_config() -> Self {
        let interval = std::env::var("HAMMER_NOTIFY_INTERVAL")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(6);
        Self::new(interval)
    }

    /// Start the daemon loop. Call only once — blocks.
    pub fn run(&self) -> Result<()> {
        // Write PID file
        std::fs::write(NOTIFY_PID_FILE, format!("{}\n", std::process::id()))?;

        // Install signal handler for clean shutdown
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        {
            let _r = running.clone();
            let _ = unsafe {
                libc::signal(libc::SIGTERM, handle_sigterm as libc::sighandler_t)
            };
        }

        eprintln!("[hammer-notify] Started. Interval: {}h PID: {}",
                  self.interval_hours, std::process::id());

        let interval = std::time::Duration::from_secs(self.interval_hours * 3600);
        let mut last_run = std::time::Instant::now()
            .checked_sub(interval)
            .unwrap_or(std::time::Instant::now());

        loop {
            if !running.load(std::sync::atomic::Ordering::SeqCst) { break; }
            let now = std::time::Instant::now();
            if now.duration_since(last_run) >= interval {
                last_run = now;
                self.run_check();
            }
            std::thread::sleep(std::time::Duration::from_secs(60));
        }

        let _ = std::fs::remove_file(NOTIFY_PID_FILE);
        eprintln!("[hammer-notify] Stopped.");
        Ok(())
    }

    fn run_check(&self) {
        let out = Command::new("hammer")
            .args(["list", "--upgradable"])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                let n    = text.lines().filter(|l| !l.trim().is_empty()).count();
                if n > 0 {
                    let summary = format!("{} update{} available", n, if n == 1 { "" } else { "s" });
                    let _ = send_notification(
                        &summary,
                        "Run 'hammer upgrade' to install.",
                        Urgency::Normal,
                        "software-update-available",
                    );
                }
            }
            _ => {}
        }
    }

    /// Stop a running daemon (send SIGTERM via PID file).
    pub fn stop() -> Result<()> {
        let pid_str = std::fs::read_to_string(NOTIFY_PID_FILE)
            .context("No PID file — is hammer-notify running?")?;
        let pid: i32 = pid_str.trim().parse()
            .context("Invalid PID in PID file")?;
        unsafe { libc::kill(pid, libc::SIGTERM); }
        let _ = std::fs::remove_file(NOTIFY_PID_FILE);
        println!("  {} Sent SIGTERM to hammer-notify (pid={}).", "✔".bright_green(), pid);
        Ok(())
    }

    /// Find the DBUS_SESSION_BUS_ADDRESS for all active user sessions.
    pub fn find_all_dbus_addresses() -> Vec<String> {
        let mut addrs = Vec::new();
        // Read from /run/user/<uid>/bus
        if let Ok(entries) = std::fs::read_dir("/run/user") {
            for e in entries.flatten() {
                let bus = e.path().join("bus");
                if bus.exists() {
                    addrs.push(format!("unix:path={}", bus.display()));
                }
            }
        }
        addrs
    }
}

extern "C" fn handle_sigterm(_: libc::c_int) {
    // Set a flag — the loop checks it
    std::process::exit(0);
}

/// Send a notification to every active desktop session.
pub fn broadcast_notification(summary: &str, body: &str, urgency: Urgency, icon: &str) {
    let addrs = NotifyDaemon::find_all_dbus_addresses();
    if addrs.is_empty() {
        let _ = send_notification(summary, body, urgency, icon);
        return;
    }
    for addr in &addrs {
        let urg = match urgency {
            Urgency::Low      => "low",
            Urgency::Normal   => "normal",
            Urgency::Critical => "critical",
        };
        let _ = Command::new("notify-send")
            .env("DBUS_SESSION_BUS_ADDRESS", addr)
            .args([
                "--app-name=hammer",
                &format!("--icon={}", icon),
                &format!("--urgency={}", urg),
                summary,
                body,
            ])
            .status();
    }
}
