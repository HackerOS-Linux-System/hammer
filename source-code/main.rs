// ── pkg/ ──────────────────────────────────────────────────────
#[path = "pkg/cache.rs"]          mod cache;
#[path = "pkg/db.rs"]             mod db;
#[path = "pkg/deb.rs"]            mod deb;
#[path = "pkg/package.rs"]        mod package;
#[path = "pkg/solver/mod.rs"]     mod solver;
#[path = "pkg/store.rs"]          mod store;
#[path = "pkg/transaction.rs"]    mod transaction;

// ── system/ ───────────────────────────────────────────────────
#[path = "system/gpg.rs"]         mod gpg;
#[path = "system/gpg_verify.rs"]  mod gpg_verify;
#[path = "system/grub.rs"]        mod grub;
#[path = "system/immutable.rs"]   mod immutable;
#[path = "system/livepatch.rs"]   mod livepatch;
#[path = "system/postinst.rs"]    mod postinst;
#[path = "system/profile.rs"]     mod profile;
#[path = "system/service.rs"]     mod service;

// ── tools/ ────────────────────────────────────────────────────
#[path = "tools/hk_tools.rs"]     mod hk_tools;
#[path = "tools/selfupdate.rs"]   mod selfupdate;
#[path = "tools/setup.rs"]        mod setup;

// ── internal/ ─────────────────────────────────────────────────
#[path = "internal/diff.rs"]      mod diff;
#[path = "internal/livecheck.rs"] mod livecheck;
#[path = "internal/lock.rs"]      mod lock;
#[path = "internal/log.rs"]       mod log;

// ── ui/ ───────────────────────────────────────────────────────
#[path = "ui/download.rs"]        mod download;
#[path = "ui/ui.rs"]              mod ui;

// ── cli/ ──────────────────────────────────────────────────────
// Files live in source-code/cli/ subfolder.
// We declare them here with explicit #[path] so Rust finds them.
#[path = "cli/types.rs"]          mod cli_types;
#[path = "cli/pkg.rs"]            mod cli_pkg;
#[path = "cli/sys.rs"]            mod cli_sys;
#[path = "cli/cli.rs"]            mod cli;

// ── top-level ─────────────────────────────────────────────────
mod repo;
mod userenv;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    crate::log::session_start();
    if let Err(e) = cli::run(args).await {
        crate::ui::fatal(&format!("{:#}", e));
        std::process::exit(1);
    }
}
