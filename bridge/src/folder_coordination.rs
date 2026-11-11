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
mod physical;

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
}
impl Folder {
    pub fn capture(path: &Path) -> Result<Self, String> {
        let path = path.canonicalize().map_err(|error| error.to_string())?;
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
        // Retain only identities for ancestors. Holding the selected root open
        // prevents its identity from being reused while an operation is queued.
        let ancestors = path
            .ancestors()
            .skip(1)
            .map(|parent| {
                same_file::Handle::from_path(parent)
                    .map(|handle| identity(&handle))
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            path,
            handle: Arc::new(handle),
            physical,
            ancestors,
        })
    }
    pub fn current(&self) -> bool {
        self.path.canonicalize().is_ok_and(|path| path == self.path)
            && same_file::Handle::from_path(&self.path).is_ok_and(|handle| handle == *self.handle)
            && self
                .path
                .ancestors()
                .skip(1)
                .zip(&self.ancestors)
                .all(|(path, expected)| {
                    same_file::Handle::from_path(path)
                        .is_ok_and(|handle| identity(&handle) == *expected)
                })
    }
    pub fn overlaps(&self, other: &Self) -> bool {
        self.physical == other.physical
            || self.ancestors.contains(&other.physical)
            || other.ancestors.contains(&self.physical)
    }
}

#[derive(Clone, Default, Debug)]
pub(crate) struct Coordinator(Arc<Shared>);
#[derive(Default, Debug)]
struct Shared {
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
    uncertain: BTreeMap<u64, Uncertain>,
}
#[derive(Debug)]
struct Uncertain {
    folder: Folder,
    _physical: Option<physical::Access>,
}
pub(crate) struct Lease {
    shared: Arc<Shared>,
    id: u64,
    physical: Option<physical::Access>,
}
pub(crate) struct Worker {
    shared: Arc<Shared>,
}
impl Lease {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn covers(&self, folder: &Folder) -> bool {
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
    pub async fn watch_process(
        &self,
        owner: &crate::exec::process_owner::Owner,
    ) -> Result<crate::exec::process_owner::Claim, String> {
        let descriptors = self
            .physical
            .as_ref()
            .ok_or("Native folder access is unavailable")?
            .duplicate_descriptors()?;
        owner.retain(descriptors).await
    }
    /// Failed process cleanup cannot make overlapping folders available again.
    /// Keep their physical identities until this execution host is restarted.
    pub fn quarantine(mut self) {
        let mut state = self.shared.state.lock().unwrap();
        if let Some(folder) = state.active.remove(&self.id) {
            state.uncertain.insert(
                self.id,
                Uncertain {
                    folder,
                    _physical: self.physical.take(),
                },
            );
        }
        drop(state);
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
        // Release OS exclusion before notifying a local waiter, so its first
        // attempt need not back off on a lease that has already completed.
        #[cfg(unix)]
        drop(self.physical.take());
        self.shared.state.lock().unwrap().active.remove(&self.id);
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
            self.shared.state.lock().unwrap().waiting.remove(&self.id);
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
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(crate) fn workers(&self) -> usize {
        self.0.state.lock().unwrap().workers
    }
    pub async fn acquire(&self, folder: Folder) -> Result<Lease, String> {
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
                state.active.values().any(|active| folder.overlaps(active))
                    || state
                        .waiting
                        .range(..id)
                        .any(|(_, earlier)| folder.overlaps(earlier))
            };
            if blocked {
                changed.await;
                delay = std::time::Duration::from_millis(10);
                continue;
            }
            // Try the complete ancestor/root lock set without awaiting while
            // holding any of it. A frozen preparer retains no OS locks or handle
            // permits, and shutdown need not wait for it to be polled again.
            if let Some(physical) = physical::Access::try_acquire(&folder)? {
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
                    shared: self.0.clone(),
                    id,
                    physical: Some(physical),
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

#[cfg(test)]
mod tests;
