//! Native agent transport for shared language tools. Database authority and the
//! normal language endpoint remain authoritative; no privileged server route.
use super::{agent_host::BridgeCancel, backend_client::RunBackend};
use crate::plugins::transport::NativePluginTransport;
use openwebide_agent::{
    intelligence::AgentLanguageHost,
    plugins::{
        actions::PluginActionHost,
        execution::{GrantedHost, PluginTransport},
    },
};
use openwebide_core::plugins::{
    PreparedPlugin,
    execution::{ContinuePlugin, InvokePlugin, PluginHostRequest, PluginInvocation},
    languages::{
        intelligence::{Call, Response},
        tools::LanguageToolContext,
    },
};
use std::sync::Arc;

pub(super) struct LanguageHost<B> {
    pub backend: Arc<B>,
    pub transport: NativePluginTransport,
    pub user: i64,
    pub session: i64,
    pub cancel: BridgeCancel,
}
impl<B> Clone for LanguageHost<B> {
    fn clone(&self) -> Self {
        Self {
            backend: self.backend.clone(),
            transport: self.transport.clone(),
            user: self.user,
            session: self.session,
            cancel: self.cancel.clone(),
        }
    }
}
impl<B: RunBackend> PluginTransport for LanguageHost<B> {
    async fn ensure_prepared(&self, plugin: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        self.transport.ensure_prepared(plugin).await
    }
    async fn start(&self, call: InvokePlugin) -> Result<PluginInvocation, String> {
        self.transport.start(call).await
    }
    async fn resume(&self, call: ContinuePlugin) -> Result<PluginInvocation, String> {
        self.transport.resume(call).await
    }
    fn cancel(&self, id: String) {
        self.transport.cancel(id);
    }
    async fn flush_cancellations(&self) {
        self.transport.flush_cancellations().await;
    }
}
impl<B: RunBackend> GrantedHost for LanguageHost<B> {
    async fn request(&self, request: &PluginHostRequest) -> Result<String, String> {
        self.backend
            .plugin_request(self.user, self.session, request)
            .await
    }
}
impl<B: RunBackend> PluginActionHost for LanguageHost<B> {
    async fn prepare(&self, plugin: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        self.ensure_prepared(plugin).await
    }
    async fn grant(&self, plugin: &PreparedPlugin) -> Result<String, String> {
        self.backend
            .language_grant(self.user, self.session, plugin)
            .await
    }
    async fn language_server(&self, call: &Call) -> Result<Response, String> {
        let mut call = call.clone();
        call.session_id = Some(self.session);
        self.backend.language_query(self.user, &call).await
    }
    fn is_current(&self) -> bool {
        !self.cancel.is_cancelled()
    }
}
impl<B: RunBackend> AgentLanguageHost for LanguageHost<B> {
    async fn language_context(&self) -> Result<LanguageToolContext, String> {
        self.backend.language_context(self.user, self.session).await
    }
}

#[cfg(test)]
mod tests {
    use super::super::{backend_client::BackendClient, http_client::ReqwestHttpClient};
    use super::*;
    use openwebide_core::{
        WorkspaceMode,
        plugins::languages::{
            LanguagePreferences,
            intelligence::{self as protocol, Query},
            source_digest,
            tools::LanguageToolScope,
        },
    };
    use serde_json::{Value, json};

    #[tokio::test]
    async fn native_language_adapter_uses_session_authority_and_preserves_transport_failures() {
        let prepared = openwebide_core::plugins::testing::receipt();
        let context = LanguageToolContext {
            scope: LanguageToolScope {
                project: 9,
                mode: WorkspaceMode::Remote,
                root: Some("project".into()),
            },
            bindings: vec![],
            preferences: LanguagePreferences::default(),
        };
        let call = Call {
            session_id: Some(999),
            grant: "authority".into(),
            prepared: prepared.clone(),
            input: protocol::Request {
                version: protocol::VERSION,
                client: "agent-client".into(),
                document: "document".into(),
                request: 1,
                revision: 1,
                language: "custom".into(),
                path: "file.custom".into(),
                source: "text".into(),
                source_digest: source_digest("text"),
                query: Query::Symbols,
            },
        };
        let response = Response {
            version: protocol::VERSION,
            client: "agent-client".into(),
            document: "document".into(),
            request: 1,
            revision: 1,
            source_digest: source_digest("text"),
            server: "service".into(),
            context: None,
            capabilities: json!({}),
            result: json!([]),
            diagnostics: vec![],
            diagnostics_ready: false,
        };
        for operation in ["context", "grant", "query"] {
            for status in [200, 401] {
                let body = match operation {
                    "context" => json!(context),
                    "grant" => json!({(prepared.digest.clone()):"authority"}),
                    _ => json!(response),
                }
                .to_string();
                let wire = format!(
                    "HTTP/1.1 {status} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let (url, captured) =
                    super::super::http_client::tests::capture(Box::leak(wire.into_boxed_str()))
                        .await;
                let adapter = LanguageHost {
                    backend: Arc::new(BackendClient::new(
                        format!("{url}/api"),
                        "shared-secret".into(),
                        ReqwestHttpClient::default(),
                    )),
                    transport: crate::plugins::transport::PluginExecutionHost::default()
                        .transport(42),
                    user: 42,
                    session: 7,
                    cancel: BridgeCancel::default(),
                };
                let result = match operation {
                    "context" => adapter.language_context().await.map(|_| ()),
                    "grant" => adapter.grant(&prepared).await.map(|_| ()),
                    _ => adapter.language_server(&call).await.map(|_| ()),
                };
                assert_eq!(result.is_ok(), status == 200);
                let request = captured.await.unwrap();
                let lower = request.to_ascii_lowercase();
                assert!(lower.contains("authorization: bearer shared-secret\r\n"));
                assert!(lower.contains("x-openwebide-user: 42\r\n"));
                match operation {
                    "context" => assert!(
                        request.starts_with("GET /api/sessions/7/language-context HTTP/1.1")
                    ),
                    "grant" => {
                        assert!(request.starts_with("POST /api/sessions/7/plugin-grants HTTP/1.1"));
                        assert!(request.contains(&prepared.digest));
                    }
                    _ => {
                        assert!(request.starts_with("POST /api/plugins/languages/query HTTP/1.1"));
                        let payload: Value =
                            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1)
                                .unwrap();
                        assert_eq!(payload["session_id"], 7);
                        assert_eq!(payload["input"]["path"], "file.custom");
                    }
                }
                assert!(adapter.is_current());
                adapter.cancel.cancel();
                assert!(!adapter.is_current());
            }
        }
    }
}
