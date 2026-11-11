//! Reload discovery and explicit rollback use the same durable journal and
//! executor as reviewed edits. Draft persistence remains independent.
use super::{
    editor_recovery::RecoveryActions,
    workspace::{EditorRoot, WorkspaceActions},
};
use crate::{
    backend::Api,
    state::{
        auth::AuthState,
        projects::ProjectsState,
        settings::SettingsState,
        ui::UiState,
        workspace::{WorkspaceRecoveryStatus, WorkspaceState},
    },
    workspace::Workspace,
    workspace_transactions::WorkspaceTransactionExecutor,
};
use leptos::{prelude::*, task::spawn_local};
use openwebide_core::{
    Project,
    workspace_transaction::{
        AdvanceWorkspaceTransaction, BeginWorkspaceTransaction, TransactionPhase,
        TransactionProgress, WorkspaceTransactionAction,
    },
};
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};
mod actions;
mod grant;

#[derive(Clone, Copy)]
struct Context {
    api: Api,
    auth: AuthState,
    projects: ProjectsState,
    workspace: WorkspaceState,
    settings: SettingsState,
    recovery: RecoveryActions,
    files: WorkspaceActions,
    tickets: StoredValue<HashMap<i64, u64>>,
    counter: StoredValue<u64>,
    original_folders:
        StoredValue<send_wrapper::SendWrapper<HashMap<i64, web_sys::FileSystemDirectoryHandle>>>,
    folder_access: RwSignal<HashSet<i64>>,
    missing_folders: RwSignal<HashSet<i64>>,
    ui: UiState,
    host: Option<crate::project_host::ProjectHost>,
}
impl Context {
    async fn recovered(
        self,
        id: i64,
        transaction: &str,
        phase: TransactionPhase,
        current: &dyn Fn() -> bool,
    ) -> Result<bool, String> {
        match phase {
            TransactionPhase::Restored => Ok(true),
            TransactionPhase::Committed => {
                let deadline = js_sys::Date::now() + 30_000.0;
                loop {
                    if !current() {
                        return Err(
                            "The workspace changed while checking saved editor files".into()
                        );
                    }
                    if js_sys::Date::now() >= deadline {
                        return Err("Wait for this window’s draft save to finish, then check workspace recovery again.".into());
                    }
                    // Do not repeatedly transfer a full recovery record while
                    // this window is awaiting its own save acknowledgement.
                    if self.recovery.workspace_commit_needs_save_ack(id, None) {
                        crate::util::sleep_ms(10).await;
                        continue;
                    }
                    let saved = self
                        .api
                        .with_value(Clone::clone)
                        .editor_recovery(id)
                        .await
                        .map_err(|error| error.to_string())?;
                    if !current() {
                        return Err(
                            "The workspace changed while checking saved editor files".into()
                        );
                    }
                    // A fetch can have read an older revision before our save
                    // committed, even when its response arrives after our ack.
                    if self
                        .recovery
                        .workspace_commit_needs_save_ack(id, Some(saved.revision))
                    {
                        crate::util::sleep_ms(10).await;
                        continue;
                    }
                    if !self
                        .recovery
                        .acknowledge_workspace_commit(id, transaction, &saved)
                    {
                        self.recovery.publication_failed(id, "The operation completed in another window. Review the saved editor files before continuing.".into());
                    }
                    return Ok(true);
                }
            }
            _ => Ok(false),
        }
    }
    fn reserve(self, project: i64) -> u64 {
        self.counter
            .update_value(|value| *value = value.wrapping_add(1));
        let ticket = self.counter.get_value();
        self.tickets.update_value(|tickets| {
            tickets.insert(project, ticket);
        });
        ticket
    }
    fn current(self, project: &Project, root: &EditorRoot, account: u64, ticket: u64) -> bool {
        self.auth.generation.try_get_untracked() == Some(account)
            && self.auth.user.try_with_untracked(Option::is_some) == Some(true)
            && self
                .tickets
                .try_with_value(|tickets| tickets.get(&project.id) == Some(&ticket))
                == Some(true)
            && self.projects.project(project.id).is_some_and(|project| {
                !root.changed(&EditorRoot::for_project(
                    &project,
                    self.projects,
                    Some(self.settings),
                ))
            })
    }
    fn status(self, project: i64, status: WorkspaceRecoveryStatus) {
        self.workspace.workspace_recovery.try_update(|states| {
            if matches!(
                states.get(&project),
                Some(WorkspaceRecoveryStatus::EditorConflict(_))
            ) {
                return;
            }
            states.insert(project, status);
        });
    }
    fn check(self, id: i64) {
        if self.workspace.editor_operation_active_untracked(id) {
            return;
        }
        let Some(project) = self.projects.project(id) else {
            return;
        };
        let account = self.auth.generation.get_untracked();
        let user = self
            .auth
            .user
            .get_untracked()
            .map(|user| user.id.get())
            .unwrap_or(0);
        let root = EditorRoot::for_project(&project, self.projects, Some(self.settings));
        let ticket = self.reserve(id);
        self.status(id, WorkspaceRecoveryStatus::Checking);
        spawn_local(async move {
            let backend = self.api.with_value(Clone::clone);
            let result=async {
                let workspace=backend.workspace_transaction(id,None).await.map_err(|e|e.to_string())?;
                let action=backend.language_action_request(id,&openwebide_core::plugins::languages::intelligence::actions::ActionJournalRequest::Load {id:None}).await?;
                if !self.current(&project, &root, account, ticket) {
                    return Err("The workspace changed while checking recovery".into());
                }
                if let Some(state) = &workspace
                    && state.journal.progress.phase == TransactionPhase::Committed
                    && self.recovery.has_workspace_publication(id)
                {
                    self.recovered(id, &state.journal.id, TransactionPhase::Committed,
                        &|| self.current(&project, &root, account, ticket)).await?;
                }
                Ok::<_,String>((workspace,action))
            }.await;
            let access = match &result {
                Ok((Some(state), _))
                    if WorkspaceRecoveryStatus::from_state(Some(state)).blocks_saves() =>
                {
                    crate::workspace_transactions::transaction_folder_access(
                        project.mode,
                        user,
                        id,
                        &state.journal.id,
                    )
                    .await
                }
                _ => Ok(crate::workspace_transactions::TransactionFolderAccess::Host),
            };
            if !self.current(&project, &root, account, ticket) {
                return;
            }
            if self.workspace.editor_operation_active_untracked(id)
                || !self.workspace.workspace_recovery.with_untracked(|states| {
                    matches!(states.get(&id), Some(WorkspaceRecoveryStatus::Checking))
                })
            {
                return;
            }
            self.original_folders.update_value(|folders| {
                folders.remove(&id);
                if let Ok(crate::workspace_transactions::TransactionFolderAccess::Retained(
                    original,
                )) = &access
                {
                    folders.insert(id, original.clone());
                }
            });
            self.missing_folders.update(|ids| {
                ids.remove(&id);
                if matches!(
                    access,
                    Ok(crate::workspace_transactions::TransactionFolderAccess::Missing)
                ) {
                    ids.insert(id);
                }
            });
            self.folder_access.update(|ids| {
                ids.remove(&id);
                if self
                    .original_folders
                    .with_value(|folders| folders.contains_key(&id))
                {
                    ids.insert(id);
                }
            });
            self.status(
                id,
                match result {
                    Ok((state, action)) => {
                        let mut status =
                            WorkspaceRecoveryStatus::from_records(state.as_ref(), action.as_ref());
                        if let (Err(error), WorkspaceRecoveryStatus::Pending { error: issue, .. }) =
                            (access, &mut status)
                        {
                            *issue = Some(error);
                        }
                        status
                    }
                    Err(error) => WorkspaceRecoveryStatus::Unavailable(error.to_string()),
                },
            );
        });
    }
    fn grant(self, id: i64) {
        if self.workspace.editor_operation_active_untracked(id)
            || !self.workspace.workspace_recovery.with_untracked(|states| {
                matches!(
                    states.get(&id),
                    Some(WorkspaceRecoveryStatus::Pending {
                        recovering: false,
                        ..
                    })
                )
            })
        {
            return;
        }
        let Some(handle) = self
            .original_folders
            .with_value(|folders| folders.get(&id).cloned())
        else {
            return;
        };
        let Some(project) = self.projects.project(id) else {
            return;
        };
        let account = self.auth.generation.get_untracked();
        let root = EditorRoot::for_project(&project, self.projects, Some(self.settings));
        let ticket = self.reserve(id);
        spawn_local(async move {
            // This is the first await following the user's click: do not put a
            // network/IndexedDB lookup before the browser permission prompt.
            let result = async {
                if !crate::local_fs::request_access(&handle).await? {
                    return Err("Access to the original folder was not granted".into());
                }
                if !self.current(&project, &root, account, ticket) {
                    return Err("The workspace changed while granting access".into());
                }
                let previous = self
                    .projects
                    .local_handles
                    .with_untracked(|handles| handles.get(&id).cloned());
                let handle =
                    crate::local_fs::retain_folder_handle(previous.as_ref(), &handle).await?;
                if !self.current(&project, &root, account, ticket) {
                    return Err("The folder changed while retaining access".into());
                }
                crate::idb::save_handle(id, project.user_id, &handle).await?;
                Ok(handle)
            }
            .await;
            if !self.current(&project, &root, account, ticket) {
                return;
            }
            match result {
                Ok(handle) => {
                    self.projects.local_handles.update(|handles| {
                        handles.insert(id, handle);
                    });
                    self.projects.needs_grant.update(|ids| {
                        ids.remove(&id);
                    });
                    self.check(id);
                }
                Err(error) => {
                    self.workspace.workspace_recovery.try_update(|states| {
                        if let Some(WorkspaceRecoveryStatus::Pending { error: issue, .. }) =
                            states.get_mut(&id)
                        {
                            *issue = Some(error);
                        }
                    });
                }
            }
        });
    }
    fn recover(self, id: i64) {
        let Some(WorkspaceRecoveryStatus::Pending {
            id: transaction,
            phase,
            recovering: false,
            ..
        }) = self
            .workspace
            .workspace_recovery
            .with_untracked(|states| states.get(&id).cloned())
        else {
            return;
        };
        let Some(project) = self.projects.project(id) else {
            return;
        };
        let account = self.auth.generation.get_untracked();
        let user = self
            .auth
            .user
            .get_untracked()
            .map(|user| user.id.get())
            .unwrap_or(0);
        let root = EditorRoot::for_project(&project, self.projects, Some(self.settings));
        let ticket = self.reserve(id);
        let lock = match self.workspace.begin_workspace_edit(id) {
            Ok(lock) => lock,
            Err(error) => {
                self.status(
                    id,
                    WorkspaceRecoveryStatus::Pending {
                        id: transaction,
                        phase,
                        recovering: false,
                        error: Some(error),
                    },
                );
                return;
            }
        };
        self.status(
            id,
            WorkspaceRecoveryStatus::Pending {
                id: transaction.clone(),
                phase,
                recovering: true,
                error: None,
            },
        );
        spawn_local(async move {
            let mut phase = phase;
            let current: Rc<dyn Fn() -> bool> =
                Rc::new(move || self.current(&project, &root, account, ticket));
            let result = async {
                lock.wait_for_saves(|| current()).await?;
                let backend = self.api.with_value(Clone::clone);
                let state = backend
                    .workspace_transaction(id, Some(&transaction))
                    .await
                    .map_err(|error| error.to_string())?
                    .ok_or("The interrupted operation is unavailable")?;
                phase = state.journal.progress.phase;
                if !current() {
                    return Err("The workspace changed before recovery".into());
                }
                // No primitive was ever started. Cancel using database CAS,
                // without requiring a folder grant or touching external edits.
                if state.journal.progress.phase == TransactionPhase::Prepared {
                    backend
                        .advance_workspace_transaction(
                            id,
                            &AdvanceWorkspaceTransaction {
                                id: transaction.clone(),
                                generation: state.journal.generation.clone(),
                                revision: state.journal.revision,
                                progress: TransactionProgress::prepared_cancellation(),
                            },
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    return Ok(());
                }
                if self
                    .recovered(
                        id,
                        &state.journal.id,
                        state.journal.progress.phase,
                        &*current,
                    )
                    .await?
                {
                    return Ok(());
                }
                let workspace = Workspace::for_project(self.api, self.projects, id)
                    .ok_or("Grant access to the original project folder before recovery")?;
                let executor =
                    WorkspaceTransactionExecutor::select(backend.clone(), workspace, user, id)?
                        .with_host(self.host);
                let generation = state.journal.generation.clone();
                let outcome = executor
                    .execute(
                        BeginWorkspaceTransaction {
                            language_review: state.language_review,
                            id: transaction.clone(),
                            plan: state.journal.plan,
                            editor: state.editor,
                        },
                        generation.clone(),
                        WorkspaceTransactionAction::Recover,
                        current.clone(),
                    )
                    .await;
                // A missing reply must never be treated as a failed rollback.
                let state = backend
                    .workspace_transaction(id, Some(&transaction))
                    .await
                    .map_err(|error| error.to_string())?;
                if state.as_ref().is_some_and(|state| state.journal.generation != generation) {
                    return Err("Workspace recovery identity changed during recovery; reload before continuing".into());
                }
                if let Some(state) = &state {
                    phase = state.journal.progress.phase;
                }
                if !current() {
                    return Err("The workspace changed during recovery".into());
                }
                if let Some(state) = &state
                    && self
                        .recovered(
                            id,
                            &state.journal.id,
                            state.journal.progress.phase,
                            &*current,
                        )
                        .await?
                {
                    return Ok(());
                }
                Err(match outcome {
                    Ok(outcome) => outcome.error.map_or_else(
                        || {
                            "Recovery remains pending. Reconnect the original folder and retry."
                                .into()
                        },
                        |error| error.message,
                    ),
                    Err(error) => error,
                })
            }
            .await;
            drop(lock);
            if !current() {
                return;
            }
            match result {
                Ok(()) => {
                    self.check(id);
                    self.folder_access.update(|ids| {
                        ids.remove(&id);
                    });
                    self.original_folders.update_value(|folders| {
                        folders.remove(&id);
                    });
                    self.missing_folders.update(|ids| {
                        ids.remove(&id);
                    });
                    self.recovery.check_files.run(id);
                    if self.projects.active_project.get_untracked() == Some(id) {
                        self.files.refresh_tree.run(());
                    }
                }
                Err(error) => self.status(
                    id,
                    WorkspaceRecoveryStatus::Pending {
                        id: transaction,
                        phase,
                        recovering: false,
                        error: Some(error),
                    },
                ),
            }
        });
    }
}

#[derive(Clone, Copy)]
pub struct WorkspaceRecoveryActions {
    pub check: Callback<i64>,
    pub recover: Callback<i64>,
    pub grant: Callback<i64>,
    pub folder_access: RwSignal<HashSet<i64>>,
    pub missing_folders: RwSignal<HashSet<i64>>,
    pub choose_folder: Callback<i64>,
    pub check_command: Callback<i64>,
    pub stop_command: Callback<i64>,
    pub acknowledge_command: Callback<i64>,
    pub discard_command: Callback<i64>,
}
impl WorkspaceRecoveryActions {
    pub fn install(
        api: Api,
        auth: AuthState,
        projects: ProjectsState,
        workspace: WorkspaceState,
    ) -> Self {
        let ctx = Context {
            api,
            auth,
            projects,
            workspace,
            settings: expect_context(),
            recovery: expect_context(),
            files: expect_context(),
            tickets: StoredValue::new(HashMap::new()),
            counter: StoredValue::new(0),
            original_folders: StoredValue::new(send_wrapper::SendWrapper::new(HashMap::new())),
            folder_access: RwSignal::new(HashSet::new()),
            missing_folders: RwSignal::new(HashSet::new()),
            ui: expect_context(),
            host: use_context(),
        };
        let account = StoredValue::new(auth.generation.get_untracked());
        let recovery_phase = Memo::new(move |_| {
            projects
                .active_project
                .get()
                .and_then(|id| ctx.recovery.phase(id))
        });
        Effect::new(move |_| {
            recovery_phase.track();
            let generation = auth.generation.get();
            if account.get_value() != generation {
                account.set_value(generation);
                ctx.tickets.set_value(HashMap::new());
                ctx.original_folders
                    .set_value(send_wrapper::SendWrapper::new(HashMap::new()));
                ctx.folder_access.set(HashSet::new());
                ctx.missing_folders.set(HashSet::new());
                workspace.workspace_recovery.set(HashMap::new());
            }
            projects.projects.track();
            projects.local_handles.track();
            ctx.settings.bridge_url.track();
            if auth.user.get().is_some()
                && projects.projects_loaded.get()
                && let Some(id) = projects.active_project.get()
            {
                ctx.check(id);
            }
        });
        let actions = Self {
            check: Callback::new(move |id| ctx.check(id)),
            recover: Callback::new(move |id| ctx.recover(id)),
            grant: Callback::new(move |id| ctx.grant(id)),
            folder_access: ctx.folder_access,
            missing_folders: ctx.missing_folders,
            choose_folder: Callback::new(move |id| ctx.choose_folder(id)),
            check_command: Callback::new(move |id| {
                ctx.command(id, actions::CommandRecovery::Check);
            }),
            stop_command: Callback::new(move |id| ctx.command(id, actions::CommandRecovery::Stop)),
            acknowledge_command: Callback::new(move |id| {
                ctx.command(id, actions::CommandRecovery::Acknowledge);
            }),
            discard_command: Callback::new(move |id| {
                ctx.command(id, actions::CommandRecovery::Discard);
            }),
        };
        provide_context(actions);
        actions
    }
}
