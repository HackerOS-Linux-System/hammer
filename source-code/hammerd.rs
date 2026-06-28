#[path = "hammerd/config.rs"]    pub mod config;
#[path = "hammerd/ipc.rs"]       pub mod ipc;
#[path = "hammerd/state.rs"]     pub mod state;
#[path = "hammerd/actions.rs"]   pub mod actions;
#[path = "hammerd/notify.rs"]    pub mod notify;
#[path = "hammerd/scheduler.rs"] pub mod scheduler;
#[path = "hammerd/server.rs"]    pub mod server;
#[path = "hammerd/client.rs"]    pub mod client;
#[path = "hammerd/service.rs"]   pub mod service;

// Re-exports for backwards compatibility (old code used hammerd::daemon::*).
// The binary itself may not use all of these — external crates / tests do.
#[allow(unused_imports)]
pub use config::{DaemonConfig, CONFIG_FILE};
#[allow(unused_imports)]
pub use ipc::{DaemonRequest, DaemonResponse};
#[allow(unused_imports)]
pub use server::{run_daemon, SOCKET_PATH};
#[allow(unused_imports)]
pub use service::{write_pid_file, PID_FILE};
#[allow(unused_imports)]
pub use client::send_request;
