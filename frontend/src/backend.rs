use crate::api::BackendApi;
use futures::future::LocalBoxFuture;
use leptos::prelude::{LocalStorage, RwSignal, StoredValue};
use openwebide_core::{
    ChatCompletion, ChatMessage, ChatRequest, ChatSession, Connection, ConversationEntry,
    EditorContext, FileDiff, FileEntry, GitBranchInfo, GitCheckoutRequest, GitCheckoutResult,
    GitCommitRequest, GitCommitResult, GitRepoStatus, GitSyncRequest, GitSyncResult, Health,
    ModelInfo, PersistedEdit, Project, ProviderKind, ResolveEditRequest, Role, RunEvent, SearchHit,
    SystemPrompt, TurnTelemetry, User, WebSearchResult, WorkspaceMode, vfs::SearchOptions,
};
use std::rc::Rc;
use web_sys::AbortSignal;

pub type Api = StoredValue<Rc<dyn Backend>, LocalStorage>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryError {
    Conflict(String),
    Unavailable(String),
}
impl std::fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict(message) | Self::Unavailable(message) => f.write_str(message),
        }
    }
}

pub trait Backend {
    fn session_expired(&self) -> RwSignal<bool>;
    fn register<'a>(
        &'a self,
        username: &'a str,
        password: &'a str,
    ) -> LocalBoxFuture<'a, Result<User, String>>;
    fn login<'a>(
        &'a self,
        username: &'a str,
        password: &'a str,
    ) -> LocalBoxFuture<'a, Result<User, String>>;
    fn me<'a>(&'a self) -> LocalBoxFuture<'a, Result<User, String>>;
    fn logout<'a>(&'a self) -> LocalBoxFuture<'a, Result<(), String>>;
    fn bridge_token<'a>(&'a self) -> LocalBoxFuture<'a, Result<(String, i64), String>>;
    fn health<'a>(&'a self) -> LocalBoxFuture<'a, Result<Health, String>>;
    fn preview_server<'a>(
        &'a self,
        probe: &'a openwebide_core::ModelProbe,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ServerDiscovery, String>>;
    fn save_model_setup<'a>(
        &'a self,
        probe: &'a openwebide_core::ModelProbe,
        profiles: &'a [openwebide_core::ModelProfile],
    ) -> LocalBoxFuture<'a, Result<(Connection, openwebide_core::ModelSetup), String>>;
    fn test_model<'a>(
        &'a self,
        probe: &'a openwebide_core::ModelProbe,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ModelTestResult, String>>;
    fn preview_model<'a>(
        &'a self,
        probe: &'a openwebide_core::ModelProbe,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ModelDetection, String>>;
    fn detect_model<'a>(
        &'a self,
        id: i64,
        model: &'a str,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ModelDetection, String>>;
    fn inspect_server<'a>(
        &'a self,
        base_url: &'a str,
        kind: Option<ProviderKind>,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ServerDiscovery, String>>;
    fn discover_servers<'a>(
        &'a self,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::ServerDiscovery>, String>>;
    fn model_runtime<'a>(
        &'a self,
        id: i64,
        model: Option<&'a str>,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ModelRuntime, String>>;
    fn model_setup<'a>(&'a self)
    -> LocalBoxFuture<'a, Result<openwebide_core::ModelSetup, String>>;
    fn save_model_defaults<'a>(
        &'a self,
        defaults: &'a openwebide_core::ModelDefaults,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ModelSetup, String>>;
    fn save_model_profile<'a>(
        &'a self,
        profile: &'a openwebide_core::ModelProfile,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ModelSetup, String>>;
    fn server_settings<'a>(
        &'a self,
        id: i64,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ServerSettings, String>>;
    fn save_server_settings<'a>(
        &'a self,
        id: i64,
        update: &'a openwebide_core::ServerSettingsUpdate,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ServerSettings, String>>;
    fn list_connections<'a>(&'a self) -> LocalBoxFuture<'a, Result<Vec<Connection>, String>>;
    fn create_connection<'a>(
        &'a self,
        name: &'a str,
        kind: ProviderKind,
        base_url: &'a str,
        model: Option<&'a str>,
        context_limit: Option<usize>,
    ) -> LocalBoxFuture<'a, Result<Connection, String>>;
    fn update_connection<'a>(
        &'a self,
        connection: &'a Connection,
    ) -> LocalBoxFuture<'a, Result<Connection, String>>;
    fn delete_connection<'a>(&'a self, id: i64) -> LocalBoxFuture<'a, Result<(), String>>;
    fn list_sessions<'a>(&'a self) -> LocalBoxFuture<'a, Result<Vec<ChatSession>, String>>;
    fn search_sessions<'a>(
        &'a self,
        search: &'a openwebide_core::SessionSearch,
    ) -> LocalBoxFuture<'a, Result<Vec<ChatSession>, String>>;
    fn session_preferences<'a>(
        &'a self,
        id: i64,
        preferences: &'a openwebide_core::SessionPreferences,
    ) -> LocalBoxFuture<'a, Result<ChatSession, String>>;
    fn session_title<'a>(
        &'a self,
        id: i64,
    ) -> LocalBoxFuture<'a, Result<Option<ChatSession>, String>>;
    fn export_session<'a>(
        &'a self,
        id: i64,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::SessionExport, String>>;
    fn list_models<'a>(
        &'a self,
        connection_id: i64,
    ) -> LocalBoxFuture<'a, Result<Vec<ModelInfo>, String>>;
    fn list_system_prompts<'a>(&'a self) -> LocalBoxFuture<'a, Result<Vec<SystemPrompt>, String>>;
    fn create_system_prompt<'a>(
        &'a self,
        name: &'a str,
        content: &'a str,
    ) -> LocalBoxFuture<'a, Result<SystemPrompt, String>>;
    fn update_system_prompt<'a>(
        &'a self,
        id: i64,
        name: &'a str,
        content: &'a str,
    ) -> LocalBoxFuture<'a, Result<SystemPrompt, String>>;
    fn delete_system_prompt<'a>(&'a self, id: i64) -> LocalBoxFuture<'a, Result<(), String>>;
    fn editor_recovery(
        &self,
        project: i64,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::editor::EditorRecoveryRecord, RecoveryError>>;
    fn save_editor_recovery<'a>(
        &'a self,
        project: i64,
        record: &'a openwebide_core::editor::EditorRecoveryRecord,
    ) -> LocalBoxFuture<'a, Result<i64, RecoveryError>>;
    fn push_config(&self) -> LocalBoxFuture<'_, Result<openwebide_core::push::PushConfig, String>> {
        Box::pin(async { Err("Background notifications are unavailable".into()) })
    }
    fn save_push_subscription<'a>(
        &'a self,
        _subscription: &'a openwebide_core::push::PushSubscription,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err("Background notifications are unavailable".into()) })
    }
    fn get_settings<'a>(
        &'a self,
    ) -> LocalBoxFuture<'a, Result<std::collections::BTreeMap<String, String>, String>>;
    fn set_setting<'a>(
        &'a self,
        key: &'a str,
        value: &'a str,
    ) -> LocalBoxFuture<'a, Result<(), String>>;
    fn startup_context(
        &self,
        project: i64,
        tools: bool,
        connection: Option<i64>,
    ) -> LocalBoxFuture<'_, Result<String, String>>;
    fn create_session<'a>(
        &'a self,
        name: &'a str,
        connection_id: Option<i64>,
        system_prompt_id: Option<i64>,
        project_id: Option<i64>,
    ) -> LocalBoxFuture<'a, Result<ChatSession, String>>;
    fn list_projects<'a>(&'a self) -> LocalBoxFuture<'a, Result<Vec<Project>, String>>;
    fn create_project<'a>(
        &'a self,
        name: &'a str,
        mode: WorkspaceMode,
        path: Option<String>,
    ) -> LocalBoxFuture<'a, Result<Project, String>>;
    fn rename_project<'a>(
        &'a self,
        id: i64,
        name: &'a str,
    ) -> LocalBoxFuture<'a, Result<Project, String>>;
    fn delete_project<'a>(&'a self, id: i64) -> LocalBoxFuture<'a, Result<(), String>>;
    fn list_files<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<Vec<FileEntry>, String>>;
    fn read_file_object_url<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<String, String>>;
    fn read_file<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<String, String>>;
    fn canonical_file_path<'a>(
        &'a self,
        _project: i64,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(
            async move { openwebide_core::vfs::workspace_path(path).map_err(|e| e.to_string()) },
        )
    }
    fn read_file_bytes<'a>(
        &'a self,
        project: i64,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<Vec<u8>, String>> {
        Box::pin(async move { self.read_file(project, path).await.map(String::into_bytes) })
    }
    fn read_file_lossy<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<String, String>>;
    fn write_file<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
        content: &'a str,
    ) -> LocalBoxFuture<'a, Result<(), String>>;
    fn write_file_bytes<'a>(
        &'a self,
        project: i64,
        path: &'a str,
        bytes: &'a [u8],
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
            self.write_file(project, path, text).await
        })
    }
    fn copy_file<'a>(
        &'a self,
        project_id: i64,
        from: &'a str,
        to: &'a str,
    ) -> LocalBoxFuture<'a, Result<(), String>>;
    fn create_file<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
        is_dir: bool,
    ) -> LocalBoxFuture<'a, Result<(), String>>;
    fn delete_file<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<(), String>>;
    fn browse<'a>(&'a self, path: &'a str) -> LocalBoxFuture<'a, Result<Vec<FileEntry>, String>>;
    fn search_content<'a>(
        &'a self,
        project_id: i64,
        query: &'a str,
        path: &'a str,
        opts: SearchOptions,
    ) -> LocalBoxFuture<'a, Result<Vec<SearchHit>, String>>;
    fn git_status<'a>(
        &'a self,
        project_id: Option<i64>,
    ) -> LocalBoxFuture<'a, Result<GitRepoStatus, String>>;
    fn git_diff<'a>(
        &'a self,
        project_id: Option<i64>,
        path: Option<&'a str>,
    ) -> LocalBoxFuture<'a, Result<String, String>>;
    fn git_file_head<'a>(
        &'a self,
        project_id: Option<i64>,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<String, String>>;
    fn git_stash<'a>(
        &'a self,
        project_id: Option<i64>,
        request: &'a openwebide_core::git::GitStashRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::git::GitStashResult, String>>;
    fn git_index_diff(&self, project_id: Option<i64>)
    -> LocalBoxFuture<'_, Result<String, String>>;
    fn git_history<'a>(
        &'a self,
        _project_id: Option<i64>,
        _request: &'a openwebide_core::git::GitHistoryRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::git::GitHistoryPage, String>>;
    fn git_commit_diff<'a>(
        &'a self,
        _project_id: Option<i64>,
        _request: &'a openwebide_core::git::GitCommitDiffRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::git::GitCommitDiff, String>>;
    fn git_branches<'a>(
        &'a self,
        project_id: Option<i64>,
    ) -> LocalBoxFuture<'a, Result<Vec<GitBranchInfo>, String>>;
    fn git_path_changes(
        &self,
        project_id: Option<i64>,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::git::GitPathChanges, String>>;
    fn git_path_action<'a>(
        &'a self,
        project_id: Option<i64>,
        request: &'a openwebide_core::git::GitPathRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::git::GitPathChanges, String>>;
    fn git_commit<'a>(
        &'a self,
        project_id: Option<i64>,
        req: &'a GitCommitRequest,
    ) -> LocalBoxFuture<'a, Result<GitCommitResult, String>>;
    fn git_checkout<'a>(
        &'a self,
        project_id: Option<i64>,
        req: &'a GitCheckoutRequest,
    ) -> LocalBoxFuture<'a, Result<GitCheckoutResult, String>>;
    fn git_sync<'a>(
        &'a self,
        project_id: Option<i64>,
        req: &'a GitSyncRequest,
    ) -> LocalBoxFuture<'a, Result<GitSyncResult, String>>;
    fn set_session_connection(
        &self,
        id: i64,
        connection_id: i64,
    ) -> LocalBoxFuture<'_, Result<ChatSession, String>>;
    fn rename_session<'a>(
        &'a self,
        id: i64,
        name: &'a str,
    ) -> LocalBoxFuture<'a, Result<ChatSession, String>>;
    fn delete_session<'a>(&'a self, id: i64) -> LocalBoxFuture<'a, Result<(), String>>;
    fn cancel_session<'a>(&'a self, session_id: i64) -> LocalBoxFuture<'a, Result<(), String>>;
    fn set_permission<'a>(
        &'a self,
        session_id: i64,
        tool_call_id: &'a str,
        approved: bool,
    ) -> LocalBoxFuture<'a, Result<(), String>>;
    fn list_messages<'a>(
        &'a self,
        session_id: i64,
    ) -> LocalBoxFuture<'a, Result<Vec<ConversationEntry>, String>>;
    fn list_run_changes(
        &self,
        _project: i64,
    ) -> LocalBoxFuture<'_, Result<Vec<openwebide_core::RunChange>, String>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn preview_run_review<'a>(
        &'a self,
        _project: i64,
        _request: &'a openwebide_core::ReviewRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ReviewPlan, String>> {
        Box::pin(async { Err("Run review unavailable".into()) })
    }
    fn prepare_run_review<'a>(
        &'a self,
        _project: i64,
        _request: &'a openwebide_core::ReviewRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ReviewPlan, String>> {
        Box::pin(async { Err("Run review unavailable".into()) })
    }
    fn complete_run_review<'a>(
        &'a self,
        _project: i64,
        _request: &'a openwebide_core::ReviewRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::RunChange, String>> {
        Box::pin(async { Err("Run review unavailable".into()) })
    }
    fn prepare_rewind(
        &self,
        session: i64,
        message: i64,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::RewindPlan, String>>;
    fn complete_rewind(
        &self,
        session: i64,
        message: i64,
    ) -> LocalBoxFuture<'_, Result<Vec<ConversationEntry>, String>>;
    fn session_search_suggestions<'a>(
        &'a self,
        search: &'a openwebide_core::SessionSearch,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::SessionSearchResults, String>> {
        Box::pin(async move {
            Ok(openwebide_core::SessionSearchResults {
                sessions: self.search_sessions(search).await?,
                ..Default::default()
            })
        })
    }
    fn staged_assistance<'a>(
        &'a self,
        request: &'a openwebide_core::AssistanceRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::assistance::GitDraftResult, String>>;
    fn assistance<'a>(
        &'a self,
        _request: &'a openwebide_core::AssistanceRequest,
    ) -> LocalBoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(async { Ok(None) })
    }
    fn model_complete<'a>(
        &'a self,
        request: &'a ChatRequest,
    ) -> LocalBoxFuture<'a, Result<ChatCompletion, String>> {
        self.chat_tools(request)
    }
    fn model_complete_with_timeout<'a>(
        &'a self,
        request: &'a ChatRequest,
        _timeout_seconds: u32,
    ) -> LocalBoxFuture<'a, Result<ChatCompletion, String>> {
        self.model_complete(request)
    }
    fn model_tokens<'a>(
        &'a self,
        _request: &'a ChatRequest,
    ) -> LocalBoxFuture<'a, Result<Option<usize>, String>> {
        Box::pin(async { Ok(None) })
    }
    fn approval_check<'a>(
        &'a self,
        session: i64,
        check: &'a openwebide_core::ApprovalCheck,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ApprovalDecision, String>>;
    fn chat_tools<'a>(
        &'a self,
        request: &'a ChatRequest,
    ) -> LocalBoxFuture<'a, Result<ChatCompletion, String>>;
    fn get_goal(
        &self,
        session: i64,
    ) -> LocalBoxFuture<'_, Result<Option<openwebide_core::Goal>, String>>;
    fn update_goal<'a>(
        &'a self,
        session: i64,
        revision: u64,
        command: &'a openwebide_core::GoalCommand,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::Goal, String>>;
    fn dispatch_goal<'a>(
        &'a self,
        session: i64,
        revision: u64,
        command: &'a openwebide_core::GoalCommand,
        _binding: Option<&'a openwebide_core::scheduled::HostBinding>,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::Goal, String>> {
        self.update_goal(session, revision, command)
    }
    fn compact_session<'a>(
        &'a self,
        session: i64,
        model: Option<&'a str>,
    ) -> LocalBoxFuture<'a, Result<ChatMessage, String>>;
    fn host_connection(
        &self,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::host_admin::HostConnection, String>> {
        Box::pin(async { Err("Host administration unavailable".into()) })
    }
    fn save_host_connection<'a>(
        &'a self,
        _connection: &'a openwebide_core::host_admin::HostConnection,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::host_admin::HostConnection, String>> {
        Box::pin(async { Err("Host administration unavailable".into()) })
    }
    fn probe_host_connection(
        &self,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::host_admin::HostEnvironment, String>> {
        Box::pin(async { Err("Host administration unavailable".into()) })
    }
    fn host_view<'a>(
        &'a self,
        _session: i64,
        _request: &'a openwebide_core::host_admin::HostRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::host_admin::HostResponse, String>> {
        Box::pin(async { Err("Host administration unavailable".into()) })
    }
    fn host_input<'a>(
        &'a self,
        _session: i64,
        _input: &'a openwebide_core::host_admin::HostInput,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err("Host administration unavailable".into()) })
    }
    fn scheduled_tasks(
        &self,
        _project: Option<i64>,
    ) -> LocalBoxFuture<'_, Result<Vec<openwebide_core::scheduled::ScheduledTask>, String>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn scheduled_monitors(
        &self,
        _session: i64,
    ) -> LocalBoxFuture<'_, Result<Vec<openwebide_core::scheduled::ScheduledTask>, String>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn bind_background_host<'a>(
        &'a self,
        _project: i64,
        _binding: &'a openwebide_core::scheduled::HostBinding,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err("Project execution host unavailable".into()) })
    }
    fn scheduled_command<'a>(
        &'a self,
        _project: Option<i64>,
        _command: &'a openwebide_core::scheduled::TaskCommand,
        _binding: Option<&'a openwebide_core::scheduled::HostBinding>,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::scheduled::ScheduledTask>, String>> {
        Box::pin(async { Err("Scheduled tasks unavailable".into()) })
    }
    fn scheduled_session_command<'a>(
        &'a self,
        _session: i64,
        _command: &'a openwebide_core::scheduled::TaskCommand,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::scheduled::ScheduledTask>, String>> {
        Box::pin(async { Err("Scheduled tasks unavailable".into()) })
    }
    fn run_lease<'a>(
        &'a self,
        _session: i64,
        _token: &'a str,
        _release: bool,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn project_skills(
        &self,
        _project: i64,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::ProjectSkills, String>> {
        Box::pin(async { Err("Project skills unavailable".into()) })
    }
    fn plugin_marketplaces<'a>(
        &'a self,
    ) -> LocalBoxFuture<
        'a,
        Result<openwebide_core::plugins::marketplace::MarketplaceSettings, String>,
    > {
        Box::pin(async { Err("Plugin management unavailable".into()) })
    }
    fn save_plugin_marketplaces<'a>(
        &'a self,
        _request: &'a openwebide_core::plugins::marketplace::SaveMarketplaces,
    ) -> LocalBoxFuture<
        'a,
        Result<openwebide_core::plugins::marketplace::MarketplaceSettings, String>,
    > {
        Box::pin(async { Err("Plugin management unavailable".into()) })
    }
    fn refresh_plugin_marketplaces<'a>(
        &'a self,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::plugins::marketplace::MarketplaceRefresh, String>>
    {
        Box::pin(async { Err("Plugin management unavailable".into()) })
    }
    fn project_plugins<'a>(
        &'a self,
        _project: i64,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::plugins::ProjectPlugin>, String>> {
        Box::pin(async { Err("Plugin management unavailable".into()) })
    }
    fn project_plugin_command<'a>(
        &'a self,
        _project: i64,
        _command: &'a openwebide_core::plugins::ProjectPluginCommand,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::plugins::ProjectPlugin>, String>> {
        Box::pin(async { Err("Plugin management unavailable".into()) })
    }
    fn remove_plugin<'a>(
        &'a self,
        _request: &'a openwebide_core::plugins::RemovePlugin,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::plugins::PluginInstallation>, String>> {
        Box::pin(async { Err("Plugin management unavailable".into()) })
    }
    fn plugin_package<'a>(
        &'a self,
        _project: Option<i64>,
        _expected: &'a openwebide_core::plugins::PreparedPlugin,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::plugins::PluginPackage, String>> {
        Box::pin(async { Err("Plugin management unavailable".into()) })
    }
    fn plugin_host_request<'a>(
        &'a self,
        _session: i64,
        _request: &'a openwebide_core::plugins::execution::PluginHostRequest,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(async { Err("Plugin host capability unavailable".into()) })
    }
    fn plugin_execution_grants<'a>(
        &'a self,
        _session: i64,
        _plugins: &'a [openwebide_core::plugins::PreparedPlugin],
    ) -> LocalBoxFuture<'a, Result<std::collections::BTreeMap<String, String>, String>> {
        Box::pin(async { Err("Plugin execution grants unavailable".into()) })
    }
    fn plugin_context_grants<'a>(
        &'a self,
        _request: &'a openwebide_core::plugins::execution::PluginGrantRequest,
    ) -> LocalBoxFuture<'a, Result<std::collections::BTreeMap<String, String>, String>> {
        Box::pin(async { Err("Plugin execution grants unavailable".into()) })
    }
    fn plugin_context_host_request<'a>(
        &'a self,
        _request: &'a openwebide_core::plugins::execution::PluginHostRequest,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(async { Err("Plugin host capability unavailable".into()) })
    }
    fn start_plugin_invocation<'a>(
        &'a self,
        _request: &'a openwebide_core::plugins::execution::PluginStartRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::plugins::execution::PluginInvocation, String>>
    {
        Box::pin(async { Err("Plugin execution host unavailable".into()) })
    }
    fn continue_plugin_invocation<'a>(
        &'a self,
        _request: &'a openwebide_core::plugins::execution::ContinuePlugin,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::plugins::execution::PluginInvocation, String>>
    {
        Box::pin(async { Err("Plugin execution host unavailable".into()) })
    }
    fn cancel_plugin_invocation<'a>(
        &'a self,
        _id: &'a str,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err("Plugin execution host unavailable".into()) })
    }
    fn plugin_installations(
        &self,
    ) -> LocalBoxFuture<'_, Result<Vec<openwebide_core::plugins::PluginInstallation>, String>> {
        Box::pin(async { Err("Plugin installation is unavailable".into()) })
    }
    fn prepare_plugin<'a>(
        &'a self,
        _project: Option<i64>,
        _source: &'a openwebide_core::plugins::PluginSource,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::plugins::PreparedPlugin, String>> {
        Box::pin(async { Err("Plugin installation is unavailable".into()) })
    }
    fn plugin_preparation<'a>(
        &'a self,
        _project: Option<i64>,
        _command: &'a openwebide_core::plugins::preparation::PreparationCommand,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::plugins::preparation::PluginPreparation, String>>
    {
        Box::pin(async { Err("Plugin preparation is unavailable".into()) })
    }
    fn record_plugin<'a>(
        &'a self,
        _request: &'a openwebide_core::plugins::RecordPlugin,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::plugins::PluginInstallation>, String>> {
        Box::pin(async { Err("Plugin installation is unavailable".into()) })
    }
    fn session_skills(
        &self,
        _session: i64,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::ProjectSkills, String>> {
        Box::pin(async { Err("Project skills unavailable".into()) })
    }
    fn skill_command<'a>(
        &'a self,
        _id: i64,
        _command: &'a openwebide_core::SkillCommand,
        _session: bool,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ProjectSkills, String>> {
        Box::pin(async { Err("Project skills unavailable".into()) })
    }
    fn project_memories(
        &self,
        _project: i64,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::ProjectMemories, String>> {
        Box::pin(async { Err("Project memory unavailable".into()) })
    }
    fn session_memories(
        &self,
        _session: i64,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::ProjectMemories, String>> {
        Box::pin(async { Err("Project memory unavailable".into()) })
    }
    fn memory_command<'a>(
        &'a self,
        _id: i64,
        _command: &'a openwebide_core::MemoryCommand,
        _session: bool,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ProjectMemories, String>> {
        Box::pin(async { Err("Project memory unavailable".into()) })
    }
    fn question_command<'a>(
        &'a self,
        _session: i64,
        _command: &'a openwebide_core::questions::QuestionCommand,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::questions::QuestionResult, String>> {
        Box::pin(async { Err("Questions unavailable".into()) })
    }
    fn get_todo_plan(
        &self,
        session: i64,
    ) -> LocalBoxFuture<'_, Result<Option<openwebide_core::TodoUpdate>, String>>;
    fn write_todo_plan<'a>(
        &'a self,
        session: i64,
        anchor: i64,
        plan: &'a openwebide_core::TodoPlan,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::TodoUpdate, String>>;
    fn fork_session<'a>(
        &'a self,
        session: i64,
        message: i64,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ForkedSession, String>>;
    fn list_queued_prompts<'a>(
        &'a self,
        session: i64,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::QueuedPrompt>, String>>;
    fn enqueue_prompt<'a>(
        &'a self,
        session: i64,
        content: &'a str,
        guidance: bool,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::QueuedPrompt, String>>;
    fn update_queued_prompt<'a>(
        &'a self,
        session: i64,
        key: openwebide_core::QueuedPromptKey,
        content: &'a str,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::QueuedPrompt, String>>;
    fn remove_queued_prompt<'a>(
        &'a self,
        session: i64,
        key: openwebide_core::QueuedPromptKey,
    ) -> LocalBoxFuture<'a, Result<(), String>>;
    fn consume_queued_prompt<'a>(
        &'a self,
        session: i64,
        key: openwebide_core::QueuedPromptKey,
        content: &'a str,
    ) -> LocalBoxFuture<'a, Result<ChatMessage, String>>;
    fn persist_message<'a>(
        &'a self,
        session_id: i64,
        role: Role,
        content: &'a str,
        usage: Option<&'a TurnTelemetry>,
        tool_calls: Option<&'a [openwebide_core::ToolCall]>,
    ) -> LocalBoxFuture<'a, Result<ChatMessage, String>>;
    fn model_context<'a>(
        &'a self,
        connection_id: i64,
        model: Option<&'a str>,
    ) -> LocalBoxFuture<'a, Result<Option<usize>, String>>;
    #[allow(clippy::too_many_arguments)]
    fn upsert_tool_step<'a>(
        &'a self,
        session_id: i64,
        anchor_message_id: i64,
        tool_call_id: &'a str,
        name: &'a str,
        summary: &'a str,
        diff: Option<&'a FileDiff>,
    ) -> LocalBoxFuture<'a, Result<(), String>>;
    fn save_task<'a>(
        &'a self,
        _session: i64,
        _anchor: i64,
        _snapshot: &'a openwebide_core::TaskSnapshot,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err("Child task persistence unavailable".into()) })
    }
    fn save_tool_timing<'a>(
        &'a self,
        session: i64,
        id: &'a str,
        timing: &'a openwebide_core::ToolTiming,
    ) -> LocalBoxFuture<'a, Result<(), String>>;
    fn save_project_checkpoint<'a>(
        &'a self,
        _session: i64,
        _id: &'a str,
        _checkpoint: &'a openwebide_core::rewind::ProjectCheckpoint,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn complete_tool_step<'a>(
        &'a self,
        session_id: i64,
        tool_call_id: &'a str,
        ok: bool,
        result_summary: &'a str,
        diff: Option<&'a FileDiff>,
    ) -> LocalBoxFuture<'a, Result<(), String>>;
    fn list_pending_edits(
        &self,
        project_id: i64,
    ) -> LocalBoxFuture<'_, Result<Vec<PersistedEdit>, String>>;
    fn resolve_pending_edit<'a>(
        &'a self,
        project_id: i64,
        request: &'a ResolveEditRequest,
    ) -> LocalBoxFuture<'a, Result<PersistedEdit, String>>;
    fn web_search<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
    ) -> LocalBoxFuture<'a, Result<Vec<WebSearchResult>, String>>;
    fn fetch_web_page<'a>(
        &'a self,
        target_url: &'a str,
    ) -> LocalBoxFuture<'a, Result<String, String>>;
    #[allow(
        clippy::too_many_arguments,
        reason = "Streaming transport carries prompt delivery metadata and callbacks"
    )]
    fn send_message<'a>(
        &'a self,
        session_id: i64,
        content: &'a str,
        model: Option<&'a str>,
        editor_context: Option<&'a EditorContext>,
        browser_preferences: Option<&'a openwebide_core::BrowserPreferences>,
        queued_prompt: Option<openwebide_core::QueuedPromptKey>,
        signal: Option<&'a AbortSignal>,
        on_event: Box<dyn FnMut(RunEvent) + 'a>,
    ) -> LocalBoxFuture<'a, Result<(), String>>;
}

impl Backend for BackendApi {
    fn host_connection(
        &self,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::host_admin::HostConnection, String>> {
        Box::pin(BackendApi::host_connection(self))
    }
    fn save_host_connection<'a>(
        &'a self,
        connection: &'a openwebide_core::host_admin::HostConnection,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::host_admin::HostConnection, String>> {
        Box::pin(BackendApi::save_host_connection(self, connection))
    }
    fn probe_host_connection(
        &self,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::host_admin::HostEnvironment, String>> {
        Box::pin(BackendApi::probe_host_connection(self))
    }
    fn host_view<'a>(
        &'a self,
        session: i64,
        request: &'a openwebide_core::host_admin::HostRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::host_admin::HostResponse, String>> {
        Box::pin(BackendApi::host_view(self, session, request))
    }
    fn host_input<'a>(
        &'a self,
        session: i64,
        input: &'a openwebide_core::host_admin::HostInput,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::host_input(self, session, input))
    }

    fn session_expired(&self) -> RwSignal<bool> {
        self.session_expired
    }
    fn register<'a>(
        &'a self,
        username: &'a str,
        password: &'a str,
    ) -> LocalBoxFuture<'a, Result<User, String>> {
        Box::pin(BackendApi::register(self, username, password))
    }
    fn login<'a>(
        &'a self,
        username: &'a str,
        password: &'a str,
    ) -> LocalBoxFuture<'a, Result<User, String>> {
        Box::pin(BackendApi::login(self, username, password))
    }
    fn me<'a>(&'a self) -> LocalBoxFuture<'a, Result<User, String>> {
        Box::pin(BackendApi::me(self))
    }
    fn logout<'a>(&'a self) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::logout(self))
    }
    fn bridge_token<'a>(&'a self) -> LocalBoxFuture<'a, Result<(String, i64), String>> {
        Box::pin(BackendApi::bridge_token(self))
    }
    fn health<'a>(&'a self) -> LocalBoxFuture<'a, Result<Health, String>> {
        Box::pin(BackendApi::health(self))
    }
    fn preview_server<'a>(
        &'a self,
        probe: &'a openwebide_core::ModelProbe,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ServerDiscovery, String>> {
        Box::pin(BackendApi::preview_server(self, probe))
    }
    fn save_model_setup<'a>(
        &'a self,
        probe: &'a openwebide_core::ModelProbe,
        profiles: &'a [openwebide_core::ModelProfile],
    ) -> LocalBoxFuture<'a, Result<(Connection, openwebide_core::ModelSetup), String>> {
        Box::pin(BackendApi::save_model_setup(self, probe, profiles))
    }
    fn test_model<'a>(
        &'a self,
        probe: &'a openwebide_core::ModelProbe,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ModelTestResult, String>> {
        Box::pin(BackendApi::test_model(self, probe))
    }
    fn preview_model<'a>(
        &'a self,
        probe: &'a openwebide_core::ModelProbe,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ModelDetection, String>> {
        Box::pin(BackendApi::preview_model(self, probe))
    }
    fn detect_model<'a>(
        &'a self,
        id: i64,
        model: &'a str,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ModelDetection, String>> {
        Box::pin(BackendApi::detect_model(self, id, model))
    }
    fn inspect_server<'a>(
        &'a self,
        base_url: &'a str,
        kind: Option<ProviderKind>,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ServerDiscovery, String>> {
        Box::pin(BackendApi::inspect_server(self, base_url, kind))
    }
    fn discover_servers<'a>(
        &'a self,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::ServerDiscovery>, String>> {
        Box::pin(BackendApi::discover_servers(self))
    }
    fn model_runtime<'a>(
        &'a self,
        id: i64,
        model: Option<&'a str>,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ModelRuntime, String>> {
        Box::pin(BackendApi::model_runtime(self, id, model))
    }
    fn model_setup<'a>(
        &'a self,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ModelSetup, String>> {
        Box::pin(BackendApi::model_setup(self))
    }
    fn save_model_defaults<'a>(
        &'a self,
        defaults: &'a openwebide_core::ModelDefaults,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ModelSetup, String>> {
        Box::pin(BackendApi::save_model_defaults(self, defaults))
    }
    fn save_model_profile<'a>(
        &'a self,
        profile: &'a openwebide_core::ModelProfile,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ModelSetup, String>> {
        Box::pin(BackendApi::save_model_profile(self, profile))
    }
    fn server_settings<'a>(
        &'a self,
        id: i64,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ServerSettings, String>> {
        Box::pin(BackendApi::server_settings(self, id))
    }
    fn save_server_settings<'a>(
        &'a self,
        id: i64,
        update: &'a openwebide_core::ServerSettingsUpdate,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ServerSettings, String>> {
        Box::pin(BackendApi::save_server_settings(self, id, update))
    }
    fn list_connections<'a>(&'a self) -> LocalBoxFuture<'a, Result<Vec<Connection>, String>> {
        Box::pin(BackendApi::list_connections(self))
    }
    fn create_connection<'a>(
        &'a self,
        name: &'a str,
        kind: ProviderKind,
        base_url: &'a str,
        model: Option<&'a str>,
        context_limit: Option<usize>,
    ) -> LocalBoxFuture<'a, Result<Connection, String>> {
        Box::pin(BackendApi::create_connection(
            self,
            name,
            kind,
            base_url,
            model,
            context_limit,
        ))
    }
    fn update_connection<'a>(
        &'a self,
        connection: &'a Connection,
    ) -> LocalBoxFuture<'a, Result<Connection, String>> {
        Box::pin(BackendApi::update_connection(self, connection))
    }
    fn delete_connection<'a>(&'a self, id: i64) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::delete_connection(self, id))
    }
    fn list_sessions<'a>(&'a self) -> LocalBoxFuture<'a, Result<Vec<ChatSession>, String>> {
        Box::pin(BackendApi::list_sessions(self))
    }
    fn search_sessions<'a>(
        &'a self,
        search: &'a openwebide_core::SessionSearch,
    ) -> LocalBoxFuture<'a, Result<Vec<ChatSession>, String>> {
        Box::pin(BackendApi::search_sessions(self, search))
    }
    fn session_preferences<'a>(
        &'a self,
        id: i64,
        preferences: &'a openwebide_core::SessionPreferences,
    ) -> LocalBoxFuture<'a, Result<ChatSession, String>> {
        Box::pin(BackendApi::session_preferences(self, id, preferences))
    }
    fn session_title<'a>(
        &'a self,
        id: i64,
    ) -> LocalBoxFuture<'a, Result<Option<ChatSession>, String>> {
        Box::pin(BackendApi::session_title(self, id))
    }
    fn export_session<'a>(
        &'a self,
        id: i64,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::SessionExport, String>> {
        Box::pin(BackendApi::export_session(self, id))
    }
    fn list_models<'a>(
        &'a self,
        connection_id: i64,
    ) -> LocalBoxFuture<'a, Result<Vec<ModelInfo>, String>> {
        Box::pin(BackendApi::list_models(self, connection_id))
    }
    fn list_system_prompts<'a>(&'a self) -> LocalBoxFuture<'a, Result<Vec<SystemPrompt>, String>> {
        Box::pin(BackendApi::list_system_prompts(self))
    }
    fn create_system_prompt<'a>(
        &'a self,
        name: &'a str,
        content: &'a str,
    ) -> LocalBoxFuture<'a, Result<SystemPrompt, String>> {
        Box::pin(BackendApi::create_system_prompt(self, name, content))
    }
    fn update_system_prompt<'a>(
        &'a self,
        id: i64,
        name: &'a str,
        content: &'a str,
    ) -> LocalBoxFuture<'a, Result<SystemPrompt, String>> {
        Box::pin(BackendApi::update_system_prompt(self, id, name, content))
    }
    fn delete_system_prompt<'a>(&'a self, id: i64) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::delete_system_prompt(self, id))
    }
    fn editor_recovery(
        &self,
        project: i64,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::editor::EditorRecoveryRecord, RecoveryError>>
    {
        Box::pin(BackendApi::editor_recovery(self, project))
    }
    fn save_editor_recovery<'a>(
        &'a self,
        project: i64,
        record: &'a openwebide_core::editor::EditorRecoveryRecord,
    ) -> LocalBoxFuture<'a, Result<i64, RecoveryError>> {
        Box::pin(BackendApi::save_editor_recovery(self, project, record))
    }
    fn push_config(&self) -> LocalBoxFuture<'_, Result<openwebide_core::push::PushConfig, String>> {
        Box::pin(BackendApi::push_config(self))
    }
    fn save_push_subscription<'a>(
        &'a self,
        subscription: &'a openwebide_core::push::PushSubscription,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::save_push_subscription(self, subscription))
    }
    fn get_settings<'a>(
        &'a self,
    ) -> LocalBoxFuture<'a, Result<std::collections::BTreeMap<String, String>, String>> {
        Box::pin(BackendApi::get_settings(self))
    }
    fn set_setting<'a>(
        &'a self,
        key: &'a str,
        value: &'a str,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::set_setting(self, key, value))
    }
    fn startup_context(
        &self,
        project: i64,
        tools: bool,
        connection: Option<i64>,
    ) -> LocalBoxFuture<'_, Result<String, String>> {
        Box::pin(BackendApi::startup_context(
            self, project, tools, connection,
        ))
    }
    fn create_session<'a>(
        &'a self,
        name: &'a str,
        connection_id: Option<i64>,
        system_prompt_id: Option<i64>,
        project_id: Option<i64>,
    ) -> LocalBoxFuture<'a, Result<ChatSession, String>> {
        Box::pin(BackendApi::create_session(
            self,
            name,
            connection_id,
            system_prompt_id,
            project_id,
        ))
    }
    fn list_projects<'a>(&'a self) -> LocalBoxFuture<'a, Result<Vec<Project>, String>> {
        Box::pin(BackendApi::list_projects(self))
    }
    fn create_project<'a>(
        &'a self,
        name: &'a str,
        mode: WorkspaceMode,
        path: Option<String>,
    ) -> LocalBoxFuture<'a, Result<Project, String>> {
        Box::pin(BackendApi::create_project(self, name, mode, path))
    }
    fn rename_project<'a>(
        &'a self,
        id: i64,
        name: &'a str,
    ) -> LocalBoxFuture<'a, Result<Project, String>> {
        Box::pin(BackendApi::rename_project(self, id, name))
    }
    fn delete_project<'a>(&'a self, id: i64) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::delete_project(self, id))
    }
    fn list_files<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<Vec<FileEntry>, String>> {
        Box::pin(BackendApi::list_files(self, project_id, path))
    }
    fn read_file_object_url<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(BackendApi::read_file_object_url(self, project_id, path))
    }
    fn read_file<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(BackendApi::read_file(self, project_id, path))
    }
    fn canonical_file_path<'a>(
        &'a self,
        project: i64,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(BackendApi::canonical_file_path(self, project, path))
    }
    fn read_file_bytes<'a>(
        &'a self,
        project: i64,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<Vec<u8>, String>> {
        Box::pin(BackendApi::read_file_bytes(self, project, path))
    }
    fn read_file_lossy<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(BackendApi::read_file_lossy(self, project_id, path))
    }
    fn write_file<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
        content: &'a str,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::write_file(self, project_id, path, content))
    }
    fn write_file_bytes<'a>(
        &'a self,
        project: i64,
        path: &'a str,
        bytes: &'a [u8],
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::write_file_bytes(self, project, path, bytes))
    }
    fn copy_file<'a>(
        &'a self,
        project_id: i64,
        from: &'a str,
        to: &'a str,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::copy_file(self, project_id, from, to))
    }
    fn create_file<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
        is_dir: bool,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::create_file(self, project_id, path, is_dir))
    }
    fn delete_file<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::delete_file(self, project_id, path))
    }
    fn browse<'a>(&'a self, path: &'a str) -> LocalBoxFuture<'a, Result<Vec<FileEntry>, String>> {
        Box::pin(BackendApi::browse(self, path))
    }
    fn search_content<'a>(
        &'a self,
        project_id: i64,
        query: &'a str,
        path: &'a str,
        opts: SearchOptions,
    ) -> LocalBoxFuture<'a, Result<Vec<SearchHit>, String>> {
        Box::pin(BackendApi::search_content(
            self, project_id, query, path, opts,
        ))
    }
    fn git_status<'a>(
        &'a self,
        project_id: Option<i64>,
    ) -> LocalBoxFuture<'a, Result<GitRepoStatus, String>> {
        Box::pin(BackendApi::git_status(self, project_id))
    }
    fn git_diff<'a>(
        &'a self,
        project_id: Option<i64>,
        path: Option<&'a str>,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(BackendApi::git_diff(self, project_id, path))
    }
    fn git_file_head<'a>(
        &'a self,
        project_id: Option<i64>,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(BackendApi::git_file_head(self, project_id, path))
    }
    fn git_stash<'a>(
        &'a self,
        project_id: Option<i64>,
        request: &'a openwebide_core::git::GitStashRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::git::GitStashResult, String>> {
        Box::pin(BackendApi::git_stash(self, project_id, request))
    }
    fn git_index_diff(
        &self,
        project_id: Option<i64>,
    ) -> LocalBoxFuture<'_, Result<String, String>> {
        Box::pin(BackendApi::git_index_diff(self, project_id))
    }
    fn git_history<'a>(
        &'a self,
        project_id: Option<i64>,
        request: &'a openwebide_core::git::GitHistoryRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::git::GitHistoryPage, String>> {
        Box::pin(BackendApi::git_history(self, project_id, request))
    }
    fn git_commit_diff<'a>(
        &'a self,
        project_id: Option<i64>,
        request: &'a openwebide_core::git::GitCommitDiffRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::git::GitCommitDiff, String>> {
        Box::pin(BackendApi::git_commit_diff(self, project_id, request))
    }
    fn git_branches<'a>(
        &'a self,
        project_id: Option<i64>,
    ) -> LocalBoxFuture<'a, Result<Vec<GitBranchInfo>, String>> {
        Box::pin(BackendApi::git_branches(self, project_id))
    }
    fn git_path_changes(
        &self,
        project_id: Option<i64>,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::git::GitPathChanges, String>> {
        Box::pin(BackendApi::git_path_changes(self, project_id))
    }
    fn git_path_action<'a>(
        &'a self,
        project_id: Option<i64>,
        request: &'a openwebide_core::git::GitPathRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::git::GitPathChanges, String>> {
        Box::pin(BackendApi::git_path_action(self, project_id, request))
    }
    fn git_commit<'a>(
        &'a self,
        project_id: Option<i64>,
        req: &'a GitCommitRequest,
    ) -> LocalBoxFuture<'a, Result<GitCommitResult, String>> {
        Box::pin(BackendApi::git_commit(self, project_id, req))
    }
    fn git_checkout<'a>(
        &'a self,
        project_id: Option<i64>,
        req: &'a GitCheckoutRequest,
    ) -> LocalBoxFuture<'a, Result<GitCheckoutResult, String>> {
        Box::pin(BackendApi::git_checkout(self, project_id, req))
    }
    fn git_sync<'a>(
        &'a self,
        project_id: Option<i64>,
        req: &'a GitSyncRequest,
    ) -> LocalBoxFuture<'a, Result<GitSyncResult, String>> {
        Box::pin(BackendApi::git_sync(self, project_id, req))
    }
    fn set_session_connection(
        &self,
        id: i64,
        connection_id: i64,
    ) -> LocalBoxFuture<'_, Result<ChatSession, String>> {
        Box::pin(BackendApi::set_session_connection(self, id, connection_id))
    }
    fn rename_session<'a>(
        &'a self,
        id: i64,
        name: &'a str,
    ) -> LocalBoxFuture<'a, Result<ChatSession, String>> {
        Box::pin(BackendApi::rename_session(self, id, name))
    }
    fn delete_session<'a>(&'a self, id: i64) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::delete_session(self, id))
    }
    fn cancel_session<'a>(&'a self, session_id: i64) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::cancel_session(self, session_id))
    }
    fn set_permission<'a>(
        &'a self,
        session_id: i64,
        tool_call_id: &'a str,
        approved: bool,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::set_permission(
            self,
            session_id,
            tool_call_id,
            approved,
        ))
    }
    fn list_messages<'a>(
        &'a self,
        session_id: i64,
    ) -> LocalBoxFuture<'a, Result<Vec<ConversationEntry>, String>> {
        Box::pin(BackendApi::list_messages(self, session_id))
    }
    fn session_search_suggestions<'a>(
        &'a self,
        search: &'a openwebide_core::SessionSearch,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::SessionSearchResults, String>> {
        Box::pin(BackendApi::session_search_suggestions(self, search))
    }
    fn staged_assistance<'a>(
        &'a self,
        request: &'a openwebide_core::AssistanceRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::assistance::GitDraftResult, String>> {
        Box::pin(BackendApi::staged_assistance(self, request))
    }
    fn assistance<'a>(
        &'a self,
        request: &'a openwebide_core::AssistanceRequest,
    ) -> LocalBoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(BackendApi::assistance(self, request))
    }
    fn model_complete<'a>(
        &'a self,
        request: &'a ChatRequest,
    ) -> LocalBoxFuture<'a, Result<ChatCompletion, String>> {
        Box::pin(BackendApi::model_complete(self, request))
    }
    fn model_complete_with_timeout<'a>(
        &'a self,
        request: &'a ChatRequest,
        timeout_seconds: u32,
    ) -> LocalBoxFuture<'a, Result<ChatCompletion, String>> {
        Box::pin(BackendApi::model_complete_with_timeout(
            self,
            request,
            timeout_seconds,
        ))
    }
    fn model_tokens<'a>(
        &'a self,
        request: &'a ChatRequest,
    ) -> LocalBoxFuture<'a, Result<Option<usize>, String>> {
        Box::pin(BackendApi::model_tokens(self, request))
    }
    fn list_run_changes(
        &self,
        project: i64,
    ) -> LocalBoxFuture<'_, Result<Vec<openwebide_core::RunChange>, String>> {
        Box::pin(BackendApi::list_run_changes(self, project))
    }
    fn preview_run_review<'a>(
        &'a self,
        project: i64,
        request: &'a openwebide_core::ReviewRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ReviewPlan, String>> {
        Box::pin(BackendApi::preview_run_review(self, project, request))
    }
    fn prepare_run_review<'a>(
        &'a self,
        project: i64,
        request: &'a openwebide_core::ReviewRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ReviewPlan, String>> {
        Box::pin(BackendApi::prepare_run_review(self, project, request))
    }
    fn complete_run_review<'a>(
        &'a self,
        project: i64,
        request: &'a openwebide_core::ReviewRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::RunChange, String>> {
        Box::pin(BackendApi::complete_run_review(self, project, request))
    }
    fn prepare_rewind(
        &self,
        session: i64,
        message: i64,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::RewindPlan, String>> {
        Box::pin(BackendApi::prepare_rewind(self, session, message))
    }
    fn complete_rewind(
        &self,
        session: i64,
        message: i64,
    ) -> LocalBoxFuture<'_, Result<Vec<ConversationEntry>, String>> {
        Box::pin(BackendApi::complete_rewind(self, session, message))
    }
    fn approval_check<'a>(
        &'a self,
        session: i64,
        check: &'a openwebide_core::ApprovalCheck,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ApprovalDecision, String>> {
        Box::pin(BackendApi::approval_check(self, session, check))
    }
    fn chat_tools<'a>(
        &'a self,
        request: &'a ChatRequest,
    ) -> LocalBoxFuture<'a, Result<ChatCompletion, String>> {
        Box::pin(BackendApi::chat_tools(self, request))
    }
    fn get_goal(
        &self,
        session: i64,
    ) -> LocalBoxFuture<'_, Result<Option<openwebide_core::Goal>, String>> {
        Box::pin(BackendApi::get_goal(self, session))
    }
    fn update_goal<'a>(
        &'a self,
        session: i64,
        revision: u64,
        command: &'a openwebide_core::GoalCommand,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::Goal, String>> {
        Box::pin(BackendApi::update_goal(self, session, revision, command))
    }
    fn dispatch_goal<'a>(
        &'a self,
        session: i64,
        revision: u64,
        command: &'a openwebide_core::GoalCommand,
        binding: Option<&'a openwebide_core::scheduled::HostBinding>,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::Goal, String>> {
        Box::pin(BackendApi::dispatch_goal(
            self, session, revision, command, binding,
        ))
    }
    fn compact_session<'a>(
        &'a self,
        session: i64,
        model: Option<&'a str>,
    ) -> LocalBoxFuture<'a, Result<ChatMessage, String>> {
        Box::pin(BackendApi::compact_session(self, session, model))
    }
    fn scheduled_tasks(
        &self,
        project: Option<i64>,
    ) -> LocalBoxFuture<'_, Result<Vec<openwebide_core::scheduled::ScheduledTask>, String>> {
        Box::pin(BackendApi::scheduled_tasks(self, project))
    }
    fn scheduled_monitors(
        &self,
        session: i64,
    ) -> LocalBoxFuture<'_, Result<Vec<openwebide_core::scheduled::ScheduledTask>, String>> {
        Box::pin(BackendApi::scheduled_monitors(self, session))
    }
    fn bind_background_host<'a>(
        &'a self,
        project: i64,
        binding: &'a openwebide_core::scheduled::HostBinding,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::bind_background_host(self, project, binding))
    }
    fn scheduled_command<'a>(
        &'a self,
        project: Option<i64>,
        command: &'a openwebide_core::scheduled::TaskCommand,
        binding: Option<&'a openwebide_core::scheduled::HostBinding>,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::scheduled::ScheduledTask>, String>> {
        Box::pin(BackendApi::scheduled_command(
            self, project, command, binding,
        ))
    }
    fn scheduled_session_command<'a>(
        &'a self,
        session: i64,
        command: &'a openwebide_core::scheduled::TaskCommand,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::scheduled::ScheduledTask>, String>> {
        Box::pin(BackendApi::scheduled_session_command(
            self, session, command,
        ))
    }
    fn run_lease<'a>(
        &'a self,
        session: i64,
        token: &'a str,
        release: bool,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::run_lease(self, session, token, release))
    }
    fn project_skills(
        &self,
        project: i64,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::ProjectSkills, String>> {
        Box::pin(BackendApi::skills(self, project, false))
    }
    fn plugin_marketplaces<'a>(
        &'a self,
    ) -> LocalBoxFuture<
        'a,
        Result<openwebide_core::plugins::marketplace::MarketplaceSettings, String>,
    > {
        Box::pin(BackendApi::plugin_marketplaces(self))
    }
    fn save_plugin_marketplaces<'a>(
        &'a self,
        request: &'a openwebide_core::plugins::marketplace::SaveMarketplaces,
    ) -> LocalBoxFuture<
        'a,
        Result<openwebide_core::plugins::marketplace::MarketplaceSettings, String>,
    > {
        Box::pin(BackendApi::save_plugin_marketplaces(self, request))
    }
    fn refresh_plugin_marketplaces<'a>(
        &'a self,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::plugins::marketplace::MarketplaceRefresh, String>>
    {
        Box::pin(BackendApi::refresh_plugin_marketplaces(self))
    }
    fn project_plugins<'a>(
        &'a self,
        project: i64,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::plugins::ProjectPlugin>, String>> {
        Box::pin(BackendApi::project_plugins(self, project))
    }
    fn project_plugin_command<'a>(
        &'a self,
        project: i64,
        command: &'a openwebide_core::plugins::ProjectPluginCommand,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::plugins::ProjectPlugin>, String>> {
        Box::pin(BackendApi::project_plugin_command(self, project, command))
    }
    fn remove_plugin<'a>(
        &'a self,
        request: &'a openwebide_core::plugins::RemovePlugin,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::plugins::PluginInstallation>, String>> {
        Box::pin(BackendApi::remove_plugin(self, request))
    }
    fn plugin_package<'a>(
        &'a self,
        project: Option<i64>,
        expected: &'a openwebide_core::plugins::PreparedPlugin,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::plugins::PluginPackage, String>> {
        Box::pin(BackendApi::plugin_package(self, project, expected))
    }
    fn plugin_host_request<'a>(
        &'a self,
        session: i64,
        request: &'a openwebide_core::plugins::execution::PluginHostRequest,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(BackendApi::plugin_host_request(self, session, request))
    }
    fn plugin_execution_grants<'a>(
        &'a self,
        session: i64,
        plugins: &'a [openwebide_core::plugins::PreparedPlugin],
    ) -> LocalBoxFuture<'a, Result<std::collections::BTreeMap<String, String>, String>> {
        Box::pin(BackendApi::plugin_execution_grants(self, session, plugins))
    }
    fn plugin_context_grants<'a>(
        &'a self,
        request: &'a openwebide_core::plugins::execution::PluginGrantRequest,
    ) -> LocalBoxFuture<'a, Result<std::collections::BTreeMap<String, String>, String>> {
        Box::pin(BackendApi::plugin_context_grants(self, request))
    }
    fn plugin_context_host_request<'a>(
        &'a self,
        request: &'a openwebide_core::plugins::execution::PluginHostRequest,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(BackendApi::plugin_context_host_request(self, request))
    }
    fn start_plugin_invocation<'a>(
        &'a self,
        request: &'a openwebide_core::plugins::execution::PluginStartRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::plugins::execution::PluginInvocation, String>>
    {
        Box::pin(BackendApi::start_plugin_invocation(self, request))
    }
    fn continue_plugin_invocation<'a>(
        &'a self,
        request: &'a openwebide_core::plugins::execution::ContinuePlugin,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::plugins::execution::PluginInvocation, String>>
    {
        Box::pin(BackendApi::continue_plugin_invocation(self, request))
    }
    fn cancel_plugin_invocation<'a>(
        &'a self,
        id: &'a str,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::cancel_plugin_invocation(self, id))
    }
    fn plugin_installations(
        &self,
    ) -> LocalBoxFuture<'_, Result<Vec<openwebide_core::plugins::PluginInstallation>, String>> {
        Box::pin(BackendApi::plugin_installations(self))
    }
    fn prepare_plugin<'a>(
        &'a self,
        project: Option<i64>,
        source: &'a openwebide_core::plugins::PluginSource,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::plugins::PreparedPlugin, String>> {
        Box::pin(BackendApi::prepare_plugin(self, project, source))
    }
    fn plugin_preparation<'a>(
        &'a self,
        project: Option<i64>,
        command: &'a openwebide_core::plugins::preparation::PreparationCommand,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::plugins::preparation::PluginPreparation, String>>
    {
        Box::pin(BackendApi::plugin_preparation(self, project, command))
    }
    fn record_plugin<'a>(
        &'a self,
        request: &'a openwebide_core::plugins::RecordPlugin,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::plugins::PluginInstallation>, String>> {
        Box::pin(BackendApi::record_plugin(self, request))
    }
    fn session_skills(
        &self,
        session: i64,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::ProjectSkills, String>> {
        Box::pin(BackendApi::skills(self, session, true))
    }
    fn skill_command<'a>(
        &'a self,
        id: i64,
        command: &'a openwebide_core::SkillCommand,
        session: bool,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ProjectSkills, String>> {
        Box::pin(BackendApi::skill_command(self, id, command, session))
    }
    fn project_memories(
        &self,
        project: i64,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::ProjectMemories, String>> {
        Box::pin(BackendApi::memories(self, project, false))
    }
    fn session_memories(
        &self,
        session: i64,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::ProjectMemories, String>> {
        Box::pin(BackendApi::memories(self, session, true))
    }
    fn memory_command<'a>(
        &'a self,
        id: i64,
        command: &'a openwebide_core::MemoryCommand,
        session: bool,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ProjectMemories, String>> {
        Box::pin(BackendApi::memory_command(self, id, command, session))
    }
    fn question_command<'a>(
        &'a self,
        session: i64,
        command: &'a openwebide_core::questions::QuestionCommand,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::questions::QuestionResult, String>> {
        Box::pin(BackendApi::question_command(self, session, command))
    }
    fn get_todo_plan(
        &self,
        session: i64,
    ) -> LocalBoxFuture<'_, Result<Option<openwebide_core::TodoUpdate>, String>> {
        Box::pin(BackendApi::get_todo_plan(self, session))
    }
    fn write_todo_plan<'a>(
        &'a self,
        session: i64,
        anchor: i64,
        plan: &'a openwebide_core::TodoPlan,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::TodoUpdate, String>> {
        Box::pin(BackendApi::write_todo_plan(self, session, anchor, plan))
    }
    fn fork_session<'a>(
        &'a self,
        session: i64,
        message: i64,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ForkedSession, String>> {
        Box::pin(BackendApi::fork_session(self, session, message))
    }
    fn list_queued_prompts<'a>(
        &'a self,
        session: i64,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::QueuedPrompt>, String>> {
        Box::pin(BackendApi::list_queued_prompts(self, session))
    }
    fn enqueue_prompt<'a>(
        &'a self,
        session: i64,
        content: &'a str,
        guidance: bool,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::QueuedPrompt, String>> {
        Box::pin(BackendApi::enqueue_prompt(self, session, content, guidance))
    }
    fn update_queued_prompt<'a>(
        &'a self,
        session: i64,
        key: openwebide_core::QueuedPromptKey,
        content: &'a str,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::QueuedPrompt, String>> {
        Box::pin(BackendApi::update_queued_prompt(
            self, session, key, content,
        ))
    }
    fn remove_queued_prompt<'a>(
        &'a self,
        session: i64,
        key: openwebide_core::QueuedPromptKey,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::remove_queued_prompt(self, session, key))
    }
    fn consume_queued_prompt<'a>(
        &'a self,
        session: i64,
        key: openwebide_core::QueuedPromptKey,
        content: &'a str,
    ) -> LocalBoxFuture<'a, Result<ChatMessage, String>> {
        Box::pin(BackendApi::consume_queued_prompt(
            self, session, key, content,
        ))
    }
    fn persist_message<'a>(
        &'a self,
        session_id: i64,
        role: Role,
        content: &'a str,
        usage: Option<&'a TurnTelemetry>,
        tool_calls: Option<&'a [openwebide_core::ToolCall]>,
    ) -> LocalBoxFuture<'a, Result<ChatMessage, String>> {
        Box::pin(BackendApi::persist_message(
            self, session_id, role, content, usage, tool_calls,
        ))
    }
    fn model_context<'a>(
        &'a self,
        connection_id: i64,
        model: Option<&'a str>,
    ) -> LocalBoxFuture<'a, Result<Option<usize>, String>> {
        Box::pin(BackendApi::model_context(self, connection_id, model))
    }
    #[allow(clippy::too_many_arguments)]
    fn upsert_tool_step<'a>(
        &'a self,
        session_id: i64,
        anchor_message_id: i64,
        tool_call_id: &'a str,
        name: &'a str,
        summary: &'a str,
        diff: Option<&'a FileDiff>,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::upsert_tool_step(
            self,
            session_id,
            anchor_message_id,
            tool_call_id,
            name,
            summary,
            diff,
        ))
    }
    fn save_task<'a>(
        &'a self,
        session: i64,
        anchor: i64,
        snapshot: &'a openwebide_core::TaskSnapshot,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::save_task(self, session, anchor, snapshot))
    }
    fn save_tool_timing<'a>(
        &'a self,
        session: i64,
        id: &'a str,
        timing: &'a openwebide_core::ToolTiming,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::save_tool_timing(self, session, id, timing))
    }
    fn save_project_checkpoint<'a>(
        &'a self,
        session: i64,
        id: &'a str,
        checkpoint: &'a openwebide_core::rewind::ProjectCheckpoint,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::save_project_checkpoint(
            self, session, id, checkpoint,
        ))
    }
    fn complete_tool_step<'a>(
        &'a self,
        session_id: i64,
        tool_call_id: &'a str,
        ok: bool,
        result_summary: &'a str,
        diff: Option<&'a FileDiff>,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::complete_tool_step(
            self,
            session_id,
            tool_call_id,
            ok,
            result_summary,
            diff,
        ))
    }
    fn list_pending_edits(
        &self,
        project_id: i64,
    ) -> LocalBoxFuture<'_, Result<Vec<PersistedEdit>, String>> {
        Box::pin(BackendApi::list_pending_edits(self, project_id))
    }
    fn resolve_pending_edit<'a>(
        &'a self,
        project_id: i64,
        request: &'a ResolveEditRequest,
    ) -> LocalBoxFuture<'a, Result<PersistedEdit, String>> {
        Box::pin(BackendApi::resolve_pending_edit(self, project_id, request))
    }
    fn web_search<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
    ) -> LocalBoxFuture<'a, Result<Vec<WebSearchResult>, String>> {
        Box::pin(BackendApi::web_search(self, query, limit))
    }
    fn fetch_web_page<'a>(
        &'a self,
        target_url: &'a str,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(BackendApi::fetch_web_page(self, target_url))
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "Streaming transport carries prompt delivery metadata and callbacks"
    )]
    fn send_message<'a>(
        &'a self,
        session_id: i64,
        content: &'a str,
        model: Option<&'a str>,
        editor_context: Option<&'a EditorContext>,
        browser_preferences: Option<&'a openwebide_core::BrowserPreferences>,
        queued_prompt: Option<openwebide_core::QueuedPromptKey>,
        signal: Option<&'a AbortSignal>,
        on_event: Box<dyn FnMut(RunEvent) + 'a>,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(BackendApi::send_message(
            self,
            session_id,
            content,
            model,
            editor_context,
            browser_preferences,
            queued_prompt,
            signal,
            on_event,
        ))
    }
}
