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
