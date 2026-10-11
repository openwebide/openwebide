use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::state::{
    auth::AuthState,
    chat::ChatState,
    git::GitState,
    projects::ProjectsState,
    settings::SettingsState,
    ui::{ConfirmRequest, UiState},
    workspace::WorkspaceState,
};

use crate::backend::Api;

pub struct AuthActionContext {
    pub api: Api,
    pub auth: AuthState,
    pub projects: ProjectsState,
    pub workspace: WorkspaceState,
    pub git: GitState,
    pub chat: ChatState,
    pub settings: SettingsState,
    pub ui: UiState,
}

#[derive(Clone, Copy)]
pub struct AuthActions {
    pub on_logout: Callback<()>,
}

impl AuthActions {
    pub fn new(context: AuthActionContext) -> Self {
        let AuthActionContext {
            api,
            auth,
            projects,
            workspace,
            git,
            chat,
            settings,
            ui,
        } = context;
        let reset_user_state = Callback::new(move |()| {
            for url in auth.reset_user_state(projects, workspace, git, chat, settings, ui) {
                let _ = web_sys::Url::revoke_object_url(&url);
            }
        });

        Effect::new(move |_| {
            if auth.checked.get() {
                return;
            }
            let generation = auth.generation.get_untracked();
            spawn_local(async move {
                let user = api.with_value(Clone::clone).me().await.ok();
                if auth.generation.get_untracked() != generation {
                    return;
                }
                if let Some(user) = user {
                    auth.set_user(user);
                }
                auth.checked.set(true);
            });
        });

        Effect::new(move |_| {
            if api.with_value(Clone::clone).session_expired().get() {
                reset_user_state.run(());
                ui.notify("Your session expired. Please sign in again.");
                api.with_value(Clone::clone).session_expired().set(false);
            }
        });

        let on_logout = Callback::new(move |()| {
            let has_unsaved = workspace.dirty.get_untracked()
                || workspace
                    .snapshots
                    .get_untracked()
                    .values()
                    .any(|snapshot| snapshot.dirty);
            let mut message =
                "Log out of this account? Open tabs and projects will be closed.".to_string();
            if has_unsaved {
                message.push_str(" Unsaved changes will be lost.");
            }
            ui.set_confirm(ConfirmRequest {
                title: "Log out".to_string(),
                message,
                confirm_label: "Log out".to_string(),
                action: Callback::new(move |()| {
                    spawn_local(async move {
                        let _ = api.with_value(Clone::clone).logout().await;
                    });
                    reset_user_state.run(());
                }),
            });
        });

        Self { on_logout }
    }
}
