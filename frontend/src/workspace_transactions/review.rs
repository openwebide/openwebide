//! Adapter selection for ordinary workspace reviews. Both native readers expose
//! complete inventories; the editor's filtered file tree is never a fallback.
use super::{Runtime, WorkspaceTransactionExecutor, inspection, with_workspace_lock};
use crate::workspace_transactions::companion::BrowserFolderTransport;
use openwebide_core::{
    FileEntry,
    plugins::languages::intelligence::workspace_edit::{WorkspaceDocument, WorkspaceDraft},
    vfs::VfsEntryKind,
    workspace_entries::WorkspaceEntries,
    workspace_transaction::{
        FileOperation, TransactionFiles,
        companion::{CompanionFiles, InspectedFolder},
    },
};
use std::{collections::BTreeMap, rc::Rc};

enum ReviewRuntime {
    Remote(InspectedFolder<inspection::RemoteFolderInspector>),
    Local(Rc<futures::lock::Mutex<CompanionFiles<BrowserFolderTransport>>>),
    #[cfg(test)]
    Legacy(crate::workspace::Workspace),
}

/// One proven read capability survives action discovery, preview and execution.
/// Browser resources stay on their original thread; clones share one cursor.
pub struct WorkspaceReviewFiles {
    id: String,
    root: String,
    executor: send_wrapper::SendWrapper<WorkspaceTransactionExecutor>,
    runtime: send_wrapper::SendWrapper<ReviewRuntime>,
}
impl WorkspaceReviewFiles {
    fn new(
        mut executor: WorkspaceTransactionExecutor,
        id: String,
        root: String,
        runtime: ReviewRuntime,
    ) -> Self {
        // A captured reader must not retain an older reader through its factory.
        executor.review_files = None;
        Self {
            id,
            root,
            executor: send_wrapper::SendWrapper::new(executor),
            runtime: send_wrapper::SendWrapper::new(runtime),
        }
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn root_identity(&self) -> &str {
        &self.root
    }
    pub(crate) fn local_files(
        &self,
    ) -> Option<Rc<futures::lock::Mutex<CompanionFiles<BrowserFolderTransport>>>> {
        match &*self.runtime {
            ReviewRuntime::Local(files) => Some(files.clone()),
            ReviewRuntime::Remote(_) => None,
            #[cfg(test)]
            ReviewRuntime::Legacy(_) => None,
        }
    }
    /// Only actual resource previews retain a recovery handle. Discovery and
    /// text-only results must not leave unused origin-bound records behind.
    pub async fn retain_root(&self, current: Rc<dyn Fn() -> bool>) -> Result<(), String> {
        let executor = (*self.executor).clone();
        let Runtime::Browser { handle, lock } = &executor.runtime else {
            return Ok(());
        };
        #[cfg(test)]
        if matches!(&*self.runtime, ReviewRuntime::Legacy(_)) {
            return Ok(());
        }
        let (handle, lock, id) = (handle.clone(), lock.clone(), self.id.clone());
        with_workspace_lock(lock, async move {
            executor.check_new_review(&id, &current).await?;
            crate::idb::capture_transaction_root(executor.user, executor.project, &id, &handle)
                .await?;
            if !current() {
                return Err(
                    "The selected folder changed while retaining its review capability".into(),
                );
            }
            Ok(())
        })
        .await
    }
    pub async fn prepare(
        &self,
        operations: Vec<FileOperation>,
        documents: BTreeMap<String, WorkspaceDocument>,
        order: Vec<String>,
        selected: Option<String>,
        current: impl Fn() -> bool,
    ) -> Result<WorkspaceDraft, String> {
        let root = self.root_identity().to_owned();
        #[cfg(test)]
        if matches!(&*self.runtime, ReviewRuntime::Legacy(_)) {
            return WorkspaceDraft::prepare(
                self, root, operations, documents, order, selected, current,
            )
            .await;
        }
        WorkspaceDraft::prepare_native(self, root, operations, documents, order, selected, current)
            .await
    }
    pub async fn close(&self) {
        if let ReviewRuntime::Local(files) = &*self.runtime {
            let _ = files.lock().await.close().await;
        }
    }
}
impl WorkspaceEntries for WorkspaceReviewFiles {
    async fn canonicalize(&self, path: &str) -> Result<String, String> {
        match &*self.runtime {
            ReviewRuntime::Remote(files) => files.canonicalize(path).await,
            ReviewRuntime::Local(files) => files.lock().await.canonicalize(path).await,
            #[cfg(test)]
            ReviewRuntime::Legacy(files) => files.canonicalize(path).await,
        }
    }
    async fn list(&self, path: &str) -> Result<Vec<FileEntry>, String> {
        match &*self.runtime {
            ReviewRuntime::Remote(files) => files.list(path).await,
            ReviewRuntime::Local(files) => files.lock().await.list(path).await,
            #[cfg(test)]
            ReviewRuntime::Legacy(files) => WorkspaceEntries::list(files, path).await,
        }
    }
    async fn read_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        match &*self.runtime {
            ReviewRuntime::Remote(files) => files.read_bytes(path).await,
            ReviewRuntime::Local(files) => files.lock().await.read_bytes(path).await,
            #[cfg(test)]
            ReviewRuntime::Legacy(files) => WorkspaceEntries::read_bytes(files, path).await,
        }
    }
    async fn create(&self, _path: &str, _kind: VfsEntryKind) -> Result<(), String> {
        Err("Workspace review is read-only".into())
    }
    async fn write_bytes(&self, _path: &str, _bytes: &[u8]) -> Result<(), String> {
        Err("Workspace review is read-only".into())
    }
    async fn delete(&self, _path: &str) -> Result<(), String> {
        Err("Workspace review is read-only".into())
    }
}
impl WorkspaceTransactionExecutor {
    /// Prove the folder before native action discovery, then reuse this exact
    /// capability. Fresh inspection still runs under the browser workspace lock.
    pub async fn review_files(
        &self,
        id: String,
        current: Rc<dyn Fn() -> bool>,
    ) -> Result<WorkspaceReviewFiles, String> {
        if !openwebide_core::workspace_transaction::valid_id(&id) || !current() {
            return Err("The workspace review identity changed".into());
        }
        match &self.runtime {
            Runtime::Native => {
                self.check_new_review(&id, &current).await?;
                let files =
                    inspection::remote_files(self.backend.clone(), self.project, current).await?;
                let root = files.root_identity().to_owned();
                Ok(WorkspaceReviewFiles::new(
                    self.clone(),
                    id,
                    root,
                    ReviewRuntime::Remote(files),
                ))
            }
            Runtime::Browser { handle, lock } => {
                let (executor, handle) = (self.clone(), handle.clone());
                with_workspace_lock(lock.clone(), async move {
                    executor.check_new_review(&id, &current).await?;
                    let host = executor.host.ok_or("Connect the paired execution host before reviewing native resource operations")?;
                    let files = host.folder_files(executor.user, executor.project, handle, None, current.clone()).await?;
                    if !current() {return Err("The selected folder changed during review preparation".into())}
                    let root = files.root_identity().to_owned();
                    Ok(WorkspaceReviewFiles::new(executor, id, root, ReviewRuntime::Local(Rc::new(futures::lock::Mutex::new(files)))))
                }).await
            }
        }
    }
    async fn check_new_review(
        &self,
        id: &str,
        current: &Rc<dyn Fn() -> bool>,
    ) -> Result<(), String> {
        if !current() {
            return Err("The workspace review changed before journal inspection".into());
        }
        let result = crate::util::with_deadline(
            async {
                if self
                    .backend
                    .workspace_transaction(self.project, Some(id))
                    .await
                    .map_err(|e| e.to_string())?
                    .is_some()
                {
                    return Err(
                        "This operation already has a journal; use workspace recovery".to_string(),
                    );
                }
                if !current() {
                    return Err("The workspace review changed".into());
                }
                crate::workspace_mutations::ensure_idle(
                    self.backend.clone(),
                    self.project,
                    current.clone(),
                )
                .await?;
                Ok(())
            },
            30_000,
        )
        .await;
        if !current() {
            return Err("The workspace review changed during journal inspection".into());
        }
        result.ok_or_else(|| "Workspace journal inspection timed out".to_string())?
    }
    #[cfg(test)]
    pub(crate) async fn legacy_review_files(
        &self,
        id: &str,
        files: crate::workspace::Workspace,
    ) -> Result<WorkspaceReviewFiles, String> {
        Ok(WorkspaceReviewFiles::new(
            self.clone(),
            id.into(),
            self.root_identity(id).await?,
            ReviewRuntime::Legacy(files),
        ))
    }
}

#[cfg(test)]
mod tests;
