//! Native terminal, tool execution, and agent run bridge.
pub mod auth;
mod error;
pub mod exec;
pub mod host_admin;
pub mod mcp;
pub mod paths;
pub mod plugins;
pub mod runs;
pub mod scheduled;
pub mod secret;
pub mod server;
pub mod terminals;
pub use error::BridgeError;
pub use server::{ServerConfig, check_request, run_server, run_server_until};
pub use terminals::session::SessionManager;
// Preserve the library's module paths for existing hosts.
pub use exec::{git, proc};
pub use runs::{agent_host, backend_client, http_client, native_vfs};
pub use server::http;
pub use terminals::{headless, pty, seq_ring, session};
