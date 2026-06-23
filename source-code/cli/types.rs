use owo_colors::OwoColorize;

#[derive(Debug, Default, Clone)]
pub struct GlobalFlags {
    // ── Confirmation ──────────────────────────────────────────
    pub yes:          bool,   // -y / --yes
    pub assume_yes:   bool,   // --assume-yes (non-interactive)

    // ── Output mode ───────────────────────────────────────────
    pub verbose:      bool,   // -v / --verbose
    pub quiet:        bool,   // -q / --quiet
    pub debug:        bool,   // --debug
    pub json:         bool,   // --json
    pub no_progress:  bool,   // --no-progress
    pub color:        ColorMode,

    // ── Operation ─────────────────────────────────────────────
    pub dry_run:      bool,   // --dry-run / -n
    pub user_mode:    bool,   // --user / -U
    pub arch:         Option<String>, // --arch=<arch>
    pub root:         Option<String>, // --root=<path>

    // ── Network ───────────────────────────────────────────────
    pub no_download:  bool,   // --no-download (use cache only)
    pub force:        bool,   // --force (skip guards)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum ColorMode { #[default] Auto, Always, Never }

impl GlobalFlags {
    pub fn parse(args: &mut Vec<String>) -> Self {
        let mut f = GlobalFlags::default();

        // Parse env var overrides first
        if std::env::var("HAMMER_LOG_LEVEL").as_deref() == Ok("debug") {
            f.debug = true;
            f.verbose = true;
        }
        if std::env::var("NO_COLOR").is_ok() { f.color = ColorMode::Never; }

        args.retain(|a| {
            match a.as_str() {
                "--user" | "-U"          => { f.user_mode    = true; false }
                "-y" | "--yes"           => { f.yes          = true; false }
                "--assume-yes"           => { f.assume_yes   = true; f.yes = true; false }
                "-v" | "--verbose"       => { f.verbose      = true; false }
                "-q" | "--quiet"         => { f.quiet        = true; false }
                "--debug"                => { f.debug        = true; f.verbose = true; false }
                "--json"                 => { f.json         = true; false }
                "--no-progress"          => { f.no_progress  = true; false }
                "-n" | "--dry-run"       => { f.dry_run      = true; false }
                "--no-download"          => { f.no_download  = true; false }
                "--force"                => { f.force        = true; false }
                "--color=always"         => { f.color = ColorMode::Always; false }
                "--color=never"          => { f.color = ColorMode::Never;  false }
                "--color=auto"           => { f.color = ColorMode::Auto;   false }
                _ => {
                    if let Some(arch) = a.strip_prefix("--arch=") {
                        f.arch = Some(arch.to_string()); false
                    } else if let Some(root) = a.strip_prefix("--root=") {
                        f.root = Some(root.to_string()); false
                    } else {
                        true
                    }
                }
            }
        });

        // Apply color mode globally
        // Color mode is applied via env var NO_COLOR / CLICOLOR_FORCE
        if f.color == ColorMode::Never  { std::env::set_var("NO_COLOR", "1"); }
        if f.color == ColorMode::Always { std::env::set_var("CLICOLOR_FORCE", "1"); }

        // Apply log level
        if f.debug   { crate::log::set_verbose(); }
        if f.quiet   { crate::log::set_quiet(); }

        f
    }

    /// Whether output should be shown with color.
    pub fn use_color(&self) -> bool {
        match self.color {
            ColorMode::Always => true,
            ColorMode::Never  => false,
            ColorMode::Auto   => {
                // Auto: use color if stdout is a terminal
                unsafe { libc::isatty(libc::STDOUT_FILENO) != 0 }
            }
        }
    }

    /// Print a dry-run banner (call at start of any mutating command).
    pub fn print_dry_run_banner(&self) {
        if self.dry_run {
            println!("  {} {} — no changes will be made",
                     "DRY-RUN".bold().yellow(), "mode".bold());
        }
    }

    /// Whether interactive prompts should be shown.
    pub fn interactive(&self) -> bool {
        !self.yes && !self.assume_yes && !self.json && !self.no_progress
    }
}

// ── Helpers ───────────────────────────────────────────────────

pub fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

pub fn flag_value<'a>(args: &'a [String], prefix: &str) -> Option<&'a str> {
    args.iter()
        .find(|a| a.starts_with(prefix))
        .map(|a| a[prefix.len()..].trim_start_matches('='))
}

pub fn non_flag_args(args: &[String]) -> Vec<String> {
    args.iter().filter(|a| !a.starts_with('-')).cloned().collect()
}
