//! Physical directory exclusion for accepted native work. Ancestor identities
//! cover nested projects, symlink aliases and alternate paths to the same root.
//! Waiting futures are cancellable; accepted workers retain their own lease.
use std::{
    collections::BTreeMap,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::Notify;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod original;
mod physical;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use original::OriginalFolder;

#[derive(Default, Clone, PartialEq, Eq, Debug)]
pub(crate) struct FileIdentity(pub Vec<u8>);
impl Hasher for FileIdentity {
    fn finish(&self) -> u64 {
        0
    }
    fn write(&mut self, bytes: &[u8]) {
        self.0
            .extend_from_slice(&u64::try_from(bytes.len()).unwrap().to_le_bytes());
        self.0.extend_from_slice(bytes);
    }
}
fn identity(handle: &same_file::Handle) -> FileIdentity {
    let mut result = FileIdentity::default();
    handle.hash(&mut result);
    result
}

#[derive(Clone, Debug)]
pub(crate) struct Folder {
    pub path: PathBuf,
    pub handle: Arc<same_file::Handle>,
    physical: FileIdentity,
    ancestors: Vec<FileIdentity>,
    #[cfg(unix)]
    original: crate::file_identity::Identity,
    #[cfg(unix)]
    original_ancestors: Vec<crate::file_identity::Identity>,
}
impl Folder {
    /// OS-owner recovery uses the same physical exclusion as accepted writers.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn with_exclusive_access<T>(
        &self,
        work: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let _access = physical::Access::try_acquire(self)?
            .ok_or("The original folder still has an active native owner or watcher")?;
        if !self.current() {
            return Err("Reconnect the original physical recovery folder".into());
        }
        work()
    }
    pub fn capture(path: &Path) -> Result<Self, String> {
        let path = path.canonicalize().map_err(|error| error.to_string())?;
        if openwebide_core::vfs::private_workspace_path(&path.to_string_lossy().replace('\\', "/"))
        {
            return Err("Private runtime folders cannot be execution workspaces".into());
        }
        Self::observe(path)
    }
    /// Capture OS-selected recovery state without admitting an execution root.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn capture_state(path: &Path) -> Result<Self, String> {
        Self::observe(path.canonicalize().map_err(|error| error.to_string())?)
    }
    fn observe(path: PathBuf) -> Result<Self, String> {
        let handle = same_file::Handle::from_path(&path).map_err(|error| error.to_string())?;
        if !handle
            .as_file()
            .metadata()
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            return Err("The execution folder is not a directory".into());
        }
        let physical = identity(&handle);
        #[cfg(unix)]
        let original = crate::file_identity::Identity::of(handle.as_file())
            .map_err(|error| error.to_string())?;
        // Retain only identities for ancestors. Holding the selected root open
        // prevents its identity from being reused while an operation is queued.
        let mut ancestors = Vec::new();
        #[cfg(unix)]
        let mut original_ancestors = Vec::new();
        for parent in path.ancestors().skip(1) {
            let handle = same_file::Handle::from_path(parent).map_err(|error| error.to_string())?;
            ancestors.push(identity(&handle));
            #[cfg(unix)]
            original_ancestors.push(
                crate::file_identity::Identity::of(handle.as_file())
                    .map_err(|error| error.to_string())?,
            );
        }
        Ok(Self {
            path,
            handle: Arc::new(handle),
            physical,
            ancestors,
            #[cfg(unix)]
            original,
            #[cfg(unix)]
            original_ancestors,
        })
    }
    pub fn current(&self) -> bool {
        #[cfg(unix)]
        if self.original_ancestors.len() != self.ancestors.len()
            || !crate::file_identity::Identity::of(self.handle.as_file())
                .is_ok_and(|observed| observed == self.original)
        {
            return false;
        }
        self.path.canonicalize().is_ok_and(|path| path == self.path)
            && same_file::Handle::from_path(&self.path).is_ok_and(|handle| handle == *self.handle)
            && self
                .path
                .ancestors()
                .skip(1)
                .zip(&self.ancestors)
                .enumerate()
                .all(|(index, (path, expected))| {
                    same_file::Handle::from_path(path).is_ok_and(|handle| {
                        if identity(&handle) != *expected {
                            return false;
                        }
                        #[cfg(unix)]
                        {
                            crate::file_identity::Identity::of(handle.as_file())
                                .is_ok_and(|observed| observed == self.original_ancestors[index])
                        }
                        #[cfg(not(unix))]
                        {
                            let _ = index;
                            true
                        }
                    })
                })
    }
    pub fn overlaps(&self, other: &Self) -> bool {
        self.physical == other.physical
            || self.ancestors.contains(&other.physical)
            || other.ancestors.contains(&self.physical)
    }
}

/// Ordinary editing and host work may coexist. Reviewed changes retain the
/// complete folder until their effects and recovery have been acknowledged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkspaceUse {
    Ordinary,
    Reviewed,
}
impl WorkspaceUse {
    fn conflicts(self, other: Self) -> bool {
        self != Self::Ordinary || other != Self::Ordinary
    }
}

#[derive(Clone, Default, Debug)]
pub(crate) struct Coordinator(Arc<Shared>);
#[derive(Default, Debug)]
struct Shared {
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    default_recovery_home: crate::native_recovery::TestHome,
    #[cfg(all(
        any(test, feature = "test-support"),
        any(target_os = "linux", target_os = "macos")
    ))]
    recovery_home: Mutex<Option<Arc<PathBuf>>>,
    state: Mutex<State>,
    changed: Notify,
}
#[derive(Default, Debug)]
struct State {
    next: u64,
    closing: bool,
    workers: usize,
    active: BTreeMap<u64, Folder>,
    waiting: BTreeMap<u64, Folder>,
    uses: BTreeMap<u64, WorkspaceUse>,
    uncertain: BTreeMap<u64, Uncertain>,
}
#[derive(Debug)]
struct Uncertain {
    folder: Folder,
    _physical: Option<physical::Access>,
}
pub(crate) struct Lease {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    recovery: Option<crate::native_recovery::Permit>,
    shared: Arc<Shared>,
    id: u64,
    physical: Option<physical::Access>,
    uncertain: std::sync::atomic::AtomicBool,
    proof_only: bool,
    workspace_use: WorkspaceUse,
}
pub(crate) struct Worker {
    shared: Arc<Shared>,
}
impl Lease {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn ordinary(&self) -> bool {
        self.workspace_use == WorkspaceUse::Ordinary
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn recovery(&self) -> Option<&crate::native_recovery::Permit> {
        self.recovery.as_ref()
    }
    pub(crate) fn covers(&self, folder: &Folder) -> bool {
        self.workspace_use == WorkspaceUse::Reviewed && self.permits_files(folder)
    }
    pub(crate) fn permits_files(&self, folder: &Folder) -> bool {
        !self.proof_only && self.proof_covers(folder)
    }
    pub(crate) fn proof_covers(&self, folder: &Folder) -> bool {
        !self.uncertain.load(std::sync::atomic::Ordering::Acquire) && self.retains_proof(folder)
    }
    /// Private recovery publication can retain a negative cleanup result while
    /// the same physical lease is quarantined. This grants no workspace writes.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn retains(&self, folder: &Folder) -> bool {
        !self.proof_only
            && self.physical.is_some()
            && self
                .shared
                .state
                .lock()
                .unwrap()
                .active
                .get(&self.id)
                .is_some_and(|held| {
                    held.physical == folder.physical && held.handle == folder.handle
                })
    }
    fn retains_proof(&self, folder: &Folder) -> bool {
        self.shared
            .state
            .lock()
            .unwrap()
            .active
            .get(&self.id)
            .is_some_and(|held| {
                held.current()
                    && folder.current()
                    && (held.physical == folder.physical
                        || folder.ancestors.contains(&held.physical))
            })
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn command_descriptors(
        &self,
        root: &std::path::Path,
    ) -> Result<Vec<std::os::fd::OwnedFd>, String> {
        let folder = Folder::capture(root)?;
        if !self.permits_files(&folder) {
            return Err("The command no longer owns its original execution folder".into());
        }
        self.physical
            .as_ref()
            .ok_or("Native folder access is unavailable")?
            .duplicate_descriptors()
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub async fn watch_process(
        &self,
        owner: &crate::exec::process_owner::Owner,
    ) -> Result<crate::exec::process_owner::Claim, String> {
        owner.retain(self.process_descriptors()?).await
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) async fn watch_command_process(
        &self,
        owner: &crate::exec::process_owner::Owner,
        cleanup: crate::native_recovery::ProcessCleanup,
    ) -> Result<crate::exec::process_owner::Claim, String> {
        owner
            .retain_command(self.process_descriptors()?, cleanup)
            .await
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn process_descriptors(&self) -> Result<Vec<std::os::fd::OwnedFd>, String> {
        if self.proof_only {
            return Err("Folder proof access cannot authorize a process".into());
        }
        self.physical
            .as_ref()
            .ok_or("Native folder access is unavailable")?
            .duplicate_descriptors()
    }
    /// Failed process cleanup cannot make overlapping folders available again.
    /// Keep their physical identities until this execution host is restarted.
    #[cfg(test)]
    pub fn quarantine(self) {
        self.mark_uncertain();
    }
    pub(crate) fn mark_uncertain(&self) {
        self.uncertain
            .store(true, std::sync::atomic::Ordering::Release);
        // Stop local admission before publishing the physical quarantine.
        self.shared
            .state
            .lock()
            .unwrap()
            .uses
            .insert(self.id, WorkspaceUse::Reviewed);
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if self.ordinary()
            && let Some(physical) = &self.physical
            && let Err(error) = physical.quarantine()
        {
            // Retain every original descriptor and reject this host's new work.
            // An OS locking failure cannot be reported as confirmed cleanup.
            self.shared.state.lock().unwrap().closing = true;
            tracing::error!(%error, "Native workspace quarantine could not be published");
        }
        self.shared.changed.notify_waiters();
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap().workers -= 1;
        self.shared.changed.notify_waiters();
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if self.uncertain.load(std::sync::atomic::Ordering::Acquire) {
            let mut state = self.shared.state.lock().unwrap();
            state.uses.remove(&self.id);
            if let Some(folder) = state.active.remove(&self.id) {
                state.uncertain.insert(
                    self.id,
                    Uncertain {
                        folder,
                        _physical: self.physical.take(),
                    },
                );
            }
        } else {
            // Release OS exclusion before notifying an ordinary local waiter.
            #[cfg(unix)]
            drop(self.physical.take());
            let mut state = self.shared.state.lock().unwrap();
            state.active.remove(&self.id);
            state.uses.remove(&self.id);
        }
        self.shared.changed.notify_waiters();
    }
}
struct Waiting {
    shared: Arc<Shared>,
    id: u64,
    armed: bool,
}
impl Drop for Waiting {
    fn drop(&mut self) {
        if self.armed {
            let mut state = self.shared.state.lock().unwrap();
            state.waiting.remove(&self.id);
            state.uses.remove(&self.id);
            self.shared.changed.notify_waiters();
        }
    }
}
impl Coordinator {
    /// Register before spawning, so shutdown cannot miss a not-yet-polled task.
    pub fn worker(&self) -> Result<Worker, String> {
        let mut state = self.0.state.lock().unwrap();
        if state.closing {
            return Err("Workspace execution is stopping".into());
        }
        if state.workers >= 256 {
            return Err("Too many native workspace workers".into());
        }
        state.workers += 1;
        Ok(Worker {
            shared: self.0.clone(),
        })
    }
    pub async fn stopped(&self) {
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.0.state.lock().unwrap().closing {
                return;
            }
            changed.await;
        }
    }
    /// Preparation may be abandoned. Accepted mutations instead retain their
    /// lease and finish their cleanup before the worker registration is dropped.
    pub async fn prepare<F: std::future::Future>(&self, work: F) -> Result<F::Output, String> {
        tokio::select! {
            biased;
            () = self.stopped() => Err("Workspace execution is stopping".into()),
            result = work => Ok(result),
        }
    }
    pub async fn shutdown(&self) {
        {
            let mut state = self.0.state.lock().unwrap();
            state.closing = true;
            state.waiting.clear();
        }
        self.0.changed.notify_waiters();
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let done = {
                let state = self.0.state.lock().unwrap();
                state.active.is_empty() && state.workers == 0
            };
            if done {
                return;
            }
            changed.await;
        }
    }
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.0.state.lock().unwrap().waiting.len()
    }
    #[cfg(all(
        any(test, feature = "test-support"),
        any(target_os = "linux", target_os = "macos")
    ))]
    pub(crate) fn workers(&self) -> usize {
        self.0.state.lock().unwrap().workers
    }
    #[cfg(all(
        any(test, feature = "test-support"),
        any(target_os = "linux", target_os = "macos")
    ))]
    pub(crate) fn set_recovery_home(&self, home: Arc<PathBuf>) {
        let mut configured = self.0.recovery_home.lock().unwrap();
        assert!(
            configured
                .as_ref()
                .is_none_or(|existing| **existing == *home),
            "A live coordinator cannot change recovery namespaces"
        );
        *configured = Some(home);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn recovery_home(&self) -> Result<Arc<PathBuf>, String> {
        if self.0.state.lock().unwrap().closing {
            return Err("Execution host is stopping".into());
        }
        #[cfg(test)]
        {
            Ok(self
                .0
                .recovery_home
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| self.0.default_recovery_home.0.clone()))
        }
        #[cfg(not(test))]
        {
            #[cfg(feature = "test-support")]
            if let Some(home) = self.0.recovery_home.lock().unwrap().clone() {
                return Ok(home);
            }
            Ok(Arc::new(crate::native_recovery::home()?))
        }
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn recovery_namespace(&self) -> Result<crate::native_recovery::Namespace, String> {
        let home = self.recovery_home()?;
        crate::native_recovery::Namespace::open(&home)
    }
    pub async fn acquire(&self, folder: Folder) -> Result<Lease, String> {
        self.acquire_in(
            folder,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            None,
            false,
            WorkspaceUse::Reviewed,
        )
        .await
    }
    pub(crate) async fn acquire_ordinary(&self, folder: Folder) -> Result<Lease, String> {
        self.acquire_in(
            folder,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            None,
            false,
            WorkspaceUse::Ordinary,
        )
        .await
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) async fn acquire_recovery(
        &self,
        folder: Folder,
        permit: crate::native_recovery::Permit,
    ) -> Result<Lease, String> {
        self.acquire_in(folder, Some(permit), false, WorkspaceUse::Reviewed)
            .await
    }
    /// Private challenge/read access never authorizes workspace mutations or processes.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) async fn acquire_proof(&self, folder: Folder) -> Result<Lease, String> {
        self.acquire_in(folder, None, true, WorkspaceUse::Ordinary)
            .await
    }
    async fn acquire_in(
        &self,
        folder: Folder,
        #[cfg(any(target_os = "linux", target_os = "macos"))] recovery: Option<
            crate::native_recovery::Permit,
        >,
        proof_only: bool,
        workspace_use: WorkspaceUse,
    ) -> Result<Lease, String> {
        let id = {
            let mut state = self.0.state.lock().unwrap();
            if state.closing {
                return Err("Workspace execution is stopping".into());
            }
            if state
                .uncertain
                .values()
                .any(|held| folder.overlaps(&held.folder))
            {
                return Err("Native process cleanup is unconfirmed for this folder; stop the original processes before restarting the execution host".into());
            }
            if state.active.len() + state.waiting.len() + state.uncertain.len() >= 256 {
                return Err("Too many workspace operations are waiting for folder access".into());
            }
            state.next = state
                .next
                .checked_add(1)
                .ok_or("Workspace operation identity exhausted")?;
            let id = state.next;
            state.waiting.insert(id, folder.clone());
            state.uses.insert(id, workspace_use);
            id
        };
        let mut waiting = Waiting {
            shared: self.0.clone(),
            id,
            armed: true,
        };
        let mut delay = std::time::Duration::from_millis(10);
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let blocked = {
                let state = self.0.state.lock().unwrap();
                if state.closing {
                    return Err("Workspace execution is stopping".into());
                }
                if state
                    .uncertain
                    .values()
                    .any(|held| folder.overlaps(&held.folder))
                {
                    return Err("Native process cleanup is unconfirmed for this folder; stop the original processes before restarting the execution host".into());
                }
                let conflicts = |id: &u64, held: &Folder| {
                    folder.overlaps(held)
                        && workspace_use.conflicts(
                            state
                                .uses
                                .get(id)
                                .copied()
                                .unwrap_or(WorkspaceUse::Reviewed),
                        )
                };
                state
                    .active
                    .iter()
                    .any(|(id, active)| conflicts(id, active))
                    || state
                        .waiting
                        .range(..id)
                        .any(|(id, earlier)| conflicts(id, earlier))
            };
            if blocked {
                changed.await;
                delay = std::time::Duration::from_millis(10);
                continue;
            }
            // Try the complete ancestor/root lock set without awaiting while
            // holding any of it. A frozen preparer retains no OS locks or handle
            // permits, and shutdown need not wait for it to be polled again.
            if let Some(physical) = physical::Access::for_use(&folder, workspace_use)? {
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                {
                    let namespace = self.recovery_namespace()?;
                    if workspace_use == WorkspaceUse::Ordinary {
                        namespace.prepare_ordinary_admission()?;
                    }
                    if proof_only {
                        namespace.ensure_current()?;
                    } else {
                        namespace.admit(&folder, recovery.as_ref())?;
                    }
                }
                let mut state = self.0.state.lock().unwrap();
                if state.closing {
                    return Err("Workspace execution is stopping".into());
                }
                if !folder.current() {
                    return Err("The physical execution folder changed while waiting".into());
                }
                state.waiting.remove(&id);
                state.active.insert(id, folder.clone());
                waiting.armed = false;
                return Ok(Lease {
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                    recovery,
                    shared: self.0.clone(),
                    id,
                    physical: Some(physical),
                    uncertain: std::sync::atomic::AtomicBool::new(false),
                    proof_only,
                    workspace_use,
                });
            }
            tokio::select! {
                () = changed => {},
                () = tokio::time::sleep(delay) => {},
            }
            delay = (delay * 2).min(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn quarantine_command(descriptors: &[std::os::fd::OwnedFd]) -> Result<(), String> {
    physical::quarantine_descriptors(descriptors)
}

#[cfg(test)]
mod ordinary_tests;
#[cfg(test)]
mod tests;
