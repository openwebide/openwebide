//! App-owned rewind orchestration above the shared workspace primitives.
use crate::{
    backend::Api,
    state::{
        auth::AuthState,
        chat::ChatState,
        projects::ProjectsState,
        ui::{ConfirmRequest, UiState},
        workspace::WorkspaceState,
    },
    workspace::Workspace,
};
use leptos::{prelude::*, task::spawn_local};

struct RewindCompletion {
    busy: RwSignal<bool>,
    serial: std::rc::Rc<std::cell::Cell<u64>>,
    token: u64,
}
impl Drop for RewindCompletion {
    fn drop(&mut self) {
        if self.serial.get() == self.token {
            self.busy.try_set(false);
        }
    }
}

pub fn actions(
    api: Api,
    chat: ChatState,
    projects: ProjectsState,
    workspace: WorkspaceState,
    ui: UiState,
    request_open: Callback<String>,
    refresh_git: Callback<()>,
) -> Callback<i64> {
    let auth = expect_context::<AuthState>();
    let todos = use_context::<super::todos::TodoActions>();
    let settings = use_context::<crate::state::settings::SettingsState>();
    let epoch = super::workspace::project_epoch(projects, auth);
    let serial = StoredValue::new_local(std::rc::Rc::new(std::cell::Cell::new(0_u64)));
    Callback::new(move |message: i64| {
        let Some(session) = chat.active_session.get_untracked() else {
            return;
        };
        if (chat.streaming.get_untracked()
            || chat.compacting.get_untracked()
            || chat.goal_busy.get_untracked())
            || chat.rewinding.get_untracked()
        {
            return;
        }
        let project = projects.active_project.get_untracked();
        let generation = auth.generation.get_untracked();
        let original_epoch = epoch.get_untracked();
        let current = move || {
            auth.generation.try_get_untracked() == Some(generation)
                && epoch.try_get_untracked() == Some(original_epoch)
                && chat.active_session.try_get_untracked() == Some(Some(session))
                && projects.active_project.try_get_untracked() == Some(project)
        };
        let mutations = match project
            .map(|id| {
                crate::workspace_mutations::WorkspaceMutations::for_project(
                    api, projects, auth, settings, id, current,
                )
            })
            .transpose()
        {
            Ok(mutations) => mutations,
            Err(error) => {
                ui.notify(error);
                return;
            }
        };
        let mutations = mutations.map(|scope| {
            scope.with_preflight(move || {
                if workspace.dirty.get_untracked() {
                    Err("Save or discard the editor's unsaved changes before rewinding".into())
                } else {
                    Ok(())
                }
            })
        });
        let mutations = send_wrapper::SendWrapper::new(mutations);
        spawn_local(async move {
            let backend = api.with_value(Clone::clone);
            let preview = match backend.list_messages(session).await.and_then(|entries| {
                openwebide_core::RewindPlan::from_conversation(&entries, message)
            }) {
                Ok(plan) => plan,
                Err(error) => {
                    if current() {
                        ui.notify(error);
                    }
                    return;
                }
            };
            if !current() {
                return;
            }
            let mut explanation = "Restore project files and the conversation to before this prompt, including shell-made file changes. Git history and external effects stay intact. Gitignored paths are excluded from shell checkpoints. The prompt will replace the current draft. Files must match their recorded snapshots.".to_string();
            if let Some(warning) = openwebide_core::rewind::coverage_warning(&preview.skipped) {
                explanation.push_str(&format!(
                    "\n\n{warning}. These files will keep their current contents."
                ));
            }
            ui.set_confirm(ConfirmRequest {
            title: "Rewind to this prompt?".into(),
            message: explanation,
            confirm_label: "Rewind".into(),
            action: Callback::new(move |()| {
                if !current() || (chat.streaming.get_untracked() || chat.compacting.get_untracked() || chat.goal_busy.get_untracked()) || chat.rewinding.get_untracked() { return; }
                if workspace.dirty.get_untracked() {
                    ui.notify("Save or discard the editor's unsaved changes before rewinding.");
                    return;
                }
                let files = project.and_then(|id| Workspace::for_project(api, projects, id));
                if project.is_some() && files.is_none() {
                    ui.notify("Grant access to this project folder before rewinding.");
                    return;
                }
                chat.rewinding.set(true);
                let serial = serial.get_value();
                let token = serial.get().wrapping_add(1);
                serial.set(token);
                let completion = RewindCompletion {busy: chat.rewinding, serial, token};
                let mutations = mutations.clone();
                spawn_local(async move {
                    let work = async move {
                    let _completion = completion;
                    let result = async {
                        let backend = api.with_value(Clone::clone);
                        // Check conflicts before creating the durable project lock.
                        let preview = openwebide_core::RewindPlan::from_conversation(&backend.list_messages(session).await?, message)?;
                        if let Some(files) = &files {
                            openwebide_core::rewind::verify_files(files, &preview, current).await?;
                        }
                        if !current() { return Err("Workspace changed before rewind".into()); }
                        let plan = backend.prepare_rewind(session, message).await?;
                        if !current() { return Err("Return to this session to resume the rewind".into()); }
                        if let Some(files) = &files {
                            openwebide_core::rewind::restore_files(files, &plan, current).await?;
                        } else if !plan.files.is_empty() {
                            return Err("This checkpoint needs access to its project files".into());
                        }
                        if !current() { return Err("Return to this session to resume the rewind".into()); }
                        let entries = backend.complete_rewind(session, message).await?;
                        if current() {
                            chat.history_gen.update_value(|value| *value += 1);
                            if let Some(todos) = todos { todos.refresh.run(session); }
                            chat.session_telemetry.update(|telemetry| telemetry.restore_from_conversation(&entries));
                            chat.messages.install_history(super::chat::history_items(entries));
                            chat.interrupted_run.set(None);
                            chat.active_run.set(None);
                            chat.active_editor_context.set(None);
                            let prompt = openwebide_core::PromptContent::decode(&plan.prompt);
                            chat.prompt_images.set(prompt.images);
                            chat.draft.set(openwebide_core::tui::extract_editor_context_prelude(&prompt.text).1.to_string());
                            if let Some(id) = project {
                                super::workspace::refresh_pending(api, projects, workspace, ui, auth, id).await;
                                if current() {
                                    refresh_git.run(());
                                    if let Some(path) = workspace.open_file.get_untracked() {
                                        if plan.files.iter().any(|file| file.path == path && file.before.is_none() && file.binary_before.is_none() && file.backup_path.is_none()) {
                                            workspace.open_file.set(None);
                                            workspace.content.set(String::new().into());
                                        } else {
                                            request_open.run(path);
                                        }
                                    }
                                }
                            }
                        }
                        Ok::<_, String>(())
                    }.await;
                    if current() {
                        match result {
                            Ok(()) => ui.notify("Checkpoint restored."),
                            Err(error) => ui.notify(format!("Rewind paused: {error}. Retry the same checkpoint to resume.")),
                        }
                    }
                    };
                    let guarded = match mutations.take() {
                        Some(mutations) => mutations.run(async move {work.await; Ok(())}).await,
                        None => {work.await; Ok(())},
                    };
                    if let Err(error) = guarded && current() {
                        ui.notify(format!("Rewind paused: {error}"));
                    }
                });
            }),
        });
        });
    })
}
