//! Shared MCP tool discovery and invocation policy. Hosts supply only JSON-RPC I/O.
use openwebide_core::{ToolCall, ToolDefinition};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
};

pub const MAX_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_TOOLS: usize = 256;
pub const MAX_SERVERS: usize = 32;

/// Host-owned executable configuration; never supplied by model tool arguments.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub disabled: bool,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default, rename = "mcpServers")]
    pub servers: BTreeMap<String, Server>,
}
impl Config {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 256 * 1024 {
            return Err("MCP configuration exceeds 256 KiB".into());
        }
        let config: Self = serde_json::from_slice(bytes)
            .map_err(|error| format!("Invalid MCP configuration: {error}"))?;
        if config.servers.len() > MAX_SERVERS {
            return Err("Too many MCP servers".into());
        }
        for (name, server) in &config.servers {
            if name.is_empty()
                || name.len() > 32
                || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
            {
                return Err("MCP server names must contain 1–32 letters, digits or hyphens".into());
            }
            if server.command.trim().is_empty()
                || server.command.len() > 4096
                || server.args.len() > 128
                || server.env.len() > 128
                || server.args.iter().any(|arg| arg.len() > 8192)
                || server
                    .env
                    .iter()
                    .any(|(key, value)| key.is_empty() || key.len() > 256 || value.len() > 8192)
            {
                return Err(format!("Invalid MCP server configuration: {name}"));
            }
        }
        Ok(config)
    }
    pub fn merge(mut self, project: Self) -> Result<Self, String> {
        self.servers.extend(project.servers);
        Self::parse(&serde_json::to_vec(&self).map_err(|error| error.to_string())?)
    }
}

pub trait Rpc: Send {
    /// Runtime-bounded compatibility probe; None means an older handshake server.
    fn probe(&mut self) -> impl Future<Output = Result<Option<Value>, String>> + Send {
        std::future::ready(Ok(None))
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
    ) -> impl Future<Output = Result<Value, String>> + Send;
    fn notify(
        &mut self,
        method: &str,
        params: Value,
    ) -> impl Future<Output = Result<(), String>> + Send;
}
pub use openwebide_core::mcp::{Catalog, Tool};

/// Legacy stdio servers retain their initialize/initialized handshake.
pub async fn initialize(rpc: &mut impl Rpc) -> Result<(), String> {
    if let Some(result) = rpc.probe().await? {
        if !result
            .get("supportedVersions")
            .and_then(Value::as_array)
            .is_some_and(|versions| {
                versions
                    .iter()
                    .any(|version| version.as_str() == Some("2026-07-28"))
            })
        {
            return Err("MCP server has no supported modern protocol version".into());
        }
        if !result
            .get("capabilities")
            .and_then(|value| value.get("tools"))
            .is_some_and(Value::is_object)
        {
            return Err("MCP server does not advertise tools".into());
        }
        return Ok(());
    }
    let result = rpc.request("initialize", json!({"protocolVersion":"2025-11-25", "capabilities":{}, "clientInfo":{"name":"Open WebIDE", "version":env!("CARGO_PKG_VERSION")}})).await?;
    let version = result
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or("MCP initialization omitted its protocol version")?;
    if !["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"].contains(&version) {
        return Err(format!("Unsupported MCP protocol version: {version}"));
    }
    if !result
        .get("capabilities")
        .and_then(|value| value.get("tools"))
        .is_some_and(Value::is_object)
    {
        return Err("MCP server does not advertise tools".into());
    }
    rpc.notify("notifications/initialized", json!({})).await
}

pub async fn discover(rpc: &mut impl Rpc, server: &str) -> Result<Vec<Tool>, String> {
    // Validate namespace before incorporating untrusted tool names.
    Config::parse(
        &serde_json::to_vec(&json!({"mcpServers":{server:{"command":"validated"}}}))
            .map_err(|error| error.to_string())?,
    )?;
    let mut tools = Vec::new();
    let mut bytes = 0;
    let mut names = BTreeSet::new();
    let mut cursors = BTreeSet::new();
    let mut cursor: Option<String> = None;
    for _ in 0..32 {
        let result = rpc
            .request(
                "tools/list",
                cursor
                    .as_ref()
                    .map_or_else(|| json!({}), |cursor| json!({"cursor":cursor})),
            )
            .await?;
        let page = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or("Invalid MCP tool list")?;
        for tool in page {
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .ok_or("MCP tool has no name")?;
            let exposed = format!("mcp__{server}__{name}");
            if name.is_empty()
                || exposed.len() > 128
                || !name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-.".contains(&c))
                || !names.insert(name.to_owned())
            {
                return Err("Invalid or duplicate MCP tool name".into());
            }
            let schema = tool
                .get("inputSchema")
                .filter(|value| {
                    value.is_object() && value.get("type").and_then(Value::as_str) == Some("object")
                })
                .ok_or("MCP tool requires an object input schema")?;
            let description = tool
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or(name);
            if description.len() > 16 * 1024 {
                return Err("MCP tool description exceeds its limit".into());
            }
            bytes += description.len() + exposed.len() + schema.to_string().len();
            if bytes > MAX_MESSAGE_BYTES {
                return Err("MCP tool schemas exceed their combined size limit".into());
            }
            tools.push(Tool {
                server: server.into(),
                name: name.into(),
                definition: ToolDefinition {
                    name: exposed,
                    description: description.into(),
                    parameters: schema.clone(),
                },
            });
            if tools.len() > MAX_TOOLS {
                return Err("MCP tool count exceeds its limit".into());
            }
        }
        cursor = match result.get("nextCursor") {
            None | Some(Value::Null) => return Ok(tools),
            Some(Value::String(cursor)) if !cursor.is_empty() && cursor.len() <= 4096 => {
                Some(cursor.clone())
            }
            _ => return Err("Invalid MCP pagination cursor".into()),
        };
        if !cursors.insert(cursor.clone()) {
            return Err("MCP pagination repeated a cursor".into());
        }
    }
    Err("MCP pagination exceeds its limit".into())
}

pub async fn call(
    rpc: &mut impl Rpc,
    tool: &Tool,
    call: &ToolCall,
) -> Result<crate::ToolOutcome, String> {
    if call.name != tool.definition.name || call.arguments.len() > 256 * 1024 {
        return Err("Invalid MCP tool invocation".into());
    }
    let arguments: Value =
        serde_json::from_str(&call.arguments).map_err(|error| error.to_string())?;
    if !arguments.is_object() {
        return Err("MCP tool arguments must be an object".into());
    }
    let result = rpc
        .request(
            "tools/call",
            json!({"name":tool.name, "arguments":arguments}),
        )
        .await?;
    if result
        .get("resultType")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind != "complete")
    {
        return Err("MCP server requested an unsupported client interaction".into());
    }
    if !result.get("content").is_some_and(Value::is_array) {
        return Err("Invalid MCP tool result".into());
    }
    let content = serde_json::to_string(&result).map_err(|error| error.to_string())?;
    if content.len() > MAX_MESSAGE_BYTES {
        return Err("MCP tool result exceeds its limit".into());
    }
    let ok = !result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(crate::ToolOutcome {
        ok,
        content,
        summary: format!("{}: {}", call.name, if ok { "completed" } else { "failed" }),
        diff: None,
    })
}

/// Native adapters supply spawn and deadline primitives, without discovery policy.
pub trait ProcessHost: Send + Sync {
    type Connection: Rpc;
    fn spawn(
        &self,
        server: &Server,
    ) -> impl Future<Output = Result<Self::Connection, String>> + Send;
    fn bounded<F: Future + Send>(
        &self,
        future: F,
        seconds: u64,
    ) -> impl Future<Output = Result<F::Output, String>> + Send;
}
pub struct PreparedServers<C> {
    pub catalog: Catalog,
    pub connections: Vec<(String, C, Vec<Tool>)>,
}
/// Shared orchestration: partial availability, deadlines, immutable discovery and limits.
pub async fn prepare_servers<H: ProcessHost>(
    host: &H,
    config: Config,
) -> PreparedServers<H::Connection> {
    let mut catalog = Catalog::default();
    let mut connections = Vec::new();
    let mut total_bytes = 0;
    for (name, config) in config
        .servers
        .into_iter()
        .filter(|(_, config)| !config.disabled)
    {
        let prepared = host
            .bounded(
                async {
                    let mut connection = host.spawn(&config).await?;
                    initialize(&mut connection).await?;
                    let tools = discover(&mut connection, &name).await?;
                    let bytes = serde_json::to_vec(&tools)
                        .map_err(|error| error.to_string())?
                        .len();
                    if catalog.tools.len() + tools.len() > MAX_TOOLS
                        || total_bytes + bytes > MAX_MESSAGE_BYTES
                    {
                        return Err("MCP catalog exceeds its limit".into());
                    }
                    Ok::<_, String>((connection, tools, bytes))
                },
                15,
            )
            .await;
        match prepared {
            Ok(Ok((connection, tools, bytes))) => {
                total_bytes += bytes;
                catalog.tools.extend(tools.clone());
                connections.push((name, connection, tools));
            }
            Ok(Err(error)) | Err(error) => {
                catalog.failures.push(format!(
                    "{name}: {}",
                    error.chars().take(512).collect::<String>()
                ));
            }
        }
    }
    PreparedServers {
        catalog,
        connections,
    }
}

pub trait Transport: Clone + Send + Sync {
    fn invoke(
        &self,
        catalog: &Catalog,
        call: &ToolCall,
    ) -> impl Future<Output = Result<crate::ToolOutcome, String>> + Send;
}

/// Shared run facade; both agent adapters wrap their existing executor here.
pub struct McpTools<E, T> {
    pub executor: E,
    pub transport: T,
    pub catalog: std::sync::Arc<Catalog>,
}
impl<E, T> McpTools<E, T> {
    fn tool(&self, name: &str) -> Option<&Tool> {
        self.catalog
            .tools
            .iter()
            .find(|tool| tool.definition.name == name)
    }
}
impl<E: crate::ToolExecutor + Sync, T: Transport> crate::ToolExecutor for McpTools<E, T> {
    fn reserve_tool_call(&self) -> Result<(), String> {
        self.executor.reserve_tool_call()
    }
    fn has_context(&self) -> bool {
        self.executor.has_context()
    }
    async fn context(
        &mut self,
        tools: &[ToolDefinition],
        call: Option<&ToolCall>,
    ) -> Option<String> {
        self.executor.context(tools, call).await
    }
    fn describe(&self, call: &ToolCall) -> String {
        self.tool(&call.name).map_or_else(
            || self.executor.describe(call),
            |tool| format!("MCP {}: {}", tool.server, tool.name),
        )
    }
    async fn preview(&self, call: &ToolCall) -> Option<crate::ToolPreview> {
        if let Some(tool) = self.tool(&call.name) {
            return Some(crate::ToolPreview {
                diff: None,
                note: Some(format!("MCP server: {}\n{}", tool.server, call.arguments)),
            });
        }
        self.executor.preview(call).await
    }
    async fn checkpoint(
        &self,
        call: &ToolCall,
    ) -> Result<Option<openwebide_core::FileDiff>, String> {
        if self.tool(&call.name).is_some() {
            Ok(None)
        } else {
            self.executor.checkpoint(call).await
        }
    }
    async fn project_checkpoint(
        &self,
        call: &ToolCall,
    ) -> Result<Option<openwebide_core::rewind::ProjectSnapshot>, String> {
        if self.tool(&call.name).is_some() {
            Ok(None)
        } else {
            self.executor.project_checkpoint(call).await
        }
    }
    async fn execute(&self, call: &ToolCall) -> crate::ToolOutcome {
        if self.tool(&call.name).is_none() {
            return self.executor.execute(call).await;
        }
        self.transport
            .invoke(&self.catalog, call)
            .await
            .unwrap_or_else(|error| crate::ToolOutcome {
                ok: false,
                content: error,
                summary: format!("{} failed", call.name),
                diff: None,
            })
    }
    fn acknowledge(&self, id: &str) {
        self.executor.acknowledge(id);
    }
    fn set_context_limit(&self, limit: Option<usize>) {
        self.executor.set_context_limit(limit);
    }
}

/// Add only validated, collision-free definitions; failure notices remain visible to the model.
pub fn configure(
    tools: &mut Vec<ToolDefinition>,
    prompt: &mut Option<String>,
    catalog: &Catalog,
) -> Result<(), String> {
    if catalog.tools.len() > MAX_TOOLS || catalog.failures.len() > MAX_SERVERS {
        return Err("MCP catalog exceeds its limit".into());
    }
    let mut names: BTreeSet<_> = tools.iter().map(|tool| tool.name.clone()).collect();
    for tool in &catalog.tools {
        if tool.definition.name != format!("mcp__{}__{}", tool.server, tool.name)
            || !names.insert(tool.definition.name.clone())
        {
            return Err("MCP tool name conflicts with this run".into());
        }
    }
    tools.extend(catalog.tools.iter().map(|tool| tool.definition.clone()));
    if !catalog.failures.is_empty() {
        prompt.get_or_insert_with(String::new).push_str(&format!(
            "\n\nUnavailable MCP servers (host diagnostics):\n{}",
            catalog.failures.join("\n")
        ));
    }
    Ok(())
}

pub fn attach(plan: &mut openwebide_core::RunPlan, catalog: Catalog) -> Result<(), String> {
    configure(
        &mut plan.request.tools,
        &mut plan.request.system_prompt,
        &catalog,
    )?;
    plan.connection
        .tool_selection
        .apply(&mut plan.request.tools);
    if plan.request.model_settings.tools == Some(false) {
        plan.request.tools.clear();
    }
    plan.mcp = catalog;
    if !plan.request.tools.is_empty() && plan.kind == openwebide_core::RunKind::Chat {
        plan.kind = plan.environment.project_root.as_ref().map_or(
            openwebide_core::RunKind::WebChat,
            |root| openwebide_core::RunKind::Agent {
                project_path: root.clone(),
            },
        );
    }
    plan.validate_prompt()
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        replies: std::collections::VecDeque<Value>,
        sent: Vec<(String, Value)>,
    }
    impl Rpc for Fixture {
        async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
            self.sent.push((method.into(), params));
            self.replies
                .pop_front()
                .ok_or_else(|| "No fixture response".into())
        }
        async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
            self.sent.push((method.into(), params));
            Ok(())
        }
    }
    #[test]
    fn configuration_overrides_and_disabled_servers_are_validated() {
        let host = Config::parse(br#"{"mcpServers":{"docs":{"command":"host"}}}"#).unwrap();
        let project =
            Config::parse(br#"{"mcpServers":{"docs":{"command":"project","disabled":true}}}"#)
                .unwrap();
        let merged = host.merge(project).unwrap();
        assert!(merged.servers["docs"].disabled);
        assert_eq!(merged.servers["docs"].command, "project");
        assert!(Config::parse(br#"{"mcpServers":{"bad__name":{"command":"host"}}}"#).is_err());
        assert!(
            Config::parse(
                br#"{"mcpServers":{"docs":{"command":"host","url":"https://example.com"}}}"#
            )
            .is_err()
        );
    }
    #[test]
    fn handshake_pagination_and_error_results_keep_wire_identity_and_approvals() {
        futures::executor::block_on(async {
            let mut fixture = Fixture {sent:Vec::new(), replies:[json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}}}), json!({"tools":[{"name":"search","inputSchema":{"type":"object"}}],"nextCursor":"page2"}), json!({"tools":[{"name":"write","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}}]}), json!({"content":[{"type":"text","text":"denied"}],"isError":true})].into()};
            initialize(&mut fixture).await.unwrap();
            let tools = discover(&mut fixture, "docs").await.unwrap();
            assert_eq!(fixture.sent[1].0, "notifications/initialized");
            assert_eq!(fixture.sent[3].1, json!({"cursor":"page2"}));
            let call = ToolCall {
                id: "1".into(),
                name: tools[1].definition.name.clone(),
                arguments: "{\"text\":\"value\"}".into(),
            };
            assert!(crate::policy::requires_approval(&call));
            assert!(
                !super::call(&mut fixture, &tools[1], &call)
                    .await
                    .unwrap()
                    .ok
            );
            assert_eq!(
                fixture.sent.last().unwrap().1,
                json!({"name":"write","arguments":{"text":"value"}})
            );
        });
    }
    #[test]
    fn broken_schema_and_pagination_do_not_publish_partial_tools() {
        futures::executor::block_on(async {
            for replies in [
                vec![json!({"tools":[{"name":"search","inputSchema":{"type":"array"}}]})],
                vec![
                    json!({"tools":[],"nextCursor":"loop"}),
                    json!({"tools":[],"nextCursor":"loop"}),
                ],
                vec![
                    json!({"tools":[{"name":"search","inputSchema":{"type":"object"}},{"name":"search","inputSchema":{"type":"object"}}]}),
                ],
            ] {
                let mut fixture = Fixture {
                    sent: Vec::new(),
                    replies: replies.into(),
                };
                assert!(discover(&mut fixture, "docs").await.is_err());
            }
        });
    }
}
