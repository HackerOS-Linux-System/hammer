use std::fs::OpenOptions;
use std::io::Write;

pub const LOG_FILE: &str = "/var/log/hammer.log";

fn write(level: &str, msg: &str) {
    let now  = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let line = format!("[{}] {:5} {}\n", now, level, msg);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(LOG_FILE) {
        let _ = f.write_all(line.as_bytes());
    }
}

pub fn info(msg: &str)  { write("INFO",  msg); }
pub fn warn(msg: &str)  { write("WARN",  msg); }
pub fn error(msg: &str) { write("ERROR", msg); }

pub fn cmd(args: &[String]) {
    write("CMD", &format!("hammer {}", args.join(" ")));
}

pub fn transaction_start(action: &str, packages: &[String]) {
    write("INFO", &format!("transaction::{} [{}]", action, packages.join(", ")));
}

pub fn transaction_done(action: &str, packages: &[String]) {
    write("INFO", &format!("transaction::{} done [{}]", action, packages.join(", ")));
}

pub fn pkg(action: &str, name: &str, version: &str) {
    write("PKG", &format!("{:<10} {}-{}", action, name, version));
}

pub fn file_op(action: &str, path: &str) {
    write("FILE", &format!("{:<8} {}", action, path));
}

pub fn session_start() {
    let now   = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let uname = std::process::Command::new("uname")
    .arg("-r").output().ok()
    .and_then(|o| String::from_utf8(o.stdout).ok())
    .unwrap_or_default();
    let uname = uname.trim();
    let line = format!(
        "\n══════════════════════════════════════════════════════════════════\n\
[{now}] ⬡ HAMMER SESSION  pid={}  kernel={uname}\n\
══════════════════════════════════════════════════════════════════\n",
std::process::id()
    );
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(LOG_FILE) {
        let _ = f.write_all(line.as_bytes());
    }
}
