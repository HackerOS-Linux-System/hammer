use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering as AO};
use std::sync::OnceLock;

pub const LOG_FILE:     &str = "/var/log/hammer.log";
pub const LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;   // 10 MiB
pub const LOG_KEEP_DAYS: i64 = 90;

// ── Log level ─────────────────────────────────────────────────

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level { Error = 0, Warn = 1, Info = 2, Pkg = 3, File = 4, Debug = 5 }

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn  => "WARN ",
            Level::Info  => "INFO ",
            Level::Pkg   => "PKG  ",
            Level::File  => "FILE ",
            Level::Debug => "DEBUG",
        }
    }
}

static CURRENT_LEVEL: AtomicU8 = AtomicU8::new(Level::Info as u8);

pub fn set_level(l: Level)  { CURRENT_LEVEL.store(l as u8, AO::Relaxed); }
pub fn current_level() -> Level {
    match CURRENT_LEVEL.load(AO::Relaxed) {
        0 => Level::Error, 1 => Level::Warn, 2 => Level::Info,
        3 => Level::Pkg,   4 => Level::File, _ => Level::Debug,
    }
}
pub fn set_verbose() { set_level(Level::Debug); }
pub fn set_quiet()   { set_level(Level::Warn); }

// ── Session ID ────────────────────────────────────────────────

static SESSION_ID: OnceLock<u64> = OnceLock::new();

fn session_id() -> u64 {
    *SESSION_ID.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    })
}

// ── Core write ────────────────────────────────────────────────

#[derive(serde::Serialize)]
struct LogEntry<'a> {
    ts:      String,
    level:   &'a str,
    session: u64,
    pid:     u32,
    message: &'a str,
}

fn write_entry(level: Level, msg: &str) {
    if level > current_level() { return; }

    let now = chrono::Local::now();
    let ts  = now.format("%Y-%m-%d %H:%M:%S%.3f").to_string();
    let pid = std::process::id();
    let sid = session_id();

    // Plain text line
    let line = format!("[{}] {} [pid={} sid={}] {}\n", ts, level.as_str(), pid, sid, msg);

    // Try journald first (via systemd-cat or direct sd_journal_print if linked)
    #[cfg(target_os = "linux")]
    try_journald(level, msg);

    // Append to log file (with rotation check)
    rotate_if_needed();
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(LOG_FILE) {
        let _ = f.write_all(line.as_bytes());
    }
}

fn try_journald(level: Level, msg: &str) {
    // Use systemd-cat as a simple journald bridge
    let priority = match level {
        Level::Error => "3",
        Level::Warn  => "4",
        Level::Info | Level::Pkg | Level::File => "6",
        Level::Debug => "7",
    };
    // Fire-and-forget, ignore errors
    let _ = std::process::Command::new("systemd-cat")
        .args(["--identifier=hammer", &format!("--priority={}", priority)])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            if let Some(stdin) = c.stdin.as_mut() {
                let _ = stdin.write_all(msg.as_bytes());
            }
            c.wait()
        });
}

fn rotate_if_needed() {
    let path = std::path::Path::new(LOG_FILE);
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size < LOG_MAX_BYTES { return; }

    // Rename: hammer.log → hammer.log.1, hammer.log.1 → hammer.log.2, keep up to .5
    for i in (1..5u8).rev() {
        let old = format!("{}.{}", LOG_FILE, i);
        let new = format!("{}.{}", LOG_FILE, i + 1);
        let _ = std::fs::rename(&old, &new);
    }
    let _ = std::fs::rename(LOG_FILE, format!("{}.1", LOG_FILE));
}

// ── Public logging functions ──────────────────────────────────

pub fn error(msg: &str) { write_entry(Level::Error, msg); }
pub fn warn(msg: &str)  { write_entry(Level::Warn,  msg); }
pub fn info(msg: &str)  { write_entry(Level::Info,  msg); }
pub fn debug(msg: &str) { write_entry(Level::Debug, msg); }

pub fn pkg(action: &str, name: &str, version: &str) {
    write_entry(Level::Pkg, &format!("{:<10} {}-{}", action, name, version));
}

pub fn file_op(action: &str, path: &str) {
    write_entry(Level::File, &format!("{:<8} {}", action, path));
}

pub fn cmd(args: &[String]) {
    write_entry(Level::Info, &format!("cmd: hammer {}", args.join(" ")));
}

pub fn transaction_start(action: &str, packages: &[String]) {
    write_entry(Level::Info, &format!(
        "transaction::{} START [{}]", action, packages.join(", ")
    ));
}

pub fn transaction_done(action: &str, packages: &[String]) {
    write_entry(Level::Info, &format!(
        "transaction::{} DONE  [{}]", action, packages.join(", ")
    ));
}

pub fn session_start() {
    let _ = SESSION_ID.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    });

    // Read log level from environment
    if let Ok(level) = std::env::var("HAMMER_LOG_LEVEL") {
        match level.to_lowercase().as_str() {
            "error" => set_level(Level::Error),
            "warn"  => set_level(Level::Warn),
            "info"  => set_level(Level::Info),
            "pkg"   => set_level(Level::Pkg),
            "file"  => set_level(Level::File),
            "debug" => set_level(Level::Debug),
            _ => {}
        }
    }

    let kernel = std::process::Command::new("uname").arg("-r").output().ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    let kernel = kernel.trim();

    let sep = "═".repeat(66);
    let header = format!(
        "\n{sep}\n[SESSION {}] ⬡ HAMMER v{}  pid={}  kernel={}\n{sep}\n",
        session_id(), env!("CARGO_PKG_VERSION"), std::process::id(), kernel
    );

    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(LOG_FILE) {
        let _ = f.write_all(header.as_bytes());
    }
}

// ── hammer log CLI ────────────────────────────────────────────

pub fn cmd_log(args: &[String]) -> anyhow::Result<()> {
    use owo_colors::OwoColorize;

    let n: usize = args.iter()
        .find(|a| a.starts_with("-n") || a.starts_with("--lines="))
        .and_then(|a| {
            if a.starts_with("-n") { a[2..].trim().parse().ok() }
            else { a.split('=').nth(1).and_then(|v| v.parse().ok()) }
        })
        .unwrap_or(50);

    let json_mode   = args.iter().any(|a| a == "--json");
    let follow_mode = args.iter().any(|a| a == "-f" || a == "--follow");
    let level_filter= args.iter()
        .find(|a| a.starts_with("--level="))
        .and_then(|a| a.strip_prefix("--level="));
    let session_filter = args.iter()
        .find(|a| a.starts_with("--session="))
        .and_then(|a| a.strip_prefix("--session=").and_then(|v| v.parse::<u64>().ok()));
    let grep_filter = args.iter()
        .find(|a| a.starts_with("--grep="))
        .and_then(|a| a.strip_prefix("--grep="));

    let path = std::path::Path::new(LOG_FILE);
    if !path.exists() {
        println!("  {} No log file found at {}", "·".dimmed(), LOG_FILE);
        return Ok(());
    }

    let content = std::fs::read_to_string(path)?;
    let mut lines: Vec<&str> = content.lines().collect();

    // Apply filters
    if let Some(lf) = level_filter {
        let lf_up = lf.to_uppercase();
        lines.retain(|l| l.contains(&lf_up));
    }
    if let Some(sf) = session_filter {
        let sf_str = format!("sid={}", sf);
        lines.retain(|l| l.contains(&sf_str));
    }
    if let Some(gf) = grep_filter {
        lines.retain(|l| l.to_lowercase().contains(&gf.to_lowercase()));
    }

    // Take last N lines
    let start = lines.len().saturating_sub(n);
    let lines = &lines[start..];

    if json_mode {
        // Emit JSON array
        let entries: Vec<serde_json::Value> = lines.iter().map(|l| {
            serde_json::json!({ "line": l })
        }).collect();
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    println!();
    println!("  {}  hammer log  (last {} lines)", "⬡".bright_cyan().bold(), lines.len());
    println!("  {}", "─".repeat(70).dimmed());

    for line in lines {
        let coloured = if line.contains("ERROR") {
            line.red().to_string()
        } else if line.contains("WARN ") {
            line.yellow().to_string()
        } else if line.contains("DEBUG") {
            line.dimmed().to_string()
        } else if line.contains("PKG  ") {
            line.bright_cyan().to_string()
        } else if line.contains("SESSION") {
            line.bright_white().bold().to_string()
        } else {
            line.to_string()
        };
        println!("  {}", coloured);
    }

    if follow_mode {
        println!("  {} Following log (Ctrl+C to stop)…", "·".dimmed());
        let mut pos = std::fs::metadata(path)?.len();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let new_size = std::fs::metadata(path)?.len();
            if new_size > pos {
                let mut f = std::fs::File::open(path)?;
                use std::io::{Seek, SeekFrom, BufRead, BufReader};
                f.seek(SeekFrom::Start(pos))?;
                for line in BufReader::new(&f).lines().flatten() {
                    println!("  {}", line);
                }
                pos = new_size;
            }
        }
    }
    Ok(())
}
