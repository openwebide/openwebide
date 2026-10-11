pub mod fake_backend;

pub mod fake_transport;

#[cfg(target_arch = "wasm32")]
pub use crate::mcp_bridge::McpBridge;
