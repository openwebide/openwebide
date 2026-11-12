//! Native storage adapter for public cache-path declarations. Only derived
//! server data lives here; preferences, grants and editor drafts stay in the DB.
use super::*;
use crate::exec::{HostExecution, SpawnSpec, ToolExecution};
use openwebide_core::plugins::{content_digest, languages::validate_server_cache};
use std::{
    fs::{self, File, OpenOptions},
    hash::Hash,
    path::{Path, PathBuf},
};

const MAX_NAMESPACES: usize = 32;
const DEFAULT_RETAINED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
mod monitor;
pub(super) use monitor::poll;
pub(super) struct Manager {
    root: Option<PathBuf>,
    retained_bytes: Result<u64, String>,
    sampled: Mutex<Option<Instant>>,
}
impl Default for Manager {
    fn default() -> Self {
        let root = std::env::var_os("OPENWEBIDE_BRIDGE_LANGUAGE_CACHE_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os(if cfg!(windows) {
                    "LOCALAPPDATA"
                } else {
                    "XDG_CACHE_HOME"
                })
                .map(PathBuf::from)
                .map(|root| root.join("openwebide/languages"))
            })
            .or_else(|| {
                #[allow(deprecated)]
                std::env::home_dir().map(|home| home.join(".cache/openwebide/languages"))
            });
        let retained_bytes = match std::env::var("OPENWEBIDE_BRIDGE_LANGUAGE_CACHE_MIB") {
            Ok(value) => value.parse::<u64>().ok().filter(|value| *value > 0)
                .and_then(|value| value.checked_mul(1024 * 1024))
                .ok_or_else(|| "OPENWEBIDE_BRIDGE_LANGUAGE_CACHE_MIB must be a positive integer MiB retention budget".into()),
            Err(std::env::VarError::NotPresent) => Ok(DEFAULT_RETAINED_BYTES),
            Err(error) => Err(error.to_string()),
        };
        Self {
            root,
            retained_bytes,
            sampled: Mutex::default(),
        }
    }
}
impl Host {
    /// Execution-host configuration; set before cloning or serving this host.
    pub fn set_cache_directory(&mut self, root: PathBuf) -> Result<(), String> {
        if !root.is_absolute() {
            return Err("Language cache directory must be absolute".into());
        }
        let inner = Arc::get_mut(&mut self.inner)
            .ok_or("Configure language caches before using the host")?;
        inner.cache.root = Some(root);
        Ok(())
    }
    /// Bounds retained derived data when allocating or retiring services. Live
    /// process writes require server/OS quotas for a hard instantaneous limit.
    pub fn set_cache_retention_limit(&mut self, bytes: u64) -> Result<(), String> {
        if bytes < 1024 * 1024 {
            return Err("Language cache retention requires at least 1 MiB".into());
        }
        Arc::get_mut(&mut self.inner)
            .ok_or("Configure language caches before using the host")?
            .cache
            .retained_bytes = Ok(bytes);
        Ok(())
    }
}
impl Manager {
    pub(super) async fn prepare(
        &self,
        scope: &Scope,
        workspace: &Path,
        program: &program::Program,
        inputs: &inputs::Inputs,
        coordinator: &crate::folder_coordination::Coordinator,
    ) -> Result<Option<Lease>, String> {
        let Some(cache) = &scope.server.cache else {
            return Ok(None);
        };
        validate_server_cache(&scope.server)?;
        let retained_bytes = self.retained_bytes.clone()?;
        let root = self
            .root
            .clone()
            .filter(|root| root.is_absolute())
            .ok_or("Configure an absolute native language cache directory")?;
        let execution = crate::exec::workspace::Execution {
            inner: Arc::new(HostExecution),
            coordinator: coordinator.clone(),
        };
        let version = execution
            .run_command(SpawnSpec {
                command: program
                    .path()
                    .to_str()
                    .ok_or("Language executable path must be UTF-8")?
                    .into(),
                args: cache.version_args.clone(),
                cwd: workspace.into(),
                env: Default::default(),
                timeout: Duration::from_secs(5),
                cancel: Box::pin(std::future::pending()),
                folder: None,
            })
            .await
            .map_err(|error| error.to_string())?;
        if version.exit_code != Some(0)
            || version.stdout.trim().is_empty()
            || version.stdout.len() > 16 * 1024
        {
            return Err("The declared language cache version probe must succeed with 1–16,384 bytes of output".into());
        }
        program.check().await?;
        if !inputs.is_baseline(&inputs.observe(&scope.root).await?) {
            return Err("Declared project inputs changed before cache allocation".into());
        }
        let handle = same_file::Handle::from_path(&scope.root).map_err(|e| e.to_string())?;
        let host = crate::server::get_system_hostname().unwrap_or_else(|| "unknown-host".into());
        let key = content_digest(
            &serde_json::to_vec(&(
                1,
                host,
                std::env::consts::OS,
                std::env::consts::ARCH,
                &scope.owner,
                scope.project,
                &scope.root,
                identity(&handle),
                workspace,
                &scope.provider,
                &scope.digest,
                &scope.server,
                program.cache_identity(),
                version.stdout.trim(),
                &inputs.baseline,
            ))
            .map_err(|e| e.to_string())?,
        );
        let project = scope.root.clone();
        tokio::task::spawn_blocking(move || allocate(&root, &project, &key, retained_bytes))
            .await
            .map_err(|e| e.to_string())?
            .map(Some)
    }
}
fn identity(handle: &same_file::Handle) -> String {
    let mut hasher = crate::folder_coordination::FileIdentity::default();
    handle.hash(&mut hasher);
    content_digest(&hasher.0)
}
fn failure(error: std::io::Error) -> String {
    format!("Native language cache: {error}")
}
fn directory(path: &Path, private: bool) -> Result<(), String> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(failure)?;
    let meta = fs::symlink_metadata(path).map_err(failure)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("Language cache control directories cannot be links".into());
    }
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(
                "Language cache control directories must be private to the execution-host user"
                    .into(),
            );
        }
    }
    #[cfg(not(unix))]
    let _ = private;
    Ok(())
}
fn file(path: &Path, new: bool) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if new {
        options.create_new(true);
    } else {
        options.create(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
    }
    let file = options.open(path).map_err(failure)?;
    let metadata = file.metadata().map_err(failure)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Language cache control files cannot be links".into());
    }
    Ok(file)
}
fn candidate(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err("Language cache paths must be absolute without parent segments".into());
    }
    let mut current = path;
    let mut suffix = Vec::new();
    while !current.try_exists().map_err(failure)? {
        suffix.push(
            current
                .file_name()
                .ok_or("Invalid language cache directory")?
                .to_os_string(),
        );
        current = current.parent().ok_or("Invalid language cache directory")?;
    }
    let mut result = current.canonicalize().map_err(failure)?;
    for part in suffix.into_iter().rev() {
        result.push(part);
    }
    Ok(result)
}
struct Existing {
    path: PathBuf,
    key: String,
    used: std::time::SystemTime,
    available: bool,
    idle: bool,
    bytes: u64,
}
fn inspect(path: PathBuf) -> Result<Option<Existing>, String> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    if name.len() != 97
        || !name.is_ascii()
        || name.as_bytes()[64] != b'-'
        || !name[..64]
            .bytes()
            .chain(name[65..].bytes())
            .all(|ch| ch.is_ascii_hexdigit())
    {
        return Ok(None);
    }
    let key = name[..64].to_string();
    let marker = path.join(".owner");
    let meta = match fs::symlink_metadata(&marker) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(failure(error)),
    };
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 256 {
        return Ok(None);
    }
    let owner = fs::read_to_string(&marker).map_err(failure)?;
    let data = path.join("data");
    let kind = match fs::symlink_metadata(&data) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(failure(error)),
    };
    if !kind.is_dir() || kind.file_type().is_symlink() {
        return Ok(None);
    }
    let handle = same_file::Handle::from_path(&data).map_err(failure)?;
    if owner != format!("1\n{key}\n{}\n", identity(&handle)) {
        return Ok(None);
    }
    directory(&path, true)?;
    directory(&path.join(".leases"), true)?;
    let mut available = true;
    let mut leases = 0;
    for entry in fs::read_dir(path.join(".leases")).map_err(failure)? {
        leases += 1;
        if leases > 128 {
            return Err("Too many native cache leases".into());
        }
        let entry = entry.map_err(failure)?;
        let lease = file(&entry.path(), false)?;
        match lease.try_lock() {
            Ok(()) => available = false, // An unconfirmed owner must never be stolen by elapsed time.
            Err(std::fs::TryLockError::WouldBlock) => (),
            Err(std::fs::TryLockError::Error(error)) => return Err(failure(error)),
        }
    }
    Ok(Some(Existing {
        path,
        key,
        used: meta.modified().map_err(failure)?,
        available,
        idle: leases == 0,
        bytes: 0,
    }))
}
fn lock(root: &Path) -> Result<File, String> {
    let lock = file(&root.join(".lock"), false)?;
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(
                    "Native language cache allocation is busy; retry when its owner finishes"
                        .into(),
                );
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(failure(error)),
        }
    }
    Ok(lock)
}

fn inventory(root: &Path) -> Result<Vec<Existing>, String> {
    let mut entries = Vec::new();
    let mut count = 0;
    for entry in fs::read_dir(root).map_err(failure)? {
        count += 1;
        if count > 256 {
            return Err("The language cache root contains too many entries".into());
        }
        let entry = entry.map_err(failure)?;
        if entry.file_type().map_err(failure)?.is_dir()
            && let Some(existing) = inspect(entry.path())?
        {
            entries.push(existing);
        }
    }
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut count = 0;
    for entry in &mut entries {
        let mut pending = vec![entry.path.join("data")];
        while let Some(path) = pending.pop() {
            count += 1;
            if count > 250_000 || Instant::now() > deadline {
                return Err("Native language cache inventory exceeded its entry/time limit".into());
            }
            let meta = match fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(failure(error)),
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_file() {
                entry.bytes = entry.bytes.saturating_add(meta.len());
            } else if meta.is_dir() {
                for child in fs::read_dir(path).map_err(failure)? {
                    if count + pending.len() >= 250_000 {
                        return Err(
                            "Native language cache inventory exceeded its entry/time limit".into(),
                        );
                    }
                    pending.push(child.map_err(failure)?.path());
                }
            }
        }
    }
    entries.sort_by_key(|entry| entry.used);
    Ok(entries)
}
fn prune_idle(entries: &mut Vec<Existing>, bytes: u64, namespaces: usize) -> Result<bool, String> {
    let mut total = entries
        .iter()
        .fold(0u64, |total, entry| total.saturating_add(entry.bytes));
    while entries.len() > namespaces || total > bytes {
        let Some(index) = entries.iter().position(|entry| entry.idle) else {
            return Ok(false);
        };
        let entry = entries.remove(index);
        fs::remove_dir_all(&entry.path).map_err(failure)?;
        total = total.saturating_sub(entry.bytes);
    }
    Ok(true)
}
fn prune(entries: &mut Vec<Existing>, bytes: u64, namespaces: usize) -> Result<(), String> {
    if prune_idle(entries, bytes, namespaces)? {
        Ok(())
    } else {
        Err("Native language cache retention is held by active or unconfirmed owners".into())
    }
}
fn maintain(root: &Path, retained_bytes: u64) -> Result<(), String> {
    let _lock = lock(root)?;
    prune(&mut inventory(root)?, retained_bytes, MAX_NAMESPACES)
}
fn allocate(root: &Path, project: &Path, key: &str, retained_bytes: u64) -> Result<Lease, String> {
    let root = candidate(&root.join("v1"))?;
    let project = project.canonicalize().map_err(failure)?;
    if root.starts_with(&project) || project.starts_with(&root) {
        return Err("Language caches must be outside the opened project".into());
    }
    directory(&root, true)?;
    let _lock = lock(&root)?;
    let mut entries = inventory(&root)?;
    prune(&mut entries, retained_bytes, MAX_NAMESPACES)?;
    let path = if let Some(existing) = entries
        .iter()
        .rev()
        .find(|entry| entry.key == key && entry.available)
    {
        existing.path.clone()
    } else {
        prune(&mut entries, retained_bytes, MAX_NAMESPACES - 1)?;
        let path = root.join(format!("{key}-{:032x}", rand::random::<u128>()));
        directory(&path, true)?;
        directory(&path.join(".leases"), true)?;
        directory(&path.join("data"), false)?;
        let data = same_file::Handle::from_path(path.join("data")).map_err(failure)?;
        use std::io::Write;
        file(&path.join(".owner"), true)?
            .write_all(format!("1\n{key}\n{}\n", identity(&data)).as_bytes())
            .map_err(failure)?;
        path
    };
    file(&path.join(".owner"), false)?
        .set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::now()))
        .map_err(failure)?;
    let marker = path
        .join(".leases")
        .join(format!("{:032x}", rand::random::<u128>()));
    let descriptor = file(&marker, true)?;
    descriptor.try_lock().map_err(|e| e.to_string())?;
    let marker_handle =
        same_file::Handle::from_file(descriptor.try_clone().map_err(failure)?).map_err(failure)?;
    let data = path.join("data");
    let data_handle = same_file::Handle::from_path(&data).map_err(failure)?;
    Ok(Lease {
        data,
        marker,
        _ownership: descriptor,
        marker_handle,
        data_handle,
        remove: true,
        retained_bytes,
    })
}
pub(super) struct Lease {
    pub data: PathBuf,
    marker: PathBuf,
    _ownership: File,
    marker_handle: same_file::Handle,
    data_handle: same_file::Handle,
    remove: bool,
    retained_bytes: u64,
}
impl Lease {
    pub(super) fn starting(&mut self) {
        self.remove = false;
    }
    pub(super) fn activate(mut self, completion: crate::exec::proc::Cleanup) -> Use {
        self.remove = false;
        Use {
            lease: Mutex::new(Some(self)),
            completion,
        }
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if self.remove
            && same_file::Handle::from_path(&self.marker)
                .is_ok_and(|handle| handle == self.marker_handle)
        {
            let _ = fs::remove_file(&self.marker);
        }
    }
}
pub(super) struct Use {
    lease: Mutex<Option<Lease>>,
    completion: crate::exec::proc::Cleanup,
}
impl Use {
    pub(super) fn check(&self) -> Result<(), String> {
        let lease = self.lease.lock().unwrap();
        let lease = lease.as_ref().ok_or("Native language cache was retired")?;
        if same_file::Handle::from_path(&lease.data).is_ok_and(|handle| handle == lease.data_handle)
            && same_file::Handle::from_path(&lease.marker)
                .is_ok_and(|handle| handle == lease.marker_handle)
        {
            Ok(())
        } else {
            Err("Native language cache ownership changed; retry with current storage".into())
        }
    }
    /// Retire derived storage only after the original process tree confirms
    /// cleanup. The stopped session may still retain unsaved documents.
    pub(super) fn retire(&self) {
        let Some(mut lease) = self.lease.lock().unwrap().take() else {
            return;
        };
        let completion = self.completion.clone();
        crate::exec::proc::track_cleanup(tokio::spawn(async move {
            if completion.wait().await.is_ok() {
                lease.remove = true;
                let root = lease.data.parent().unwrap().parent().unwrap().to_path_buf();
                let retained_bytes = lease.retained_bytes;
                let result = tokio::task::spawn_blocking(move || {
                    drop(lease);
                    maintain(&root, retained_bytes)
                })
                .await;
                if !matches!(result, Ok(Ok(()))) {
                    tracing::warn!(?result, "Native language cache retention could not finish");
                }
                return;
            }
            // Failure/aborted cleanup leaves its pre-created lease marker. A
            // later host chooses another physical directory, never that path.
            drop(lease);
        }));
    }
}
impl Drop for Use {
    fn drop(&mut self) {
        self.retire();
    }
}

#[cfg(test)]
mod tests;
