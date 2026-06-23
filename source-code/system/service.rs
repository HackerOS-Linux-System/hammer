use anyhow::{bail, Result};
use owo_colors::OwoColorize;
use std::path::Path;
use std::process::Command;

use crate::log;

// ─────────────────────────────────────────────────────────────
//  ServiceInfo
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ServiceInfo {
    pub unit:         String,
    pub package:      Option<String>,
    pub active_state: String,
    pub sub_state:    String,
    pub load_state:   String,
    pub description:  String,
}

// ─────────────────────────────────────────────────────────────
//  Entry point
// ─────────────────────────────────────────────────────────────

pub fn cmd_service(args: &[String]) -> Result<()> {
    let sub   = args.first().map(|s| s.as_str()).unwrap_or("list");
    let units: Vec<String> = args[1.min(args.len())..].iter()
    .filter(|a| !a.starts_with('-')).cloned().collect();

    match sub {
        "list"     | "ls"    => cmd_service_list(),
        "start"              => cmd_service_op("start",   &units),
        "stop"               => cmd_service_op("stop",    &units),
        "restart"            => cmd_service_op("restart", &units),
        "reload"             => cmd_service_op("reload",  &units),
        "enable"             => cmd_service_op("enable",  &units),
        "disable"            => cmd_service_op("disable", &units),
        "status"             => cmd_service_status(&units),
        "log" | "logs"       => cmd_service_log(&units),
        other => bail!(
            "Unknown service subcommand: '{}'\n  \
Usage: hammer service [list|start|stop|restart|reload|enable|disable|status|log] [unit...]",
other
        ),
    }
}

// ─────────────────────────────────────────────────────────────
//  list
// ─────────────────────────────────────────────────────────────

fn cmd_service_list() -> Result<()> {
    println!();
    println!("  {}  Services installed by hammer packages", "⬡".bright_cyan().bold());
    println!("  {}", "─".repeat(70).dimmed());
    println!("  {:<36} {:<12} {:<12} {}", "Unit".bold(), "State".bold(), "Load".bold(), "Package".bold());
    println!("  {}", "─".repeat(70).dimmed());

    let services = find_hammer_services()?;

    if services.is_empty() {
        println!("  {} No services found.", "·".dimmed());
        println!();
        println!("  Services appear here after installing packages that ship systemd units.");
        println!("  Example: {}", "hammer install nginx".cyan());
        return Ok(());
    }

    for svc in &services {
        let state_col = match svc.active_state.as_str() {
            "active"   => "active".bright_green().to_string(),
            "failed"   => "failed".red().bold().to_string(),
            "inactive" => "inactive".dimmed().to_string(),
            other      => other.to_string(),
        };
        let pkg_name = svc.package.as_deref().unwrap_or("unknown");
        println!("  {:<36} {:<20} {:<12} {}",
                 svc.unit.bold(), state_col, svc.load_state.dimmed(), pkg_name.cyan());
    }

    println!("  {}", "─".repeat(70).dimmed());
    println!("  {} service(s) found.", services.len());
    println!();
    println!("  Start:  {}   Stop: {}",
             "hammer service start <unit>".cyan(),
             "hammer service stop <unit>".cyan());
    println!("  Enable: {}   Logs: {}",
             "hammer service enable <unit>".cyan(),
             "hammer service log <unit>".cyan());
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  start / stop / restart / reload / enable / disable
// ─────────────────────────────────────────────────────────────

fn cmd_service_op(op: &str, units: &[String]) -> Result<()> {
    if units.is_empty() {
        bail!("Usage: hammer service {} <unit...>", op);
    }
    for unit in units {
        let unit_name = ensure_service_suffix(unit);
        println!("  {} {} {}…", "⬡".cyan().bold(), op, unit_name.bold());

        let mut args = vec![op, &unit_name];
        if op == "enable" { args.push("--no-reload"); }

        let status = Command::new("systemctl").args(&args).status();
        match status {
            Ok(s) if s.success() => {
                println!("  {} {} {} ok.", "✔".bright_green(), op, unit_name.bold());
                log::info(&format!("service: {} {} ok", op, unit_name));
            }
            Ok(s) => {
                let code = s.code().unwrap_or(-1);
                println!("  {} {} {} failed (exit {}). Logs: {}",
                         "✗".red().bold(), op, unit_name.bold(), code,
                         format!("hammer service log {}", unit_name).cyan());
            }
            Err(e) => println!("  {} systemctl not available: {}", "!".yellow().bold(), e),
        }
    }
    if op == "enable" || op == "disable" {
        let _ = Command::new("systemctl").arg("daemon-reload").status();
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  status
// ─────────────────────────────────────────────────────────────

fn cmd_service_status(units: &[String]) -> Result<()> {
    if units.is_empty() { return cmd_service_list(); }
    for unit in units {
        let unit_name = ensure_service_suffix(unit);
        println!("  {}  {}", "⬡".cyan().bold(), unit_name.bold());
        println!("  {}", "─".repeat(60).dimmed());
        let _ = Command::new("systemctl")
        .args(["status", "--no-pager", &unit_name])
        .status();
        println!();
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  logs
// ─────────────────────────────────────────────────────────────

fn cmd_service_log(units: &[String]) -> Result<()> {
    if units.is_empty() { bail!("Usage: hammer service log <unit>"); }
    for unit in units {
        let unit_name = ensure_service_suffix(unit);
        println!("  {}  Logs for {} (last 50 lines)", "⬡".cyan().bold(), unit_name.bold());
        println!("  {}", "─".repeat(60).dimmed());
        let _ = Command::new("journalctl")
        .args(["-u", &unit_name, "-n", "50", "--no-pager"])
        .status();
        println!();
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Find hammer-installed services
// ─────────────────────────────────────────────────────────────

fn find_hammer_services() -> Result<Vec<ServiceInfo>> {
    let mut services = Vec::new();
    let unit_dirs = [
        "/etc/systemd/system",
        "/usr/lib/systemd/system",
        "/hammer/active/lib/systemd/system",
        "/hammer/active/usr/lib/systemd/system",
    ];
    for dir_str in &unit_dirs {
        let dir = Path::new(dir_str);
        if !dir.exists() { continue; }
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            if !name.ends_with(".service") { continue; }
            if name.starts_with("hammer") || name.starts_with("systemd-") { continue; }
            if is_hammer_installed_unit(&path) {
                if let Ok(info) = get_service_status(&name) {
                    services.push(info);
                }
            }
        }
    }
    services.sort_by(|a, b| a.unit.cmp(&b.unit));
    services.dedup_by(|a, b| a.unit == b.unit);
    Ok(services)
}

fn is_hammer_installed_unit(path: &Path) -> bool {
    if let Ok(target) = std::fs::read_link(path) {
        let t = target.to_string_lossy();
        if t.contains("/hammer/") { return true; }
    }
    if let Ok(content) = std::fs::read_to_string(path) {
        if content.contains("/hammer/active") || content.contains("X-Hammer-Package") {
            return true;
        }
    }
    false
}

fn get_service_status(unit: &str) -> Result<ServiceInfo> {
    let mut info = ServiceInfo {
        unit:         unit.to_string(),
        package:      None,
        active_state: "unknown".to_string(),
        sub_state:    "unknown".to_string(),
        load_state:   "unknown".to_string(),
        description:  String::new(),
    };

    let output = Command::new("systemctl")
    .args(["show", "--no-pager",
          "--property=ActiveState,SubState,LoadState,Description",
          unit])
    .output();

    if let Ok(out) = output {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if let Some(v) = line.strip_prefix("ActiveState=")  { info.active_state = v.to_string(); }
            if let Some(v) = line.strip_prefix("SubState=")     { info.sub_state    = v.to_string(); }
            if let Some(v) = line.strip_prefix("LoadState=")    { info.load_state   = v.to_string(); }
            if let Some(v) = line.strip_prefix("Description=")  { info.description  = v.to_string(); }
        }
    }

    // FIX: InstalledDb::empty() doesn't exist
    // Use open() and fall back gracefully
    info.package = find_unit_package(unit);
    Ok(info)
}

fn find_unit_package(unit: &str) -> Option<String> {
    // FIX: open the DB and handle error gracefully — no empty() constructor
    let db = crate::db::InstalledDb::open().ok()?;
    let unit_bare = unit.strip_suffix(".service").unwrap_or(unit);
    // list_all returns Result — unwrap_or_default gives empty Vec on error
    db.list_all().unwrap_or_default().into_iter()
    .find(|p| p.name == unit_bare || p.name.contains(unit_bare))
    .map(|p| p.name)
}

fn ensure_service_suffix(name: &str) -> String {
    if name.contains('.') { name.to_string() } else { format!("{}.service", name) }
}

// ─────────────────────────────────────────────────────────────
//  list --all (all systemd units, not just hammer ones)
// ─────────────────────────────────────────────────────────────

pub fn cmd_service_list_all(filter_pkg: Option<&str>) -> Result<()> {
    println!();
    println!("  {}  All systemd units{}", "⬡".bright_cyan().bold(),
             filter_pkg.map(|p| format!(" — package: {}", p.cyan())).unwrap_or_default());
    println!("  {}", "─".repeat(80).dimmed());
    println!("  {:<40} {:<14} {:<12} {}",
             "Unit".bold(), "Active".bold(), "Load".bold(), "Description".bold());
    println!("  {}", "─".repeat(80).dimmed());

    // Use systemctl --output=json if possible, fall back to text
    let out = Command::new("systemctl")
        .args(["list-units", "--all", "--no-legend", "--no-pager",
               "--type=service,socket,timer,path"])
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout);

    let db = crate::db::InstalledDb::open().ok();

    let mut count = 0usize;
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 { continue; }
        let unit   = parts[0];
        let load   = parts[1];
        let active = parts[2];
        let desc   = parts[4..].join(" ");

        // Package filter
        if let Some(pkg) = filter_pkg {
            let bare = unit.split('.').next().unwrap_or(unit);
            let owned = db.as_ref()
                .and_then(|db| db.list_all().ok())
                .map(|pkgs| pkgs.iter().any(|p| {
                    p.name == pkg && (p.name == bare || p.name.contains(bare))
                }))
                .unwrap_or(false);
            if !owned { continue; }
        }

        let active_col = match active {
            "active"   => active.bright_green().to_string(),
            "failed"   => active.red().bold().to_string(),
            "inactive" => active.dimmed().to_string(),
            other      => other.to_string(),
        };
        println!("  {:<40} {:<22} {:<12} {}",
                 unit.bold(), active_col, load.dimmed(),
                 desc.chars().take(40).collect::<String>().dimmed());
        count += 1;
    }
    println!("  {}", "─".repeat(80).dimmed());
    println!("  {} unit(s).", count);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  journal with --since / --until
// ─────────────────────────────────────────────────────────────

pub fn cmd_service_log_filtered(
    units:  &[String],
    since:  Option<&str>,
    until:  Option<&str>,
    lines:  usize,
    follow: bool,
) -> Result<()> {
    if units.is_empty() { bail!("Usage: hammer service log <unit> [--since=...] [--until=...]"); }
    for unit in units {
        let unit_name = ensure_service_suffix(unit);
        println!("  {}  Logs for {}", "⬡".cyan().bold(), unit_name.bold());
        if let Some(s) = since { println!("  {} Since: {}", "·".dimmed(), s.cyan()); }
        if let Some(u) = until { println!("  {} Until: {}", "·".dimmed(), u.cyan()); }
        println!("  {}", "─".repeat(60).dimmed());

        let mut args = vec!["-u", &unit_name, "--no-pager"];
        let lines_str = format!("{}", lines);
        if !follow { args.extend_from_slice(&["-n", &lines_str]); }
        let since_str; let until_str;
        if let Some(s) = since { since_str = s.to_string(); args.extend_from_slice(&["--since", &since_str]); }
        if let Some(u) = until { until_str = u.to_string(); args.extend_from_slice(&["--until", &until_str]); }
        if follow { args.push("--follow"); }

        let _ = Command::new("journalctl").args(&args).status();
        println!();
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  preset — enable/disable units according to preset policy
// ─────────────────────────────────────────────────────────────

pub fn cmd_service_preset(units: &[String]) -> Result<()> {
    println!("  {}  Applying service presets…", "⬡".bright_cyan().bold());
    if units.is_empty() {
        let _ = Command::new("systemctl").args(["preset-all", "--no-reload"]).status();
    } else {
        for unit in units {
            let uname = ensure_service_suffix(unit);
            let _ = Command::new("systemctl")
                .args(["preset", "--no-reload", &uname]).status();
            println!("  {} preset applied: {}", "✔".bright_green(), uname.cyan());
        }
    }
    let _ = Command::new("systemctl").arg("daemon-reload").status();
    Ok(())
}
