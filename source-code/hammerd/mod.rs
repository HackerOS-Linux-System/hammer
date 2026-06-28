pub mod actions;
pub mod client;
pub mod config;
pub mod ipc;
pub mod notify;
pub mod scheduler;
pub mod server;
pub mod service;
pub mod state;

// Re-export the most commonly used items at crate level
pub use config::DaemonConfig;
pub use ipc::{DaemonRequest, DaemonResponse};
pub use server::run_daemon;
pub use client::send_request;
pub use service::install_hammerd_service;
