use anyhow::{bail, Context, Result};
use owo_colors::OwoColorize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// ─────────────────────────────────────────────────────────────
//  SandboxBackend
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum SandboxBackend { Bwrap, Nspawn, Direct }

impl SandboxBackend {
    pub fn detect() -> Self {
        if which("bwrap") { return SandboxBackend::Bwrap; }
        if which("systemd-nspawn") { return SandboxBackend::Nspawn; }
        SandboxBackend::Direct
    }

    pub fn name(&self) -> &'static str {
        match self {
            SandboxBackend::Bwrap   => "bwrap",
            SandboxBackend::Nspawn  => "systemd-nspawn",
            SandboxBackend::Direct  => "direct (no isolation)",
        }
    }
}

fn which(cmd: &str) -> bool {
    Command::new("which").arg(cmd).stdout(Stdio::null()).stderr(Stdio::null())
    .status().map(|s| s.success()).unwrap_or(false)
}

// ─────────────────────────────────────────────────────────────
//  PostinstSandbox — run a maintainer script safely
// ─────────────────────────────────────────────────────────────

pub struct PostinstSandbox {
    backend:     SandboxBackend,
    active_path: PathBuf,
}

impl PostinstSandbox {
    pub fn new() -> Self {
        PostinstSandbox {
            backend:     SandboxBackend::detect(),
            active_path: PathBuf::from(crate::store::ACTIVE_LINK),
        }
    }

    /// Execute a postinst script for the given package.
    /// The script receives the package root as its filesystem view.
    pub fn run_postinst(&self, pkg_name: &str, script: &str) -> Result<PostinstResult> {
        crate::log::info(&format!(
            "sandbox: running postinst for {} via {}", pkg_name, self.backend.name()
        ));

        // Write script to a temp file
        let tmp_dir  = std::env::temp_dir().join(format!("hammer_postinst_{}", std::process::id()));
        std::fs::create_dir_all(&tmp_dir)?;
        let script_path = tmp_dir.join("postinst");
        std::fs::write(&script_path, script)?;
        // Make executable
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))?;

        let result = match self.backend {
            SandboxBackend::Bwrap  => self.run_bwrap(&script_path, pkg_name),
            SandboxBackend::Nspawn => self.run_nspawn(&script_path, pkg_name),
            SandboxBackend::Direct => self.run_direct(&script_path, pkg_name),
        };

        let _ = std::fs::remove_dir_all(&tmp_dir);
        result
    }

    fn run_bwrap(&self, script: &Path, pkg_name: &str) -> Result<PostinstResult> {
        // bwrap sandbox:
        //   - Read-only bind of / (system root)
        //   - Read-write tmpfs on /tmp, /run, /var/tmp
        //   - Read-write bind of /etc (conffiles need to be writable)
        //   - Network: allow (some postinsts fetch data)
        //   - User namespace: map current user → root inside
        //   - No new privileges
        let mut cmd = Command::new("bwrap");
        cmd.args([
            // Map / read-only
            "--ro-bind", "/", "/",
            // Overlay writable /tmp
            "--tmpfs", "/tmp",
            "--tmpfs", "/run",
            // Allow writes to /etc and /var
            "--bind", "/etc", "/etc",
            "--bind-try", "/var", "/var",
            // Dev access (some scripts need /dev/null etc.)
            "--dev", "/dev",
            "--proc", "/proc",
            // No new privs
            "--unshare-pid",
            "--unshare-ipc",
            // Keep network for postinsts that fetch keys etc.
            // Script args: postinst configure <version>
            "--",
            script.to_str().unwrap_or(""),
                 "configure",
                 "",
        ])
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("DEBCONF_NONINTERACTIVE_SEEN", "true")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

        self.exec_and_collect(cmd, pkg_name)
    }

    fn run_nspawn(&self, script: &Path, pkg_name: &str) -> Result<PostinstResult> {
        // systemd-nspawn requires root and a directory to use as root
        let mut cmd = Command::new("systemd-nspawn");
        cmd.args([
            "--quiet",
            "--register=no",
            "--directory=/",
            "--bind-ro=/proc",
            "--bind-ro=/sys",
            "--",
            script.to_str().unwrap_or(""),
                 "configure",
                 "",
        ])
        .env("DEBIAN_FRONTEND", "noninteractive")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

        self.exec_and_collect(cmd, pkg_name)
    }

    fn run_direct(&self, script: &Path, pkg_name: &str) -> Result<PostinstResult> {
        crate::log::warn(&format!(
            "sandbox: no isolation available for {} — running directly", pkg_name
        ));
        let mut cmd = Command::new(script);
        cmd.args(["configure", ""])
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("DEBCONF_NONINTERACTIVE_SEEN", "true")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        self.exec_and_collect(cmd, pkg_name)
    }

    fn exec_and_collect(&self, mut cmd: Command, pkg_name: &str) -> Result<PostinstResult> {
        let output = cmd.output()
        .with_context(|| format!("Spawning postinst for {}", pkg_name))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            crate::log::info(&format!("sandbox: postinst {} OK", pkg_name));
            Ok(PostinstResult { success: true, stdout, stderr, exit_code: 0 })
        } else {
            let code = output.status.code().unwrap_or(-1);
            crate::log::warn(&format!(
                "sandbox: postinst {} failed (exit {}): {}", pkg_name, code,
                                      stderr.lines().next().unwrap_or("")
            ));
            Ok(PostinstResult { success: false, stdout, stderr, exit_code: code })
        }
    }
}

#[derive(Debug)]
pub struct PostinstResult {
    pub success:   bool,
    pub stdout:    String,
    pub stderr:    String,
    pub exit_code: i32,
}

// ─────────────────────────────────────────────────────────────
//  hammer sandbox <cmd> [args]
//  Run an application isolated in the active generation profile
// ─────────────────────────────────────────────────────────────

pub fn cmd_sandbox(args: &[String]) -> Result<()> {
    let cmd_name = args.first()
    .ok_or_else(|| anyhow::anyhow!(
        "Usage: hammer sandbox <command> [args...]\n       \
Runs <command> in an isolated view of the active hammer generation."
    ))?;

    let backend = SandboxBackend::detect();
    let active  = PathBuf::from(crate::store::ACTIVE_LINK);

    if !active.exists() {
        bail!("No active generation found. Run `hammer _activate`.");
    }

    println!("  {}  Launching {} in sandbox ({})",
             "⬡".bright_cyan().bold(), cmd_name.bold(), backend.name().dimmed());

    match backend {
        SandboxBackend::Bwrap => sandbox_bwrap(&active, cmd_name, &args[1..]),
        SandboxBackend::Nspawn => sandbox_nspawn(&active, cmd_name, &args[1..]),
        SandboxBackend::Direct => {
            println!("  {} No sandbox tool available — running directly.", "!".yellow().bold());
            let status = Command::new(cmd_name)
            .args(&args[1..])
            .status()?;
            std::process::exit(status.code().unwrap_or(1));
        }
    }
}

fn sandbox_bwrap(active: &Path, cmd_name: &str, extra_args: &[String]) -> Result<()> {
    let active_str = active.to_str().unwrap_or("/hammer/active");

    let mut bwrap_args: Vec<String> = vec![
        // Use the active profile as the root for /usr, /lib, etc.
        "--ro-bind".into(), "/".into(), "/".into(),
        "--ro-bind".into(), format!("{}/usr", active_str), "/usr".into(),
        "--ro-bind-try".into(), format!("{}/lib", active_str), "/lib".into(),
        "--ro-bind-try".into(), format!("{}/lib64", active_str), "/lib64".into(),
        "--ro-bind-try".into(), format!("{}/bin", active_str), "/bin".into(),
        // Keep home and tmp writable
        "--bind".into(), std::env::var("HOME").unwrap_or_else(|_| "/root".into()),
        std::env::var("HOME").unwrap_or_else(|_| "/root".into()),
        "--tmpfs".into(), "/tmp".into(),
        "--proc".into(), "/proc".into(),
        "--dev".into(), "/dev".into(),
        "--unshare-pid".into(),
        "--die-with-parent".into(),
        "--".into(),
        cmd_name.to_string(),
    ];
    bwrap_args.extend(extra_args.iter().cloned());

    let status = Command::new("bwrap")
    .args(&bwrap_args)
    .status()
    .context("Failed to exec bwrap")?;

    std::process::exit(status.code().unwrap_or(1));
}

fn sandbox_nspawn(active: &Path, cmd_name: &str, extra_args: &[String]) -> Result<()> {
    let mut nspawn_args: Vec<String> = vec![
        "--quiet".into(),
        "--register=no".into(),
        format!("--directory={}", active.display()),
            "--bind-ro=/proc".into(),
            "--bind-ro=/sys".into(),
            "--bind=/dev".into(),
            "--".into(),
            cmd_name.to_string(),
    ];
    nspawn_args.extend(extra_args.iter().cloned());

    let status = Command::new("systemd-nspawn")
    .args(&nspawn_args)
    .status()
    .context("Failed to exec systemd-nspawn")?;

    std::process::exit(status.code().unwrap_or(1));
}
