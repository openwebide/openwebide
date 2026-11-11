//! Bounded project-wide metadata observations, shared by both native routes.
//! Read workers retain their permits until they actually stop, including when
//! their caller is cancelled. Only complete inventories advance a generation.
use super::{files::FileRevision, *};
use openwebide_core::plugins::languages::ProjectFiles;
use std::{
    path::Path,
    sync::{
        LazyLock,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Semaphore;

const MAX_ENTRIES: usize = 65_536;
const MAX_RECORDS: usize = 16_384;
const MAX_PATH_BYTES: usize = 4 * 1024 * 1024;
const MAX_DEPTH: usize = 128;
const SCAN_TIMEOUT: Duration = Duration::from_secs(4);
struct Budgets {
    sources: Arc<Semaphore>,
    metadata: Arc<Semaphore>,
}
impl Budgets {
    fn new(total: usize) -> Self {
        let metadata = total / 4;
        Self {
            sources: Arc::new(Semaphore::new(total - metadata)),
            metadata: Arc::new(Semaphore::new(metadata)),
        }
    }
    fn for_sources(&self, sources: bool) -> Arc<Semaphore> {
        if sources {
            self.sources.clone()
        } else {
            self.metadata.clone()
        }
    }
}
static SCANS: LazyLock<Budgets> = LazyLock::new(|| Budgets::new(4));

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod events;
mod metadata;
#[cfg(windows)]
pub(super) mod windows;
pub(super) use metadata::MetadataFiles;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    revision: FileRevision,
    #[cfg(windows)]
    identity: windows::Identity,
}
impl Stamp {
    fn from_file(file: &std::fs::File) -> Result<Self, String> {
        #[cfg(windows)]
        {
            let (metadata, identity) = windows::from_file(file)?;
            Ok(Self {
                revision: FileRevision::from(&metadata),
                identity,
            })
        }
        #[cfg(not(windows))]
        {
            Ok(Self {
                revision: FileRevision::from(&file.metadata().map_err(|error| error.to_string())?),
            })
        }
    }
    // The common interface is fallible on Windows, which reads a native handle.
    #[cfg_attr(not(windows), allow(clippy::unnecessary_wraps))]
    fn read(path: &Path, metadata: &std::fs::Metadata) -> Result<Self, String> {
        #[cfg(windows)]
        {
            let (current, identity) = windows::read(path)?;
            if FileRevision::from(&current) != FileRevision::from(metadata) {
                return Err("A project file changed while reading its identity".into());
            }
            Ok(Self {
                revision: FileRevision::from(&current),
                identity,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = path;
            Ok(Self {
                revision: FileRevision::from(metadata),
            })
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    stamp: Stamp,
    canonical: Option<Arc<PathBuf>>,
    target: Option<Stamp>,
    watchable: bool,
}
struct Ancestor {
    path: Arc<PathBuf>,
    parent: Option<Arc<Self>>,
}
fn contains(mut ancestor: Option<&Ancestor>, path: &Path) -> bool {
    while let Some(current) = ancestor {
        if current.path.as_path() == path {
            return true;
        }
        ancestor = current.parent.as_deref();
    }
    false
}
#[derive(Default, Debug, PartialEq, Eq)]
pub(super) struct Inventory(BTreeMap<PathBuf, Entry>);
#[derive(Default)]
struct State {
    inventory: Option<Inventory>,
    revision: u64,
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    events: Option<events::Events>,
}
struct Collected {
    inventory: Inventory,
    changed: bool,
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    events: events::Events,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Observation {
    pub revision: u64,
}
pub(super) struct Tracker<S = ProjectFiles> {
    files: S,
    state: tokio::sync::Mutex<State>,
    scanning: Arc<Semaphore>,
    limit: usize,
}
struct Cancellation(Arc<AtomicBool>);
impl Drop for Cancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}
pub(super) trait FileSelection: Clone + Send + 'static {
    fn collect(
        &self,
        root: &Path,
        cancelled: &AtomicBool,
        limit: usize,
    ) -> Result<Inventory, String>;
    fn project_sources(&self) -> bool;
}
impl FileSelection for ProjectFiles {
    fn collect(
        &self,
        root: &Path,
        cancelled: &AtomicBool,
        limit: usize,
    ) -> Result<Inventory, String> {
        collect(root, self, cancelled, limit)
    }
    fn project_sources(&self) -> bool {
        true
    }
}
impl Tracker {
    pub fn matches(&self, files: &ProjectFiles) -> bool {
        self.files.signature() == files.signature()
    }
}
impl<S: FileSelection> Tracker<S> {
    pub fn new(files: S) -> Self {
        Self {
            files,
            state: tokio::sync::Mutex::default(),
            scanning: Arc::new(Semaphore::new(1)),
            limit: MAX_RECORDS,
        }
    }
    pub async fn observe(&self, root: &Path) -> Result<Observation, String> {
        self.observe_with(root, |root, files, cancelled, limit| {
            files.collect(root, cancelled, limit)
        })
        .await
    }
    async fn observe_with<F>(&self, root: &Path, scan: F) -> Result<Observation, String>
    where
        F: FnOnce(&Path, &S, &AtomicBool, usize) -> Result<Inventory, String> + Send + 'static,
    {
        tokio::time::timeout(SCAN_TIMEOUT, async {
            let mut state = self.state.lock().await;
            let local = self
                .scanning
                .clone()
                .acquire_owned()
                .await
                .map_err(|error| error.to_string())?;
            let global = SCANS
                .for_sources(self.files.project_sources())
                .acquire_owned()
                .await
                .map_err(|error| error.to_string())?;
            let cancel = Cancellation(Arc::new(AtomicBool::new(false)));
            let flag = cancel.0.clone();
            let files = self.files.clone();
            let root = root.to_path_buf();
            let limit = self.limit;
            #[cfg(any(target_os = "macos", target_os = "linux", windows))]
            let previous = state.events.take();
            let collected = tokio::task::spawn_blocking(move || {
                let _permits = (local, global);
                check(&flag)?;
                #[cfg(any(target_os = "macos", target_os = "linux", windows))]
                let started = previous.is_none();
                #[cfg(any(target_os = "macos", target_os = "linux", windows))]
                let mut events = previous.map_or_else(
                    || {
                        if files.project_sources() {
                            events::Events::new()
                        } else {
                            events::Events::metadata()
                        }
                    },
                    Ok,
                )?;
                #[cfg(any(target_os = "macos", target_os = "linux", windows))]
                let prior_changes = events.poll()?;
                let inventory = scan(&root, &files, &flag, limit)?;
                check(&flag)?;
                #[cfg(any(target_os = "macos", target_os = "linux", windows))]
                let registered = events.reconcile(&watch_paths(&inventory)?, &flag)?;
                #[cfg(any(target_os = "macos", target_os = "linux", windows))]
                if registered && files.collect(&root, &flag, limit)? != inventory {
                    return Err("Project files changed while installing source watches".into());
                }
                #[cfg(any(target_os = "macos", target_os = "linux", windows))]
                if events.poll()? {
                    return Err("Project files changed while collecting source metadata".into());
                }
                check(&flag)?;
                Ok::<_, String>(Collected {
                    inventory,
                    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
                    changed: started || prior_changes || registered,
                    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
                    changed: false,
                    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
                    events,
                })
            })
            .await
            .map_err(|error| error.to_string())??;
            if state.inventory.as_ref() != Some(&collected.inventory) || collected.changed {
                let revision = state
                    .revision
                    .checked_add(1)
                    .ok_or("Project generation exhausted")?;
                state.inventory = Some(collected.inventory);
                state.revision = revision;
            }
            #[cfg(any(target_os = "macos", target_os = "linux", windows))]
            {
                state.events = Some(collected.events);
            }
            Ok(Observation {
                revision: state.revision,
            })
        })
        .await
        .map_err(|_| "Reading project-source metadata timed out".to_string())?
    }
}

fn check(cancelled: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::Relaxed) {
        Err("Project-source observation was cancelled".into())
    } else {
        Ok(())
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn watch_paths(inventory: &Inventory) -> Result<BTreeMap<PathBuf, Stamp>, String> {
    let mut paths = BTreeMap::new();
    for entry in inventory.0.values().filter(|entry| entry.watchable) {
        let path = entry
            .canonical
            .as_ref()
            .ok_or("A project source disappeared")?;
        let revision = entry.target.as_ref().unwrap_or(&entry.stamp);
        if paths
            .insert(path.as_ref().clone(), revision.clone())
            .is_some_and(|previous| previous != *revision)
        {
            return Err("A project-source alias changed during collection".into());
        }
    }
    Ok(paths)
}

fn collect(
    root: &Path,
    files: &ProjectFiles,
    cancelled: &AtomicBool,
    limit: usize,
) -> Result<Inventory, String> {
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    let mut result = Inventory::default();
    let mut entries = 0;
    let mut path_bytes = 0_usize;
    let mut queued_bytes = root.as_os_str().as_encoded_bytes().len();
    let mut pending = vec![(root.clone(), None::<Arc<Ancestor>>, 0_usize)];
    while let Some((directory, ancestors, depth)) = pending.pop() {
        queued_bytes -= directory.as_os_str().as_encoded_bytes().len();
        check(cancelled)?;
        let canonical = directory
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if contains(ancestors.as_deref(), &canonical) {
            continue;
        }
        let canonical = Arc::new(canonical);
        let relative = directory
            .strip_prefix(&root)
            .map_err(|error| error.to_string())?
            .to_path_buf();
        let metadata = std::fs::symlink_metadata(&directory).map_err(|error| error.to_string())?;
        let target = metadata
            .file_type()
            .is_symlink()
            .then(|| std::fs::metadata(&directory).map_err(|error| error.to_string()))
            .transpose()?;
        let record = Entry {
            stamp: Stamp::read(&directory, &metadata)?,
            canonical: Some(canonical.clone()),
            target: target
                .as_ref()
                .map(|metadata| Stamp::read(&canonical, metadata))
                .transpose()?,
            watchable: true,
        };
        insert(&mut result, relative, record, &mut path_bytes, limit)?;
        // Siblings share their ancestor chain and recorded canonical strings;
        // a wide/deep tree must not clone the whole ancestry for each child.
        let ancestors = Arc::new(Ancestor {
            path: canonical,
            parent: ancestors,
        });
        for child in std::fs::read_dir(&directory).map_err(|error| error.to_string())? {
            check(cancelled)?;
            entries += 1;
            if entries > MAX_ENTRIES {
                return Err("Project-source inventory exceeds its directory-entry limit".into());
            }
            let path = child.map_err(|error| error.to_string())?.path();
            let relative = path
                .strip_prefix(&root)
                .map_err(|error| error.to_string())?
                .to_path_buf();
            let portable = relative.to_string_lossy().replace('\\', "/");
            if !files.includes_directory(&portable) {
                continue;
            }
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            let canonical = match path.canonicalize() {
                Ok(path) => Some(Arc::new(path)),
                Err(error)
                    if metadata.file_type().is_symlink()
                        && error.kind() == std::io::ErrorKind::NotFound =>
                {
                    None
                }
                Err(error) => return Err(error.to_string()),
            };
            if canonical.as_ref().is_some_and(|path| {
                !files.includes_directory(&path.to_string_lossy().replace('\\', "/"))
            }) {
                continue;
            }
            let target = if metadata.file_type().is_symlink() {
                canonical
                    .as_ref()
                    .map(|path| std::fs::metadata(path.as_path()))
                    .transpose()
                    .map_err(|error| error.to_string())?
            } else {
                None
            };
            let is_directory = target.as_ref().unwrap_or(&metadata).is_dir();
            let relevant = files.includes_file(&portable);
            if relevant
                && !is_directory
                && !(metadata.file_type().is_symlink() && target.is_none())
                && !target.as_ref().unwrap_or(&metadata).is_file()
            {
                return Err("A project-source input is not a regular file".into());
            }
            if is_directory {
                if depth >= MAX_DEPTH {
                    return Err("Project-source inventory exceeds its directory-depth limit".into());
                }
                let next = canonical
                    .as_ref()
                    .ok_or("A project source directory disappeared")?;
                if !contains(Some(&ancestors), next) {
                    queued_bytes =
                        queued_bytes.saturating_add(path.as_os_str().as_encoded_bytes().len());
                    if queued_bytes > MAX_PATH_BYTES || pending.len() >= limit {
                        return Err("Project-source directory queue exceeds its budget".into());
                    }
                    pending.push((path, Some(ancestors.clone()), depth + 1));
                    continue;
                }
            }
            if is_directory || metadata.file_type().is_symlink() || relevant {
                let record = Entry {
                    stamp: Stamp::read(&path, &metadata)?,
                    target: target
                        .as_ref()
                        .zip(canonical.as_ref())
                        .map(|(metadata, path)| Stamp::read(path, metadata))
                        .transpose()?,
                    canonical,
                    watchable: is_directory || target.as_ref().unwrap_or(&metadata).is_file(),
                };
                insert(&mut result, relative, record, &mut path_bytes, limit)?;
            }
        }
    }
    check(cancelled)?;
    Ok(result)
}
fn insert(
    inventory: &mut Inventory,
    path: PathBuf,
    entry: Entry,
    bytes: &mut usize,
    limit: usize,
) -> Result<(), String> {
    *bytes = bytes
        .saturating_add(path.as_os_str().as_encoded_bytes().len())
        .saturating_add(
            entry
                .canonical
                .as_ref()
                .map_or(0, |path| path.as_os_str().as_encoded_bytes().len()),
        );
    if inventory.0.len() >= limit || *bytes > MAX_PATH_BYTES {
        return Err("Project-source inventory exceeds its file or path budget".into());
    }
    inventory.0.insert(path, entry);
    Ok(())
}

#[cfg(test)]
mod tests;
