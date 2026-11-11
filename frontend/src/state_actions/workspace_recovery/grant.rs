//! A lost browser capability requires an explicit folder choice and confirmation.
//! Matching recorded contents is a preflight, never automatic authorization.
use super::*;
use crate::state::ui::ConfirmRequest;
use openwebide_core::workspace_transaction::Snapshot;
use std::sync::Arc;

struct FolderGrantReview {
    project: Project,
    root: EditorRoot,
    account: u64,
    ticket: u64,
    user: i64,
    id: String,
    generation: String,
    revision: u64,
    snapshot: Option<Arc<Snapshot>>,
    handle: web_sys::FileSystemDirectoryHandle,
}
impl Context {
    fn grant_error(self, id: i64, error: String) {
        self.workspace.workspace_recovery.try_update(|states| {
            if let Some(WorkspaceRecoveryStatus::Pending { error: issue, .. }) = states.get_mut(&id)
            {
                *issue = Some(error.clone());
            }
        });
        self.ui.notify(error);
    }
    pub(super) fn choose_folder(self, id: i64) {
        if self.workspace.editor_operation_active_untracked(id)
            || !self.missing_folders.with_untracked(|ids| ids.contains(&id))
        {
            return;
        }
        let Some(WorkspaceRecoveryStatus::Pending {
            id: transaction,
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
        spawn_local(async move {
            let checked_project = project.clone();
            let checked_root = root.clone();
            let current: Rc<dyn Fn() -> bool> =
                Rc::new(move || self.current(&checked_project, &checked_root, account, ticket));
            let result = async {
                // Preserve the browser's activation from this explicit click.
                let Some(handle) = crate::local_fs::pick_directory().await? else { return Ok(None); };
                if !current() { return Err("The folder choice was superseded".into()); }
                let state = Arc::new(self.api.with_value(Clone::clone).workspace_transaction(id,Some(&transaction)).await.map_err(|e|e.to_string())?.ok_or("The interrupted operation is unavailable")?);
                if !current() { return Err("The workspace changed while loading recovery".into()); }
                let executor = WorkspaceTransactionExecutor::select(self.api.with_value(Clone::clone), Workspace::Local { handle: handle.clone() }, user, id)?.with_host(self.host);
                let snapshot = executor.review_folder(state.clone(), current.clone()).await?;
                if !current() { return Err("The folder review changed".into()); }
                let paths = state.journal.plan.changed_paths();
                let shown = paths.iter().take(12).copied().collect::<Vec<_>>().join(", ");
                let remaining = if paths.len() > 12 { format!(" and {} more paths",paths.len() - 12) } else { String::new() };
                let details = if snapshot.is_some() {
                    format!("Its affected files match the interrupted operation ({shown}{remaining}). Granting access does not restore files; use Restore interrupted changes afterward.")
                } else {
                    "The companion and browser verified the original physical folder. The operation’s editor result is already saved. Granting access permits Finish workspace recovery to complete cleanup while preserving later drafts.".into()
                };
                let message = format!("Use ‘{}’ as the recovery folder for ‘{}’? Only choose the original project folder. {details}",handle.name(),project.name);
                let review = FolderGrantReview { project: project.clone(), root: root.clone(), account, ticket, user, id: transaction.clone(), generation: state.journal.generation.clone(), revision: state.journal.revision, snapshot: snapshot.map(Arc::new), handle };
                Ok::<_,String>(Some((message,review)))
            }.await;
            if !current() {
                return;
            }
            match result {
                Ok(Some((message, review))) => {
                    if self.ui.confirm.try_with_untracked(Option::is_some) != Some(false) {
                        self.grant_error(
                            id,
                            "Finish the open confirmation before choosing a recovery folder".into(),
                        );
                        return;
                    }
                    let review = Arc::new(send_wrapper::SendWrapper::new(review));
                    self.ui.confirm.set(Some(ConfirmRequest {
                        title: "Grant recovery folder".into(),
                        message,
                        confirm_label: "Grant this folder".into(),
                        action: Callback::new(move |()| self.approve_folder(review.clone())),
                    }));
                }
                Ok(None) => (),
                Err(error) => self.grant_error(id, error),
            }
        });
    }
    fn approve_folder(self, review: Arc<send_wrapper::SendWrapper<FolderGrantReview>>) {
        if !self.current(&review.project, &review.root, review.account, review.ticket) {
            return;
        }
        let id = review.project.id;
        let lock = match self.workspace.begin_workspace_edit(id) {
            Ok(lock) => lock,
            Err(error) => {
                self.grant_error(id, error);
                return;
            }
        };
        spawn_local(async move {
            let current: Rc<dyn Fn() -> bool> = {
                let review = review.clone();
                Rc::new(move || {
                    self.current(&review.project, &review.root, review.account, review.ticket)
                })
            };
            let result = async {
                lock.wait_for_saves(|| current()).await?;
                let executor = WorkspaceTransactionExecutor::select(
                    self.api.with_value(Clone::clone),
                    Workspace::Local {
                        handle: review.handle.clone(),
                    },
                    review.user,
                    id,
                )?
                .with_host(self.host);
                executor
                    .grant_reviewed_folder(
                        review.id.clone(),
                        review.generation.clone(),
                        review.revision,
                        review.snapshot.clone(),
                        current.clone(),
                    )
                    .await?;
                if !current() {
                    return Err("The workspace changed during the folder grant".into());
                }
                let previous = self
                    .projects
                    .local_handles
                    .with_untracked(|handles| handles.get(&id).cloned());
                let handle =
                    crate::local_fs::retain_folder_handle(previous.as_ref(), &review.handle)
                        .await?;
                if !current() {
                    return Err("The folder changed while retaining access".into());
                }
                crate::idb::save_handle(id, review.project.user_id, &handle).await?;
                Ok::<_, String>(handle)
            }
            .await;
            drop(lock);
            if !current() {
                return;
            }
            match result {
                Ok(handle) => {
                    batch(|| {
                        self.projects.local_handles.update(|handles| {
                            handles.insert(id, handle);
                        });
                        self.projects.needs_grant.update(|ids| {
                            ids.remove(&id);
                        });
                    });
                    self.check(id);
                }
                Err(error) => self.grant_error(id, error),
            }
        });
    }
}
