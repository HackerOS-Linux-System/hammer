use std::path::Path;
use std::process::Command;

use crate::log;

// ─────────────────────────────────────────────────────────────
//  Action
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Action {
    CreateUser    { name: String, system: bool, home: Option<String>, shell: Option<String>, group: Option<String> },
    CreateGroup   { name: String, system: bool, gid: Option<u32> },
    EnableUnit    { unit: String },
    StartUnit     { unit: String },
    StopUnit      { unit: String },
    DisableUnit   { unit: String },
    DaemonReload,
    RegisterAlternative { link: String, name: String, path: String, priority: u32 },
    RunLdconfig,
    CreateSymlink { src: String, dest: String, force: bool },
    CreateDir     { path: String, mode: Option<String> },
    SetOwner      { path: String, owner: String },
    SetMode       { path: String, mode: String },
    InstallFile   { src: String, dest: String, mode: Option<String>, owner: Option<String> },
    Skipped       { original_line: String, reason: String },
}

#[derive(Debug)]
pub struct ActionResult {
    pub action:  String,
    pub success: bool,
    pub message: String,
}

// ─────────────────────────────────────────────────────────────
//  PostinstTranslator
// ─────────────────────────────────────────────────────────────

pub struct PostinstTranslator {
    pub pkg_name: String,
}

impl PostinstTranslator {
    pub fn new(pkg_name: &str) -> Self {
        PostinstTranslator { pkg_name: pkg_name.to_string() }
    }

    pub fn translate(&self, script: &str) -> Vec<Action> {
        let mut actions = Vec::new();
        for line in script.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty()
                || trimmed.starts_with('#')
                || trimmed.starts_with("set ")
                || trimmed.starts_with("export ")
                || trimmed.starts_with("if ")
                || trimmed.starts_with("fi")
                || trimmed.starts_with("then")
                || trimmed.starts_with("else")
                || trimmed.starts_with("case ")
                || trimmed.starts_with("esac")
                || trimmed.starts_with("for ")
                || trimmed.starts_with("done")
                || trimmed.starts_with("while ")
                || trimmed.starts_with("do ")
                || trimmed.starts_with("local ")
                || trimmed.starts_with("return ")
                || trimmed.starts_with("echo ")
                || trimmed.starts_with("printf ")
                || trimmed.starts_with("exit ")
                || trimmed.starts_with("true")
                || trimmed.starts_with("false")
                || trimmed.starts_with(':')
                { continue; }
                if let Some(action) = self.translate_line(trimmed) {
                    actions.push(action);
                }
        }
        actions
    }

    fn translate_line(&self, line: &str) -> Option<Action> {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.is_empty() { return None; }
        // Strip leading variable assignments (FOO=bar cmd …)
        let cmd_idx = tokens.iter().position(|t| !t.contains('=')).unwrap_or(0);
        let tokens  = &tokens[cmd_idx..];
        if tokens.is_empty() { return None; }
        let cmd = tokens[0];

        match cmd {
            "useradd" | "adduser"   => Some(parse_adduser(tokens)),
            "groupadd" | "addgroup" => Some(parse_addgroup(tokens)),
            "usermod"               => Some(Action::Skipped {
                original_line: line.to_string(),
                                            reason: "usermod: handled by system if user exists".to_string(),
            }),
            "systemctl" => {
                if tokens.len() < 2 { return None; }
                match tokens[1] {
                    "enable" | "enable-now" => {
                        let unit = tokens.get(2).unwrap_or(&"")
                        .trim_start_matches("--now")
                        .trim_start_matches("--no-reload");
                        if unit.is_empty() { return None; }
                        Some(Action::EnableUnit { unit: unit.to_string() })
                    }
                    "start"        => Some(Action::StartUnit  { unit: tokens.get(2).unwrap_or(&"").to_string() }),
                    "stop"         => Some(Action::StopUnit   { unit: tokens.get(2).unwrap_or(&"").to_string() }),
                    "disable"      => Some(Action::DisableUnit { unit: tokens.get(2).unwrap_or(&"").to_string() }),
                    "daemon-reload"=> Some(Action::DaemonReload),
                    sub            => Some(Action::Skipped {
                        original_line: line.to_string(),
                                           reason: format!("systemctl {}: not translated", sub),
                    }),
                }
            }
            "deb-systemd-helper" | "deb-systemd-invoke" => {
                let sub  = tokens.iter().find(|t| matches!(**t, "enable"|"disable"|"start"|"stop"));
                let unit = tokens.last().unwrap_or(&"");
                match sub {
                    Some(&"enable")  => Some(Action::EnableUnit  { unit: unit.to_string() }),
                    Some(&"start")   => Some(Action::StartUnit   { unit: unit.to_string() }),
                    Some(&"disable") => Some(Action::DisableUnit { unit: unit.to_string() }),
                    _ => Some(Action::Skipped { original_line: line.to_string(), reason: "deb-systemd-helper: unknown".to_string() }),
                }
            }
            "update-alternatives" => {
                if tokens.get(1) == Some(&"--install") && tokens.len() >= 6 {
                    Some(Action::RegisterAlternative {
                        link:     tokens[2].to_string(),
                         name:     tokens[3].to_string(),
                         path:     tokens[4].to_string(),
                         priority: tokens[5].parse().unwrap_or(50),
                    })
                } else {
                    Some(Action::Skipped { original_line: line.to_string(), reason: "update-alternatives: only --install translated".to_string() })
                }
            }
            "ldconfig" => Some(Action::RunLdconfig),
            "ln" => {
                let force = tokens.iter().any(|t| t.contains('f'));
                let args: Vec<&str> = tokens.iter().filter(|t| !t.starts_with('-')).skip(1).copied().collect();
                if args.len() >= 2 {
                    Some(Action::CreateSymlink { src: args[0].to_string(), dest: args[1].to_string(), force })
                } else {
                    Some(Action::Skipped { original_line: line.to_string(), reason: "ln: missing args".to_string() })
                }
            }
            "mkdir" => {
                let mode = tokens.windows(2).find(|w| w[0]=="-m"||w[0]=="--mode").map(|w| w[1].to_string());
                let path = tokens.iter().filter(|t| !t.starts_with('-')).nth(1).unwrap_or(&"").to_string();
                if path.is_empty() { return None; }
                Some(Action::CreateDir { path, mode })
            }
            "chown" => {
                let args: Vec<&str> = tokens.iter().filter(|t| !t.starts_with('-')).skip(1).copied().collect();
                if args.len() >= 2 { Some(Action::SetOwner { path: args[1].to_string(), owner: args[0].to_string() }) }
                else               { Some(Action::Skipped { original_line: line.to_string(), reason: "chown: missing args".to_string() }) }
            }
            "chmod" => {
                let args: Vec<&str> = tokens.iter().filter(|t| !t.starts_with('-')).skip(1).copied().collect();
                if args.len() >= 2 { Some(Action::SetMode { path: args[1].to_string(), mode: args[0].to_string() }) }
                else               { Some(Action::Skipped { original_line: line.to_string(), reason: "chmod: missing args".to_string() }) }
            }
            "install" => {
                let mode  = tokens.windows(2).find(|w| w[0]=="-m"||w[0]=="--mode").map(|w| w[1].to_string());
                let owner = tokens.windows(2).find(|w| w[0]=="-o"||w[0]=="--owner").map(|w| w[1].to_string());
                let dir_mode = tokens.iter().any(|t| *t == "-d");
                let pos: Vec<&str> = tokens.iter().filter(|t| !t.starts_with('-')).skip(1).copied().collect();
                if dir_mode && pos.len() == 1 { Some(Action::CreateDir { path: pos[0].to_string(), mode }) }
                else if pos.len() >= 2        { Some(Action::InstallFile { src: pos[0].to_string(), dest: pos[1].to_string(), mode, owner }) }
                else                          { Some(Action::Skipped { original_line: line.to_string(), reason: "install: missing args".to_string() }) }
            }
            "dpkg" | "dpkg-reconfigure" | "dpkg-statoverride" | "dpkg-trigger"
            | "apt" | "apt-get" | "apt-cache" => {
                Some(Action::Skipped { original_line: line.to_string(), reason: format!("{}: replaced by hammer", cmd) })
            }
            _ => {
                let interesting = ["service","invoke-rc.d","update-rc.d","locale-gen",
                "update-initramfs","grub-install","update-grub","mkinitramfs"];
                let reason = if interesting.iter().any(|&i| cmd.contains(i)) {
                    format!("{}: system action skipped in hammer environment", cmd)
                } else {
                    format!("{}: unknown command", cmd)
                };
                Some(Action::Skipped { original_line: line.to_string(), reason })
            }
        }
    }

    pub fn execute_all(&self, actions: &[Action]) -> Vec<ActionResult> {
        actions.iter().map(|a| self.execute_one(a)).collect()
    }

    pub fn execute_one(&self, action: &Action) -> ActionResult {
        match action {
            Action::CreateUser { name, system, home, shell, group } =>
            execute_create_user(name, *system, home.as_deref(), shell.as_deref(), group.as_deref()),
            Action::CreateGroup { name, system, gid } =>
            execute_create_group(name, *system, *gid),
            Action::EnableUnit  { unit } => execute_systemctl("enable",  unit),
            Action::StartUnit   { unit } => execute_systemctl("start",   unit),
            Action::StopUnit    { unit } => execute_systemctl("stop",    unit),
            Action::DisableUnit { unit } => execute_systemctl("disable", unit),
            Action::DaemonReload         => execute_simple("systemctl", &["daemon-reload"]),
            Action::RegisterAlternative { link, name, path, priority } =>
            execute_update_alternatives(link, name, path, *priority),
            Action::RunLdconfig => execute_simple("ldconfig", &[]),
            Action::CreateSymlink { src, dest, force } => execute_symlink(src, dest, *force),
            Action::CreateDir  { path, mode }  => execute_mkdir(path, mode.as_deref()),
            Action::SetOwner   { path, owner } => execute_chown(path, owner),
            Action::SetMode    { path, mode  } => execute_chmod(path, mode),
            Action::InstallFile { src, dest, mode, owner } =>
            execute_install_file(src, dest, mode.as_deref(), owner.as_deref()),
            Action::Skipped { original_line, reason } => {
                log::info(&format!("postinst skip: {} ({})",
                                   &original_line[..original_line.len().min(60)], reason));
                ActionResult { action: "skip".to_string(), success: true,
                    message: format!("skipped: {}", reason) }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  Execution helpers
// ─────────────────────────────────────────────────────────────

fn execute_create_user(name: &str, system: bool, home: Option<&str>, shell: Option<&str>, group: Option<&str>) -> ActionResult {
    if user_exists(name) {
        return ActionResult { action: format!("user:{}", name), success: true,
        message: format!("user '{}' already exists", name) };
    }
    let mut args = vec!["--no-create-home".to_string()];
    if system      { args.push("--system".to_string()); }
    if let Some(h) = home  { args.extend(["--home-dir".to_string(), h.to_string()]); }
    if let Some(s) = shell { args.extend(["--shell".to_string(), s.to_string()]); }
    if let Some(g) = group { args.extend(["--ingroup".to_string(), g.to_string()]); }
    args.push(name.to_string());

    let ok = Command::new("adduser").args(&args).status().map(|s| s.success()).unwrap_or(false);
    if ok {
        log::info(&format!("postinst: created user '{}'", name));
        ActionResult { action: format!("user:{}", name), success: true, message: format!("created user '{}'", name) }
    } else {
        let mut ua = vec!["-r".to_string(), "-M".to_string()];
        if let Some(s) = shell { ua.extend(["-s".to_string(), s.to_string()]); }
        ua.push(name.to_string());
        let ok2 = Command::new("useradd").args(&ua).status().map(|s| s.success()).unwrap_or(false);
        log::info(&format!("postinst: {} user '{}' (useradd)", if ok2 { "created" } else { "failed" }, name));
        ActionResult { action: format!("user:{}", name), success: ok2,
        message: format!("{} user '{}' via useradd", if ok2 { "created" } else { "failed" }, name) }
    }
}

fn execute_create_group(name: &str, system: bool, gid: Option<u32>) -> ActionResult {
    if group_exists(name) {
        return ActionResult { action: format!("group:{}", name), success: true,
        message: format!("group '{}' already exists", name) };
    }
    let mut args = vec![];
    if system { args.push("--system".to_string()); }
    if let Some(g) = gid { args.extend(["--gid".to_string(), g.to_string()]); }
    args.push(name.to_string());
    let ok = Command::new("groupadd").args(&args).status().map(|s| s.success()).unwrap_or(false);
    log::info(&format!("postinst: {} group '{}'", if ok { "created" } else { "failed" }, name));
    ActionResult { action: format!("group:{}", name), success: ok,
    message: format!("{} group '{}'", if ok { "created" } else { "failed" }, name) }
}

fn execute_systemctl(op: &str, unit: &str) -> ActionResult {
    let unit = unit.trim();
    if unit.is_empty() || unit.contains("hammer-activate") {
        return ActionResult { action: format!("systemctl-{}", op), success: true,
        message: "skipped".to_string() };
    }
    let extra: &[&str] = if op == "enable" { &["--no-reload"] } else { &[] };
    let mut args = vec![op, unit];
    args.extend_from_slice(extra);
    let ok = Command::new("systemctl").args(&args).status().map(|s| s.success()).unwrap_or(false);
    log::info(&format!("postinst: systemctl {} {} → {}", op, unit, if ok { "ok" } else { "failed" }));
    ActionResult { action: format!("systemctl-{}", op), success: ok,
    message: format!("systemctl {} {}", op, unit) }
}

fn execute_update_alternatives(link: &str, name: &str, path: &str, priority: u32) -> ActionResult {
    let real_path = if path.contains("/hammer/store/") || path.contains("/hammer/profiles/") {
        let suffix = path.split("/usr/").nth(1).unwrap_or(path);
        format!("/hammer/active/usr/{}", suffix)
    } else { path.to_string() };

    let ok = Command::new("update-alternatives")
    .args(["--install", link, name, &real_path, &priority.to_string()])
    .status().map(|s| s.success()).unwrap_or(false);
    if ok { let _ = Command::new("update-alternatives").args(["--set", name, &real_path]).status(); }
    log::info(&format!("postinst: alternatives {} → {} ({})", name, real_path, if ok { "ok" } else { "failed" }));
    ActionResult { action: "alternatives".to_string(), success: ok,
        message: format!("registered alternative {} → {}", name, real_path) }
}

fn execute_simple(cmd: &str, args: &[&str]) -> ActionResult {
    let ok = Command::new(cmd).args(args).status().map(|s| s.success()).unwrap_or(false);
    ActionResult { action: cmd.to_string(), success: ok, message: format!("{} {}", cmd, args.join(" ")) }
}

fn execute_symlink(src: &str, dest: &str, force: bool) -> ActionResult {
    let dp = Path::new(dest);
    if force && dp.symlink_metadata().is_ok() { std::fs::remove_file(dp).ok(); }
    if let Some(parent) = dp.parent() { std::fs::create_dir_all(parent).ok(); }
    let ok = std::os::unix::fs::symlink(src, dp).is_ok();
    ActionResult { action: "symlink".to_string(), success: ok, message: format!("ln -s {} {}", src, dest) }
}

fn execute_mkdir(path: &str, mode: Option<&str>) -> ActionResult {
    let ok = std::fs::create_dir_all(path).is_ok();
    if ok { if let Some(m) = mode { let _ = Command::new("chmod").args([m, path]).status(); } }
    ActionResult { action: "mkdir".to_string(), success: ok, message: format!("mkdir -p {}", path) }
}

fn execute_chown(path: &str, owner: &str) -> ActionResult {
    if !Path::new(path).exists() {
        return ActionResult { action: "chown".to_string(), success: true,
            message: format!("chown: {} does not exist, skipped", path) };
    }
    let ok = Command::new("chown").args([owner, path]).status().map(|s| s.success()).unwrap_or(false);
    ActionResult { action: "chown".to_string(), success: ok, message: format!("chown {} {}", owner, path) }
}

fn execute_chmod(path: &str, mode: &str) -> ActionResult {
    if !Path::new(path).exists() {
        return ActionResult { action: "chmod".to_string(), success: true,
            message: format!("chmod: {} does not exist, skipped", path) };
    }
    let ok = Command::new("chmod").args([mode, path]).status().map(|s| s.success()).unwrap_or(false);
    ActionResult { action: "chmod".to_string(), success: ok, message: format!("chmod {} {}", mode, path) }
}

fn execute_install_file(src: &str, dest: &str, mode: Option<&str>, owner: Option<&str>) -> ActionResult {
    if !Path::new(src).exists() {
        return ActionResult { action: "install".to_string(), success: true,
            message: format!("install: {} not found, skipped", src) };
    }
    if let Some(p) = Path::new(dest).parent() { std::fs::create_dir_all(p).ok(); }
    let ok = std::fs::copy(src, dest).is_ok();
    if ok {
        if let Some(m) = mode  { let _ = Command::new("chmod").args([m,   dest]).status(); }
        if let Some(o) = owner { let _ = Command::new("chown").args([o,   dest]).status(); }
    }
    ActionResult { action: "install".to_string(), success: ok, message: format!("install {} → {}", src, dest) }
}

// ─────────────────────────────────────────────────────────────
//  Parsing helpers
// ─────────────────────────────────────────────────────────────

fn parse_adduser(tokens: &[&str]) -> Action {
    let system = tokens.iter().any(|t| *t == "--system");
    let home   = tokens.windows(2).find(|w| w[0]=="--home"||w[0]=="--home-dir"||w[0]=="-d").map(|w| w[1].to_string());
    let shell  = tokens.windows(2).find(|w| w[0]=="--shell"||w[0]=="-s").map(|w| w[1].to_string());
    let group  = tokens.windows(2).find(|w| w[0]=="--ingroup").map(|w| w[1].to_string());
    let name   = tokens.iter().filter(|t| !t.starts_with('-') && !matches!(**t, "useradd"|"adduser")).last().unwrap_or(&"").to_string();
    Action::CreateUser { name, system, home, shell, group }
}

fn parse_addgroup(tokens: &[&str]) -> Action {
    let system = tokens.iter().any(|t| *t == "--system");
    let gid    = tokens.windows(2).find(|w| w[0]=="--gid"||w[0]=="-g").and_then(|w| w[1].parse().ok());
    let name   = tokens.iter().filter(|t| !t.starts_with('-') && !matches!(**t, "groupadd"|"addgroup")).last().unwrap_or(&"").to_string();
    Action::CreateGroup { name, system, gid }
}

fn user_exists(name: &str) -> bool {
    Command::new("id").arg(name).output().map(|o| o.status.success()).unwrap_or(false)
}

fn group_exists(name: &str) -> bool {
    std::fs::read_to_string("/etc/group").unwrap_or_default()
    .lines().any(|l| l.starts_with(&format!("{}:", name)))
}
