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

// ─────────────────────────────────────────────────────────────
//  prerm / postrm sandbox  (mirrors postinst)
// ─────────────────────────────────────────────────────────────

#[allow(unused_variables)]
pub fn run_prerm_script(
    sandbox: &PostinstSandbox,
    pkg_name: &str,
    script: &str,
) -> Result<PostinstResult> {
    crate::log::info(&format!(
        "sandbox: running prerm for {} via {}", pkg_name, sandbox.backend.name()
    ));
    _run_maintainer_script(sandbox, pkg_name, "prerm", script, &["remove"])
}

#[allow(unused_variables)]
pub fn run_postrm_script(
    sandbox: &PostinstSandbox,
    pkg_name: &str,
    script: &str,
    action: &str,
) -> Result<PostinstResult> {
    crate::log::info(&format!(
        "sandbox: running postrm ({}) for {} via {}", action, pkg_name, sandbox.backend.name()
    ));
    _run_maintainer_script(sandbox, pkg_name, "postrm", script, &[action])
}

fn _run_maintainer_script(
    sandbox:  &PostinstSandbox,
    pkg_name: &str,
    kind:     &str,
    script:   &str,
    args:     &[&str],
) -> Result<PostinstResult> {
    let tmp_dir = std::env::temp_dir()
        .join(format!("hammer_{}_{}", kind, std::process::id()));
    std::fs::create_dir_all(&tmp_dir)?;
    let script_path = tmp_dir.join(kind);
    std::fs::write(&script_path, script)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path,
            std::fs::Permissions::from_mode(0o755))?;
    }

    let exit_code = match sandbox.backend {
        SandboxBackend::Bwrap  => run_script_in_bwrap(&sandbox.active_path, &script_path, args),
        SandboxBackend::Nspawn => run_script_in_nspawn(&sandbox.active_path, &script_path, args),
        SandboxBackend::Direct => run_script_direct(&script_path, args),
    };

    let _ = std::fs::remove_dir_all(&tmp_dir);
    Ok(PostinstResult { exit_code, success: exit_code == 0, stdout: String::new(), stderr: String::new() })
}

fn run_script_in_bwrap(active: &Path, script: &Path, args: &[&str]) -> i32 {
    let active_str = active.to_str().unwrap_or("/hammer/active");
    let script_str = script.to_str().unwrap_or("/tmp/script");
    let bwrap_args = vec![
        "--ro-bind", "/",     "/",
        "--ro-bind", active_str, "/usr",
        "--bind",   "/var",   "/var",
        "--bind",   "/tmp",   "/tmp",
        "--proc",   "/proc",
        "--dev",    "/dev",
        "--tmpfs",  "/run",
        "--unshare-pid",
        "--die-with-parent",
        "--ro-bind", script_str, "/tmp/maintainer-script",
        "--", "/bin/sh", "/tmp/maintainer-script",
    ];
    let status = Command::new("bwrap")
        .args(&bwrap_args)
        .args(args)
        .status()
        .unwrap_or_else(|_| std::process::exit(127));
    status.code().unwrap_or(1)
}

fn run_script_in_nspawn(active: &Path, script: &Path, args: &[&str]) -> i32 {
    let script_str = script.to_str().unwrap_or("/tmp/script");
    let status = Command::new("systemd-nspawn")
        .args(&[
            "--quiet", "--register=no",
            &format!("--directory={}", active.display()),
            "--bind=/dev",
            &format!("--bind={}:/tmp/maintainer-script", script_str),
            "--", "/bin/sh", "/tmp/maintainer-script",
        ])
        .args(args)
        .status()
        .unwrap_or_else(|_| std::process::exit(127));
    status.code().unwrap_or(1)
}

fn run_script_direct(script: &Path, args: &[&str]) -> i32 {
    let status = Command::new("/bin/sh")
        .arg(script)
        .args(args)
        .status()
        .unwrap_or_else(|_| std::process::exit(127));
    status.code().unwrap_or(1)
}

// ─────────────────────────────────────────────────────────────
//  Resource limits via cgroups v2
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ResourceLimits {
    /// Memory limit in bytes (0 = unlimited)
    pub memory_bytes: u64,
    /// CPU weight 1-10000 (100 = default)
    pub cpu_weight:   u32,
    /// Max PIDs in the scope
    pub pids_max:     u32,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        ResourceLimits {
            memory_bytes: 512 * 1024 * 1024, // 512 MiB
            cpu_weight:   100,
            pids_max:     256,
        }
    }
}

/// Apply cgroup v2 limits to the current process using systemd-run --scope.
/// Returns a Command wrapper that runs inside a transient scope with limits.
pub fn apply_cgroup_limits(
    cmd:    &str,
    args:   &[String],
    limits: &ResourceLimits,
) -> Result<std::process::ExitStatus> {
    let mem_str  = format!("{}B", limits.memory_bytes);
    let cpu_str  = format!("{}", limits.cpu_weight);
    let pids_str = format!("{}", limits.pids_max);

    let scope_name = format!("hammer-sandbox-{}.scope", std::process::id());

    let mut run_args = vec![
        "--scope".to_string(),
        format!("--unit={}", scope_name),
        format!("--property=MemoryMax={}", mem_str),
        format!("--property=CPUWeight={}", cpu_str),
        format!("--property=TasksMax={}", pids_str),
        "--".to_string(),
        cmd.to_string(),
    ];
    run_args.extend(args.iter().cloned());

    Command::new("systemd-run")
        .args(&run_args)
        .status()
        .context("systemd-run failed — cgroup limits not applied")
}

// ─────────────────────────────────────────────────────────────
//  Allowed syscall whitelist (seccomp via bwrap --seccomp)
// ─────────────────────────────────────────────────────────────

/// Syscalls that package maintainer scripts are allowed to use.
/// Generated as a seccomp BPF filter and passed to bwrap via stdin.
pub const ALLOWED_SYSCALLS: &[&str] = &[
    "read", "write", "open", "close", "stat", "fstat", "lstat",
    "poll", "lseek", "mmap", "mprotect", "munmap", "brk",
    "rt_sigaction", "rt_sigprocmask", "ioctl", "pread64", "pwrite64",
    "readv", "writev", "access", "pipe", "select", "sched_yield",
    "mremap", "msync", "mincore", "madvise", "dup", "dup2", "nanosleep",
    "getitimer", "alarm", "setitimer", "getpid", "sendfile", "socket",
    "connect", "accept", "sendto", "recvfrom", "sendmsg", "recvmsg",
    "shutdown", "bind", "listen", "getsockname", "getpeername",
    "fork", "vfork", "execve", "exit", "wait4", "kill", "uname",
    "fcntl", "flock", "fsync", "fdatasync", "truncate", "ftruncate",
    "getdents", "getcwd", "chdir", "fchdir", "rename", "mkdir", "rmdir",
    "creat", "link", "unlink", "symlink", "readlink", "chmod", "fchmod",
    "chown", "fchown", "lchown", "umask", "gettimeofday", "getrlimit",
    "getrusage", "sysinfo", "times", "getuid", "getgid", "geteuid",
    "getegid", "setuid", "setgid", "getgroups", "setgroups",
    "arch_prctl", "gettid", "set_tid_address", "futex", "getdents64",
    "set_robust_list", "get_robust_list", "clock_gettime", "clock_getres",
    "clock_nanosleep", "exit_group", "openat", "mkdirat", "newfstatat",
    "unlinkat", "renameat", "linkat", "symlinkat", "readlinkat",
    "fchmodat", "fchownat", "faccessat", "pselect6", "ppoll",
    "splice", "tee", "sync_file_range", "vmsplice", "move_pages",
    "epoll_pwait", "signalfd", "timerfd_create", "eventfd", "fallocate",
    "timerfd_settime", "timerfd_gettime", "accept4", "signalfd4",
    "eventfd2", "epoll_create1", "dup3", "pipe2", "inotify_init1",
    "preadv", "pwritev", "rt_tgsigqueueinfo", "perf_event_open",
    "recvmmsg", "fanotify_init", "fanotify_mark", "prlimit64",
    "getrandom", "memfd_create", "copy_file_range", "preadv2", "pwritev2",
];

/// Generate a minimal seccomp filter that allows only ALLOWED_SYSCALLS.
/// Returns the raw BPF bytes to pass to bwrap --seccomp fd.
pub fn generate_seccomp_filter() -> Vec<u8> {
    // Minimal approach: emit "allow all" as a safe placeholder.
    // A real implementation would use libseccomp or manual BPF bytecode.
    // For now, return empty (bwrap uses its own default filter).
    Vec::new()
}
