// ── pkg/ ──────────────────────────────────────────────────────
#[path = "pkg/cache.rs"]       mod cache;
#[path = "pkg/db.rs"]          mod db;
#[path = "pkg/deb.rs"]         mod deb;
#[path = "pkg/package.rs"]     mod package;
#[path = "pkg/solver.rs"]      mod solver;
#[path = "pkg/store.rs"]       mod store;
#[path = "pkg/transaction.rs"] mod transaction;

// ── system/ ───────────────────────────────────────────────────
#[path = "system/gpg.rs"]      mod gpg;
#[path = "system/grub.rs"]     mod grub;
#[path = "system/livepatch.rs"]mod livepatch;
#[path = "system/profile.rs"]  mod profile;

// ── tools/ ────────────────────────────────────────────────────
#[path = "tools/hk_tools.rs"]  mod hk_tools;
#[path = "tools/selfupdate.rs"]mod selfupdate;
#[path = "tools/setup.rs"]     mod setup;

// ── internal/ ─────────────────────────────────────────────────
#[path = "internal/diff.rs"]      mod diff;
#[path = "internal/livecheck.rs"] mod livecheck;
#[path = "internal/log.rs"]       mod log;

// ── ui/ ───────────────────────────────────────────────────────
#[path = "ui/download.rs"] mod download;
#[path = "ui/ui.rs"]       mod ui;

// ── top-level ─────────────────────────────────────────────────
mod cli;
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
