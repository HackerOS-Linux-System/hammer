use anyhow::{bail, Context, Result};
use std::process::{Command, Stdio};

pub struct RunResult {
    pub status_ok: bool,
    pub stdout:    String,
    pub stderr:    String,
}

/// Uruchamia `program args...`, przechwytuje stdout/stderr. Nie dziedziczy
/// terminala rodzica — użyj [`run_inherit`] dla operacji długotrwałych,
/// gdzie chcemy pokazać użytkownikowi na żywo output (np. `skopeo copy`).
pub fn run_capture(program: &str, args: &[&str]) -> Result<RunResult> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("Cannot spawn '{}'", program))?;

    Ok(RunResult {
        status_ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    })
}

/// Jak [`run_capture`], ale zwraca błąd jeśli proces nie zakończył się 0.
pub fn run_capture_checked(program: &str, args: &[&str]) -> Result<String> {
    let r = run_capture(program, args)?;
    if !r.status_ok {
        bail!("'{} {}' failed: {}", program, args.join(" "), r.stderr.trim());
    }
    Ok(r.stdout)
}

/// Uruchamia proces dziedzicząc stdin/stdout/stderr rodzica (użytkownik
/// widzi output na żywo — dla `skopeo copy`, `apt`-like operacji, itd).
pub fn run_inherit(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("Cannot spawn '{}'", program))?;

    if !status.success() {
        bail!("'{} {}' exited with {}", program, args.join(" "), status);
    }
    Ok(())
}

/// Uruchamia `program args...` w katalogu `cwd`, dziedzicząc I/O rodzica.
pub fn run_inherit_in(program: &str, args: &[&str], cwd: &std::path::Path) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("Cannot spawn '{}' in {}", program, cwd.display()))?;

    if !status.success() {
        bail!("'{} {}' (cwd={}) exited with {}", program, args.join(" "), cwd.display(), status);
    }
    Ok(())
}

/// Sprawdza czy `program` istnieje w `$PATH`.
pub fn tool_available(program: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {} >/dev/null 2>&1", program))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
