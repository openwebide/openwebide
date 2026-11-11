//! Lock real directory objects, not replaceable sentinel paths. Shared ancestor
//! locks and an exclusive selected-root lock exclude nested physical projects.
//! These descriptors are currently owned by the bridge process; they do not yet
//! establish exclusion against descendants surviving a crashed bridge.
use super::super::{Folder, identity};
use std::{
    fs::TryLockError,
    path::PathBuf,
    sync::{Arc, OnceLock},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const MAX_DIRECTORIES: usize = 64;
struct Budget {
    capacity: usize,
    available: Arc<Semaphore>,
}
impl Budget {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            available: Arc::new(Semaphore::new(capacity)),
        }
    }
}
fn budget() -> &'static Budget {
    static BUDGET: OnceLock<Budget> = OnceLock::new();
    BUDGET.get_or_init(|| {
        let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
        // SAFETY: getrlimit initializes this appropriately sized output on
        // success; the value is only read after checking its return code.
        let capacity = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) } == 0 {
            // SAFETY: successful getrlimit initialized both fields.
            usize::try_from(unsafe { limit.assume_init() }.rlim_cur / 8)
                .unwrap_or(256)
                .clamp(8, 256)
        } else {
            32
        };
        Budget::new(capacity)
    })
}

#[derive(Debug)]
struct Directory {
    path: PathBuf,
    handle: same_file::Handle,
}
#[derive(Debug)]
pub(in super::super) struct Access {
    directories: Vec<Directory>,
    _permits: OwnedSemaphorePermit,
}
impl Access {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn duplicate_descriptors(&self) -> Result<Vec<std::os::fd::OwnedFd>, String> {
        use std::os::fd::AsRawFd;
        self.directories
            .iter()
            .map(|directory| {
                crate::exec::process_owner::duplicate(directory.handle.as_file().as_raw_fd())
            })
            .collect()
    }
    pub fn try_acquire(folder: &Folder) -> Result<Option<Self>, String> {
        Self::with_budget(folder, budget())
    }
    fn with_budget(folder: &Folder, budget: &Budget) -> Result<Option<Self>, String> {
        if !folder.current() {
            return Err("The physical execution folder changed while waiting".into());
        }
        let count = folder.ancestors.len().saturating_add(1);
        if count > MAX_DIRECTORIES || count > budget.capacity {
            return Err("The execution folder exceeds native directory-lock capacity".into());
        }
        let Ok(permits) = budget
            .available
            .clone()
            .try_acquire_many_owned(u32::try_from(count).unwrap())
        else {
            return Ok(None);
        };
        let mut selected = folder
            .path
            .ancestors()
            .zip(std::iter::once(&folder.physical).chain(&folder.ancestors))
            .collect::<Vec<_>>();
        selected.reverse();
        let mut directories = Vec::with_capacity(count);
        for (path, expected) in selected {
            let handle = same_file::Handle::from_path(path).map_err(|error| error.to_string())?;
            if !handle
                .as_file()
                .metadata()
                .map_err(|error| error.to_string())?
                .is_dir()
                || identity(&handle) != *expected
            {
                return Err("The execution folder ancestry changed before native access".into());
            }
            let lock = if path == folder.path {
                handle.as_file().try_lock()
            } else {
                handle.as_file().try_lock_shared()
            };
            match lock {
                Ok(()) => directories.push(Directory {
                    path: path.to_owned(),
                    handle,
                }),
                Err(TryLockError::WouldBlock) => return Ok(None),
                Err(TryLockError::Error(error)) => {
                    return Err(format!("Cannot coordinate native folder access: {error}"));
                }
            }
        }
        if directories.iter().any(|held| {
            !same_file::Handle::from_path(&held.path).is_ok_and(|current| current == held.handle)
        }) {
            return Err(
                "The execution folder ancestry changed while acquiring native access".into(),
            );
        }
        Ok(Some(Self {
            directories,
            _permits: permits,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn blocked_attempt_releases_ancestor_locks_and_the_complete_handle_reservation() {
        // A concurrent fork can inherit our temporary shared flock until exec,
        // even with CLOEXEC. Run the unchanged immediate-release assertions in
        // a fresh process that creates its fixture after exec and spawns no peer.
        if std::env::var_os("OPENWEBIDE_TEST_PHYSICAL_RELEASE_ISOLATED").as_deref()
            != Some(std::ffi::OsStr::new("1"))
        {
            let output = tokio::time::timeout(std::time::Duration::from_secs(5),
                tokio::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "folder_coordination::physical::unix::tests::blocked_attempt_releases_ancestor_locks_and_the_complete_handle_reservation", "--nocapture"])
                    .env("OPENWEBIDE_TEST_PHYSICAL_RELEASE_ISOLATED", "1")
                    .kill_on_drop(true).output(),
            ).await.expect("Isolated physical release probe timed out").unwrap();
            assert!(
                output.status.success(),
                "Isolated physical release probe failed: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let folder = Folder::capture(&root).unwrap();
        let capacity = folder.ancestors.len() + 1;
        let budget = Budget::new(capacity);
        let foreign = std::fs::File::open(&root).unwrap();
        foreign.try_lock().unwrap();
        assert!(Access::with_budget(&folder, &budget).unwrap().is_none());
        assert_eq!(budget.available.available_permits(), capacity);
        // A failed full-set acquisition cannot leave an ancestor read-locked.
        std::fs::File::open(directory.path())
            .unwrap()
            .try_lock()
            .unwrap();
        drop(foreign);
        let held = Access::with_budget(&folder, &budget).unwrap().unwrap();
        assert_eq!(budget.available.available_permits(), 0);
        drop(held);
        assert_eq!(budget.available.available_permits(), capacity);
    }

    #[test]
    fn insufficient_handle_capacity_fails_without_reserving_partial_access() {
        let directory = tempfile::tempdir().unwrap();
        let folder = Folder::capture(directory.path()).unwrap();
        let capacity = folder.ancestors.len();
        let budget = Budget::new(capacity);
        assert!(Access::with_budget(&folder, &budget).is_err());
        assert_eq!(budget.available.available_permits(), capacity);
        std::fs::File::open(directory.path())
            .unwrap()
            .try_lock()
            .unwrap();
    }
}
