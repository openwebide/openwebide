//! Segment routing for the Spin HTTP entry point.
use crate::api;
use crate::error::{ApiError, JsonResp};
use crate::state::AppState;
use bytes::Bytes;
use spin_sdk::http::{FullBody, Request, Response, box_body};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Health,
    ThemeScript,
    Register,
    Login,
    Me,
    Logout,
    BridgeToken,
    ListConnections,
    ModelSetup,
    ModelDefaults,
    ModelProfile,
    ServerSettings,
    SaveServerSettings,
    ModelRuntime,
    DetectModel,
    InspectServer,
    DiscoverServers,
    CreateConnection,
    UpdateConnection,
    DeleteConnection,
    SetToolStreamUnsupported,
    PushConfig,
    PushContext,
    PushSubscribe,
    PushStatus,
    PushUnsubscribe,
    PushNotify,
    PushDispatch,
    ScheduledList,
    ScheduledCommand,
    ScheduledMonitors,
    BindBackgroundHost,
    ScheduledSessionCommand,
    ScheduledDue,
    PluginJobService,
    PluginRunService,
    HostJournal,
    HostInspect,
    HostConnection,
    SaveHostConnection,
    HostProbe,
    HostInput,
    Questions,
    ScheduledResult,
    SessionRunLease,
    GetSettings,
    SetSetting,
    ListSystemPrompts,
    CreateSystemPrompt,
    UpdateSystemPrompt,
    DeleteSystemPrompt,
    GetProjectMemories,
    ProjectMemoryCommand,
    GetSessionMemories,
    SessionMemoryCommand,
    GetProjectSkills,
    ProjectSkillCommand,
    GetSessionSkills,
    SessionSkillCommand,
    PluginExecutionGrants,
    PluginHostRequest,
    PluginContextGrants,
    PluginContextHostRequest,
    PluginStartInvocation,
    PluginContinueInvocation,
    PluginCancelInvocation,
    ListProjects,
    ListPlugins,
    ListMarketplaces,
    SaveMarketplaces,
    RefreshMarketplaces,
    ListProjectPlugins,
    ProjectPluginCommand,
    RemovePlugin,
    PluginPackage,
    RecordPlugin,
    PreparePlugin,
    PluginPreparation,
    GetEditorRecovery,
    SaveEditorRecovery,
    CreateProject,
    ListPendingEdits,
    ListRunChanges,
    RunReview,
    ResolvePendingEdit,
    RenameProject,
    DeleteProject,
    Browse,
    ListSessions,
    SearchSessions,
    SearchSessionSuggestions,
    SessionPreferences,
    ExportSession,
    SessionTitle,
    CreateSession,
    RenameSession,
    SetSessionConnection,
    DeleteSession,
    SetToolPermission,
    ApprovalCheck,
    ListMessages,
    PrepareRewind,
    CompleteRewind,
    SendSessionMessage,
    RunPlan,
    CancelSession,
    PersistMessage,
    ListQueuedPrompts,
    EnqueuePrompt,
    UpdateQueuedPrompt,
    RemoveQueuedPrompt,
    ConsumeQueuedPrompt,
    ForkSession,
    GetGoal,
    UpdateGoal,
    CompactSession,
    GetTodoPlan,
    WriteTodoPlan,
    UpsertToolStep,
    SaveToolTiming,
    SaveTask,
    CompleteToolStep,
    ListModels,
    ModelContext,
    Chat,
    ChatTools,
    Assistance,
    ModelComplete,
    ModelBackground,
    ModelTokens,
    PreviewServer,
    PreviewModel,
    TestModel,
    SaveModelSetup,
    WebSearch,
    WebFetch,
    FilesGet,
    FilesPut,
    FilesPost,
    FilesDelete,
    GitGet,
    GitPost,
}

impl Route {
    fn is_public(self) -> bool {
        matches!(
            self,
            Self::Health | Self::Register | Self::Login | Self::Logout
        )
    }
}

fn numeric_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())
}

fn resolve(method: &str, segments: &[&str]) -> Option<Route> {
    match (method, segments) {
        ("GET", ["theme.js"]) => Some(Route::ThemeScript),
        ("GET", ["health"]) => Some(Route::Health),
        ("POST", ["sessions", "search-suggestions"]) => Some(Route::SearchSessionSuggestions),
        ("POST", ["assistance"]) => Some(Route::Assistance),
        ("POST", ["auth", "register"]) => Some(Route::Register),
        ("POST", ["auth", "login"]) => Some(Route::Login),
        ("GET", ["auth", "me"]) => Some(Route::Me),
        ("POST", ["auth", "logout"]) => Some(Route::Logout),
        ("POST", ["bridge", "token"]) => Some(Route::BridgeToken),
        ("POST", ["model-setup", "detect"]) => Some(Route::DetectModel),
        ("POST", ["model-setup", "inspect"]) => Some(Route::InspectServer),
        ("POST", ["model-setup", "discover"]) => Some(Route::DiscoverServers),
        ("GET", ["model-setup"]) => Some(Route::ModelSetup),
        ("PUT", ["model-setup", "defaults"]) => Some(Route::ModelDefaults),
        ("PUT", ["model-setup", "profile"]) => Some(Route::ModelProfile),
        ("GET", ["connections", id, "settings"]) if numeric_id(id) => Some(Route::ServerSettings),
        ("PUT", ["connections", id, "settings"]) if numeric_id(id) => {
            Some(Route::SaveServerSettings)
        }
        ("GET", ["connections", id, "runtime"]) if numeric_id(id) => Some(Route::ModelRuntime),
        ("GET", ["connections"]) => Some(Route::ListConnections),
        ("POST", ["connections"]) => Some(Route::CreateConnection),
        ("PUT", ["connections", _]) => Some(Route::UpdateConnection),
        ("DELETE", ["connections", _]) => Some(Route::DeleteConnection),
        ("POST", ["connections", _, "tool-stream-unsupported"]) => {
            Some(Route::SetToolStreamUnsupported)
        }
        ("GET", ["push", "config"]) => Some(Route::PushConfig),
        ("GET", ["push", "context"]) => Some(Route::PushContext),
        ("POST", ["push", "subscriptions"]) => Some(Route::PushSubscribe),
        ("POST", ["push", "subscriptions", "status"]) => Some(Route::PushStatus),
        ("DELETE", ["push", "subscriptions"]) => Some(Route::PushUnsubscribe),
        ("POST", ["push", "dispatch"]) => Some(Route::PushDispatch),
        ("POST", ["sessions", id, "notifications"]) if numeric_id(id) => Some(Route::PushNotify),
        ("GET", ["sessions", id, "monitors"]) if numeric_id(id) => Some(Route::ScheduledMonitors),
        ("POST", ["projects", id, "execution-host"]) if numeric_id(id) => {
            Some(Route::BindBackgroundHost)
        }
        ("GET", ["scheduled-tasks"]) => Some(Route::ScheduledList),
        ("POST", ["scheduled-tasks"]) => Some(Route::ScheduledCommand),
        ("GET", ["host", "connection"]) => Some(Route::HostConnection),
        ("PUT", ["host", "connection"]) => Some(Route::SaveHostConnection),
        ("POST", ["host", "probe"]) => Some(Route::HostProbe),
        ("POST", ["sessions", id, "host-input"]) if numeric_id(id) => Some(Route::HostInput),
        ("POST", ["host", "journal"]) => Some(Route::HostJournal),
        ("POST", ["sessions", id, "host"]) if numeric_id(id) => Some(Route::HostInspect),
        ("POST", ["scheduled-tasks", "due"]) => Some(Route::ScheduledDue),
        ("POST", ["plugins", "jobs", "service"]) => Some(Route::PluginJobService),
        ("POST", ["plugins", "runs", "service"]) => Some(Route::PluginRunService),
        ("POST", ["scheduled-tasks", "result"]) => Some(Route::ScheduledResult),
        ("POST", ["sessions", id, "scheduled-tasks"]) if numeric_id(id) => {
            Some(Route::ScheduledSessionCommand)
        }
        ("POST", ["sessions", id, "run-lease"]) if numeric_id(id) => Some(Route::SessionRunLease),
        ("GET", ["settings"]) => Some(Route::GetSettings),
        ("PUT", ["settings"]) => Some(Route::SetSetting),
        ("GET", ["system-prompts"]) => Some(Route::ListSystemPrompts),
        ("POST", ["system-prompts"]) => Some(Route::CreateSystemPrompt),
        ("PUT", ["system-prompts", _]) => Some(Route::UpdateSystemPrompt),
        ("DELETE", ["system-prompts", _]) => Some(Route::DeleteSystemPrompt),
        ("GET", ["projects", id, "editor-recovery"]) if numeric_id(id) => {
            Some(Route::GetEditorRecovery)
        }
        ("PUT", ["projects", id, "editor-recovery"]) if numeric_id(id) => {
            Some(Route::SaveEditorRecovery)
        }
        ("GET", ["projects", id, "memories"]) if numeric_id(id) => Some(Route::GetProjectMemories),
        ("POST", ["projects", id, "memories"]) if numeric_id(id) => {
            Some(Route::ProjectMemoryCommand)
        }
        ("GET", ["sessions", id, "memories"]) if numeric_id(id) => Some(Route::GetSessionMemories),
        ("POST", ["sessions", id, "memories"]) if numeric_id(id) => {
            Some(Route::SessionMemoryCommand)
        }
        ("GET", ["projects", id, "skills"]) if numeric_id(id) => Some(Route::GetProjectSkills),
        ("POST", ["projects", id, "skills"]) if numeric_id(id) => Some(Route::ProjectSkillCommand),
        ("GET", ["sessions", id, "skills"]) if numeric_id(id) => Some(Route::GetSessionSkills),
        ("POST", ["sessions", id, "skills"]) if numeric_id(id) => Some(Route::SessionSkillCommand),
        ("POST", ["sessions", id, "plugin-grants"]) if numeric_id(id) => {
            Some(Route::PluginExecutionGrants)
        }
        ("POST", ["plugins", "execution-grants"]) => Some(Route::PluginContextGrants),
        ("POST", ["plugins", "host"]) => Some(Route::PluginContextHostRequest),
        ("POST", ["plugins", "invoke"]) => Some(Route::PluginStartInvocation),
        ("POST", ["plugins", "continue"]) => Some(Route::PluginContinueInvocation),
        ("POST", ["plugins", "cancel"]) => Some(Route::PluginCancelInvocation),
        ("POST", ["sessions", id, "plugin-host"]) if numeric_id(id) => {
            Some(Route::PluginHostRequest)
        }
        ("GET", ["projects"]) => Some(Route::ListProjects),
        ("GET", ["plugin-marketplaces"]) => Some(Route::ListMarketplaces),
        ("POST", ["plugin-marketplaces"]) => Some(Route::SaveMarketplaces),
        ("POST", ["plugin-marketplaces", "refresh"]) => Some(Route::RefreshMarketplaces),
        ("POST", ["plugins", "remove"]) => Some(Route::RemovePlugin),
        ("GET", ["projects", id, "plugins"]) if numeric_id(id) => Some(Route::ListProjectPlugins),
        ("POST", ["projects", id, "plugins"]) if numeric_id(id) => {
            Some(Route::ProjectPluginCommand)
        }
        ("POST", ["projects", id, "plugins", "package"]) if numeric_id(id) => {
            Some(Route::PluginPackage)
        }
        ("GET", ["plugins"]) => Some(Route::ListPlugins),
        ("POST", ["plugins"]) => Some(Route::RecordPlugin),
        ("POST", ["plugins", "prepare"]) => Some(Route::PreparePlugin),
        ("POST", ["plugins", "preparation"]) => Some(Route::PluginPreparation),
        ("POST", ["projects", id, "plugins", "preparation"]) if numeric_id(id) => {
            Some(Route::PluginPreparation)
        }
        ("POST", ["plugins", "package"]) => Some(Route::PluginPackage),
        ("POST", ["projects", id, "plugins", "prepare"]) if numeric_id(id) => {
            Some(Route::PreparePlugin)
        }
        ("POST", ["projects"]) => Some(Route::CreateProject),
        ("PUT", ["projects", _]) => Some(Route::RenameProject),
        ("DELETE", ["projects", _]) => Some(Route::DeleteProject),
        ("GET", ["projects", id, "pending-edits"]) if numeric_id(id) => {
            Some(Route::ListPendingEdits)
        }
        ("POST", ["projects", id, "pending-edits", "resolve"]) if numeric_id(id) => {
            Some(Route::ResolvePendingEdit)
        }
        ("GET", ["browse"]) => Some(Route::Browse),
        ("GET", ["sessions"]) => Some(Route::ListSessions),
        ("POST", ["sessions", "search"]) => Some(Route::SearchSessions),
        ("PUT", ["sessions", _, "preferences"]) => Some(Route::SessionPreferences),
        ("POST", ["sessions", _, "title"]) => Some(Route::SessionTitle),
        ("GET", ["sessions", _, "export"]) => Some(Route::ExportSession),
        ("POST", ["sessions"]) => Some(Route::CreateSession),
        ("PUT", ["sessions", id]) if numeric_id(id) => Some(Route::RenameSession),
        ("PUT", ["sessions", id, "connection"]) if numeric_id(id) => {
            Some(Route::SetSessionConnection)
        }
        ("DELETE", ["sessions", id]) if numeric_id(id) => Some(Route::DeleteSession),
        ("POST", ["sessions", _, "approval-check"]) => Some(Route::ApprovalCheck),
        ("POST", ["sessions", _, "permissions", _]) => Some(Route::SetToolPermission),
        ("POST", ["sessions", _, "rewind"]) => Some(Route::PrepareRewind),
        ("POST", ["sessions", _, "rewind", "complete"]) => Some(Route::CompleteRewind),
        ("GET", ["sessions", _, "messages"]) => Some(Route::ListMessages),
        ("POST", ["sessions", _, "messages"]) => Some(Route::SendSessionMessage),
        ("POST", ["sessions", _, "run-plan"]) => Some(Route::RunPlan),
        ("POST", ["sessions", _, "cancel"]) => Some(Route::CancelSession),
        ("POST", ["sessions", _, "messages", "persist"]) => Some(Route::PersistMessage),
        ("GET", ["sessions", _, "queue"]) => Some(Route::ListQueuedPrompts),
        ("POST", ["sessions", _, "queue"]) => Some(Route::EnqueuePrompt),
        ("PUT", ["sessions", _, "queue"]) => Some(Route::UpdateQueuedPrompt),
        ("DELETE", ["sessions", _, "queue"]) => Some(Route::RemoveQueuedPrompt),
        ("POST", ["sessions", _, "queue", "send"]) => Some(Route::ConsumeQueuedPrompt),
        ("POST", ["sessions", _, "fork"]) => Some(Route::ForkSession),
        ("GET", ["sessions", _, "goal"]) => Some(Route::GetGoal),
        ("POST", ["sessions", _, "goal"]) => Some(Route::UpdateGoal),
        ("POST", ["sessions", _, "compact"]) => Some(Route::CompactSession),
        ("POST", ["sessions", id, "questions"]) if numeric_id(id) => Some(Route::Questions),
        ("GET", ["sessions", _, "todos"]) => Some(Route::GetTodoPlan),
        ("POST", ["sessions", _, "todos"]) => Some(Route::WriteTodoPlan),
        ("POST", ["sessions", _, "tool-steps", "upsert"]) => Some(Route::UpsertToolStep),
        ("POST", ["sessions", _, "tasks"]) => Some(Route::SaveTask),
        ("POST", ["sessions", _, "tool-steps", "timing"]) => Some(Route::SaveToolTiming),
        ("POST", ["sessions", _, "tool-steps", "complete"]) => Some(Route::CompleteToolStep),
        ("GET", ["models"]) => Some(Route::ListModels),
        ("GET", ["models", "context"]) => Some(Route::ModelContext),
        ("POST", ["chat"]) => Some(Route::Chat),
        ("POST", ["models", "complete"]) => Some(Route::ModelComplete),
        ("POST", ["models", "background"]) => Some(Route::ModelBackground),
        ("POST", ["models", "tokens"]) => Some(Route::ModelTokens),
        ("POST", ["model-setup", "save"]) => Some(Route::SaveModelSetup),
        ("POST", ["models", "preview"]) => Some(Route::PreviewServer),
        ("POST", ["models", "preview", "test"]) => Some(Route::TestModel),
        ("POST", ["models", "preview", "detect"]) => Some(Route::PreviewModel),
        ("POST", ["chat-tools"]) => Some(Route::ChatTools),
        ("GET", ["web", "search"]) => Some(Route::WebSearch),
        ("GET", ["web", "fetch"]) => Some(Route::WebFetch),
        ("GET", ["projects", _, "run-changes"]) => Some(Route::ListRunChanges),
        ("POST", ["projects", _, "reviews", "preview" | "prepare" | "complete"]) => {
            Some(Route::RunReview)
        }
        ("GET", ["projects", _, "files"]) => Some(Route::FilesGet),
        ("GET", ["projects", _, "files", "context"]) => Some(Route::FilesGet),
        ("GET", ["projects", _, "files", "read"]) => Some(Route::FilesGet),
        ("GET", ["projects", _, "files", "raw"]) => Some(Route::FilesGet),
        ("GET", ["projects", _, "files", "canonical"]) => Some(Route::FilesGet),
        ("GET", ["projects", _, "files", "search"]) => Some(Route::FilesGet),
        ("GET", ["projects", _, "files", "content-search"]) => Some(Route::FilesGet),
        ("PUT", ["projects", _, "files", "write"]) => Some(Route::FilesPut),
        ("POST", ["projects", _, "files", "create"]) => Some(Route::FilesPost),
        ("POST", ["projects", _, "files", "copy"]) => Some(Route::FilesPost),
        ("DELETE", ["projects", _, "files", "delete"]) => Some(Route::FilesDelete),
        ("GET", ["git", "status"]) => Some(Route::GitGet),
        ("GET", ["git", "path-status"]) => Some(Route::GitGet),
        ("GET", ["projects", _, "git", "path-status"]) => Some(Route::GitGet),
        (
            "POST",
            [
                "git",
                "path-status" | "path" | "history" | "commit-diff" | "stash" | "index-diff",
            ],
        ) => Some(Route::GitPost),
        (
            "POST",
            [
                "projects",
                _,
                "git",
                "path-status" | "path" | "history" | "commit-diff" | "stash" | "index-diff",
            ],
        ) => Some(Route::GitPost),
        ("GET", ["projects", _, "git", "status"]) => Some(Route::GitGet),
        ("GET", ["git", "diff"]) => Some(Route::GitGet),
        ("GET", ["projects", _, "git", "diff"]) => Some(Route::GitGet),
        ("GET", ["git", "branches"]) => Some(Route::GitGet),
        ("GET", ["projects", _, "git", "branches"]) => Some(Route::GitGet),
        ("GET", ["git", "show"]) => Some(Route::GitGet),
        ("GET", ["projects", _, "git", "show"]) => Some(Route::GitGet),
        ("POST", ["git", "status"]) => Some(Route::GitPost),
        ("POST", ["projects", _, "git", "status"]) => Some(Route::GitPost),
        ("POST", ["git", "diff"]) => Some(Route::GitPost),
        ("POST", ["projects", _, "git", "diff"]) => Some(Route::GitPost),
        ("POST", ["git", "branches"]) => Some(Route::GitPost),
        ("POST", ["projects", _, "git", "branches"]) => Some(Route::GitPost),
        ("POST", ["git", "commit"]) => Some(Route::GitPost),
        ("POST", ["projects", _, "git", "commit"]) => Some(Route::GitPost),
        ("POST", ["git", "checkout"]) => Some(Route::GitPost),
        ("POST", ["projects", _, "git", "checkout"]) => Some(Route::GitPost),
        ("POST", ["git", "sync"]) => Some(Route::GitPost),
        ("POST", ["projects", _, "git", "sync"]) => Some(Route::GitPost),
        _ => None,
    }
}

pub async fn route(req: Request) -> JsonResp {
    let path = req.uri().path().to_string();
    let method = req.method().clone();
    let origin_header = req
        .headers()
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let origin = origin_header.as_deref();
    if method.as_str() == "OPTIONS" {
        return with_cors(preflight(origin), origin);
    }
    let segments: Vec<&str> = path
        .strip_prefix("/api/")
        .unwrap_or("")
        .split('/')
        .collect();
    let route = resolve(method.as_str(), &segments);
    let state = match AppState::new().await {
        Ok(state) => state,
        Err(e) => {
            let error = ApiError::internal(format!("{e:#}"));
            error.log_for_route(method.as_str(), &path);
            return with_cors(error.into_response(), origin);
        }
    };
    if matches!(
        route,
        Some(
            Route::PushDispatch
                | Route::ScheduledDue
                | Route::ScheduledResult
                | Route::HostJournal
                | Route::PluginJobService
                | Route::PluginRunService
        )
    ) {
        let result = match crate::auth::require_bridge_service(&state, req.headers()).await {
            Ok(()) => match route {
                Some(Route::HostJournal) => api::host_admin::journal(req, &state).await,
                Some(Route::PluginJobService) => api::plugin_jobs::service(req, &state).await,
                Some(Route::PluginRunService) => api::plugin_runs::service(req, &state).await,
                Some(Route::ScheduledDue) => api::scheduled::due(req, &state).await,
                Some(Route::ScheduledResult) => api::scheduled::result(req, &state).await,
                _ => api::push::dispatch(&state).await,
            },
            Err(error) => Err(error),
        };
        return match result {
            Ok(response) => response,
            Err(error) => {
                error.log_for_route(method.as_str(), &path);
                error.into_response()
            }
        };
    }
    let user = match authenticate_route(&state, req.headers(), route, &path).await {
        Ok(user) => user,
        Err(e) => {
            e.log_for_route(method.as_str(), &path);
            return with_cors(e.into_response(), origin);
        }
    };
    // Native bridge requests already passed shared-secret authentication above.
    let bridge_authenticated = req.headers().contains_key("authorization") && user.is_some();
    if !csrf_allowed(method.as_str(), &path, req.headers()) && !bridge_authenticated {
        return with_cors(
            ApiError::unauthorized("not signed in").into_response(),
            origin,
        );
    }
    let result = match (route, user) {
        (Some(Route::ThemeScript), user) => api::settings::theme_script(&state, user).await,
        (Some(Route::Health), None) => Ok(api::auth::health()),
        (Some(Route::Register), None) => api::auth::register(req, &state).await,
        (Some(Route::Login), None) => api::auth::login(req, &state).await,
        (Some(Route::Logout), None) => api::auth::logout(req, &state).await,
        (Some(Route::Me), Some(user)) => api::auth::me(&state, user).await,
        (Some(Route::BridgeToken), Some(user)) => api::bridge::bridge_token(&state, user).await,
        (Some(Route::DetectModel), Some(user)) => api::model_setup::detect(req, &state, user).await,
        (Some(Route::InspectServer), Some(_)) => api::model_setup::inspect(req).await,
        (Some(Route::DiscoverServers), Some(_)) => api::model_setup::discover().await,
        (Some(Route::ModelSetup), Some(user)) => api::model_setup::get(&state, user).await,
        (Some(Route::ModelDefaults), Some(user)) => {
            api::model_setup::defaults(req, &state, user).await
        }
        (Some(Route::ModelProfile), Some(user)) => {
            api::model_setup::profile(req, &state, user).await
        }
        (Some(Route::ServerSettings), Some(_)) => {
            api::model_setup::settings(req, &state, &path, false).await
        }
        (Some(Route::SaveServerSettings), Some(_)) => {
            api::model_setup::settings(req, &state, &path, true).await
        }
        (Some(Route::ModelRuntime), Some(user)) => {
            api::model_setup::native_runtime(req, &state, &path, user, bridge_authenticated).await
        }
        (Some(Route::ListConnections), Some(user)) => {
            api::connections::list_connections(&state, user).await
        }
        (Some(Route::CreateConnection), Some(user)) => {
            api::connections::create_connection(req, &state, user).await
        }
        (Some(Route::UpdateConnection), Some(user)) => {
            api::connections::update_connection(req, &state, &path, user).await
        }
        (Some(Route::DeleteConnection), Some(user)) => {
            api::connections::delete_connection(&state, &path, user).await
        }
        (Some(Route::SetToolStreamUnsupported), Some(user)) => {
            api::connections::set_tool_stream_unsupported(req, &state, &path, user).await
        }
        (Some(Route::GetProjectMemories), Some(user)) => {
            api::memories::get(&state, &path, user, false).await
        }
        (Some(Route::ProjectMemoryCommand), Some(user)) => {
            api::memories::command(req, &state, &path, user, false).await
        }
        (Some(Route::GetSessionMemories), Some(user)) => {
            api::memories::get(&state, &path, user, true).await
        }
        (Some(Route::SessionMemoryCommand), Some(user)) => {
            api::memories::command(req, &state, &path, user, true).await
        }
        (Some(Route::PluginExecutionGrants), Some(user)) => {
            api::plugins::grant_execution(req, &state, &path, user).await
        }
        (Some(Route::PluginHostRequest), Some(user)) => {
            api::plugins::host_request(req, &state, &path, user).await
        }
        (Some(Route::PluginContextGrants), Some(user)) => {
            api::plugins::grant_context(req, &state, user).await
        }
        (Some(Route::PluginContextHostRequest), Some(user)) => {
            api::plugins::context_host_request(req, &state, user).await
        }
        (Some(Route::PluginStartInvocation), Some(user)) => {
            api::plugins::start_invocation(req, &state, user).await
        }
        (Some(Route::PluginContinueInvocation), Some(user)) => {
            api::plugins::continue_invocation(req, &state, user).await
        }
        (Some(Route::PluginCancelInvocation), Some(user)) => {
            api::plugins::cancel_invocation(req, &state, user).await
        }
        (Some(Route::GetProjectSkills), Some(user)) => {
            api::skills::get(&state, &path, user, false).await
        }
        (Some(Route::ListMarketplaces), Some(user)) => {
            api::plugins::marketplaces(&state, user).await
        }
        (Some(Route::SaveMarketplaces), Some(user)) => {
            api::plugins::save_marketplaces(req, &state, user).await
        }
        (Some(Route::RefreshMarketplaces), Some(user)) => {
            api::plugins::refresh_marketplaces(&state, user).await
        }
        (Some(Route::RemovePlugin), Some(user)) => api::plugins::remove(req, &state, user).await,
        (Some(Route::ListProjectPlugins), Some(user)) => {
            api::plugins::project_list(&state, &path, user).await
        }
        (Some(Route::ProjectPluginCommand), Some(user)) => {
            api::plugins::project_command(req, &state, &path, user).await
        }
        (Some(Route::PluginPackage), Some(user)) => {
            api::plugins::package(req, &state, &path, user).await
        }
        (Some(Route::ListPlugins), Some(user)) => api::plugins::list(&state, user).await,
        (Some(Route::RecordPlugin), Some(user)) => api::plugins::record(req, &state, user).await,
        (Some(Route::PreparePlugin), Some(user)) => {
            api::plugins::prepare(req, &state, &path, user).await
        }
        (Some(Route::PluginPreparation), Some(user)) => {
            api::plugins::preparation(req, &state, &path, user).await
        }
        (Some(Route::ProjectSkillCommand), Some(user)) => {
            api::skills::command(req, &state, &path, user, false).await
        }
        (Some(Route::GetSessionSkills), Some(user)) => {
            api::skills::get(&state, &path, user, true).await
        }
        (Some(Route::SessionSkillCommand), Some(user)) => {
            api::skills::command(req, &state, &path, user, true).await
        }
        (Some(Route::GetEditorRecovery), Some(user)) => {
            api::editor_recovery::get(&state, &path, user).await
        }
        (Some(Route::SaveEditorRecovery), Some(user)) => {
            api::editor_recovery::save(req, &state, &path, user).await
        }
        (Some(Route::PushConfig), Some(_)) => api::push::config(&state).await,
        (Some(Route::PushContext), Some(user)) => api::push::context(&state, user).await,
        (Some(Route::PushSubscribe), Some(user)) => api::push::subscribe(req, &state, user).await,
        (Some(Route::PushStatus), Some(user)) => {
            api::push::subscription(req, &state, user, false).await
        }
        (Some(Route::PushUnsubscribe), Some(user)) => {
            api::push::subscription(req, &state, user, true).await
        }
        (Some(Route::PushNotify), Some(user)) => api::push::notify(req, &state, &path, user).await,
        (Some(Route::ScheduledList), Some(user)) => {
            api::scheduled::list(&state, req.uri().query(), user).await
        }
        (Some(Route::ScheduledCommand), Some(user)) => {
            api::scheduled::command(req, &state, user).await
        }
        (Some(Route::HostConnection), Some(user)) => {
            api::host_admin::connection(req, &state, user, false).await
        }
        (Some(Route::SaveHostConnection), Some(user)) => {
            api::host_admin::connection(req, &state, user, true).await
        }
        (Some(Route::HostProbe), Some(user)) => api::host_admin::probe(&state, user).await,
        (Some(Route::HostInput), Some(user)) => {
            api::host_admin::input(req, &state, &path, user).await
        }
        (Some(Route::HostInspect), Some(user)) => {
            api::host_admin::inspect(req, &state, &path, user).await
        }
        (Some(Route::ScheduledMonitors), Some(user)) => {
            api::scheduled::monitors(&state, &path, user).await
        }
        (Some(Route::BindBackgroundHost), Some(user)) => {
            api::scheduled::bind_host(req, &state, &path, user).await
        }
        (Some(Route::ScheduledSessionCommand), Some(user)) => {
            api::scheduled::session_command(req, &state, &path, user).await
        }
        (Some(Route::SessionRunLease), Some(user)) => {
            api::scheduled::lease(req, &state, &path, user).await
        }
        (Some(Route::GetSettings), Some(user)) => api::settings::get_settings(&state, user).await,
        (Some(Route::SetSetting), Some(user)) => {
            api::settings::set_setting(req, &state, user).await
        }
        (Some(Route::ListSystemPrompts), Some(user)) => {
            api::prompts::list_system_prompts(&state, user).await
        }
        (Some(Route::CreateSystemPrompt), Some(user)) => {
            api::prompts::create_system_prompt(req, &state, user).await
        }
        (Some(Route::UpdateSystemPrompt), Some(user)) => {
            api::prompts::update_system_prompt(req, &state, &path, user).await
        }
        (Some(Route::DeleteSystemPrompt), Some(user)) => {
            api::prompts::delete_system_prompt(&state, &path, user).await
        }
        (Some(Route::ListProjects), Some(user)) => api::projects::list_projects(&state, user).await,
        (Some(Route::CreateProject), Some(user)) => {
            api::projects::create_project(req, &state, user).await
        }
        (Some(Route::RenameProject), Some(user)) => {
            api::projects::rename_project(req, &state, &path, user).await
        }
        (Some(Route::DeleteProject), Some(user)) => {
            api::projects::delete_project(&state, &path, user).await
        }
        (Some(Route::ListRunChanges), Some(user)) => api::reviews::list(&state, &path, user).await,
        (Some(Route::RunReview), Some(user)) => {
            api::reviews::review(req, &state, &path, user).await
        }
        (Some(Route::ListPendingEdits), Some(user)) => {
            api::projects::list_pending_edits(&state, &path, user).await
        }
        (Some(Route::ResolvePendingEdit), Some(user)) => {
            api::projects::resolve_pending_edit(req, &state, &path, user).await
        }
        (Some(Route::Browse), Some(user)) => api::projects::browse(req, &state, user).await,
        (Some(Route::ListSessions), Some(user)) => api::sessions::list_sessions(&state, user).await,
        (Some(Route::SearchSessionSuggestions), Some(user)) => {
            api::session_search::search(req, &state, user).await
        }
        (Some(Route::SearchSessions), Some(user)) => {
            api::sessions::search_sessions(req, &state, user).await
        }
        (Some(Route::SessionPreferences), Some(user)) => {
            api::sessions::session_preferences(req, &state, &path, user).await
        }
        (Some(Route::Assistance), Some(user)) => api::assistance::generate(req, &state, user).await,
        (Some(Route::SessionTitle), Some(user)) => {
            api::sessions::session_title(&state, &path, user).await
        }
        (Some(Route::ExportSession), Some(user)) => {
            api::sessions::export_session(&state, &path, user).await
        }
        (Some(Route::CreateSession), Some(user)) => {
            api::sessions::create_session(req, &state, user).await
        }
        (Some(Route::RenameSession), Some(user)) => {
            api::sessions::rename_session(req, &state, &path, user).await
        }
        (Some(Route::SetSessionConnection), Some(user)) => {
            api::sessions::set_session_connection(req, &state, &path, user).await
        }
        (Some(Route::DeleteSession), Some(user)) => {
            api::sessions::delete_session(&state, &path, user).await
        }
        (Some(Route::ApprovalCheck), Some(user)) => {
            api::approvals::check(req, &state, &path, user).await
        }
        (Some(Route::SetToolPermission), Some(user)) => {
            api::sessions::set_tool_permission(req, &state, &path, user).await
        }
        (Some(Route::PrepareRewind), Some(user)) => {
            api::sessions::rewind_session(req, &state, &path, user, false).await
        }
        (Some(Route::CompleteRewind), Some(user)) => {
            api::sessions::rewind_session(req, &state, &path, user, true).await
        }
        (Some(Route::ListMessages), Some(user)) => {
            api::sessions::list_messages(&state, &path, user).await
        }
        (Some(Route::SendSessionMessage), Some(user)) => {
            api::sessions::send_session_message(req, state, &path, user).await
        }
        (Some(Route::RunPlan), Some(user)) => {
            api::sessions::run_plan(req, &state, &path, user).await
        }
        (Some(Route::CancelSession), Some(user)) => {
            api::sessions::cancel_session(&state, &path, user).await
        }
        (Some(Route::PersistMessage), Some(user)) => {
            api::sessions::persist_message(req, &state, &path, user).await
        }
        (Some(Route::GetGoal), Some(user)) => api::sessions::get_goal(&state, &path, user).await,
        (Some(Route::UpdateGoal), Some(user)) => {
            api::sessions::update_goal(req, &state, &path, user).await
        }
        (Some(Route::CompactSession), Some(user)) => {
            api::sessions::compact_session(req, state, &path, user).await
        }
        (Some(Route::Questions), Some(user)) => {
            api::questions::command(req, &state, &path, user).await
        }
        (Some(Route::GetTodoPlan), Some(user)) => {
            api::sessions::get_todo_plan(&state, &path, user).await
        }
        (Some(Route::WriteTodoPlan), Some(user)) => {
            api::sessions::write_todo_plan(req, &state, &path, user).await
        }
        (Some(Route::ForkSession), Some(user)) => {
            api::sessions::fork_session(req, &state, &path, user).await
        }
        (
            Some(
                route @ (Route::ListQueuedPrompts
                | Route::EnqueuePrompt
                | Route::UpdateQueuedPrompt
                | Route::RemoveQueuedPrompt
                | Route::ConsumeQueuedPrompt),
            ),
            Some(user),
        ) => {
            use api::sessions::QueueAction;
            let action = match route {
                Route::ListQueuedPrompts => QueueAction::List,
                Route::EnqueuePrompt => QueueAction::Add,
                Route::UpdateQueuedPrompt => QueueAction::Update,
                Route::RemoveQueuedPrompt => QueueAction::Remove,
                Route::ConsumeQueuedPrompt => QueueAction::Consume,
                _ => unreachable!(),
            };
            api::sessions::queued_prompts(req, &state, &path, user, action).await
        }
        (Some(Route::UpsertToolStep), Some(user)) => {
            api::sessions::upsert_tool_step(req, &state, &path, user).await
        }
        (Some(Route::SaveTask), Some(user)) => {
            api::sessions::save_task(req, &state, &path, user).await
        }
        (Some(Route::SaveToolTiming), Some(user)) => {
            api::sessions::save_tool_timing(req, &state, &path, user).await
        }
        (Some(Route::CompleteToolStep), Some(user)) => {
            api::sessions::complete_tool_step(req, &state, &path, user).await
        }
        (Some(Route::ListModels), Some(user)) => api::chat::list_models(req, &state, user).await,
        (Some(Route::ModelContext), Some(user)) => {
            api::chat::model_context(req, &state, user).await
        }
        (Some(Route::Chat), Some(user)) => api::chat::chat(req, &state, user).await,
        (Some(Route::PreviewServer), Some(_)) => {
            api::model_setup::preview(req, &state, false).await
        }
        (Some(Route::SaveModelSetup), Some(user)) => {
            api::model_setup::save_review(req, &state, user).await
        }
        (Some(Route::TestModel), Some(_)) => api::model_setup::test_model(req, &state).await,
        (Some(Route::PreviewModel), Some(_)) => api::model_setup::preview(req, &state, true).await,
        (Some(Route::ModelComplete), Some(user)) => {
            api::model_operations::route(req, &state, user, false).await
        }
        (Some(Route::ModelBackground), Some(user)) => {
            api::model_operations::background(req, &state, user).await
        }
        (Some(Route::ModelTokens), Some(user)) => {
            api::model_operations::route(req, &state, user, true).await
        }
        (Some(Route::ChatTools), Some(user)) => api::chat::chat_tools(req, &state, user).await,
        (Some(Route::WebSearch), Some(user)) => api::web::web_search(req, user).await,
        (Some(Route::WebFetch), Some(user)) => api::web::web_fetch(req, user).await,
        (Some(Route::FilesGet), Some(user)) => {
            api::files::files_get(req, &state, &path, user).await
        }
        (Some(Route::FilesPut), Some(user)) => {
            api::files::files_put(req, &state, &path, user).await
        }
        (Some(Route::FilesPost), Some(user)) => {
            api::files::files_post(req, &state, &path, user).await
        }
        (Some(Route::FilesDelete), Some(user)) => {
            api::files::files_delete(req, &state, &path, user).await
        }
        (Some(Route::GitGet), Some(user)) => api::git::git_get(req, &state, &path, user).await,
        (Some(Route::GitPost), Some(user)) => api::git::git_post(req, &state, &path, user).await,
        _ => Err(ApiError::not_found(format!("no route for {method} {path}"))),
    };
    let resp = match result {
        Ok(resp) => resp,
        Err(e) => {
            e.log_for_route(method.as_str(), &path);
            e.into_response()
        }
    };
    with_cors(resp, origin)
}

fn csrf_allowed(method: &str, path: &str, headers: &spin_sdk::http::HeaderMap) -> bool {
    (method == "GET" && path == "/api/theme.js") || crate::auth::csrf_header_ok(headers)
}

async fn authenticate_route(
    state: &AppState,
    headers: &spin_sdk::http::HeaderMap,
    route: Option<Route>,
    path: &str,
) -> Result<Option<crate::auth::AuthedUser>, ApiError> {
    if route == Some(Route::ThemeScript) {
        return crate::auth::theme_user(state, headers)
            .await
            .map(|user| user.map(Into::into));
    }
    if route.is_some_and(Route::is_public)
        || (route.is_none()
            && matches!(
                path,
                "/api/health" | "/api/auth/register" | "/api/auth/login" | "/api/auth/logout"
            ))
    {
        return Ok(None);
    }
    crate::auth::authenticate(state, headers)
        .await
        .map(|user| Some(user.into()))
}

fn preflight(_origin: Option<&str>) -> JsonResp {
    Response::builder()
        .status(204)
        .body(box_body(FullBody::new(Bytes::new())))
        .expect("valid status and headers")
}

/// Add permissive CORS headers so the frontend can be served from another
/// origin during development (e.g. `trunk serve` on port 8080).
fn with_cors(mut resp: JsonResp, origin: Option<&str>) -> JsonResp {
    let dev_origins = ["http://localhost:8080", "http://127.0.0.1:8080"];
    if let Some(o) = origin
        && dev_origins.contains(&o)
    {
        let headers = resp.headers_mut();
        headers.insert("access-control-allow-origin", o.parse().unwrap());
        headers.insert("access-control-allow-credentials", "true".parse().unwrap());
        headers.insert("vary", "origin".parse().unwrap());
        headers.insert(
            "access-control-allow-methods",
            "GET, POST, PUT, DELETE, OPTIONS".parse().unwrap(),
        );
        headers.insert(
            "access-control-allow-headers",
            "content-type, x-openwebide".parse().unwrap(),
        );
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_exact_theme_get_skips_csrf_header() {
        let mut headers = spin_sdk::http::HeaderMap::new();
        assert!(csrf_allowed("GET", "/api/theme.js", &headers));
        for method in ["GET", "HEAD", "POST", "PUT", "DELETE", "PATCH"] {
            for path in [
                "/api/theme.js",
                "/api/theme.js/",
                "/api/theme.js/extra",
                "/api/settings",
                "/api/health",
                "/api/auth/login",
                "/api/auth/register",
                "/api/auth/logout",
                "/api/unknown",
            ] {
                if method == "GET" && path == "/api/theme.js" {
                    continue;
                }
                assert!(!csrf_allowed(method, path, &headers), "{method} {path}");
            }
        }
        headers.insert("x-openwebide", "1".parse().unwrap());
        assert!(csrf_allowed("POST", "/api/theme.js", &headers));
        assert!(csrf_allowed("GET", "/api/settings", &headers));
        for path in ["/api/auth/register", "/api/auth/login", "/api/auth/logout"] {
            assert!(csrf_allowed("POST", path, &headers));
        }
    }

    #[test]
    fn theme_route_uses_only_valid_session_cookie_and_owned_setting() {
        futures::executor::block_on(async {
            use http_body_util::BodyExt;
            let state = AppState::new().await.unwrap();
            let mut invalid_cookie = spin_sdk::http::HeaderMap::new();
            invalid_cookie.insert("cookie", "owide_session=invalid".parse().unwrap());
            assert!(
                crate::auth::theme_user(&state, &invalid_cookie)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(
                state
                    .store
                    .get_setting("auth_secret")
                    .await
                    .unwrap()
                    .is_none()
            );
            let alice = state
                .store
                .insert_user("alice", "hash", openwebide_core::UserRole::User, 1)
                .await
                .unwrap();
            let bob = state
                .store
                .insert_user("bob", "hash", openwebide_core::UserRole::User, 1)
                .await
                .unwrap();
            state
                .store
                .set_user_setting(bob.id, "theme", "dark")
                .await
                .unwrap();
            let no_theme = state
                .store
                .insert_user("no-theme", "hash", openwebide_core::UserRole::User, 1)
                .await
                .unwrap();
            let no_theme_token = crate::auth::issue_token(&state, &no_theme).await.unwrap();
            let token = crate::auth::issue_token(&state, &alice).await.unwrap();
            let mut headers = spin_sdk::http::HeaderMap::new();
            for (cookie, saved, expected) in [
                (Some(token.as_str()), Some("light"), "'light'"),
                (Some(token.as_str()), Some("dark"), "'dark'"),
                (Some(token.as_str()), Some("system"), "matchMedia"),
                (
                    Some(token.as_str()),
                    Some("invalid';alert(1)"),
                    "matchMedia",
                ),
                (None, Some("light"), "matchMedia"),
                (Some("invalid"), Some("dark"), "matchMedia"),
                (Some(no_theme_token.as_str()), None, "matchMedia"),
            ] {
                headers.remove("cookie");
                if let Some(cookie) = cookie {
                    headers.insert("cookie", format!("owide_session={cookie}").parse().unwrap());
                }
                state
                    .store
                    .set_user_setting(alice.id, "theme", saved.unwrap_or(""))
                    .await
                    .unwrap();
                let route = resolve("GET", &["theme.js"]);
                assert_eq!(route, Some(Route::ThemeScript));
                assert!(csrf_allowed("GET", "/api/theme.js", &headers));
                let user = authenticate_route(&state, &headers, route, "/api/theme.js")
                    .await
                    .unwrap();
                let response = api::settings::theme_script(&state, user).await.unwrap();
                assert_eq!(response.headers()["content-type"], "application/javascript");
                assert_eq!(response.headers()["cache-control"], "no-store");
                let body = response.into_body().collect().await.unwrap().to_bytes();
                let body = std::str::from_utf8(&body).unwrap();
                assert!(body.contains(expected), "{body}");
                assert_eq!(body.lines().count(), 1);
                assert!(!body.contains("alert"));
            }
            headers.insert("cookie", format!("owide_session={token}").parse().unwrap());
            for (method, path) in [
                ("GET", "/api/settings"),
                ("POST", "/api/theme.js"),
                ("HEAD", "/api/theme.js"),
                ("GET", "/api/theme.js/"),
            ] {
                let segments: Vec<_> = path.strip_prefix("/api/").unwrap().split('/').collect();
                assert!(
                    authenticate_route(&state, &headers, resolve(method, &segments), path)
                        .await
                        .is_err()
                );
            }
        });
    }

    #[test]
    fn unsupported_methods_on_public_paths_skip_authentication() {
        futures::executor::block_on(async {
            let state = AppState::new().await.unwrap();
            let headers = spin_sdk::http::HeaderMap::new();
            for (method, path, public) in [
                ("GET", "/api/auth/login", true),
                ("HEAD", "/api/health", true),
                ("DELETE", "/api/auth/logout", true),
                ("GET", "/api/auth/register", true),
                ("GET", "/api/auth/login/extra", false),
                ("GET", "/api/auth/login/", false),
                ("GET", "/api/unknown", false),
                ("HEAD", "/api/settings", false),
            ] {
                let segments: Vec<_> = path.strip_prefix("/api/").unwrap().split('/').collect();
                let route = resolve(method, &segments);
                assert_eq!(route, None, "{method} {path}");
                let result = authenticate_route(&state, &headers, route, path).await;
                if public {
                    assert!(result.unwrap().is_none(), "{method} {path}");
                } else {
                    assert_eq!(result.unwrap_err().into_response().status().as_u16(), 401);
                }
            }
        });
    }

    #[test]
    fn protected_routes_require_authentication() {
        futures::executor::block_on(async {
            let state = AppState::new().await.unwrap();
            let headers = spin_sdk::http::HeaderMap::new();
            for route in [Route::Health, Route::Register, Route::Login, Route::Logout] {
                assert!(
                    authenticate_route(&state, &headers, Some(route), "")
                        .await
                        .unwrap()
                        .is_none()
                );
            }
            for route in [
                Route::ListPlugins,
                Route::RecordPlugin,
                Route::PreparePlugin,
                Route::PluginPreparation,
                Route::PluginExecutionGrants,
                Route::PluginHostRequest,
                Route::PluginContextGrants,
                Route::PluginContextHostRequest,
                Route::PluginStartInvocation,
                Route::PluginContinueInvocation,
                Route::PluginCancelInvocation,
                Route::GetProjectSkills,
                Route::ProjectSkillCommand,
                Route::GetSessionSkills,
                Route::SessionSkillCommand,
                Route::GetProjectMemories,
                Route::ProjectMemoryCommand,
                Route::GetSessionMemories,
                Route::SessionMemoryCommand,
                Route::Me,
                Route::ListConnections,
                Route::FilesGet,
                Route::SendSessionMessage,
                Route::WebFetch,
                Route::PluginJobService,
                Route::PluginRunService,
            ] {
                let error = authenticate_route(&state, &headers, Some(route), "")
                    .await
                    .unwrap_err();
                assert_eq!(error.into_response().status().as_u16(), 401);
            }
        });
    }

    #[test]
    fn route_table() {
        for (method, path, expected) in [
            ("GET", "theme.js", Route::ThemeScript),
            ("GET", "health", Route::Health),
            ("POST", "auth/register", Route::Register),
            ("POST", "auth/login", Route::Login),
            ("GET", "auth/me", Route::Me),
            ("POST", "auth/logout", Route::Logout),
            ("POST", "bridge/token", Route::BridgeToken),
            ("GET", "connections", Route::ListConnections),
            ("POST", "connections", Route::CreateConnection),
            ("PUT", "connections/5", Route::UpdateConnection),
            ("DELETE", "connections/5", Route::DeleteConnection),
            (
                "POST",
                "connections/5/tool-stream-unsupported",
                Route::SetToolStreamUnsupported,
            ),
            ("GET", "settings", Route::GetSettings),
            ("PUT", "settings", Route::SetSetting),
            ("GET", "system-prompts", Route::ListSystemPrompts),
            ("POST", "system-prompts", Route::CreateSystemPrompt),
            ("PUT", "system-prompts/5", Route::UpdateSystemPrompt),
            ("DELETE", "system-prompts/5", Route::DeleteSystemPrompt),
            ("GET", "projects/5/skills", Route::GetProjectSkills),
            ("GET", "sessions/5/monitors", Route::ScheduledMonitors),
            (
                "POST",
                "projects/5/execution-host",
                Route::BindBackgroundHost,
            ),
            ("POST", "projects/5/skills", Route::ProjectSkillCommand),
            ("GET", "sessions/5/skills", Route::GetSessionSkills),
            ("POST", "sessions/5/skills", Route::SessionSkillCommand),
            (
                "POST",
                "sessions/5/plugin-grants",
                Route::PluginExecutionGrants,
            ),
            ("POST", "sessions/5/plugin-host", Route::PluginHostRequest),
            (
                "POST",
                "plugins/execution-grants",
                Route::PluginContextGrants,
            ),
            ("POST", "plugins/host", Route::PluginContextHostRequest),
            ("POST", "plugins/invoke", Route::PluginStartInvocation),
            ("POST", "plugins/jobs/service", Route::PluginJobService),
            ("POST", "plugins/runs/service", Route::PluginRunService),
            ("POST", "plugins/continue", Route::PluginContinueInvocation),
            ("POST", "plugins/cancel", Route::PluginCancelInvocation),
            ("GET", "projects/5/memories", Route::GetProjectMemories),
            ("POST", "projects/5/memories", Route::ProjectMemoryCommand),
            ("GET", "sessions/5/memories", Route::GetSessionMemories),
            ("POST", "sessions/5/memories", Route::SessionMemoryCommand),
            ("GET", "projects", Route::ListProjects),
            (
                "GET",
                "projects/5/editor-recovery",
                Route::GetEditorRecovery,
            ),
            (
                "PUT",
                "projects/5/editor-recovery",
                Route::SaveEditorRecovery,
            ),
            ("POST", "projects", Route::CreateProject),
            ("GET", "projects/5/pending-edits", Route::ListPendingEdits),
            ("GET", "projects/5/run-changes", Route::ListRunChanges),
            ("POST", "projects/5/reviews/preview", Route::RunReview),
            ("POST", "projects/5/reviews/prepare", Route::RunReview),
            ("POST", "projects/5/reviews/complete", Route::RunReview),
            (
                "POST",
                "projects/5/pending-edits/resolve",
                Route::ResolvePendingEdit,
            ),
            ("PUT", "projects/5", Route::RenameProject),
            ("DELETE", "projects/5", Route::DeleteProject),
            ("GET", "browse", Route::Browse),
            ("GET", "sessions", Route::ListSessions),
            ("POST", "sessions/search", Route::SearchSessions),
            ("PUT", "sessions/5/preferences", Route::SessionPreferences),
            ("GET", "sessions/5/export", Route::ExportSession),
            ("POST", "sessions/5/title", Route::SessionTitle),
            ("POST", "sessions", Route::CreateSession),
            ("PUT", "sessions/5", Route::RenameSession),
            ("PUT", "sessions/5/connection", Route::SetSessionConnection),
            ("DELETE", "sessions/5", Route::DeleteSession),
            (
                "POST",
                "sessions/5/permissions/a1t1c0",
                Route::SetToolPermission,
            ),
            ("GET", "sessions/5/messages", Route::ListMessages),
            ("POST", "sessions/5/messages", Route::SendSessionMessage),
            ("POST", "sessions/5/run-plan", Route::RunPlan),
            ("POST", "sessions/5/compact", Route::CompactSession),
            ("GET", "sessions/5/goal", Route::GetGoal),
            ("POST", "sessions/5/goal", Route::UpdateGoal),
            ("POST", "sessions/5/cancel", Route::CancelSession),
            ("POST", "sessions/5/messages/persist", Route::PersistMessage),
            ("GET", "sessions/5/queue", Route::ListQueuedPrompts),
            ("POST", "sessions/5/queue", Route::EnqueuePrompt),
            ("PUT", "sessions/5/queue", Route::UpdateQueuedPrompt),
            ("DELETE", "sessions/5/queue", Route::RemoveQueuedPrompt),
            ("POST", "sessions/5/queue/send", Route::ConsumeQueuedPrompt),
            ("POST", "sessions/5/fork", Route::ForkSession),
            ("POST", "sessions/5/questions", Route::Questions),
            ("GET", "sessions/5/todos", Route::GetTodoPlan),
            ("POST", "sessions/5/todos", Route::WriteTodoPlan),
            (
                "POST",
                "sessions/5/tool-steps/upsert",
                Route::UpsertToolStep,
            ),
            (
                "POST",
                "sessions/5/tool-steps/complete",
                Route::CompleteToolStep,
            ),
            (
                "POST",
                "sessions/5/tool-steps/timing",
                Route::SaveToolTiming,
            ),
            ("GET", "models", Route::ListModels),
            ("GET", "models/context", Route::ModelContext),
            ("POST", "models/complete", Route::ModelComplete),
            ("POST", "models/background", Route::ModelBackground),
            ("POST", "models/tokens", Route::ModelTokens),
            ("POST", "chat", Route::Chat),
            ("POST", "chat-tools", Route::ChatTools),
            ("GET", "web/search", Route::WebSearch),
            ("GET", "web/fetch", Route::WebFetch),
            ("GET", "projects/5/files", Route::FilesGet),
            ("GET", "projects/5/files/read", Route::FilesGet),
            ("GET", "projects/5/files/raw", Route::FilesGet),
            ("GET", "projects/5/files/canonical", Route::FilesGet),
            ("GET", "projects/5/files/search", Route::FilesGet),
            ("GET", "projects/5/files/content-search", Route::FilesGet),
            ("PUT", "projects/5/files/write", Route::FilesPut),
            ("POST", "projects/5/files/create", Route::FilesPost),
            ("POST", "projects/5/files/copy", Route::FilesPost),
            ("DELETE", "projects/5/files/delete", Route::FilesDelete),
            ("GET", "plugin-marketplaces", Route::ListMarketplaces),
            ("POST", "plugin-marketplaces", Route::SaveMarketplaces),
            (
                "POST",
                "plugin-marketplaces/refresh",
                Route::RefreshMarketplaces,
            ),
            ("POST", "plugins/remove", Route::RemovePlugin),
            ("GET", "projects/5/plugins", Route::ListProjectPlugins),
            ("POST", "projects/5/plugins", Route::ProjectPluginCommand),
            ("POST", "projects/5/plugins/package", Route::PluginPackage),
            ("GET", "plugins", Route::ListPlugins),
            ("POST", "plugins", Route::RecordPlugin),
            ("POST", "projects/5/plugins/prepare", Route::PreparePlugin),
            ("POST", "plugins/prepare", Route::PreparePlugin),
            ("POST", "plugins/preparation", Route::PluginPreparation),
            (
                "POST",
                "projects/5/plugins/preparation",
                Route::PluginPreparation,
            ),
            ("POST", "plugins/package", Route::PluginPackage),
            ("GET", "git/status", Route::GitGet),
            ("GET", "git/path-status", Route::GitGet),
            ("GET", "projects/5/git/path-status", Route::GitGet),
            ("POST", "git/path-status", Route::GitPost),
            ("POST", "projects/5/git/path-status", Route::GitPost),
            ("POST", "git/path", Route::GitPost),
            ("POST", "projects/5/git/path", Route::GitPost),
            ("GET", "projects/5/git/status", Route::GitGet),
            ("GET", "git/diff", Route::GitGet),
            ("GET", "projects/5/git/diff", Route::GitGet),
            ("GET", "git/branches", Route::GitGet),
            ("GET", "projects/5/git/branches", Route::GitGet),
            ("GET", "git/show", Route::GitGet),
            ("GET", "projects/5/git/show", Route::GitGet),
            ("POST", "git/status", Route::GitPost),
            ("POST", "projects/5/git/status", Route::GitPost),
            ("POST", "git/diff", Route::GitPost),
            ("POST", "projects/5/git/diff", Route::GitPost),
            ("POST", "git/branches", Route::GitPost),
            ("POST", "projects/5/git/branches", Route::GitPost),
            ("POST", "git/commit", Route::GitPost),
            ("POST", "projects/5/git/commit", Route::GitPost),
            ("POST", "git/checkout", Route::GitPost),
            ("POST", "projects/5/git/checkout", Route::GitPost),
            ("POST", "git/sync", Route::GitPost),
            ("POST", "projects/5/git/sync", Route::GitPost),
        ] {
            let segments: Vec<_> = path.split('/').collect();
            assert_eq!(
                resolve(method, &segments),
                Some(expected),
                "{method} {path}"
            );
            assert_eq!(
                csrf_allowed(
                    method,
                    &format!("/api/{path}"),
                    &spin_sdk::http::HeaderMap::new()
                ),
                expected == Route::ThemeScript,
                "{method} {path}"
            );
            assert_eq!(
                expected.is_public(),
                matches!(
                    path,
                    "health" | "auth/register" | "auth/login" | "auth/logout"
                )
            );
        }
        for (method, path) in [
            ("DELETE", "sessions/5/anything"),
            ("GET", "unknown"),
            ("GET", "projects/5/pending-edits/extra"),
            ("GET", "projects/no/pending-edits"),
            ("GET", "projects/5/pending-edits/resolve"),
            ("POST", "projects/5/pending-edits/resolve/extra"),
            ("PUT", "projects/5/pending-edits/resolve"),
            ("PUT", "projects/5/files/wrong"),
            ("POST", "sessions/5/permissions/x/anything"),
        ] {
            assert_eq!(resolve(method, &path.split('/').collect::<Vec<_>>()), None);
        }
    }
}
