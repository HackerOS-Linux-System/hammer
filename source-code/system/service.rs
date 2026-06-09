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
