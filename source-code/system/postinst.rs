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

/// Wynik uruchomienia postinst — używany przez podsumowanie UX (0.5)
#[derive(Debug, Default)]
pub struct PostinstResult {
    /// Serwisy które zostały włączone (systemctl enable)
    pub services_enabled: Vec<String>,
    /// Serwisy które zostały uruchomione (systemctl start)
    pub services_started: Vec<String>,
    /// Użytkownicy systemowi którzy zostali stworzeni
    pub users_created: Vec<String>,
    /// Conffiles stworzone przez postinst
    pub conffiles_created: Vec<String>,
    /// Błędy (nie fatalne)
    pub warnings: Vec<String>,
    /// Liczba pominiętych linii
    pub skipped: usize,
}

// ─────────────────────────────────────────────────────────────
//  Kontekst warunkowy (parser bloków if)
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum CondBlock {
    /// Blok aktywny — linie wewnątrz mają być przetwarzane
    Active,
    /// Blok nieaktywny — pomijaj linie (np. warunek nie dotyczy "configure")
    Skip,
}

/// Sprawdź czy warunek dotyczy fazy "configure" lub "upgrade"
/// Obsługuje:
///   if [ "$1" = "configure" ]
///   if [ "$1" = "configure" ] || [ "$1" = "upgrade" ]
///   if [ -n "$2" ]  (update — "$2" = poprzednia wersja)
fn condition_matches_configure(condition: &str) -> bool {
    let c = condition.trim();
    // Najczęstsze wzorce z Debian postinst
    c.contains("\"configure\"")
        || c.contains("'configure'")
        || c.contains("= configure")
        || c.contains("\"upgrade\"")
        || c.contains("-n \"$2\"")   // jest poprzednia wersja → update
        || c.contains("-n '$2'")
        // Ogólny if bez warunków specyficznych dla dpkg → uruchom
        || (!c.contains("dpkg") && !c.contains("\"abort")
            && !c.contains("remove") && !c.contains("purge"))
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

    /// Przetłumacz skrypt na Actions, obsługując bloki warunkowe
    pub fn translate(&self, script: &str) -> Vec<Action> {
        let mut actions = Vec::new();
        // Stos bloków warunkowych: true = wykonaj, false = pomiń
        let mut cond_stack: Vec<CondBlock> = Vec::new();

        for raw_line in script.lines() {
            let trimmed = raw_line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') { continue; }

            // ── Obsługa bloków if/else/fi ──────────────────────

            if trimmed.starts_with("if ") || trimmed.starts_with("if\t") {
                // Wyodrębnij warunek (część po "if " do ";")
                let condition = trimmed
                    .trim_start_matches("if ")
                    .trim_start_matches("if\t")
                    .trim_end_matches("then")
                    .trim_end_matches(';')
                    .trim();

                let active = condition_matches_configure(condition);
                cond_stack.push(if active { CondBlock::Active } else { CondBlock::Skip });
                continue;
            }

            if trimmed == "else" {
                // Odwróć aktywność bieżącego bloku
                if let Some(top) = cond_stack.last_mut() {
                    *top = if *top == CondBlock::Active {
                        CondBlock::Skip
                    } else {
                        CondBlock::Active
                    };
                }
                continue;
            }

            if trimmed == "fi" {
                cond_stack.pop();
                continue;
            }

            // Pomiń linie wewnątrz nieaktywnego bloku
            if cond_stack.last() == Some(&CondBlock::Skip) {
                continue;
            }

            // ── Linie które zawsze pomijamy ──────────────────────
            if self.should_skip(trimmed) {
                continue;
            }

            if let Some(action) = self.translate_line(trimmed) {
                actions.push(action);
            } else {
                actions.push(Action::Skipped {
                    original_line: trimmed.to_string(),
                    reason: "nieznana komenda".to_string(),
                });
            }
        }

        actions
    }

    fn should_skip(&self, line: &str) -> bool {
        line.starts_with("set ")
            || line.starts_with("export ")
            || line.starts_with("then")
            || line.starts_with("case ")
            || line.starts_with("esac")
            || line.starts_with("for ")
            || line.starts_with("done")
            || line.starts_with("while ")
            || line.starts_with("do ")
            || line.starts_with("local ")
            || line.starts_with("return ")
            || line.starts_with("echo ")
            || line.starts_with("printf ")
            || line.starts_with("exit ")
            || line == "true"
            || line == "false"
            || line.starts_with(':')
            || line.starts_with("function ")
            || line.starts_with("source ")
            || line.starts_with(". ")
            || line.starts_with("read ")
    }

    fn translate_line(&self, line: &str) -> Option<Action> {
        // useradd / adduser
        if line.contains("useradd") || line.contains("adduser") {
            return Some(self.parse_adduser(line));
        }
        // groupadd / addgroup
        if line.contains("groupadd") || line.contains("addgroup") {
            return Some(self.parse_addgroup(line));
        }
        // systemctl
        if line.starts_with("systemctl") || line.contains("deb-systemd-invoke") {
            return self.parse_systemctl(line);
        }
        // update-alternatives
        if line.starts_with("update-alternatives") {
            return self.parse_alternatives(line);
        }
        // ldconfig
        if line.starts_with("ldconfig") {
            return Some(Action::RunLdconfig);
        }
        // mkdir
        if line.starts_with("mkdir") {
            return Some(self.parse_mkdir(line));
        }
        // chown
        if line.starts_with("chown") {
            return Some(self.parse_chown(line));
        }
        // chmod
        if line.starts_with("chmod") {
            return Some(self.parse_chmod(line));
        }
        // ln -s
        if line.starts_with("ln ") {
            return Some(self.parse_ln(line));
        }
        // install (GNU install)
        if line.starts_with("install ") {
            return Some(self.parse_install_cmd(line));
        }
        // dpkg / apt → bezpieczne pominięcie
        if line.starts_with("dpkg") || line.starts_with("apt-get") || line.starts_with("apt ") {
            return Some(Action::Skipped {
                original_line: line.to_string(),
                reason: "polecenie dpkg/apt — pominięte".to_string(),
            });
        }

        None
    }

    // ── Parsery linii ─────────────────────────────────────────

    fn parse_adduser(&self, line: &str) -> Action {
        let system = line.contains("--system") || line.contains("-r");
        let home = Self::extract_flag(line, "--home");
        let shell = Self::extract_flag(line, "--shell");
        let group = Self::extract_flag(line, "--ingroup").or_else(|| Self::extract_flag(line, "--gid"));
        let name = line.split_whitespace().last().unwrap_or("").to_string();
        Action::CreateUser { name, system, home, shell, group }
    }

    fn parse_addgroup(&self, line: &str) -> Action {
        let system = line.contains("--system") || line.contains("-r");
        let gid_str = Self::extract_flag(line, "--gid");
        let gid = gid_str.and_then(|s| s.parse().ok());
        let name = line.split_whitespace().last().unwrap_or("").to_string();
        Action::CreateGroup { name, system, gid }
    }

    fn parse_systemctl(&self, line: &str) -> Option<Action> {
        // Wyciągnij podkomendę i nazwę unit
        let parts: Vec<&str> = line.split_whitespace().collect();
        // Znajdź indeks "systemctl" lub "invoke-rc.d"
        let start = parts.iter().position(|&p| p == "systemctl"
            || p.ends_with("deb-systemd-invoke") || p.ends_with("invoke-rc.d"))?;

        let subcmd = parts.get(start + 1)?;

        // Pomiń --no-reload i podobne flagi, weź ostatni argument jako unit
        let unit = parts.iter().rev()
            .find(|&&p| !p.starts_with('-') && p != "systemctl"
                && !p.ends_with("deb-systemd-invoke") && !p.ends_with("invoke-rc.d")
                && *subcmd != p)?
            .to_string();

        match *subcmd {
            "enable" if parts.contains(&"--now") => {
                // enable --now → enable + start
                // Tutaj zwracamy tylko enable; start zostanie z kolejnej linii lub z run()
                Some(Action::EnableUnit { unit })
            }
            "enable"  => Some(Action::EnableUnit  { unit }),
            "start"   => Some(Action::StartUnit   { unit }),
            "stop"    => Some(Action::StopUnit    { unit }),
            "disable" => Some(Action::DisableUnit { unit }),
            "daemon-reload" => Some(Action::DaemonReload),
            _ => None,
        }
    }

    fn parse_alternatives(&self, line: &str) -> Option<Action> {
        if !line.contains("--install") { return None; }
        let parts: Vec<&str> = line.split_whitespace().collect();
        let install_pos = parts.iter().position(|&p| p == "--install")?;
        let link     = parts.get(install_pos + 1)?.to_string();
        let name     = parts.get(install_pos + 2)?.to_string();
        let path     = parts.get(install_pos + 3)?.to_string();
        let priority = parts.get(install_pos + 4)?.parse().unwrap_or(50);
        Some(Action::RegisterAlternative { link, name, path, priority })
    }

    fn parse_mkdir(&self, line: &str) -> Action {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let mode = Self::extract_flag_from_parts(&parts, "-m")
            .or_else(|| Self::extract_flag_from_parts(&parts, "--mode"));
        let path = parts.last().unwrap_or(&"/").to_string();
        Action::CreateDir { path, mode }
    }

    fn parse_chown(&self, line: &str) -> Action {
        let parts: Vec<&str> = line.split_whitespace().collect();
        // chown [opts] owner path
        let owner_idx = parts.iter().position(|p| !p.starts_with('-') && *p != "chown")
            .unwrap_or(1);
        let owner = parts.get(owner_idx).unwrap_or(&"root").to_string();
        let path  = parts.last().unwrap_or(&"/").to_string();
        Action::SetOwner { path, owner }
    }

    fn parse_chmod(&self, line: &str) -> Action {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let mode_idx = parts.iter().position(|p| !p.starts_with('-') && *p != "chmod")
            .unwrap_or(1);
        let mode = parts.get(mode_idx).unwrap_or(&"755").to_string();
        let path = parts.last().unwrap_or(&"/").to_string();
        Action::SetMode { path, mode }
    }

    fn parse_ln(&self, line: &str) -> Action {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let force = parts.contains(&"-f") || parts.contains(&"--force");
        let non_flag: Vec<&&str> = parts.iter().filter(|p| !p.starts_with('-') && **p != "ln").collect();
        let src  = non_flag.get(0).map(|s| s.to_string()).unwrap_or_default();
        let dest = non_flag.get(1).map(|s| s.to_string()).unwrap_or_default();
        Action::CreateSymlink { src, dest, force }
    }

    fn parse_install_cmd(&self, line: &str) -> Action {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let mode  = Self::extract_flag_from_parts(&parts, "-m");
        let owner = Self::extract_flag_from_parts(&parts, "-o");
        let non_flag: Vec<&&str> = parts.iter()
            .filter(|p| !p.starts_with('-') && **p != "install").collect();
        let src  = non_flag.get(non_flag.len().saturating_sub(2)).map(|s| s.to_string()).unwrap_or_default();
        let dest = non_flag.last().map(|s| s.to_string()).unwrap_or_default();
        Action::InstallFile { src, dest, mode, owner }
    }

    fn extract_flag(line: &str, flag: &str) -> Option<String> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        Self::extract_flag_from_parts(&parts, flag)
    }

    fn extract_flag_from_parts(parts: &[&str], flag: &str) -> Option<String> {
        for (i, &p) in parts.iter().enumerate() {
            if p == flag {
                return parts.get(i + 1).map(|s| s.to_string());
            }
            if p.starts_with(&format!("{}=", flag)) {
                return Some(p[flag.len() + 1..].to_string());
            }
        }
        None
    }

    // ── Wykonywanie Actions ───────────────────────────────────

    pub fn run(&self, actions: &[Action]) -> (Vec<ActionResult>, PostinstResult) {
        let mut results = Vec::new();
        let mut summary = PostinstResult::default();

        for action in actions {
            let res = self.execute_action(action, &mut summary);
            results.push(res);
        }

        (results, summary)
    }

    fn execute_action(&self, action: &Action, summary: &mut PostinstResult) -> ActionResult {
        match action {
            Action::CreateUser { name, system, home, shell, group } => {
                let mut cmd = Command::new("adduser");
                if *system { cmd.arg("--system"); }
                cmd.arg("--no-create-home");
                if let Some(h) = home { cmd.arg("--home").arg(h); }
                if let Some(s) = shell { cmd.arg("--shell").arg(s); }
                if let Some(g) = group { cmd.arg("--ingroup").arg(g); }
                cmd.arg(name);

                let ok = cmd.status().map(|s| s.success()).unwrap_or(false);
                if ok { summary.users_created.push(name.clone()); }
                ActionResult {
                    action: format!("adduser {}", name),
                    success: ok || self.user_exists(name),
                    message: if ok { format!("Stworzono użytkownika {}", name) }
                             else   { format!("Użytkownik {} już istnieje lub błąd", name) },
                }
            }

            Action::CreateGroup { name, system, gid } => {
                let mut cmd = Command::new("addgroup");
                if *system { cmd.arg("--system"); }
                if let Some(g) = gid { cmd.arg("--gid").arg(g.to_string()); }
                cmd.arg(name);
                let ok = cmd.status().map(|s| s.success()).unwrap_or(false);
                ActionResult {
                    action: format!("addgroup {}", name),
                    success: ok || self.group_exists(name),
                    message: format!("Grupa {}", name),
                }
            }

            Action::EnableUnit { unit } => {
                let ok = Command::new("systemctl")
                    .args(["enable", "--no-reload", unit])
                    .status().map(|s| s.success()).unwrap_or(false);
                if ok { summary.services_enabled.push(unit.clone()); }
                ActionResult {
                    action: format!("systemctl enable {}", unit),
                    success: ok,
                    message: if ok { format!("Włączono {}", unit) }
                             else   { format!("Nie udało się włączyć {}", unit) },
                }
            }

            Action::StartUnit { unit } => {
                let ok = Command::new("systemctl")
                    .args(["start", unit])
                    .status().map(|s| s.success()).unwrap_or(false);
                if ok { summary.services_started.push(unit.clone()); }
                ActionResult {
                    action: format!("systemctl start {}", unit),
                    success: ok,
                    message: if ok { format!("Uruchomiono {}", unit) }
                             else   { format!("Nie udało się uruchomić {}", unit) },
                }
            }

            Action::StopUnit { unit } => {
                let ok = Command::new("systemctl")
                    .args(["stop", unit])
                    .status().map(|s| s.success()).unwrap_or(false);
                ActionResult { action: format!("systemctl stop {}", unit), success: ok, message: String::new() }
            }

            Action::DisableUnit { unit } => {
                let ok = Command::new("systemctl")
                    .args(["disable", unit])
                    .status().map(|s| s.success()).unwrap_or(false);
                ActionResult { action: format!("systemctl disable {}", unit), success: ok, message: String::new() }
            }

            Action::DaemonReload => {
                let ok = Command::new("systemctl")
                    .arg("daemon-reload")
                    .status().map(|s| s.success()).unwrap_or(false);
                ActionResult { action: "daemon-reload".to_string(), success: ok, message: String::new() }
            }

            Action::RegisterAlternative { link, name, path, priority } => {
                let ok = Command::new("update-alternatives")
                    .args(["--install", link, name, path, &priority.to_string()])
                    .status().map(|s| s.success()).unwrap_or(false);
                ActionResult { action: format!("update-alternatives --install {}", name), success: ok, message: String::new() }
            }

            Action::RunLdconfig => {
                let ok = Command::new("ldconfig").status().map(|s| s.success()).unwrap_or(false);
                ActionResult { action: "ldconfig".to_string(), success: ok, message: String::new() }
            }

            Action::CreateSymlink { src, dest, force } => {
                if *force { let _ = std::fs::remove_file(dest); }
                let ok = std::os::unix::fs::symlink(src, dest).is_ok();
                summary.conffiles_created.push(dest.clone());
                ActionResult { action: format!("ln -s {} {}", src, dest), success: ok, message: String::new() }
            }

            Action::CreateDir { path, mode } => {
                let ok = std::fs::create_dir_all(path).is_ok();
                if ok {
                    if let Some(m) = mode {
                        let _ = Command::new("chmod").args([m, path]).status();
                    }
                    summary.conffiles_created.push(path.clone());
                }
                ActionResult { action: format!("mkdir -p {}", path), success: ok, message: String::new() }
            }

            Action::SetOwner { path, owner } => {
                let ok = Command::new("chown")
                    .args(["-R", owner, path])
                    .status().map(|s| s.success()).unwrap_or(false);
                ActionResult { action: format!("chown {} {}", owner, path), success: ok, message: String::new() }
            }

            Action::SetMode { path, mode } => {
                let ok = Command::new("chmod")
                    .args([mode, path])
                    .status().map(|s| s.success()).unwrap_or(false);
                ActionResult { action: format!("chmod {} {}", mode, path), success: ok, message: String::new() }
            }

            Action::InstallFile { src, dest, mode, owner } => {
                let _ = std::fs::copy(src, dest);
                if let Some(m) = mode { let _ = Command::new("chmod").args([m, dest.as_str()]).status(); }
                if let Some(o) = owner { let _ = Command::new("chown").args([o.as_str(), dest.as_str()]).status(); }
                summary.conffiles_created.push(dest.clone());
                ActionResult { action: format!("install → {}", dest), success: true, message: String::new() }
            }

            Action::Skipped { original_line, reason } => {
                summary.skipped += 1;
                log::debug(&format!("postinst skip [{}]: {}", reason, original_line));
                ActionResult {
                    action: format!("pominięto: {}", original_line),
                    success: true,
                    message: format!("({})", reason),
                }
            }
        }
    }

    fn user_exists(&self, name: &str) -> bool {
        Command::new("id").arg(name).status()
            .map(|s| s.success()).unwrap_or(false)
    }

    fn group_exists(&self, name: &str) -> bool {
        std::fs::read_to_string("/etc/group")
            .map(|c| c.lines().any(|l| l.starts_with(&format!("{}:", name))))
            .unwrap_or(false)
    }
}

// ─────────────────────────────────────────────────────────────
//  Pomocnicze — preinst / postrm (szkielet dla 0.5)
// ─────────────────────────────────────────────────────────────

/// Uruchom preinst skrypt (przed rozpakowaniem paczki).
/// W 0.5 obsługuje tylko "configure" i bezpieczne pominięcia.
pub fn run_preinst(pkg_name: &str, script: &str) -> PostinstResult {
    let translator = PostinstTranslator::new(pkg_name);
    let actions = translator.translate(script);
    let (_, summary) = translator.run(&actions);
    summary
}

/// Uruchom postrm (po usunięciu paczki).
/// Używany przez `hammer undo` do czyszczenia po rollback.
pub fn run_postrm(pkg_name: &str, script: &str) -> PostinstResult {
    // postrm ma inne warunki ($1 = "remove" | "purge" | "upgrade")
    // W 0.5: tylko stop/disable serwisów i ldconfig
    let translator = PostinstTranslator::new(pkg_name);
    let filtered: String = script.lines()
        .filter(|l| {
            let t = l.trim();
            t.starts_with("systemctl stop")
            || t.starts_with("systemctl disable")
            || t.starts_with("ldconfig")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let actions = translator.translate(&filtered);
    let (_, summary) = translator.run(&actions);
    summary
}
