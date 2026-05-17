mod cache;
mod cli;
mod db;
mod deb;
mod diff;
mod download;
mod gpg;
mod grub;
mod livepatch;
mod log;
mod package;
mod profile;
mod repo;
mod solver;
mod store;
mod transaction;
mod ui;
mod userenv;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Err(e) = cli::run(args).await {
        eprintln!("  Error: {}", e);
        std::process::exit(1);
    }
}
