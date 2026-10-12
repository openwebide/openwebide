use crate::backend::{Backend, RecoveryError};
use futures::future::LocalBoxFuture;
use leptos::prelude::RwSignal;
use openwebide_core::{
    ChatCompletion, ChatMessage, ChatRequest, ChatSession, Connection, ConversationEntry,
    EditDecision, EditorContext, FileDiff, FileEntry, GitBranchInfo, GitCheckoutRequest,
    GitCheckoutResult, GitCommitRequest, GitCommitResult, GitRepoStatus, GitSyncRequest,
    GitSyncResult, Health, ModelInfo, PersistedEdit, Project, ProviderKind, ResolveEditRequest,
    Role, RunEvent, SearchHit, SystemPrompt, TurnTelemetry, User, WebSearchResult, WorkspaceMode,
    vfs::SearchOptions,
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, VecDeque},
};
use web_sys::AbortSignal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    SetSetting {
        key: String,
        value: String,
    },
    SetPermission {
        session: i64,
        id: String,
        approved: bool,
    },
    WriteFile {
        path: String,
        content: String,
    },
    CopyFile {
        from: String,
        to: String,
    },
    DeleteFile {
        path: String,
    },
    SendMessage {
        session: i64,
        content: String,
        model: Option<String>,
    },
    CancelSession {
        session: i64,
    },
    Request {
        method: &'static str,
    },
}

pub type Deferred<T> = futures::channel::oneshot::Receiver<Result<T, String>>;

type SettingsLoad = futures::channel::oneshot::Receiver<Result<BTreeMap<String, String>, String>>;

#[derive(Default)]
pub struct FakeBackend {
    pub bridge_credential: RefCell<Option<(String, i64)>>,
    pub marketplaces: RefCell<openwebide_core::plugins::marketplace::MarketplaceSettings>,
    pub marketplace_results:
        RefCell<VecDeque<Deferred<openwebide_core::plugins::marketplace::MarketplaceRefresh>>>,
    pub plugin_packages: RefCell<VecDeque<Deferred<openwebide_core::plugins::PluginPackage>>>,
    pub plugin_authorities: RefCell<
        BTreeMap<
            String,
            (
                openwebide_core::plugins::execution::PluginExecutionContext,
                openwebide_core::plugins::PreparedPlugin,
            ),
        >,
    >,
    pub plugin_actor_contexts:
        RefCell<BTreeMap<String, openwebide_core::plugins::execution::PluginExecutionContext>>,
    pub panel_coordination: std::cell::Cell<bool>,
    pub panel_begins: RefCell<Vec<openwebide_core::plugins::panels::coordination::BeginPanel>>,
    pub panel_finishes: RefCell<Vec<openwebide_core::plugins::panels::coordination::FinishPanel>>,
    pub panel_closes: RefCell<Vec<String>>,
    pub panel_document_results:
        RefCell<VecDeque<Deferred<openwebide_core::plugins::panels::PanelDocument>>>,
    pub panel_documents: RefCell<VecDeque<openwebide_core::plugins::panels::PanelDocument>>,
    pub plugin_action_calls: RefCell<Vec<(Option<i64>, openwebide_core::ToolCall)>>,
    pub plugin_actors: RefCell<BTreeMap<String, openwebide_core::plugins::execution::InvokePlugin>>,
    pub plugin_commands: RefCell<Vec<(i64, openwebide_core::plugins::ProjectPluginCommand)>>,
    pub project_plugin_entries:
        RefCell<BTreeMap<i64, Vec<openwebide_core::plugins::ProjectPlugin>>>,
    pub project_plugin_results:
        RefCell<VecDeque<Deferred<Vec<openwebide_core::plugins::ProjectPlugin>>>>,
    pub plugins: RefCell<Vec<openwebide_core::plugins::PluginInstallation>>,
    pub plugin_loads:
        RefCell<VecDeque<Deferred<Vec<openwebide_core::plugins::PluginInstallation>>>>,
    pub plugin_preparations: RefCell<VecDeque<Deferred<openwebide_core::plugins::PreparedPlugin>>>,
    pub plugin_progress:
        RefCell<VecDeque<Deferred<openwebide_core::plugins::preparation::PluginPreparation>>>,
    pub plugin_preparation_commands: RefCell<
        Vec<(
            Option<i64>,
            openwebide_core::plugins::preparation::PreparationCommand,
        )>,
    >,
    pub plugin_requests: RefCell<Vec<(Option<i64>, openwebide_core::plugins::PluginSource)>>,
    pub plugin_records: RefCell<Vec<openwebide_core::plugins::RecordPlugin>>,
    pub questions: RefCell<Vec<openwebide_core::questions::AgentQuestion>>,
    pub question_commands: RefCell<Vec<(i64, openwebide_core::questions::QuestionCommand)>>,
    pub question_loads: RefCell<VecDeque<Deferred<openwebide_core::questions::QuestionResult>>>,
    pub host_connection: RefCell<openwebide_core::host_admin::HostConnection>,
    pub host_update_requests: RefCell<Vec<openwebide_core::host_admin::updates::UpdateRequest>>,
    pub host_update_results:
        RefCell<VecDeque<Deferred<openwebide_core::host_admin::updates::UpdateResponse>>>,
    pub host_operations: RefCell<Vec<openwebide_core::host_admin::HostOperation>>,
    pub host_results: RefCell<VecDeque<Deferred<openwebide_core::host_admin::HostResponse>>>,
    pub host_requests: RefCell<Vec<(i64, openwebide_core::host_admin::HostRequest)>>,
    pub host_inputs: RefCell<Vec<(i64, i64, String)>>,
    pub host_encrypted_inputs: RefCell<Vec<openwebide_core::host_admin::HostInput>>,
    pub host_input_key_results:
        RefCell<VecDeque<Deferred<openwebide_core::host_admin::HostInputKey>>>,
    host_input_recipients: RefCell<
        BTreeMap<String, openwebide_core::host_admin::private_input::PrivateInputRecipient>,
    >,
    pub assistance_requests: RefCell<Vec<openwebide_core::AssistanceRequest>>,
    pub assistance_results: RefCell<VecDeque<Deferred<Option<String>>>>,
    pub monitors: RefCell<BTreeMap<i64, Vec<openwebide_core::scheduled::ScheduledTask>>>,
    pub monitor_results:
        RefCell<VecDeque<Deferred<Vec<openwebide_core::scheduled::ScheduledTask>>>>,
    pub scheduled: RefCell<Vec<openwebide_core::scheduled::ScheduledTask>>,
    pub scheduled_load_results:
        RefCell<VecDeque<Deferred<Vec<openwebide_core::scheduled::ScheduledTask>>>>,
    pub scheduled_commands: RefCell<Vec<(Option<i64>, openwebide_core::scheduled::TaskCommand)>>,
    pub skills: RefCell<BTreeMap<i64, openwebide_core::ProjectSkills>>,
    pub skill_load_results: RefCell<VecDeque<Deferred<openwebide_core::ProjectSkills>>>,
    pub skill_command_results: RefCell<VecDeque<Deferred<openwebide_core::ProjectSkills>>>,
    pub skill_commands: RefCell<Vec<(i64, openwebide_core::SkillCommand, bool)>>,
    pub memories: RefCell<BTreeMap<i64, openwebide_core::ProjectMemories>>,
    pub memory_load_results: RefCell<VecDeque<Deferred<openwebide_core::ProjectMemories>>>,
    pub memory_command_results: RefCell<VecDeque<Deferred<openwebide_core::ProjectMemories>>>,
    pub memory_commands: RefCell<Vec<(i64, openwebide_core::MemoryCommand, bool)>>,
    pub goals: RefCell<BTreeMap<i64, openwebide_core::Goal>>,
    pub goal_bindings: RefCell<Vec<Option<openwebide_core::scheduled::HostBinding>>>,
    pub goal_load_results: RefCell<VecDeque<Deferred<Option<openwebide_core::Goal>>>>,
    pub compact_results: RefCell<VecDeque<Deferred<ChatMessage>>>,
    pub compact_error: RefCell<Option<String>>,

    pub editor_recovery_records:
        RefCell<BTreeMap<i64, openwebide_core::editor::EditorRecoveryRecord>>,
    pub recovery_load_results: RefCell<
        VecDeque<
            futures::channel::oneshot::Receiver<
                Result<openwebide_core::editor::EditorRecoveryRecord, RecoveryError>,
            >,
        >,
    >,
    pub recovery_save_results:
        RefCell<VecDeque<futures::channel::oneshot::Receiver<Result<(), RecoveryError>>>>,

    pub session_search_results: RefCell<VecDeque<Deferred<Vec<ChatSession>>>>,
    pub session_export_results: RefCell<VecDeque<Deferred<openwebide_core::SessionExport>>>,
    pub title_results: RefCell<VecDeque<Deferred<Option<ChatSession>>>>,
    pub todo_updates: RefCell<BTreeMap<i64, Vec<openwebide_core::TodoUpdate>>>,
    pub todo_load_results: RefCell<VecDeque<Deferred<Option<openwebide_core::TodoUpdate>>>>,
    pub todo_errors: RefCell<VecDeque<String>>,

    pub fork_results: RefCell<VecDeque<Deferred<openwebide_core::ForkedSession>>>,
    pub fork_errors: RefCell<VecDeque<String>>,
    pub queued_prompts: RefCell<BTreeMap<i64, Vec<openwebide_core::QueuedPrompt>>>,
    pub queue_load_results: RefCell<VecDeque<Deferred<Vec<openwebide_core::QueuedPrompt>>>>,
    pub queue_errors: RefCell<VecDeque<String>>,
    pub queue_next_id: std::cell::Cell<i64>,
    pub run_changes: RefCell<BTreeMap<i64, Vec<openwebide_core::RunChange>>>,
    pub reviews: RefCell<BTreeMap<i64, openwebide_core::ReviewPlan>>,
    pub review_results: RefCell<VecDeque<Deferred<openwebide_core::ReviewPlan>>>,
    pub review_history: RefCell<
        Vec<(
            i64,
            openwebide_core::ReviewRequest,
            openwebide_core::RunChange,
        )>,
    >,
    pub rewinds: RefCell<BTreeMap<i64, openwebide_core::RewindPlan>>,
    pub git_diffs: RefCell<VecDeque<Result<String, String>>>,
    pub git_path_results: RefCell<VecDeque<Deferred<openwebide_core::git::GitPathChanges>>>,
    pub git_path_requests: RefCell<Vec<(Option<i64>, openwebide_core::git::GitPathRequest)>>,
    pub git_path_status_requests: RefCell<Vec<Option<i64>>>,
    pub git_statuses: RefCell<VecDeque<Deferred<GitRepoStatus>>>,
    pub git_stashes: RefCell<openwebide_core::git::GitStashResult>,
    pub git_stash_requests: RefCell<Vec<openwebide_core::git::GitStashRequest>>,
    pub git_index_diff: RefCell<String>,
    pub git_history: RefCell<openwebide_core::git::GitHistoryPage>,
    pub git_history_results: RefCell<VecDeque<Deferred<openwebide_core::git::GitHistoryPage>>>,
    pub git_history_requests: RefCell<Vec<openwebide_core::git::GitHistoryRequest>>,
    pub git_commit_diff: RefCell<Option<openwebide_core::git::GitCommitDiff>>,
    pub git_commit_diff_results: RefCell<VecDeque<Deferred<openwebide_core::git::GitCommitDiff>>>,
    pub git_commit_diff_requests: RefCell<Vec<openwebide_core::git::GitCommitDiffRequest>>,
    pub git_commit_results: RefCell<VecDeque<Deferred<GitCommitResult>>>,
    pub git_commit_requests: RefCell<Vec<GitCommitRequest>>,
    pub git_sync_results: RefCell<VecDeque<Deferred<GitSyncResult>>>,
    pub git_sync_requests: RefCell<Vec<GitSyncRequest>>,
    pub git_branches: RefCell<Vec<GitBranchInfo>>,
    pub git_branches_results: RefCell<VecDeque<Deferred<Vec<GitBranchInfo>>>>,
    pub git_checkout_results: RefCell<VecDeque<Deferred<GitCheckoutResult>>>,
    pub git_checkout_requests: RefCell<Vec<(Option<i64>, GitCheckoutRequest)>>,
    pub git_status_requests: RefCell<Vec<Option<i64>>>,
    pub model_setup: RefCell<openwebide_core::ModelSetup>,
    pub model_default_requests: RefCell<Vec<openwebide_core::ModelDefaults>>,
    pub model_default_results: RefCell<VecDeque<Deferred<openwebide_core::ModelSetup>>>,
    pub detections: RefCell<BTreeMap<(i64, String), openwebide_core::ModelDetection>>,
    pub test_results: RefCell<VecDeque<Result<openwebide_core::ModelTestResult, String>>>,
    pub server_settings_results: RefCell<VecDeque<Deferred<openwebide_core::ServerSettings>>>,
    pub server_settings: RefCell<BTreeMap<i64, openwebide_core::ServerSettings>>,
    pub endpoint_latency_ms: RefCell<i32>,
    pub project_results: RefCell<VecDeque<Deferred<Vec<Project>>>>,
    pub connection_results: RefCell<VecDeque<Deferred<Vec<Connection>>>>,
    pub session_results: RefCell<VecDeque<Deferred<Vec<ChatSession>>>>,
    pub prompt_results: RefCell<VecDeque<Deferred<Vec<SystemPrompt>>>>,
    pub model_results: RefCell<VecDeque<Deferred<Vec<ModelInfo>>>>,
    pub context_results: RefCell<VecDeque<Deferred<Option<usize>>>>,
    pub search_results: RefCell<VecDeque<Deferred<Vec<SearchHit>>>>,
    pub search_requests: RefCell<Vec<(i64, String, SearchOptions)>>,
    pub model_requests: RefCell<Vec<i64>>,
    pub context_requests: RefCell<Vec<(i64, Option<String>)>>,
    pub tool_timings: RefCell<BTreeMap<(i64, String), openwebide_core::ToolTiming>>,
    pub tool_sources: RefCell<BTreeMap<(i64, String), bool>>,
    pub message_save_results: RefCell<VecDeque<Result<(), String>>>,
    pub step_save_error: RefCell<Option<String>>,
    pub completion_error: RefCell<Option<String>>,
    pub persisted_edits: RefCell<BTreeMap<(i64, String), PersistedEdit>>,
    pub resolution_error: RefCell<Option<String>>,
    pub file_write_results: RefCell<VecDeque<Deferred<()>>>,
    pub file_read_results: RefCell<VecDeque<Deferred<String>>>,
    pub file_list_results: RefCell<VecDeque<Deferred<Vec<FileEntry>>>>,
    pub pending_results: RefCell<VecDeque<Deferred<Vec<PersistedEdit>>>>,
    pub resolution_results: RefCell<VecDeque<Deferred<()>>>,
    pub resolution_response_results: RefCell<VecDeque<Deferred<()>>>,
    pub resolution_requests: RefCell<Vec<(i64, ResolveEditRequest)>>,
    pub file_error: RefCell<Option<String>>,
    pub sessions: RefCell<Vec<ChatSession>>,
    pub messages: RefCell<BTreeMap<i64, Vec<ConversationEntry>>>,
    pub projects: RefCell<Vec<Project>>,
    pub project_delete_error: RefCell<Option<String>>,
    pub browse_entries: RefCell<BTreeMap<String, Vec<FileEntry>>>,
    pub files: RefCell<BTreeMap<(i64, String), String>>,
    pub binary_files: RefCell<BTreeMap<(i64, String), Vec<u8>>>,
    pub directories: RefCell<BTreeSet<(i64, String)>>,
    pub models: RefCell<Vec<ModelInfo>>,
    pub connections: RefCell<Vec<Connection>>,
    pub system_prompts: RefCell<Vec<SystemPrompt>>,
    pub prompt_save_results: RefCell<VecDeque<Deferred<SystemPrompt>>>,
    pub prompt_delete_results: RefCell<VecDeque<Deferred<()>>>,
    pub panel_save_results:
        RefCell<VecDeque<futures::channel::oneshot::Receiver<Result<(), String>>>>,
    pub settings: RefCell<BTreeMap<String, String>>,
    pub editor_save_results: RefCell<VecDeque<Deferred<()>>>,
    pub settings_load_error: RefCell<Option<String>>,
    pub settings_load_started: RefCell<Option<futures::channel::oneshot::Sender<()>>>,
    pub settings_load_results: RefCell<VecDeque<SettingsLoad>>,
    pub history_save_results:
        RefCell<VecDeque<futures::channel::oneshot::Receiver<Result<(), String>>>>,
    pub background_completion: RefCell<Option<Result<ChatCompletion, String>>>,
    pub background_requests: RefCell<Vec<ChatRequest>>,
    pub scripted_completions: RefCell<VecDeque<ChatCompletion>>,
    pub completion_requests: RefCell<Vec<ChatRequest>>,
    pub scripted_events: RefCell<VecDeque<Vec<RunEvent>>>,
    pub calls: RefCell<Vec<Call>>,
    pub session_expired: RwSignal<bool>,
}

impl FakeBackend {
    fn fixture_skill_records(
        &self,
        project: i64,
        request: &openwebide_core::plugins::records::RecordRequest,
    ) -> Result<String, String> {
        use openwebide_core::plugins::records::RecordOperation;
        let mut all = self.skills.borrow_mut();
        let data = all.entry(project).or_default();
        match &request.operation {
            RecordOperation::Create { value } => {
                let draft = serde_json::from_value(value["draft"].clone())
                    .map_err(|error| error.to_string())?;
                let id = data.entries.iter().map(|entry| entry.id).max().unwrap_or(0) + 1;
                data.entries.insert(
                    0,
                    openwebide_core::ProjectSkill {
                        plugin: None,
                        id,
                        revision: 1,
                        updated_at: 0,
                        draft,
                    },
                );
            }
            RecordOperation::Update {
                id,
                revision,
                value,
            } => {
                let entry = data
                    .entries
                    .iter_mut()
                    .find(|entry| entry.id == *id && entry.revision == *revision)
                    .ok_or("Skill changed. Refresh before editing.")?;
                entry.draft = serde_json::from_value(value["draft"].clone())
                    .map_err(|error| error.to_string())?;
                entry.revision += 1;
            }
            RecordOperation::Delete { id, revision } => {
                let index = data
                    .entries
                    .iter()
                    .position(|entry| entry.id == *id && entry.revision == *revision)
                    .ok_or("Skill changed. Refresh before deleting.")?;
                data.entries.remove(index);
            }
            _ => {}
        }
        let records: Vec<_> = data.entries.iter().map(|entry| serde_json::json!({"id":entry.id,"revision":entry.revision,"updated_at":entry.updated_at,"value":{"draft":entry.draft,"origin":entry.plugin}})).collect();
        Ok(serde_json::json!({"enabled":data.enabled,"records":records,"next":null}).to_string())
    }

    fn fixture_tasks<'a>(
        &'a self,
        project: Option<i64>,
        command: &'a openwebide_core::scheduled::TaskCommand,
        _binding: Option<&'a openwebide_core::scheduled::HostBinding>,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::scheduled::ScheduledTask>, String>> {
        Box::pin(async move {
            use openwebide_core::scheduled::{ScheduledTask, TaskCommand};
            {
                let mut entries = self.scheduled.borrow_mut();
                match command {
                    TaskCommand::Monitor { .. } => {
                        return Err("Monitor test adapter not configured".into());
                    }
                    TaskCommand::List => {}
                    TaskCommand::Create { draft } => {
                        draft.validate(openwebide_core::now_seconds(js_sys::Date::now()))?;
                        let id = entries.iter().map(|task| task.id).max().unwrap_or(0) + 1;
                        entries.push(ScheduledTask {
                            id,
                            revision: 1,
                            project_id: project,
                            draft: draft.clone(),
                            next_run: draft
                                .schedule
                                .next_after(openwebide_core::now_seconds(js_sys::Date::now()))?,
                            host_id: "server".into(),
                            host_available: true,
                            last_run: None,
                        });
                    }
                    TaskCommand::Update {
                        id,
                        revision,
                        draft,
                    } => {
                        let task = entries
                            .iter_mut()
                            .find(|task| {
                                task.id == *id
                                    && task.revision == *revision
                                    && task.project_id == project
                            })
                            .ok_or("Task changed")?;
                        task.draft = draft.clone();
                        task.revision += 1;
                    }
                    TaskCommand::SetEnabled {
                        id,
                        revision,
                        enabled,
                    } => {
                        let task = entries
                            .iter_mut()
                            .find(|task| {
                                task.id == *id
                                    && task.revision == *revision
                                    && task.project_id == project
                            })
                            .ok_or("Task changed")?;
                        task.draft.enabled = *enabled;
                        task.revision += 1;
                    }
                    TaskCommand::Delete { id, revision } => {
                        let pos = entries
                            .iter()
                            .position(|task| {
                                task.id == *id
                                    && task.revision == *revision
                                    && task.project_id == project
                            })
                            .ok_or("Task changed")?;
                        entries.remove(pos);
                    }
                }
            }
            self.scheduled_tasks(project).await
        })
    }
    fn fixture_monitor<'a>(
        &'a self,
        session: i64,
        command: &'a openwebide_core::scheduled::TaskCommand,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::scheduled::ScheduledTask>, String>> {
        Box::pin(async move {
            use openwebide_core::scheduled::{MonitorCommand, TaskCommand};
            let pending = self.monitor_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending
                    .await
                    .unwrap_or_else(|_| Err("response dropped".into()));
            }
            let TaskCommand::Monitor { command, .. } = command else {
                return Err("Expected a monitor command".into());
            };
            let mut entries = self.monitors.borrow_mut();
            let tasks = entries.entry(session).or_default();
            match command {
                MonitorCommand::List {} => {}
                MonitorCommand::Cancel { id, revision } => {
                    let pos = tasks
                        .iter()
                        .position(|task| task.id == *id && task.revision == *revision)
                        .ok_or("Monitor changed")?;
                    tasks.remove(pos);
                }
                MonitorCommand::Start { .. } => return Err("Seed monitors to test creation".into()),
            }
            Ok(tasks.clone())
        })
    }

    fn read(&self, project: i64, path: &str) -> Result<String, String> {
        self.files
            .borrow()
            .get(&(project, path.to_string()))
            .cloned()
            .ok_or_else(|| format!("file not found: {path}"))
    }
}

impl Backend for FakeBackend {
    fn host_updates<'a>(
        &'a self,
        request: &'a openwebide_core::host_admin::updates::UpdateRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::host_admin::updates::UpdateResponse, String>>
    {
        self.host_update_requests.borrow_mut().push(request.clone());
        let response = self.host_update_results.borrow_mut().pop_front();
        Box::pin(async move {
            match response {
                Some(response) => response
                    .await
                    .map_err(|_| "Host update fixture cancelled".to_owned())?,
                None => Err("No host update fixture".into()),
            }
        })
    }
    fn plugin_marketplaces(
        &self,
    ) -> LocalBoxFuture<
        '_,
        Result<openwebide_core::plugins::marketplace::MarketplaceSettings, String>,
    > {
        Box::pin(async { Ok(self.marketplaces.borrow().clone()) })
    }
    fn save_plugin_marketplaces<'a>(
        &'a self,
        request: &'a openwebide_core::plugins::marketplace::SaveMarketplaces,
    ) -> LocalBoxFuture<
        'a,
        Result<openwebide_core::plugins::marketplace::MarketplaceSettings, String>,
    > {
        Box::pin(async move {
            let next = openwebide_core::plugins::marketplace::save_sources(
                self.marketplaces.borrow().clone(),
                request,
            )
            .map_err(|e| e.to_string())?;
            *self.marketplaces.borrow_mut() = next.clone();
            Ok(next)
        })
    }
    fn refresh_plugin_marketplaces(
        &self,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::plugins::marketplace::MarketplaceRefresh, String>>
    {
        Box::pin(async {
            let pending = self.marketplace_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending.await.map_err(|e| e.to_string())?;
            }
            Ok(openwebide_core::plugins::marketplace::MarketplaceRefresh {
                settings: self.marketplaces.borrow().clone(),
                failures: Vec::new(),
            })
        })
    }
    fn plugin_context_grants<'a>(
        &'a self,
        request: &'a openwebide_core::plugins::execution::PluginGrantRequest,
    ) -> LocalBoxFuture<'a, Result<BTreeMap<String, String>, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "plugin_context_grants",
            });
            let mut grants = BTreeMap::new();
            for plugin in &request.plugins {
                let bindings = match request.context.project_id {
                    Some(project) => self
                        .project_plugin_entries
                        .borrow()
                        .get(&project)
                        .cloned()
                        .unwrap_or_default(),
                    None => openwebide_core::plugins::default_bindings(&self.plugins.borrow()),
                };
                if !bindings
                    .iter()
                    .any(|binding| binding.enabled && binding.prepared.digest == plugin.digest)
                {
                    return Err("Plugin is no longer enabled".into());
                }
                let token = format!("grant-{}", self.plugin_authorities.borrow().len());
                self.plugin_authorities
                    .borrow_mut()
                    .insert(token.clone(), (request.context.clone(), plugin.clone()));
                grants.insert(plugin.digest.clone(), token);
            }
            Ok(grants)
        })
    }
    fn begin_plugin_panel<'a>(
        &'a self,
        request: &'a openwebide_core::plugins::panels::coordination::BeginPanel,
    ) -> LocalBoxFuture<
        'a,
        Result<openwebide_core::plugins::panels::coordination::PanelClaim, String>,
    > {
        Box::pin(async move {
            self.panel_begins.borrow_mut().push(request.clone());
            if !self.panel_coordination.get() {
                return Err("Panel coordination unavailable".into());
            }
            Ok(
                openwebide_core::plugins::panels::coordination::PanelClaim::Claimed {
                    input: request.input.clone(),
                },
            )
        })
    }
    fn finish_plugin_panel<'a>(
        &'a self,
        request: &'a openwebide_core::plugins::panels::coordination::FinishPanel,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.panel_finishes.borrow_mut().push(request.clone());
            Ok(())
        })
    }
    fn close_plugin_panel<'a>(
        &'a self,
        instance: &'a str,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.panel_closes.borrow_mut().push(instance.into());
            Ok(())
        })
    }
    fn start_plugin_invocation<'a>(
        &'a self,
        request: &'a openwebide_core::plugins::execution::PluginStartRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::plugins::execution::PluginInvocation, String>>
    {
        Box::pin(async move {
            use openwebide_core::plugins::execution::*;
            self.calls.borrow_mut().push(Call::Request {
                method: "start_plugin_invocation",
            });
            let id = format!("actor-{}", self.calls.borrow().len());
            let context = self
                .plugin_authorities
                .borrow()
                .get(&request.grant)
                .ok_or("Missing authority")?
                .0
                .clone();
            self.plugin_action_calls.borrow_mut().push((
                context.project_id,
                openwebide_core::ToolCall {
                    id: id.clone(),
                    name: request.call.name.clone(),
                    arguments: request.call.arguments.clone(),
                },
            ));
            self.plugin_actor_contexts
                .borrow_mut()
                .insert(id.clone(), context);
            self.plugin_actors
                .borrow_mut()
                .insert(id.clone(), request.call.clone());
            Ok(PluginInvocation {
                id,
                step: PluginStep::Ready,
            })
        })
    }
    fn continue_plugin_invocation<'a>(
        &'a self,
        request: &'a openwebide_core::plugins::execution::ContinuePlugin,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::plugins::execution::PluginInvocation, String>>
    {
        Box::pin(async move {
            use openwebide_core::plugins::execution::*;
            self.calls.borrow_mut().push(Call::Request {
                method: "continue_plugin_invocation",
            });
            let call = self
                .plugin_actors
                .borrow()
                .get(&request.id)
                .cloned()
                .ok_or("Missing actor")?;
            if matches!(call.operation, PluginOperation::Panel) {
                let pending = self.panel_document_results.borrow_mut().pop_front();
                let document = if let Some(pending) = pending {
                    pending.await.map_err(|error| error.to_string())??
                } else {
                    self.panel_documents
                        .borrow_mut()
                        .pop_front()
                        .ok_or("Missing panel document fixture")?
                };
                self.plugin_actors.borrow_mut().remove(&request.id);
                self.plugin_actor_contexts.borrow_mut().remove(&request.id);
                return Ok(PluginInvocation {
                    id: request.id.clone(),
                    step: PluginStep::Complete {
                        ok: true,
                        content: serde_json::to_string(&document).unwrap(),
                        summary: "Fixture panel document".into(),
                    },
                });
            }
            if call.name.starts_with("schedule_") || call.name == "monitor" {
                let mut args: serde_json::Value =
                    serde_json::from_str(&call.arguments).map_err(|error| error.to_string())?;
                let context = self
                    .plugin_actor_contexts
                    .borrow_mut()
                    .remove(&request.id)
                    .ok_or("Missing fixture context")?;
                self.plugin_actors.borrow_mut().remove(&request.id);
                let result = if call.name == "monitor" {
                    let session = args["session_id"]
                        .as_i64()
                        .ok_or("Missing monitor conversation")?;
                    args.as_object_mut().unwrap().remove("session_id");
                    let command = openwebide_core::scheduled::TaskCommand::Monitor {
                        session_id: session,
                        command: serde_json::from_value(args).map_err(|error| error.to_string())?,
                    };
                    self.fixture_monitor(session, &command).await
                } else {
                    args.as_object_mut().unwrap().insert(
                        "action".into(),
                        serde_json::json!(call.name.strip_prefix("schedule_").unwrap()),
                    );
                    let command =
                        serde_json::from_value(args).map_err(|error| error.to_string())?;
                    self.fixture_tasks(context.project_id, &command, None).await
                };
                return Ok(PluginInvocation {
                    id: request.id.clone(),
                    step: match result {
                        Ok(value) => PluginStep::Complete {
                            ok: true,
                            content: serde_json::to_string(&value).unwrap(),
                            summary: "Fixture task action".into(),
                        },
                        Err(error) => PluginStep::Complete {
                            ok: false,
                            content: error,
                            summary: "Fixture task failure".into(),
                        },
                    },
                });
            }
            let step = if request.sequence == 0 {
                let args: serde_json::Value =
                    serde_json::from_str(&call.arguments).map_err(|error| error.to_string())?;
                let action = call
                    .name
                    .strip_prefix("memory_")
                    .ok_or("Unknown fixture tool")?;
                let value = serde_json::json!({"title":args["title"],"content":args["content"],"auto_title":args["auto_title"]});
                let operation = match action {
                    "create" => serde_json::json!({"action":action,"value":value}),
                    "update" => {
                        serde_json::json!({"action":action,"id":args["id"],"revision":args["revision"],"value":value})
                    }
                    _ => {
                        serde_json::json!({"action":action,"id":args["id"],"revision":args["revision"]})
                    }
                };
                PluginStep::HostCall {
                    sequence: 1,
                    capability: "collections".into(),
                    payload: serde_json::json!({"collection":"memories","operation":operation})
                        .to_string(),
                }
            } else {
                self.plugin_actors.borrow_mut().remove(&request.id);
                self.plugin_actor_contexts.borrow_mut().remove(&request.id);
                match &request.response {
                    Ok(content) => PluginStep::Complete {
                        ok: true,
                        content: content.clone(),
                        summary: "Stored".into(),
                    },
                    Err(error) => PluginStep::Complete {
                        ok: false,
                        content: error.clone(),
                        summary: "Failed".into(),
                    },
                }
            };
            Ok(PluginInvocation {
                id: request.id.clone(),
                step,
            })
        })
    }
    fn cancel_plugin_invocation<'a>(
        &'a self,
        id: &'a str,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "cancel_plugin_invocation",
            });
            self.plugin_actors.borrow_mut().remove(id);
            self.plugin_actor_contexts.borrow_mut().remove(id);
            Ok(())
        })
    }
    fn plugin_host_request<'a>(
        &'a self,
        session: i64,
        request: &'a openwebide_core::plugins::execution::PluginHostRequest,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "plugin_host_request",
            });
            if self
                .plugin_authorities
                .borrow()
                .get(&request.grant)
                .and_then(|(scope, _)| scope.session_id)
                != Some(session)
            {
                return Err("Plugin conversation changed".into());
            }
            self.plugin_context_host_request(request).await
        })
    }
    fn plugin_context_host_request<'a>(
        &'a self,
        request: &'a openwebide_core::plugins::execution::PluginHostRequest,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            use openwebide_core::plugins::records::RecordOperation;
            self.calls.borrow_mut().push(Call::Request {
                method: "plugin_context_host_request",
            });
            let (scope, _) = self
                .plugin_authorities
                .borrow()
                .get(&request.grant)
                .cloned()
                .ok_or("Missing grant")?;
            let project = scope.project_id.ok_or("Missing project")?;
            let command: openwebide_core::plugins::records::RecordRequest =
                serde_json::from_str(&request.payload).map_err(|error| error.to_string())?;
            if command.collection == "skills" {
                return self.fixture_skill_records(project, &command);
            }
            let pending = self.memory_command_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                pending.await.map_err(|error| error.to_string())??;
            }
            let mut all = self.memories.borrow_mut();
            let data = all.entry(project).or_default();
            if !data.enabled
                && !scope.user_action
                && !matches!(
                    command.operation,
                    RecordOperation::List { .. } | RecordOperation::Read { .. }
                )
            {
                return Err("Memory is disabled".into());
            }
            match command.operation {
                RecordOperation::Create { value } => {
                    data.entries.insert(
                        0,
                        openwebide_core::ProjectMemory {
                            id: data.entries.iter().map(|entry| entry.id).max().unwrap_or(0) + 1,
                            revision: 1,
                            updated_at: 0,
                            title: value["title"].as_str().unwrap_or_default().into(),
                            content: value["content"].as_str().unwrap_or_default().into(),
                            auto_title: value["auto_title"].as_bool().unwrap_or(false),
                        },
                    );
                }
                RecordOperation::Update {
                    id,
                    revision,
                    value,
                } => {
                    let entry = data
                        .entries
                        .iter_mut()
                        .find(|entry| entry.id == id && entry.revision == revision)
                        .ok_or("Memory changed. Refresh before editing.")?;
                    entry.title = value["title"].as_str().unwrap_or_default().into();
                    entry.content = value["content"].as_str().unwrap_or_default().into();
                    entry.auto_title = value["auto_title"].as_bool().unwrap_or(false);
                    entry.revision += 1;
                }
                RecordOperation::Delete { id, revision } => {
                    let index = data
                        .entries
                        .iter()
                        .position(|entry| entry.id == id && entry.revision == revision)
                        .ok_or("Memory changed. Refresh before deleting.")?;
                    data.entries.remove(index);
                }
                _ => {}
            }
            let records: Vec<_> = data.entries.iter().map(|entry| serde_json::json!({"id":entry.id,"revision":entry.revision,"updated_at":entry.updated_at,"value":{"title":entry.title,"content":entry.content,"auto_title":entry.auto_title}})).collect();
            Ok(
                serde_json::json!({"enabled":data.enabled,"records":records,"next":null})
                    .to_string(),
            )
        })
    }
    fn project_plugins(
        &self,
        project: i64,
    ) -> LocalBoxFuture<'_, Result<Vec<openwebide_core::plugins::ProjectPlugin>, String>> {
        Box::pin(async move {
            Ok(self
                .project_plugin_entries
                .borrow()
                .get(&project)
                .cloned()
                .unwrap_or_default())
        })
    }
    fn plugin_package<'a>(
        &'a self,
        _project: Option<i64>,
        _expected: &'a openwebide_core::plugins::PreparedPlugin,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::plugins::PluginPackage, String>> {
        Box::pin(async {
            let pending = self
                .plugin_packages
                .borrow_mut()
                .pop_front()
                .ok_or("No package fixture prepared")?;
            pending.await.map_err(|e| e.to_string())?
        })
    }
    fn project_plugin_command<'a>(
        &'a self,
        project: i64,
        command: &'a openwebide_core::plugins::ProjectPluginCommand,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::plugins::ProjectPlugin>, String>> {
        Box::pin(async move {
            self.plugin_commands
                .borrow_mut()
                .push((project, command.clone()));
            let pending = self
                .project_plugin_results
                .borrow_mut()
                .pop_front()
                .ok_or("No activation fixture prepared")?;
            let entries = pending.await.map_err(|e| e.to_string())??;
            self.project_plugin_entries
                .borrow_mut()
                .insert(project, entries.clone());
            Ok(entries)
        })
    }
    fn remove_plugin<'a>(
        &'a self,
        request: &'a openwebide_core::plugins::RemovePlugin,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::plugins::PluginInstallation>, String>> {
        Box::pin(async move {
            let mut entries = self.plugins.borrow_mut();
            let index = entries
                .iter()
                .position(|e| e.prepared.source == request.source && e.revision == request.revision)
                .ok_or("Installation changed")?;
            entries.remove(index);
            for bindings in self.project_plugin_entries.borrow_mut().values_mut() {
                bindings.retain(|binding| {
                    binding.prepared.source.repository != request.source.repository
                        || binding.prepared.source.path != request.source.path
                });
            }
            Ok(entries.clone())
        })
    }

    fn plugin_installations(
        &self,
    ) -> LocalBoxFuture<'_, Result<Vec<openwebide_core::plugins::PluginInstallation>, String>> {
        Box::pin(async {
            let pending = self.plugin_loads.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending.await.map_err(|error| error.to_string())?;
            }
            Ok(self.plugins.borrow().clone())
        })
    }
    fn prepare_plugin<'a>(
        &'a self,
        project: Option<i64>,
        source: &'a openwebide_core::plugins::PluginSource,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::plugins::PreparedPlugin, String>> {
        Box::pin(async move {
            self.plugin_requests
                .borrow_mut()
                .push((project, source.clone()));
            let pending = self.plugin_preparations.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending.await.map_err(|error| error.to_string())?;
            }
            self.project_plugin_entries
                .borrow()
                .values()
                .flatten()
                .find(|binding| binding.prepared.source == *source)
                .map(|binding| binding.prepared.clone())
                .ok_or_else(|| "No plugin fixture prepared".into())
        })
    }
    fn plugin_preparation<'a>(
        &'a self,
        project: Option<i64>,
        command: &'a openwebide_core::plugins::preparation::PreparationCommand,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::plugins::preparation::PluginPreparation, String>>
    {
        Box::pin(async move {
            use openwebide_core::plugins::preparation::{
                PluginPreparation, PreparationCommand, PreparationState,
            };
            self.plugin_preparation_commands
                .borrow_mut()
                .push((project, command.clone()));
            let pending = self.plugin_progress.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending.await.map_err(|error| error.to_string())?;
            }
            match command {
                PreparationCommand::Start { source } => Ok(PluginPreparation {
                    id: "f".repeat(32),
                    state: PreparationState::Ready,
                    prepared: Some(self.prepare_plugin(project, source).await?),
                    error: None,
                }),
                PreparationCommand::Status { id } => Ok(PluginPreparation {
                    id: id.clone(),
                    state: PreparationState::Preparing,
                    prepared: None,
                    error: None,
                }),
                PreparationCommand::Cancel { id } => Ok(PluginPreparation {
                    id: id.clone(),
                    state: PreparationState::Cancelled,
                    prepared: None,
                    error: None,
                }),
            }
        })
    }
    fn record_plugin<'a>(
        &'a self,
        request: &'a openwebide_core::plugins::RecordPlugin,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::plugins::PluginInstallation>, String>> {
        Box::pin(async move {
            self.plugin_records.borrow_mut().push(request.clone());
            let entries = openwebide_core::plugins::record_installation(
                self.plugins.borrow().clone(),
                request,
                1,
            )
            .map_err(|error| error.to_string())?;
            *self.plugins.borrow_mut() = entries.clone();
            if request.package.is_some() {
                let mut projects = self.project_plugin_entries.borrow_mut();
                projects.entry(1).or_default();
                for bindings in projects.values_mut() {
                    if let Some(binding) = bindings.iter_mut().find(|binding| {
                        binding.prepared.source.repository == request.prepared.source.repository
                            && binding.prepared.source.path == request.prepared.source.path
                    }) {
                        if binding.enabled {
                            binding.prepared = request.prepared.clone();
                            binding.revision += 1;
                        }
                    } else {
                        bindings.push(openwebide_core::plugins::ProjectPlugin {
                            id: i64::try_from(bindings.len()).unwrap() + 1,
                            revision: 1,
                            prepared: request.prepared.clone(),
                            enabled: true,
                        });
                    }
                }
            }
            Ok(entries)
        })
    }

    fn host_connection(
        &self,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::host_admin::HostConnection, String>> {
        Box::pin(async { Ok(self.host_connection.borrow().clone()) })
    }
    fn save_host_connection<'a>(
        &'a self,
        connection: &'a openwebide_core::host_admin::HostConnection,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::host_admin::HostConnection, String>> {
        Box::pin(async move {
            connection.validate()?;
            let mut saved = connection.clone();
            saved.revision += 1;
            *self.host_connection.borrow_mut() = saved.clone();
            Ok(saved)
        })
    }
    fn question_command<'a>(
        &'a self,
        session: i64,
        command: &'a openwebide_core::questions::QuestionCommand,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::questions::QuestionResult, String>> {
        self.question_commands
            .borrow_mut()
            .push((session, command.clone()));
        let deferred = if matches!(command, openwebide_core::questions::QuestionCommand::List) {
            self.question_loads.borrow_mut().pop_front()
        } else {
            None
        };
        Box::pin(async move {
            if let Some(deferred) = deferred {
                return deferred
                    .await
                    .map_err(|_| "deferred question request dropped".to_string())?;
            }
            if let openwebide_core::questions::QuestionCommand::Reply { id, reply } = command {
                let mut questions = self.questions.borrow_mut();
                let question = questions
                    .iter_mut()
                    .find(|question| question.session_id == session && question.id == *id)
                    .ok_or("Question no longer exists")?;
                question.request.validate_reply(reply)?;
                if question.reply.is_some() {
                    return Err("Question already answered".into());
                }
                question.reply = Some(reply.clone());
            }
            Ok(openwebide_core::questions::QuestionResult {
                questions: self
                    .questions
                    .borrow()
                    .iter()
                    .filter(|question| question.session_id == session && question.reply.is_none())
                    .cloned()
                    .collect(),
            })
        })
    }
    fn host_view<'a>(
        &'a self,
        session: i64,
        request: &'a openwebide_core::host_admin::HostRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::host_admin::HostResponse, String>> {
        Box::pin(async move {
            self.host_requests
                .borrow_mut()
                .push((session, request.clone()));
            let pending = self.host_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending.await.map_err(|error| error.to_string())?;
            }
            Ok(openwebide_core::host_admin::HostResponse::Operations(
                self.host_operations.borrow().clone(),
            ))
        })
    }
    fn host_input_key<'a>(
        &'a self,
        session: i64,
        prompt: &'a openwebide_core::host_admin::HostInputKeyRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::host_admin::HostInputKey, String>> {
        Box::pin(async move {
            let pending = self.host_input_key_results.borrow_mut().pop_front();
            if let Some(result) = pending {
                return result
                    .await
                    .map_err(|_| "Private key request cancelled".to_owned())?;
            }
            let mut recipients = self.host_input_recipients.borrow_mut();
            let recipient = recipients.entry(prompt.token.clone()).or_insert_with(|| {
                openwebide_core::host_admin::private_input::PrivateInputRecipient::new(
                    1,
                    session,
                    prompt.clone(),
                    [7; 32],
                )
            });
            Ok(recipient.public_key())
        })
    }
    fn host_input<'a>(
        &'a self,
        session: i64,
        input: &'a openwebide_core::host_admin::HostInput,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.host_encrypted_inputs.borrow_mut().push(input.clone());
            let data = self
                .host_input_recipients
                .borrow_mut()
                .get_mut(&input.prompt.token)
                .ok_or("Unknown private prompt")?
                .open(1, session, input)?;
            self.host_inputs
                .borrow_mut()
                .push((session, input.prompt.operation, data));
            Ok(())
        })
    }
    fn session_expired(&self) -> RwSignal<bool> {
        self.session_expired
    }
    fn register<'a>(
        &'a self,
        _username: &'a str,
        _password: &'a str,
    ) -> LocalBoxFuture<'a, Result<User, String>> {
        Box::pin(async move {
            self.calls
                .borrow_mut()
                .push(Call::Request { method: "register" });
            Err("register has no scripted response".into())
        })
    }
    fn login<'a>(
        &'a self,
        _username: &'a str,
        _password: &'a str,
    ) -> LocalBoxFuture<'a, Result<User, String>> {
        Box::pin(async move {
            self.calls
                .borrow_mut()
                .push(Call::Request { method: "login" });
            Err("login has no scripted response".into())
        })
    }
    fn me<'a>(&'a self) -> LocalBoxFuture<'a, Result<User, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request { method: "me" });
            Err("me has no scripted response".into())
        })
    }
    fn logout<'a>(&'a self) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.calls
                .borrow_mut()
                .push(Call::Request { method: "logout" });
            Ok(())
        })
    }
    fn bridge_token<'a>(&'a self) -> LocalBoxFuture<'a, Result<(String, i64), String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "bridge_token",
            });
            self.bridge_credential
                .borrow()
                .clone()
                .ok_or_else(|| "bridge_token has no scripted response".into())
        })
    }
    fn health<'a>(&'a self) -> LocalBoxFuture<'a, Result<Health, String>> {
        Box::pin(async move {
            self.calls
                .borrow_mut()
                .push(Call::Request { method: "health" });
            Ok(Health {
                status: "ok".into(),
                version: "test".into(),
            })
        })
    }
    fn preview_server<'a>(
        &'a self,
        probe: &'a openwebide_core::ModelProbe,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ServerDiscovery, String>> {
        Box::pin(async move {
            Ok(openwebide_core::ServerDiscovery {
                base_url: probe.base_url.clone(),
                kind: probe.kind,
                models: self.list_models(probe.server_id.unwrap_or(0)).await?,
                detections: Default::default(),
                detection_errors: Default::default(),
            })
        })
    }
    fn save_model_setup<'a>(
        &'a self,
        probe: &'a openwebide_core::ModelProbe,
        profiles: &'a [openwebide_core::ModelProfile],
    ) -> LocalBoxFuture<'a, Result<(Connection, openwebide_core::ModelSetup), String>> {
        Box::pin(async move {
            for profile in profiles {
                profile.settings.validate()?;
            }
            let server = if let Some(id) = probe.server_id {
                let mut server = self
                    .connections
                    .borrow()
                    .iter()
                    .find(|server| server.id == id)
                    .cloned()
                    .ok_or("Server not found")?;
                server.kind = probe.kind;
                server.base_url.clone_from(&probe.base_url);
                self.update_connection(&server).await?
            } else {
                self.create_connection("Server", probe.kind, &probe.base_url, None, None)
                    .await?
            };
            self.save_server_settings(server.id, &probe.transport)
                .await?;
            for profile in profiles {
                let mut profile = profile.clone();
                profile.selection.server_id = server.id;
                self.save_model_profile(&profile).await?;
            }
            let defaults = openwebide_core::model_setup::review_defaults(
                &self.model_setup.borrow().defaults,
                server.id,
                profiles,
            );
            let setup = self.save_model_defaults(&defaults).await?;
            let server = self
                .connections
                .borrow()
                .iter()
                .find(|connection| connection.id == server.id)
                .cloned()
                .unwrap();
            Ok((server, setup))
        })
    }
    fn test_model<'a>(
        &'a self,
        _probe: &'a openwebide_core::ModelProbe,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ModelTestResult, String>> {
        Box::pin(async {
            if let Some(result) = self.test_results.borrow_mut().pop_front() {
                return result;
            }
            Ok(openwebide_core::ModelTestResult {
                structured_tools: true,
                streamed_tools: true,
                first_token_ms: Some(25),
                tokens_per_second: Some(50.0),
                ..Default::default()
            })
        })
    }
    fn preview_model<'a>(
        &'a self,
        probe: &'a openwebide_core::ModelProbe,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ModelDetection, String>> {
        self.detect_model(
            probe.server_id.unwrap_or(0),
            probe.model.as_deref().unwrap_or_default(),
        )
    }
    fn detect_model<'a>(
        &'a self,
        id: i64,
        model: &'a str,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ModelDetection, String>> {
        Box::pin(async move {
            if let Some(detected) = self.detections.borrow().get(&(id, model.into())) {
                return Ok(detected.clone());
            }
            Ok(openwebide_core::ModelDetection {
                context_limit: Some(8192),
                source: "test server".into(),
                capabilities: vec!["tools".into()],
                ..Default::default()
            })
        })
    }
    fn inspect_server<'a>(
        &'a self,
        base_url: &'a str,
        kind: Option<ProviderKind>,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ServerDiscovery, String>> {
        Box::pin(async move {
            Ok(openwebide_core::ServerDiscovery {
                base_url: base_url.into(),
                kind: kind.unwrap_or(ProviderKind::Ollama),
                models: self.models.borrow().clone(),
                detections: Default::default(),
                detection_errors: Default::default(),
            })
        })
    }
    fn discover_servers<'a>(
        &'a self,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::ServerDiscovery>, String>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn model_runtime<'a>(
        &'a self,
        id: i64,
        model: Option<&'a str>,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ModelRuntime, String>> {
        Box::pin(async move {
            let mut connection = self
                .connections
                .borrow()
                .iter()
                .find(|connection| connection.id == id)
                .cloned()
                .ok_or("Server not found")?;
            let setup = self.model_setup.borrow();
            let model = model
                .map(str::to_string)
                .or_else(|| {
                    setup
                        .defaults
                        .primary
                        .as_ref()
                        .filter(|selection| selection.server_id == id)
                        .map(|selection| selection.model.clone())
                })
                .or_else(|| connection.model.clone())
                .unwrap_or_default();
            let settings = setup.resolve(id, &model);
            connection.model = Some(model);
            Ok(openwebide_core::ModelRuntime {
                connection,
                settings,
                transport: Default::default(),
            })
        })
    }
    fn model_setup<'a>(
        &'a self,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ModelSetup, String>> {
        Box::pin(async { Ok(self.model_setup.borrow().clone()) })
    }
    fn save_model_defaults<'a>(
        &'a self,
        defaults: &'a openwebide_core::ModelDefaults,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ModelSetup, String>> {
        Box::pin(async move {
            self.model_default_requests
                .borrow_mut()
                .push(defaults.clone());
            let pending = self.model_default_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending.await.map_err(|error| error.to_string())?;
            }
            self.model_setup.borrow_mut().defaults = defaults.clone();
            Ok(self.model_setup.borrow().clone())
        })
    }
    fn save_model_profile<'a>(
        &'a self,
        profile: &'a openwebide_core::ModelProfile,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ModelSetup, String>> {
        Box::pin(async move {
            let mut setup = self.model_setup.borrow_mut();
            setup
                .profiles
                .retain(|item| item.selection != profile.selection);
            setup.profiles.push(profile.clone());
            Ok(setup.clone())
        })
    }
    fn server_settings<'a>(
        &'a self,
        id: i64,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ServerSettings, String>> {
        Box::pin(async move {
            let pending = self.server_settings_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending
                    .await
                    .map_err(|_| "Server settings request dropped".to_string())?;
            }
            Ok(self.server_settings.borrow().get(&id).cloned().unwrap_or(
                openwebide_core::ServerSettings {
                    timeout_seconds: 300,
                    ..Default::default()
                },
            ))
        })
    }
    fn save_server_settings<'a>(
        &'a self,
        id: i64,
        update: &'a openwebide_core::ServerSettingsUpdate,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ServerSettings, String>> {
        Box::pin(async move {
            let mut settings = self.server_settings.borrow_mut();
            let item = settings
                .entry(id)
                .or_insert_with(|| openwebide_core::ServerSettings {
                    timeout_seconds: 300,
                    ..Default::default()
                });
            if let Some(selection) = &update.tool_selection {
                selection.validate()?;
                item.tool_selection = selection.clone();
                if let Some(connection) = self
                    .connections
                    .borrow_mut()
                    .iter_mut()
                    .find(|connection| connection.id == id)
                {
                    connection.tool_selection = selection.clone();
                }
            }
            if let Some(preset) = update.preset {
                item.preset = preset;
            }
            if update.clear_api_key {
                item.has_api_key = false;
            } else if update.api_key.is_some() {
                item.has_api_key = true;
            }
            if let Some(timeout) = update.timeout_seconds {
                item.timeout_seconds = timeout;
            }
            if let Some(keep_alive) = &update.keep_alive {
                item.keep_alive = (!keep_alive.trim().is_empty()).then(|| keep_alive.clone());
            }
            Ok(item.clone())
        })
    }
    fn list_connections<'a>(&'a self) -> LocalBoxFuture<'a, Result<Vec<Connection>, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "list_connections",
            });
            let pending = self.connection_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending
                    .await
                    .unwrap_or_else(|_| Err("response dropped".into()));
            }
            let latency = *self.endpoint_latency_ms.borrow();
            if latency > 0 {
                crate::util::sleep_ms(latency).await;
            }
            Ok(self.connections.borrow().clone())
        })
    }
    fn create_connection<'a>(
        &'a self,
        name: &'a str,
        kind: ProviderKind,
        base_url: &'a str,
        model: Option<&'a str>,
        context_limit: Option<usize>,
    ) -> LocalBoxFuture<'a, Result<Connection, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "create_connection",
            });
            let connection = Connection {
                id: self
                    .connections
                    .borrow()
                    .iter()
                    .map(|c| c.id)
                    .max()
                    .unwrap_or(0)
                    + 1,
                name: name.into(),
                kind,
                base_url: base_url.into(),
                model: model.map(str::to_string),
                enabled: true,
                context_limit,
                tool_stream_unsupported: false,
                tool_stream_revision: 0,
                tool_selection: Default::default(),
            };
            self.connections.borrow_mut().push(connection.clone());
            Ok(connection)
        })
    }
    fn update_connection<'a>(
        &'a self,
        connection: &'a Connection,
    ) -> LocalBoxFuture<'a, Result<Connection, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "update_connection",
            });
            let mut connections = self.connections.borrow_mut();
            let current = connections
                .iter_mut()
                .find(|c| c.id == connection.id)
                .ok_or("connection not found")?;
            *current = connection.clone();
            Ok(current.clone())
        })
    }
    fn delete_connection<'a>(&'a self, id: i64) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "delete_connection",
            });
            self.connections.borrow_mut().retain(|item| item.id != id);
            self.server_settings.borrow_mut().remove(&id);
            let mut setup = self.model_setup.borrow_mut();
            setup
                .profiles
                .retain(|profile| profile.selection.server_id != id);
            if setup
                .defaults
                .primary
                .as_ref()
                .is_some_and(|selection| selection.server_id == id)
            {
                setup.defaults.primary = None;
            }
            if setup
                .defaults
                .fast
                .as_ref()
                .is_some_and(|selection| selection.server_id == id)
            {
                setup.defaults.fast = None;
            }
            for session in self.sessions.borrow_mut().iter_mut() {
                if session.connection_id == Some(id) {
                    session.connection_id = None;
                }
            }
            Ok(())
        })
    }
    fn list_sessions<'a>(&'a self) -> LocalBoxFuture<'a, Result<Vec<ChatSession>, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "list_sessions",
            });
            let pending = self.session_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending
                    .await
                    .unwrap_or_else(|_| Err("response dropped".into()));
            }
            let latency = *self.endpoint_latency_ms.borrow();
            if latency > 0 {
                crate::util::sleep_ms(latency).await;
            }
            Ok(self.sessions.borrow().clone())
        })
    }
    fn search_sessions<'a>(
        &'a self,
        search: &'a openwebide_core::SessionSearch,
    ) -> LocalBoxFuture<'a, Result<Vec<ChatSession>, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "search_sessions",
            });
            let pending = self.session_search_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending
                    .await
                    .map_err(|_| "search response dropped".to_string())?;
            }
            search.validate()?;
            let query = search.query.trim().to_ascii_lowercase();
            let mut sessions:Vec<_>=self.sessions.borrow().iter().filter(|session| session.project_id==search.project_id && session.archived==search.archived
                && (query.is_empty() || session.name.to_ascii_lowercase().contains(&query) || self.messages.borrow().get(&session.id).is_some_and(|entries| entries.iter().any(|entry| matches!(entry,ConversationEntry::Message(message) if message.content.to_ascii_lowercase().contains(&query)))))).cloned().collect();
            sessions.sort_by_key(|session| {
                (
                    std::cmp::Reverse(session.pinned),
                    std::cmp::Reverse(session.id),
                )
            });
            Ok(sessions)
        })
    }
    fn session_preferences<'a>(
        &'a self,
        id: i64,
        preferences: &'a openwebide_core::SessionPreferences,
    ) -> LocalBoxFuture<'a, Result<ChatSession, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "session_preferences",
            });
            let mut sessions = self.sessions.borrow_mut();
            let session = sessions
                .iter_mut()
                .find(|session| session.id == id)
                .ok_or("not found")?;
            if let Some(value) = preferences.pinned {
                session.pinned = value;
            }
            if let Some(value) = preferences.archived {
                session.archived = value;
            }
            Ok(session.clone())
        })
    }
    fn session_title<'a>(
        &'a self,
        _id: i64,
    ) -> LocalBoxFuture<'a, Result<Option<ChatSession>, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "session_title",
            });
            let pending = self.title_results.borrow_mut().pop_front();
            match pending {
                Some(pending) => pending
                    .await
                    .map_err(|_| "title response dropped".to_string())?,
                None => Ok(None),
            }
        })
    }
    fn export_session<'a>(
        &'a self,
        id: i64,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::SessionExport, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "export_session",
            });
            let pending = self.session_export_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending
                    .await
                    .map_err(|_| "export response dropped".to_string())?;
            }
            let sessions = self.sessions.borrow();
            let session = sessions
                .iter()
                .find(|session| session.id == id)
                .ok_or("not found")?;
            let messages = self.messages.borrow();
            let entries = messages.get(&id).cloned().unwrap_or_default();
            Ok(openwebide_core::SessionExport {
                filename: openwebide_core::session_markdown_filename(session),
                markdown: openwebide_core::session_markdown(session, &entries),
            })
        })
    }
    fn list_models<'a>(
        &'a self,
        connection_id: i64,
    ) -> LocalBoxFuture<'a, Result<Vec<ModelInfo>, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "list_models",
            });
            self.model_requests.borrow_mut().push(connection_id);
            let pending = self.model_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending
                    .await
                    .unwrap_or_else(|_| Err("response dropped".into()));
            }
            let latency = *self.endpoint_latency_ms.borrow();
            if latency > 0 {
                crate::util::sleep_ms(latency).await;
            }
            Ok(self.models.borrow().clone())
        })
    }
    fn list_system_prompts<'a>(&'a self) -> LocalBoxFuture<'a, Result<Vec<SystemPrompt>, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "list_system_prompts",
            });
            let pending = self.prompt_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending
                    .await
                    .unwrap_or_else(|_| Err("response dropped".into()));
            }
            let latency = *self.endpoint_latency_ms.borrow();
            if latency > 0 {
                crate::util::sleep_ms(latency).await;
            }
            Ok(self.system_prompts.borrow().clone())
        })
    }
    fn create_system_prompt<'a>(
        &'a self,
        name: &'a str,
        content: &'a str,
    ) -> LocalBoxFuture<'a, Result<SystemPrompt, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "create_system_prompt",
            });
            let pending = self.prompt_save_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending
                    .await
                    .unwrap_or_else(|_| Err("prompt response cancelled".into()));
            }
            let prompt = SystemPrompt {
                id: self
                    .system_prompts
                    .borrow()
                    .iter()
                    .map(|p| p.id)
                    .max()
                    .unwrap_or(0)
                    + 1,
                name: name.into(),
                content: content.into(),
            };
            self.system_prompts.borrow_mut().push(prompt.clone());
            Ok(prompt)
        })
    }
    fn update_system_prompt<'a>(
        &'a self,
        id: i64,
        name: &'a str,
        content: &'a str,
    ) -> LocalBoxFuture<'a, Result<SystemPrompt, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "update_system_prompt",
            });
            let pending = self.prompt_save_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending
                    .await
                    .unwrap_or_else(|_| Err("prompt response cancelled".into()));
            }
            let mut prompts = self.system_prompts.borrow_mut();
            let prompt = prompts
                .iter_mut()
                .find(|p| p.id == id)
                .ok_or("prompt not found")?;
            prompt.name = name.into();
            prompt.content = content.into();
            Ok(prompt.clone())
        })
    }
    fn delete_system_prompt<'a>(&'a self, id: i64) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "delete_system_prompt",
            });
            let pending = self.prompt_delete_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending
                    .await
                    .unwrap_or_else(|_| Err("prompt response cancelled".into()));
            }
            self.system_prompts
                .borrow_mut()
                .retain(|item| item.id != id);
            Ok(())
        })
    }
    fn editor_recovery(
        &self,
        project: i64,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::editor::EditorRecoveryRecord, RecoveryError>>
    {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "editor_recovery",
            });
            let pending = self.recovery_load_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending
                    .await
                    .map_err(|error| RecoveryError::Unavailable(error.to_string()))?;
            }
            Ok(self
                .editor_recovery_records
                .borrow()
                .get(&project)
                .cloned()
                .unwrap_or_default())
        })
    }
    fn save_editor_recovery<'a>(
        &'a self,
        project: i64,
        record: &'a openwebide_core::editor::EditorRecoveryRecord,
    ) -> LocalBoxFuture<'a, Result<i64, RecoveryError>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "save_editor_recovery",
            });
            let pending = self.recovery_save_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                pending
                    .await
                    .map_err(|error| RecoveryError::Unavailable(error.to_string()))??;
            }
            record
                .state
                .validate()
                .map_err(RecoveryError::Unavailable)?;
            let mut records = self.editor_recovery_records.borrow_mut();
            let revision = records.get(&project).map_or(0, |record| record.revision);
            if revision != record.revision {
                return Err(RecoveryError::Conflict(
                    "Editor recovery changed in another window".into(),
                ));
            }
            let revision = revision
                .checked_add(1)
                .ok_or_else(|| RecoveryError::Conflict("Revision limit reached".into()))?;
            records.insert(
                project,
                openwebide_core::editor::EditorRecoveryRecord {
                    revision,
                    state: record.state.clone(),
                },
            );
            Ok(revision)
        })
    }
    fn get_settings<'a>(
        &'a self,
    ) -> LocalBoxFuture<'a, Result<std::collections::BTreeMap<String, String>, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "get_settings",
            });
            if let Some(started) = self.settings_load_started.borrow_mut().take() {
                let _ = started.send(());
            }
            let result = self.settings_load_results.borrow_mut().pop_front();
            if let Some(result) = result {
                return result.await.map_err(|error| error.to_string())?;
            }
            let latency = *self.endpoint_latency_ms.borrow();
            if latency > 0 {
                crate::util::sleep_ms(latency).await;
            }
            if let Some(error) = self.settings_load_error.borrow().clone() {
                return Err(error);
            }
            Ok(self.settings.borrow().clone())
        })
    }
    fn set_setting<'a>(
        &'a self,
        key: &'a str,
        value: &'a str,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::SetSetting {
                key: key.into(),
                value: value.into(),
            });
            if key == "editor_preferences" {
                let pending = self.editor_save_results.borrow_mut().pop_front();
                if let Some(pending) = pending {
                    pending.await.map_err(|error| error.to_string())??;
                }
            }
            if key == "panel_visibility" {
                let result = self.panel_save_results.borrow_mut().pop_front();
                if let Some(result) = result {
                    result.await.map_err(|error| error.to_string())??;
                }
            }
            if key == "prompt_history" {
                let result = self.history_save_results.borrow_mut().pop_front();
                if let Some(result) = result {
                    result.await.map_err(|error| error.to_string())??;
                }
            }
            self.settings.borrow_mut().insert(key.into(), value.into());
            Ok(())
        })
    }
    fn startup_context(
        &self,
        _project: i64,
        _tools: bool,
        _connection: Option<i64>,
    ) -> LocalBoxFuture<'_, Result<String, String>> {
        Box::pin(async { Ok("Environment\nProject instructions ready".into()) })
    }
    fn create_session<'a>(
        &'a self,
        name: &'a str,
        connection_id: Option<i64>,
        system_prompt_id: Option<i64>,
        project_id: Option<i64>,
    ) -> LocalBoxFuture<'a, Result<ChatSession, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "create_session",
            });
            let session = ChatSession {
                pinned: false,
                archived: false,
                auto_title: true,
                title_revision: 0,
                id: self
                    .sessions
                    .borrow()
                    .iter()
                    .map(|s| s.id)
                    .max()
                    .unwrap_or(0)
                    + 1,
                name: name.into(),
                connection_id,
                system_prompt_id,
                project_id,
                user_id: None,
                created_at: 0,
            };
            self.settings.borrow_mut().insert(
                openwebide_core::ApprovalMode::setting_key(session.id),
                serde_json::to_string(&openwebide_core::ApprovalMode::NEW_SESSION).unwrap(),
            );
            self.sessions.borrow_mut().push(session.clone());
            Ok(session)
        })
    }
    fn list_projects<'a>(&'a self) -> LocalBoxFuture<'a, Result<Vec<Project>, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "list_projects",
            });
            let pending = self.project_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending
                    .await
                    .unwrap_or_else(|_| Err("response dropped".into()));
            }
            let latency = *self.endpoint_latency_ms.borrow();
            if latency > 0 {
                crate::util::sleep_ms(latency).await;
            }
            Ok(self.projects.borrow().clone())
        })
    }
    fn create_project<'a>(
        &'a self,
        name: &'a str,
        mode: WorkspaceMode,
        path: Option<String>,
    ) -> LocalBoxFuture<'a, Result<Project, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "create_project",
            });
            let project = Project {
                id: self
                    .projects
                    .borrow()
                    .iter()
                    .map(|p| p.id)
                    .max()
                    .unwrap_or(0)
                    + 1,
                name: name.into(),
                mode,
                path,
                user_id: None,
                created_at: 0,
            };
            self.projects.borrow_mut().push(project.clone());
            Ok(project)
        })
    }
    fn rename_project<'a>(
        &'a self,
        id: i64,
        name: &'a str,
    ) -> LocalBoxFuture<'a, Result<Project, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "rename_project",
            });
            let mut items = self.projects.borrow_mut();
            let item = items
                .iter_mut()
                .find(|item| item.id == id)
                .ok_or("not found")?;
            item.name = name.into();
            Ok(item.clone())
        })
    }
    fn delete_project<'a>(&'a self, id: i64) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "delete_project",
            });
            if let Some(error) = self.project_delete_error.borrow().clone() {
                return Err(error);
            }
            self.projects.borrow_mut().retain(|item| item.id != id);
            let sessions = self
                .sessions
                .borrow()
                .iter()
                .filter(|session| session.project_id == Some(id))
                .map(|session| session.id)
                .collect::<Vec<_>>();
            for session in sessions {
                self.sessions.borrow_mut().retain(|item| item.id != session);
                self.messages.borrow_mut().remove(&session);
            }
            self.files
                .borrow_mut()
                .retain(|(project, _), _| *project != id);
            self.directories
                .borrow_mut()
                .retain(|(project, _)| *project != id);
            self.persisted_edits
                .borrow_mut()
                .retain(|(project, _), _| *project != id);
            Ok(())
        })
    }
    fn list_files<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<Vec<FileEntry>, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "list_files",
            });
            let pending = self.file_list_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending
                    .await
                    .unwrap_or_else(|_| Err("response dropped".into()));
            }
            let prefix = if path.is_empty() {
                String::new()
            } else {
                format!("{}/", path.trim_end_matches('/'))
            };
            let files = self.files.borrow();
            let directories = self.directories.borrow();
            let mut entries = BTreeMap::new();
            for (project, child) in files
                .keys()
                .chain(self.binary_files.borrow().keys())
                .chain(directories.iter())
            {
                if *project != project_id {
                    continue;
                }
                let Some(relative) = child.strip_prefix(&prefix) else {
                    continue;
                };
                if relative.is_empty() {
                    continue;
                }
                let name = relative.split('/').next().unwrap();
                let child_path = format!("{prefix}{name}");
                let is_dir = relative.contains('/')
                    || directories.contains(&(project_id, child_path.clone()));
                entries.insert(
                    name.to_string(),
                    FileEntry {
                        name: name.into(),
                        path: child_path,
                        is_dir,
                        size: 0,
                    },
                );
            }
            Ok(entries.into_values().collect())
        })
    }
    fn read_file_object_url<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "read_file_object_url",
            });
            let content = self.read(project_id, path)?;
            let parts = js_sys::Array::new();
            parts.push(&wasm_bindgen::JsValue::from_str(&content));
            let blob =
                web_sys::Blob::new_with_str_sequence(&parts).map_err(|e| format!("{e:?}"))?;
            crate::workspace::preview_object_url(&blob, path).map_err(|e| format!("{e:?}"))
        })
    }
    fn read_file<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "read_file",
            });
            let pending = self.file_read_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending
                    .await
                    .unwrap_or_else(|_| Err("response dropped".into()));
            }
            self.read(project_id, path)
        })
    }
    fn read_file_bytes<'a>(
        &'a self,
        project: i64,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<Vec<u8>, String>> {
        Box::pin(async move {
            if let Some(bytes) = self.binary_files.borrow().get(&(project, path.into())) {
                return Ok(bytes.clone());
            }
            self.read(project, path).map(String::into_bytes)
        })
    }
    fn write_file_bytes<'a>(
        &'a self,
        project: i64,
        path: &'a str,
        bytes: &'a [u8],
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if let Ok(text) = std::str::from_utf8(bytes) {
                self.binary_files
                    .borrow_mut()
                    .remove(&(project, path.into()));
                self.write_file(project, path, text).await
            } else {
                self.binary_files
                    .borrow_mut()
                    .insert((project, path.into()), bytes.to_vec());
                self.files.borrow_mut().remove(&(project, path.into()));
                Ok(())
            }
        })
    }
    fn read_file_lossy<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "read_file_lossy",
            });
            self.read(project_id, path)
        })
    }
    fn write_file<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
        content: &'a str,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if let Some(error) = self.file_error.borrow().as_ref() {
                return Err(error.clone());
            }
            self.calls.borrow_mut().push(Call::WriteFile {
                path: path.into(),
                content: content.into(),
            });
            let pending = self.file_write_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                pending.await.map_err(|error| error.to_string())??;
            }
            self.files
                .borrow_mut()
                .insert((project_id, path.into()), content.into());
            Ok(())
        })
    }
    fn copy_file<'a>(
        &'a self,
        project_id: i64,
        from: &'a str,
        to: &'a str,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if let Some(error) = self.file_error.borrow().as_ref() {
                return Err(error.clone());
            }
            self.calls.borrow_mut().push(Call::CopyFile {
                from: from.into(),
                to: to.into(),
            });
            let content = self.read(project_id, from)?;
            self.files
                .borrow_mut()
                .insert((project_id, to.into()), content);
            Ok(())
        })
    }
    fn create_file<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
        is_dir: bool,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "create_file",
            });
            let key = (project_id, path.to_string());
            if self.files.borrow().contains_key(&key)
                || (!is_dir && self.directories.borrow().contains(&key))
            {
                return Err(format!("file already exists: {path}"));
            }
            if is_dir {
                self.directories.borrow_mut().insert(key);
            } else {
                self.files.borrow_mut().insert(key, String::new());
            }
            Ok(())
        })
    }
    fn delete_file<'a>(
        &'a self,
        project_id: i64,
        path: &'a str,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if let Some(error) = self.file_error.borrow().as_ref() {
                return Err(error.clone());
            }
            self.calls
                .borrow_mut()
                .push(Call::DeleteFile { path: path.into() });
            self.binary_files
                .borrow_mut()
                .remove(&(project_id, path.into()));
            self.files.borrow_mut().remove(&(project_id, path.into()));
            Ok(())
        })
    }
    fn browse<'a>(&'a self, path: &'a str) -> LocalBoxFuture<'a, Result<Vec<FileEntry>, String>> {
        Box::pin(async move {
            self.calls
                .borrow_mut()
                .push(Call::Request { method: "browse" });
            Ok(self
                .browse_entries
                .borrow()
                .get(path)
                .cloned()
                .unwrap_or_default())
        })
    }
    fn search_content<'a>(
        &'a self,
        project_id: i64,
        query: &'a str,
        _path: &'a str,
        opts: SearchOptions,
    ) -> LocalBoxFuture<'a, Result<Vec<SearchHit>, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "search_content",
            });
            self.search_requests
                .borrow_mut()
                .push((project_id, query.into(), opts));
            let result = self.search_results.borrow_mut().pop_front();
            if let Some(result) = result {
                return result
                    .await
                    .map_err(|_| "search response dropped".to_string())?;
            }
            Ok(Vec::new())
        })
    }
    fn git_status<'a>(
        &'a self,
        project_id: Option<i64>,
    ) -> LocalBoxFuture<'a, Result<GitRepoStatus, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "git_status",
            });
            self.git_status_requests.borrow_mut().push(project_id);
            let result = self.git_statuses.borrow_mut().pop_front();
            match result {
                Some(result) => result
                    .await
                    .map_err(|_| "git status response dropped".to_string())?,
                None => Err("git_status has no scripted response".into()),
            }
        })
    }
    fn git_diff<'a>(
        &'a self,
        _project_id: Option<i64>,
        _path: Option<&'a str>,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            self.calls
                .borrow_mut()
                .push(Call::Request { method: "git_diff" });
            self.git_diffs
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| Ok(String::new()))
        })
    }
    fn git_file_head<'a>(
        &'a self,
        _project_id: Option<i64>,
        _path: &'a str,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "git_file_head",
            });
            Err("git_file_head has no scripted response".into())
        })
    }
    fn git_stash<'a>(
        &'a self,
        _project_id: Option<i64>,
        request: &'a openwebide_core::git::GitStashRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::git::GitStashResult, String>> {
        Box::pin(async move {
            self.git_stash_requests.borrow_mut().push(request.clone());
            Ok(self.git_stashes.borrow().clone())
        })
    }
    fn git_index_diff(
        &self,
        _project_id: Option<i64>,
    ) -> LocalBoxFuture<'_, Result<String, String>> {
        Box::pin(async move { Ok(self.git_index_diff.borrow().clone()) })
    }
    fn git_history<'a>(
        &'a self,
        _project_id: Option<i64>,
        request: &'a openwebide_core::git::GitHistoryRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::git::GitHistoryPage, String>> {
        Box::pin(async move {
            self.git_history_requests.borrow_mut().push(request.clone());
            let pending = self.git_history_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                pending.await.map_err(|error| error.to_string())?
            } else {
                Ok(self.git_history.borrow().clone())
            }
        })
    }
    fn git_commit_diff<'a>(
        &'a self,
        _project_id: Option<i64>,
        request: &'a openwebide_core::git::GitCommitDiffRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::git::GitCommitDiff, String>> {
        Box::pin(async move {
            self.git_commit_diff_requests
                .borrow_mut()
                .push(request.clone());
            let pending = self.git_commit_diff_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                pending.await.map_err(|error| error.to_string())?
            } else {
                self.git_commit_diff
                    .borrow()
                    .clone()
                    .ok_or_else(|| "Commit diff unavailable".into())
            }
        })
    }
    fn git_branches<'a>(
        &'a self,
        _project_id: Option<i64>,
    ) -> LocalBoxFuture<'a, Result<Vec<GitBranchInfo>, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "git_branches",
            });
            let pending = self.git_branches_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                pending.await.map_err(|error| error.to_string())?
            } else {
                Ok(self.git_branches.borrow().clone())
            }
        })
    }
    fn git_path_changes(
        &self,
        project_id: Option<i64>,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::git::GitPathChanges, String>> {
        Box::pin(async move {
            self.git_path_status_requests.borrow_mut().push(project_id);
            let pending = self.git_path_results.borrow_mut().pop_front();
            match pending {
                Some(pending) => pending
                    .await
                    .unwrap_or_else(|_| Err("response dropped".into())),
                None => Err("git_path_changes has no scripted response".into()),
            }
        })
    }
    fn git_path_action<'a>(
        &'a self,
        project_id: Option<i64>,
        request: &'a openwebide_core::git::GitPathRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::git::GitPathChanges, String>> {
        Box::pin(async move {
            self.git_path_requests
                .borrow_mut()
                .push((project_id, request.clone()));
            let pending = self.git_path_results.borrow_mut().pop_front();
            match pending {
                Some(pending) => pending
                    .await
                    .unwrap_or_else(|_| Err("response dropped".into())),
                None => Err("git_path_action has no scripted response".into()),
            }
        })
    }
    fn git_commit<'a>(
        &'a self,
        _project_id: Option<i64>,
        req: &'a GitCommitRequest,
    ) -> LocalBoxFuture<'a, Result<GitCommitResult, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "git_commit",
            });
            self.git_commit_requests.borrow_mut().push(req.clone());
            let pending = self.git_commit_results.borrow_mut().pop_front();
            match pending {
                Some(pending) => pending.await.map_err(|error| error.to_string())?,
                None => Err("git_commit has no scripted response".into()),
            }
        })
    }
    fn git_checkout<'a>(
        &'a self,
        project_id: Option<i64>,
        req: &'a GitCheckoutRequest,
    ) -> LocalBoxFuture<'a, Result<GitCheckoutResult, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "git_checkout",
            });
            self.git_checkout_requests
                .borrow_mut()
                .push((project_id, req.clone()));
            let pending = self.git_checkout_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                pending.await.map_err(|error| error.to_string())?
            } else {
                Err("git_checkout has no scripted response".into())
            }
        })
    }
    fn git_sync<'a>(
        &'a self,
        _project_id: Option<i64>,
        req: &'a GitSyncRequest,
    ) -> LocalBoxFuture<'a, Result<GitSyncResult, String>> {
        Box::pin(async move {
            self.calls
                .borrow_mut()
                .push(Call::Request { method: "git_sync" });
            self.git_sync_requests.borrow_mut().push(req.clone());
            let pending = self.git_sync_results.borrow_mut().pop_front();
            match pending {
                Some(pending) => pending.await.map_err(|error| error.to_string())?,
                None => Err("git_sync has no scripted response".into()),
            }
        })
    }
    fn set_session_connection(
        &self,
        id: i64,
        connection_id: i64,
    ) -> LocalBoxFuture<'_, Result<ChatSession, String>> {
        Box::pin(async move {
            let mut items = self.sessions.borrow_mut();
            let item = items
                .iter_mut()
                .find(|item| item.id == id)
                .ok_or("not found")?;
            item.connection_id = Some(connection_id);
            Ok(item.clone())
        })
    }
    fn rename_session<'a>(
        &'a self,
        id: i64,
        name: &'a str,
    ) -> LocalBoxFuture<'a, Result<ChatSession, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "rename_session",
            });
            let mut items = self.sessions.borrow_mut();
            let item = items
                .iter_mut()
                .find(|item| item.id == id)
                .ok_or("not found")?;
            item.name = name.into();
            item.auto_title = false;
            item.title_revision += 1;
            Ok(item.clone())
        })
    }
    fn delete_session<'a>(&'a self, id: i64) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "delete_session",
            });
            self.sessions.borrow_mut().retain(|item| item.id != id);
            self.messages.borrow_mut().remove(&id);
            self.queued_prompts.borrow_mut().remove(&id);
            self.todo_updates.borrow_mut().remove(&id);
            Ok(())
        })
    }
    fn cancel_session<'a>(&'a self, session_id: i64) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::CancelSession {
                session: session_id,
            });
            Ok(())
        })
    }
    fn set_permission<'a>(
        &'a self,
        session_id: i64,
        tool_call_id: &'a str,
        approved: bool,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::SetPermission {
                session: session_id,
                id: tool_call_id.into(),
                approved,
            });
            Ok(())
        })
    }
    fn list_messages<'a>(
        &'a self,
        session_id: i64,
    ) -> LocalBoxFuture<'a, Result<Vec<ConversationEntry>, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "list_messages",
            });
            Ok(self
                .messages
                .borrow()
                .get(&session_id)
                .cloned()
                .unwrap_or_default())
        })
    }
    fn list_run_changes(
        &self,
        project: i64,
    ) -> LocalBoxFuture<'_, Result<Vec<openwebide_core::RunChange>, String>> {
        Box::pin(async move {
            Ok(self
                .run_changes
                .borrow()
                .get(&project)
                .cloned()
                .unwrap_or_default())
        })
    }
    fn preview_run_review<'a>(
        &'a self,
        project: i64,
        request: &'a openwebide_core::ReviewRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ReviewPlan, String>> {
        Box::pin(async move {
            let deferred = self.review_results.borrow_mut().pop_front();
            if let Some(deferred) = deferred {
                return deferred.await.map_err(|_| "review cancelled".to_string())?;
            }
            if let Some(plan) = self.reviews.borrow().get(&project) {
                return if plan.request == *request {
                    Ok(plan.clone())
                } else {
                    Err("Review in progress".into())
                };
            }
            self.run_changes
                .borrow()
                .get(&project)
                .and_then(|records| {
                    records.iter().find(|record| {
                        record.session_id == request.session_id
                            && record.message_id == request.message_id
                            && record.file.path == request.path
                    })
                })
                .ok_or("Review not found")?
                .prepare(request.clone())
        })
    }
    fn prepare_run_review<'a>(
        &'a self,
        project: i64,
        request: &'a openwebide_core::ReviewRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ReviewPlan, String>> {
        Box::pin(async move {
            let plan = self.preview_run_review(project, request).await?;
            self.reviews.borrow_mut().insert(project, plan.clone());
            Ok(plan)
        })
    }
    fn complete_run_review<'a>(
        &'a self,
        project: i64,
        request: &'a openwebide_core::ReviewRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::RunChange, String>> {
        Box::pin(async move {
            if let Some(error) = self.resolution_error.borrow().clone() {
                return Err(error);
            }
            let plan = self.reviews.borrow().get(&project).cloned();
            let Some(plan) = plan else {
                return self
                    .review_history
                    .borrow()
                    .iter()
                    .find(|(id, previous, _)| *id == project && previous == request)
                    .map(|(_, _, record)| record.clone())
                    .ok_or("Review not prepared".into());
            };
            if plan.request != *request {
                return Err("Review changed".into());
            }
            let reviewed = plan.reviewed;
            let file = reviewed.pending_file()?;
            let mut edits = self.persisted_edits.borrow_mut();
            let revision = edits
                .get(&(project, request.path.clone()))
                .map_or(1, |edit| edit.revision + 1);
            edits.insert(
                (project, request.path.clone()),
                PersistedEdit {
                    project_id: project,
                    path: request.path.clone(),
                    revision,
                    decision: if reviewed.pending() > 0 {
                        EditDecision::Pending
                    } else {
                        request.decision
                    },
                    diff: file.preview(),
                    file: Some(file),
                },
            );
            let mut records = self.run_changes.borrow_mut();
            let record = records
                .get_mut(&project)
                .and_then(|records| {
                    records.iter_mut().find(|record| {
                        record.session_id == request.session_id
                            && record.message_id == request.message_id
                            && record.file.path == request.path
                    })
                })
                .ok_or("Review not found")?;
            *record = reviewed.clone();
            self.reviews.borrow_mut().remove(&project);
            self.review_history
                .borrow_mut()
                .push((project, request.clone(), reviewed.clone()));
            Ok(reviewed)
        })
    }
    fn prepare_rewind(
        &self,
        session: i64,
        message: i64,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::RewindPlan, String>> {
        Box::pin(async move {
            if let Some(plan) = self.rewinds.borrow().get(&session).cloned() {
                return Ok(plan);
            }
            let entries = self.list_messages(session).await?;
            let plan = openwebide_core::RewindPlan::from_conversation(&entries, message)?;
            self.rewinds.borrow_mut().insert(session, plan.clone());
            Ok(plan)
        })
    }
    fn complete_rewind(
        &self,
        session: i64,
        message: i64,
    ) -> LocalBoxFuture<'_, Result<Vec<ConversationEntry>, String>> {
        Box::pin(async move {
            let plan = self
                .rewinds
                .borrow_mut()
                .remove(&session)
                .ok_or("rewind not prepared")?;
            if plan.message_id != message {
                return Err("checkpoint changed".into());
            }
            let mut all = self.messages.borrow_mut();
            let entries = all.entry(session).or_default();
            entries.retain(|entry| match entry {
                ConversationEntry::Message(m) => m.id < message,
                ConversationEntry::ToolStep(step) => step.anchor_message_id < message,
                ConversationEntry::Task(task) => task.anchor_message_id < message,
            });
            if let Some(project) = self
                .sessions
                .borrow()
                .iter()
                .find(|s| s.id == session)
                .and_then(|s| s.project_id)
            {
                let mut edits = self.persisted_edits.borrow_mut();
                for file in plan.files {
                    edits.remove(&(project, file.path));
                }
            }
            if let Some(updates) = self.todo_updates.borrow_mut().get_mut(&session) {
                updates.retain(|update| update.anchor_message_id < message);
            }
            Ok(entries.clone())
        })
    }
    fn approval_check<'a>(
        &'a self,
        session: i64,
        check: &'a openwebide_core::ApprovalCheck,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ApprovalDecision, String>> {
        Box::pin(async move {
            let mode = self
                .settings
                .borrow()
                .get(&openwebide_core::ApprovalMode::setting_key(session))
                .and_then(|value| serde_json::from_str::<openwebide_core::ApprovalMode>(value).ok())
                .unwrap_or_default();
            Ok(openwebide_core::ApprovalDecision {
                approved: mode.auto_approves(&check.call.name),
            })
        })
    }
    fn staged_assistance<'a>(
        &'a self,
        request: &'a openwebide_core::AssistanceRequest,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::assistance::GitDraftResult, String>> {
        Box::pin(async move {
            let runtime = self
                .model_runtime(request.connection_id, request.model.as_deref())
                .await?;
            let text = self
                .assistance(request)
                .await?
                .ok_or("The model could not produce a draft. Try again.")?;
            Ok(openwebide_core::assistance::GitDraftResult {
                text,
                context_limit: runtime
                    .settings
                    .context_limit
                    .or(runtime.connection.context_limit)
                    .unwrap_or(8192),
                output_limit: runtime.settings.max_output_tokens.unwrap_or(512).min(512),
                timeout_seconds: runtime.transport.timeout_seconds,
                input_tokens: request.input.len(),
                estimated: true,
            })
        })
    }
    fn assistance<'a>(
        &'a self,
        request: &'a openwebide_core::AssistanceRequest,
    ) -> LocalBoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(async move {
            self.assistance_requests.borrow_mut().push(request.clone());
            let pending = self.assistance_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                pending
                    .await
                    .map_err(|_| "Assistance cancelled".to_owned())?
            } else {
                Ok(None)
            }
        })
    }
    fn model_complete<'a>(
        &'a self,
        request: &'a ChatRequest,
    ) -> LocalBoxFuture<'a, Result<ChatCompletion, String>> {
        Box::pin(async move {
            self.background_requests.borrow_mut().push(request.clone());
            if let Some(result) = self.background_completion.borrow().clone() {
                result
            } else {
                self.chat_tools(request).await
            }
        })
    }
    fn chat_tools<'a>(
        &'a self,
        request: &'a ChatRequest,
    ) -> LocalBoxFuture<'a, Result<ChatCompletion, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "chat_tools",
            });
            self.completion_requests.borrow_mut().push(request.clone());
            self.scripted_completions
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| "chat_tools has no scripted response".into())
        })
    }
    fn get_goal(
        &self,
        session: i64,
    ) -> LocalBoxFuture<'_, Result<Option<openwebide_core::Goal>, String>> {
        Box::pin(async move {
            self.calls
                .borrow_mut()
                .push(Call::Request { method: "get_goal" });
            let pending = self.goal_load_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending.await.map_err(|error| error.to_string())?;
            }
            Ok(self.goals.borrow().get(&session).cloned())
        })
    }
    fn update_goal<'a>(
        &'a self,
        session: i64,
        revision: u64,
        command: &'a openwebide_core::GoalCommand,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::Goal, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "update_goal",
            });
            let existing = self.goals.borrow().get(&session).cloned();
            if existing.as_ref().map_or(0, |goal| goal.revision) != revision {
                return Err("Goal changed".into());
            }
            let goal =
                openwebide_core::Goal::transition(existing.as_ref(), session, command.clone(), 1)?;
            self.goals.borrow_mut().insert(session, goal.clone());
            Ok(goal)
        })
    }
    fn dispatch_goal<'a>(
        &'a self,
        session: i64,
        revision: u64,
        command: &'a openwebide_core::GoalCommand,
        binding: Option<&'a openwebide_core::scheduled::HostBinding>,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::Goal, String>> {
        Box::pin(async move {
            self.goal_bindings.borrow_mut().push(binding.cloned());
            let mut goal = self.update_goal(session, revision, command).await?;
            goal.worker = true;
            self.goals.borrow_mut().insert(session, goal.clone());
            Ok(goal)
        })
    }
    fn compact_session<'a>(
        &'a self,
        session: i64,
        _model: Option<&'a str>,
    ) -> LocalBoxFuture<'a, Result<ChatMessage, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "compact_session",
            });
            let pending = self.compact_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending.await.map_err(|error| error.to_string())?;
            }
            if let Some(error) = self.compact_error.borrow().as_ref() {
                return Err(error.clone());
            }
            let summary = openwebide_core::Compaction {
                summary: "Saved progress".into(),
                retained: Vec::new(),
                through_message_id: 1,
            };
            self.persist_message(
                session,
                Role::System,
                &summary
                    .stored_content()
                    .map_err(|error| error.to_string())?,
                None,
                None,
            )
            .await
        })
    }
    fn scheduled_tasks(
        &self,
        project: Option<i64>,
    ) -> LocalBoxFuture<'_, Result<Vec<openwebide_core::scheduled::ScheduledTask>, String>> {
        Box::pin(async move {
            let pending = self.scheduled_load_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending.await.map_err(|error| error.to_string())?;
            }
            Ok(self
                .scheduled
                .borrow()
                .iter()
                .filter(|task| task.project_id == project)
                .cloned()
                .collect())
        })
    }
    fn scheduled_command<'a>(
        &'a self,
        project: Option<i64>,
        command: &'a openwebide_core::scheduled::TaskCommand,
        binding: Option<&'a openwebide_core::scheduled::HostBinding>,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::scheduled::ScheduledTask>, String>> {
        self.scheduled_commands
            .borrow_mut()
            .push((project, command.clone()));
        self.fixture_tasks(project, command, binding)
    }
    fn project_skills(
        &self,
        project: i64,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::ProjectSkills, String>> {
        Box::pin(async move {
            let pending = self.skill_load_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending.await.map_err(|e| e.to_string())?;
            }
            Ok(self
                .skills
                .borrow()
                .get(&project)
                .cloned()
                .unwrap_or_default())
        })
    }
    fn session_skills(
        &self,
        session: i64,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::ProjectSkills, String>> {
        Box::pin(async move {
            let project = self
                .sessions
                .borrow()
                .iter()
                .find(|entry| entry.id == session)
                .and_then(|entry| entry.project_id);
            match project {
                Some(project) => self.project_skills(project).await,
                None => Ok(openwebide_core::ProjectSkills {
                    enabled: false,
                    entries: Vec::new(),
                }),
            }
        })
    }
    fn skill_command<'a>(
        &'a self,
        id: i64,
        command: &'a openwebide_core::SkillCommand,
        session: bool,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ProjectSkills, String>> {
        Box::pin(async move {
            use openwebide_core::{ProjectSkill, SkillCommand};
            command.validate()?;
            self.skill_commands
                .borrow_mut()
                .push((id, command.clone(), session));
            let pending = self.skill_command_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending.await.map_err(|e| e.to_string())?;
            }
            let project = if session {
                self.sessions
                    .borrow()
                    .iter()
                    .find(|entry| entry.id == id)
                    .and_then(|entry| entry.project_id)
                    .ok_or("Project skills require a project")?
            } else {
                id
            };
            let mut map = self.skills.borrow_mut();
            let data = map.entry(project).or_default();
            if session && (!data.enabled || matches!(command, SkillCommand::SetEnabled { .. })) {
                return Err("Project skills disabled".into());
            }
            match command {
                SkillCommand::Create { draft } => {
                    if data
                        .entries
                        .iter()
                        .any(|entry| entry.draft.name == draft.name)
                    {
                        return Err("Skill name already exists".into());
                    }
                    let id = data.entries.iter().map(|entry| entry.id).max().unwrap_or(0) + 1;
                    data.entries.push(ProjectSkill {
                        plugin: None,
                        id,
                        revision: 1,
                        updated_at: 0,
                        draft: draft.clone(),
                    });
                }
                SkillCommand::Update {
                    id,
                    revision,
                    draft,
                } => {
                    let entry = data
                        .entries
                        .iter_mut()
                        .find(|entry| entry.id == *id && entry.revision == *revision)
                        .ok_or("Skill changed")?;
                    entry.draft = draft.clone();
                    entry.revision += 1;
                }
                SkillCommand::Delete { id, revision } => {
                    let index = data
                        .entries
                        .iter()
                        .position(|entry| entry.id == *id && entry.revision == *revision)
                        .ok_or("Skill changed")?;
                    data.entries.remove(index);
                }
                SkillCommand::SetEnabled { enabled } => data.enabled = *enabled,
                SkillCommand::List { query } => {
                    return Ok(openwebide_core::ProjectSkills {
                        enabled: data.enabled,
                        entries: data
                            .entries
                            .iter()
                            .filter(|entry| {
                                (!session || entry.draft.enabled)
                                    && (entry.draft.name.contains(query)
                                        || entry.draft.description.contains(query))
                            })
                            .cloned()
                            .collect(),
                    });
                }
                SkillCommand::Read { id, resource } => {
                    let entry = data
                        .entries
                        .iter()
                        .find(|entry| entry.id == *id && (!session || entry.draft.enabled))
                        .ok_or("Skill not found")?;
                    if resource
                        .as_ref()
                        .is_some_and(|name| !entry.draft.resources.iter().any(|r| &r.name == name))
                    {
                        return Err("Resource not found".into());
                    }
                    return Ok(openwebide_core::ProjectSkills {
                        enabled: data.enabled,
                        entries: vec![entry.clone()],
                    });
                }
            }
            Ok(data.clone())
        })
    }
    fn scheduled_monitors(
        &self,
        session: i64,
    ) -> LocalBoxFuture<'_, Result<Vec<openwebide_core::scheduled::ScheduledTask>, String>> {
        Box::pin(async move {
            let pending = self.monitor_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending.await.map_err(|error| error.to_string())?;
            }
            Ok(self
                .monitors
                .borrow()
                .get(&session)
                .cloned()
                .unwrap_or_default())
        })
    }
    fn scheduled_session_command<'a>(
        &'a self,
        session: i64,
        command: &'a openwebide_core::scheduled::TaskCommand,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::scheduled::ScheduledTask>, String>> {
        self.scheduled_commands
            .borrow_mut()
            .push((None, command.clone()));
        self.fixture_monitor(session, command)
    }
    fn project_memories(
        &self,
        project: i64,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::ProjectMemories, String>> {
        Box::pin(async move {
            let pending = self.memory_load_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending.await.map_err(|error| error.to_string())?;
            }
            Ok(self
                .memories
                .borrow()
                .get(&project)
                .cloned()
                .unwrap_or_default())
        })
    }
    fn session_memories(
        &self,
        session: i64,
    ) -> LocalBoxFuture<'_, Result<openwebide_core::ProjectMemories, String>> {
        Box::pin(async move {
            let project = self
                .sessions
                .borrow()
                .iter()
                .find(|entry| entry.id == session)
                .and_then(|entry| entry.project_id);
            match project {
                Some(project) => self.project_memories(project).await,
                None => Ok(openwebide_core::ProjectMemories {
                    enabled: false,
                    entries: Vec::new(),
                }),
            }
        })
    }
    fn memory_command<'a>(
        &'a self,
        id: i64,
        command: &'a openwebide_core::MemoryCommand,
        session: bool,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ProjectMemories, String>> {
        Box::pin(async move {
            use openwebide_core::{MemoryCommand, ProjectMemory};
            command.validate()?;
            self.memory_commands
                .borrow_mut()
                .push((id, command.clone(), session));
            let pending = self.memory_command_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending.await.map_err(|error| error.to_string())?;
            }
            let project = if session {
                self.sessions
                    .borrow()
                    .iter()
                    .find(|entry| entry.id == id)
                    .and_then(|entry| entry.project_id)
                    .ok_or("Project memory requires a project")?
            } else {
                id
            };
            let mut entries = self.memories.borrow_mut();
            let data = entries.entry(project).or_default();
            if session && !data.enabled {
                return Err("Project memory disabled".into());
            }
            match command {
                MemoryCommand::Create {
                    auto_title,
                    title,
                    content,
                } => {
                    let id = data.entries.iter().map(|entry| entry.id).max().unwrap_or(0) + 1;
                    data.entries.insert(
                        0,
                        ProjectMemory {
                            auto_title: *auto_title,
                            id,
                            title: title.clone(),
                            content: content.clone(),
                            revision: 1,
                            updated_at: 0,
                        },
                    );
                }
                MemoryCommand::Update {
                    auto_title,
                    id,
                    revision,
                    title,
                    content,
                } => {
                    let entry = data
                        .entries
                        .iter_mut()
                        .find(|entry| entry.id == *id && entry.revision == *revision)
                        .ok_or("Memory changed. Refresh before editing.")?;
                    entry.auto_title = *auto_title;
                    entry.title = title.clone();
                    entry.content = content.clone();
                    entry.revision += 1;
                }
                MemoryCommand::Delete { id, revision } => {
                    let index = data
                        .entries
                        .iter()
                        .position(|entry| entry.id == *id && entry.revision == *revision)
                        .ok_or("Memory changed. Refresh before deleting.")?;
                    data.entries.remove(index);
                }
                MemoryCommand::SetEnabled { enabled } => {
                    if session {
                        return Err("Agents cannot toggle memory".into());
                    }
                    data.enabled = *enabled;
                }
                MemoryCommand::Search { query } => {
                    return Ok(openwebide_core::ProjectMemories {
                        enabled: data.enabled,
                        entries: data
                            .entries
                            .iter()
                            .filter(|entry| {
                                entry.title.contains(query) || entry.content.contains(query)
                            })
                            .cloned()
                            .collect(),
                    });
                }
                MemoryCommand::Read { id } => {
                    return Ok(openwebide_core::ProjectMemories {
                        enabled: data.enabled,
                        entries: vec![
                            data.entries
                                .iter()
                                .find(|entry| entry.id == *id)
                                .ok_or("Memory missing")?
                                .clone(),
                        ],
                    });
                }
            }
            Ok(data.clone())
        })
    }
    fn get_todo_plan(
        &self,
        session: i64,
    ) -> LocalBoxFuture<'_, Result<Option<openwebide_core::TodoUpdate>, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "get_todo_plan",
            });
            let pending = self.todo_load_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending.await.map_err(|error| error.to_string())?;
            }
            Ok(self
                .todo_updates
                .borrow()
                .get(&session)
                .and_then(|updates| updates.last().cloned()))
        })
    }
    fn write_todo_plan<'a>(
        &'a self,
        session: i64,
        anchor: i64,
        plan: &'a openwebide_core::TodoPlan,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::TodoUpdate, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "write_todo_plan",
            });
            plan.validate()?;
            if let Some(error) = self.todo_errors.borrow_mut().pop_front() {
                return Err(error);
            }
            let messages = self.messages.borrow();
            let latest = messages.get(&session).and_then(|entries| {
                entries.iter().rev().find_map(|entry| match entry {
                    ConversationEntry::Message(message) if message.role == Role::User => {
                        Some(message.id)
                    }
                    _ => None,
                })
            });
            if latest != Some(anchor) {
                return Err("Plan belongs to an earlier prompt".into());
            }
            let mut updates = self.todo_updates.borrow_mut();
            let updates = updates.entry(session).or_default();
            let update = openwebide_core::TodoUpdate {
                id: updates.last().map_or(1, |update| update.id + 1),
                session_id: session,
                anchor_message_id: anchor,
                created_at: 0,
                plan: plan.clone(),
            };
            updates.push(update.clone());
            Ok(update)
        })
    }
    fn fork_session<'a>(
        &'a self,
        source: i64,
        target: i64,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::ForkedSession, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "fork_session",
            });
            let pending = self.fork_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending.await.map_err(|error| error.to_string())?;
            }
            if let Some(error) = self.fork_errors.borrow_mut().pop_front() {
                return Err(error);
            }

            let source_session = self
                .sessions
                .borrow()
                .iter()
                .find(|session| session.id == source)
                .cloned()
                .ok_or("Session missing")?;
            let entries = self
                .messages
                .borrow()
                .get(&source)
                .cloned()
                .unwrap_or_default();
            let prompt = entries
                .iter()
                .find_map(|entry| match entry {
                    ConversationEntry::Message(message)
                        if message.id == target && message.role == Role::User =>
                    {
                        Some(message.content.clone())
                    }
                    _ => None,
                })
                .ok_or("Prompt missing")?;
            let session = self
                .create_session(
                    &format!("{} (branch)", source_session.name),
                    source_session.connection_id,
                    source_session.system_prompt_id,
                    source_session.project_id,
                )
                .await?;
            let mut ids = BTreeMap::new();
            let mut copied = Vec::new();
            for entry in entries.iter().filter(
                |entry| matches!(entry, ConversationEntry::Message(message) if message.id < target),
            ) {
                if let ConversationEntry::Message(message) = entry {
                    let copy = self
                        .persist_message(
                            session.id,
                            message.role,
                            &message.content,
                            message.usage.as_ref(),
                            message.tool_calls.as_deref(),
                        )
                        .await?;
                    ids.insert(message.id, copy.id);
                    copied.push(ConversationEntry::Message(copy));
                }
            }
            for entry in entries.iter().filter(|entry| matches!(entry, ConversationEntry::Task(task) if task.anchor_message_id < target)) {
                if let ConversationEntry::Task(task) = entry {
                    let mut task = task.clone();
                    task.anchor_message_id = ids.get(&task.anchor_message_id).copied().ok_or("Missing task anchor")?;
                    copied.push(ConversationEntry::Task(task));
                }
            }
            for entry in entries.iter().filter(|entry| matches!(entry, ConversationEntry::ToolStep(step) if step.anchor_message_id < target)) {
                if let ConversationEntry::ToolStep(step) = entry { let mut step = step.clone(); step.anchor_message_id = ids.get(&step.anchor_message_id).copied().ok_or("Missing tool anchor")?; copied.push(ConversationEntry::ToolStep(step)); }
            }
            self.messages
                .borrow_mut()
                .insert(session.id, copied.clone());
            let plans = self
                .todo_updates
                .borrow()
                .get(&source)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|update| update.anchor_message_id < target)
                .map(|mut update| {
                    update.session_id = session.id;
                    update.anchor_message_id = ids[&update.anchor_message_id];
                    update
                })
                .collect();
            self.todo_updates.borrow_mut().insert(session.id, plans);
            let mode = self
                .settings
                .borrow()
                .get(&openwebide_core::ApprovalMode::setting_key(source))
                .cloned();
            if let Some(mode) = mode {
                self.settings
                    .borrow_mut()
                    .insert(openwebide_core::ApprovalMode::setting_key(session.id), mode);
            }
            Ok(openwebide_core::ForkedSession {
                session,
                prompt,
                history: copied,
            })
        })
    }
    fn list_queued_prompts<'a>(
        &'a self,
        session: i64,
    ) -> LocalBoxFuture<'a, Result<Vec<openwebide_core::QueuedPrompt>, String>> {
        Box::pin(async move {
            let pending = self.queue_load_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending.await.map_err(|error| error.to_string())?;
            }
            Ok(self
                .queued_prompts
                .borrow()
                .get(&session)
                .cloned()
                .unwrap_or_default())
        })
    }
    fn enqueue_prompt<'a>(
        &'a self,
        session: i64,
        content: &'a str,
        guidance: bool,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::QueuedPrompt, String>> {
        Box::pin(async move {
            if let Some(error) = self.queue_errors.borrow_mut().pop_front() {
                return Err(error);
            }
            openwebide_core::chat_queue::validate_content(content)?;
            let id = self.queue_next_id.get() + 1;
            self.queue_next_id.set(id);
            let prompt = openwebide_core::QueuedPrompt {
                scheduled_task: None,
                plugin_run: None,
                id,
                session_id: session,
                revision: 1,
                content: content.into(),
                created_at: 0,
                guidance,
            };
            self.queued_prompts
                .borrow_mut()
                .entry(session)
                .or_default()
                .push(prompt.clone());
            self.queued_prompts
                .borrow_mut()
                .get_mut(&session)
                .unwrap()
                .sort_by_key(|prompt| (!prompt.guidance, prompt.id));
            Ok(prompt)
        })
    }
    fn update_queued_prompt<'a>(
        &'a self,
        session: i64,
        key: openwebide_core::QueuedPromptKey,
        content: &'a str,
    ) -> LocalBoxFuture<'a, Result<openwebide_core::QueuedPrompt, String>> {
        Box::pin(async move {
            if let Some(error) = self.queue_errors.borrow_mut().pop_front() {
                return Err(error);
            }
            openwebide_core::chat_queue::validate_content(content)?;
            let mut all = self.queued_prompts.borrow_mut();
            let prompt = all
                .entry(session)
                .or_default()
                .iter_mut()
                .find(|entry| entry.key() == key)
                .ok_or("Queued prompt changed")?;
            prompt.revision += 1;
            prompt.content = content.into();
            Ok(prompt.clone())
        })
    }
    fn remove_queued_prompt<'a>(
        &'a self,
        session: i64,
        key: openwebide_core::QueuedPromptKey,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if let Some(error) = self.queue_errors.borrow_mut().pop_front() {
                return Err(error);
            }
            let mut all = self.queued_prompts.borrow_mut();
            let queue = all.entry(session).or_default();
            let index = queue
                .iter()
                .position(|entry| entry.key() == key)
                .ok_or("Queued prompt changed")?;
            queue.remove(index);
            Ok(())
        })
    }
    fn consume_queued_prompt<'a>(
        &'a self,
        session: i64,
        key: openwebide_core::QueuedPromptKey,
        content: &'a str,
    ) -> LocalBoxFuture<'a, Result<ChatMessage, String>> {
        Box::pin(async move {
            if let Some(error) = self.queue_errors.borrow_mut().pop_front() {
                return Err(error);
            }
            let first = self
                .queued_prompts
                .borrow()
                .get(&session)
                .and_then(|queue| queue.first())
                .cloned()
                .ok_or("Queued prompt missing")?;
            if first.key() != key || first.content != content {
                return Err("Queued prompt changed".into());
            }
            let message = self
                .persist_message(session, Role::User, content, None, None)
                .await?;
            self.queued_prompts
                .borrow_mut()
                .get_mut(&session)
                .unwrap()
                .remove(0);
            Ok(message)
        })
    }
    fn persist_message<'a>(
        &'a self,
        session_id: i64,
        role: Role,
        content: &'a str,
        usage: Option<&'a TurnTelemetry>,
        tool_calls: Option<&'a [openwebide_core::ToolCall]>,
    ) -> LocalBoxFuture<'a, Result<ChatMessage, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "persist_message",
            });
            if let Some(result) = self.message_save_results.borrow_mut().pop_front() {
                result?;
            }
            let message = ChatMessage {
                id: i64::try_from(self.messages.borrow().values().map(Vec::len).sum::<usize>())
                    .expect("test message count fits i64")
                    + 1,
                session_id,
                role,
                content: content.into(),
                created_at: 0,
                tool_calls: tool_calls.map(<[openwebide_core::ToolCall]>::to_vec),
                tool_call_id: None,
                usage: usage.cloned(),
            };
            self.messages
                .borrow_mut()
                .entry(session_id)
                .or_default()
                .push(ConversationEntry::Message(message.clone()));
            Ok(message)
        })
    }
    fn model_context<'a>(
        &'a self,
        connection_id: i64,
        model: Option<&'a str>,
    ) -> LocalBoxFuture<'a, Result<Option<usize>, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "model_context",
            });
            self.context_requests
                .borrow_mut()
                .push((connection_id, model.map(str::to_string)));
            let pending = self.context_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending
                    .await
                    .unwrap_or_else(|_| Err("response dropped".into()));
            }
            let latency = *self.endpoint_latency_ms.borrow();
            if latency > 0 {
                crate::util::sleep_ms(latency).await;
            }
            Ok(self
                .connections
                .borrow()
                .iter()
                .find(|c| c.id == connection_id)
                .and_then(|c| c.context_limit))
        })
    }
    fn upsert_tool_step<'a>(
        &'a self,
        session_id: i64,
        _anchor_message_id: i64,
        tool_call_id: &'a str,
        _name: &'a str,
        _summary: &'a str,
        _diff: Option<&'a FileDiff>,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "upsert_tool_step",
            });
            if let Some(error) = self.step_save_error.borrow().as_ref() {
                return Err(error.clone());
            }
            self.tool_sources
                .borrow_mut()
                .entry((session_id, tool_call_id.into()))
                .or_insert(false);
            Ok(())
        })
    }
    fn save_task<'a>(
        &'a self,
        session: i64,
        anchor: i64,
        snapshot: &'a openwebide_core::TaskSnapshot,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "save_task",
            });
            if let Some(error) = self.step_save_error.borrow().as_ref() {
                return Err(error.clone());
            }
            let mut messages = self.messages.borrow_mut();
            let entries = messages.entry(session).or_default();
            if let Some(ConversationEntry::Task(task)) = entries.iter_mut().find(|entry| matches!(entry, ConversationEntry::Task(task) if task.snapshot.task.id == snapshot.task.id)) {
                task.snapshot = snapshot.clone();
            } else { entries.push(ConversationEntry::Task(Box::new(openwebide_core::TaskHistory { anchor_message_id: anchor, snapshot: snapshot.clone() }))); }
            Ok(())
        })
    }
    fn save_tool_timing<'a>(
        &'a self,
        session: i64,
        id: &'a str,
        timing: &'a openwebide_core::ToolTiming,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if let Some(error) = self.step_save_error.borrow().as_ref() {
                return Err(error.clone());
            }
            let mut timings = self.tool_timings.borrow_mut();
            let key = (session, id.to_string());
            let timing = timings
                .get(&key)
                .map_or(Ok(*timing), |previous| previous.merge(*timing))
                .map_err(str::to_string)?;
            timings.insert(key, timing);
            if let Some(entries) = self.messages.borrow_mut().get_mut(&session) {
                for entry in entries {
                    if let ConversationEntry::ToolStep(step) = entry
                        && step.tool_call_id == id
                    {
                        step.timing = Some(timing);
                    }
                }
            }
            Ok(())
        })
    }
    fn complete_tool_step<'a>(
        &'a self,
        session_id: i64,
        tool_call_id: &'a str,
        ok: bool,
        _result_summary: &'a str,
        diff: Option<&'a FileDiff>,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "complete_tool_step",
            });
            if let Some(error) = self.completion_error.borrow().as_ref() {
                return Err(error.clone());
            }
            let session = self
                .sessions
                .borrow()
                .iter()
                .find(|session| session.id == session_id)
                .cloned()
                .ok_or("session not found")?;
            let mut sources = self.tool_sources.borrow_mut();
            let applied = sources
                .get_mut(&(session_id, tool_call_id.into()))
                .ok_or("tool step not found")?;
            if *applied {
                return Ok(());
            }
            if let (true, Some(project_id), Some(diff)) = (ok, session.project_id, diff) {
                let mut edits = self.persisted_edits.borrow_mut();
                let key = (project_id, diff.path.clone());
                let previous = edits.get(&key);
                let revision = previous.map_or(1, |edit| edit.revision + 1);
                let mut merged = diff.clone();
                let mut decision = EditDecision::Pending;
                if let Some(previous) =
                    previous.filter(|edit| edit.decision == EditDecision::Pending)
                {
                    merged = previous.diff.clone();
                    merged.new.clone_from(&diff.new);
                    if !merged.old_unavailable && merged.old.as_deref() == Some(merged.new.as_str())
                    {
                        decision = EditDecision::Accepted;
                    }
                }
                edits.insert(
                    key,
                    PersistedEdit {
                        file: None,
                        project_id,
                        path: diff.path.clone(),
                        revision,
                        decision,
                        diff: merged,
                    },
                );
            }
            *applied = true;
            Ok(())
        })
    }
    fn list_pending_edits(
        &self,
        project_id: i64,
    ) -> LocalBoxFuture<'_, Result<Vec<PersistedEdit>, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "list_pending_edits",
            });
            let pending = self.pending_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                return pending
                    .await
                    .map_err(|_| "pending edit request cancelled".to_string())?;
            }
            Ok(self
                .persisted_edits
                .borrow()
                .values()
                .filter(|edit| {
                    edit.project_id == project_id && edit.decision == EditDecision::Pending
                })
                .cloned()
                .collect())
        })
    }
    fn resolve_pending_edit<'a>(
        &'a self,
        project_id: i64,
        request: &'a ResolveEditRequest,
    ) -> LocalBoxFuture<'a, Result<PersistedEdit, String>> {
        Box::pin(async move {
            self.resolution_requests
                .borrow_mut()
                .push((project_id, request.clone()));
            let pending = self.resolution_results.borrow_mut().pop_front();
            if let Some(pending) = pending {
                pending
                    .await
                    .map_err(|_| "resolution cancelled".to_string())??;
            }
            if let Some(error) = self.resolution_error.borrow().as_ref() {
                return Err(error.clone());
            }
            if request.decision == EditDecision::Pending || request.revision <= 0 {
                return Err("invalid edit resolution".into());
            }
            let committed = {
                let mut edits = self.persisted_edits.borrow_mut();
                let edit = edits
                    .get_mut(&(project_id, request.path.clone()))
                    .ok_or("pending edit not found")?;
                if edit.revision != request.revision
                    || (edit.decision != EditDecision::Pending && edit.decision != request.decision)
                {
                    return Err("edit revision or decision changed".into());
                }
                edit.decision = request.decision;
                edit.clone()
            };
            let response = self.resolution_response_results.borrow_mut().pop_front();
            if let Some(response) = response {
                response
                    .await
                    .map_err(|_| "response cancelled".to_string())??;
            }
            Ok(committed)
        })
    }
    fn web_search<'a>(
        &'a self,
        _query: &'a str,
        _limit: usize,
    ) -> LocalBoxFuture<'a, Result<Vec<WebSearchResult>, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "web_search",
            });
            Err("web_search has no scripted response".into())
        })
    }
    fn fetch_web_page<'a>(
        &'a self,
        _target_url: &'a str,
    ) -> LocalBoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(Call::Request {
                method: "fetch_web_page",
            });
            Err("fetch_web_page has no scripted response".into())
        })
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
        _editor_context: Option<&'a EditorContext>,
        _browser_preferences: Option<&'a openwebide_core::BrowserPreferences>,
        queued_prompt: Option<openwebide_core::QueuedPromptKey>,
        signal: Option<&'a AbortSignal>,
        mut on_event: Box<dyn FnMut(RunEvent) + 'a>,
    ) -> LocalBoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if let Some(key) = queued_prompt {
                let message = self.consume_queued_prompt(session_id, key, content).await?;
                on_event(RunEvent::Message { message });
            }

            self.calls.borrow_mut().push(Call::SendMessage {
                session: session_id,
                content: content.into(),
                model: model.map(str::to_string),
            });
            let events = self
                .scripted_events
                .borrow_mut()
                .pop_front()
                .unwrap_or_default();
            if queued_prompt.is_none() {
                if let Some(message) = events.iter().find_map(|event| match event {
                    RunEvent::Message { message } if message.role == Role::User => Some(message),
                    _ => None,
                }) {
                    self.messages
                        .borrow_mut()
                        .entry(session_id)
                        .or_default()
                        .push(ConversationEntry::Message(message.clone()));
                } else {
                    self.persist_message(session_id, Role::User, content, None, None)
                        .await?;
                }
            }
            let mut assistant = None;
            let mut deltas = String::new();
            let mut interrupted = false;
            for event in events {
                if signal.is_some_and(AbortSignal::aborted) {
                    return Err("aborted".into());
                }
                match &event {
                    RunEvent::Delta { content: delta } => deltas.push_str(delta),
                    RunEvent::Message { message } | RunEvent::Done { message }
                        if message.role == Role::Assistant =>
                    {
                        assistant = Some(message.clone());
                    }
                    RunEvent::Cancelled | RunEvent::Error { .. } => interrupted = true,
                    _ => {}
                }
                on_event(event);
            }
            if let Some(message) = assistant {
                self.messages
                    .borrow_mut()
                    .entry(session_id)
                    .or_default()
                    .push(ConversationEntry::Message(message));
            } else if !interrupted && !deltas.is_empty() {
                self.persist_message(session_id, Role::Assistant, &deltas, None, None)
                    .await?;
            }
            Ok(())
        })
    }
}
