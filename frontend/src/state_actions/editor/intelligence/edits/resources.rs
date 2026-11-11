//! One review and publication flow for both filesystem runtimes.
use super::*;
use crate::{
    state::{
        auth::AuthState,
        projects::ProjectsState,
        settings::SettingsState,
        ui::UiState,
        workspace::{EditorRecoveryGuard, PreparedEditorRecovery},
    },
    state_actions::{editor_recovery::RecoveryActions, workspace::EditorRoot},
    workspace::Workspace,
    workspace_transactions::WorkspaceTransactionExecutor,
};
use openwebide_core::{
    FileDiff, Project,
    editor::{EditorRecovery, EditorRecoveryRecord},
    plugins::languages::intelligence::{
        actions::{ActionApproval, ActionReview, ServerCommand},
        results::WorkspaceChange,
        workspace_edit::{WorkspaceDocument, WorkspaceDraft},
    },
    workspace_transaction::{
        BeginWorkspaceTransaction, EditorRecoveryPatch, FileOperation, TransactionPhase,
        WorkspaceTransactionAction,
    },
};
use std::{
    collections::{BTreeMap, HashMap},
    rc::Rc,
};
mod commands;
mod publication;

#[derive(Clone, Copy)]
pub(crate) struct ResourceContext {
    api: Api,
    projects: ProjectsState,
    auth: AuthState,
    settings: SettingsState,
    recovery: RecoveryActions,
    ui: UiState,
    host: Option<crate::project_host::ProjectHost>,
    #[cfg(test)]
    legacy_review: bool,
}
impl ResourceContext {
    pub(crate) fn from_context() -> Option<Self> {
        Some(Self {
            api: use_context()?,
            projects: use_context()?,
            auth: use_context()?,
            settings: use_context()?,
            recovery: use_context()?,
            ui: use_context()?,
            host: use_context(),
            #[cfg(test)]
            legacy_review: false,
        })
    }
    fn root_current(self, project: &Project, root: &EditorRoot, account: u64) -> bool {
        self.auth.generation.try_get_untracked() == Some(account)
            && self.projects.project(project.id).is_some_and(|project| {
                !root.changed(&EditorRoot::for_project(
                    &project,
                    self.projects,
                    Some(self.settings),
                ))
            })
    }
    pub(in crate::state_actions::editor::intelligence) async fn prepare_files(
        self,
        snapshot: &Snapshot,
    ) -> Result<Arc<crate::workspace_transactions::review::WorkspaceReviewFiles>, String> {
        let project = self
            .projects
            .project(snapshot.presentation.project)
            .ok_or("The selected project is unavailable")?;
        let root = EditorRoot::for_project(&project, self.projects, Some(self.settings));
        let account = snapshot.presentation.account;
        let observed_project = project.clone();
        let current: Rc<dyn Fn() -> bool> =
            Rc::new(move || self.root_current(&observed_project, &root, account));
        if !current() {
            return Err("The selected folder changed".into());
        }
        let user = self
            .auth
            .user
            .get_untracked()
            .ok_or("Sign in before requesting workspace actions")?
            .id
            .get();
        let executor = WorkspaceTransactionExecutor::select(
            self.api.with_value(Clone::clone),
            Workspace::for_project(self.api, self.projects, project.id)
                .ok_or("The selected folder is unavailable")?,
            user,
            project.id,
        )?
        .with_host(self.host);
        let id = transaction_id()?;
        #[cfg(test)]
        let files = if self.legacy_review {
            executor
                .legacy_review_files(
                    &id,
                    Workspace::for_project(self.api, self.projects, project.id)
                        .ok_or("The selected folder is unavailable")?,
                )
                .await?
        } else {
            executor.review_files(id, current.clone()).await?
        };
        #[cfg(not(test))]
        let files = executor.review_files(id, current.clone()).await?;
        if !current() {
            return Err("The selected folder changed during action preparation".into());
        }
        Ok(Arc::new(files))
    }
    fn discard(self, review: Arc<WorkspaceResourceReview>) {
        // Cleanup retains only authority identifiers, never the preview's large
        // document/filesystem snapshots while waiting for a network response.
        let (account, user, project, id) = (
            review.snapshot.presentation.account,
            review.user,
            review.project.id,
            review.id.clone(),
        );
        self.discard_identity(account, user, project, id);
    }
    fn discard_identity(self, account: u64, user: i64, project: i64, id: String) {
        spawn_local(async move {
            if self.auth.generation.try_get_untracked() != Some(account) {
                return;
            }
            if self
                .api
                .with_value(Clone::clone)
                .workspace_transaction(project, Some(&id))
                .await
                .is_ok_and(|state| state.is_none())
                && self.auth.generation.try_get_untracked() == Some(account)
            {
                let _ = crate::idb::delete_transaction_root(user, project, &id).await;
            }
        });
    }
}
pub struct WorkspaceResourceReview {
    pub operations: Vec<String>,
    pub previews: Vec<FileDiff>,
    id: String,
    user: i64,
    project: Project,
    root: send_wrapper::SendWrapper<EditorRoot>,
    snapshot: Snapshot,
    guard: EditorRecoveryGuard,
    before: EditorRecovery,
    draft: WorkspaceDraft,
    action: Option<ActionApproval>,
    analysis: Arc<AnalysisResult>,
}
pub(crate) fn clear_workspace_review(
    review: RwSignal<Option<Arc<WorkspaceTextReview>>>,
    resources: RwSignal<Option<Arc<WorkspaceResourceReview>>>,
    epoch: StoredValue<u64>,
    context: Option<ResourceContext>,
) {
    epoch.try_update_value(|epoch| *epoch = epoch.wrapping_add(1));
    if let Some(previous) = resources.try_get_untracked().flatten()
        && let Some(context) = context
    {
        context.discard(previous);
    }
    review.try_set(None);
    resources.try_set(None);
}
fn transaction_id() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    web_sys::window()
        .ok_or("The browser is unavailable")?
        .crypto()
        .map_err(|_| "Secure workspace identity is unavailable")?
        .get_random_values_with_u8_array(&mut bytes)
        .map_err(|_| "Could not create a workspace review identity")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
fn operation_label(operation: &FileOperation) -> String {
    match operation {
        FileOperation::Create {
            path, overwrite, ..
        } => format!(
            "Create {path}{}",
            if *overwrite {
                " (overwrite existing file)"
            } else {
                ""
            }
        ),
        FileOperation::Rename {
            from,
            to,
            overwrite,
            ..
        } => format!(
            "Move {from} → {to}{}",
            if *overwrite {
                " (overwrite destination)"
            } else {
                ""
            }
        ),
        FileOperation::Delete {
            path, recursive, ..
        } => format!(
            "Delete {path}{}",
            if *recursive { " and its contents" } else { "" }
        ),
    }
}
impl WorkspaceResourceReview {
    pub fn command(&self) -> Option<&ServerCommand> {
        self.action
            .as_ref()
            .and_then(|action| action.review.command.as_ref())
    }
    fn after_recovery(&self) -> Result<EditorRecovery, String> {
        self.draft.recovery(&self.before)
    }
    fn patch(
        &self,
        record: &EditorRecoveryRecord,
        after: &EditorRecovery,
    ) -> Result<EditorRecoveryPatch, String> {
        self.draft.recovery_patch(record, after)
    }
}
impl EditorIntelligence {
    pub fn prepare_action(self, review: ActionReview) {
        if review.command.is_none() {
            self.prepare_workspace(review.edit);
            return;
        }
        let Some(analysis) = self.analysis.get_value() else {
            self.error.set(Some(
                "Request a fresh action review before applying it".into(),
            ));
            return;
        };
        let (input, provider) = match analysis.action(&review) {
            Ok(action) => action,
            Err(message) => {
                self.error.set(Some(message));
                return;
            }
        };
        clear_workspace_review(
            self.review,
            self.resources,
            self.review_epoch,
            self.resource_context,
        );
        self.prepare_resource_workspace(
            review.edit.clone(),
            Some((input, review, provider)),
            analysis,
        );
    }
    pub(super) fn prepare_resource_workspace(
        self,
        edit: WorkspaceEdit,
        action: Option<(protocol::Request, ActionReview, RegisteredLanguage)>,
        analysis: Arc<AnalysisResult>,
    ) {
        let Some(context) = self.resource_context else {
            self.error.set(Some(
                "Editor recovery is unavailable for file-operation review".into(),
            ));
            return;
        };
        let snapshot = analysis.snapshot.clone();
        if !self.snapshot_current(&snapshot) {
            return;
        }
        let files = match analysis.workspace_files() {
            Ok(files) => files,
            Err(message) => {
                self.error.set(Some(message));
                return;
            }
        };
        let Some(project) = context.projects.project(snapshot.presentation.project) else {
            return;
        };
        if Workspace::for_project(context.api, context.projects, project.id).is_none() {
            self.error.set(Some(
                "Grant folder access before reviewing file operations".into(),
            ));
            return;
        }
        let root = EditorRoot::for_project(&project, context.projects, Some(context.settings));
        let user = context
            .auth
            .user
            .get_untracked()
            .map(|user| user.id.get())
            .unwrap_or(0);
        let epoch = self.review_epoch.get_value();
        self.error.set(None);
        self.busy.set(true);
        spawn_local(async move {
            let observed_snapshot = snapshot.clone();
            let observed_project = project.clone();
            let observed_root = root.clone();
            let observe: Rc<dyn Fn() -> bool> = Rc::new(move || {
                self.review_epoch.try_get_value() == Some(epoch)
                    && self.snapshot_current(&observed_snapshot)
                    && context.root_current(
                        &observed_project,
                        &observed_root,
                        observed_snapshot.presentation.account,
                    )
            });
            let current = || observe();
            let mut captured_identity = None;
            let result = async {
                let id = files.id().to_owned();
                captured_identity = Some(id.clone());
                files.retain_root(observe.clone()).await?;
                if !current() {
                    return Err("Workspace review was superseded".into());
                }
                let before = self
                    .editor
                    .workspace
                    .editor_recovery_cooperative(
                        &project,
                        || context.recovery.file_read_only(),
                        current,
                    )
                    .await?
                    .ok_or("The editor changed while preparing file-operation review")?;
                let guard = self
                    .editor
                    .workspace
                    .recovery_guard_from_state(project.id, before.clone());
                let mut documents = BTreeMap::new();
                for file in &before.files {
                    if let Some(saved) = &file.document {
                        let document = self
                            .editor
                            .workspace
                            .editor_documents
                            .with_untracked(|documents| {
                                documents
                                    .get(&(project.id, file.path.clone()))
                                    .filter(|document| document.matches_recovery(saved))
                                    .cloned()
                            })
                            .map(Ok)
                            .unwrap_or_else(|| saved.restore())?;
                        let mut document = WorkspaceDocument::new(&file.path, document);
                        document.read_only = file.read_only;
                        documents.insert(file.path.clone(), document);
                    }
                }
                let changes = edit.ordered()?;
                let operations = changes
                    .iter()
                    .filter_map(|change| match change {
                        WorkspaceChange::File { operation } => Some(operation.clone()),
                        WorkspaceChange::Text { .. } => None,
                    })
                    .collect::<Vec<_>>();
                let labels = operations.iter().map(operation_label).collect();
                let mut draft = files.prepare(
                    operations,
                    documents,
                    before.files.iter().map(|file| file.path.clone()).collect(),
                    before.selected.clone(),
                    current,
                )
                .await?;
                for change in changes {
                    if !current() {
                        return Err("Workspace review was superseded".into());
                    }
                    match change {
                        WorkspaceChange::File { operation } => draft.resource(operation)?,
                        WorkspaceChange::Text { file } => {
                            draft.document(&*files, &file.path, current).await?;
                            draft.text(file)?;
                        }
                    }
                }
                draft.finish()?;
                let action=if let Some((input,review,provider))=action {
                    let text_paths=review.edit.clone().ordered()?.into_iter().filter_map(|change|match change {WorkspaceChange::Text {file}=>Some(file.path),WorkspaceChange::File {..}=>None}).collect::<Vec<_>>();
                    for (path,document) in &draft.documents {
                        let original=document.original_path.as_deref().unwrap_or(path);
                        let served_before=snapshot.service.registry.server_document(&provider,original,document.document.text(),&snapshot.service.preferences)?.is_some();
                        if document.document.is_dirty() && (served_before || text_paths.iter().any(|affected|affected==path || affected==original))
                            && snapshot.service.registry.server_document(&provider,path,document.document.text(),&snapshot.service.preferences)?.is_none() {
                            return Err(format!("{path} cannot be synchronized to this command's server; no part of the action was applied"));
                        }
                    }
                    let project_for_guard=project.clone();
                    let root_for_guard=root.clone();
                    let account=snapshot.presentation.account;
                    let client=crate::plugin_actions::project_language_actions(context.api,context.host.ok_or("The language execution host is unavailable")?,project.id,snapshot.service.preferences.clone(),move ||context.root_current(&project_for_guard,&root_for_guard,account)).await?;
                    Some(client.prepare(id.clone(),&provider.namespace,input,review).await?)
                } else {None};
                if !self.editor.workspace.editor_recovery_guard_matches(
                    &project,
                    &guard,
                    context.recovery.file_read_only(),
                ) {
                    return Err("Editor files changed while preparing the review".into());
                }
                let mut previews = Vec::new();
                for (path, document) in &draft.documents {
                    let old = document
                        .original_path
                        .as_ref()
                        .and_then(|path| draft.before_documents.get(path))
                        .map(|document| document.document.text().to_string());
                    if old.as_deref() != Some(document.document.text())
                        || document.original_path.as_deref() != Some(path)
                    {
                        previews.push(FileDiff {
                            path: path.clone(),
                            old,
                            new: document.document.text().into(),
                            old_unavailable: false,
                            backup_path: None,
                        });
                    }
                }
                for (path, document) in &draft.before_documents {
                    if !draft
                        .documents
                        .values()
                        .any(|after| after.original_path.as_ref() == Some(path))
                    {
                        previews.push(FileDiff {
                            path: path.clone(),
                            old: Some(document.document.text().into()),
                            new: String::new(),
                            old_unavailable: false,
                            backup_path: None,
                        });
                    }
                }
                Ok::<_, String>(WorkspaceResourceReview {
                    operations: labels,
                    previews,
                    id,
                    user,
                    project: project.clone(),
                    root: send_wrapper::SendWrapper::new(root.clone()),
                    snapshot: snapshot.clone(),
                    guard,
                    before,
                    draft,
                    action,
                    analysis,
                })
            }
            .await;
            if (result.is_err() || !current())
                && let Some(id) = captured_identity
            {
                context.discard_identity(snapshot.presentation.account, user, project.id, id);
            }
            if self.review_epoch.try_get_value() != Some(epoch) {
                return;
            }
            self.busy.try_set(false);
            if !current() {
                return;
            }
            match result {
                Ok(review) => self.resources.set(Some(Arc::new(review))),
                Err(error) => self.error.set(Some(error)),
            }
        });
    }
    pub fn apply_resource_workspace(
        self,
        review: Arc<WorkspaceResourceReview>,
        read_only: Signal<bool>,
    ) {
        if read_only.get_untracked() || self.busy.get_untracked() {
            return;
        }
        let Some(context) = self.resource_context else {
            return;
        };
        if !self.resources.with_untracked(|current| {
            current
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &review))
        }) {
            return;
        }
        if self
            .editor
            .workspace
            .workspace_saves_blocked(review.project.id)
        {
            self.error.set(Some(
                "Resolve workspace recovery before applying another operation".into(),
            ));
            return;
        }
        let current = {
            let review = review.clone();
            Rc::new(move || {
                self.resources
                    .try_get_untracked()
                    .flatten()
                    .is_some_and(|current| Arc::ptr_eq(&current, &review))
                    && self.snapshot_current(&review.snapshot)
                    && context.root_current(
                        &review.project,
                        &review.root,
                        review.snapshot.presentation.account,
                    )
            }) as Rc<dyn Fn() -> bool>
        };
        if !current() || self.editor.is_composing() {
            self.error.set(Some(
                "Finish editing and request a fresh workspace review".into(),
            ));
            return;
        }
        let lock = match self
            .editor
            .workspace
            .begin_workspace_edit(review.project.id)
        {
            Ok(lock) => lock,
            Err(error) => {
                self.error.set(Some(error));
                return;
            }
        };
        self.editor.cancel_native_text();
        self.editor.typing.set(None);
        self.error.set(None);
        self.busy.set(true);
        spawn_local(async move {
            let mut lock = Some(lock);
            let operation = async {
                lock.as_ref().ok_or("The workspace operation lost its editor lock")?.wait_for_saves(|| current()).await?;
                let mut pause = context.recovery.pause_and_flush(review.project.id, current.clone()).await?;
                if !self.editor.workspace.editor_recovery_guard_matches(&review.project, &review.guard, context.recovery.file_read_only()) { return Err("Editor files changed since review; request the operation again".into()); }
                let after = review.after_recovery()?;
                let patch = review.patch(pause.record(), &after)?;
                if patch.apply_to(&pause.record().state)? != after { return Err("The editor recovery patch no longer matches the review".into()); }
                let documents = review.draft.documents.iter().map(|(path, document)| ((review.project.id, path.clone()), document.document.clone())).collect::<HashMap<_, _>>();
                let prepared = PreparedEditorRecovery::from_documents(review.project.id, &after, documents);
                let executor = WorkspaceTransactionExecutor::select(context.api.with_value(Clone::clone), Workspace::for_project(context.api, context.projects, review.project.id).ok_or("The selected folder is unavailable")?, review.user, review.project.id)?.with_host(context.host)
                    .with_review_files(review.analysis.workspace_files()?);
                let language_review = if review.draft.plan.version == 2 {
                    Some(Box::new(review.analysis.language_review(&review.draft.plan, &patch)?))
                } else { None };
                let begin=BeginWorkspaceTransaction {
                    language_review,id:review.id.clone(),plan:review.draft.plan.clone(),editor:patch};
                review.analysis.validate().await?;
                if !current() {return Err("Workspace review changed during analysis validation".into());}
                if !self.editor.workspace.editor_recovery_guard_matches(&review.project, &review.guard, context.recovery.file_read_only()) {
                    return Err("Editor files changed during analysis validation; request the operation again".into());
                }
                self.editor.workspace.workspace_recovery.try_update(|states| { states.insert(review.project.id, crate::state::workspace::WorkspaceRecoveryStatus::Pending { id: review.id.clone(), phase:TransactionPhase::Prepared, recovering: true, error: None }); });
                let identity=executor.prepare(&begin).await?;
                let action=if let Some(approval)=&review.action {
                    let client=commands::client(context,&review).await?;
                    let approved=client.approve(approval).await?;
                    commands::prepare(self,context,&review,&approved).await?;
                    Some((client,approved))
                } else {None};
                let result = executor.execute(begin, identity.generation.clone(), WorkspaceTransactionAction::Apply, current.clone()).await;
                let state = context.api.with_value(Clone::clone).workspace_transaction(review.project.id, Some(&review.id)).await.map_err(|e| e.to_string())?;
                if state.as_ref().is_some_and(|state| state.journal.generation != identity.generation) {
                    return Err("Workspace recovery identity changed before editor publication; reload before continuing".into());
                }
                if context.root_current(&review.project, &review.root, review.snapshot.presentation.account) {
                    self.editor.workspace.workspace_recovery.try_update(|states| {
                        let mut status=crate::state::workspace::WorkspaceRecoveryStatus::from_records(state.as_ref(),action.as_ref().map(|(_,record)|record));
                        if let crate::state::workspace::WorkspaceRecoveryStatus::LanguageAction {recovering,..}=&mut status {*recovering=true;}
                        states.insert(review.project.id,status);
                    });
                }
                let committed=state.as_ref().is_some_and(|state|state.journal.progress.phase==TransactionPhase::Committed);
                let decided=state.as_ref().is_some_and(|state|state.journal.progress.commits_editor());
                let issue=||match result {Ok(outcome)=>outcome.error.map_or_else(||"Workspace operation is pending recovery".into(),|error|error.message),Err(error)=>error};
                if !decided {return Err(issue());}
                self.publish_resource_decision(&review,state.as_ref().unwrap(),&mut pause,prepared,&after,current.as_ref()).await?;
                drop(pause);
                if !committed {return Ok(Some(issue()));}
                if let Some((client,approved))=&action {commands::complete(self,context,&review,client,approved,&after,&mut lock).await?;}
                Ok::<_,String>(None)
            }.await;
            drop(lock);
            if review.action.is_some() {
                let issue = match &operation {
                    Err(error) | Ok(Some(error)) => Some(error.clone()),
                    Ok(None) => None,
                };
                commands::refresh(context, self, &review, issue).await;
            }
            let owns_review = self
                .resources
                .try_get_untracked()
                .flatten()
                .is_some_and(|current| Arc::ptr_eq(&current, &review));
            if owns_review {
                self.busy.try_set(false);
            }
            match operation {
                Ok(pending) => {
                    if owns_review {
                        clear_workspace_review(
                            self.review,
                            self.resources,
                            self.review_epoch,
                            self.resource_context,
                        );
                        self.result.try_set(None);
                    }
                    if self.editor.workspace.active_project.get_untracked()
                        == Some(review.project.id)
                    {
                        if let Some(actions) = self.workspace_actions {
                            actions.refresh_tree.run(());
                        }
                        if pending.is_none() {
                            self.request
                                .run(IntelligenceCommand::Query(Query::Synchronize));
                        }
                    }
                    if let Some(error) = pending
                        && owns_review
                        && context.root_current(
                            &review.project,
                            &review.root,
                            review.snapshot.presentation.account,
                        )
                    {
                        self.editor
                            .workspace
                            .workspace_recovery
                            .try_update(|states| {
                                if let Some(
                                    crate::state::workspace::WorkspaceRecoveryStatus::Pending {
                                        id,
                                        error: issue,
                                        ..
                                    },
                                ) = states.get_mut(&review.project.id)
                                    && id == &review.id
                                {
                                    *issue = Some(error.clone());
                                }
                            });
                        self.error.try_set(Some(error.clone()));
                        context.ui.notify(error);
                    }
                }
                Err(error) => {
                    if context.root_current(
                        &review.project,
                        &review.root,
                        review.snapshot.presentation.account,
                    ) {
                        self.editor
                            .workspace
                            .workspace_recovery
                            .try_update(|states| {
                                if let Some(
                                    crate::state::workspace::WorkspaceRecoveryStatus::Pending {
                                        id,
                                        recovering,
                                        error: pending_error,
                                        ..
                                    },
                                ) = states.get_mut(&review.project.id)
                                    && id == &review.id
                                {
                                    *recovering = false;
                                    *pending_error = Some(error.clone());
                                }
                            });
                    }
                    if current() && owns_review {
                        self.error.try_set(Some(error.clone()));
                        context.ui.notify(error);
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests;
