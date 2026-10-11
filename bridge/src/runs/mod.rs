pub mod agent_host;
pub mod backend_client;
mod completions;
pub mod http_client;
pub mod native_vfs;
#[path = "runs.rs"]
mod registry;
pub(crate) use completions::complete;
#[cfg(test)]
pub(crate) use registry::assert_deferred_mcp_run;
pub use registry::*;
