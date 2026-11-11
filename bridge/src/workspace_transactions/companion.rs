//! Captured folder capabilities for browser-owned transaction orchestration.
//! Each accepted call outlives its HTTP observer. Proving a new session drains
//! earlier calls and fences every older session for the same physical folder.
use super::delegation::PairedJournal;
use super::{CapturedRoot, Host as Transactions};
use crate::folder_coordination::{Coordinator, Lease};
use crate::runs::native_vfs::resources::ResourceFiles;
use openwebide_core::workspace_transaction::{
    ResourceScope, TransactionFiles, WorkspaceTransactionWorker,
    companion::{FileAction, FileValue, FolderAction, FolderReply, FolderRequest},
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio::time::Instant;

mod files;
mod proof;
use proof::Challenge;

const MAX_SESSIONS: usize = 32;
const CACHE_LIMIT: usize = 16 * 1024;
const PROOF_TTL: Duration = Duration::from_secs(45);
const ACTIVE_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone)]
pub struct Host {
    coordinator: Coordinator,
    state: Arc<Mutex<State>>,
    reaping: Arc<AtomicBool>,
}
#[derive(Default)]
struct State {
    sessions: BTreeMap<String, Session>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Pending,
    Active,
    Closed,
}
struct Session {
    owner: String,
    user: i64,
    project: i64,
    root: CapturedRoot,
    challenge: Option<Challenge>,
    status: Status,
    touched: Instant,
    scope: Option<ResourceScope>,
    worker: Option<WorkspaceTransactionWorker>,
    journal: Option<PairedJournal>,
    sequence: u64,
    last: Option<(String, Option<Result<FileValue, String>>)>,
}
pub(super) struct ProvenFolder {
    pub(super) root: CapturedRoot,
    pub(super) owner: String,
    pub(super) worker: WorkspaceTransactionWorker,
    pub(super) scope: ResourceScope,
    session: String,
    user: i64,
    project: i64,
}
impl Session {
    fn retire(&mut self) {
        self.status = Status::Closed;
        self.challenge.take();
        self.last = None;
        self.worker = None;
        self.journal = None;
    }
    fn expired(&self) -> bool {
        self.touched.elapsed()
            >= if self.status == Status::Pending {
                PROOF_TTL
            } else {
                ACTIVE_TTL
            }
    }
    fn authorize(&self, request: &FolderRequest, owner: &str) -> Result<(), String> {
        if self.owner != owner || self.user != request.user || self.project != request.project {
            return Err("Folder session belongs to another account or project".into());
        }
        if self.status == Status::Closed || !self.root.current() {
            return Err("Folder session was superseded or its physical root changed".into());
        }
        Ok(())
    }
    fn files(&self, id: &str) -> Result<ResourceFiles, String> {
        ResourceFiles::new(
            self.root.handle.as_file(),
            &self.root.identity,
            self.scope.clone().unwrap_or_else(|| ResourceScope {
                transaction: id.into(),
                fingerprint: "0".repeat(64),
            }),
        )
    }
}
impl State {
    fn sweep(&mut self) {
        self.sessions
            .retain(|_, session| session.status != Status::Closed && !session.expired());
    }
}

/// Holding this guard excludes paired calls during native transaction execution.
pub(super) struct FolderFence {
    guard: OwnedMutexGuard<State>,
    lease: Lease,
    selected: Option<String>,
}
impl Drop for FolderFence {
    fn drop(&mut self) {
        if let Some(selected) = &self.selected
            && let Some(session) = self.guard.sessions.get_mut(selected)
        {
            session.touched = Instant::now();
        }
    }
}
impl FolderFence {
    pub(super) fn lease(&self) -> &Lease {
        &self.lease
    }
}
impl Default for Host {
    fn default() -> Self {
        Self::new(Coordinator::default())
    }
}
impl Host {
    pub(crate) fn new(coordinator: Coordinator) -> Self {
        Self {
            coordinator,
            state: Default::default(),
            reaping: Default::default(),
        }
    }
    pub(super) async fn exclude(&self, root: &CapturedRoot) -> Result<FolderFence, String> {
        self.exclude_selected(root, None).await
    }
    pub(super) async fn exclude_proven(
        &self,
        selected: &ProvenFolder,
    ) -> Result<FolderFence, String> {
        self.exclude_selected(&selected.root, Some(selected)).await
    }
    async fn exclude_selected(
        &self,
        root: &CapturedRoot,
        selected: Option<&ProvenFolder>,
    ) -> Result<FolderFence, String> {
        let lease = self.coordinator.acquire(root.folder.clone()).await?;
        let mut state = self.state.clone().lock_owned().await;
        state.sweep();
        if let Some(selected) = selected {
            let session = state
                .sessions
                .get(&selected.session)
                .ok_or("The original proven folder session expired")?;
            if session.status != Status::Active
                || !session.root.current()
                || session.owner != selected.owner
                || session.user != selected.user
                || session.project != selected.project
                || session.root.identity != selected.root.identity
                || session.scope.as_ref() != Some(&selected.scope)
                || session.worker.as_ref().is_none_or(|worker| {
                    worker.id != selected.worker.id || worker.secret != selected.worker.secret
                })
                || session.journal.is_some()
            {
                return Err("The original proven folder changed before native execution".into());
            }
        }
        for (id, session) in &mut state.sessions {
            if selected.is_none_or(|selected| &selected.session != id)
                && session.root.folder.overlaps(&root.folder)
            {
                session.retire();
            }
        }
        Ok(FolderFence {
            guard: state,
            lease,
            selected: selected.map(|selected| selected.session.clone()),
        })
    }
    pub(super) async fn proven(
        &self,
        owner: &str,
        request: &FolderRequest,
    ) -> Result<ProvenFolder, String> {
        let FolderAction::Execute {
            session: id,
            delegation,
            ..
        } = &request.action
        else {
            return Err("A complete native execution request is required".into());
        };
        let mut state = self.coordinator.prepare(self.state.lock()).await?;
        state.sweep();
        let session = state
            .sessions
            .get(id)
            .ok_or("The original proven folder session expired")?;
        session.authorize(request, owner)?;
        let scope = session
            .scope
            .as_ref()
            .ok_or("Bind the original reviewed plan before native execution")?;
        if session.status != Status::Active
            || session.journal.is_some()
            || delegation.scope.root_identity != session.root.identity
            || scope.transaction != delegation.scope.id
        {
            return Err("The original proven folder belongs to another transaction".into());
        }
        Ok(ProvenFolder {
            root: session.root.clone(),
            owner: owner.into(),
            worker: session
                .worker
                .clone()
                .ok_or("The original private worker is unavailable")?,
            scope: scope.clone(),
            session: id.clone(),
            user: request.user,
            project: request.project,
        })
    }
    /// A backend-owned remote project uses the same descriptor-bound reader
    /// without claiming a browser folder capability. This accepts no mutations.
    pub async fn inspect(
        &self,
        workspace: PathBuf,
        request: openwebide_core::workspace_transaction::companion::NativeFolderInspection,
    ) -> Result<FileValue, String> {
        request.validate()?;
        let registration = self.coordinator.worker()?;
        let host = self.clone();
        tokio::spawn(async move {
            let _registration = registration;
            let mut state = host.coordinator.prepare(host.state.lock()).await?;
            state.sweep();
            let root = Transactions::capture_root(
                &workspace,
                &request.path,
                request.user,
                request.project,
            )?;
            if root.identity != request.inspection.root {
                return Err("The inspected physical project folder changed".into());
            }
            // Read-only ResourceFiles never opens or creates a retained area.
            let files = ResourceFiles::new(
                root.handle.as_file(),
                &root.identity,
                ResourceScope {
                    transaction: "0".repeat(32),
                    fingerprint: "0".repeat(64),
                },
            )?;
            let value = files::execute(&files, None, &request.inspection.action).await?;
            if !root.current() {
                return Err("The physical project folder changed during inspection".into());
            }
            Ok(value)
        })
        .await
        .map_err(|error| format!("Workspace inspection stopped: {error}"))?
    }

    pub async fn request(
        &self,
        workspace: PathBuf,
        owner: String,
        request: FolderRequest,
    ) -> Result<FolderReply, String> {
        request.validate()?;
        if matches!(request.action, FolderAction::Execute { .. }) {
            return Err("Complete native execution must use the workspace transaction host".into());
        }
        if owner.is_empty() || owner.len() > 128 {
            return Err("Invalid folder authority".into());
        }
        let registration = self.coordinator.worker()?;
        let host = self.clone();
        // Disconnecting the observer cannot cancel an accepted filesystem call.
        tokio::spawn(async move {
            let _registration = registration;
            host.owned(workspace, owner, request).await
        })
        .await
        .map_err(|error| format!("Folder request stopped: {error}"))?
    }
    async fn owned(
        &self,
        workspace: PathBuf,
        owner: String,
        request: FolderRequest,
    ) -> Result<FolderReply, String> {
        // Resolve the capability briefly under the session mutex, then wait
        // without that mutex. Every consumer takes the folder lease first.
        let opened = if let FolderAction::Open { path, .. } = &request.action {
            Some(Transactions::capture_root(
                &workspace,
                path,
                request.user,
                request.project,
            )?)
        } else {
            None
        };
        let folder = if let Some(root) = &opened {
            root.folder.clone()
        } else {
            let state = self.coordinator.prepare(self.state.lock()).await?;
            let id = match &request.action {
                FolderAction::Prove { session, .. }
                | FolderAction::File { session, .. }
                | FolderAction::Admit { session, .. }
                | FolderAction::Advance { session, .. }
                | FolderAction::Execute { session, .. }
                | FolderAction::Close { session } => session,
                FolderAction::Open { .. } => unreachable!(),
            };
            let session = state
                .sessions
                .get(id)
                .ok_or("The folder session expired; reconnect the selected folder")?;
            session.authorize(&request, &owner)?;
            session.root.folder.clone()
        };
        let lease = self.coordinator.acquire(folder).await?;
        let mut state = self.state.lock().await;
        state.sweep();
        if let FolderAction::Open { expected, .. } = &request.action {
            if state.sessions.len() >= MAX_SESSIONS {
                return Err(
                    "Too many folder sessions; close an existing review or retry later".into(),
                );
            }
            let root = opened.unwrap();
            if expected
                .as_ref()
                .is_some_and(|expected| *expected != root.identity)
            {
                return Err(
                    "Reconnect the original physical project folder before recovery".into(),
                );
            }
            let id = format!("{:032x}", rand::random::<u128>());
            let challenge = Challenge::new(&root, &id, self.coordinator.clone(), &lease)?;
            let reply = FolderReply::Opened {
                session: id.clone(),
                challenge: challenge.name().into(),
                identity: root.identity.clone(),
            };
            state.sessions.insert(
                id.clone(),
                Session {
                    owner,
                    user: request.user,
                    project: request.project,
                    root,
                    challenge: Some(challenge),
                    status: Status::Pending,
                    touched: Instant::now(),
                    scope: None,
                    worker: None,
                    journal: None,
                    sequence: 0,
                    last: None,
                },
            );
            self.reap();
            return Ok(reply);
        }
        let id = match &request.action {
            FolderAction::Prove { session, .. }
            | FolderAction::File { session, .. }
            | FolderAction::Admit { session, .. }
            | FolderAction::Advance { session, .. }
            | FolderAction::Execute { session, .. }
            | FolderAction::Close { session } => session,
            FolderAction::Open { .. } => unreachable!(),
        };
        let session = state
            .sessions
            .get(id)
            .ok_or("The folder session expired; reconnect the selected folder")?;
        session.authorize(&request, &owner)?;
        if let FolderAction::Close { .. } = request.action {
            let session = state.sessions.get_mut(id).unwrap();
            let released = match session.journal.take() {
                Some(journal) => journal.release().await,
                None => Ok(()),
            };
            if let Some(challenge) = session.challenge.as_mut()
                && let Err(error) = challenge.clear(&lease)
            {
                tracing::warn!(%error, "Folder proof cleanup needs attention");
            }
            session.retire();
            released?;
            return Ok(FolderReply::Closed);
        }
        if let FolderAction::Prove { proof, .. } = &request.action {
            let session = state.sessions.get_mut(id).unwrap();
            if session.status == Status::Active {
                return Ok(FolderReply::Ready {
                    identity: session.root.identity.clone(),
                    worker: session
                        .worker
                        .as_ref()
                        .ok_or("The paired worker is unavailable")?
                        .delegation_key(),
                });
            }
            let challenge = session
                .challenge
                .as_mut()
                .ok_or("The folder challenge is unavailable")?;
            if !challenge.proves(proof) {
                return Err(
                    "The browser did not prove access to the selected physical folder".into(),
                );
            }
            challenge.clear(&lease)?;
            let identity = session.root.identity.clone();
            // All previously accepted calls hold this same gate until terminal.
            // A delayed call from any older window now fails before filesystem I/O.
            let selected = state.sessions.remove(id).unwrap();
            for older in state.sessions.values_mut() {
                if older.root.folder.overlaps(&selected.root.folder) {
                    older.retire();
                }
            }
            state.sessions.insert(id.clone(), selected);
            let session = state.sessions.get_mut(id).unwrap();
            session.challenge.take();
            session.status = Status::Active;
            session.worker = Some(WorkspaceTransactionWorker {
                id: format!("{:032x}", rand::random::<u128>()),
                secret: format!("{:032x}", rand::random::<u128>()),
            });
            session.touched = Instant::now();
            return Ok(FolderReply::Ready {
                identity,
                worker: session.worker.as_ref().unwrap().delegation_key(),
            });
        }
        let session = state.sessions.get_mut(id).unwrap();
        if session.status != Status::Active {
            return Err("Prove browser folder access before using native files".into());
        }
        if let FolderAction::Admit {
            api_url,
            delegation,
            ..
        } = &request.action
        {
            if delegation.scope.root_identity != session.root.identity
                || session
                    .scope
                    .as_ref()
                    .is_some_and(|scope| scope.transaction != delegation.scope.id)
            {
                return Err("The delegated journal does not match this folder session".into());
            }
            if let Some(journal) = &session.journal {
                if !journal.matches(api_url, delegation) {
                    return Err("The folder session already owns another journal".into());
                }
                journal.admit().await?;
            } else {
                let journal = PairedJournal::new(
                    api_url.clone(),
                    &session.root.identity,
                    delegation.clone(),
                    session
                        .worker
                        .as_ref()
                        .ok_or("The paired worker is unavailable")?
                        .clone(),
                )?;
                // Retain the exact client even if admission's reply is lost.
                // Repeating admission is safe under the same physical gate.
                session.journal = Some(journal);
                session.journal.as_ref().unwrap().admit().await?;
            }
            session.touched = Instant::now();
            return Ok(FolderReply::Admitted);
        }
        if let FolderAction::Advance { request, .. } = &request.action {
            let journal = session
                .journal
                .as_ref()
                .ok_or("Admit the paired journal before advancing recovery")?;
            if journal.identity() != (request.id.as_str(), request.generation.as_str()) {
                return Err("Paired recovery belongs to another reviewed operation".into());
            }
            journal.scope().await?;
            let response = journal
                .advance(request.revision, request.progress.clone())
                .await?;
            session.touched = Instant::now();
            return Ok(FolderReply::Advanced { response });
        }
        let FolderAction::File {
            sequence, action, ..
        } = &request.action
        else {
            unreachable!()
        };
        let session = state.sessions.get_mut(id).unwrap();
        if session.status != Status::Active {
            return Err("Prove browser folder access before using native files".into());
        }
        // Check before the duplicate acknowledgement too: an old cached success
        // is not authority after another native worker has taken over.
        if action.mutates() {
            let journal = session
                .journal
                .as_ref()
                .ok_or("Admit the paired journal before changing native resources")?;
            if session
                .scope
                .as_ref()
                .is_none_or(|scope| scope.transaction != journal.identity().0)
            {
                return Err("Bind the admitted resource transaction before mutation".into());
            }
            journal.mutation_scope().await?;
        }
        let fingerprint = openwebide_core::plugins::content_digest(
            &serde_json::to_vec(action).map_err(|error| error.to_string())?,
        );
        if *sequence == session.sequence {
            return match &session.last {
                Some((previous, result)) if *previous == fingerprint => Ok(FolderReply::File {
                    outcome: result.clone().ok_or(
                        "The read acknowledgement was too large to retain; reconnect the folder",
                    )?,
                }),
                _ => Err("A folder sequence cannot be reused for another operation".into()),
            };
        }
        if session.sequence.checked_add(1) != Some(*sequence) {
            return Err("Folder requests must be acknowledged in sequence".into());
        }
        session.sequence = *sequence;
        session.touched = Instant::now();
        session.last = Some((fingerprint.clone(), None));
        let result = if let FileAction::Bind { scope } = action {
            if session
                .journal
                .as_ref()
                .is_some_and(|journal| journal.identity().0 != scope.transaction)
                || session
                    .scope
                    .as_ref()
                    .is_some_and(|previous| previous != scope)
            {
                Err("Folder session already belongs to another reviewed transaction".into())
            } else {
                let files = ResourceFiles::new(
                    session.root.handle.as_file(),
                    &session.root.identity,
                    scope.clone(),
                )?;
                if files.supports_resource_moves() {
                    session.scope = Some(scope.clone());
                    Ok(FileValue::Unit)
                } else {
                    Err("Native recovery ownership does not match this reviewed transaction".into())
                }
            }
        } else if action.needs_scope() && session.scope.is_none() {
            Err("Bind the reviewed transaction before changing native resources".into())
        } else {
            let files = session.files(id)?;
            files::execute(&files, session.scope.as_ref(), action).await
        };
        let result = if session.root.current() {
            result
        } else {
            Err("The physical folder changed during its filesystem call".into())
        };
        let cached = serde_json::to_vec(&result)
            .ok()
            .filter(|bytes| bytes.len() <= CACHE_LIMIT)
            .map(|_| result.clone());
        session.last = Some((fingerprint, cached));
        Ok(FolderReply::File { outcome: result })
    }
    fn reap(&self) {
        if self.reaping.swap(true, Ordering::Relaxed) {
            return;
        }
        let weak = Arc::downgrade(&self.state);
        let reaping = self.reaping.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(PROOF_TTL).await;
                let Some(state) = weak.upgrade() else {
                    return;
                };
                let mut state = state.lock().await;
                state.sweep();
                if state.sessions.is_empty() {
                    reaping.store(false, Ordering::Relaxed);
                    return;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests;
