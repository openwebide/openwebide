//! Browser-driven agent loop for local-mode workspaces.
//!
//! When working on a folder picked via the Chromium File System Access API,
//! the agent loop executes directly in the browser against [`BrowserFsaVfs`],
//! requests LLM tool calls from the backend via `/api/chat-tools`, and persists
//! messages and tool steps to the backend store for full session parity.

use leptos::prelude::{GetUntracked, WithValue};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures::{Stream, StreamExt};
use openwebide_agent::{AgentConfig, BridgeClient, CancelCheck, PermissionGate, VfsToolExecutor};
use openwebide_core::{
    ChatCompletion, ChatMessage, ChatRequest, ChatResponse, CommandOutcome, EditorContext,
    ModelInfo, ProviderKind, Role, RunEvent, ToolCall, TurnTelemetry,
};
use openwebide_llm::{LlmProvider, ProviderError, StreamChunk, ToolStreamChunk, completion_chunks};
use send_wrapper::SendWrapper;

use crate::backend::Api;
use crate::local_fs::BrowserFsaVfs;

use crate::util::sleep_ms;

/// An [`LlmProvider`] adapter that delegates completions to the backend's `/api/chat-tools`.
pub struct BrowserLlmProvider {
    // Local fields and futures stay on the browser spawn_local thread.
    // SendWrapper checks access/poll/drop without changing native provider contracts.
    api: SendWrapper<Api>,
    kind: ProviderKind,
    bridge: SendWrapper<Option<crate::bridge::BridgeConn>>,
}

impl BrowserLlmProvider {
    pub fn new(api: Api, kind: ProviderKind, bridge: Option<crate::bridge::BridgeConn>) -> Self {
        Self {
            api: SendWrapper::new(api),
            kind,
            bridge: SendWrapper::new(bridge),
        }
    }
}

impl LlmProvider for BrowserLlmProvider {
    fn kind(&self) -> ProviderKind {
        self.kind
    }

    fn list_models(&self) -> impl Future<Output = Result<Vec<ModelInfo>, ProviderError>> + Send {
        SendWrapper::new(async move {
            Err(ProviderError::NotImplemented(
                "list_models not supported in browser provider".into(),
            ))
        })
    }

    fn chat(
        &self,
        request: &ChatRequest,
    ) -> impl Future<Output = Result<String, ProviderError>> + Send {
        let api = *self.api;
        let request = request.clone();
        SendWrapper::new(async move {
            match api.with_value(Clone::clone).chat_tools(&request).await {
                Ok(completion) => match completion.response {
                    ChatResponse::Text(s) => Ok(s),
                    ChatResponse::ToolCalls(_) => Err(ProviderError::Parse("expected text".into())),
                },
                Err(e) => Err(ProviderError::Http(e)),
            }
        })
    }

    fn chat_stream(
        &self,
        request: &ChatRequest,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamChunk, ProviderError>> + Send + 'static>> {
        Box::pin(
            self.chat_tools_stream(request)
                .filter_map(|chunk| async move {
                    match chunk {
                        Ok(ToolStreamChunk::Delta(text)) => Some(Ok(StreamChunk::Delta(text))),
                        Ok(ToolStreamChunk::Reasoning(text)) => {
                            Some(Ok(StreamChunk::Reasoning(text)))
                        }
                        Ok(ToolStreamChunk::Stop(reason)) => Some(Ok(StreamChunk::Stop(reason))),
                        Ok(ToolStreamChunk::Usage(usage)) => Some(Ok(StreamChunk::Usage(usage))),
                        Ok(ToolStreamChunk::Response(ChatResponse::Text(_))) => None,
                        Ok(ToolStreamChunk::Response(ChatResponse::ToolCalls(_))) => {
                            Some(Err(ProviderError::Parse("expected text".into())))
                        }
                        Err(error) => Some(Err(error)),
                    }
                }),
        )
    }

    fn chat_tools(
        &self,
        request: &ChatRequest,
    ) -> impl Future<Output = Result<ChatCompletion, ProviderError>> + Send {
        let api = *self.api;
        let request = request.clone();
        SendWrapper::new(async move {
            api.with_value(Clone::clone)
                .chat_tools(&request)
                .await
                .map_err(ProviderError::Http)
        })
    }

    fn chat_tools_stream(
        &self,
        request: &ChatRequest,
    ) -> Pin<Box<dyn Stream<Item = Result<ToolStreamChunk, ProviderError>> + Send + 'static>> {
        if let Some(bridge) = &*self.bridge
            && bridge.status().get_untracked()
                == (crate::bridge::BridgeStatus::Ready { runs: true })
        {
            let id = format!("completion-{}", js_sys::Math::random());
            let receiver = bridge.register_completion(id.clone());
            let guard = CompletionGuard {
                bridge: bridge.clone(),
                id: id.clone(),
            };
            let error = bridge
                .send(openwebide_core::BridgeClientMessage::CompletionStart {
                    id,
                    request: request.clone(),
                })
                .err();
            // Guard the whole stream so cancellation in CompletionGuard::drop
            // is also restricted to the thread that created the bridge receiver.
            return Box::pin(SendWrapper::new(BrowserCompletionStream {
                receiver,
                _guard: guard,
                ended: false,
                error,
            }));
        }
        let api = *self.api;
        let request = request.clone();
        Box::pin(
            futures::stream::once(SendWrapper::new(async move {
                let chunks = match api.with_value(Clone::clone).chat_tools(&request).await {
                    Ok(c) => completion_chunks(c).into_iter().map(Ok).collect::<Vec<_>>(),
                    Err(e) => vec![Err(ProviderError::Http(e))],
                };
                futures::stream::iter(chunks)
            }))
            .flatten(),
        )
    }

    fn context_limit(
        &self,
        _model: Option<&str>,
    ) -> impl Future<Output = Result<Option<usize>, ProviderError>> + Send {
        SendWrapper::new(async move { Ok(None) })
    }
}

struct CompletionGuard {
    bridge: crate::bridge::BridgeConn,
    id: String,
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        let _ = self
            .bridge
            .send(openwebide_core::BridgeClientMessage::CompletionCancel {
                id: self.id.clone(),
            });
        self.bridge.unregister_completion(&self.id);
    }
}

struct BrowserCompletionStream {
    receiver: futures::channel::mpsc::UnboundedReceiver<openwebide_core::BridgeServerMessage>,
    _guard: CompletionGuard,
    ended: bool,
    error: Option<String>,
}

impl Stream for BrowserCompletionStream {
    type Item = Result<ToolStreamChunk, ProviderError>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use openwebide_core::BridgeServerMessage;
        use std::task::Poll;
        if self.ended {
            return Poll::Ready(None);
        }
        if let Some(error) = self.error.take() {
            self.ended = true;
            return Poll::Ready(Some(Err(ProviderError::Http(error))));
        }
        match Pin::new(&mut self.receiver).poll_next(cx) {
            Poll::Ready(Some(BridgeServerMessage::CompletionChunk { chunk, .. })) => {
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(BridgeServerMessage::CompletionEnd { error, .. })) => {
                self.ended = true;
                Poll::Ready(error.map(|error| Err(ProviderError::Http(error))))
            }
            Poll::Ready(None) => {
                self.ended = true;
                Poll::Ready(Some(Err(ProviderError::Http("bridge disconnected".into()))))
            }
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(_)) => unreachable!("only completion frames reach the receiver"),
        }
    }
}

/// Browser bridge client sending command execution requests to the local bridge daemon.
#[derive(Clone)]
pub struct BrowserBridgeClient {
    http_url: String,
    credentials: crate::bridge::BridgeCredentials,
    cwd: String,
    verified: Arc<AtomicBool>,
}

impl BrowserBridgeClient {
    pub fn for_project(
        http_url: String,
        cwd: String,
        credentials: crate::bridge::BridgeCredentials,
    ) -> Self {
        Self {
            http_url,
            credentials,
            cwd,
            verified: Arc::new(AtomicBool::new(true)),
        }
    }

    pub fn cwd_verified(&self) -> bool {
        self.verified.load(Ordering::Relaxed)
    }

    /// One authenticated transport adapter for agent tools and the workspace Git UI.
    pub async fn git_request<T: serde::de::DeserializeOwned>(
        &self,
        operation: &str,
        mut payload: serde_json::Value,
    ) -> Result<T, String> {
        if !self.cwd_verified() {
            return Err("Bridge cwd is no longer verified; rediscover the folder.".into());
        }
        payload["cwd"] = serde_json::json!(self.git_cwd());
        let token = self.credentials.credential().await?;
        let guard = crate::api::CommandFetchGuard(
            web_sys::AbortController::new().map_err(|error| format!("{error:?}"))?,
        );
        let response = gloo_net::http::Request::post(&format!("{}/git/{operation}", self.http_url))
            .abort_signal(Some(&guard.0.signal()))
            .header("Content-Type", "application/json")
            .header("Authorization", &format!("Bearer {token}"))
            .body(payload.to_string())
            .map_err(|error| error.to_string())?
            .send()
            .await
            .map_err(|error| format!("Bridge connection error: {error}"))?;
        if !response.ok() {
            let body = response.text().await.unwrap_or_default();
            if response.status() == 400 && is_cwd_resolution_error(&body) {
                self.verified.store(false, Ordering::Relaxed);
            }
            if response.status() == 415 {
                return Err("binary file".into());
            }
            return Err(format!("Bridge error (HTTP {}): {body}", response.status()));
        }
        response
            .json()
            .await
            .map_err(|error| format!("Parse error: {error}"))
    }

    pub fn cwd(&self) -> &str {
        &self.cwd
    }
    fn git_cwd(&self) -> &str {
        if self.cwd.is_empty() { "." } else { &self.cwd }
    }
}

impl BridgeClient for BrowserBridgeClient {
    fn context_status(
        &self,
    ) -> impl Future<Output = openwebide_agent::clients::ContextStatus> + Send {
        SendWrapper::new(async move {
            let work = Box::pin(futures::future::join(self.environment(), self.git_status()));
            let deadline = Box::pin(crate::util::sleep_ms(2000));
            match futures::future::select(work, deadline).await {
                futures::future::Either::Left((status, _)) => status,
                futures::future::Either::Right(_) => {
                    openwebide_agent::clients::context_status_unavailable()
                }
            }
        })
    }

    fn environment(
        &self,
    ) -> impl Future<Output = Result<openwebide_core::ExecutionEnvironment, String>> + Send {
        let endpoint = format!("{}/environment", self.http_url);
        let credentials = self.credentials.clone();
        SendWrapper::new(async move {
            let token = credentials
                .credential()
                .await
                .map_err(|error| error.to_string())?;
            let guard = crate::api::CommandFetchGuard(
                web_sys::AbortController::new().map_err(|error| format!("{error:?}"))?,
            );
            let response = gloo_net::http::Request::post(&endpoint)
                .header("Content-Type", "application/json")
                .header("Authorization", &format!("Bearer {token}"))
                .abort_signal(Some(&guard.0.signal()))
                .body("{}")
                .map_err(|error| error.to_string())?
                .send()
                .await
                .map_err(|error| error.to_string())?;
            if !response.ok() {
                return Err(format!("Bridge HTTP {}", response.status()));
            }
            response.json().await.map_err(|error| error.to_string())
        })
    }

    fn host_info(&self) -> impl Future<Output = Result<openwebide_core::HostInfo, String>> + Send {
        let endpoint = format!("{}/host/info", self.http_url);
        let credentials = self.credentials.clone();
        SendWrapper::new(async move {
            let token = credentials
                .credential()
                .await
                .map_err(|error| error.to_string())?;
            let guard = crate::api::CommandFetchGuard(
                web_sys::AbortController::new().map_err(|error| format!("{error:?}"))?,
            );
            let response = gloo_net::http::Request::post(&endpoint)
                .header("Content-Type", "application/json")
                .header("Authorization", &format!("Bearer {token}"))
                .abort_signal(Some(&guard.0.signal()))
                .body("{}")
                .map_err(|error| error.to_string())?
                .send()
                .await
                .map_err(|error| error.to_string())?;
            if !response.ok() {
                return Err(format!("Bridge HTTP {}", response.status()));
            }
            response.json().await.map_err(|error| error.to_string())
        })
    }

    fn execute_command(
        &self,
        command: &str,
        timeout_seconds: u64,
    ) -> impl Future<Output = Result<CommandOutcome, String>> + Send {
        let endpoint = format!("{}/exec", self.http_url);
        let credentials = self.credentials.clone();
        let verified = self.verified.clone();
        let payload = serde_json::json!({
            "command": command,
            "cwd": self.cwd,
            "timeout_seconds": timeout_seconds,
        })
        .to_string();

        SendWrapper::new(async move {
            if !verified.load(Ordering::Relaxed) {
                return Err(
                    "Bridge cwd is no longer verified; send again to rediscover the folder.".into(),
                );
            }
            let token = credentials
                .credential()
                .await
                .map_err(|e| format!("auth error: {e}"))?;
            let guard = crate::api::CommandFetchGuard(
                web_sys::AbortController::new()
                    .map_err(|e| format!("request cancellation error: {e:?}"))?,
            );
            let resp = gloo_net::http::Request::post(&endpoint)
                .abort_signal(Some(&guard.0.signal()))
                .header("Content-Type", "application/json")
                .header("Authorization", &format!("Bearer {token}"))
                .body(payload)
                .map_err(|e| format!("request error: {e}"))?
                .send()
                .await
                .map_err(|e| {
                    format!("Failed to connect to bridge daemon at {endpoint}: {e}. Ensure 'openwebide-bridge' is running.")
                })?;

            if !resp.ok() {
                let err_text = resp.text().await.unwrap_or_default();
                if resp.status() == 400 && is_cwd_resolution_error(&err_text) {
                    verified.store(false, Ordering::Relaxed);
                }
                return Err(format!("Bridge error (HTTP {}): {err_text}", resp.status()));
            }

            resp.json::<CommandOutcome>()
                .await
                .map_err(|e| format!("Failed to parse bridge outcome JSON: {e}"))
        })
    }

    fn git_status(
        &self,
    ) -> impl Future<Output = Result<openwebide_core::GitRepoStatus, String>> + Send {
        SendWrapper::new(self.git_request("status", serde_json::json!({})))
    }

    fn git_diff(&self, path: Option<&str>) -> impl Future<Output = Result<String, String>> + Send {
        let payload = serde_json::json!({ "path": path });
        SendWrapper::new(async move {
            let response: openwebide_core::GitDiff = self.git_request("diff", payload).await?;
            Ok(response.diff)
        })
    }

    fn git_commit(
        &self,
        req: &openwebide_core::GitCommitRequest,
    ) -> impl Future<Output = Result<openwebide_core::GitCommitResult, String>> + Send {
        SendWrapper::new(self.git_request("commit", serde_json::to_value(req).unwrap_or_default()))
    }

    fn git_checkout(
        &self,
        req: &openwebide_core::GitCheckoutRequest,
    ) -> impl Future<Output = Result<openwebide_core::GitCheckoutResult, String>> + Send {
        SendWrapper::new(
            self.git_request("checkout", serde_json::to_value(req).unwrap_or_default()),
        )
    }
}

fn is_cwd_resolution_error(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.as_str())
                .map(str::to_owned)
        })
        .is_some_and(|error| {
            error.starts_with("cwd does not exist:")
                || error.starts_with("cwd escapes workspace root:")
                || error == "cwd is missing or empty"
        })
}

async fn probe_command<B: BridgeClient>(
    bridge: B,
    command: &str,
    timeout_seconds: u64,
) -> Result<CommandOutcome, String> {
    let request = bridge.execute_command(command, timeout_seconds);
    let deadline = sleep_ms(
        i32::try_from(timeout_seconds.saturating_mul(1000).saturating_add(1000))
            .unwrap_or(i32::MAX),
    );
    futures::pin_mut!(request, deadline);
    match futures::future::select(request, deadline).await {
        futures::future::Either::Left((result, _)) => result,
        futures::future::Either::Right(_) => Err("Bridge probe timed out".into()),
    }
}

pub const BRIDGE_FOLDER_NOTICE: &str = "Command and git tools are off for this local project: the bridge can't see this folder. Start `openwebide-bridge --workspace <a folder containing it>` (or inside it) and send again.";

pub async fn resolve_bridge_cwd(
    api: Api,
    handle: web_sys::FileSystemDirectoryHandle,
    pid: i64,
    bridge_cfg: &crate::bridge::BridgeConfig,
    credentials: &crate::bridge::BridgeCredentials,
) -> Option<String> {
    resolve_bridge_cwd_guarded(api, handle, pid, bridge_cfg, credentials, || true).await
}

pub async fn resolve_bridge_cwd_guarded(
    api: Api,
    handle: web_sys::FileSystemDirectoryHandle,
    pid: i64,
    bridge_cfg: &crate::bridge::BridgeConfig,
    credentials: &crate::bridge::BridgeCredentials,
    current: impl Fn() -> bool,
) -> Option<String> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    // Math.random is in [0, 1), scaled values fit u32; fractional bits are discarded.
    let nonce = format!(
        "{:08x}{:08x}",
        (js_sys::Math::random() * 4294967296.0) as u32,
        (js_sys::Math::random() * 4294967296.0) as u32
    );
    resolve_bridge_cwd_with_guard(
        api,
        &BrowserFsaVfs::new(handle),
        pid,
        &nonce,
        |cwd| {
            BrowserBridgeClient::for_project(bridge_cfg.http_url.clone(), cwd, credentials.clone())
        },
        current,
    )
    .await
}

pub async fn resolve_bridge_cwd_with<V: openwebide_core::Vfs, B: BridgeClient>(
    api: Api,
    vfs: &V,
    pid: i64,
    nonce: &str,
    bridge: impl Fn(String) -> B,
) -> Option<String> {
    resolve_bridge_cwd_with_guard(api, vfs, pid, nonce, bridge, || true).await
}

async fn resolve_bridge_cwd_with_guard<V: openwebide_core::Vfs, B: BridgeClient>(
    api: Api,
    vfs: &V,
    pid: i64,
    nonce: &str,
    bridge: impl Fn(String) -> B,
    current: impl Fn() -> bool,
) -> Option<String> {
    if !current() {
        return None;
    }
    // The pane may be disposed while awaiting a probe; marker cleanup still has to finish.
    let backend = api.with_value(Clone::clone);
    let probe = format!(".openwebide-probe-{nonce}");
    let written = vfs.write(&probe, "").await;
    let result = if written.is_ok() && current() {
        let key = format!("local_bridge_cwd.{pid}");
        let candidate = backend
            .get_settings()
            .await
            .ok()
            .and_then(|settings| settings.get(&key).cloned());
        let mut found = None;
        if let Some(candidate) = candidate
            && current()
            && probe_command(bridge(candidate.clone()), &format!("test -f {probe}"), 5)
                .await
                .is_ok_and(|out| out.is_success())
        {
            found = Some(candidate);
        }
        if found.is_none() && current() {
            let command = format!(
                r"find . -maxdepth 5 \( -name .git -o -name node_modules -o -name target -o -name .spin \) -prune -o -name {probe} -print -quit"
            );
            if let Ok(out) = probe_command(bridge(String::new()), &command, 10).await {
                found = crate::parse_probe_output(&out.stdout, nonce);
                if let Some(cwd) = &found
                    && current()
                {
                    let _ = backend.set_setting(&key, cwd).await;
                }
            }
        }
        found
    } else {
        None
    };
    // A failed write may still have created the file before its stream failed.
    if vfs.delete(&probe).await.is_err() {
        return None;
    }
    result
}

pub fn local_tools(cwd: Option<&str>) -> Vec<openwebide_core::ToolDefinition> {
    openwebide_agent::session::tools_for_host(cwd.is_some())
}

/// Local-mode cancel checker observing a shared atomic flag.
#[derive(Clone)]
pub struct LocalCancelCheck {
    pub flag: Arc<AtomicBool>,
}

impl CancelCheck for LocalCancelCheck {
    fn cancelled(&self) -> impl Future<Output = ()> + Send {
        let flag = self.flag.clone();
        SendWrapper::new(async move {
            while !flag.load(Ordering::Relaxed) {
                crate::util::sleep_ms(100).await;
            }
        })
    }

    fn check(&self) -> impl Future<Output = bool> + Send {
        let flag = self.flag.clone();
        SendWrapper::new(async move { flag.load(Ordering::Relaxed) })
    }
}

/// Local-mode permission gate requiring approval for file modifications.
#[derive(Clone)]
pub struct LocalPermissionGate {
    pub decisions: Arc<Mutex<HashMap<String, bool>>>,
    pub cancel: Arc<AtomicBool>,
}

impl PermissionGate for LocalPermissionGate {
    fn approve(&self, call: &ToolCall) -> impl Future<Output = bool> + Send {
        let decisions = self.decisions.clone();
        let cancel = self.cancel.clone();
        let id = call.id.clone();
        SendWrapper::new(async move {
            for _ in 0..3000 {
                if cancel.load(Ordering::Relaxed) {
                    return false;
                }
                if let Some(decision) = decisions.lock().unwrap().remove(&id) {
                    return decision;
                }
                sleep_ms(100).await;
            }
            false
        })
    }
}

struct ApprovalAdapter {
    api: SendWrapper<Api>,
    session: i64,
    connection_id: i64,
    model: Option<String>,
}
impl openwebide_agent::policy::ApprovalSource for ApprovalAdapter {
    fn check(&self, call: &ToolCall) -> impl Future<Output = bool> + Send {
        let api = *self.api;
        let session = self.session;
        let check = openwebide_core::ApprovalCheck {
            connection_id: self.connection_id,
            model: self.model.clone(),
            call: call.clone(),
        };
        SendWrapper::new(async move {
            api.with_value(Clone::clone)
                .approval_check(session, &check)
                .await
                .is_ok_and(|decision| decision.approved)
        })
    }
}

/// Model I/O primitives for the shared compaction workflow.
#[derive(Clone)]
pub struct BrowserModelSource {
    pub api: SendWrapper<Api>,
}
impl openwebide_agent::compaction::CompactionSource for BrowserModelSource {
    fn available(&self) -> bool {
        true
    }
    fn runtime(
        &self,
        selection: &openwebide_core::ModelSelection,
    ) -> impl Future<Output = Result<openwebide_core::ModelRuntime, String>> + Send {
        SendWrapper::new(async move {
            self.api
                .with_value(Clone::clone)
                .model_runtime(selection.server_id, Some(&selection.model))
                .await
        })
    }
    fn complete(
        &self,
        request: &ChatRequest,
    ) -> impl Future<Output = Result<ChatCompletion, String>> + Send {
        SendWrapper::new(async move {
            self.api
                .with_value(Clone::clone)
                .model_complete(request)
                .await
        })
    }
    fn complete_with_timeout(
        &self,
        request: &ChatRequest,
        timeout_seconds: u32,
    ) -> impl Future<Output = Result<ChatCompletion, String>> + Send {
        SendWrapper::new(async move {
            let api = self.api.with_value(Clone::clone);
            let completion = api.model_complete_with_timeout(request, timeout_seconds);
            let timeout_ms =
                i32::try_from(timeout_seconds.saturating_mul(1000)).unwrap_or(i32::MAX);
            let deadline = crate::util::sleep_ms(timeout_ms);
            futures::pin_mut!(completion, deadline);
            match futures::future::select(completion, deadline).await {
                futures::future::Either::Left((result, _)) => result,
                futures::future::Either::Right(_) => {
                    Err("Background model deadline exceeded".into())
                }
            }
        })
    }
    fn context_limit(&self, request: &ChatRequest) -> impl Future<Output = Option<usize>> + Send {
        SendWrapper::new(async move {
            self.api
                .with_value(Clone::clone)
                .model_context(request.connection_id, request.model.as_deref())
                .await
                .ok()
                .flatten()
        })
    }
    fn tokens(&self, request: &ChatRequest) -> impl Future<Output = Option<usize>> + Send {
        SendWrapper::new(async move {
            self.api
                .with_value(Clone::clone)
                .model_tokens(request)
                .await
                .ok()
                .flatten()
        })
    }
}

/// Run the agent loop locally in the browser against a local folder handle.
#[allow(
    clippy::too_many_arguments,
    reason = "Execution host entry point receives the existing UI run callbacks and signals"
)]
pub async fn run_local_agent(
    api: Api,
    session_id: i64,
    user_content: String,
    model: Option<String>,
    editor_context: Option<EditorContext>,
    browser_preferences: Option<openwebide_core::BrowserPreferences>,
    connection_id: i64,
    system_prompt: Option<String>,
    handle: web_sys::FileSystemDirectoryHandle,
    project: openwebide_core::Project,
    chat: crate::state::chat::ChatState,
    cancel_flag: Arc<AtomicBool>,
    local_decisions: Arc<Mutex<HashMap<String, bool>>>,
    mut on_event: impl FnMut(RunEvent),
    project_host: crate::project_host::ProjectHost,
    bridge_connection: Option<crate::bridge::BridgeConn>,
    resume: Option<crate::state::chat::InterruptedRun>,
    queued_prompt: Option<openwebide_core::QueuedPromptKey>,
    current: impl Fn() -> bool + Clone + 'static,
) -> Result<(), String> {
    let host = project_host
        .resolve_guarded(Some(project.id), true, current.clone())
        .await
        .ok();
    if !current() {
        return Err("Project access changed".into());
    }
    let cwd = host
        .as_ref()
        .and_then(crate::project_host::ProjectExecution::cwd);
    if cwd.is_none() {
        chat.notify_bridge_folder_once(session_id);
    }
    let environment = openwebide_core::RunEnvironment {
        browser_preferences,
        project_name: Some(project.name),
        project_root: Some(
            cwd.clone()
                .unwrap_or_else(|| format!("Browser-selected folder: {}", handle.name())),
        ),
        mode: Some(openwebide_core::WorkspaceMode::Local),
        timestamp: openwebide_core::now_seconds(js_sys::Date::now()),
    };
    let vfs = BrowserFsaVfs::new(handle);
    // 1. Fetch prior conversation history before persisting the new message
    let history_entries = api
        .with_value(Clone::clone)
        .list_messages(session_id)
        .await?;
    let resume = if let Some(resume) = resume {
        let current = crate::state::chat::interrupted_run(
            &crate::state_actions::chat::history_items(history_entries.clone()),
        )
        .filter(|current| current.anchor_id == resume.anchor_id)
        .ok_or_else(|| "conversation changed; reload the session before resuming".to_string())?;
        Some(current)
    } else {
        None
    };
    let runtime = api
        .with_value(Clone::clone)
        .model_runtime(connection_id, model.as_deref())
        .await?;
    let memories = api
        .with_value(Clone::clone)
        .session_memories(session_id)
        .await?;
    let mut input = openwebide_agent::session::PlanInput {
        environment: environment.clone(),
        system_prompt,
        messages: openwebide_agent::session::conversation_history(history_entries),
        tools: local_tools(cwd.as_deref()),
        content: user_content,
        editor: editor_context,
    };
    let skills = api
        .with_value(Clone::clone)
        .session_skills(session_id)
        .await?;
    let plugin_bindings = api
        .with_value(Clone::clone)
        .project_plugins(project.id)
        .await?;
    openwebide_agent::plugins::configure(
        &mut input.tools,
        &mut input.system_prompt,
        &openwebide_agent::plugins::PluginContext {
            bindings: &plugin_bindings,
            memories: &memories,
            skills: &skills,
            context_limit: runtime.settings.context_limit,
        },
    );
    let plugin_context = openwebide_core::plugins::execution::PluginExecutionContext {
        user_action: false,
        project_id: Some(project.id),
        session_id: Some(session_id),
        primary: runtime
            .connection
            .model
            .as_ref()
            .filter(|model| !model.trim().is_empty())
            .map(|model| openwebide_core::ModelSelection {
                server_id: runtime.connection.id,
                model: model.clone(),
            }),
    };
    let planning_api = api.with_value(Clone::clone);
    let plugin_bridge = project_host
        .plugin_host(Some(project.id), current.clone())?
        .local_bridge();
    let plugin_transport = BrowserPluginTransport(plugin_bridge.clone());
    let plan = openwebide_agent::session::plan_with_plugin_context(
        &runtime,
        input,
        &plugin_bindings,
        &plugin_transport,
        BrowserPluginServices {
            api: SendWrapper::new(api),
            session: session_id,
        },
        |plugins| async move {
            planning_api
                .plugin_context_grants(&openwebide_core::plugins::execution::PluginGrantRequest {
                    context: plugin_context,
                    plugins,
                })
                .await
        },
    )
    .await?;
    let mut plan = plan;
    plan.plugin_skills = openwebide_agent::skills::package_snapshot(&skills);
    if !current() {
        return Err("Project access changed".into());
    }
    plan.validate_prompt()?;
    let lease = format!(
        "browser-{session_id}-{}-{}",
        js_sys::Date::now(),
        js_sys::Math::random()
    );
    let backend = api.with_value(Clone::clone);
    backend.run_lease(session_id, &lease, false).await?;
    let (abort, registration) = futures::future::AbortHandle::new_pair();
    let renewal_backend = backend.clone();
    let renewal_lease = lease.clone();
    let renewal_cancel = cancel_flag.clone();
    leptos::task::spawn_local(async move {
        let _ = futures::future::Abortable::new(
            async move {
                loop {
                    crate::util::sleep_ms(15000).await;
                    if renewal_backend
                        .run_lease(session_id, &renewal_lease, false)
                        .await
                        .is_err()
                    {
                        renewal_cancel.store(true, Ordering::SeqCst);
                        break;
                    }
                }
            },
            registration,
        )
        .await;
    });
    let result = async {
        let mut request = plan.request;
        let (anchor_id, first_turn) = if let Some(resume) = resume {
            (resume.anchor_id, resume.first_turn)
        } else {
            let user_message = if let Some(key) = queued_prompt {
                api.with_value(Clone::clone)
                    .consume_queued_prompt(session_id, key, &plan.user_content)
                    .await?
            } else {
                api.with_value(Clone::clone)
                    .persist_message(session_id, Role::User, &plan.user_content, None, None)
                    .await?
            };
            let anchor_id = user_message.id;
            on_event(RunEvent::Message {
                message: user_message.clone(),
            });
            request.messages.push(user_message);
            (anchor_id, 1)
        };

        let provider = BrowserLlmProvider::new(api, runtime.connection.kind, bridge_connection);
        let cancel = LocalCancelCheck {
            flag: cancel_flag.clone(),
        };
        if matches!(plan.kind, openwebide_core::RunKind::Chat) {
            let content = openwebide_agent::session::chat_context(&mut request, &environment);
            let message = api
                .with_value(Clone::clone)
                .persist_message(session_id, Role::System, &content, None, None)
                .await?;
            if !current() {
                return Err("Project access changed".into());
            }
            on_event(RunEvent::Message { message });
            let prepared = openwebide_agent::session::compact_request(
                &provider,
                &BrowserModelSource {
                    api: SendWrapper::new(api),
                },
                &mut request,
                &cancel,
                SessionPersistence {
                    anchor: anchor_id,
                    api: SendWrapper::new(api),
                    session: session_id,
                },
                session_id,
                anchor_id,
            )
            .await;
            for event in prepared.events {
                on_event(event);
            }
            if prepared.terminal {
                return Ok(());
            }
            let mut events = Box::pin(openwebide_agent::session::chat_events(
                SessionPersistence {
                    anchor: anchor_id,
                    api: SendWrapper::new(api),
                    session: session_id,
                },
                provider.chat_stream(&request),
                cancel,
                &request,
            ));
            while let Some(event) = events.next().await {
                on_event(event);
            }
            return Ok(());
        }
        let config = AgentConfig {
            first_turn,
            ..AgentConfig::default()
        };
        let bridge = host
            .as_ref()
            .and_then(crate::project_host::ProjectExecution::local_bridge);
        let factory = BrowserTaskFactory {
            plugin_skills: Arc::new(plan.plugin_skills),
            plugin_executables: Arc::new(plan.plugin_executables),
            plugin_grants: Arc::new(plan.plugin_grants),
            api: SendWrapper::new(api),
            vfs,
            bridge,
            plugin_bridge,
            environment,
            session: session_id,
            anchor: anchor_id,
            manual: LocalPermissionGate {
                decisions: local_decisions,
                cancel: cancel_flag,
            },
            connection: provider.bridge.clone(),
        };
        let executor = factory.executor();
        let gate = factory.gate(&request);
        let stream = openwebide_agent::tasks::host::run_tree(
            factory,
            BrowserModelSource {
                api: SendWrapper::new(api),
            },
            cancel,
            provider,
            executor,
            gate,
            request,
            config,
            anchor_id,
        );

        let mut stream = Box::pin(openwebide_agent::session::events(
            SessionPersistence {
                anchor: anchor_id,
                api: SendWrapper::new(api),
                session: session_id,
            },
            session_id,
            anchor_id,
            stream,
        ));
        while let Some(event) = stream.next().await {
            on_event(event);
        }
        Ok(())
    }
    .await;
    abort.abort();
    let _ = backend.run_lease(session_id, &lease, true).await;
    result
}

struct SessionPersistence {
    anchor: i64,
    api: SendWrapper<Api>,
    session: i64,
}
impl openwebide_agent::session::RunPersistence for SessionPersistence {
    fn finish(&self) -> impl Future<Output = ()> + Send {
        SendWrapper::new(async move {
            let _ = self
                .api
                .with_value(Clone::clone)
                .question_command(
                    self.session,
                    &openwebide_core::questions::QuestionCommand::CancelRun {
                        anchor: self.anchor,
                    },
                )
                .await;
        })
    }

    fn task(
        &self,
        anchor: i64,
        snapshot: &openwebide_core::TaskSnapshot,
    ) -> impl Future<Output = Result<(), String>> + Send {
        SendWrapper::new(async move {
            self.api
                .with_value(Clone::clone)
                .save_task(self.session, anchor, snapshot)
                .await
        })
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "Browser epoch milliseconds are positive and fit in u64"
    )]
    fn now_ms(&self) -> u64 {
        js_sys::Date::now() as u64
    }
    fn timing(
        &self,
        id: &str,
        timing: &openwebide_core::ToolTiming,
    ) -> impl Future<Output = Result<(), String>> + Send {
        SendWrapper::new(async move {
            self.api
                .with_value(Clone::clone)
                .save_tool_timing(self.session, id, timing)
                .await
        })
    }

    fn now(&self) -> i64 {
        openwebide_core::now_seconds(js_sys::Date::now())
    }
    fn message(
        &self,
        role: Role,
        content: &str,
        usage: Option<&TurnTelemetry>,
        calls: Option<&[ToolCall]>,
    ) -> impl Future<Output = Result<ChatMessage, String>> + Send {
        SendWrapper::new(async move {
            self.api
                .with_value(Clone::clone)
                .persist_message(self.session, role, content, usage, calls)
                .await
        })
    }
    fn step(
        &self,
        anchor: i64,
        id: &str,
        name: &str,
        summary: &str,
        diff: Option<&openwebide_core::FileDiff>,
    ) -> impl Future<Output = Result<(), String>> + Send {
        SendWrapper::new(async move {
            self.api
                .with_value(Clone::clone)
                .upsert_tool_step(self.session, anchor, id, name, summary, diff)
                .await
        })
    }
    fn checkpoint(
        &self,
        id: &str,
        checkpoint: &openwebide_core::rewind::ProjectCheckpoint,
    ) -> impl Future<Output = Result<(), String>> + Send {
        SendWrapper::new(async move {
            self.api
                .with_value(Clone::clone)
                .save_project_checkpoint(self.session, id, checkpoint)
                .await
        })
    }
    fn result(
        &self,
        id: &str,
        ok: bool,
        summary: &str,
        diff: Option<&openwebide_core::FileDiff>,
    ) -> impl Future<Output = Result<(), String>> + Send {
        SendWrapper::new(async move {
            self.api
                .with_value(Clone::clone)
                .complete_tool_step(self.session, id, ok, summary, diff)
                .await
        })
    }
}

struct TodoPersistence {
    api: SendWrapper<Api>,
    session: i64,
    anchor: i64,
}
impl openwebide_agent::todo::TodoStore for TodoPersistence {
    fn read(
        &self,
    ) -> impl Future<Output = Result<Option<openwebide_core::TodoUpdate>, String>> + Send {
        SendWrapper::new(async move {
            self.api
                .with_value(Clone::clone)
                .get_todo_plan(self.session)
                .await
        })
    }
    fn write(
        &self,
        plan: &openwebide_core::TodoPlan,
    ) -> impl Future<Output = Result<openwebide_core::TodoUpdate, String>> + Send {
        SendWrapper::new(async move {
            self.api
                .with_value(Clone::clone)
                .write_todo_plan(self.session, self.anchor, plan)
                .await
        })
    }
}

type BrowserBuiltinTaskExecutor = openwebide_agent::todo::TodoTools<
    VfsToolExecutor<BrowserFsaVfs, openwebide_agent::NoopWebClient, Option<BrowserBridgeClient>>,
    TodoPersistence,
>;
type BrowserBaseTaskExecutor = openwebide_agent::plugins::execution::PluginTools<
    openwebide_agent::skills::packages::PackageSkillTools<BrowserBuiltinTaskExecutor>,
    BrowserPluginTransport,
    openwebide_agent::plugins::execution::GrantedServices<BrowserPluginServices>,
>;
#[derive(Clone)]
struct BrowserPluginTransport(Option<crate::plugin_bridge::PluginBridgeClient>);
impl openwebide_agent::plugins::execution::PluginTransport for BrowserPluginTransport {
    async fn ensure_prepared(
        &self,
        expected: &openwebide_core::plugins::PreparedPlugin,
    ) -> Result<openwebide_core::plugins::PreparedPlugin, String> {
        SendWrapper::new(async move {
            let client = self
                .0
                .as_ref()
                .ok_or("Connect this project's execution host first.")?;
            crate::project_plugins::prepare_on_host(
                crate::project_plugins::PluginTransport::Local(client.clone()),
                &expected.source,
                || true,
                || false,
                |_| {},
            )
            .await
        })
        .await
    }

    async fn start(
        &self,
        call: openwebide_core::plugins::execution::InvokePlugin,
    ) -> Result<openwebide_core::plugins::execution::PluginInvocation, String> {
        SendWrapper::new(async move {
            self.0
                .as_ref()
                .ok_or("Connect this project's execution host first.")?
                .plugin_request("invoke", serde_json::json!({"call":call}))
                .await
        })
        .await
    }
    async fn resume(
        &self,
        continuation: openwebide_core::plugins::execution::ContinuePlugin,
    ) -> Result<openwebide_core::plugins::execution::PluginInvocation, String> {
        SendWrapper::new(async move {
            self.0
                .as_ref()
                .ok_or("Connect this project's execution host first.")?
                .plugin_request("continue", serde_json::json!({"continuation":continuation}))
                .await
        })
        .await
    }
    fn cancel(&self, id: String) {
        if let Some(client) = self.0.clone() {
            wasm_bindgen_futures::spawn_local(async move {
                let _ = client
                    .plugin_request::<serde_json::Value>("cancel", serde_json::json!({"id":id}))
                    .await;
            });
        }
    }
}
struct BrowserPluginServices {
    api: SendWrapper<Api>,
    session: i64,
}
impl openwebide_agent::plugins::execution::GrantedHost for BrowserPluginServices {
    async fn request(
        &self,
        request: &openwebide_core::plugins::execution::PluginHostRequest,
    ) -> Result<String, String> {
        SendWrapper::new(async move {
            self.api
                .with_value(Clone::clone)
                .plugin_host_request(self.session, request)
                .await
        })
        .await
    }
}
type BrowserTaskGate = openwebide_agent::plugins::execution::PluginGate<
    openwebide_agent::policy::PolicyGate<LocalPermissionGate, ApprovalAdapter>,
>;
#[derive(Clone)]
struct BrowserTaskFactory {
    plugin_grants: Arc<std::collections::BTreeMap<String, String>>,
    plugin_executables: Arc<Vec<openwebide_core::plugins::PreparedPlugin>>,
    plugin_skills: Arc<Vec<openwebide_core::ProjectSkill>>,
    api: SendWrapper<Api>,
    vfs: BrowserFsaVfs,
    bridge: Option<BrowserBridgeClient>,
    plugin_bridge: Option<crate::plugin_bridge::PluginBridgeClient>,
    environment: openwebide_core::RunEnvironment,
    session: i64,
    anchor: i64,
    manual: LocalPermissionGate,
    connection: SendWrapper<Option<crate::bridge::BridgeConn>>,
}
impl BrowserTaskFactory {
    fn executor(&self) -> BrowserTaskExecutor {
        openwebide_agent::questions::QuestionTools::new(
            self.base_executor(),
            BrowserQuestionPersistence {
                api: self.api.clone(),
                session: self.session,
            },
            self.anchor,
        )
    }
    fn base_executor(&self) -> BrowserBaseTaskExecutor {
        openwebide_agent::plugins::execution::PluginTools {
            executor: openwebide_agent::skills::packages::PackageSkillTools::new(
                self.builtin_executor(),
                &self.plugin_skills,
            ),
            transport: BrowserPluginTransport(self.plugin_bridge.clone()),
            services: openwebide_agent::plugins::execution::GrantedServices {
                grants: self.plugin_grants.clone(),
                host: BrowserPluginServices {
                    api: self.api.clone(),
                    session: self.session,
                },
            },
            plugins: self.plugin_executables.clone(),
        }
    }
    fn builtin_executor(&self) -> BrowserBuiltinTaskExecutor {
        openwebide_agent::todo::TodoTools::new(
            VfsToolExecutor::with_web_and_bridge(
                self.vfs.clone(),
                openwebide_agent::NoopWebClient,
                self.bridge.clone(),
            )
            .with_context(self.environment.clone()),
            TodoPersistence {
                api: self.api.clone(),
                session: self.session,
                anchor: self.anchor,
            },
        )
    }
    fn gate(&self, request: &ChatRequest) -> BrowserTaskGate {
        openwebide_agent::plugins::execution::PluginGate {
            plugins: self.plugin_executables.clone(),
            gate: openwebide_agent::policy::PolicyGate {
                manual: self.manual.clone(),
                source: ApprovalAdapter {
                    api: self.api.clone(),
                    session: self.session,
                    connection_id: request.connection_id,
                    model: request.model.clone(),
                },
            },
        }
    }
}
impl openwebide_agent::tasks::host::TaskFactory for BrowserTaskFactory {
    type Provider = BrowserLlmProvider;
    type Executor = BrowserTaskExecutor;
    type Gate = BrowserTaskGate;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "Browser epoch milliseconds are positive and fit in u64"
    )]
    fn now_ms(&self) -> u64 {
        js_sys::Date::now() as u64
    }
    fn prepare(
        &self,
        request: &ChatRequest,
    ) -> impl Future<Output = Result<openwebide_agent::tasks::host::TaskPrimitives<Self>, String>> + Send
    {
        SendWrapper::new(async move {
            let runtime = self
                .api
                .with_value(Clone::clone)
                .model_runtime(request.connection_id, request.model.as_deref())
                .await?;
            let provider = BrowserLlmProvider::new(
                *self.api,
                runtime.connection.kind,
                self.connection.clone().take(),
            );
            Ok((provider, self.executor(), self.gate(request)))
        })
    }
}

struct BrowserQuestionPersistence {
    api: SendWrapper<Api>,
    session: i64,
}
impl openwebide_agent::questions::QuestionStore for BrowserQuestionPersistence {
    fn command(
        &self,
        command: &openwebide_core::questions::QuestionCommand,
    ) -> impl Future<Output = Result<openwebide_core::questions::QuestionResult, String>> + Send
    {
        SendWrapper::new(async move {
            self.api
                .with_value(Clone::clone)
                .question_command(self.session, command)
                .await
        })
    }
    fn wait(&self) -> impl Future<Output = ()> + Send {
        SendWrapper::new(async {
            sleep_ms(500).await;
        })
    }
}

type BrowserTaskExecutor =
    openwebide_agent::questions::QuestionTools<BrowserBaseTaskExecutor, BrowserQuestionPersistence>;
