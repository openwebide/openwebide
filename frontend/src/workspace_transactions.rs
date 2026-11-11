//! Runtime selection and primitives. Planning, retry/recovery dispatch and every
//! filesystem mutation remain in the shared core transaction engine.
use crate::{
    backend::Backend, local_fs::BrowserFsaVfs, workspace::Workspace,
    workspace_journal::BackendWorkspaceJournal,
};
use futures::channel::oneshot;
use openwebide_core::workspace_transaction::{
    self as transaction, BeginWorkspaceTransaction, ExecuteWorkspaceTransaction, JournalRecord,
    VfsTransactionFiles, WorkspaceJournal, WorkspaceTransactionAction, WorkspaceTransactionOutcome,
};
use std::{cell::RefCell, future::Future, rc::Rc};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise, spawn_local};
use web_sys::FileSystemDirectoryHandle;

pub mod companion;
pub mod inspection;
pub mod review;

#[wasm_bindgen(inline_js = r#"
export function workspaceExecutionLock(name, work) {
    if (!navigator.locks) return Promise.reject(new Error('This browser cannot coordinate workspace recovery across windows'));
    return navigator.locks.request(name, {mode:'exclusive'}, () => work());
}
"#)]
extern "C" {
    fn workspaceExecutionLock(name: &str, work: &js_sys::Function) -> js_sys::Promise;
}

/// Project IDs are database-global, including across accounts. Sharing this
/// key prevents a signed-out window from releasing another account's I/O early.
pub(crate) fn workspace_lock_name(project: i64) -> String {
    format!("openwebide-workspace:{project}")
}

/// Own the complete critical section independently of its caller's future.
/// The browser keeps this Web Lock across all awaited filesystem/journal work.
pub async fn with_workspace_lock<T: 'static>(
    name: String,
    work: impl Future<Output = Result<T, String>> + 'static,
) -> Result<T, String> {
    let (sender, receiver) = oneshot::channel();
    spawn_local(async move {
        let sender = Rc::new(RefCell::new(Some(sender)));
        let completion = sender.clone();
        let callback = Closure::once(move || {
            future_to_promise(async move {
                let result = work.await;
                if let Some(sender) = completion.borrow_mut().take() {
                    let _ = sender.send(result);
                }
                Ok(JsValue::UNDEFINED)
            })
        });
        let result = JsFuture::from(workspaceExecutionLock(
            &name,
            callback.as_ref().unchecked_ref(),
        ))
        .await;
        if let Err(error) = result
            && let Some(sender) = sender.borrow_mut().take()
        {
            let _ = sender.send(Err(format!("Could not lock the workspace: {error:?}")));
        }
        // Dropping the callback here is safe: the browser finished its promise.
        drop(callback);
    });
    receiver
        .await
        .map_err(|_| "Workspace execution stopped before acknowledgement".to_string())?
}

#[derive(Clone)]
enum Runtime {
    Native,
    Browser {
        handle: FileSystemDirectoryHandle,
        lock: String,
    },
}

/// Only browser-local projects retain an origin-bound directory capability.
pub enum TransactionFolderAccess {
    Host,
    Retained(FileSystemDirectoryHandle),
    Missing,
}
pub async fn transaction_folder_access(
    mode: openwebide_core::WorkspaceMode,
    user: i64,
    project: i64,
    id: &str,
) -> Result<TransactionFolderAccess, String> {
    match mode {
        openwebide_core::WorkspaceMode::Local => {
            Ok(crate::idb::load_transaction_root(user, project, id)
                .await?
                .map_or(
                    TransactionFolderAccess::Missing,
                    TransactionFolderAccess::Retained,
                ))
        }
        openwebide_core::WorkspaceMode::Remote => Ok(TransactionFolderAccess::Host),
    }
}
#[derive(Clone)]
pub struct WorkspaceTransactionExecutor {
    runtime: Runtime,
    backend: Rc<dyn Backend>,
    project: i64,
    user: i64,
    host: Option<crate::project_host::ProjectHost>,
    review_files: Option<std::sync::Arc<review::WorkspaceReviewFiles>>,
}
impl WorkspaceTransactionExecutor {
    /// Bind a browser capability only after the user explicitly reviewed and
    /// confirmed this folder. Content matching alone never grants authority.
    pub async fn grant_reviewed_folder(
        &self,
        id: String,
        generation: String,
        revision: u64,
        reviewed: Option<std::sync::Arc<transaction::Snapshot>>,
        current: Rc<dyn Fn() -> bool>,
    ) -> Result<(), String> {
        let Runtime::Browser { handle, lock } = &self.runtime else {
            return Err("This execution host does not need a browser folder grant".into());
        };
        if !transaction::valid_id(&id) || !transaction::valid_id(&generation) || !current() {
            return Err("The folder review changed".into());
        }
        let (backend, user, project, handle) = (
            self.backend.clone(),
            self.user,
            self.project,
            handle.clone(),
        );
        let executor = self.clone();
        with_workspace_lock(lock.clone(), async move {
            let state = backend
                .workspace_transaction(project, Some(&id))
                .await
                .map_err(|e| e.to_string())?
                .ok_or("The interrupted operation is unavailable")?;
            if state.journal.id != id
                || state.journal.generation != generation
                || state.journal.revision != revision
                || !current()
            {
                return Err("The interrupted operation changed; choose the folder again".into());
            }
            if executor
                .review_folder_contents(&state, current.clone())
                .await?
                .as_ref()
                != reviewed.as_deref()
            {
                return Err("The folder changed after review; choose it again".into());
            }
            // The reviewed tree is shared; release this full journal before
            // loading its final acknowledgement rather than retaining two plans.
            drop(state);
            let latest = backend
                .workspace_transaction(project, Some(&id))
                .await
                .map_err(|e| e.to_string())?
                .ok_or("The interrupted operation is unavailable")?;
            if latest.journal.id != id
                || latest.journal.generation != generation
                || latest.journal.revision != revision
                || !current()
            {
                return Err("The operation or folder changed after review".into());
            }
            crate::idb::capture_transaction_root(user, project, &id, &handle).await
        })
        .await
    }
    /// Review a candidate capability without binding it. The browser lock also
    /// prevents this proof from revoking a session midway through another tab's
    /// transaction. Confirmation rechecks the same cursor under that lock.
    pub async fn review_folder(
        &self,
        state: std::sync::Arc<transaction::WorkspaceTransactionState>,
        current: Rc<dyn Fn() -> bool>,
    ) -> Result<Option<transaction::Snapshot>, String> {
        let Runtime::Browser { lock, .. } = &self.runtime else {
            return Err("This execution host does not need a browser folder grant".into());
        };
        let executor = self.clone();
        with_workspace_lock(lock.clone(), async move {
            executor.review_folder_contents(&state, current).await
        })
        .await
    }
    async fn review_folder_contents(
        &self,
        state: &transaction::WorkspaceTransactionState,
        current: Rc<dyn Fn() -> bool>,
    ) -> Result<Option<transaction::Snapshot>, String> {
        state.journal.validate()?;
        let Runtime::Browser { handle, .. } = &self.runtime else {
            return Err("The selected browser folder is unavailable".into());
        };
        if !current() {
            return Err("The folder review changed".into());
        }
        if state.journal.plan.version == 2 {
            let mut files = self
                .host
                .ok_or("Connect the paired execution host before native recovery")?
                .folder_files(
                    self.user,
                    self.project,
                    handle.clone(),
                    Some(state.journal.plan.root.clone()),
                    current.clone(),
                )
                .await?;
            files
                .bind(transaction::ResourceScope {
                    transaction: state.journal.id.clone(),
                    fingerprint: state.journal.plan.fingerprint()?,
                })
                .await?;
            let result =
                if state.journal.progress.phase == transaction::TransactionPhase::Finalizing {
                    // The joint physical proof is authority here. Later workspace
                    // edits must not invalidate an irreversible cleanup decision.
                    Ok(None)
                } else {
                    transaction::preview_recovery(&files, &state.journal, || current())
                        .await
                        .map(Some)
                };
            let _ = files.close().await;
            result
        } else {
            if state.journal.plan.root != local_identity(self.user, self.project, &state.journal.id)
            {
                return Err("The interrupted operation belongs to another folder".into());
            }
            let vfs = BrowserFsaVfs::new(handle.clone());
            let files = VfsTransactionFiles {
                vfs: &vfs,
                identity: &state.journal.plan.root,
            };
            transaction::preview_recovery(&files, &state.journal, || current())
                .await
                .map(Some)
        }
    }
    pub fn select(
        backend: Rc<dyn Backend>,
        workspace: Workspace,
        user: i64,
        project: i64,
    ) -> Result<Self, String> {
        if user <= 0 || project <= 0 {
            return Err("Invalid workspace execution scope".into());
        }
        let runtime = match workspace {
            Workspace::Remote { project_id, .. } if project_id == project => Runtime::Native,
            Workspace::Remote { .. } => return Err("The selected project changed".into()),
            Workspace::Local { handle } => Runtime::Browser {
                handle,
                lock: workspace_lock_name(project),
            },
        };
        Ok(Self {
            runtime,
            backend,
            project,
            user,
            host: None,
            review_files: None,
        })
    }
    pub fn with_host(mut self, host: Option<crate::project_host::ProjectHost>) -> Self {
        self.host = host;
        self
    }
    pub fn with_review_files(
        mut self,
        files: std::sync::Arc<review::WorkspaceReviewFiles>,
    ) -> Self {
        self.review_files = Some(files);
        self
    }
    /// Capture the owning filesystem capability before building the review.
    /// Recovery never creates a new binding for an existing durable journal.
    #[cfg(test)]
    pub async fn root_identity(&self, id: &str) -> Result<String, String> {
        if !transaction::valid_id(id) {
            return Err("Invalid workspace transaction identity".into());
        }
        match &self.runtime {
            Runtime::Native => self
                .backend
                .workspace_transaction_root(self.project)
                .await
                .map(|root| root.identity)
                .map_err(|e| e.to_string()),
            Runtime::Browser { handle, lock } => {
                let (backend, user, project, id, handle) = (
                    self.backend.clone(),
                    self.user,
                    self.project,
                    id.to_string(),
                    handle.clone(),
                );
                with_workspace_lock(lock.clone(), async move {
                    let original = crate::idb::load_transaction_root(user, project, &id).await?;
                    if original.is_none()
                        && backend
                            .workspace_transaction(project, Some(&id))
                            .await
                            .map_err(|e| e.to_string())?
                            .is_some()
                    {
                        return Err(
                            "Grant access to the original transaction folder before recovery"
                                .into(),
                        );
                    }
                    crate::idb::capture_transaction_root(user, project, &id, &handle).await?;
                    Ok(local_identity(user, project, &id))
                })
                .await
            }
        }
    }
    pub async fn prepare(
        &self,
        input: &BeginWorkspaceTransaction,
    ) -> Result<transaction::PreparedJournal, String> {
        input.plan.validate()?;
        input.editor.validate()?;
        if let Some(review) = &input.language_review {
            review.validate_transaction(&input.plan, &input.editor)?;
        }
        let prepared = self
            .backend
            .begin_workspace_transaction(self.project, input)
            .await
            .map_err(|error| error.to_string())?;
        if prepared.journal.id != input.id
            || !transaction::valid_id(&prepared.journal.generation)
            || prepared.journal.fingerprint != input.plan.fingerprint()?
            || prepared.editor_revision != input.editor.revision
        {
            return Err("Workspace recovery acknowledgement does not match this review".into());
        }
        Ok(prepared.journal)
    }
    pub async fn execute(
        &self,
        input: BeginWorkspaceTransaction,
        generation: String,
        action: WorkspaceTransactionAction,
        current: Rc<dyn Fn() -> bool>,
    ) -> Result<WorkspaceTransactionOutcome, String> {
        if !transaction::valid_id(&generation) || !current() {
            return Err("The reviewed workspace changed".into());
        }
        if self
            .review_files
            .as_ref()
            .is_some_and(|files| files.id() != input.id || files.root_identity() != input.plan.root)
        {
            return Err("The proven workspace capability belongs to another review".into());
        }
        let backend = self.backend.clone();
        let project = self.project;
        let user = self.user;
        let host = self.host;
        let review_files = self.review_files.clone();
        match &self.runtime {
            Runtime::Native => {
                with_workspace_lock(workspace_lock_name(project), async move {
                    let (_, record, language_grant) = reviewed_record(
                        backend.clone(),
                        project,
                        &input,
                        &generation,
                        action,
                        &current,
                    )
                    .await?;
                    if !current() {
                        return Err("The reviewed workspace changed".into());
                    }
                    let outcome = backend
                        .execute_workspace_transaction(
                            project,
                            &ExecuteWorkspaceTransaction {
                                language_grant,
                                id: input.id,
                                generation: record.generation,
                                action,
                            },
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    outcome.validate(&generation)?;
                    Ok(outcome)
                })
                .await
            }
            Runtime::Browser { handle, lock } => {
                let handle = handle.clone();
                with_workspace_lock(lock.clone(), async move {
                    let original = crate::idb::load_transaction_root(user, project, &input.id)
                        .await?
                        .ok_or("Grant access to the original transaction folder before recovery")?;
                    let same = JsFuture::from(original.is_same_entry(&handle))
                        .await
                        .map_err(|error| {
                            format!("Could not verify the original folder: {error:?}")
                        })?;
                    let identity = if input.plan.version == 2 { input.plan.root.clone() }
                        else { local_identity(user, project, &input.id) };
                    if same.as_bool() != Some(true) || input.plan.root != identity {
                        return Err("Reconnect the original project folder before recovery".into());
                    }
                    let (journal, record, language_grant) =
                        reviewed_record(backend, project, &input, &generation, action, &current).await?;
                    if input.plan.version == 2 {
                        let retained = if let Some(review) = &review_files {
                            review.local_files().ok_or("The original proven browser folder is unavailable")?
                        } else {
                            Rc::new(futures::lock::Mutex::new(host.ok_or("Connect the paired execution host before native resource operations")?
                                .folder_files(user, project, handle, Some(identity), current.clone()).await?))
                        };
                        let mut files = retained.lock().await;
                        files.bind(transaction::ResourceScope { transaction: input.id,
                            fingerprint: input.plan.fingerprint()? }).await?;
                        let (api_url, delegation) = journal.delegation(&*files,record.revision).await?;
                        let outcome = files.execute(api_url, delegation, ExecuteWorkspaceTransaction {
                            id: record.id, generation: record.generation, action, language_grant,
                        }).await;
                        // This also drains a request whose HTTP observer failed.
                        // A later session fences this capability even if close's
                        // acknowledgement is itself lost.
                        let _ = files.close().await;
                        return outcome;
                    }
                    let vfs = BrowserFsaVfs::new(handle);
                    let files = VfsTransactionFiles {
                        vfs: &vfs,
                        identity: &identity,
                    };
                    Ok(execute_files(&files, &journal, record, action, &current).await)
                })
                .await
            }
        }
    }
}
async fn execute_files(
    files: &impl transaction::TransactionFiles,
    journal: &impl WorkspaceJournal,
    record: JournalRecord,
    action: WorkspaceTransactionAction,
    current: &Rc<dyn Fn() -> bool>,
) -> WorkspaceTransactionOutcome {
    let generation = record.generation.clone();
    match transaction::execute(files, journal, record, action, || current()).await {
        Ok(record) => WorkspaceTransactionOutcome {
            generation,
            phase: Some(record.progress.phase),
            error: None,
        },
        Err(error) => {
            let phase = if current() {
                journal
                    .load(None)
                    .await
                    .ok()
                    .flatten()
                    .map(|record| record.progress.phase)
            } else {
                None
            };
            WorkspaceTransactionOutcome {
                generation,
                phase,
                error: Some(error),
            }
        }
    }
}
async fn reviewed_record(
    backend: Rc<dyn Backend>,
    project: i64,
    input: &BeginWorkspaceTransaction,
    generation: &str,
    action: WorkspaceTransactionAction,
    current: &Rc<dyn Fn() -> bool>,
) -> Result<(BackendWorkspaceJournal, JournalRecord, Option<String>), String> {
    let journal = BackendWorkspaceJournal::new(
        backend.clone(),
        project,
        input.id.clone(),
        generation.into(),
        input.plan.root.clone(),
        input.editor.clone(),
        input.language_review.clone(),
    )?;
    let record = journal.load(Some(&input.id)).await?;
    if !current() {
        return Err("The reviewed workspace changed".into());
    }
    let record = record.ok_or("The reviewed operation is unavailable")?;
    if record.plan != input.plan || !current() {
        return Err("Workspace recovery no longer matches this review".into());
    }
    // Initial Apply alone obtains current sessionless manual authority. A
    // successor recovering accepted work uses its durable receipt instead.
    let language_grant = if action == WorkspaceTransactionAction::Apply
        && record.progress.phase == transaction::TransactionPhase::Prepared
        && let Some(review) = &input.language_review
    {
        use openwebide_core::plugins::execution::{PluginExecutionContext, PluginGrantRequest};
        let grants = backend
            .plugin_context_grants(&PluginGrantRequest {
                context: PluginExecutionContext {
                    project_id: Some(project),
                    user_action: true,
                    ..Default::default()
                },
                plugins: vec![review.prepared.clone()],
            })
            .await?;
        if !current() {
            return Err("The reviewed workspace changed during authority checks".into());
        }
        Some(
            grants
                .get(&review.prepared.digest)
                .filter(|grant| transaction::valid_id(grant))
                .ok_or("Fresh manual language authority is unavailable")?
                .clone(),
        )
    } else {
        None
    };
    Ok((journal, record, language_grant))
}

fn local_identity(user: i64, project: i64, id: &str) -> String {
    format!("browser:{user}:{project}:{id}")
}
