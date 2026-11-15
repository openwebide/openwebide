//! Lock real directory objects, not replaceable sentinel paths. Shared ancestor
//! locks and an exclusive selected-root lock exclude nested physical projects.
//! Command watchers may retain these descriptors through bridge loss. They do
//! not establish durable recovery authority after all owners disappear.
use super::super::{Folder, WorkspaceUse, identity};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
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
    original: crate::file_identity::Identity,
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn quarantine(&self) -> Result<(), String> {
        quarantine(
            self.directories
                .iter()
                .map(|directory| directory.handle.as_file().as_fd()),
        )
    }
    pub fn try_acquire(folder: &Folder) -> Result<Option<Self>, String> {
        Self::for_use(folder, WorkspaceUse::Reviewed)
    }
    pub fn for_use(folder: &Folder, workspace_use: WorkspaceUse) -> Result<Option<Self>, String> {
        Self::with_usage(folder, budget(), workspace_use)
    }
    #[cfg(test)]
    fn with_budget(folder: &Folder, budget: &Budget) -> Result<Option<Self>, String> {
        Self::with_usage(folder, budget, WorkspaceUse::Reviewed)
    }
    fn with_usage(
        folder: &Folder,
        budget: &Budget,
        workspace_use: WorkspaceUse,
    ) -> Result<Option<Self>, String> {
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
            .zip(std::iter::once(&folder.original).chain(&folder.original_ancestors))
            .collect::<Vec<_>>();
        selected.reverse();
        let mut directories = Vec::with_capacity(count);
        for ((path, expected), original) in selected {
            let handle = same_file::Handle::from_path(path).map_err(|error| error.to_string())?;
            if !handle
                .as_file()
                .metadata()
                .map_err(|error| error.to_string())?
                .is_dir()
                || identity(&handle) != *expected
                || crate::file_identity::Identity::of(handle.as_file())
                    .map_err(|error| error.to_string())?
                    != *original
            {
                return Err("The execution folder ancestry changed before native access".into());
            }
            let selected_root = path == folder.path;
            let lock = if selected_root
                && (workspace_use == WorkspaceUse::Reviewed
                    || !cfg!(any(target_os = "linux", target_os = "macos")))
            {
                handle.as_file().try_lock()
            } else {
                shared_directory(handle.as_file())
            };
            match lock {
                Ok(()) => {
                    if !compatible_use(handle.as_file(), selected_root, workspace_use)? {
                        return Ok(None);
                    }
                    directories.push(Directory {
                        path: path.to_owned(),
                        handle,
                        original: original.clone(),
                    });
                }
                Err(TryLockError::WouldBlock) => return Ok(None),
                Err(TryLockError::Error(error)) => {
                    return Err(format!("Cannot coordinate native folder access: {error}"));
                }
            }
        }
        if directories.iter().any(|held| {
            !same_file::Handle::from_path(&held.path).is_ok_and(|current| {
                current == held.handle
                    && crate::file_identity::Identity::of(current.as_file())
                        .is_ok_and(|identity| identity == held.original)
            })
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

// Darwin reports a flock shared lock to OFD queries, including queries on the
// same descriptor. Its shared ancestry therefore uses range 2. Linux also keeps
// its independent flock for compatibility with older directory-lock owners.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn shared_directory(file: &std::fs::File) -> Result<(), TryLockError> {
    #[cfg(target_os = "linux")]
    file.try_lock_shared()?;
    read_range(file.as_fd(), 2, 1).map_err(|error| {
        if matches!(error.raw_os_error(), Some(libc::EACCES | libc::EAGAIN)) {
            TryLockError::WouldBlock
        } else {
            TryLockError::Error(error)
        }
    })
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn shared_directory(file: &std::fs::File) -> Result<(), TryLockError> {
    file.try_lock_shared()
}

// Purpose ranges: ordinary root=0, reviewed ancestor=1, shared ancestry=2.
// Quarantine extends ancestry to 3 on ancestors and 4 on the selected root.
// Every new user checks root quarantine on its complete path, and checks for
// quarantined descendants at its selected root. Siblings remain independent.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn compatible_use(
    file: &std::fs::File,
    selected_root: bool,
    workspace_use: WorkspaceUse,
) -> Result<bool, String> {
    // The complete exclusive flock already excludes every shared description;
    // Darwin would also report that same flock in this descriptor's queries.
    if selected_root && workspace_use == WorkspaceUse::Reviewed {
        return Ok(true);
    }
    if !range_free(file.as_fd(), 4)? || (selected_root && !range_free(file.as_fd(), 3)?) {
        return Ok(false);
    }
    let own = match (selected_root, workspace_use) {
        (true, WorkspaceUse::Ordinary) => 0,
        (false, WorkspaceUse::Reviewed) => 1,
        _ => return Ok(true),
    };
    // Publish before observing the incompatible purpose. Racing purposes cannot
    // both enter; the existing waiter releases the complete partial set on retry.
    read_range(file.as_fd(), own, 1)
        .map_err(|error| format!("Cannot coordinate ordinary workspace access: {error}"))?;
    range_free(file.as_fd(), 1 - own)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn compatible_use(
    _file: &std::fs::File,
    _selected_root: bool,
    _workspace_use: WorkspaceUse,
) -> Result<bool, String> {
    Ok(true)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn lock_type(value: impl TryInto<libc::c_short>) -> libc::c_short {
    value
        .try_into()
        .unwrap_or_else(|_| unreachable!("OS lock types fit the native flock field"))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn read_range(
    file: BorrowedFd<'_>,
    start: libc::off_t,
    length: libc::off_t,
) -> std::io::Result<()> {
    // SAFETY: flock consists only of integer fields.
    let mut range: libc::flock = unsafe { std::mem::zeroed() };
    range.l_type = lock_type(libc::F_RDLCK);
    range.l_whence = libc::SEEK_SET.try_into().unwrap();
    range.l_start = start;
    range.l_len = length;
    loop {
        // SAFETY: the borrowed descriptor and initialized flock stay live.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_SETLK, &range) } != -1 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn range_free(file: BorrowedFd<'_>, start: libc::off_t) -> Result<bool, String> {
    // SAFETY: flock consists only of integer fields.
    let mut range: libc::flock = unsafe { std::mem::zeroed() };
    range.l_type = lock_type(libc::F_WRLCK);
    range.l_whence = libc::SEEK_SET.try_into().unwrap();
    range.l_start = start;
    range.l_len = 1;
    loop {
        // SAFETY: fcntl writes into this live, initialized flock.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_GETLK, &raw mut range) } != -1 {
            return Ok(range.l_type == lock_type(libc::F_UNLCK));
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(format!(
                "Cannot observe workspace quarantine or review: {error}"
            ));
        }
    }
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn quarantine<'a>(
    directories: impl ExactSizeIterator<Item = BorrowedFd<'a>>,
) -> Result<(), String> {
    let count = directories.len();
    if count == 0 || count > MAX_DIRECTORIES {
        return Err("Quarantine requires the original complete directory claim".into());
    }
    for (index, descriptor) in directories.enumerate() {
        // Extend existing read-lock ranges on the same original descriptions.
        // Duplicates held by an independent watcher retain these changes too.
        read_range(descriptor, 2, if index + 1 == count { 3 } else { 2 })
            .map_err(|error| format!("Cannot retain ordinary workspace quarantine: {error}"))?;
    }
    Ok(())
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn quarantine_descriptors(descriptors: &[OwnedFd]) -> Result<(), String> {
    quarantine(descriptors.iter().map(AsFd::as_fd))
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
