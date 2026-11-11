//! Result-specific native observations, carried as a bounded public path set
//! and an authenticated aggregate. No result cache or bearer authority exists.
use super::*;
use openwebide_core::{
    Vfs, workspace_entries::WorkspaceEntries, workspace_transaction::TransactionFiles,
};
use std::path::Path;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod native;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use native::Native;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod portable;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use portable::Native;

// Reserve before retaining each path and stamp, including BTree node, String
// capacities, request framing and final path-vector duplication. The fixed
// host pool covers in-flight normalization and validation, not just idle data.
const ENTRY_BYTES: usize = 512;
const MAX_BYTES: usize = 16 * 1024 * 1024;
static CAPACITY: std::sync::LazyLock<Arc<Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(Semaphore::new(64 * 1024 * 1024)));
fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}
struct FileObservation {
    revision: String,
    identity: String,
}
pub(in crate::languages::host) struct Snapshot {
    capacity: Arc<Semaphore>,
    entries: BTreeMap<String, FileObservation>,
    permits: Vec<OwnedSemaphorePermit>,
    bytes: usize,
    paths: usize,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            capacity: CAPACITY.clone(),
            entries: BTreeMap::new(),
            permits: vec![],
            bytes: 0,
            paths: 0,
        }
    }
}
impl Snapshot {
    pub fn paths(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }
    pub fn revisions(&self) -> Vec<String> {
        self.entries
            .values()
            .map(|entry| entry.revision.clone())
            .collect()
    }
    pub fn identities(&self) -> Vec<String> {
        self.entries
            .values()
            .map(|entry| entry.identity.clone())
            .collect()
    }
    pub fn digest(&self) -> String {
        use sha2::Digest;
        let mut hash = Sha256::new();
        for (path, stamp) in &self.entries {
            hash.update(path.len().to_le_bytes());
            hash.update(path.as_bytes());
            hash.update(stamp.revision.len().to_le_bytes());
            hash.update(stamp.revision.as_bytes());
            hash.update(stamp.identity.len().to_le_bytes());
            hash.update(stamp.identity.as_bytes());
        }
        openwebide_core::plugins::content_digest(&hash.finalize())
    }
    pub fn ensure_current(&self, root: &Path) -> Result<(), String> {
        if self.entries.is_empty() {
            return Ok(());
        }
        Files::verify(
            root,
            &self.paths(),
            &self.identities(),
            &self.revisions(),
            &self.digest(),
        )
        .map(|_| ())
    }
    fn retain(&mut self, path: String, stamp: String, identity: String) -> Result<(), String> {
        if let Some(original) = self.entries.get(&path) {
            if original.revision != stamp || original.identity != identity {
                return Err("An affected file changed during result normalization".into());
            }
            return Ok(());
        }
        let bytes = ENTRY_BYTES
            + path.len().saturating_mul(4)
            + stamp.len().saturating_mul(3)
            + identity.len().saturating_mul(3);
        if self.entries.len() >= protocol::MAX_ANALYSIS_FILES
            || self.bytes.saturating_add(bytes) > MAX_BYTES
            || self.paths.saturating_add(path.len()) > protocol::MAX_ANALYSIS_PATH_BYTES
        {
            return Err("Affected-file observation exceeds its bounded budget".into());
        }
        let permit = self
            .capacity
            .clone()
            .try_acquire_many_owned(u32::try_from(bytes).unwrap())
            .map_err(|_| "Affected-file observation capacity is busy")?;
        self.bytes += bytes;
        self.paths += path.len();
        self.permits.push(permit);
        self.entries.insert(
            path,
            FileObservation {
                revision: stamp,
                identity,
            },
        );
        Ok(())
    }
}
pub(in crate::languages::host) struct Files {
    native: Native,
    vfs: crate::runs::native_vfs::NativeFsVfs,
    snapshot: Mutex<Snapshot>,
    started: Instant,
}
impl Files {
    pub fn new(root: &Path) -> Result<Self, String> {
        Ok(Self {
            native: Native::new(root)?,
            vfs: crate::runs::native_vfs::NativeFsVfs { root: root.into() },
            snapshot: Mutex::new(Snapshot::default()),
            started: Instant::now(),
        })
    }
    fn current(&self) -> Result<(), String> {
        if self.started.elapsed() > Duration::from_secs(8) {
            return Err("Affected-file observation timed out".into());
        }
        if !self.native.current(&self.vfs.root) {
            return Err("The affected-file root changed".into());
        }
        Ok(())
    }
    fn observe(&self, path: &str) -> Result<(), String> {
        self.current()?;
        if (!path.is_empty() && !protocol::relative_path(path))
            || path.len() > 4096
            || openwebide_core::vfs::workspace_path(path).map_err(text)? != path
        {
            return Err("Invalid affected-file path".into());
        }
        for (path, stamp, identity) in self.native.revisions(path)? {
            self.snapshot
                .lock()
                .unwrap()
                .retain(path, stamp, identity)?;
        }
        self.current()
    }
    pub fn read_limited(&self, path: &str, limit: u64) -> Result<Vec<u8>, String> {
        self.observe(path)?;
        let bytes = self.native.read(path, limit)?;
        self.observe(path)?;
        Ok(bytes)
    }
    pub fn finish(self) -> Result<Snapshot, String> {
        let paths = self.snapshot.lock().unwrap().paths();
        for path in paths {
            self.observe(&path)?;
        }
        Ok(self.snapshot.into_inner().unwrap())
    }
    pub fn verify(
        root: &Path,
        paths: &[String],
        identities: &[String],
        revisions: &[String],
        digest: &str,
    ) -> Result<Snapshot, String> {
        let files = Self::new(root)?;
        for path in paths {
            files.observe(path)?;
        }
        let current = files.finish()?;
        if current.paths() != paths
            || current.identities() != identities
            || current.revisions() != revisions
            || current.digest() != digest
        {
            return Err("The original affected-file observation changed".into());
        }
        Ok(current)
    }
}
impl WorkspaceEntries for Files {
    async fn canonicalize(&self, path: &str) -> Result<String, String> {
        self.observe(path)?;
        let canonical = self.vfs.canonicalize(path).await.map_err(text)?;
        self.observe(path)?;
        self.observe(&canonical)?;
        Ok(canonical)
    }
    async fn list(&self, path: &str) -> Result<Vec<openwebide_core::FileEntry>, String> {
        self.observe(path)?;
        let mut entries = self.native.list(path)?;
        entries.retain(|entry| {
            entry.name != ".spin" && !openwebide_core::vfs::private_workspace_path(&entry.path)
        });
        for entry in &mut entries {
            if entry.is_dir {
                entry.size = 0;
            }
        }
        self.observe(path)?;
        Ok(entries)
    }
    async fn read_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        self.read_limited(path, openwebide_core::vfs::MAX_READ_BYTES)
    }
    async fn create(&self, _: &str, _: openwebide_core::vfs::VfsEntryKind) -> Result<(), String> {
        Err("Language normalization is read-only".into())
    }
    async fn write_bytes(&self, _: &str, _: &[u8]) -> Result<(), String> {
        Err("Language normalization is read-only".into())
    }
    async fn delete(&self, _: &str) -> Result<(), String> {
        Err("Language normalization is read-only".into())
    }
}
impl TransactionFiles for Files {
    fn root_identity(&self) -> &str {
        "language-edit-normalization"
    }
    async fn replace_atomic(&self, _: &str, _: &[u8]) -> Result<(), String> {
        Err("Language normalization is read-only".into())
    }
    async fn remove_entry(
        &self,
        _: &str,
        _: openwebide_core::vfs::VfsEntryKind,
    ) -> Result<(), String> {
        Err("Language normalization is read-only".into())
    }
}

#[cfg(test)]
mod tests;
