//! File-tree policy shared by toolbar, context menus and editor actions.
use crate::{
    project_git::ProjectGit,
    state::{
        auth::AuthState,
        chat::ChatState,
        layout::Panel,
        projects::ProjectsState,
        ui::{ConfirmRequest, PromptRequest, UiState},
        workspace::WorkspaceState,
    },
    workspace::Workspace,
};
use leptos::{prelude::*, task::spawn_local};
use openwebide_core::{
    FileEntry,
    git::{GitPathAction, GitPathRequest},
    vfs::VfsEntryKind,
    workspace_entries::{contains_path, entry_path, moved_path, parent},
};
use wasm_bindgen::JsCast;

#[derive(Clone, Debug)]
enum Mutation {
    Create(String, VfsEntryKind),
    Move(String, String),
    Delete(String),
    Ignore(String, bool),
    Git(GitPathRequest),
    Stash(openwebide_core::git::GitStashRequest),
}
impl Mutation {
    fn path(&self) -> &str {
        match self {
            Self::Create(path, _)
            | Self::Move(path, _)
            | Self::Delete(path)
            | Self::Ignore(path, _) => path,
            Self::Git(request) => &request.path,
            Self::Stash(_) => "",
        }
    }
    fn discards(&self) -> bool {
        matches!(
            self,
            Self::Delete(_)
                | Self::Git(GitPathRequest {
                    action: GitPathAction::Revert,
                    ..
                })
        )
    }
}
#[derive(Clone, Copy)]
struct Scope {
    project: i64,
    epoch: u64,
    auth: u64,
}

#[derive(Clone, Copy)]
pub struct FileTreeActions {
    pub busy: RwSignal<bool>,
    pub expanding: RwSignal<bool>,
    expansion_generation: StoredValue<u64>,
    pub reveal_target: RwSignal<Option<String>>,
    pub epoch: Memo<u64>,
    pub send_prompt: RwSignal<Option<Callback<String>>>,
    projects: ProjectsState,
    api: crate::backend::Api,
    settings: Option<crate::state::settings::SettingsState>,
    workspace: WorkspaceState,
    auth: AuthState,
    ui: UiState,
    chat: Option<ChatState>,
    git: Option<ProjectGit>,
    git_state: Option<crate::state::git::GitState>,
    layout: Option<super::layout::LayoutActions>,
    layout_state: Option<crate::state::layout::LayoutState>,
    workspace_for: Callback<i64, Option<Workspace>>,
    load_dir: Callback<(i64, String)>,
    open: Callback<String>,
    refresh_git: Callback<()>,
}
impl FileTreeActions {
    pub fn new(
        projects: ProjectsState,
        workspace: WorkspaceState,
        ui: UiState,
        workspace_for: Callback<i64, Option<Workspace>>,
        load_dir: Callback<(i64, String)>,
        open: Callback<String>,
        refresh_git: Callback<()>,
    ) -> Self {
        let auth = expect_context::<AuthState>();
        let git = use_context::<ProjectGit>();
        let epoch = super::workspace::project_epoch(projects, auth);
        let busy = RwSignal::new(false);
        let expanding = RwSignal::new(false);
        let expansion_generation = StoredValue::new(0_u64);
        let previous = StoredValue::new(epoch.get_untracked());
        Effect::new(move |_| {
            let next = epoch.get();
            if previous.get_value() != next {
                busy.set(false);
                expanding.set(false);
                expansion_generation.update_value(|generation| *generation += 1);
            }
            previous.set_value(next);
        });
        let reveal_target = RwSignal::new(None);
        Effect::new(move |_| {
            epoch.track();
            reveal_target.set(None);
        });
        Self {
            api: expect_context(),
            settings: use_context(),
            reveal_target,
            busy,
            expanding,
            expansion_generation,
            epoch,
            send_prompt: RwSignal::new(None),
            projects,
            workspace,
            auth,
            ui,
            git,
            git_state: use_context(),
            chat: use_context::<ChatState>(),
            layout: use_context::<super::layout::LayoutActions>(),
            layout_state: use_context::<crate::state::layout::LayoutState>(),
            workspace_for,
            load_dir,
            open,
            refresh_git,
        }
    }
    pub fn collapse_all(self) {
        self.expansion_generation
            .update_value(|generation| *generation += 1);
        self.expanding.set(false);
        self.workspace.expanded.set(Default::default());
    }
    pub fn expand_all(self) {
        let Some(scope) = self.scope() else {
            return;
        };
        if self.expanding.get_untracked() {
            return;
        }
        let Some(ws) = self.workspace_for.run(scope.project) else {
            self.ui.toast.set(Some(
                "Restore folder access before expanding the tree".into(),
            ));
            return;
        };
        self.expansion_generation
            .update_value(|generation| *generation += 1);
        let generation = self.expansion_generation.get_value();
        let include_hidden = self
            .layout_state
            .is_some_and(|layout| layout.preferences.get_untracked().include_hidden);
        self.expanding.set(true);
        spawn_local(async move {
            let current = || {
                self.current(scope) && self.expansion_generation.try_get_value() == Some(generation)
            };
            let mut pending = std::collections::VecDeque::from([(String::new(), 0_u32)]);
            let mut visited = std::collections::HashSet::new();
            let mut limited = false;
            while let Some((dir, depth)) = pending.pop_front() {
                if !current() {
                    return;
                }
                if !visited.insert(dir.clone()) {
                    continue;
                }
                if visited.len() > 512 {
                    limited = true;
                    break;
                }
                let listing = match futures::future::select(
                    Box::pin(ws.list(&dir)),
                    Box::pin(crate::util::sleep_ms(25_000)),
                )
                .await
                {
                    futures::future::Either::Left((listing, _)) => listing,
                    futures::future::Either::Right(_) => {
                        if current() {
                            self.expanding.set(false);
                            self.ui
                                .toast
                                .set(Some("Expanding folders timed out; try again".into()));
                        }
                        return;
                    }
                };
                match listing {
                    Ok(entries) => {
                        if !current() {
                            return;
                        }
                        for entry in &entries {
                            if entry.is_dir
                                && (include_hidden || !entry.name.starts_with('.'))
                                && entry_path(&entry.path).is_ok()
                                && parent(&entry.path) == dir
                            {
                                if depth < 64 && visited.len() + pending.len() < 512 {
                                    pending.push_back((entry.path.clone(), depth + 1));
                                } else {
                                    limited = true;
                                }
                            }
                        }
                        let visible: Vec<_> = entries
                            .iter()
                            .filter(|entry| include_hidden || !entry.name.starts_with('.'))
                            .cloned()
                            .collect();
                        let nested = openwebide_core::file_nesting::parents(&visible);
                        self.workspace.entries.update(|map| {
                            map.insert(dir.clone(), entries);
                        });
                        self.workspace.expanded.update(|expanded| {
                            expanded.insert(dir);
                            expanded.extend(nested.into_values());
                        });
                    }
                    Err(error) => {
                        if !current() {
                            return;
                        }
                        if error.needs_folder_access() {
                            self.projects.needs_grant.update(|ids| {
                                ids.insert(scope.project);
                            });
                        }
                        self.ui
                            .toast
                            .set(Some(format!("Could not expand all folders: {error}")));
                        self.expanding.set(false);
                        return;
                    }
                }
                if visited.len().is_multiple_of(16) {
                    crate::util::yield_task().await;
                }
            }
            if current() {
                self.expanding.set(false);
                if limited {
                    self.ui.toast.set(Some("Expanded up to 512 folders and 64 levels; expand remaining folders individually".into()));
                }
            }
        });
    }
    pub fn reveal(self, path: &str) {
        let Some(scope) = self.scope() else {
            return;
        };
        if path.split('/').any(|part| part.starts_with('.'))
            && let Some(actions) = self.layout
        {
            actions.set_tree_preferences.run(true);
        }
        if let Some(layout) = self.layout {
            layout
                .select_files_view
                .run(crate::state::responsive::FilesView::Explorer);
        }
        let mut dirs = vec![String::new()];
        let mut parent = openwebide_core::workspace_entries::parent(path);
        while !parent.is_empty() {
            dirs.push(parent.to_string());
            parent = openwebide_core::workspace_entries::parent(parent);
        }
        self.workspace
            .expanded
            .update(|expanded| expanded.extend(dirs.iter().cloned()));
        for dir in dirs {
            self.load_dir.run((scope.project, dir));
        }
        self.reveal_target.set(Some(path.to_string()));
    }
    fn scope(self) -> Option<Scope> {
        Some(Scope {
            project: self.projects.active_project.get_untracked()?,
            epoch: self.epoch.get_untracked(),
            auth: self.auth.generation.get_untracked(),
        })
    }
    fn current(self, scope: Scope) -> bool {
        self.projects.active_project.try_get_untracked() == Some(Some(scope.project))
            && self.auth.generation.try_get_untracked() == Some(scope.auth)
            && self.epoch.try_get_untracked() == Some(scope.epoch)
    }
    pub fn disabled(self) -> bool {
        self.busy.get()
            || self.projects.active_project.get().is_some_and(|project| {
                self.workspace.editor_operation_active(project)
                    || self.workspace.workspace_recovery.with(|states| {
                        states.get(&project).is_some_and(
                            crate::state::workspace::WorkspaceRecoveryStatus::blocks_saves,
                        )
                    })
            })
            || self.git_state.is_some_and(|git| {
                git.commit_busy.get() || git.branch_busy.get() || git.sync_busy.get().is_some()
            })
            || self
                .chat
                .is_some_and(|chat| chat.streaming.get() || chat.rewinding.get())
    }
    fn guard(self, scope: Scope, mutation: &Mutation) -> Result<(), String> {
        if !self.current(scope) {
            return Err("Project changed; reopen the menu".into());
        }
        if matches!(mutation, Mutation::Git(_) | Mutation::Stash(_))
            && self.git_state.is_some_and(|git| {
                git.status_error.get_untracked().is_some()
                    || git.status.with_untracked(|status| {
                        status.as_ref().is_some_and(|status| {
                            status.availability
                                != openwebide_core::git::GitStatusAvailability::Complete
                        })
                    })
            })
        {
            return Err("Git status unavailable. Reconnect the execution host and refresh.".into());
        }
        if self.disabled() {
            return Err("Wait for the current operation to finish".into());
        }
        self.guard_editor(mutation)
    }
    fn guard_editor(self, mutation: &Mutation) -> Result<(), String> {
        if matches!(
            mutation,
            Mutation::Stash(_)
                | Mutation::Git(GitPathRequest {
                    action: GitPathAction::StageAll | GitPathAction::UnstageAll,
                    ..
                })
        ) {
            if matches!(mutation, Mutation::Stash(request) if request.action==openwebide_core::git::GitStashAction::Drop)
            {
                return Ok(());
            }
            if self.workspace.is_resolving()
                || !self
                    .workspace
                    .pending_edits
                    .with_untracked(std::collections::HashMap::is_empty)
                || self.workspace.dirty.get_untracked()
                || self.workspace.editor_buffers.with_untracked(|buffers| {
                    buffers.iter().any(|((project, _), buffer)| {
                        Some(*project) == self.workspace.active_project.get_untracked()
                            && buffer.dirty
                    })
                })
            {
                return Err(
                    "Save or discard unsaved editor changes and resolve pending edits first".into(),
                );
            }
            return Ok(());
        }
        entry_path(mutation.path())?;
        if self.workspace.is_resolving()
            || self.workspace.pending_edits.with_untracked(|edits| {
                edits.keys().any(|path| {
                    contains_path(mutation.path(), path)
                        || (matches!(mutation, Mutation::Ignore(..)) && path == ".gitignore")
                })
            })
        {
            return Err("Accept or reject pending edits before changing this path".into());
        }
        let affected = self.workspace.open_file.with_untracked(|path| {
            path.as_ref()
                .is_some_and(|path| contains_path(mutation.path(), path))
        });
        let ignore_dirty = matches!(mutation, Mutation::Ignore(..))
            && self.workspace.open_file.get_untracked().as_deref() == Some(".gitignore");
        let retained_dirty = self.workspace.editor_buffers.with_untracked(|buffers| {
            buffers.iter().any(|((project, path), buffer)| {
                Some(*project) == self.workspace.active_project.get_untracked()
                    && buffer.dirty
                    && (contains_path(mutation.path(), path)
                        || (matches!(mutation, Mutation::Ignore(..)) && path == ".gitignore"))
            })
        });
        if ((self.workspace.dirty.get_untracked() && (affected || ignore_dirty)) || retained_dirty)
            && !mutation.discards()
        {
            return Err("Save or discard the editor's unsaved changes first".into());
        }
        Ok(())
    }
    fn dispatch(self, scope: Scope, mutation: Mutation) {
        if !self.current(scope) {
            return;
        }
        if let Err(error) = self.guard(scope, &mutation) {
            self.ui.notify(error);
            return;
        }
        let Some(save) = self.workspace.begin_editor_save(scope.project) else {
            self.ui
                .notify("Resolve workspace recovery before changing files");
            return;
        };
        self.busy.set(true);
        spawn_local(async move {
            let result = self.execute(scope, &mutation).await;
            drop(save);
            if !self.current(scope) {
                return;
            }
            self.busy.set(false);
            match result {
                Ok(()) => {
                    // Clear old directory keys before reloading expanded folders.
                    self.workspace.entries.set(Default::default());
                    self.workspace.search.set(None);
                    let mut dirs = self.workspace.expanded.get_untracked();
                    dirs.insert(String::new());
                    for dir in dirs {
                        self.load_dir.run((scope.project, dir));
                    }
                    self.refresh_git.run(());
                }
                Err(error) => {
                    self.ui.notify(error);
                    self.load_dir.run((scope.project, String::new()));
                    self.refresh_git.run(());
                }
            }
        });
    }
    async fn execute(self, scope: Scope, mutation: &Mutation) -> Result<(), String> {
        // Git owns its complete operation's critical section in ProjectGit.
        if matches!(mutation, Mutation::Git(_) | Mutation::Stash(_)) {
            return self.execute_files(scope, mutation).await;
        }
        let guard = crate::workspace_mutations::WorkspaceMutations::for_project(
            self.api,
            self.projects,
            self.auth,
            self.settings,
            scope.project,
            move || self.current(scope),
        )?;
        let mutation = mutation.clone();
        guard
            .run(async move { self.execute_files(scope, &mutation).await })
            .await
    }
    async fn execute_files(self, scope: Scope, mutation: &Mutation) -> Result<(), String> {
        if !self.current(scope) {
            return Err("Project changed".into());
        }
        self.guard_editor(mutation)?;
        let files = self
            .workspace_for
            .run(scope.project)
            .ok_or("Grant access to the project folder first")?;
        let current = || self.current(scope);
        match mutation {
            Mutation::Create(path, kind) => {
                openwebide_core::workspace_entries::create_entry(&files, path, *kind, current)
                    .await?;
                if current() && *kind == VfsEntryKind::File {
                    self.open.run(path.clone());
                }
            }
            Mutation::Move(from, to) => {
                files.move_entry(from, to, current).await?;
                if current() {
                    self.workspace.expanded.update(|dirs| {
                        *dirs = dirs
                            .iter()
                            .map(|path| moved_path(path, from, to).unwrap_or_else(|| path.clone()))
                            .collect();
                    });
                    let moved = self
                        .workspace
                        .open_file
                        .get_untracked()
                        .and_then(|path| moved_path(&path, from, to));
                    self.close_affected(from);
                    if let Some(path) = moved {
                        self.open.run(path);
                    }
                }
            }
            Mutation::Delete(path) => {
                openwebide_core::workspace_entries::delete_entry(&files, path, current).await?;
                if current() {
                    self.close_affected(path);
                }
            }
            Mutation::Ignore(path, is_dir) => {
                files.ignore(path, *is_dir, current).await?;
                if current()
                    && self.workspace.open_file.get_untracked().as_deref() == Some(".gitignore")
                {
                    self.close_affected(".gitignore");
                    self.open.run(".gitignore".into());
                }
            }
            Mutation::Stash(request) => {
                let repo = self
                    .git
                    .ok_or("Git execution unavailable")?
                    .repository(Some(scope.project))
                    .await?;
                if !current() {
                    return Err("Project changed".into());
                }
                let check = Mutation::Stash(request.clone());
                repo.with_current(move || self.current(scope))
                    .with_preflight(move || self.guard_editor(&check))
                    .stash(request)
                    .await?;
                if current() {
                    if let Some(git) = self.git_state {
                        git.changes_revision.update(|revision| *revision += 1);
                        git.history_revision.update(|revision| *revision += 1);
                    }
                    if matches!(
                        request.action,
                        openwebide_core::git::GitStashAction::Save
                            | openwebide_core::git::GitStashAction::Apply
                    ) {
                        let open = self.workspace.open_file.get_untracked();
                        let tabs = self.workspace.editor_tabs.with_untracked(|tabs| {
                            tabs.get(&scope.project).cloned().unwrap_or_default()
                        });
                        let paths = self.workspace.editor_buffers.with_untracked(|buffers| {
                            buffers
                                .keys()
                                .filter(|(project, _)| *project == scope.project)
                                .map(|(_, path)| path.clone())
                                .collect::<Vec<_>>()
                        });
                        for path in paths {
                            self.invalidate_buffers(&path);
                        }
                        if let Some(path) = open {
                            self.close_affected(&path);
                            self.open.run(path);
                        }
                        self.workspace.editor_tabs.update(|projects| {
                            projects.insert(scope.project, tabs);
                        });
                    }
                }
            }
            Mutation::Git(request) => {
                let repo = self
                    .git
                    .ok_or("Git execution unavailable")?
                    .repository(Some(scope.project))
                    .await?;
                if !current() {
                    return Err("Project changed".into());
                }
                let changes = repo.path_changes().await?;
                if !current() {
                    return Err("Project changed".into());
                }
                let paths = if matches!(
                    request.action,
                    GitPathAction::StageAll | GitPathAction::UnstageAll
                ) {
                    Vec::new()
                } else {
                    changes.action_paths(&request.path)?
                };
                for path in &paths {
                    self.guard_editor(&Mutation::Git(GitPathRequest {
                        path: path.clone(),
                        action: request.action,
                    }))?;
                }
                let check = Mutation::Git(request.clone());
                let changes = repo
                    .with_current(move || self.current(scope))
                    .with_preflight(move || self.guard_editor(&check))
                    .path_action(request)
                    .await?;
                if current()
                    && let Some(git) = self.git_state
                {
                    git.path_changes.set(Some(changes));
                    git.changes_revision.update(|revision| *revision += 1);
                }
                if current() && request.action == GitPathAction::Revert {
                    for path in &paths {
                        self.invalidate_buffers(path);
                    }
                }
                if current()
                    && request.action == GitPathAction::Revert
                    && let Some(open) = self.workspace.open_file.get_untracked()
                    && paths.iter().any(|path| contains_path(path, &open))
                {
                    self.workspace.dirty.set(false);
                    // A revert may remove a staged addition or rename destination.
                    let exists = openwebide_core::rewind::RewindFiles::read(&files, &open)
                        .await?
                        .is_some();
                    if current() {
                        if exists {
                            self.close_affected(&open);
                            self.open.run(open);
                        } else {
                            self.close_affected(&open);
                        }
                    }
                }
            }
        }
        Ok(())
    }
    fn invalidate_buffers(self, path: &str) {
        let project = self.workspace.active_project.get_untracked();
        self.workspace.editor_tabs.update(|tabs| {
            if let Some(paths) = project.and_then(|project| tabs.get_mut(&project)) {
                paths.retain(|file| !contains_path(path, file));
            }
        });
        self.workspace.editor_recovery_overwrites.update(|permits| {
            permits.retain(|(id, file), _| Some(*id) != project || !contains_path(path, file));
        });
        self.workspace.editor_recovery_checks.update(|checks| {
            checks.retain(|(id, file), _| Some(*id) != project || !contains_path(path, file));
        });
        self.workspace.editor_recovered.update(|files| {
            files.retain(|(id, file)| Some(*id) != project || !contains_path(path, file));
        });
        self.workspace.editor_buffers.update(|buffers| {
            buffers.retain(|(id, file), _| Some(*id) != project || !contains_path(path, file));
        });
        self.workspace.editor_documents.update(|documents| {
            documents.retain(|(id, file), _| Some(*id) != project || !contains_path(path, file));
        });
        self.workspace.editor_scroll.update(|positions| {
            positions.retain(|(id, file), _| Some(*id) != project || !contains_path(path, file));
        });
    }
    fn close_affected(self, path: &str) {
        self.invalidate_buffers(path);
        self.workspace
            .expanded
            .update(|dirs| dirs.retain(|dir| !contains_path(path, dir)));
        if self
            .workspace
            .open_file
            .with_untracked(|open| open.as_ref().is_some_and(|open| contains_path(path, open)))
        {
            if let Some(url) = self.workspace.media_url.get_untracked() {
                let _ = web_sys::Url::revoke_object_url(&url);
            }
            self.workspace.media_url.set(None);
            self.workspace.open_file.set(None);
            self.workspace.content.set(String::new().into());
            self.workspace.dirty.set(false);
        }
    }
    pub fn create(self, directory: &str, kind: VfsEntryKind) {
        let Some(scope) = self.scope() else {
            return;
        };
        if self.disabled() {
            return;
        }
        let value = if directory.is_empty() {
            String::new()
        } else {
            format!("{directory}/")
        };
        self.ui.set_prompt(PromptRequest {
            title: if kind.is_dir() {
                "New folder"
            } else {
                "New file"
            }
            .into(),
            value,
            placeholder: "Path relative to project".into(),
            submit_label: "Create".into(),
            on_submit: Callback::new(move |path: String| {
                self.dispatch(scope, Mutation::Create(path, kind));
            }),
        });
    }
    pub fn move_entry(self, entry: &FileEntry, rename: bool) {
        let Some(scope) = self.scope() else {
            return;
        };
        let source = entry.path.clone();
        if let Err(error) = self.guard(scope, &Mutation::Move(source.clone(), source.clone())) {
            self.ui.notify(error);
            return;
        }
        let value = if rename {
            entry.name.clone()
        } else {
            source.clone()
        };
        self.ui.set_prompt(PromptRequest {
            title: if rename { "Rename" } else { "Move" }.into(),
            value,
            placeholder: if rename {
                "New name"
            } else {
                "Destination path relative to project"
            }
            .into(),
            submit_label: if rename { "Rename" } else { "Move" }.into(),
            on_submit: Callback::new(move |value: String| {
                if rename && (value.is_empty() || value.contains(['/', '\\'])) {
                    self.ui.notify("Enter a single file or folder name");
                    return;
                }
                let to = if rename && !parent(&source).is_empty() {
                    format!("{}/{value}", parent(&source))
                } else {
                    value
                };
                self.dispatch(scope, Mutation::Move(source.clone(), to));
            }),
        });
    }
    pub fn delete(self, entry: &FileEntry) {
        self.confirm(Mutation::Delete(entry.path.clone()), "Delete", format!(
            "Delete `{}`{}? Unsaved editor changes for this path will be discarded. This cannot be undone.",
            entry.path, if entry.is_dir {" and everything inside it"} else {""}));
    }
    pub fn history(self, path: &str) {
        if let Some(git) = self.git_state {
            git.file_history.set(Some(path.to_owned()));
        }
    }
    pub fn git_action(self, path: &str, action: GitPathAction) {
        let mutation = Mutation::Git(GitPathRequest {
            path: path.into(),
            action,
        });
        if action == GitPathAction::Revert {
            self.confirm(mutation, "Revert", format!("Discard staged, working-tree and unsaved editor changes for `{path}` (including the original paths of renamed files) and restore HEAD? Untracked files are preserved."));
        } else if let Some(scope) = self.scope() {
            self.dispatch(scope, mutation);
        }
    }
    pub fn stash(self, request: openwebide_core::git::GitStashRequest) {
        let mutation = Mutation::Stash(request.clone());
        if request.action == openwebide_core::git::GitStashAction::Drop {
            self.confirm(mutation,"Drop stash",format!("Permanently drop stash {}? Its saved changes will no longer be recoverable from the stash list.",request.hash.as_deref().unwrap_or_default()));
        } else if let Some(scope) = self.scope() {
            self.dispatch(scope, mutation);
        }
    }
    fn confirm(self, mutation: Mutation, title: &str, message: String) {
        let Some(scope) = self.scope() else {
            return;
        };
        if let Err(error) = self.guard(scope, &mutation) {
            self.ui.notify(error);
            return;
        }
        self.ui.set_confirm(ConfirmRequest {
            title: title.into(),
            message,
            confirm_label: title.into(),
            action: Callback::new(move |()| self.dispatch(scope, mutation.clone())),
        });
    }
    pub fn ignore(self, entry: &FileEntry) {
        if let Some(scope) = self.scope() {
            self.dispatch(scope, Mutation::Ignore(entry.path.clone(), entry.is_dir));
        }
    }
    pub fn copy_path(self, path: &str) {
        let Some(scope) = self.scope() else {
            return;
        };
        let path = path.to_string();
        spawn_local(async move {
            if let Err(error) = crate::clipboard::copy_text(&path).await
                && self.current(scope)
            {
                self.ui.notify(error);
            }
        });
    }
    pub async fn path_changes(self) -> Result<openwebide_core::git::GitPathChanges, String> {
        let scope = self.scope().ok_or("Open a project first")?;
        let repository = self
            .git
            .ok_or("Git execution unavailable")?
            .repository(Some(scope.project))
            .await?;
        if !self.current(scope) {
            return Err("Project changed".into());
        }
        let result = repository.path_changes().await;
        if !self.current(scope) {
            return Err("Project changed".into());
        }
        result
    }
    pub fn chat(self, entry: &FileEntry, task: &str, diff: bool) {
        let Some(scope) = self.scope() else {
            return;
        };
        let Some(chat) = self.chat else {
            return;
        };
        let path = &entry.path;
        let kind = if diff {
            openwebide_core::prompt::MentionKind::Diff
        } else if entry.is_dir {
            openwebide_core::prompt::MentionKind::Folder
        } else {
            openwebide_core::prompt::MentionKind::File
        };
        let prompt = format!(
            "{task} {}",
            openwebide_core::prompt::mention_token(kind, path)
        );
        if self.disabled()
            || chat.connection_changing.get_untracked()
            || chat.creating_session.get_untracked()
        {
            self.ui
                .notify("Wait for the current chat operation to finish");
            return;
        }
        let Some(send) = self.send_prompt.get_untracked() else {
            self.ui.notify("Chat actions unavailable");
            return;
        };
        send.run(prompt);
        if let Some(layout) = self.layout {
            layout.show.run(Panel::Chat);
        }
        leptos::leptos_dom::helpers::request_animation_frame(move || {
            if !self.current(scope) {
                return;
            }
            if let Ok(Some(input)) = document().query_selector(".composer-input")
                && let Some(input) = input.dyn_ref::<web_sys::HtmlElement>()
            {
                let _ = input.focus();
            }
        });
    }
}
