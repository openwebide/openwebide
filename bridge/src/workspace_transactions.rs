//! Native runtime and persistence adapters for the shared transaction engine.
//! A worker owns the complete operation, including its lock, after disconnect.
#[cfg(test)]
use crate::runs::native_vfs::NativeFsVfs;
use crate::{
    folder_coordination::{Coordinator, FileIdentity, Folder},
    paths::resolve_in_root,
    runs::{backend_client::BackendClient, native_vfs::workspace::WorkspaceFiles},
};
#[cfg(test)]
use openwebide_core::workspace_transaction::TransactionPhase;
use openwebide_core::{
    WorkspaceMode,
    workspace_transaction::{
        self as transaction, JournalRecord, NativeWorkspaceTransaction, PreparedJournal,
        TransactionProgress, VfsTransactionFiles, WorkspaceJournal, WorkspaceJournalOperation,
        WorkspaceJournalRequest, WorkspaceJournalResponse, WorkspacePlan,
        WorkspaceTransactionOutcome, WorkspaceTransactionScope, WorkspaceTransactionState,
        WorkspaceTransactionWorker, valid_id,
    },
};
use std::{
    collections::BTreeMap,
    future::Future,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::Mutex as AsyncMutex;
pub mod command_delegation;
pub mod delegation;
mod folder;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod original_command;
use folder::ExecutionFolder;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod companion;

pub trait JournalBackend: Send + Sync + 'static {
    fn command_outcome(
        &self,
        _expected: &transaction::WorkspaceCommandRecovery,
    ) -> impl Future<
        Output = Result<
            openwebide_core::plugins::languages::intelligence::actions::CommandOutcome,
            String,
        >,
    > + Send {
        async { Err("Original retained command outcomes are unavailable on this transport".into()) }
    }
    fn recovery_origin(&self) -> Option<&str> {
        None
    }
    fn request(
        &self,
        request: &WorkspaceJournalRequest,
    ) -> impl Future<Output = Result<WorkspaceJournalResponse, String>> + Send;
}
impl JournalBackend for BackendClient {
    fn recovery_origin(&self) -> Option<&str> {
        Some(self.recovery_origin())
    }
    async fn request(
        &self,
        request: &WorkspaceJournalRequest,
    ) -> Result<WorkspaceJournalResponse, String> {
        self.workspace_journal_service(request).await
    }
}
struct Journal<B> {
    backend: Arc<B>,
    user: i64,
    project: i64,
    id: String,
    generation: String,
    worker: WorkspaceTransactionWorker,
    mode: WorkspaceMode,
}
impl<B: JournalBackend> Journal<B> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    async fn command_recovery(
        &self,
    ) -> Result<
        (
            transaction::WorkspaceCommandRecovery,
            transaction::RecoveryAuthority,
        ),
        String,
    > {
        let reply = self
            .backend
            .request(&WorkspaceJournalRequest {
                user: self.user,
                project: self.project,
                id: self.id.clone(),
                operation: WorkspaceJournalOperation::CommandRecoveryScope,
            })
            .await?;
        let (recovery, authority) = reply.command_recovery(self.user, self.project, &self.id)?;
        Ok((recovery.clone(), authority.clone()))
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    async fn recovery(
        &self,
    ) -> Result<
        (
            WorkspaceTransactionScope,
            transaction::RecoveryAuthority,
            transaction::WorkspaceActivity,
        ),
        String,
    > {
        match self
            .backend
            .request(&WorkspaceJournalRequest {
                user: self.user,
                project: self.project,
                id: self.id.clone(),
                operation: WorkspaceJournalOperation::RecoveryScope,
            })
            .await?
        {
            WorkspaceJournalResponse::RecoveryScope {
                scope,
                authority,
                activity,
            } if scope.id == self.id
                && scope.generation == self.generation
                && scope.root.mode == self.mode
                && authority.matches_scope(self.user, self.project, &scope) =>
            {
                Ok((scope, *authority, activity))
            }
            _ => Err("The original private workspace recovery scope is unavailable".into()),
        }
    }
    async fn claim(
        &self,
        scope: &WorkspaceTransactionScope,
        review: Option<(&str, &str)>,
        command: Option<
            Box<openwebide_core::plugins::languages::intelligence::actions::CommandReservation>,
        >,
    ) -> Result<(), String> {
        let response = self
            .backend
            .request(&WorkspaceJournalRequest {
                user: self.user,
                project: self.project,
                id: self.id.clone(),
                operation: WorkspaceJournalOperation::ClaimWorker {
                    command,
                    scope: scope.clone(),
                    worker: self.worker.clone(),
                    review_digest: review.map(|(digest, _)| digest.into()),
                    language_grant: review.map(|(_, grant)| grant.into()),
                },
            })
            .await?;
        let mut expected = scope.clone();
        expected.worker = Some(self.worker.state());
        if let Some((digest, _)) = review {
            expected.accepted_language_review_digest = Some(digest.into());
        }
        match response {
            WorkspaceJournalResponse::WorkerClaimed { scope } if scope == expected => Ok(()),
            _ => Err("Workspace worker admission was not acknowledged".into()),
        }
    }
    async fn release(&self) -> Result<(), String> {
        match self
            .backend
            .request(&WorkspaceJournalRequest {
                user: self.user,
                project: self.project,
                id: self.id.clone(),
                operation: WorkspaceJournalOperation::ReleaseWorker {
                    generation: self.generation.clone(),
                    worker: self.worker.clone(),
                },
            })
            .await?
        {
            WorkspaceJournalResponse::WorkerReleased { generation, worker }
                if generation == self.generation && worker == self.worker.id =>
            {
                Ok(())
            }
            _ => Err("Workspace worker release was not acknowledged".into()),
        }
    }
    async fn scope(&self) -> Result<WorkspaceTransactionScope, String> {
        match self
            .backend
            .request(&WorkspaceJournalRequest {
                user: self.user,
                project: self.project,
                id: self.id.clone(),
                operation: WorkspaceJournalOperation::Scope,
            })
            .await?
        {
            WorkspaceJournalResponse::Scope { scope: Some(scope) } if scope.id == self.id => {
                scope.validate()?;
                if scope.root.mode != self.mode {
                    return Err("Reconnect the original workspace before native recovery".into());
                }
                Ok(scope)
            }
            _ => Err("The owned workspace recovery scope is unavailable".into()),
        }
    }
    async fn state(&self) -> Result<WorkspaceTransactionState, String> {
        match self
            .backend
            .request(&WorkspaceJournalRequest {
                user: self.user,
                project: self.project,
                id: self.id.clone(),
                operation: WorkspaceJournalOperation::Load,
            })
            .await?
        {
            WorkspaceJournalResponse::Loaded { state: Some(state) }
                if state.journal.id == self.id =>
            {
                if state.journal.generation != self.generation {
                    return Err(
                        "Workspace recovery identity changed; reload before continuing".into(),
                    );
                }
                state.journal.validate()?;
                state.editor.validate()?;
                if state.editor.root.mode != self.mode {
                    return Err("Reconnect the original workspace before native recovery".into());
                }
                Ok(*state)
            }
            _ => Err("The owned workspace recovery record is unavailable".into()),
        }
    }
}
impl<B: JournalBackend> WorkspaceJournal for Journal<B> {
    async fn prepare(&self, plan: &WorkspacePlan) -> Result<PreparedJournal, String> {
        let state = self.state().await?;
        state.journal.prepared_for(plan)
    }
    async fn advance(
        &self,
        id: &str,
        generation: &str,
        revision: u64,
        progress: &TransactionProgress,
    ) -> Result<u64, String> {
        if id != self.id || generation != self.generation {
            return Err("Workspace recovery identity changed".into());
        }
        let response = self
            .backend
            .request(&WorkspaceJournalRequest {
                user: self.user,
                project: self.project,
                id: id.into(),
                operation: WorkspaceJournalOperation::AdvanceOwned {
                    worker: self.worker.clone(),
                    generation: generation.into(),
                    revision,
                    progress: progress.clone(),
                },
            })
            .await?;
        match response {
            WorkspaceJournalResponse::Advanced { response } => {
                response.validate(generation, revision)?;
                Ok(response.revision)
            }
            _ => Err("Invalid workspace recovery acknowledgement".into()),
        }
    }
    async fn load(&self, id: Option<&str>) -> Result<Option<JournalRecord>, String> {
        if id.is_some_and(|id| id != self.id) {
            return Err("Workspace recovery identity changed".into());
        }
        self.state().await.map(|state| Some(state.journal))
    }
}

type Locks = BTreeMap<(i64, i64), Arc<AsyncMutex<()>>>;
#[derive(Clone)]
pub struct Host {
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    recovery_home: Arc<PathBuf>,
    locks: Arc<Mutex<Locks>>,
    slots: Arc<tokio::sync::Semaphore>,
    pub(crate) coordinator: Coordinator,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub companion: companion::Host,
}
impl Default for Host {
    fn default() -> Self {
        Self::new()
    }
}
#[derive(Clone)]
struct CapturedRoot {
    folder: Folder,
    identity: String,
}
impl std::ops::Deref for CapturedRoot {
    type Target = Folder;
    fn deref(&self) -> &Folder {
        &self.folder
    }
}
impl Host {
    pub fn new() -> Self {
        let coordinator = Coordinator::default();
        Self {
            #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
            recovery_home: crate::native_recovery::testing_home(),
            locks: Default::default(),
            slots: Arc::new(tokio::sync::Semaphore::new(1)),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            companion: companion::Host::new(coordinator.clone()),
            coordinator,
        }
    }
    #[cfg(all(
        any(test, feature = "test-support"),
        any(target_os = "linux", target_os = "macos")
    ))]
    pub(crate) fn set_test_recovery_home(&mut self, home: Arc<PathBuf>) {
        self.coordinator.set_recovery_home(home.clone());
        #[cfg(test)]
        {
            self.recovery_home = home;
        }
    }
    fn capture_root(
        workspace: &std::path::Path,
        relative: &str,
        user: i64,
        project: i64,
    ) -> Result<CapturedRoot, String> {
        use std::hash::Hash;
        if user <= 0 || project <= 0 {
            return Err("Invalid workspace root scope".into());
        }
        let allowed = workspace.canonicalize().map_err(|e| e.to_string())?;
        let path = resolve_in_root(&allowed, Some(relative))?
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if !path.starts_with(&allowed) {
            return Err("The original folder escapes the execution workspace".into());
        }
        let physical_path = path
            .to_str()
            .ok_or("Workspace root names must be valid UTF-8")?;
        if openwebide_core::vfs::private_workspace_path(physical_path)
            || path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case(".openwebide"))
        {
            return Err("A runtime control folder cannot be used as a resource workspace".into());
        }
        let folder = Folder::capture(&path)?;
        let handle = folder.handle.clone();
        let metadata = handle.as_file().metadata().map_err(|e| e.to_string())?;
        if !metadata.is_dir() {
            return Err("The selected workspace root is not a folder".into());
        }
        let mut physical = FileIdentity::default();
        handle.hash(&mut physical);
        let created = metadata
            .created()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|time| time.as_nanos());
        // This identifies a filesystem capability, not an authentication
        // credential. The authenticated route supplies account/project scope;
        // rotating its transport secret must not strand a durable rollback.
        let mut bytes = serde_json::to_vec(&("workspace-root-v2", user, project, &path, created))
            .map_err(|e| e.to_string())?;
        bytes.extend_from_slice(&physical.0);
        let identity = openwebide_core::plugins::content_digest(&bytes);
        Ok(CapturedRoot { folder, identity })
    }
    pub fn root_identity(
        workspace: &std::path::Path,
        relative: &str,
        user: i64,
        project: i64,
    ) -> Result<String, String> {
        Self::capture_root(workspace, relative, user, project).map(|root| root.identity)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) async fn folder_request(
        &self,
        workspace: PathBuf,
        owner: String,
        request: transaction::companion::FolderRequest,
        languages: Option<crate::languages::host::Host>,
    ) -> Result<transaction::companion::FolderReply, String> {
        use transaction::companion::{FolderAction, FolderReply};
        request.validate()?;
        if owner.is_empty() || owner.len() > 128 {
            return Err("Invalid folder authority".into());
        }
        if let FolderAction::Execute {
            api_url,
            delegation,
            request: execution,
            ..
        } = &request.action
        {
            let folder = self.companion.proven(&owner, &request).await?;
            let journal = delegation::PairedJournal::new(
                api_url.clone(),
                &folder.root.identity,
                delegation.clone(),
                folder.worker.clone(),
            )?;
            let command = NativeWorkspaceTransaction {
                user: request.user,
                project: request.project,
                request: execution.clone(),
            };
            let outcome = self
                .execute_in(
                    ExecutionFolder::Paired(Box::new(folder)),
                    command,
                    Arc::new(journal),
                    languages,
                )
                .await?;
            Ok(FolderReply::Executed { outcome })
        } else {
            self.companion.request(workspace, owner, request).await
        }
    }

    pub async fn execute<B: JournalBackend>(
        &self,
        workspace_root: PathBuf,
        command: NativeWorkspaceTransaction,
        backend: Arc<B>,
    ) -> Result<WorkspaceTransactionOutcome, String> {
        self.execute_reviewed(workspace_root, command, backend, None)
            .await
    }
    pub(crate) async fn execute_reviewed<B: JournalBackend>(
        &self,
        workspace_root: PathBuf,
        command: NativeWorkspaceTransaction,
        backend: Arc<B>,
        languages: Option<crate::languages::host::Host>,
    ) -> Result<WorkspaceTransactionOutcome, String> {
        self.execute_in(
            ExecutionFolder::Remote(workspace_root),
            command,
            backend,
            languages,
        )
        .await
    }
    async fn execute_in<B: JournalBackend>(
        &self,
        folder: ExecutionFolder,
        command: NativeWorkspaceTransaction,
        backend: Arc<B>,
        languages: Option<crate::languages::host::Host>,
    ) -> Result<WorkspaceTransactionOutcome, String> {
        if command.user <= 0
            || command.project <= 0
            || !valid_id(&command.request.id)
            || !valid_id(&command.request.generation)
        {
            return Err("Invalid workspace transaction scope".into());
        }
        let lock = {
            let mut locks = self
                .locks
                .lock()
                .map_err(|_| "Workspace execution locks are unavailable")?;
            // Remove only idle entries. Queued and running workers retain theirs.
            if locks.len() >= 128 {
                locks.retain(|_, lock| Arc::strong_count(lock) > 1);
            }
            if locks.len() >= 128 && !locks.contains_key(&(command.user, command.project)) {
                return Err("Too many workspaces are applying reviewed operations".into());
            }
            locks
                .entry((command.user, command.project))
                .or_default()
                .clone()
        };
        // A dropped HTTP request drops only the join handle. The owned worker
        // keeps the lock and durable cursor until it commits or restores.
        let registration = self.coordinator.worker()?;
        let coordinator = self.coordinator.clone();
        let slots = self.slots.clone();
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        let recovery_home = self.recovery_home.clone();
        #[cfg(all(not(test), any(target_os = "linux", target_os = "macos")))]
        let recovery_home = coordinator.recovery_home()?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let companion = self.companion.clone();
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        coordinator.set_recovery_home(recovery_home.clone());
        tokio::spawn(async move {
            let _registration = registration;
            let _guard = coordinator.prepare(lock.lock_owned()).await?;
            let journal = Journal {
                backend,
                user: command.user,
                project: command.project,
                id: command.request.id,
                generation: command.request.generation,
                worker: folder.worker(),
                mode: folder.mode(),
            };
            let scope = coordinator.prepare(journal.scope()).await??;
            if scope.generation != journal.generation {
                return Err("Workspace recovery identity changed before execution".into());
            }
            let root = folder.capture(&scope, command.user, command.project)?;
            if root.identity != scope.root_identity {
                return Err(
                    "Reconnect the original physical project folder before recovery".into(),
                );
            }
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let native_namespace = crate::native_recovery::Namespace::open(&recovery_home)?;
            #[cfg(any(target_os="linux",target_os="macos"))]
            let (private_scope, authority, activity)=coordinator.prepare(journal.recovery()).await??;
            #[cfg(any(target_os="linux",target_os="macos"))]
            if private_scope!=scope {return Err("Workspace recovery changed before native admission".into());}
            #[cfg(any(target_os="linux",target_os="macos"))]
            let permit=native_namespace.permit(&root.folder,&authority,&scope,activity)?;
            #[cfg(any(target_os="linux",target_os="macos"))]
            let fence = folder.exclude(&companion, &root, permit.clone()).await?;
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            let fence = Arc::new(coordinator.acquire(root.folder.clone()).await?);
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            native_namespace.ensure_current()?;
            // Queue the physical folder before retaining the one large snapshot
            // slot. A busy folder must not prevent independent roots from loading.
            let _slot = coordinator
                .prepare(slots.acquire_owned())
                .await?
                .map_err(|_| "Workspace execution stopped")?;
            // Reload under both locks. Metadata obtained before a queued wait
            // cannot authorize a different or advanced recovery operation.
            let state = coordinator.prepare(journal.state()).await??;
            let current_scope = coordinator.prepare(journal.scope()).await??;
            if current_scope != scope || !scope.matches_state(&state) || !root.current() {
                return Err(
                    "Workspace recovery changed while waiting; reload before continuing".into(),
                );
            }
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            if !folder.matches_state(&state)? { return Err("The loaded plan differs from the original proven review".into()); }
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let language_scope = state.language_review.as_ref().filter(|_| current_scope.accepted_language_review_digest.is_none()
                && state.journal.progress != TransactionProgress::prepared_cancellation()).map(|review| {
                Ok::<_, String>(crate::languages::host::Scope {
                    owner: folder.owner(command.user), project: command.project,
                    root: root.path.clone(), provider: review.prepared.storage_namespace(),
                    digest: review.prepared.digest.clone(),
                    server: openwebide_core::plugins::languages::intelligence::declared_server(&review.prepared, &review.input.language)?.clone(),
                })
            }).transpose()?;
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let review_guard = if state.language_review.is_some()
                && current_scope.accepted_language_review_digest.is_none()
                && state.journal.progress != TransactionProgress::prepared_cancellation() {
                if command.request.language_grant.as_deref().is_none_or(|grant| !valid_id(grant)) {
                    return Err("Fresh manual language authority is required for first admission".into());
                }
                let languages = languages.as_ref().ok_or("The original language service is unavailable for native admission")?;
                Some(coordinator.prepare(languages.validate_workspace_review(
                    language_scope.clone().ok_or("The original language scope is unavailable")?, &state, fence.lease(),
                )).await??)
            } else { None };
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            if state.language_review.is_some() && current_scope.accepted_language_review_digest.is_none()
                && state.journal.progress != TransactionProgress::prepared_cancellation() {
                let _ = languages;
                return Err("Native admission for retained language reviews is unavailable on this platform".into());
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            if state.journal.plan.version == 2 {
                return Err("Native resource recovery is unavailable on this platform".into());
            }
            // This runtime may replace a prior worker only while the complete
            // physical gate excludes its accepted work. Retain the gate through
            // both mutation execution and the durable release acknowledgement.
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            if current_scope.worker.as_ref().is_some_and(|worker| worker.active) {
                return Err("Recovering an active worker requires cross-process native folder exclusion on this platform".into());
            }
            let admission = if current_scope.accepted_language_review_digest.is_none()
                && state.journal.progress != TransactionProgress::prepared_cancellation() {
                state.language_review.as_ref().map(|review| {
                    Ok::<_, String>((review.fingerprint()?, command.request.language_grant.clone().ok_or("Fresh manual language authority is required for first admission")?))
                }).transpose()?
            } else { None };
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            native_namespace.ensure_current()?;
            #[cfg(any(target_os="linux",target_os="macos"))]
            let command_reservation = {
                let (private_scope, authority, activity)=coordinator.prepare(journal.recovery()).await??;
                if private_scope!=current_scope {return Err("Original recovery changed before enrollment".into());}
                let fresh=native_namespace.permit(&root.folder,&authority,&current_scope,activity)?.with_origin(journal.backend.recovery_origin())?;
                if fence.lease().recovery().is_none() {return Err("Native recovery requires its original permit".into());}
                if activity.language_command && admission.is_some() {
                    let (recovery, command_authority) = coordinator.prepare(journal.command_recovery()).await??;
                    if recovery.scope != current_scope || recovery.activity != activity || command_authority != authority {
                        return Err("The original command changed before file admission".into());
                    }
                    let languages = languages.as_ref().ok_or("The original language service is unavailable")?;
                    let worker = coordinator.prepare(languages.prepare_workspace_command(
                        language_scope.ok_or("The original language scope is unavailable")?, &state, &recovery,
                    )).await??;
                    native_namespace.reserve_command(&root.folder, fence.lease(), &fresh, crate::native_recovery::CommandIntent {
                        recovery: &recovery, worker: &worker,
                    })?;
                    Some(Box::new(openwebide_core::plugins::languages::intelligence::actions::CommandReservation {
                        expected: Box::new(recovery), worker,
                    }))
                } else {
                    native_namespace.enroll(&root.folder,fence.lease(),&fresh)?;
                    None
                }
            };
            #[cfg(not(any(target_os="linux",target_os="macos")))]
            let command_reservation = None;
            journal.claim(&current_scope, admission.as_ref().map(|(digest, grant)| (digest.as_str(), grant.as_str())), command_reservation).await?;
            let result = async {
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                native_namespace.ensure_current()?;
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                native_namespace.files_admitted(&root.folder, fence.lease(), &permit)?;
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                if let Some(review) = &review_guard { review.ensure_current()?; }
                if !root.current() {
                    return Err(
                        "The physical project folder changed during worker admission".into(),
                    );
                }
                let current = || {
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                    if native_namespace.ensure_current().is_err() { return false; }
                    root.current()
                };
                let result = if state.journal.plan.version == 2 {
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                    {
                        let files = crate::runs::native_vfs::resources::ResourceFiles::reviewed(
                            root.handle.as_file(),
                            &root.identity,
                            transaction::ResourceScope {
                                transaction: state.journal.id.clone(),
                                fingerprint: state.journal.plan.fingerprint()?,
                            },
                            state.language_review.as_deref(),
                            &state.journal.plan,
                        )?;
                        transaction::execute(
                            &files,
                            &journal,
                            state.journal,
                            command.request.action,
                            &current,
                        )
                        .await
                    }
                    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                    {
                        return Err(
                            "Native resource recovery is unavailable on this platform".into()
                        );
                    }
                } else {
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                    let lease = fence.shared_lease();
                    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                    let lease = fence.clone();
                    let vfs = WorkspaceFiles::with_lease(root.folder.clone(), lease)
                        .map_err(|error| error.to_string())?;
                    let files = VfsTransactionFiles {
                        vfs: &vfs,
                        identity: &root.identity,
                    };
                    transaction::execute(
                        &files,
                        &journal,
                        state.journal,
                        command.request.action,
                        &current,
                    )
                    .await
                };
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                native_namespace.ensure_current()?;
                match result {
                    Ok(record) => Ok(WorkspaceTransactionOutcome {
                        generation: journal.generation.clone(),
                        phase: Some(record.progress.phase),
                        error: None,
                    }),
                    Err(error) => {
                        let phase = journal
                            .load(None)
                            .await
                            .ok()
                            .flatten()
                            .map(|record| record.progress.phase);
                        Ok(WorkspaceTransactionOutcome {
                            generation: journal.generation.clone(),
                            phase,
                            error: Some(error),
                        })
                    }
                }
            }
            .await;
            // Paired access is revoked by release. Observe the exact original
            // compound state while still admitted and physically fenced.
            #[cfg(any(target_os="linux",target_os="macos"))]
            let final_owner = if root.current() {Some(journal.recovery().await)} else {None};
            #[cfg(any(target_os="linux",target_os="macos"))]
            let receipt = (|| -> Result<(), String> {
                if let Some(Ok((scope, authority, activity))) = &final_owner {
                    let fresh = native_namespace.permit(&root.folder, authority, scope, *activity)?;
                    native_namespace.files_finished(&root.folder, fence.lease(), &fresh, *activity)?;
                }
                Ok(())
            })();
            let released = journal.release().await;
            #[cfg(any(target_os="linux",target_os="macos"))]
            receipt?;
            #[cfg(any(target_os="linux",target_os="macos"))]
            if released.is_ok() && let Some(proof)=final_owner {
                let (scope,authority,activity)=proof?;
                let fresh=native_namespace.permit(&root.folder,&authority,&scope,activity)?;
                native_namespace.finish_files(&root.folder,fence.lease(),&fresh,activity)?;
            }
            match (result, released) {
                (result, Ok(())) => result,
                (Ok(mut outcome), Err(error)) => {
                    if let Some(original) = outcome.error.as_mut() {
                        original.message.push_str("; worker release: ");
                        original.message.push_str(&error);
                        Ok(outcome)
                    } else {
                        Err(error)
                    }
                }
                (Err(error), Err(release)) => Err(format!("{error}; worker release: {release}")),
            }
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

#[cfg(test)]
mod tests;
