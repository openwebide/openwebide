//! Run-pinned host MCP discovery results. Executable configuration stays on hosts.
use crate::ToolDefinition;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    pub definition: ToolDefinition,
    pub server: String,
    pub name: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    pub id: String,
    pub tools: Vec<Tool>,
    pub failures: Vec<String>,
}
