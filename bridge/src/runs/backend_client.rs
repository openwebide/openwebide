use std::future::Future;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Request;
use openwebide_core::{
    ChatMessage, Connection, EditorContext, FileDiff, Role, RunPlan, TurnTelemetry, WebSearchResult,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::runs::http_client::ReqwestHttpClient;

pub trait RunBackend: Send + Sync {
    fn plugin_request(
        &self,
        _user: i64,
        _session: i64,
        _request: &openwebide_core::plugins::execution::PluginHostRequest,
    ) -> impl Future<Output = Result<String, String>> + Send {
        async { Err("Plugin host capability unavailable".into()) }
    }
    fn host_journal(
        &self,
        _command: &openwebide_core::host_admin::HostJournalCommand,
    ) -> impl Future<Output = Result<openwebide_core::host_admin::HostJournalResult, String>> + Send
    {
        async { Err("Host operation journal unavailable".into()) }
    }
    fn scheduled_command(
        &self,
        _user: i64,
        _session: i64,
        _command: &openwebide_core::scheduled::TaskCommand,
    ) -> impl Future<Output = Result<Vec<openwebide_core::scheduled::ScheduledTask>, String>> + Send
    {
        async { Err("Scheduled tasks unavailable".into()) }
    }
    fn run_lease(
        &self,
        _user: i64,
        _session: i64,
        _token: &str,
        _release: bool,
        _since: i64,
        _permission: Option<&str>,
    ) -> impl Future<Output = Result<openwebide_core::scheduled::RunControl, String>> + Send {
        async { Ok(Default::default()) }
    }
    fn notify(
        &self,
        _user: i64,
        _session: i64,
        _event: &openwebide_core::push::RunNotification,
    ) -> impl Future<Output = Result<(), String>> + Send {
        async { Ok(()) }
    }

    fn memory_command(
        &self,
        _user: i64,
        _session: i64,
        _command: &openwebide_core::MemoryCommand,
    ) -> impl Future<Output = Result<openwebide_core::ProjectMemories, String>> + Send {
        async { Err("Project memory unavailable".into()) }
    }
    fn question_command(
        &self,
        _user: i64,
        _session: i64,
        _command: &openwebide_core::questions::QuestionCommand,
    ) -> impl Future<Output = Result<openwebide_core::questions::QuestionResult, String>> + Send
    {
        async { Err("Questions unavailable".into()) }
    }
    fn skill_command(
        &self,
        _user: i64,
        _session: i64,
        _command: &openwebide_core::SkillCommand,
    ) -> impl Future<Output = Result<openwebide_core::ProjectSkills, String>> + Send {
        async { Err("Project skills unavailable".into()) }
    }
    fn get_todo_plan(
        &self,
        _user: i64,
        _session: i64,
    ) -> impl Future<Output = Result<Option<openwebide_core::TodoUpdate>, String>> + Send {
        async { Ok(None) }
    }
    fn write_todo_plan(
        &self,
        _user: i64,
        _session: i64,
        _anchor: i64,
        _plan: &openwebide_core::TodoPlan,
    ) -> impl Future<Output = Result<openwebide_core::TodoUpdate, String>> + Send {
        async { Err("Session plan persistence unavailable".into()) }
    }

    fn consume_queued_prompt(
        &self,
        _user: i64,
        _session: i64,
        _key: openwebide_core::QueuedPromptKey,
        _content: &str,
    ) -> impl Future<Output = Result<ChatMessage, String>> + Send {
        async { Err("Queued prompt persistence unavailable".into()) }
    }
    fn model_runtime(
        &self,
        _user: i64,
        _selection: &openwebide_core::ModelSelection,
    ) -> impl Future<Output = Result<openwebide_core::ModelRuntime, String>> + Send {
        async { Err("Model runtime unavailable".into()) }
    }
    fn model_complete(
        &self,
        _user: i64,
        _request: &openwebide_core::ChatRequest,
    ) -> impl Future<Output = Result<openwebide_core::ChatCompletion, String>> + Send {
        async { Err("Model completion unavailable".into()) }
    }
    fn model_complete_with_timeout(
        &self,
        user: i64,
        request: &openwebide_core::ChatRequest,
        _timeout_seconds: u32,
    ) -> impl Future<Output = Result<openwebide_core::ChatCompletion, String>> + Send {
        self.model_complete(user, request)
    }
    fn model_context(
        &self,
        _user: i64,
        _request: &openwebide_core::ChatRequest,
    ) -> impl Future<Output = Option<usize>> + Send {
        async { None }
    }
    fn model_tokens(
        &self,
        _user: i64,
        _request: &openwebide_core::ChatRequest,
    ) -> impl Future<Output = Option<usize>> + Send {
        async { None }
    }
    fn approval_check(
        &self,
        _user: i64,
        _session: i64,
        _check: &openwebide_core::ApprovalCheck,
    ) -> impl Future<Output = Result<openwebide_core::ApprovalDecision, String>> + Send {
        async { Ok(openwebide_core::ApprovalDecision::default()) }
    }
    fn run_plan(
        &self,
        user_id: i64,
        session_id: i64,
        content: &str,
        model: Option<&str>,
        editor_context: Option<&EditorContext>,
    ) -> impl Future<Output = Result<RunPlan, String>> + Send;
    fn queued_run_plan(
        &self,
        user: i64,
        session: i64,
        content: &str,
        model: Option<&str>,
        editor_context: Option<&EditorContext>,
        _key: openwebide_core::QueuedPromptKey,
    ) -> impl Future<Output = Result<RunPlan, String>> + Send {
        self.run_plan(user, session, content, model, editor_context)
    }
    fn persist_message(
        &self,
        user_id: i64,
        session_id: i64,
        role: Role,
        content: &str,
        usage: Option<&TurnTelemetry>,
        tool_calls: Option<&[openwebide_core::ToolCall]>,
    ) -> impl Future<Output = Result<ChatMessage, String>> + Send;
    #[allow(clippy::too_many_arguments)]
    fn upsert_tool_step(
        &self,
        user_id: i64,
        session_id: i64,
        anchor_id: i64,
        id: &str,
        name: &str,
        summary: &str,
        diff: Option<&FileDiff>,
    ) -> impl Future<Output = Result<(), String>> + Send;
    fn save_task(
        &self,
        _user: i64,
        _session: i64,
        _anchor: i64,
        _snapshot: &openwebide_core::TaskSnapshot,
    ) -> impl Future<Output = Result<(), String>> + Send {
        std::future::ready(Err("Child task persistence unavailable".into()))
    }
    fn save_tool_timing(
        &self,
        user: i64,
        session: i64,
        id: &str,
        timing: &openwebide_core::ToolTiming,
    ) -> impl Future<Output = Result<(), String>> + Send;
    fn save_project_checkpoint(
        &self,
        _user: i64,
        _session: i64,
        _id: &str,
        _checkpoint: &openwebide_core::rewind::ProjectCheckpoint,
    ) -> impl Future<Output = Result<(), String>> + Send {
        std::future::ready(Ok(()))
    }
    fn complete_tool_step(
        &self,
        user_id: i64,
        session_id: i64,
        id: &str,
        ok: bool,
        summary: &str,
        diff: Option<&FileDiff>,
    ) -> impl Future<Output = Result<(), String>> + Send;
    fn set_tool_stream_unsupported(
        &self,
        user_id: i64,
        connection_id: i64,
        tool_stream_revision: i64,
        model: Option<&str>,
    ) -> impl Future<Output = Result<(), String>> + Send;
    fn list_connections(
        &self,
        user_id: i64,
    ) -> impl Future<Output = Result<Vec<Connection>, String>> + Send;
    fn web_search(
        &self,
        user_id: i64,
        query: &str,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<WebSearchResult>, String>> + Send;
    fn web_fetch(
        &self,
        user_id: i64,
        url: &str,
    ) -> impl Future<Output = Result<String, String>> + Send;
}

#[derive(Clone, Debug)]
pub struct BackendClient {
    url: String,
    secret: Arc<str>,
    http: ReqwestHttpClient,
}

impl BackendClient {
    pub fn new(url: String, secret: Arc<str>, http: ReqwestHttpClient) -> Self {
        Self { url, secret, http }
    }

    pub async fn plugin_run_service(
        &self,
        request: &openwebide_core::plugins::runs::RunServiceRequest,
    ) -> Result<openwebide_core::plugins::runs::RunServiceResponse, String> {
        self.service_call("/plugins/runs/service", json!(request))
            .await
    }
    pub async fn plugin_job_service(
        &self,
        request: &openwebide_core::plugins::jobs::JobServiceRequest,
    ) -> Result<openwebide_core::plugins::jobs::JobServiceResponse, String> {
        self.service_call("/plugins/jobs/service", json!(request))
            .await
    }
    pub async fn due_tasks(
        &self,
        host: &openwebide_core::scheduled::ExecutionHost,
    ) -> Result<Vec<openwebide_core::scheduled::TaskDelivery>, String> {
        self.service_call("/scheduled-tasks/due", json!(host)).await
    }
    pub async fn task_result(
        &self,
        host: &str,
        result: &openwebide_core::scheduled::DispatchResult,
    ) -> Result<(), String> {
        let _: Value = self
            .service_call(
                "/scheduled-tasks/result",
                json!({"host_id":host,"result":result}),
            )
            .await?;
        Ok(())
    }
    async fn service_call<T: DeserializeOwned>(
        &self,
        path: &str,
        body: Value,
    ) -> Result<T, String> {
        let request = Request::builder()
            .method("POST")
            .uri(format!("{}{path}", self.url.trim_end_matches('/')))
            .header("authorization", format!("Bearer {}", self.secret))
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body.to_string())))
            .map_err(|error| error.to_string())?;
        serde_json::from_value(
            self.http
                .json(request)
                .await
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
    }
    pub async fn dispatch_push(&self) -> Result<(), String> {
        let request = Request::builder()
            .method("POST")
            .uri(format!("{}/push/dispatch", self.url.trim_end_matches('/')))
            .header("authorization", format!("Bearer {}", self.secret))
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from_static(b"{}")))
            .map_err(|_| "Invalid dispatcher request")?;
        let response = self
            .http
            .send(request)
            .await
            .map_err(|_| "Push dispatcher unavailable")?;
        if !response.status().is_success() {
            return Err(format!("Push dispatcher returned {}", response.status()));
        }
        Ok(())
    }
    pub async fn model_runtime(
        &self,
        user_id: i64,
        id: i64,
        model: Option<&str>,
    ) -> Result<openwebide_core::ModelRuntime, String> {
        let query = model
            .map(|model| format!("?model={}", encode_query(model)))
            .unwrap_or_default();
        self.call(
            user_id,
            "GET",
            &format!("/connections/{id}/runtime{query}"),
            json!({}),
        )
        .await
    }
    async fn call<T: DeserializeOwned>(
        &self,
        user_id: i64,
        method: &str,
        path: &str,
        body: Value,
    ) -> Result<T, String> {
        let body = if method == "GET" {
            Bytes::new()
        } else {
            Bytes::from(serde_json::to_vec(&body).map_err(|e| e.to_string())?)
        };
        let request = Request::builder()
            .method(method)
            .uri(format!("{}{}", self.url.trim_end_matches('/'), path))
            .header("authorization", format!("Bearer {}", self.secret))
            .header("x-openwebide-user", user_id.to_string())
            .header("content-type", "application/json")
            .body(Full::new(body))
            .map_err(|e| e.to_string())?;
        serde_json::from_value(self.http.json(request).await.map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())
    }
}

pub fn encode_query(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl RunBackend for BackendClient {
    async fn plugin_request(
        &self,
        user: i64,
        session: i64,
        request: &openwebide_core::plugins::execution::PluginHostRequest,
    ) -> Result<String, String> {
        self.call(
            user,
            "POST",
            &format!("/sessions/{session}/plugin-host"),
            json!(request),
        )
        .await
    }
    async fn host_journal(
        &self,
        command: &openwebide_core::host_admin::HostJournalCommand,
    ) -> Result<openwebide_core::host_admin::HostJournalResult, String> {
        self.call(0, "POST", "/host/journal", json!(command)).await
    }
    async fn scheduled_command(
        &self,
        user: i64,
        session: i64,
        command: &openwebide_core::scheduled::TaskCommand,
    ) -> Result<Vec<openwebide_core::scheduled::ScheduledTask>, String> {
        self.call(
            user,
            "POST",
            &format!("/sessions/{session}/scheduled-tasks"),
            json!(command),
        )
        .await
    }

    async fn run_lease(
        &self,
        user: i64,
        session: i64,
        token: &str,
        release: bool,
        since: i64,
        permission: Option<&str>,
    ) -> Result<openwebide_core::scheduled::RunControl, String> {
        self.call(
            user,
            "POST",
            &format!("/sessions/{session}/run-lease"),
            json!({"token":token,"release":release,"since":since,"permission_id":permission}),
        )
        .await
    }

    async fn notify(
        &self,
        user: i64,
        session: i64,
        event: &openwebide_core::push::RunNotification,
    ) -> Result<(), String> {
        let _: Value = self
            .call(
                user,
                "POST",
                &format!("/sessions/{session}/notifications"),
                serde_json::to_value(event).map_err(|error| error.to_string())?,
            )
            .await?;
        Ok(())
    }

    async fn memory_command(
        &self,
        user: i64,
        session: i64,
        command: &openwebide_core::MemoryCommand,
    ) -> Result<openwebide_core::ProjectMemories, String> {
        self.call(
            user,
            "POST",
            &format!("/sessions/{session}/memories"),
            serde_json::to_value(command).map_err(|error| error.to_string())?,
        )
        .await
    }
    async fn question_command(
        &self,
        user: i64,
        session: i64,
        command: &openwebide_core::questions::QuestionCommand,
    ) -> Result<openwebide_core::questions::QuestionResult, String> {
        self.call(
            user,
            "POST",
            &format!("/sessions/{session}/questions"),
            serde_json::to_value(command).map_err(|error| error.to_string())?,
        )
        .await
    }
    async fn skill_command(
        &self,
        user: i64,
        session: i64,
        command: &openwebide_core::SkillCommand,
    ) -> Result<openwebide_core::ProjectSkills, String> {
        self.call(
            user,
            "POST",
            &format!("/sessions/{session}/skills"),
            serde_json::to_value(command).map_err(|error| error.to_string())?,
        )
        .await
    }
    async fn get_todo_plan(
        &self,
        user: i64,
        session: i64,
    ) -> Result<Option<openwebide_core::TodoUpdate>, String> {
        self.call(
            user,
            "GET",
            &format!("/sessions/{session}/todos"),
            Value::Null,
        )
        .await
    }
    async fn write_todo_plan(
        &self,
        user: i64,
        session: i64,
        anchor: i64,
        plan: &openwebide_core::TodoPlan,
    ) -> Result<openwebide_core::TodoUpdate, String> {
        self.call(
            user,
            "POST",
            &format!("/sessions/{session}/todos"),
            json!({ "anchor_message_id": anchor, "plan": plan }),
        )
        .await
    }

    async fn model_runtime(
        &self,
        user: i64,
        selection: &openwebide_core::ModelSelection,
    ) -> Result<openwebide_core::ModelRuntime, String> {
        BackendClient::model_runtime(self, user, selection.server_id, Some(&selection.model)).await
    }
    async fn model_complete(
        &self,
        user: i64,
        request: &openwebide_core::ChatRequest,
    ) -> Result<openwebide_core::ChatCompletion, String> {
        self.call(
            user,
            "POST",
            "/models/complete",
            serde_json::to_value(request).map_err(|error| error.to_string())?,
        )
        .await
    }
    async fn model_complete_with_timeout(
        &self,
        user: i64,
        request: &openwebide_core::ChatRequest,
        timeout_seconds: u32,
    ) -> Result<openwebide_core::ChatCompletion, String> {
        self.call(
            user,
            "POST",
            "/models/background",
            serde_json::to_value(openwebide_core::BackgroundCompletion {
                request: request.clone(),
                timeout_seconds,
            })
            .map_err(|error| error.to_string())?,
        )
        .await
    }
    async fn model_context(
        &self,
        user: i64,
        request: &openwebide_core::ChatRequest,
    ) -> Option<usize> {
        let model = request
            .model
            .as_deref()
            .map(|model| format!("&model={}", encode_query(model)))
            .unwrap_or_default();
        let value: Value = self
            .call(
                user,
                "GET",
                &format!(
                    "/models/context?connection_id={}{}",
                    request.connection_id, model
                ),
                json!({}),
            )
            .await
            .ok()?;
        value["context_limit"]
            .as_u64()
            .and_then(|limit| usize::try_from(limit).ok())
    }
    async fn model_tokens(
        &self,
        user: i64,
        request: &openwebide_core::ChatRequest,
    ) -> Option<usize> {
        self.call(
            user,
            "POST",
            "/models/tokens",
            serde_json::to_value(request).ok()?,
        )
        .await
        .ok()
        .flatten()
    }
    async fn approval_check(
        &self,
        user: i64,
        session: i64,
        check: &openwebide_core::ApprovalCheck,
    ) -> Result<openwebide_core::ApprovalDecision, String> {
        self.call(
            user,
            "POST",
            &format!("/sessions/{session}/approval-check"),
            serde_json::to_value(check).map_err(|error| error.to_string())?,
        )
        .await
    }
    async fn run_plan(
        &self,
        user_id: i64,
        session_id: i64,
        content: &str,
        model: Option<&str>,
        editor_context: Option<&EditorContext>,
    ) -> Result<RunPlan, String> {
        self.call(
            user_id,
            "POST",
            &format!("/sessions/{session_id}/run-plan"),
            json!({"content":content,"model":model,"editor_context":editor_context}),
        )
        .await
    }
    async fn queued_run_plan(
        &self,
        user: i64,
        session: i64,
        content: &str,
        model: Option<&str>,
        editor_context: Option<&EditorContext>,
        key: openwebide_core::QueuedPromptKey,
    ) -> Result<RunPlan, String> {
        self.call(user, "POST", &format!("/sessions/{session}/run-plan"),
            json!({"content":content,"model":model,"editor_context":editor_context,"queued_prompt":key})).await
    }
    async fn consume_queued_prompt(
        &self,
        user: i64,
        session: i64,
        key: openwebide_core::QueuedPromptKey,
        content: &str,
    ) -> Result<ChatMessage, String> {
        self.call(
            user,
            "POST",
            &format!("/sessions/{session}/queue/send"),
            json!({"key":key,"content":content}),
        )
        .await
    }
    async fn persist_message(
        &self,
        user_id: i64,
        session_id: i64,
        role: Role,
        content: &str,
        usage: Option<&TurnTelemetry>,
        tool_calls: Option<&[openwebide_core::ToolCall]>,
    ) -> Result<ChatMessage, String> {
        self.call(
            user_id,
            "POST",
            &format!("/sessions/{session_id}/messages/persist"),
            json!({"role":role,"content":content,"usage":usage,"tool_calls":tool_calls}),
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    async fn upsert_tool_step(
        &self,
        user_id: i64,
        session_id: i64,
        anchor_id: i64,
        id: &str,
        name: &str,
        summary: &str,
        diff: Option<&FileDiff>,
    ) -> Result<(), String> {
        let _: Value = self.call(user_id, "POST", &format!("/sessions/{session_id}/tool-steps/upsert"), json!({"anchor_message_id":anchor_id,"tool_call_id":id,"name":name,"summary":summary,"diff":diff})).await?;
        Ok(())
    }
    async fn save_task(
        &self,
        user: i64,
        session: i64,
        anchor: i64,
        snapshot: &openwebide_core::TaskSnapshot,
    ) -> Result<(), String> {
        let _: Value = self
            .call(
                user,
                "POST",
                &format!("/sessions/{session}/tasks"),
                json!({"anchor_message_id":anchor,"snapshot":snapshot}),
            )
            .await?;
        Ok(())
    }
    async fn save_tool_timing(
        &self,
        user: i64,
        session: i64,
        id: &str,
        timing: &openwebide_core::ToolTiming,
    ) -> Result<(), String> {
        let _: Value = self
            .call(
                user,
                "POST",
                &format!("/sessions/{session}/tool-steps/timing"),
                json!({"tool_call_id":id,"timing":timing}),
            )
            .await?;
        Ok(())
    }
    async fn save_project_checkpoint(
        &self,
        user: i64,
        session: i64,
        id: &str,
        checkpoint: &openwebide_core::rewind::ProjectCheckpoint,
    ) -> Result<(), String> {
        let _: Value = self.call(user, "POST", &format!("/sessions/{session}/tool-steps/upsert"), json!({"anchor_message_id":0,"tool_call_id":id,"name":"","summary":"","checkpoint":checkpoint})).await?;
        Ok(())
    }
    async fn complete_tool_step(
        &self,
        user_id: i64,
        session_id: i64,
        id: &str,
        ok: bool,
        summary: &str,
        diff: Option<&FileDiff>,
    ) -> Result<(), String> {
        let _: Value = self
            .call(
                user_id,
                "POST",
                &format!("/sessions/{session_id}/tool-steps/complete"),
                json!({"tool_call_id":id,"ok":ok,"result_summary":summary,"diff":diff}),
            )
            .await?;
        Ok(())
    }
    async fn set_tool_stream_unsupported(
        &self,
        user_id: i64,
        connection_id: i64,
        tool_stream_revision: i64,
        model: Option<&str>,
    ) -> Result<(), String> {
        let _: Value = self
            .call(
                user_id,
                "POST",
                &format!("/connections/{connection_id}/tool-stream-unsupported"),
                json!({"tool_stream_revision": tool_stream_revision, "model": model}),
            )
            .await?;
        Ok(())
    }
    async fn list_connections(&self, user_id: i64) -> Result<Vec<Connection>, String> {
        self.call(user_id, "GET", "/connections", Value::Null).await
    }
    async fn web_search(
        &self,
        user_id: i64,
        query: &str,
        limit: usize,
    ) -> Result<Vec<WebSearchResult>, String> {
        self.call(
            user_id,
            "GET",
            &format!("/web/search?query={}&limit={limit}", encode_query(query)),
            Value::Null,
        )
        .await
    }
    async fn web_fetch(&self, user_id: i64, url: &str) -> Result<String, String> {
        let response: Value = self
            .call(
                user_id,
                "GET",
                &format!("/web/fetch?url={}", encode_query(url)),
                Value::Null,
            )
            .await?;
        response
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "missing page content".into())
    }
}

pub struct ApprovalAdapter<B> {
    pub backend: Arc<B>,
    pub user: i64,
    pub session: i64,
    pub connection_id: i64,
    pub model: Option<String>,
}
impl<B: RunBackend> openwebide_agent::policy::ApprovalSource for ApprovalAdapter<B> {
    async fn check(&self, call: &openwebide_core::ToolCall) -> bool {
        self.backend
            .approval_check(
                self.user,
                self.session,
                &openwebide_core::ApprovalCheck {
                    connection_id: self.connection_id,
                    model: self.model.clone(),
                    call: call.clone(),
                },
            )
            .await
            .is_ok_and(|decision| decision.approved)
    }
}

pub struct ModelSource<B> {
    pub backend: Arc<B>,
    pub user: i64,
}
impl<B> Clone for ModelSource<B> {
    fn clone(&self) -> Self {
        Self {
            backend: self.backend.clone(),
            user: self.user,
        }
    }
}
impl<B: RunBackend> openwebide_agent::compaction::CompactionSource for ModelSource<B> {
    fn available(&self) -> bool {
        true
    }
    async fn runtime(
        &self,
        selection: &openwebide_core::ModelSelection,
    ) -> Result<openwebide_core::ModelRuntime, String> {
        self.backend.model_runtime(self.user, selection).await
    }
    async fn complete(
        &self,
        request: &openwebide_core::ChatRequest,
    ) -> Result<openwebide_core::ChatCompletion, String> {
        self.backend.model_complete(self.user, request).await
    }
    async fn complete_with_timeout(
        &self,
        request: &openwebide_core::ChatRequest,
        timeout_seconds: u32,
    ) -> Result<openwebide_core::ChatCompletion, String> {
        tokio::time::timeout(
            std::time::Duration::from_secs(u64::from(timeout_seconds)),
            self.backend
                .model_complete_with_timeout(self.user, request, timeout_seconds),
        )
        .await
        .map_err(|_| "Background model deadline exceeded".to_owned())?
    }
    async fn context_limit(&self, request: &openwebide_core::ChatRequest) -> Option<usize> {
        self.backend.model_context(self.user, request).await
    }
    async fn tokens(&self, request: &openwebide_core::ChatRequest) -> Option<usize> {
        self.backend.model_tokens(self.user, request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn records_streamed_tools_flag_at_connection_route() {
        let (url, captured) = crate::runs::http_client::tests::capture(
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
        )
        .await;
        let client = BackendClient::new(
            format!("{url}/api"),
            "shared-secret".into(),
            ReqwestHttpClient::default(),
        );
        client
            .set_tool_stream_unsupported(42, 7, 3, Some("main"))
            .await
            .unwrap();
        let request = captured.await.unwrap().to_ascii_lowercase();
        assert!(
            request.starts_with("post /api/connections/7/tool-stream-unsupported http/1.1\r\n")
        );
        assert!(request.contains("x-openwebide-user: 42\r\n"));
        assert!(request.contains("\"tool_stream_revision\":3"));
    }

    #[tokio::test]
    async fn sends_secret_and_acting_user() {
        let (url, captured) = crate::runs::http_client::tests::capture(
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]",
        )
        .await;
        let client = BackendClient::new(
            format!("{url}/api"),
            "shared-secret".into(),
            ReqwestHttpClient::default(),
        );
        assert!(client.list_connections(42).await.unwrap().is_empty());
        let request = captured.await.unwrap().to_ascii_lowercase();
        assert!(request.starts_with("get /api/connections http/1.1\r\n"));
        assert!(request.contains("authorization: bearer shared-secret\r\n"));
        assert!(request.contains("x-openwebide-user: 42\r\n"));
    }
}

#[cfg(test)]
mod memory_tests {
    use super::*;
    #[tokio::test]
    async fn memory_adapter_forwards_owned_session_and_reports_failed_persistence() {
        for status in [200, 409] {
            let body = if status == 200 {
                r#"{"enabled":true,"entries":[]}"#
            } else {
                r#"{"error":"Memory changed"}"#
            };
            let response = format!(
                "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let (url, captured) =
                crate::runs::http_client::tests::capture(Box::leak(response.into_boxed_str()))
                    .await;
            let client = BackendClient::new(
                format!("{url}/api"),
                "secret".into(),
                ReqwestHttpClient::default(),
            );
            let result = client
                .memory_command(
                    42,
                    7,
                    &openwebide_core::MemoryCommand::Delete { id: 9, revision: 2 },
                )
                .await;
            assert_eq!(result.is_ok(), status == 200);
            let request = captured.await.unwrap().to_ascii_lowercase();
            assert!(request.starts_with("post /api/sessions/7/memories http/1.1"));
            assert!(request.contains("authorization: bearer secret\r\n"));
            assert!(request.contains("x-openwebide-user: 42\r\n"));
            assert!(request.contains(r#""revision":2"#));
            assert!(request.contains(r#""action":"delete""#));
        }
    }
}

#[cfg(test)]
mod scheduled_tests {
    use super::*;
    #[tokio::test]
    async fn queued_plan_adapter_forwards_authoritative_prompt_identity() {
        let body = r#"{"error":"Queue changed"}"#;
        let response = format!(
            "HTTP/1.1 409 Conflict\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (url, captured) =
            crate::runs::http_client::tests::capture(Box::leak(response.into_boxed_str())).await;
        let client = BackendClient::new(
            format!("{url}/api"),
            "secret".into(),
            ReqwestHttpClient::default(),
        );
        let result = client
            .queued_run_plan(
                42,
                7,
                "Work",
                None,
                None,
                openwebide_core::QueuedPromptKey { id: 9, revision: 2 },
            )
            .await;
        assert!(result.is_err());
        let request = captured.await.unwrap();
        let (headers, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("POST /api/sessions/7/run-plan HTTP/1.1"));
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("x-openwebide-user: 42\r\n")
        );
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["queued_prompt"], json!({"id":9,"revision":2}));
        assert_eq!(body["content"], "Work");
        assert!(body["model"].is_null());
    }

    #[tokio::test]
    async fn schedule_adapters_keep_session_identity_and_service_scope_on_success_and_failure() {
        for status in [200, 409] {
            let body = if status == 200 {
                "[]"
            } else {
                r#"{"error":"Changed"}"#
            };
            for service in [false, true] {
                let response = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let (url, captured) =
                    crate::runs::http_client::tests::capture(Box::leak(response.into_boxed_str()))
                        .await;
                let client = BackendClient::new(
                    format!("{url}/api"),
                    "secret".into(),
                    ReqwestHttpClient::default(),
                );
                let result = if service {
                    client
                        .due_tasks(&openwebide_core::scheduled::ExecutionHost {
                            id: "paired".into(),
                            name: "Host".into(),
                            last_seen: 0,
                        })
                        .await
                        .map(|_| ())
                } else {
                    client
                        .scheduled_command(
                            42,
                            7,
                            &openwebide_core::scheduled::TaskCommand::Monitor {
                                session_id: 0,
                                command: openwebide_core::scheduled::MonitorCommand::Cancel {
                                    id: 9,
                                    revision: 2,
                                },
                            },
                        )
                        .await
                        .map(|_| ())
                };
                assert_eq!(result.is_ok(), status == 200);
                let request = captured.await.unwrap().to_ascii_lowercase();
                assert!(request.contains("authorization: bearer secret\r\n"));
                if service {
                    assert!(request.starts_with("post /api/scheduled-tasks/due http/1.1"));
                    assert!(!request.contains("x-openwebide-user:"));
                    assert!(request.contains(r#""id":"paired""#));
                } else {
                    assert!(request.starts_with("post /api/sessions/7/scheduled-tasks http/1.1"));
                    assert!(request.contains("x-openwebide-user: 42\r\n"));
                    assert!(request.contains(r#""revision":2"#));
                    assert!(request.contains(r#""action":"monitor""#));
                    assert!(request.contains(r#""session_id":0"#));
                }
            }
        }
    }
}

#[cfg(test)]
mod assistance_tests {
    use super::*;
    use openwebide_agent::model::ModelSource as _;
    #[tokio::test]
    async fn bounded_model_adapter_forwards_deadline_and_user() {
        let body = serde_json::to_string(&openwebide_core::ChatCompletion {
            response: openwebide_core::ChatResponse::Text("Summary".into()),
            reasoning: String::new(),
            preamble: String::new(),
            usage: None,
            stop_reason: Default::default(),
        })
        .unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (url, captured) =
            crate::runs::http_client::tests::capture(Box::leak(response.into_boxed_str())).await;
        let client = BackendClient::new(
            format!("{url}/api"),
            "secret".into(),
            ReqwestHttpClient::default(),
        );
        let source = ModelSource {
            backend: Arc::new(client),
            user: 42,
        };
        let request = openwebide_core::ChatRequest {
            connection_id: 7,
            model_settings: Default::default(),
            system_prompt: None,
            model: None,
            messages: Vec::new(),
            tools: Vec::new(),
        };
        source.complete_with_timeout(&request, 5).await.unwrap();
        let request = captured.await.unwrap().to_ascii_lowercase();
        assert!(request.starts_with("post /api/models/background http/1.1"));
        assert!(request.contains("x-openwebide-user: 42\r\n"));
        assert!(request.contains("\"timeout_seconds\":5"));
    }
}

#[cfg(test)]
mod skill_tests {
    use super::*;
    #[tokio::test]
    async fn skill_adapter_forwards_owned_session_and_reports_failed_persistence() {
        for status in [200, 409] {
            let body = if status == 200 {
                r#"{"enabled":true,"entries":[]}"#
            } else {
                r#"{"error":"Skill changed"}"#
            };
            let response = format!(
                "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let (url, captured) =
                crate::runs::http_client::tests::capture(Box::leak(response.into_boxed_str()))
                    .await;
            let client = BackendClient::new(
                format!("{url}/api"),
                "secret".into(),
                ReqwestHttpClient::default(),
            );
            let result = client
                .skill_command(
                    42,
                    7,
                    &openwebide_core::SkillCommand::Delete { id: 9, revision: 2 },
                )
                .await;
            assert_eq!(result.is_ok(), status == 200);
            let request = captured.await.unwrap().to_ascii_lowercase();
            assert!(request.starts_with("post /api/sessions/7/skills http/1.1"));
            assert!(request.contains("authorization: bearer secret\r\n"));
            assert!(request.contains("x-openwebide-user: 42\r\n"));
            assert!(request.contains(r#""revision":2"#));
            assert!(request.contains(r#""action":"delete""#));
        }
    }
}

#[cfg(test)]
mod question_tests {
    use super::*;
    #[tokio::test]
    async fn question_transport_authenticates_the_owner_and_preserves_commands_and_failures() {
        use openwebide_core::questions::*;
        for status in [200, 401, 500] {
            let body = serde_json::to_string(&QuestionResult { questions: vec![] }).unwrap();
            let response = format!(
                "HTTP/1.1 {status} Response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let (url, captured) =
                crate::runs::http_client::tests::capture(Box::leak(response.into_boxed_str()))
                    .await;
            let client = BackendClient::new(
                format!("{url}/api"),
                "secret".into(),
                ReqwestHttpClient::default(),
            );
            let command = QuestionCommand::Reply {
                id: "a3t1c0".into(),
                reply: QuestionReply::Answer {
                    answers: vec![QuestionAnswer {
                        id: "path".into(),
                        value: AnswerValue::Text {
                            text: "/srv/media".into(),
                        },
                    }],
                },
            };
            assert_eq!(
                client.question_command(42, 7, &command).await.is_ok(),
                status == 200
            );
            let request = captured.await.unwrap();
            assert!(request.starts_with("POST /api/sessions/7/questions HTTP/1.1"));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("x-openwebide-user: 42\r\n")
            );
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer secret\r\n")
            );
            let body = request.split("\r\n\r\n").nth(1).unwrap();
            assert_eq!(
                serde_json::from_str::<QuestionCommand>(body).unwrap(),
                command
            );
        }
    }
}
