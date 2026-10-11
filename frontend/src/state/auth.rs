use leptos::prelude::*;
use openwebide_core::{SessionTelemetry, User};

use super::{
    chat::ChatState, git::GitState, projects::ProjectsState, settings::SettingsState, ui::UiState,
    workspace::WorkspaceState,
};

/// Authentication state shared by the gate and app shell.
#[derive(Clone, Copy)]
pub struct AuthState {
    pub user: RwSignal<Option<User>>,
    pub checked: RwSignal<bool>,
    pub generation: RwSignal<u64>,
    pub username: Memo<Option<String>>,
    #[cfg(target_arch = "wasm32")]
    pub bridge: StoredValue<Option<crate::bridge::BridgeConn>, LocalStorage>,
}

impl AuthState {
    pub fn new() -> Self {
        let user = RwSignal::new(Option::<User>::None);
        let checked = RwSignal::new(false);
        let username =
            Memo::new(move |_| user.with(|user| user.as_ref().map(|user| user.username.clone())));

        Self {
            user,
            checked,
            generation: RwSignal::new(0),
            username,
            #[cfg(target_arch = "wasm32")]
            bridge: StoredValue::new_local(None),
        }
    }

    pub fn set_user(&self, user: User) {
        self.generation.update(|generation| *generation += 1);
        self.user.set(Some(user));
    }

    pub fn mark_checked(&self) {
        self.checked.set(true);
    }

    /// Clear the signed-in account while keeping the completed auth check.
    pub fn logout(&self) {
        self.generation.update(|generation| *generation += 1);
        self.user.set(None);
    }

    /// Clear user-scoped frontend state after logout or session expiry.
    /// Returns object URLs for the browser shell to revoke.
    pub fn reset_user_state(
        &self,
        projects: ProjectsState,
        workspace: WorkspaceState,
        git: GitState,
        chat: ChatState,
        settings: SettingsState,
        ui: UiState,
    ) -> Vec<String> {
        #[cfg(target_arch = "wasm32")]
        self.bridge.update_value(|connection| {
            if let Some(connection) = connection.take() {
                connection.close();
            }
        });
        ui.whats_new_open.set(false);
        ui.about_release_notes.set(false);
        projects.projects_loaded.set(false);
        #[cfg(target_arch = "wasm32")]
        if let Some(controller) = chat.abort.get() {
            controller.abort();
        }
        chat.local_cancel_flag
            .with_value(|flag| flag.store(true, std::sync::atomic::Ordering::Relaxed));
        chat.creating_session.set(false);
        chat.streaming.set(false);
        chat.rewinding.set(false);
        chat.prompt_images.set(Vec::new());
        chat.todo_plan.set(None);
        chat.todo_loading.set(false);
        chat.todo_error.set(None);
        chat.queued_prompts.set(Vec::new());
        chat.queue_busy.set(false);
        chat.queue_loading.set(false);
        chat.queue_running.set(Default::default());
        chat.queue_edit.set(None);
        chat.queue_steering.set(None);
        chat.queue_delivering.set(None);
        chat.queue_epoch.update_value(|epoch| *epoch += 1);
        chat.loading_history.set(None);
        chat.prompt_edit.set(None);
        chat.branching.set(false);
        chat.branch_draft_context.set_value(None);
        chat.reading_images.set(false);
        chat.image_picker_requested.set(false);
        settings.browser_notifications.set(false);
        settings.editor_preferences.set(Default::default());
        settings.editor_preference_revision.set(0);
        chat.send_generation
            .update_value(|generation| *generation += 1);
        chat.active_run.set(None);
        chat.notice.set(None);
        chat.interrupted_run.set(None);
        chat.dismissed_interruptions.set_value(Default::default());
        chat.bridge_folder_notices.set_value(Default::default());
        chat.streaming_session.set(None);

        self.logout();
        chat.active_session.set(None);
        projects.reset();
        chat.sessions.set(Vec::new());
        chat.messages.set(Vec::new());
        chat.prompt_history.set(Vec::new());
        let mut object_urls = workspace
            .snapshots
            .get_untracked()
            .into_values()
            .filter_map(|snapshot| snapshot.media_url)
            .collect::<Vec<_>>();
        object_urls.extend(workspace.media_url.get_untracked());
        workspace.reset();
        git.reset();

        chat.approval_mode.set(Default::default());
        chat.draft_approval_mode
            .set(openwebide_core::ApprovalMode::NEW_SESSION);
        chat.last_sessions.set(Default::default());
        chat.last_chat_session.set(None);
        chat.current_run_anchor.set(None);
        chat.session_telemetry.set(SessionTelemetry::default());
        settings.connections.set(Vec::new());
        settings.system_prompts.set(Vec::new());
        settings.show_prompt_form.set(false);
        settings.prompt_edit_id.set(None);
        settings.prompt_name.set(String::new());
        settings.prompt_content.set(String::new());
        chat.models.set(Vec::new());
        chat.session_model.set(Default::default());
        chat.selected_model.set(None);
        chat.draft_connection.set(None);
        chat.connection_changing.set(false);
        settings.default_connection.set(None);
        settings.model_setup.set(Default::default());
        settings.model_defaults_saving.set(None);
        settings.model_defaults_error.set(None);
        settings.model_defaults_revision.set(0);
        settings.pending_model_defaults.set_value(None);
        settings.show_conn_form.set(false);
        settings.conn_edit_id.set(None);
        settings.conn_base_url.set(String::new());
        settings.default_prompt.set(None);
        chat.show_terminal.set(false);
        chat.active_editor_context.set(None);
        ui.plugins_open.set(false);
        settings.requested_tab.set(0);
        ui.clear_toast();

        object_urls
    }
}

impl Default for AuthState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwebide_agent::policy::ApprovalMode;
    use openwebide_core::{
        ChatSession, Connection, FileDiff, FileEntry, GitRepoStatus, Project, ProviderKind,
        SearchHit, SystemPrompt, UserRole, WorkspaceMode,
    };

    use crate::state::{projects::ProjectsState, settings::Theme, workspace::WorkspaceSnapshot};

    #[test]
    fn logout_clears_the_user_and_derived_username_but_keeps_checked_state() {
        Owner::new().with(|| {
            let auth = AuthState::new();
            auth.set_user(User {
                id: openwebide_core::UserId::new(1),
                username: "alice".into(),
                role: UserRole::User,
                created_at: 0,
            });
            auth.mark_checked();

            assert_eq!(auth.username.get_untracked().as_deref(), Some("alice"));

            auth.logout();

            assert!(auth.user.get_untracked().is_none());
            assert!(auth.username.get_untracked().is_none());
            assert!(auth.checked.get_untracked());
        });
    }

    #[test]
    fn reset_user_state_clears_user_scoped_stores_and_returns_media_urls() {
        Owner::new().with(|| {
            let auth = AuthState::new();
            auth.set_user(User {
                id: openwebide_core::UserId::new(1),
                username: "alice".into(),
                role: UserRole::User,
                created_at: 0,
            });
            auth.mark_checked();

            let projects = ProjectsState::new();
            let workspace = WorkspaceState::with_active_project(projects.active_project);
            let chat = ChatState::with_active_session(workspace.active_session);
            let git = GitState::with_active_project(projects.active_project);
            let settings = SettingsState::new(Theme::Light, "https://bridge.test".into());
            let ui = UiState::new();

            projects.projects_loaded.set(true);
            projects.active_project.set(Some(1));
            projects.needs_grant.set([1].into_iter().collect());
            projects.projects.set(vec![Project {
                id: 1,
                name: "project".into(),
                mode: WorkspaceMode::Remote,
                path: None,
                user_id: Some(openwebide_core::UserId::new(1)),
                created_at: 0,
            }]);
            projects.open_tab_ids.set(vec![1]);
            workspace.entries.set(
                [(
                    String::new(),
                    vec![FileEntry {
                        name: "main.rs".into(),
                        path: "main.rs".into(),
                        is_dir: false,
                        size: 12,
                    }],
                )]
                .into_iter()
                .collect(),
            );
            workspace
                .expanded
                .set(["src".to_string()].into_iter().collect());
            workspace.open_file.set(Some("main.rs".into()));
            workspace.content.set("unsaved edit".into());
            workspace.dirty.set(true);
            workspace.search.set(Some(vec![SearchHit {
                path: "main.rs".into(),
                line: 1,
                text: "match".into(),
            }]));
            workspace.pending_edits.set(
                [(
                    "main.rs".into(),
                    FileDiff {
                        path: "main.rs".into(),
                        old: Some("old".into()),
                        new: "new".into(),
                        old_unavailable: false,
                        backup_path: None,
                    },
                )]
                .into_iter()
                .collect(),
            );
            workspace.active_session.set(Some(2));
            workspace.media_url.set(Some("blob:active".into()));
            workspace.snapshots.set(
                [(
                    1,
                    WorkspaceSnapshot {
                        media_url: Some("blob:saved".into()),
                        ..WorkspaceSnapshot::default()
                    },
                )]
                .into_iter()
                .collect(),
            );
            git.status.set(Some(GitRepoStatus::default()));
            chat.sessions.set(vec![ChatSession {
                pinned: false,
                archived: false,
                auto_title: false,
                title_revision: 0,
                id: 2,
                name: "session".into(),
                connection_id: Some(3),
                system_prompt_id: None,
                project_id: Some(1),
                user_id: Some(openwebide_core::UserId::new(1)),
                created_at: 0,
            }]);
            chat.prompt_history.set(vec!["private prompt".into()]);
            chat.notify("notice");
            chat.active_run.set(Some((2, "run".into(), 1)));
            chat.notice.set(Some("fallback".into()));
            chat.interrupted_run
                .set(Some(crate::state::chat::InterruptedRun {
                    anchor_id: 4,
                    first_turn: 2,
                }));
            chat.dismissed_interruptions.update_value(|dismissed| {
                dismissed.insert(4);
            });
            chat.streaming.set(true);
            chat.streaming_session.set(Some(2));
            chat.local_cancel_flag
                .with_value(|flag| flag.store(false, std::sync::atomic::Ordering::Relaxed));
            chat.models.set(vec![openwebide_core::ModelInfo {
                name: "model".into(),
            }]);
            chat.selected_model.set(Some("model".into()));
            chat.session_model.update(|models| {
                models.insert(2, Some("model".into()));
            });
            chat.approval_mode
                .set([(2, ApprovalMode::AlwaysForSession)].into_iter().collect());
            chat.current_run_anchor.set(Some(4));
            chat.session_telemetry.update(|telemetry| {
                telemetry.tool_calls_count = 2;
            });
            chat.show_terminal.set(true);
            chat.active_editor_context
                .set(Some(openwebide_core::EditorContext {
                    file_path: "main.rs".into(),
                    cursor_line: 1,
                    cursor_col: 1,
                    selection: None,
                }));
            settings.connections.set(vec![Connection {
                id: 3,
                name: "connection".into(),
                kind: ProviderKind::Ollama,
                base_url: "http://localhost:11434".into(),
                model: None,
                enabled: true,
                context_limit: None,
                tool_stream_unsupported: false,
                tool_stream_revision: 0,
                tool_selection: Default::default(),
            }]);
            settings.system_prompts.set(vec![SystemPrompt {
                id: 4,
                name: "prompt".into(),
                content: "prompt".into(),
            }]);
            settings.default_connection.set(Some(3));
            settings.default_prompt.set(Some(4));
            settings.model_defaults_saving.set(Some(1));
            settings
                .model_defaults_error
                .set(Some((1, "Private save failure".into())));
            settings
                .pending_model_defaults
                .set_value(Some((1, Default::default())));
            settings.show_prompt_form.set(true);
            settings.prompt_edit_id.set(Some(4));
            settings.prompt_name.set("Private prompt".into());
            settings.prompt_content.set("Private instructions".into());
            ui.notify("session expired");

            let urls = auth.reset_user_state(projects, workspace, git, chat, settings, ui);

            assert!(!projects.projects_loaded.get_untracked());
            assert!(projects.projects.get_untracked().is_empty());
            assert!(projects.open_tab_ids.get_untracked().is_empty());
            assert!(projects.needs_grant.get_untracked().is_empty());
            assert!(projects.active_project.get_untracked().is_none());
            assert!(workspace.active_session.get_untracked().is_none());
            assert!(workspace.entries.get_untracked().is_empty());
            assert!(workspace.expanded.get_untracked().is_empty());
            assert!(workspace.open_file.get_untracked().is_none());
            assert!(workspace.content.get_untracked().is_empty());
            assert!(!workspace.dirty.get_untracked());
            assert!(workspace.search.get_untracked().is_none());
            assert!(workspace.pending_edits.get_untracked().is_empty());
            assert!(workspace.pending_diff.get_untracked().is_none());
            assert!(workspace.snapshots.get_untracked().is_empty());
            assert!(workspace.media_url.get_untracked().is_none());
            assert!(git.status.get_untracked().is_none());
            assert!(chat.sessions.get_untracked().is_empty());
            assert!(chat.messages.get_untracked().is_empty());
            assert!(chat.prompt_history.get_untracked().is_empty());
            assert!(!chat.streaming.get_untracked());
            assert!(chat.active_run.get_untracked().is_none());
            assert!(chat.notice.get_untracked().is_none());
            assert!(chat.interrupted_run.get_untracked().is_none());
            assert!(chat.dismissed_interruptions.get_value().is_empty());
            assert!(chat.streaming_session.get_untracked().is_none());
            assert!(
                chat.local_cancel_flag
                    .with_value(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
            );
            assert!(chat.models.get_untracked().is_empty());
            assert!(chat.selected_model.get_untracked().is_none());
            assert!(chat.session_model.get_untracked().is_empty());
            assert!(chat.approval_mode.get_untracked().is_empty());
            assert!(chat.current_run_anchor.get_untracked().is_none());
            assert_eq!(
                chat.session_telemetry.get_untracked(),
                SessionTelemetry::default()
            );
            assert!(!chat.show_terminal.get_untracked());
            assert!(chat.active_editor_context.get_untracked().is_none());
            assert!(settings.connections.get_untracked().is_empty());
            assert!(settings.system_prompts.get_untracked().is_empty());
            assert!(!settings.show_prompt_form.get_untracked());
            assert!(settings.prompt_edit_id.get_untracked().is_none());
            assert!(settings.prompt_name.get_untracked().is_empty());
            assert!(settings.prompt_content.get_untracked().is_empty());
            assert!(settings.default_connection.get_untracked().is_none());
            assert!(settings.default_prompt.get_untracked().is_none());
            assert!(settings.model_defaults_saving.get_untracked().is_none());
            assert!(settings.model_defaults_error.get_untracked().is_none());
            assert!(settings.pending_model_defaults.get_value().is_none());
            assert!(ui.toast.get_untracked().is_none());
            assert!(auth.user.get_untracked().is_none());
            assert!(auth.checked.get_untracked());
            assert_eq!(urls, vec!["blob:saved", "blob:active"]);
        });
    }
}
