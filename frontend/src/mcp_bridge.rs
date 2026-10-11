//! Browser bridge I/O for the shared MCP run facade; servers execute only on hosts.
use crate::plugin_bridge::PluginBridgeClient;
use openwebide_core::{ToolCall, mcp::Catalog};
use send_wrapper::SendWrapper;
use std::sync::Arc;
#[derive(Clone)]
pub struct McpBridge {
    client: Option<PluginBridgeClient>,
    _lease: Arc<Lease>,
}
struct Lease {
    client: Option<PluginBridgeClient>,
    id: String,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(client) = self.client.clone()
            && !self.id.is_empty()
        {
            let id = self.id.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let _ = client
                    .host_request::<serde_json::Value>("/mcp/close", serde_json::json!({"id":id}))
                    .await;
            });
        }
    }
}
impl McpBridge {
    pub async fn open(client: Option<PluginBridgeClient>, cwd: Option<String>) -> (Self, Catalog) {
        let catalog = if let Some(client) = &client {
            client
                .host_request("/mcp/open", serde_json::json!({"cwd":cwd}))
                .await
                .unwrap_or_else(|error| Catalog {
                    failures: vec![error],
                    ..Default::default()
                })
        } else {
            Catalog::default()
        };
        let lease = Arc::new(Lease {
            client: client.clone(),
            id: catalog.id.clone(),
        });
        (
            Self {
                client,
                _lease: lease,
            },
            catalog,
        )
    }
}
impl openwebide_agent::mcp::Transport for McpBridge {
    async fn invoke(
        &self,
        catalog: &Catalog,
        call: &ToolCall,
    ) -> Result<openwebide_agent::ToolOutcome, String> {
        SendWrapper::new(async move {
            #[derive(serde::Deserialize)]
            struct ResultBody {
                ok: bool,
                content: String,
                summary: String,
            }
            let response: ResultBody = self
                .client
                .as_ref()
                .ok_or("Connect this project's execution host first")?
                .host_request(
                    "/mcp/call",
                    serde_json::json!({"catalog":catalog,"call":call}),
                )
                .await?;
            Ok(openwebide_agent::ToolOutcome {
                ok: response.ok,
                content: response.content,
                summary: response.summary,
                diff: None,
            })
        })
        .await
    }
}
