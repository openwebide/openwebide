use crate::state::{
    auth::AuthState,
    projects::ProjectsState,
    ui::{ConfirmRequest, UiState},
    workspace::WorkspaceState,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use openwebide_core::{
    EditDecision, FileKind, PersistedEdit, ResolveEditRequest, vfs::SearchOptions,
};

use crate::{backend::Api, local_fs, workspace::Workspace};

#[derive(Clone, PartialEq)]
struct EditorRoot {
    identity: (i64, openwebide_core::WorkspaceMode, Option<String>),
    handle: Option<web_sys::FileSystemDirectoryHandle>,
    bridge: Option<String>,
}

impl EditorRoot {
    fn changed(&self, next: &Self) -> bool {
        self.identity != next.identity
            || self.bridge != next.bridge
            || self
                .handle
                .as_ref()
                .is_some_and(|handle| next.handle.as_ref() != Some(handle))
    }

    fn for_project(
        project: &openwebide_core::Project,
        projects: ProjectsState,
        settings: Option<crate::state::settings::SettingsState>,
    ) -> Self {
        Self {
            identity: Workspace::root_identity(project),
            handle: projects
                .local_handles
                .with_untracked(|handles| handles.get(&project.id).cloned()),
            bridge: settings.map(|settings| settings.bridge_url.get_untracked()),
        }
    }
}

/// Invalidates in-flight work when the account, project, folder or bridge changes.
pub(crate) fn project_epoch(projects: ProjectsState, auth: AuthState) -> Memo<u64> {
    let settings = use_context::<crate::state::settings::SettingsState>();
    let root = Memo::new(move |_| {
        let active = projects.active_project.get();
        projects.projects.with(|items| {
            items
                .iter()
                .find(|project| Some(project.id) == active)
                .map(crate::workspace::Workspace::root_identity)
        })
    });
    let generation = StoredValue::new(0_u64);
    Memo::new(move |_| {
        root.with(|_| ());
        projects.local_handles.track();
        auth.generation.track();
        if let Some(settings) = settings {
            settings.bridge_url.track();
        }
        generation.update_value(|value| *value += 1);
        generation.get_value()
    })
}

pub async fn refresh_pending(
    api: Api,
    projects: ProjectsState,
    workspace: WorkspaceState,
    ui: UiState,
    auth: AuthState,
    project_id: i64,
) {
    let auth_generation = auth.generation.get_untracked();
    let token = workspace.begin_pending_refresh(project_id);
    let result = api
        .with_value(Clone::clone)
        .list_pending_edits(project_id)
        .await;
    if auth.generation.try_get_untracked() != Some(auth_generation)
        || !workspace.pending_refresh_current(project_id, token)
        || projects
            .projects
            .try_with_untracked(|projects| projects.iter().any(|p| p.id == project_id))
            != Some(true)
    {
        return;
    }
    match result {
        Ok(edits) => workspace.set_persisted_edits(project_id, edits),
        Err(error) => ui.notify(error),
    }
}

pub fn pending_refresh(
    api: Api,
    projects: ProjectsState,
    workspace: WorkspaceState,
    ui: UiState,
) -> Callback<i64> {
    let auth = expect_context::<AuthState>();
    Callback::new(move |project_id| {
        let generation = auth.generation.get_untracked();
        spawn_local(async move {
            if auth.generation.try_get_untracked() == Some(generation) {
                refresh_pending(api, projects, workspace, ui, auth, project_id).await;
            }
        });
    })
}

async fn save_resolution(
    api: Api,
    project_id: i64,
    edit: &PersistedEdit,
    decision: EditDecision,
    current: impl Fn() -> bool,
) -> Result<PersistedEdit, String> {
    let request = ResolveEditRequest {
        path: edit.path.clone(),
        revision: edit.revision,
        decision,
    };
    let backend = api.with_value(Clone::clone);
    match backend.resolve_pending_edit(project_id, &request).await {
        Ok(edit) => Ok(edit),
        Err(error) => {
            if !current() {
                return Err(error);
            }
            backend.resolve_pending_edit(project_id, &request).await
        }
    }
}

/// Browser and backend actions for opening, editing, and searching files.
#[derive(Clone, Copy)]
pub struct WorkspaceActions {
    pub workspace_for: Callback<i64, Option<Workspace>>,
    pub on_grant_access: Callback<()>,
    pub ensure_root: Callback<i64>,
    pub refresh_tree: Callback<()>,
    pub on_open_lossy: Callback<()>,
    pub request_open: Callback<String>,
    pub open_with_completion: Callback<(String, Callback<Result<(), String>>)>,
    pub close_file: Callback<String>,
    pub file_tab_action: Callback<(String, crate::tabs::TabAction)>,
    pub on_toggle: Callback<String>,
    pub on_save: Callback<()>,
    pub on_accept: Callback<()>,
    pub on_reject: Callback<()>,
    pub on_new_file: Callback<()>,
    pub on_new_dir: Callback<()>,
    pub on_search_input: Callback<(String, SearchOptions)>,
    pub on_cancel_search: Callback<()>,
    pub on_search: Callback<(String, SearchOptions)>,
    pub on_clear_search: Callback<()>,
}

impl WorkspaceActions {
    pub fn new(
        api: Api,
        projects: ProjectsState,
        workspace: WorkspaceState,
        ui: UiState,
        read_only: RwSignal<bool>,
        refresh_git: Callback<()>,
    ) -> Self {
        let active_project = projects.active_project;
        Effect::new(move |_| {
            if let Some((project, path)) = active_project.get().zip(workspace.open_file.get()) {
                workspace.register_editor_tab(project, path);
            }
        });
        let auth = expect_context::<AuthState>();
        let workspace_for = Callback::new(move |project_id: i64| -> Option<Workspace> {
            Workspace::for_project(api, projects, project_id)
        });

        super::tree::watch_tree(projects, workspace, auth, workspace_for, refresh_git);
        let refresh_epoch = project_epoch(projects, auth);
        let refresh_tree = Callback::new(move |()| {
            let Some(project_id) = active_project.get_untracked() else {
                return;
            };
            let Some(ws) = workspace_for.run(project_id) else {
                ui.notify("Grant folder access in Files before refreshing");
                return;
            };
            let token = refresh_epoch.get_untracked();
            let current = move || refresh_epoch.try_get_untracked() == Some(token);
            refresh_git.run(());
            spawn_local(async move {
                let result =
                    super::tree::refresh_tree(ws, projects, workspace, project_id, current).await;
                if current()
                    && let Err(error) = result
                {
                    ui.notify(format!("Could not refresh files: {error}"));
                }
            });
        });

        let tooling_generation = StoredValue::new(0_u64);
        Effect::new(move |_| {
            let project_id = active_project.get();
            let entries = workspace.entries.with(|entries| entries.get("").cloned());
            let epoch = auth.generation.get();
            projects.local_handles.track();
            tooling_generation.update_value(|value| *value += 1);
            let generation = tooling_generation.get_value();
            if let (Some(id), Some(entries), Some(ws)) = (
                project_id,
                entries,
                project_id.and_then(|id| workspace_for.run(id)),
            ) {
                spawn_local(async move {
                    let tooling = crate::project_setup::from_entries(&ws, &entries)
                        .await
                        .ok()
                        .flatten();
                    if tooling_generation.try_get_value() != Some(generation)
                        || auth.generation.try_get_untracked() != Some(epoch)
                        || active_project.try_get_untracked() != Some(Some(id))
                    {
                        return;
                    }
                    projects.tooling.update(|tools| {
                        if let Some(tooling) = tooling {
                            tools.insert(id, tooling);
                        } else {
                            tools.remove(&id);
                        }
                    });
                });
            }
        });

        let directory_epoch = project_epoch(projects, auth);
        let settings = use_context::<crate::state::settings::SettingsState>();
        let rule_generation = StoredValue::new(0_u64);
        Effect::new(move |_| {
            let key = active_project.get().zip(workspace.open_file.get());
            let epoch = directory_epoch.get();
            workspace.editor_configuration_revision.track();
            let defaults = settings
                .map(|settings| settings.editor_preferences.get())
                .unwrap_or_default();
            rule_generation.update_value(|generation| *generation += 1);
            let generation = rule_generation.get_value();
            let Some((project_id, path)) = key else {
                return;
            };
            let Some(ws) = workspace_for.run(project_id) else {
                return;
            };
            let text = workspace.content.get_untracked();
            spawn_local(async move {
                let current = || {
                    rule_generation.try_get_value() == Some(generation)
                        && directory_epoch.try_get_untracked() == Some(epoch)
                        && active_project.try_get_untracked() == Some(Some(project_id))
                        && workspace.open_file.try_get_untracked().flatten().as_deref()
                            == Some(path.as_str())
                };
                if let Ok((rules, warnings)) =
                    openwebide_core::editor::load_rules(&ws, &path, &text, defaults, current).await
                {
                    if !current() {
                        return;
                    }
                    workspace.editor_rules.update(|values| {
                        values.insert((project_id, path), rules);
                    });
                    if !warnings.is_empty() {
                        ui.notify(warnings.join("\n"));
                    }
                }
            });
        });

        let load_dir = Callback::new(move |(project_id, dir): (i64, String)| {
            let generation = auth.generation.get_untracked();
            let epoch = directory_epoch.get_untracked();
            spawn_local(async move {
                let current = || {
                    auth.generation.try_get_untracked() == Some(generation)
                        && directory_epoch.try_get_untracked() == Some(epoch)
                        && active_project.try_get_untracked() == Some(Some(project_id))
                };
                if !current() {
                    return;
                }
                let Some(ws) = workspace_for.run(project_id) else {
                    if current() {
                        projects.needs_grant.update(|ids| {
                            ids.insert(project_id);
                        });
                    }
                    return;
                };
                match ws.list(&dir).await {
                    Ok(entries) => {
                        if current() {
                            workspace.entries.update(|entries_by_dir| {
                                entries_by_dir.insert(dir, entries);
                            });
                        }
                    }
                    Err(error) => {
                        if !current() {
                            return;
                        }
                        if error.needs_folder_access() {
                            projects.needs_grant.update(|projects_needing_grant| {
                                projects_needing_grant.insert(project_id);
                            });
                        } else if current() {
                            ui.toast.set(Some(error.to_string()));
                        }
                    }
                }
            });
        });

        let on_grant_access = Callback::new(move |()| {
            let Some(project_id) = active_project.get() else {
                return;
            };
            let handle = projects
                .local_handles
                .with(|handles| handles.get(&project_id).cloned());
            let Some(project) = projects.project(project_id) else {
                return;
            };
            let generation = auth.generation.get_untracked();
            spawn_local(async move {
                let current = || {
                    auth.generation.try_get_untracked() == Some(generation)
                        && projects
                            .projects
                            .try_with_untracked(|items| items.iter().any(|p| p.id == project_id))
                            == Some(true)
                };
                let result = async {
                    let restored = match handle.as_ref() {
                        Some(handle) if local_fs::request_access(handle).await.unwrap_or(false) => {
                            handle.clone()
                        }
                        previous => {
                            let Some(picked) = local_fs::pick_directory_from(previous).await?
                            else {
                                return Ok::<_, String>(());
                            };
                            let matches = match previous {
                                Some(previous) => {
                                    wasm_bindgen_futures::JsFuture::from(
                                        previous.is_same_entry(&picked),
                                    )
                                    .await
                                    .map_err(|error| local_fs::js_error(&error))?
                                    .as_bool()
                                        == Some(true)
                                }
                                None => {
                                    picked.name()
                                        == project.path.as_deref().unwrap_or(&project.name)
                                }
                            };
                            if !matches {
                                return Err(format!(
                                    "Choose the original folder ({}) to reconnect this project.",
                                    project.name
                                ));
                            }
                            picked
                        }
                    };
                    if !current() {
                        return Ok(());
                    }
                    crate::idb::save_handle(project_id, project.user_id, &restored).await?;
                    if !current() {
                        return Ok(());
                    }
                    projects.local_handles.update(|handles| {
                        handles.insert(project_id, restored);
                    });
                    projects.needs_grant.update(|ids| {
                        ids.remove(&project_id);
                    });
                    load_dir.run((project_id, String::new()));
                    refresh_git.run(());
                    Ok(())
                }
                .await;
                if let Err(error) = result
                    && current()
                {
                    ui.notify(error);
                }
            });
        });

        let ensure_root = Callback::new(move |project_id: i64| {
            if workspace.entries.get().contains_key("") {
                return;
            }
            load_dir.run((project_id, String::new()));
        });

        let save_editor = super::editor::EditorActions::new(workspace);
        let on_open_lossy = Callback::new(move |()| {
            let Some(project_id) = active_project.get_untracked() else {
                return;
            };
            let Some(path) = workspace.open_file.get_untracked() else {
                return;
            };
            let baseline = workspace.content.get_untracked();
            let revision = workspace.begin_editor_read();
            let generation = auth.generation.get_untracked();
            let epoch = directory_epoch.get_untracked();
            read_only.set(true);
            workspace.dirty.set(false);
            workspace.editor_loading.set(true);
            spawn_local(async move {
                let current = || {
                    auth.generation.try_get_untracked() == Some(generation)
                        && directory_epoch.try_get_untracked() == Some(epoch)
                        && workspace.editor_read_current(revision, project_id, &path)
                };
                let Some(ws) = workspace_for.run(project_id) else {
                    if current() {
                        workspace.editor_loading.set(false);
                    }
                    return;
                };
                let result = ws.read_lossy(&path).await;
                if !current() {
                    return;
                }
                match result {
                    Ok(content) => {
                        save_editor.publish_read(content, &baseline, current).await;
                        if !current() {
                            return;
                        }
                        workspace.editor_loading.set(false);
                        workspace.retain_editor_buffer(true);
                    }
                    Err(error) => {
                        workspace.editor_loading.set(false);
                        ui.toast.set(Some(error.to_string()));
                    }
                }
            });
        });

        let editor_roots =
            StoredValue::new(send_wrapper::SendWrapper::new(std::collections::HashMap::<
                i64,
                EditorRoot,
            >::new()));
        let open_request = Callback::new(
            move |(path, completion): (String, Option<Callback<Result<(), String>>>)| {
                let Some(project_id) = active_project.get_untracked() else {
                    if let Some(done) = completion {
                        let _ = done.try_run(Err("Open a project first.".into()));
                    }
                    return;
                };
                let Some(project) = projects.project(project_id) else {
                    if let Some(done) = completion {
                        let _ = done.try_run(Err("Project unavailable.".into()));
                    }
                    return;
                };
                let root = EditorRoot::for_project(&project, projects, settings);
                let changed = editor_roots.with_value(|roots| {
                    roots
                        .get(&project_id)
                        .is_some_and(|previous| previous.changed(&root))
                });
                if changed
                    && (workspace.dirty.get_untracked()
                        || workspace.editor_buffers.with_untracked(|buffers| {
                            buffers
                                .iter()
                                .any(|((id, _), buffer)| *id == project_id && buffer.dirty)
                        }))
                {
                    if let Some(done) = completion {
                        let _ = done.try_run(Err(
                            "Restore the original project folder before opening this file.".into(),
                        ));
                    }
                    ui.notify("The project folder changed while files have unsaved edits. Restore the original folder to save them before opening files from the new folder.");
                    return;
                }
                editor_roots.update_value(|roots| {
                    roots.insert(project_id, root);
                });
                save_editor.cancel_composition();
                if changed {
                    workspace.editor_recovery_overwrites.update(|permits| {
                        permits.retain(|(id, _), _| *id != project_id);
                    });
                    workspace.editor_recovery_checks.update(|checks| {
                        checks.retain(|(id, _), _| *id != project_id);
                    });
                    workspace.editor_recovered.update(|files| {
                        files.retain(|(id, _)| *id != project_id);
                    });
                    workspace.editor_tabs.update(|tabs| {
                        tabs.remove(&project_id);
                    });
                    workspace
                        .editor_buffers
                        .update(|buffers| buffers.retain(|(id, _), _| *id != project_id));
                    workspace
                        .editor_documents
                        .update(|documents| documents.retain(|(id, _), _| *id != project_id));
                    workspace
                        .editor_scroll
                        .update(|positions| positions.retain(|(id, _), _| *id != project_id));
                    workspace.open_file.set(None);
                    workspace.content.set(String::new().into());
                    workspace.dirty.set(false);
                }
                workspace.retain_editor_buffer(read_only.get_untracked());
                let cached = workspace
                    .editor_buffers
                    .with_untracked(|buffers| buffers.get(&(project_id, path.clone())).cloned());
                let revision = workspace.begin_editor_read();
                let generation = auth.generation.get_untracked();
                let epoch = directory_epoch.get_untracked();
                let kind = FileKind::from_path(&path);
                let retained = cached
                    .as_ref()
                    .is_some_and(|buffer| buffer.dirty || buffer.read_only)
                    || workspace
                        .editor_recovery_checks
                        .with_untracked(|checks| checks.contains_key(&(project_id, path.clone())));
                let baseline = cached
                    .as_ref()
                    .map(|buffer| buffer.content.clone())
                    .unwrap_or_default();
                // Publish the restored text and identity together so history is never
                // reconciled against another file's text or a temporary empty buffer.
                batch(|| {
                    workspace.open_file.set(Some(path.clone()));
                    workspace
                        .dirty
                        .set(cached.as_ref().is_some_and(|buffer| buffer.dirty));
                    workspace.content.set(
                        cached
                            .as_ref()
                            .map(|buffer| buffer.content.clone())
                            .unwrap_or_default(),
                    );
                    read_only.set(cached.as_ref().is_some_and(|buffer| buffer.read_only));
                    workspace
                        .editor_loading
                        .set(!retained && !kind.is_non_text());
                    revoke_object_url(workspace.media_url.get_untracked());
                    workspace.media_url.set(None);
                });
                ui.toast.set(None);
                spawn_local(async move {
                    let current = || {
                        auth.generation.try_get_untracked() == Some(generation)
                            && directory_epoch.try_get_untracked() == Some(epoch)
                            && workspace.editor_read_current(revision, project_id, &path)
                    };
                    if !current() {
                        return;
                    }
                    let Some(ws) = workspace_for.run(project_id) else {
                        workspace.editor_loading.set(false);
                        if let Some(done) = completion {
                            let _ = done.try_run(Err("Reconnect the project folder first.".into()));
                        }
                        return;
                    };
                    if kind.is_non_text() && FileKind::supports_preview(&path) {
                        match ws.read_blob_url(&path).await {
                            Ok(url) => {
                                if current() {
                                    workspace.media_url.set(Some(url));
                                } else {
                                    revoke_object_url(Some(url));
                                }
                            }
                            Err(error) => {
                                if current()
                                    && let Some(done) = completion
                                {
                                    let _ = done.try_run(Err(error.to_string()));
                                    return;
                                }
                            }
                        }
                    }
                    if kind.is_non_text() || retained {
                        if let Some(done) = completion {
                            let result = ws
                                .canonical_path(&path)
                                .await
                                .map(|_| ())
                                .map_err(|error| error.to_string());
                            if current() {
                                let _ = done.try_run(result);
                            }
                        }
                        return;
                    }
                    let result = ws.read(&path).await;
                    if !current() {
                        return;
                    }
                    // Facade callers may edit without a DOM. Such input still wins
                    // over a read that began against an empty loading buffer.
                    let completion_result =
                        result.as_ref().map(|_| ()).map_err(ToString::to_string);
                    let loaded = result.is_ok() || workspace.dirty.get_untracked();
                    if !workspace.dirty.get_untracked()
                        && workspace.content.get_untracked() == baseline
                    {
                        match result {
                            Ok(content) => {
                                save_editor.publish_read(content, &baseline, current).await;
                                if !current() {
                                    return;
                                }
                            }
                            Err(error) => ui.toast.set(Some(error.to_string())),
                        }
                    }
                    workspace.editor_loading.set(false);
                    if loaded {
                        workspace.retain_editor_buffer(read_only.get_untracked());
                    }
                    if let Some(done) = completion {
                        let _ = done.try_run(completion_result);
                    }
                });
            },
        );
        let on_open = Callback::new(move |path| open_request.run((path, None)));
        let open_with_completion = Callback::new(
            move |(path, done): (String, Callback<Result<(), String>>)| {
                let project = active_project.get_untracked();
                let generation = auth.generation.get_untracked();
                let epoch = directory_epoch.get_untracked();
                let session = workspace.active_session.get_untracked();
                let selection = workspace.editor_read_revision.get_untracked();
                let selected_file = workspace.open_file.get_untracked();
                spawn_local(async move {
                    let result = async {
                        let ws = workspace_for
                            .run(project.ok_or("Open a project first.")?)
                            .ok_or("Reconnect the project folder first.")?;
                        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
                        if !ws
                            .list(parent)
                            .await
                            .map_err(|error| error.to_string())?
                            .iter()
                            .any(|entry| entry.name == name && !entry.is_dir)
                        {
                            return Err("The current working file no longer exists.".to_owned());
                        }
                        Ok(())
                    }
                    .await;
                    if workspace.editor_read_revision.try_get_untracked() != Some(selection)
                        || workspace.open_file.try_get_untracked() != Some(selected_file)
                        || workspace.active_session.try_get_untracked() != Some(session)
                        || active_project.try_get_untracked() != Some(project)
                        || auth.generation.try_get_untracked() != Some(generation)
                        || directory_epoch.try_get_untracked() != Some(epoch)
                    {
                        return;
                    }
                    match result {
                        Ok(()) => open_request.run((path, Some(done))),
                        Err(error) => {
                            let _ = done.try_run(Err(error));
                        }
                    }
                });
            },
        );
        let request_open = on_open;

        let close_files = Callback::new(move |paths: Vec<String>| {
            if paths.is_empty() {
                return;
            }
            let Some(project) = active_project.get_untracked() else {
                return;
            };
            if workspace.is_resolving() {
                ui.notify("Wait for the current editor operation to finish");
                return;
            }
            let generation = auth.generation.get_untracked();
            let epoch = directory_epoch.get_untracked();
            let buffer = |path: &String| {
                if workspace.open_file.get_untracked().as_deref() == Some(path) {
                    Some((
                        workspace.content.get_untracked(),
                        workspace.dirty.get_untracked(),
                    ))
                } else {
                    workspace.editor_buffers.with_untracked(|buffers| {
                        buffers
                            .get(&(project, path.clone()))
                            .map(|buffer| (buffer.content.clone(), buffer.dirty))
                    })
                }
            };
            let captured: Vec<_> = paths
                .iter()
                .map(|path| (path.clone(), buffer(path)))
                .collect();
            let dirty: Vec<_> = captured
                .iter()
                .filter(|(_, buffer)| buffer.as_ref().is_some_and(|(_, dirty)| *dirty))
                .map(|(path, _)| path.clone())
                .collect();
            let count = paths.len();
            let close = Callback::new(move |()| {
                if auth.generation.try_get_untracked() != Some(generation)
                    || directory_epoch.try_get_untracked() != Some(epoch)
                    || active_project.try_get_untracked() != Some(Some(project))
                    || workspace.is_resolving()
                {
                    return;
                }
                let latest: Vec<_> = captured
                    .iter()
                    .map(|(path, _)| {
                        let buffer = if workspace.open_file.get_untracked().as_ref() == Some(path) {
                            Some((
                                workspace.content.get_untracked(),
                                workspace.dirty.get_untracked(),
                            ))
                        } else {
                            workspace.editor_buffers.with_untracked(|buffers| {
                                buffers
                                    .get(&(project, path.clone()))
                                    .map(|buffer| (buffer.content.clone(), buffer.dirty))
                            })
                        };
                        (path.clone(), buffer)
                    })
                    .collect();
                if latest != captured {
                    ui.notify("The file changed while the close dialog was open. Close it again to review your latest edits.");
                    return;
                }
                let active = workspace
                    .open_file
                    .get_untracked()
                    .is_some_and(|path| paths.contains(&path));
                if active {
                    save_editor.cancel_composition();
                }
                let next = batch(|| {
                    let mut next = None;
                    for path in &paths {
                        let candidate = workspace.remove_editor_tab(project, path);
                        if workspace.open_file.get_untracked().as_ref() == Some(path) {
                            next = candidate;
                        }
                    }
                    let next = workspace.editor_tabs.with_untracked(|tabs| {
                        let remaining = tabs.get(&project)?;
                        next.filter(|path| remaining.contains(path))
                            .or_else(|| remaining.first().cloned())
                    });
                    if active {
                        workspace.begin_editor_read();
                        workspace.editor_loading.set(false);
                        revoke_object_url(workspace.media_url.get_untracked());
                        workspace.media_url.set(None);
                        workspace.open_file.set(None);
                        workspace.content.set(String::new().into());
                        workspace.dirty.set(false);
                        read_only.set(false);
                    }
                    next
                });
                if active && let Some(next) = next {
                    request_open.run(next);
                }
            });
            if !dirty.is_empty() {
                ui.confirm.set(Some(ConfirmRequest {
                    title: if count == 1 {
                        "Close unsaved file"
                    } else {
                        "Close unsaved files"
                    }
                    .into(),
                    message: format!(
                        "Discard unsaved changes to {} and close the selected files?",
                        dirty
                            .iter()
                            .map(|path| format!("`{path}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    confirm_label: "Discard and close".into(),
                    action: close,
                }));
            } else {
                close.run(());
            }
        });

        let close_file = Callback::new(move |path: String| close_files.run(vec![path]));
        let file_tab_action =
            Callback::new(move |(path, action): (String, crate::tabs::TabAction)| {
                let Some(project) = active_project.get_untracked() else {
                    return;
                };
                let paths = workspace
                    .editor_tabs
                    .with_untracked(|tabs| tabs.get(&project).cloned().unwrap_or_default());
                if matches!(
                    action,
                    crate::tabs::TabAction::MoveLeft | crate::tabs::TabAction::MoveRight
                ) {
                    workspace.editor_tabs.update(|tabs| {
                        if let Some(paths) = tabs.get_mut(&project) {
                            action.reorder(paths, &path);
                        }
                    });
                } else {
                    close_files.run(action.targets(&paths, &path));
                }
            });

        let on_toggle = Callback::new(move |dir: String| {
            let mut expanded = workspace.expanded.get();
            let opening = !expanded.contains(&dir);
            if opening {
                expanded.insert(dir.clone());
            } else {
                expanded.remove(&dir);
            }
            workspace.expanded.set(expanded);
            if opening
                && let Some(project_id) = active_project.get()
                && !workspace.entries.get().contains_key(&dir)
            {
                load_dir.run((project_id, dir));
            }
        });

        let on_save = Callback::new(move |()| {
            let Some(project_id) = active_project.get_untracked() else {
                return;
            };
            let Some(path) = workspace.open_file.get_untracked() else {
                return;
            };
            let overwrite = workspace
                .editor_recovery_overwrites
                .with_untracked(|permits| permits.contains_key(&(project_id, path.clone())));
            if (!workspace.dirty.get_untracked() && !overwrite)
                || read_only.get_untracked()
                || workspace.is_resolving()
            {
                if overwrite {
                    workspace.editor_recovery_overwrites.update(|permits| {
                        permits.remove(&(project_id, path.clone()));
                    });
                    workspace.editor_recovery_checks.update(|checks| {
                        checks.insert(
                            (project_id, path.clone()),
                            crate::state::workspace::RecoveredFileIssue::Pending,
                        );
                    });
                }
                return;
            }
            if let Some(issue) = workspace
                .editor_recovery_checks
                .with_untracked(|checks| checks.get(&(project_id, path.clone())).cloned())
            {
                ui.notify(issue.message());
                return;
            }
            let Some(project) = projects.project(project_id) else {
                return;
            };
            let root = EditorRoot::for_project(&project, projects, settings);
            if editor_roots.with_value(|roots| {
                roots
                    .get(&project_id)
                    .is_some_and(|origin| origin.changed(&root))
            }) {
                workspace.editor_recovery_overwrites.update(|permits| {
                    permits.remove(&(project_id, path.clone()));
                });
                workspace.editor_recovery_checks.update(|checks| {
                    checks.insert(
                        (project_id, path.clone()),
                        crate::state::workspace::RecoveredFileIssue::Pending,
                    );
                });
                ui.notify("The project folder changed while this file was open. Restore the original folder before saving its draft.");
                return;
            }
            let content_before = workspace.content.get_untracked();
            let account = auth.generation.get_untracked();
            let epoch = directory_epoch.get_untracked();
            let defaults = settings
                .map(|settings| settings.editor_preferences.get_untracked())
                .unwrap_or_default();
            ui.clear_toast();
            spawn_local(async move {
                let current = || {
                    auth.generation.try_get_untracked() == Some(account)
                        && directory_epoch.try_get_untracked() == Some(epoch)
                        && save_editor.is_current(project_id, &path)
                };
                let Some(ws) = workspace_for.run(project_id) else {
                    return;
                };
                let Ok((rules, warnings)) = openwebide_core::editor::load_rules(
                    &ws,
                    &path,
                    &content_before,
                    defaults,
                    current,
                )
                .await
                else {
                    return;
                };
                if !current() {
                    return;
                }
                if workspace.content.get_untracked() != content_before {
                    ui.notify("The file changed while preparing to save. Save again to include your latest edits.");
                    return;
                }
                if let Err(error) = super::editor_recovery::verify_recovered_save(
                    workspace, projects, &ws, project_id, &path, current,
                )
                .await
                {
                    if current() {
                        ui.notify(error);
                    }
                    return;
                }
                if !current() || workspace.content.get_untracked() != content_before {
                    return;
                }
                let content = match save_editor.prepare_save(&rules) {
                    Ok(Some(content)) => content,
                    Ok(None) => return,
                    Err(error) => {
                        ui.notify(error.to_string());
                        return;
                    }
                };
                match ws.write(&path, &content).await {
                    Ok(()) => {
                        // A background save may finish after changing tabs, but it must
                        // still acknowledge the same account and workspace root.
                        if auth.generation.try_get_untracked() != Some(account)
                            || projects.project(project_id).is_none_or(|project| {
                                root.changed(&EditorRoot::for_project(&project, projects, settings))
                            })
                        {
                            return;
                        }
                        workspace.editor_documents.update(|documents| {
                            if let Some(document) = documents.get_mut(&(project_id, path.clone())) {
                                document.mark_saved_source(content.shared());
                            }
                        });
                        workspace.editor_buffers.update(|buffers| {
                            if let Some(buffer) = buffers.get_mut(&(project_id, path.clone())) {
                                buffer.dirty = buffer.content != content;
                            }
                        });
                        if save_editor.is_current(project_id, &path) {
                            workspace
                                .dirty
                                .set(workspace.content.get_untracked() != content);
                        }
                        workspace.snapshots.update(|snapshots| {
                            if let Some(snapshot) = snapshots.get_mut(&project_id)
                                && snapshot.open_file.as_deref() == Some(path.as_str())
                            {
                                snapshot.dirty = snapshot.content != content;
                            }
                        });
                        if path.rsplit('/').next() == Some(".editorconfig") {
                            workspace
                                .editor_configuration_revision
                                .update(|value| *value += 1);
                        }
                        refresh_git.run(());
                        if current() && !warnings.is_empty() {
                            ui.notify(warnings.join("\n"));
                        }
                    }
                    Err(error) => {
                        if current() {
                            if overwrite {
                                workspace.editor_recovery_checks.update(|checks| {
                                    checks.insert(
                                        (project_id, path.clone()),
                                        crate::state::workspace::RecoveredFileIssue::Unavailable(
                                            error.to_string(),
                                        ),
                                    );
                                });
                            }
                            ui.notify(error.to_string());
                        }
                    }
                }
            });
        });

        let review_actions = super::reviews::ReviewActions::new(
            api,
            projects,
            workspace,
            ui,
            request_open,
            refresh_git,
        );
        provide_context(review_actions);
        let resolve = Callback::new(move |(edit, decision): (PersistedEdit, EditDecision)| {
            let project_id = edit.project_id;
            let path = edit.path.clone();
            let key = (project_id, path.clone());
            if workspace
                .resolving_edits
                .with_untracked(|edits| edits.contains(&key))
            {
                return;
            }
            workspace.resolving_edits.update(|edits| {
                edits.insert(key.clone());
            });
            let agent_writes_before = workspace
                .agent_writes
                .with_untracked(|writes| writes.get(&key).copied().unwrap_or_default());
            let editor_before = if active_project.get_untracked() == Some(project_id) {
                (
                    workspace.open_file.get_untracked(),
                    workspace.content.get_untracked(),
                    workspace.dirty.get_untracked(),
                )
            } else {
                workspace.snapshots.with_untracked(|snapshots| {
                    snapshots
                        .get(&project_id)
                        .map(|snapshot| {
                            (
                                snapshot.open_file.clone(),
                                snapshot.content.clone(),
                                snapshot.dirty,
                            )
                        })
                        .unwrap_or_default()
                })
            };
            let auth_generation = auth.generation.get_untracked();
            let epoch = workspace.pending_epoch.get_untracked();
            ui.clear_toast();
            spawn_local(async move {
                let current = || {
                    auth.generation.try_get_untracked() == Some(auth_generation)
                        && workspace.pending_epoch.try_get_untracked() == Some(epoch)
                        && projects.projects.try_with_untracked(|projects| {
                            projects.iter().any(|p| p.id == project_id)
                        }) == Some(true)
                };
                if !current() {
                    return;
                }
                let action = crate::pending::reject_action(&edit.diff);
                let mut restored_content = None;
                let result = async {
                    if decision == EditDecision::Rejected {
                        let ws = workspace_for.run(project_id).ok_or_else(|| {
                            "Reconnect this folder using Grant folder access in the file tree."
                                .to_string()
                        })?;
                        // Check the revision before mutating files as well as when saving the decision.
                        let edits = api
                            .with_value(Clone::clone)
                            .list_pending_edits(project_id)
                            .await?;
                        if !current() {
                            return Err("Review was interrupted.".into());
                        }
                        if !edits
                            .iter()
                            .any(|record| record.path == path && record.revision == edit.revision)
                        {
                            // A previous save may have committed even if its response was lost.
                            save_resolution(api, project_id, &edit, decision, current).await?;
                            return Ok(());
                        }
                        match &action {
                            crate::pending::RejectAction::Restore(previous) => {
                                ws.write(&path, previous).await?;
                            }
                            crate::pending::RejectAction::RestoreFromBackup(backup) => {
                                ws.copy(backup, &path).await?;
                                restored_content = ws.read_lossy(&path).await.ok();
                            }
                            crate::pending::RejectAction::Delete => {
                                // A retry after a successful delete may find the file already absent.
                                let parent = parent_dir(&path);
                                let name = path.rsplit('/').next().unwrap_or(&path);
                                let entries = ws.list(&parent).await?;
                                if !current() {
                                    return Err("Review was interrupted.".into());
                                }
                                if entries.iter().any(|entry| entry.name == name) {
                                    ws.delete(&path).await?;
                                }
                            }
                            crate::pending::RejectAction::Unavailable => {
                                return Err("Original contents are unavailable.".into());
                            }
                        }
                    }
                    if !current() {
                        return Err("Review was interrupted.".into());
                    }
                    save_resolution(api, project_id, &edit, decision, current).await?;
                    Ok::<_, String>(())
                }
                .await;
                if !current() {
                    return;
                }
                if result.is_ok() {
                    // Finish the editor transition before clearing review exposes an editable buffer.
                    let transition = |snapshot: &mut crate::state::workspace::WorkspaceSnapshot| {
                        if snapshot.open_file.as_deref() != Some(path.as_str())
                            || snapshot.open_file != editor_before.0
                            || snapshot.content != editor_before.1
                            || snapshot.dirty != editor_before.2
                            || workspace.agent_writes.with_untracked(|writes| {
                                writes.get(&key).copied().unwrap_or_default()
                            }) != agent_writes_before
                            || snapshot
                                .persisted_edits
                                .get(&path)
                                .is_some_and(|record| record.revision != edit.revision)
                        {
                            return false;
                        }
                        if decision == EditDecision::Accepted {
                            snapshot.content = edit.diff.new.clone().into();
                        } else {
                            match &action {
                                crate::pending::RejectAction::Restore(previous) => {
                                    snapshot.content = previous.clone().into();
                                }
                                crate::pending::RejectAction::RestoreFromBackup(_) => {
                                    if let Some(content) = &restored_content {
                                        snapshot.content = content.clone().into();
                                    } else {
                                        snapshot.open_file = None;
                                    }
                                }
                                crate::pending::RejectAction::Delete => snapshot.open_file = None,
                                crate::pending::RejectAction::Unavailable => unreachable!(),
                            }
                        }
                        snapshot.dirty = false;
                        true
                    };
                    if active_project.get_untracked() == Some(project_id) {
                        let mut snapshot = workspace.active_snapshot();
                        if transition(&mut snapshot) {
                            workspace.content.set(snapshot.content);
                            workspace.open_file.set(snapshot.open_file);
                            workspace.dirty.set(snapshot.dirty);
                            load_dir.run((project_id, parent_dir(&path)));
                            refresh_git.run(());
                        }
                    } else {
                        workspace.snapshots.update(|snapshots| {
                            if let Some(snapshot) = snapshots.get_mut(&project_id) {
                                transition(snapshot);
                            }
                        });
                    }
                    workspace.clear_resolved_edit(&edit);
                    // The file transition is committed. Background list refresh must
                    // not keep the editor locked or discard newly typed text.
                    workspace.resolving_edits.update(|edits| {
                        edits.remove(&key);
                    });
                }
                // Invalidate older list responses and verify authoritative state after either outcome.
                refresh_pending(api, projects, workspace, ui, auth, project_id).await;
                if !current() {
                    return;
                }
                if result.is_err() {
                    workspace.resolving_edits.update(|edits| {
                        edits.remove(&key);
                    });
                }
                match result {
                    Ok(()) => {
                        let still_pending = if active_project.get_untracked() == Some(project_id) {
                            workspace
                                .pending_edits
                                .with_untracked(|edits| edits.contains_key(&path))
                        } else {
                            workspace.snapshots.with_untracked(|snapshots| {
                                snapshots.get(&project_id).is_some_and(|snapshot| {
                                    snapshot.pending_edits.contains_key(&path)
                                })
                            })
                        };
                        if still_pending {
                            return;
                        }
                        if let Some(backup) = &edit.diff.backup_path
                            && let Some(ws) = workspace_for.run(project_id)
                        {
                            let _ = ws.delete(backup).await;
                        }
                    }
                    Err(error) => ui.notify(error),
                }
            });
        });

        let on_accept = Callback::new(move |()| {
            let Some(path) = workspace.open_file.get_untracked() else {
                return;
            };
            if let Some(edit) = workspace
                .persisted_edits
                .with_untracked(|edits| edits.get(&path).cloned())
            {
                if edit.file.is_some() {
                    if !review_actions.for_file.run((path, EditDecision::Accepted)) {
                        ui.notify("Review the current file version in the changes panel.");
                    }
                } else {
                    resolve.run((edit, EditDecision::Accepted));
                }
            }
        });

        let on_reject = Callback::new(move |()| {
            let Some(path) = workspace.open_file.get_untracked() else {
                return;
            };
            let Some(edit) = workspace
                .persisted_edits
                .with_untracked(|edits| edits.get(&path).cloned())
            else {
                return;
            };
            if edit.file.is_some() {
                if !review_actions.for_file.run((path, EditDecision::Rejected)) {
                    ui.notify("Review the current file version in the changes panel.");
                }
                return;
            }
            if workspace.is_resolving() {
                return;
            }
            let action = crate::pending::reject_action(&edit.diff);
            ui.confirm.set(Some(ConfirmRequest {
                title: "Reject edit".to_string(),
                message: match &action {
                    crate::pending::RejectAction::Restore(_) => "Reject this edit? The file will be restored to its previous contents.".to_string(),
                    crate::pending::RejectAction::RestoreFromBackup(_) => "Reject this edit? The file will be restored from backup.".to_string(),
                    crate::pending::RejectAction::Delete => "Reject this edit? The newly created file will be deleted.".to_string(),
                    crate::pending::RejectAction::Unavailable => "Cannot reject this edit because the file's previous contents were too large to back up and no copy exists.".to_string(),
                },
                confirm_label: if matches!(action, crate::pending::RejectAction::Unavailable) { "Ok" } else { "Reject" }.to_string(),
                action: Callback::new(move |()| {
                    if !matches!(action, crate::pending::RejectAction::Unavailable) {
                        resolve.run((edit.clone(), EditDecision::Rejected));
                    }
                }),
            }));
        });

        let file_tree = super::file_tree::FileTreeActions::new(
            projects,
            workspace,
            ui,
            workspace_for,
            load_dir,
            request_open,
            refresh_git,
        );
        provide_context(file_tree);
        let on_new_file =
            Callback::new(move |()| file_tree.create("", openwebide_core::vfs::VfsEntryKind::File));
        let on_new_dir = Callback::new(move |()| {
            file_tree.create("", openwebide_core::vfs::VfsEntryKind::Directory);
        });

        let search_gen = StoredValue::new(0u64);
        let search_timer = StoredValue::new(None::<leptos::leptos_dom::helpers::TimeoutHandle>);
        let on_cancel_search = Callback::new(move |()| {
            search_gen.try_update_value(|generation| *generation += 1);
            search_timer.try_update_value(|timer| {
                if let Some(timer) = timer.take() {
                    timer.clear();
                }
            });
        });
        on_cleanup(move || on_cancel_search.run(()));
        StoredValue::new_local(RenderEffect::new(move |_| {
            active_project.get();
            on_cancel_search.run(());
        }));

        let dispatch_search = Callback::new(
            move |(project_id, query, options, generation): (i64, String, SearchOptions, u64)| {
                spawn_local(async move {
                    if active_project.try_get_untracked() != Some(Some(project_id))
                        || search_gen.try_get_value() != Some(generation)
                    {
                        return;
                    }
                    let Some(ws) = workspace_for.run(project_id) else {
                        return;
                    };
                    let result = ws.search_content(&query, "", options).await;
                    if active_project.try_get_untracked() != Some(Some(project_id))
                        || search_gen.try_get_value() != Some(generation)
                    {
                        return;
                    }
                    match result {
                        Ok(results) => workspace.search.set(Some(results)),
                        Err(error) => ui.toast.set(Some(error.to_string())),
                    }
                });
            },
        );
        let on_search = Callback::new(move |(query, options): (String, SearchOptions)| {
            on_cancel_search.run(());
            if let Some(project_id) = active_project.get_untracked() {
                dispatch_search.run((project_id, query, options, search_gen.get_value()));
            }
        });
        let on_search_input = Callback::new(move |(query, options): (String, SearchOptions)| {
            // Invalidate in-flight responses before the debounce interval starts.
            on_cancel_search.run(());
            if query.is_empty() {
                workspace.search.set(None);
                return;
            }
            let Some(project_id) = active_project.get_untracked() else {
                return;
            };
            let generation = search_gen.get_value();
            if let Ok(timer) = leptos::leptos_dom::helpers::set_timeout_with_handle(
                move || {
                    search_timer.set_value(None);
                    dispatch_search.run((project_id, query, options, generation));
                },
                std::time::Duration::from_millis(250),
            ) {
                search_timer.set_value(Some(timer));
            }
        });
        let on_clear_search = Callback::new(move |()| {
            on_cancel_search.run(());
            workspace.search.set(None);
        });

        let actions = Self {
            workspace_for,
            on_grant_access,
            ensure_root,
            refresh_tree,
            on_open_lossy,
            request_open,
            open_with_completion,
            close_file,
            file_tab_action,
            on_toggle,
            on_save,
            on_accept,
            on_reject,
            on_new_file,
            on_new_dir,
            on_search_input,
            on_cancel_search,
            on_search,
            on_clear_search,
        };
        provide_context(actions);
        actions
    }
}

fn parent_dir(path: &str) -> String {
    path.rfind('/')
        .map(|index| path[..index].to_string())
        .unwrap_or_default()
}

fn revoke_object_url(url: Option<String>) {
    if let Some(url) = url {
        let _ = web_sys::Url::revoke_object_url(&url);
    }
}
