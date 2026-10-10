use leptos::{prelude::*, task::spawn_local};
use openwebide_core::{
    Project,
    editor::{EditorRecoveryRecord, RecoveryDiskState},
};

use crate::{
    backend::{Api, RecoveryError},
    state::{
        auth::AuthState,
        editor_recovery::{EditorRecoveryState, RecoveryPhase, RecoveryProject},
        projects::ProjectsState,
        settings::SettingsState,
        ui::{ConfirmRequest, UiState},
        workspace::{RecoveredFileIssue, WorkspaceState},
    },
    workspace::Workspace,
};

#[derive(Clone, Copy)]
struct RecoveryContext {
    api: Api,
    auth: AuthState,
    projects: ProjectsState,
    workspace: WorkspaceState,
    state: EditorRecoveryState,
    settings: SettingsState,
    ui: UiState,
    read_only: RwSignal<bool>,
    open: Callback<String>,
    counter: StoredValue<u64>,
    timers: StoredValue<std::collections::HashMap<i64, u64>>,
    checking: StoredValue<std::collections::HashMap<i64, u64>>,
    file_guard: StoredValue<send_wrapper::SendWrapper<Option<FileReviewGuard>>>,
    save_file: Callback<()>,
}

struct FileReviewGuard {
    ticket: u64,
    project: Project,
    account: u64,
    handle: Option<web_sys::FileSystemDirectoryHandle>,
    bridge: String,
    guard: crate::state::workspace::EditorRecoveryGuard,
    disk: Option<String>,
    path: String,
}

#[derive(Clone, Copy)]
enum LoadPolicy {
    Initial,
    Restore,
    KeepCurrent,
}

#[derive(Clone, Copy)]
pub struct RecoveryActions {
    pub review_file: Callback<()>,
    pub close_file_review: Callback<()>,
    pub reload_file: Callback<()>,
    pub overwrite_file: Callback<()>,
    pub retry: Callback<i64>,
    pub restore: Callback<i64>,
    pub keep_current: Callback<i64>,
    pub check_files: Callback<i64>,
}

impl RecoveryContext {
    fn next_ticket(self) -> u64 {
        self.counter
            .update_value(|ticket| *ticket = ticket.wrapping_add(1));
        self.counter.get_value()
    }

    fn current(self, account: u64, project: &Project, ticket: u64) -> bool {
        self.auth.generation.try_get_untracked() == Some(account)
            && self.auth.user.try_with_untracked(Option::is_some) == Some(true)
            && self.projects.projects.try_with_untracked(|projects| {
                projects.iter().any(|candidate| {
                    candidate.id == project.id
                        && Workspace::root_identity(candidate) == Workspace::root_identity(project)
                })
            }) == Some(true)
            && self.state.projects.try_with_untracked(|projects| {
                projects
                    .get(&project.id)
                    .is_some_and(|entry| entry.ticket == ticket)
            }) == Some(true)
    }

    fn reject_changed_root(self, account: u64, id: i64, ticket: u64) {
        if self.auth.generation.try_get_untracked() == Some(account)
            && self.projects.project(id).is_some()
            && self.state.projects.try_with_untracked(|projects| {
                projects
                    .get(&id)
                    .is_some_and(|entry| entry.ticket == ticket)
            }) == Some(true)
        {
            self.phase(id, RecoveryPhase::Conflict("The project folder changed while recovery was running. Your current files are preserved.".into()));
        }
    }

    fn phase(self, id: i64, phase: RecoveryPhase) {
        self.state.projects.update(|projects| {
            if let Some(entry) = projects.get_mut(&id) {
                entry.phase = phase;
                entry.writing = false;
            }
        });
    }

    fn load(self, id: i64, policy: LoadPolicy) {
        if self
            .state
            .projects
            .with_untracked(|projects| projects.get(&id).is_some_and(|entry| entry.writing))
        {
            return;
        }
        let Some(project) = self.projects.project(id) else {
            return;
        };
        let account = self.auth.generation.get_untracked();
        let guard = match self
            .workspace
            .editor_recovery_guard(&project, self.read_only.get_untracked())
        {
            Ok(guard) => guard,
            Err(error) => {
                self.ui.notify(error);
                return;
            }
        };
        let occupied = self
            .workspace
            .editor_recovery(&project, self.read_only.get_untracked())
            .is_ok_and(|state| !state.files.is_empty());
        let handle = self
            .projects
            .local_handles
            .with_untracked(|handles| handles.get(&id).cloned());
        let bridge = self.settings.bridge_url.get_untracked();
        let ticket = self.next_ticket();
        self.state.projects.update(|projects| {
            let entry = projects.entry(id).or_insert(RecoveryProject {
                record: None,
                phase: RecoveryPhase::Loading,
                ticket,
                writing: false,
            });
            entry.phase = RecoveryPhase::Loading;
            entry.ticket = ticket;
        });
        spawn_local(async move {
            let result = self.api.with_value(Clone::clone).editor_recovery(id).await;
            if !self.current(account, &project, ticket) {
                self.reject_changed_root(account, id, ticket);
                return;
            }
            let record = match result {
                Ok(record) => record,
                Err(error) => {
                    self.phase(id, RecoveryPhase::LoadFailed(error.to_string()));
                    return;
                }
            };
            if record.revision < 0 || record.state.validate().is_err() {
                self.phase(
                    id,
                    RecoveryPhase::LoadFailed("Invalid editor recovery response".into()),
                );
                return;
            }
            let phase = if matches!(policy, LoadPolicy::KeepCurrent)
                || (record.revision == 0 && record.state.files.is_empty())
            {
                RecoveryPhase::Ready
            } else if occupied && matches!(policy, LoadPolicy::Initial) {
                RecoveryPhase::Conflict("Saved editor files are available. Choose which files to keep before recovery saves resume.".into())
            } else {
                let current = || {
                    self.current(account, &project, ticket)
                        && self
                            .projects
                            .local_handles
                            .try_with_untracked(|handles| handles.get(&id).cloned())
                            == Some(handle.clone())
                        && self.settings.bridge_url.try_get_untracked().as_ref() == Some(&bridge)
                };
                let hydrated = super::editor::EditorActions::restore_recovery(
                    self.workspace,
                    &project,
                    &guard,
                    &record.state,
                    self.read_only.get_untracked(),
                    current,
                )
                .await;
                if !current() {
                    self.reject_changed_root(account, id, ticket);
                    return;
                }
                match hydrated {
                    Ok(Some(hydrated)) => {
                        self.workspace.editor_recovery_checks.update(|checks| {
                            checks.retain(|(project, _), _| *project != id);
                            for file in &record.state.files {
                                if file.document.is_some() { checks.insert((id, file.path.clone()), RecoveredFileIssue::Pending); }
                            }
                        });
                        self.workspace.editor_recovered.update(|files| {
                            files.retain(|(project, _)| *project != id);
                            files.extend(record.state.files.iter().filter(|file| file.document.is_some()).map(|file| (id, file.path.clone())));
                        });
                        for url in hydrated.revoke_urls { let _ = web_sys::Url::revoke_object_url(&url); }
                        if self.projects.active_project.get_untracked() == Some(id) {
                            self.read_only.set(hydrated.read_only);
                            if let Some(path) = hydrated.selected { self.open.run(path); }
                        }
                        RecoveryPhase::Ready
                    }
                    Ok(None) => RecoveryPhase::Conflict("The editor changed while recovery was loading. Your current files are preserved; choose which files to keep.".into()),
                    Err(error) => RecoveryPhase::Conflict(error),
                }
            };
            self.state.projects.update(|projects| {
                if let Some(entry) = projects.get_mut(&id) {
                    entry.record = Some(record);
                    entry.phase = phase;
                }
            });
        });
    }

    fn schedule(self, id: i64) {
        let ticket = self.next_ticket();
        self.timers.update_value(|timers| {
            timers.insert(id, ticket);
        });
        let account = self.auth.generation.get_untracked();
        spawn_local(async move {
            crate::util::sleep_ms(500).await;
            if self.auth.generation.try_get_untracked() != Some(account)
                || self
                    .timers
                    .try_with_value(|timers| timers.get(&id).copied())
                    != Some(Some(ticket))
            {
                return;
            }
            self.save(id, account).await;
        });
    }

    async fn save(self, id: i64, account: u64) {
        let Some(project) = self.projects.project(id) else {
            return;
        };
        let previous = self.state.projects.with_untracked(|projects| {
            projects
                .get(&id)
                .filter(|entry| entry.phase == RecoveryPhase::Ready && !entry.writing)
                .and_then(|entry| {
                    entry
                        .record
                        .as_ref()
                        .map(|record| (entry.ticket, record.clone()))
                })
        });
        let Some((ticket, previous)) = previous else {
            return;
        };
        if !self.current(account, &project, ticket) {
            return;
        }
        if previous.state.root.as_ref().is_some_and(|root| {
            *root != openwebide_core::editor::EditorRecoveryRoot::for_project(&project)
        }) {
            self.phase(id, RecoveryPhase::Conflict("The project folder changed. Choose which editor files to keep before recovery saves resume.".into()));
            return;
        }
        let state = match self
            .workspace
            .editor_recovery(&project, self.read_only.get_untracked())
        {
            Ok(state) => state,
            Err(error) => {
                self.phase(id, RecoveryPhase::SaveFailed(error));
                return;
            }
        };
        if state == previous.state
            || (state.files.is_empty()
                && previous.state.files.is_empty()
                && state.selected.is_none()
                && previous.state.selected.is_none())
        {
            return;
        }
        let record = EditorRecoveryRecord {
            revision: previous.revision,
            state,
        };
        self.state.projects.update(|projects| {
            if let Some(entry) = projects.get_mut(&id) {
                entry.writing = true;
            }
        });
        let result = self
            .api
            .with_value(Clone::clone)
            .save_editor_recovery(id, &record)
            .await;
        if !self.current(account, &project, ticket) {
            self.reject_changed_root(account, id, ticket);
            return;
        }
        match result {
            Ok(revision) if record.revision.checked_add(1) == Some(revision) => {
                self.state.projects.update(|projects| {
                    if let Some(entry) = projects.get_mut(&id) {
                        entry.record = Some(EditorRecoveryRecord {
                            revision,
                            state: record.state,
                        });
                        entry.writing = false;
                    }
                });
            }
            Ok(_) => self.phase(
                id,
                RecoveryPhase::SaveFailed("Invalid editor recovery save acknowledgement".into()),
            ),
            Err(RecoveryError::Conflict(error)) => self.phase(id, RecoveryPhase::Conflict(error)),
            Err(RecoveryError::Unavailable(error)) => {
                self.phase(id, RecoveryPhase::SaveFailed(error));
            }
        }
    }
}

impl RecoveryActions {
    pub fn new(
        api: Api,
        auth: AuthState,
        projects: ProjectsState,
        workspace: WorkspaceState,
        state: EditorRecoveryState,
        read_only: RwSignal<bool>,
        open: Callback<String>,
    ) -> Self {
        let ctx = RecoveryContext {
            api,
            auth,
            projects,
            workspace,
            state,
            read_only,
            open,
            settings: expect_context(),
            ui: expect_context(),
            counter: StoredValue::new(0),
            timers: StoredValue::new(Default::default()),
            checking: StoredValue::new(Default::default()),
            file_guard: StoredValue::new(send_wrapper::SendWrapper::new(None)),
            save_file: expect_context::<super::workspace::WorkspaceActions>().on_save,
        };
        let account = StoredValue::new(auth.generation.get_untracked());
        Effect::new(move |_| {
            let generation = auth.generation.get();
            if account.get_value() != generation {
                account.set_value(generation);
                ctx.timers.set_value(Default::default());
                ctx.checking.set_value(Default::default());
                state.projects.set(Default::default());
                state.file_review.set(None);
                ctx.file_guard
                    .set_value(send_wrapper::SendWrapper::new(None));
            }
        });
        Effect::new(move |_| {
            auth.generation.track();
            if auth.user.get().is_none() || !projects.projects_loaded.get() {
                return;
            }
            let Some(id) = projects.active_project.get() else {
                return;
            };
            projects.projects.track();
            if !state.projects.with(|projects| projects.contains_key(&id)) {
                ctx.load(id, LoadPolicy::Initial);
            }
        });
        Effect::new(move |_| {
            auth.generation.track();
            workspace.active_project.track();
            workspace.open_file.track();
            workspace.content.track();
            workspace.dirty.track();
            workspace.editor_tabs.track();
            workspace.editor_documents.track();
            workspace.editor_buffers.track();
            workspace.editor_scroll.track();
            workspace.snapshots.track();
            read_only.track();
            projects.projects.track();
            let ids = state.projects.with(|projects| {
                projects
                    .iter()
                    .filter(|(_, entry)| entry.phase == RecoveryPhase::Ready && !entry.writing)
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>()
            });
            for id in ids {
                ctx.schedule(id);
            }
        });
        let recheck_files = Callback::new(move |id| {
            workspace.editor_recovery_checks.update(|checks| {
                for ((project, _), issue) in checks {
                    if *project == id {
                        *issue = RecoveredFileIssue::Pending;
                    }
                }
            });
            check_files(ctx, id);
        });
        Effect::new(move |_| {
            projects.local_handles.track();
            ctx.settings.bridge_url.track();
            if let Some(id) = projects.active_project.get() {
                workspace.editor_recovery_checks.update(|checks| {
                    for ((project, _), issue) in checks {
                        if *project == id && matches!(issue, RecoveredFileIssue::Unavailable(_)) {
                            *issue = RecoveredFileIssue::Pending;
                        }
                    }
                });
                check_files(ctx, id);
            }
        });
        Effect::new(move |_| {
            auth.generation.track();
            projects.local_handles.track();
            ctx.settings.bridge_url.track();
            let id = projects.active_project.get();
            workspace.editor_recovery_checks.track();
            workspace.editor_composition.track();
            if let Some(id) = id {
                check_files(ctx, id);
            }
        });
        let retry = Callback::new(move |id| {
            let phase = state
                .projects
                .with_untracked(|projects| projects.get(&id).map(|entry| entry.phase.clone()));
            match phase {
                Some(RecoveryPhase::SaveFailed(_)) => ctx.phase(id, RecoveryPhase::Ready),
                Some(RecoveryPhase::Loading | RecoveryPhase::Conflict(_)) => {}
                _ => ctx.load(id, LoadPolicy::Initial),
            }
            recheck_files.run(id);
        });
        let choice = move |id: i64, policy: LoadPolicy| {
            let generation = auth.generation.get_untracked();
            let Some(project) = projects.project(id) else {
                return;
            };
            let Ok(guard) = workspace.editor_recovery_guard(&project, read_only.get_untracked())
            else {
                return;
            };
            ctx.ui.confirm.set(Some(ConfirmRequest {
                title: "Resolve editor recovery".into(),
                message: match policy { LoadPolicy::Restore => "Replace this project's open files and drafts with the saved recovery?", _ => "Keep this window's files and replace the saved recovery? Other windows may have newer drafts." }.into(),
                confirm_label: match policy { LoadPolicy::Restore => "Restore saved files", _ => "Keep this window" }.into(),
                action: Callback::new(move |()| {
                    if auth.generation.try_get_untracked() != Some(generation) || projects.project(id).is_none() { return; }
                    // Test the captured guard without mutating state by collecting
                    // the same snapshot; load captures a new guard before its I/O.
                    if !workspace.editor_recovery_guard_matches(&project, &guard, read_only.get_untracked()) {
                        ctx.ui.notify("The editor changed while the dialog was open. Choose again to review the latest files."); return;
                    }
                    ctx.load(id, policy);
                }),
            }));
        };
        Self {
            review_file: Callback::new(move |()| ctx.review_file()),
            close_file_review: Callback::new(move |()| {
                state.file_review_ticket.set(ctx.next_ticket());
                state.file_review.set(None);
                ctx.file_guard
                    .set_value(send_wrapper::SendWrapper::new(None));
            }),
            reload_file: Callback::new(move |()| ctx.resolve_file(false)),
            overwrite_file: Callback::new(move |()| ctx.resolve_file(true)),
            retry,
            check_files: recheck_files,
            restore: Callback::new(move |id| choice(id, LoadPolicy::Restore)),
            keep_current: Callback::new(move |id| choice(id, LoadPolicy::KeepCurrent)),
        }
    }
}

struct CheckGuard {
    ctx: RecoveryContext,
    id: i64,
    ticket: u64,
}
impl Drop for CheckGuard {
    fn drop(&mut self) {
        let owned = self
            .ctx
            .checking
            .try_with_value(|checking| checking.get(&self.id).copied())
            == Some(Some(self.ticket));
        if owned {
            self.ctx.checking.update_value(|checking| {
                checking.remove(&self.id);
            });
            check_files(self.ctx, self.id);
        }
    }
}

fn check_files(ctx: RecoveryContext, id: i64) {
    if ctx
        .workspace
        .editor_composition
        .with_untracked(|composition| composition.as_ref().is_some_and(|owner| owner.key.0 == id))
    {
        return;
    }
    if ctx
        .checking
        .with_value(|checking| checking.contains_key(&id))
    {
        return;
    }
    let Some(project) = ctx.projects.project(id) else {
        return;
    };
    let Some(ws) = Workspace::for_project(ctx.api, ctx.projects, id) else {
        ctx.projects.needs_grant.update(|ids| {
            ids.insert(id);
        });
        return;
    };
    let account = ctx.auth.generation.get_untracked();
    let handle = ctx
        .projects
        .local_handles
        .with_untracked(|handles| handles.get(&id).cloned());
    let bridge = ctx.settings.bridge_url.get_untracked();
    let paths = ctx
        .workspace
        .editor_recovery_checks
        .with_untracked(|checks| {
            checks
                .iter()
                .filter(|((project, _), message)| {
                    *project == id && **message == RecoveredFileIssue::Pending
                })
                .map(|((_, path), _)| path.clone())
                .collect::<Vec<_>>()
        });
    if paths.is_empty() {
        return;
    }
    let ticket = ctx.next_ticket();
    ctx.checking.update_value(|checking| {
        checking.insert(id, ticket);
    });
    spawn_local(async move {
        let _check = CheckGuard { ctx, id, ticket };
        for path in paths {
            let current = || {
                ctx.auth.generation.try_get_untracked() == Some(account)
                    && ctx
                        .workspace
                        .editor_composition
                        .try_with_untracked(|composition| {
                            composition.as_ref().is_none_or(|owner| owner.key.0 != id)
                        })
                        == Some(true)
                    && ctx.projects.project(id).is_some_and(|current| {
                        Workspace::root_identity(&current) == Workspace::root_identity(&project)
                    })
                    && ctx
                        .projects
                        .local_handles
                        .try_with_untracked(|handles| handles.get(&id).cloned())
                        == Some(handle.clone())
                    && ctx.settings.bridge_url.try_get_untracked().as_ref() == Some(&bridge)
            };
            if !current() {
                return;
            }
            let disk = ws.read_optional_bytes(&path).await;
            if !current() {
                return;
            }
            let result = disk
                .map_err(|error| {
                    if error.needs_folder_access() {
                        ctx.projects.needs_grant.update(|ids| {
                            ids.insert(id);
                        });
                    }
                    error.to_string()
                })
                .and_then(|bytes| {
                    bytes
                        .map(String::from_utf8)
                        .transpose()
                        .map_err(|_| "The disk file is no longer valid UTF-8".into())
                });
            let key = (id, path.clone());
            // Always reconcile the latest committed document, never an old draft
            // captured before the disk read. Edits during verification are retained.
            let result = ctx.workspace.editor_documents.with_untracked(|documents| {
                documents.get(&key).map(|document| {
                    result.and_then(|disk| {
                        document
                            .recovery_disk_state(disk.as_deref())
                            .map(|state| (state, disk))
                    })
                })
            });
            let Some(result) = result else {
                ctx.workspace.editor_recovery_checks.update(|checks| {
                    checks.remove(&key);
                });
                continue;
            };
            match result {
                Ok((state, disk)) => {
                    match state {
                        RecoveryDiskState::Current => {}
                        RecoveryDiskState::AlreadySaved => {
                            let disk = disk.unwrap_or_default();
                            let source = ctx
                                .workspace
                                .editor_documents
                                .try_update(|documents| {
                                    let document = documents.get_mut(&key)?;
                                    let source = if document.matches_text(&disk) {
                                        document.shared_text()
                                    } else {
                                        std::sync::Arc::new(disk)
                                    };
                                    document.mark_saved_source(source.clone());
                                    Some(source)
                                })
                                .flatten();
                            if let Some(source) = source {
                                update_buffer_source(
                                    ctx.workspace,
                                    id,
                                    &path,
                                    source.into(),
                                    false,
                                );
                            }
                        }
                        RecoveryDiskState::Reloaded => {
                            let retained =
                                ctx.workspace.editor_documents.with_untracked(|documents| {
                                    documents.get(&key).map(|document| {
                                        (document.revision(), document.shared_text())
                                    })
                                });
                            let owned = || {
                                current()
                                    && ctx.workspace.editor_documents.try_with_untracked(
                                        |documents| {
                                            documents.get(&key).zip(retained.as_ref()).is_some_and(
                                                |(document, (revision, source))| {
                                                    document.revision() == *revision
                                                        && std::sync::Arc::ptr_eq(
                                                            &document.shared_text(),
                                                            source,
                                                        )
                                                },
                                            )
                                        },
                                    ) == Some(true)
                            };
                            let source = std::sync::Arc::new(disk.unwrap());
                            let prepared =
                                super::editor::EditorActions::prepare_source(&source, owned).await;
                            if !owned() {
                                continue;
                            }
                            match prepared {
                                Ok(Some(document)) => {
                                    ctx.workspace.editor_documents.update(|documents| {
                                        documents.insert(key.clone(), document.clone());
                                    });
                                    update_buffer_source(
                                        ctx.workspace,
                                        id,
                                        &path,
                                        document.shared_text().into(),
                                        false,
                                    );
                                }
                                Ok(None) => continue,
                                Err(error) => {
                                    ctx.workspace.editor_recovery_checks.update(|checks| {
                                        checks.insert(
                                            key,
                                            RecoveredFileIssue::Unavailable(
                                                openwebide_core::editor::EditError::Capacity(error)
                                                    .to_string(),
                                            ),
                                        );
                                    });
                                    continue;
                                }
                            }
                        }
                        RecoveryDiskState::Conflict | RecoveryDiskState::Missing => {
                            ctx.workspace.editor_recovery_checks.update(|checks| {
                                checks.insert(
                                    key,
                                    match state {
                                        RecoveryDiskState::Missing => RecoveredFileIssue::Missing,
                                        _ => RecoveredFileIssue::Conflict,
                                    },
                                );
                            });
                            continue;
                        }
                    }
                    ctx.workspace.editor_recovery_checks.update(|checks| {
                        checks.remove(&key);
                    });
                }
                Err(error) => ctx.workspace.editor_recovery_checks.update(|checks| {
                    checks.insert(key, RecoveredFileIssue::Unavailable(error));
                }),
            }
        }
    });
}

fn update_buffer_source(
    workspace: WorkspaceState,
    id: i64,
    path: &str,
    content: crate::state::workspace::EditorText,
    dirty: bool,
) {
    batch(|| {
        workspace.editor_buffers.update(|buffers| {
            if let Some(buffer) = buffers.get_mut(&(id, path.into())) {
                buffer.content.clone_from(&content);
                buffer.dirty = dirty;
            }
        });
        if workspace.active_project.get_untracked() == Some(id)
            && workspace
                .open_file
                .with_untracked(|open| open.as_deref() == Some(path))
        {
            workspace.content.set(content.clone());
            workspace.dirty.set(dirty);
        } else {
            workspace.snapshots.update(|snapshots| {
                if let Some(snapshot) = snapshots.get_mut(&id)
                    && snapshot.open_file.as_deref() == Some(path)
                {
                    snapshot.content.clone_from(&content);
                    snapshot.dirty = dirty;
                }
            });
        }
    });
}

/// Recheck a recovered document's baseline immediately before the normal save
/// facade prepares its write. Verification failures never authorize overwrites.
pub async fn verify_recovered_save(
    workspace: WorkspaceState,
    projects: ProjectsState,
    ws: &Workspace,
    id: i64,
    path: &str,
    current: impl Fn() -> bool,
) -> Result<(), String> {
    let key = (id, path.to_string());
    if !workspace
        .editor_recovered
        .with_untracked(|files| files.contains(&key))
    {
        return Ok(());
    }
    let disk = ws.read_optional_bytes(path).await;
    if !current() {
        return Err("The editor changed while checking the disk file".into());
    }
    let disk = disk
        .map_err(|error| {
            if error.needs_folder_access() {
                projects.needs_grant.update(|ids| {
                    ids.insert(id);
                });
            }
            error.to_string()
        })
        .and_then(|bytes| {
            bytes
                .map(String::from_utf8)
                .transpose()
                .map_err(|_| "The disk file is no longer valid UTF-8".into())
        });
    let approved = workspace
        .editor_recovery_overwrites
        .try_update(|permits| permits.remove(&key))
        .flatten();
    if let Some(expected) = approved
        && workspace.content.get_untracked() == expected.draft
        && disk.as_ref().is_ok_and(|actual| *actual == expected.disk)
    {
        return Ok(());
    }
    let result = disk.and_then(|disk| {
        workspace.editor_documents.with_untracked(|documents| {
            documents
                .get(&key)
                .ok_or_else(|| "The recovered document's baseline is not ready".to_string())?
                .recovery_disk_state(disk.as_deref())
        })
    });
    let issue = match result {
        Ok(RecoveryDiskState::Current | RecoveryDiskState::AlreadySaved) => return Ok(()),
        Ok(RecoveryDiskState::Missing) => RecoveredFileIssue::Missing,
        Ok(_) => RecoveredFileIssue::Conflict,
        Err(error) => RecoveredFileIssue::Unavailable(error),
    };
    let message = issue.message();
    workspace.editor_recovery_checks.update(|checks| {
        checks.insert(key, issue);
    });
    Err(message)
}

impl RecoveryContext {
    fn review_root_current(self, scope: &FileReviewGuard) -> bool {
        self.state.file_review_ticket.try_get_untracked() == Some(scope.ticket)
            && self.auth.generation.try_get_untracked() == Some(scope.account)
            && self.projects.active_project.try_get_untracked() == Some(Some(scope.project.id))
            && self
                .projects
                .project(scope.project.id)
                .is_some_and(|project| {
                    Workspace::root_identity(&project) == Workspace::root_identity(&scope.project)
                })
            && self
                .projects
                .local_handles
                .try_with_untracked(|handles| handles.get(&scope.project.id).cloned())
                == Some(scope.handle.clone())
            && self.settings.bridge_url.try_get_untracked().as_ref() == Some(&scope.bridge)
    }
    fn review_current(self, scope: &FileReviewGuard) -> bool {
        self.review_root_current(scope)
            && self.workspace.editor_recovery_guard_matches(
                &scope.project,
                &scope.guard,
                self.read_only.get_untracked(),
            )
    }

    fn review_file(self) {
        let Some(id) = self.projects.active_project.get_untracked() else {
            return;
        };
        let Some(project) = self.projects.project(id) else {
            return;
        };
        let Some(path) = self.workspace.open_file.get_untracked() else {
            return;
        };
        let Some(ws) = Workspace::for_project(self.api, self.projects, id) else {
            return;
        };
        let Ok(guard) = self
            .workspace
            .editor_recovery_guard(&project, self.read_only.get_untracked())
        else {
            return;
        };
        let ticket = self.next_ticket();
        self.state.file_review_ticket.set(ticket);
        let mut scope = FileReviewGuard {
            ticket,
            project,
            account: self.auth.generation.get_untracked(),
            handle: self
                .projects
                .local_handles
                .with_untracked(|handles| handles.get(&id).cloned()),
            bridge: self.settings.bridge_url.get_untracked(),
            guard,
            path: path.clone(),
            disk: None,
        };
        spawn_local(async move {
            let result = read_review_disk(&ws, &path).await;
            if !self.review_current(&scope) {
                return;
            }
            match result {
                Ok(disk) => {
                    scope.disk.clone_from(&disk);
                    self.state.file_review.set(Some(
                        crate::state::editor_recovery::RecoveryFileReview {
                            path,
                            disk,
                            draft: self.workspace.content.get_untracked().into(),
                            read_only: self.read_only.get_untracked(),
                        },
                    ));
                    self.file_guard
                        .set_value(send_wrapper::SendWrapper::new(Some(scope)));
                }
                Err(error) => self.ui.notify(error),
            }
        });
    }

    fn invalidate_review(self, scope: &FileReviewGuard) {
        if self.auth.generation.try_get_untracked() == Some(scope.account)
            && self.state.file_review_ticket.try_get_untracked() == Some(scope.ticket)
        {
            self.state.file_review.set(None);
            self.ui.notify("The editor changed while its review was open. Review the recovered file again before choosing an action.");
        }
    }

    async fn read_review_current(
        self,
        ws: &Workspace,
        scope: &FileReviewGuard,
    ) -> Option<Option<String>> {
        let result = read_review_disk(ws, &scope.path).await;
        if !self.review_current(scope) {
            self.invalidate_review(scope);
            return None;
        }
        let disk = match result {
            Ok(disk) => disk,
            Err(error) => {
                self.ui.notify(error);
                self.state.file_review.set(None);
                return None;
            }
        };
        if disk != scope.disk {
            self.state.file_review.set(None);
            self.ui.notify("The disk file changed while its review was open. Review it again before choosing an action.");
            return None;
        }
        Some(disk)
    }

    fn resolve_file(self, overwrite: bool) {
        let scope = self
            .file_guard
            .try_update_value(|guard| (**guard).take())
            .flatten();
        let Some(scope) = scope else {
            return;
        };
        if !self.review_current(&scope) || (overwrite && self.read_only.get_untracked()) {
            self.invalidate_review(&scope);
            return;
        }
        let Some(ws) = Workspace::for_project(self.api, self.projects, scope.project.id) else {
            return;
        };
        spawn_local(async move {
            let Some(disk) = self.read_review_current(&ws, &scope).await else {
                return;
            };
            let key = (scope.project.id, scope.path.clone());
            if overwrite {
                self.workspace.editor_recovery_overwrites.update(|permits| {
                    permits.insert(
                        key.clone(),
                        crate::state::workspace::RecoveryOverwrite {
                            disk,
                            draft: self.workspace.content.get_untracked().into(),
                        },
                    );
                });
                self.workspace.editor_recovery_checks.update(|checks| {
                    checks.remove(&key);
                });
                self.state.file_review.set(None);
                self.save_file.run(());
            } else if let Some(disk) = disk {
                let source = std::sync::Arc::new(disk);
                let revision = self.workspace.editor_source_revision.get_untracked();
                let owned = || {
                    self.review_root_current(&scope)
                        && self.workspace.editor_source_revision.try_get_untracked()
                            == Some(revision)
                        && self
                            .workspace
                            .editor_composition
                            .try_with_untracked(Option::is_none)
                            == Some(true)
                };
                let prepared = super::editor::EditorActions::prepare_source(&source, owned).await;
                if !owned() || !self.review_current(&scope) {
                    self.invalidate_review(&scope);
                    return;
                }
                // Preparation yields to other host activity. Recheck the reviewed
                // disk version before publishing its completed editor snapshot.
                if self.read_review_current(&ws, &scope).await.is_none() {
                    return;
                }
                if !owned() {
                    self.invalidate_review(&scope);
                    return;
                }
                let document = match prepared {
                    Ok(Some(document)) => Some(document),
                    Ok(None) => {
                        self.invalidate_review(&scope);
                        return;
                    }
                    // Capacity limits retain the existing bounded read-only view.
                    Err(_) => None,
                };
                batch(|| {
                    self.workspace.editor_documents.update(|documents| {
                        if let Some(document) = document {
                            documents.insert(key.clone(), document);
                        } else {
                            documents.remove(&key);
                        }
                    });
                    self.read_only.set(false);
                    self.workspace.editor_buffers.update(|buffers| {
                        if let Some(buffer) = buffers.get_mut(&key) {
                            buffer.read_only = false;
                        }
                    });
                    update_buffer_source(
                        self.workspace,
                        scope.project.id,
                        &scope.path,
                        source.clone().into(),
                        false,
                    );
                    self.workspace.editor_recovery_checks.update(|checks| {
                        checks.remove(&key);
                    });
                    self.workspace
                        .editor_fold_revision
                        .update(|revision| *revision = revision.wrapping_add(1));
                    self.state.file_review.set(None);
                });
            }
        });
    }
}

async fn read_review_disk(ws: &Workspace, path: &str) -> Result<Option<String>, String> {
    ws.read_optional_bytes(path)
        .await
        .map_err(|error| error.to_string())?
        .map(String::from_utf8)
        .transpose()
        .map_err(|_| "The disk file cannot be reviewed as UTF-8 text".into())
}
