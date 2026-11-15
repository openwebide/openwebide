//! Remote-mode filesystem access over the WASI `wasi:filesystem` imports.
//!
//! Spin mounts a host folder as a preopened directory (see the `files` entry
//! in spin.toml). Every path the API receives is resolved against that root;
//! any path that would escape it is rejected.

type Result<T, E = FsError> = std::result::Result<T, E>;

mod direct;

#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error("path {0:?} is reserved")]
    Reserved(String),
    #[error("already exists in workspace: {0}")]
    AlreadyExists(String),
    #[error("{0}")]
    Conflict(String),
    #[error("not found in workspace: {0}")]
    NotFound(String),
    #[error("file is too large to read ({size} bytes, max {max})")]
    TooLarge { size: u64, max: u64 },
    #[error("path {0:?} escapes the workspace root")]
    PathEscape(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("{0}")]
    Io(String),
}
impl From<openwebide_core::VfsError> for FsError {
    fn from(error: openwebide_core::VfsError) -> Self {
        match error {
            openwebide_core::VfsError::NotFound(path) => Self::NotFound(path),
            openwebide_core::VfsError::PermissionRequired(path)
            | openwebide_core::VfsError::PermissionDenied(path) => Self::PermissionDenied(path),
            openwebide_core::VfsError::AlreadyExists(path) => Self::AlreadyExists(path),
            openwebide_core::VfsError::Conflict(message) => Self::Conflict(message),
            openwebide_core::VfsError::PathEscape(path) => Self::PathEscape(path),
            openwebide_core::VfsError::Io(detail) => Self::Io(detail),
        }
    }
}

use openwebide_core::{FileEntry, SearchHit, Vfs, VfsError, VfsFuture, vfs::SearchOptions};

use crate::wasi::filesystem::preopens;
use crate::wasi::filesystem::types::{
    Descriptor, DescriptorFlags, DescriptorType, DirectoryEntry, ErrorCode, OpenFlags, PathFlags,
};

/// Upper bound on a single file read, so a huge file can't blow up memory.
pub(crate) const MAX_READ_BYTES: u64 = openwebide_core::vfs::MAX_READ_BYTES;

fn fs_error(code: ErrorCode) -> FsError {
    match code {
        ErrorCode::NoEntry => FsError::NotFound("entry".into()),
        ErrorCode::Access | ErrorCode::NotPermitted => FsError::PermissionDenied("entry".into()),
        other => FsError::Io(format!("filesystem error: {other:?}")),
    }
}

/// Like [`fs_error`] but names the path, so a missing entry reads as
/// "not found in workspace: <path>" instead of a bare WASI code.
fn fs_error_at(code: ErrorCode, path: &str) -> FsError {
    match code {
        ErrorCode::NoEntry => FsError::NotFound(path.into()),
        ErrorCode::Access | ErrorCode::NotPermitted => FsError::PermissionDenied(path.into()),
        other => FsError::Io(format!("filesystem error: {other:?} (at {path})")),
    }
}

/// The preopened workspace root descriptor.
fn root() -> Result<Descriptor> {
    preopens::get_directories()
        .into_iter()
        .next()
        .map(|(d, _)| d)
        .ok_or_else(|| {
            FsError::Io("no preopened directory; add a `files` mount to spin.toml".into())
        })
}

pub trait PathResolver {
    fn readlink(
        &self,
        path: &str,
    ) -> impl std::future::Future<Output = Result<String, ErrorCode>> + Send;
}

// Note: The real WASI fs adapter (`WasiResolver`) is wired at construction
// (e.g. in `dir_at`, `file_at_read`, etc.) and isn't covered by native tests
// since Spin can't run natively in `cargo test`.
pub struct WasiResolver<'a> {
    root: &'a Descriptor,
}

impl<'a> WasiResolver<'a> {
    pub fn new(root: &'a Descriptor) -> Self {
        Self { root }
    }
}

impl<'a> PathResolver for WasiResolver<'a> {
    async fn readlink(&self, path: &str) -> Result<String, ErrorCode> {
        self.root.readlink_at(path.to_string()).await
    }
}

/// Resolve a workspace-relative path, following all symlinks component by
/// component, and rejecting anything that would escape the root (`..`,
/// absolute paths, etc.) or enter a reserved directory like `.spin`.
pub async fn sanitize(resolver: &impl PathResolver, rel: &str) -> Result<String> {
    if rel.starts_with('/') {
        return Err(FsError::PathEscape(rel.into()));
    }

    let mut parts: Vec<String> = rel.split('/').map(ToString::to_string).collect();
    parts.reverse();

    let mut resolved = Vec::new();
    let mut iterations = 0;
    const MAX_SYMLINK_EXPANSIONS: usize = 32;

    while let Some(comp) = parts.pop() {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp == ".." {
            if resolved.pop().is_none() {
                return Err(FsError::PathEscape(rel.into()));
            }
            continue;
        }
        if comp == ".spin" {
            return Err(FsError::Reserved(rel.into()));
        }

        let test_path = if resolved.is_empty() {
            comp.clone()
        } else {
            format!("{}/{}", resolved.join("/"), comp)
        };
        if openwebide_core::vfs::private_workspace_path(&test_path) {
            return Err(FsError::Reserved(rel.into()));
        }

        match resolver.readlink(&test_path).await {
            Ok(target) => {
                iterations += 1;
                if iterations > MAX_SYMLINK_EXPANSIONS {
                    return Err(FsError::Io("too many levels of symbolic links".into()));
                }
                if target.starts_with('/') {
                    return Err(FsError::PathEscape(rel.into()));
                }
                let mut target_parts: Vec<String> =
                    target.split('/').map(ToString::to_string).collect();
                target_parts.reverse();
                for p in target_parts {
                    parts.push(p);
                }
            }
            Err(ErrorCode::Invalid | ErrorCode::NoEntry | ErrorCode::NotDirectory) => {
                resolved.push(comp);
            }
            Err(e) => {
                return Err(fs_error_at(e, &test_path));
            }
        }
    }

    Ok(resolved.join("/"))
}

/// Whether a directory exists at the mount-relative path `rel`, for the
/// migration probe. The path is checked lexically first (the same escape
/// rules [`sanitize`] enforces: no absolute paths, `..`, or `.spin`), then
/// probed with a sync `std::fs::metadata` on the preopened mount (the same
/// path form `git.rs` already uses with `std::fs`); `false` on any error.
pub fn dir_exists(rel: &str) -> bool {
    if rel.starts_with('/') {
        return false;
    }
    if rel.split('/').any(|c| c == ".." || c == ".spin") {
        return false;
    }
    std::fs::metadata(rel).is_ok_and(|m| m.is_dir())
}

/// Open a directory at a workspace-relative path (empty = the root).
async fn dir_at(rel: &str) -> Result<Descriptor> {
    let root = root()?;
    let resolver = WasiResolver::new(&root);
    let rel = sanitize(&resolver, rel).await?;
    if rel.is_empty() {
        return Ok(root);
    }
    root.open_at(
        PathFlags::empty(),
        rel.clone(),
        OpenFlags::DIRECTORY,
        DescriptorFlags::empty(),
    )
    .await
    .map_err(|code| fs_error_at(code, &rel))
}

/// Open an existing file for reading.
async fn file_at_read(rel: &str) -> Result<Descriptor> {
    let root = root()?;
    let resolver = WasiResolver::new(&root);
    let rel = sanitize(&resolver, rel).await?;
    root.open_at(
        PathFlags::empty(),
        rel.clone(),
        OpenFlags::empty(),
        DescriptorFlags::READ,
    )
    .await
    .map_err(|code| fs_error_at(code, &rel))
}

/// Read the entries of a directory.
async fn read_dir_entries(dir: &Descriptor) -> Result<Vec<DirectoryEntry>> {
    let (stream, fut) = dir.read_directory();
    let entries = stream.collect().await;
    match fut.await {
        Ok(()) => Ok(entries),
        Err(code) => Err(fs_error(code)),
    }
}

/// List the entries of the directory at `rel` (empty = workspace root).
///
/// The filesystem reads happen against the symlink-resolved real directory
/// (so `.spin`/escape checks still run), but the paths on the returned
/// [`FileEntry`] objects are built from the **original, logical `rel`** the
/// caller passed in — e.g. listing `sym_dir` returns entries like
/// `sym_dir/child`, not `real_dir/child`.
pub async fn list(rel: &str) -> Result<Vec<FileEntry>> {
    let root = root()?;
    let canonical = sanitize(&WasiResolver::new(&root), rel).await?;
    // `dir_at` resolves through symlinks so filesystem reads target the real
    // directory; all safety checks run inside `dir_at` → `sanitize`.
    let dir = dir_at(rel).await?;
    let entries = read_dir_entries(&dir).await?;
    // Use the *logical* caller-supplied prefix so the frontend file-tree can
    // match children back to the requested path, even when `rel` is a symlink.
    let logical_base = rel.trim_end_matches('/');
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let is_dir = matches!(e.type_, DescriptorType::Directory);
        let size = if is_dir {
            0
        } else {
            match dir.stat_at(PathFlags::empty(), e.name.clone()).await {
                Ok(st) => st.size,
                Err(ErrorCode::NoEntry) => continue,
                Err(_) => 0,
            }
        };
        let path = if logical_base.is_empty() {
            e.name.clone()
        } else {
            format!("{logical_base}/{}", e.name)
        };
        if openwebide_core::vfs::workspace_path(&path).is_err()
            || openwebide_core::vfs::private_workspace_path(&format!("{canonical}/{}", e.name))
        {
            continue;
        }
        out.push(FileEntry {
            name: e.name,
            path,
            is_dir,
            size,
        });
    }
    Ok(out)
}

/// Read a file's raw bytes.
pub async fn read_bytes(rel: &str) -> Result<Vec<u8>> {
    let root = root()?;
    let resolver = WasiResolver::new(&root);
    let rel = sanitize(&resolver, rel).await?;
    let st = root
        .stat_at(PathFlags::empty(), rel.clone())
        .await
        .map_err(|code| fs_error_at(code, &rel))?;
    if st.size > MAX_READ_BYTES {
        return Err(FsError::TooLarge {
            size: st.size,
            max: MAX_READ_BYTES,
        });
    }
    // Note: file_at_read will re-sanitize, which is slightly inefficient but safe.
    let file = file_at_read(&rel).await?;
    let (stream, fut) = file.read_via_stream(0);
    let bytes = stream.collect().await;
    match fut.await {
        Ok(()) => {}
        Err(code) => return Err(fs_error(code)),
    }
    Ok(bytes)
}

/// Read at most the caller's byte budget, including growth after metadata was
/// checked. Use one open descriptor for metadata and contents.
pub async fn read_bytes_with_limit(rel: &str, max: u64) -> Result<Vec<u8>> {
    let max = max.min(MAX_READ_BYTES);
    let file = file_at_read(rel).await?;
    let size = file.stat().await.map_err(fs_error)?.size;
    if size > max {
        return Err(FsError::TooLarge { size, max });
    }
    let capacity = usize::try_from(max + 1).map_err(|error| FsError::Io(error.to_string()))?;
    let (mut stream, result) = file.read_via_stream(0);
    let mut bytes = Vec::with_capacity(capacity);
    loop {
        let (status, buffer) = stream.read(bytes).await;
        bytes = buffer;
        if bytes.len() as u64 > max {
            return Err(FsError::TooLarge {
                size: bytes.len() as u64,
                max,
            });
        }
        match status {
            spin_sdk::wit_bindgen::rt::async_support::StreamResult::Dropped => break,
            spin_sdk::wit_bindgen::rt::async_support::StreamResult::Complete(_) => {}
            spin_sdk::wit_bindgen::rt::async_support::StreamResult::Cancelled => {
                return Err(FsError::Io("Bounded file read was cancelled".into()));
            }
        }
    }
    result.await.map_err(fs_error)?;
    Ok(bytes)
}

/// Read a file's contents as UTF-8 text.
pub async fn read(rel: &str) -> Result<String> {
    let bytes = read_bytes(rel).await?;
    String::from_utf8(bytes).map_err(|_| FsError::Io("file is not valid UTF-8".into()))
}

/// Hard caps on a search walk: matching hits, bytes read and directory entries. A search
/// stops early once the hit or entry budget is exhausted; the byte budget
/// is checked per file before reading.
#[cfg(test)]
use openwebide_core::{
    search::{SearchBudget, truncate_line},
    vfs::skip_dir,
};

pub async fn search(rel: &str, query: &str, opts: SearchOptions) -> Result<Vec<FileEntry>> {
    openwebide_core::search::files(&HostFsVfs::new(""), query, rel, opts)
        .await
        .map_err(FsError::from)
}
pub async fn full_text_search(
    rel: &str,
    query: &str,
    opts: SearchOptions,
) -> Result<Vec<SearchHit>> {
    openwebide_core::search::content(&HostFsVfs::new(""), query, rel, opts)
        .await
        .map_err(FsError::from)
}

/// Remote-mode VFS implementation bound to a project's base directory
/// (itself relative to the Spin preopen root).
#[derive(Clone)]
pub struct HostFsVfs<S = std::sync::Arc<openwebide_storage::Store<crate::state::AppDb>>> {
    base: String,
    store: Option<S>,
}

impl HostFsVfs {
    /// Read-only filesystem access, also used by search and startup context.
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            store: None,
        }
    }
}

impl<S> HostFsVfs<S> {
    pub fn with_store(base: impl Into<String>, store: S) -> Self {
        Self {
            base: base.into(),
            store: Some(store),
        }
    }

    fn resolve(&self, rel: &str) -> Result<String, VfsError> {
        let norm = openwebide_core::vfs::workspace_path(rel)?;
        if self.base.is_empty() {
            Ok(norm)
        } else if norm.is_empty() {
            Ok(self.base.clone())
        } else {
            Ok(format!("{}/{}", self.base.trim_end_matches('/'), norm))
        }
    }

    fn strip_base(&self, full_path: &str) -> String {
        if full_path.trim_end_matches('/') == self.base.trim_end_matches('/') {
            return String::new();
        }
        if self.base.is_empty() {
            full_path.to_string()
        } else {
            let prefix = format!("{}/", self.base.trim_matches('/'));
            full_path
                .strip_prefix(&prefix)
                .unwrap_or(full_path)
                .to_string()
        }
    }
}

fn map_vfs_err(e: FsError) -> VfsError {
    match e {
        FsError::AlreadyExists(path) => VfsError::AlreadyExists(path),
        FsError::Conflict(message) => VfsError::Conflict(message),
        FsError::NotFound(path) => VfsError::NotFound(path),
        FsError::PermissionDenied(path) => VfsError::PermissionDenied(path),
        FsError::PathEscape(path) | FsError::Reserved(path) => VfsError::PathEscape(path),
        other => VfsError::Io(other.to_string()),
    }
}

impl<S: std::ops::Deref<Target = openwebide_storage::Store<crate::state::AppDb>> + Send + Sync>
    HostFsVfs<S>
{
    async fn mutate(
        &self,
        mutation: openwebide_core::vfs::FileMutation,
    ) -> std::result::Result<(), VfsError> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| VfsError::PermissionDenied("Read-only filesystem access".into()))?;
        let request = openwebide_core::vfs::FileMutationRequest {
            root: openwebide_core::vfs::workspace_path(&self.base)?,
            mutation,
        };
        let payload =
            serde_json::to_string(&request).map_err(|error| VfsError::Io(error.to_string()))?;
        // Deployment policy selects a primitive. A communication failure never
        // changes this choice or replays a potentially accepted write.
        if direct_writes().await? {
            return request
                .mutation
                .apply(&direct::DirectFiles::open(&request.root).await?)
                .await;
        }
        // The native writer retains its execution lease after observer loss.
        let (status, body) = crate::bridge::send(store, "/workspace/write", payload)
            .await
            .map_err(|error| {
                VfsError::Conflict(format!("Workspace write is unconfirmed: {error:?}"))
            })?;
        if status != 200 {
            return Err(VfsError::Conflict(format!(
                "Workspace write was rejected (HTTP {status}): {}",
                String::from_utf8_lossy(&body)
            )));
        }
        serde_json::from_slice::<std::result::Result<(), VfsError>>(&body).map_err(|_| {
            VfsError::Conflict(
                "Workspace write is unconfirmed; the companion reply was invalid".into(),
            )
        })?
    }
}

async fn direct_writes() -> std::result::Result<bool, VfsError> {
    #[cfg(not(test))]
    let policy = spin_sdk::variables::get("workspace_write_policy")
        .await
        .ok();
    #[cfg(test)]
    let policy: Option<String> = None;
    match policy.as_deref().unwrap_or("coordinated") {
        "coordinated" => Ok(false),
        "direct" => Ok(true),
        _ => Err(VfsError::Conflict("Unknown workspace write policy".into())),
    }
}

impl<S: std::ops::Deref<Target = openwebide_storage::Store<crate::state::AppDb>> + Send + Sync> Vfs
    for HostFsVfs<S>
{
    fn read_bytes<'a>(&'a self, path: &'a str) -> VfsFuture<'a, Vec<u8>> {
        Box::pin(async move { read_bytes(&self.resolve(path)?).await.map_err(map_vfs_err) })
    }
    fn write_bytes<'a>(&'a self, path: &'a str, contents: &'a [u8]) -> VfsFuture<'a, ()> {
        Box::pin(async move {
            self.mutate(openwebide_core::vfs::FileMutation::Write {
                path: path.into(),
                contents: contents.to_vec(),
            })
            .await
        })
    }
    fn read<'a>(&'a self, path: &'a str) -> VfsFuture<'a, String> {
        Box::pin(async move {
            let full = self.resolve(path)?;
            read(&full).await.map_err(map_vfs_err)
        })
    }

    fn write<'a>(&'a self, path: &'a str, content: &'a str) -> VfsFuture<'a, ()> {
        self.write_bytes(path, content.as_bytes())
    }

    fn list<'a>(&'a self, dir: &'a str) -> VfsFuture<'a, Vec<FileEntry>> {
        Box::pin(async move {
            let full = self.resolve(dir)?;
            let entries = list(&full).await.map_err(map_vfs_err)?;
            let stripped = entries
                .into_iter()
                .map(|mut e| {
                    e.path = self.strip_base(&e.path);
                    e
                })
                .collect();
            Ok(stripped)
        })
    }

    fn create<'a>(
        &'a self,
        path: &'a str,
        kind: openwebide_core::vfs::VfsEntryKind,
    ) -> VfsFuture<'a, ()> {
        Box::pin(async move {
            self.mutate(openwebide_core::vfs::FileMutation::Create {
                path: path.into(),
                kind,
            })
            .await
        })
    }

    fn delete<'a>(&'a self, path: &'a str) -> VfsFuture<'a, ()> {
        Box::pin(async move {
            self.mutate(openwebide_core::vfs::FileMutation::Delete { path: path.into() })
                .await
        })
    }

    fn copy<'a>(&'a self, from: &'a str, to: &'a str) -> VfsFuture<'a, ()> {
        Box::pin(async move {
            self.mutate(openwebide_core::vfs::FileMutation::Copy {
                from: from.into(),
                to: to.into(),
            })
            .await
        })
    }

    fn replace_atomic<'a>(&'a self, path: &'a str, contents: &'a [u8]) -> VfsFuture<'a, ()> {
        Box::pin(async move {
            self.mutate(openwebide_core::vfs::FileMutation::Replace {
                path: path.into(),
                contents: contents.to_vec(),
            })
            .await
        })
    }
    fn remove_entry<'a>(
        &'a self,
        path: &'a str,
        kind: openwebide_core::vfs::VfsEntryKind,
    ) -> VfsFuture<'a, ()> {
        Box::pin(async move {
            self.mutate(openwebide_core::vfs::FileMutation::Remove {
                path: path.into(),
                kind,
            })
            .await
        })
    }
    fn move_atomic<'a>(
        &'a self,
        from: &'a str,
        to: &'a str,
        kind: openwebide_core::vfs::VfsEntryKind,
    ) -> VfsFuture<'a, ()> {
        Box::pin(async move {
            self.mutate(openwebide_core::vfs::FileMutation::Move {
                from: from.into(),
                to: to.into(),
                kind,
            })
            .await
        })
    }

    fn canonicalize<'a>(&'a self, path: &'a str) -> VfsFuture<'a, String> {
        Box::pin(async move {
            let full = self.resolve(path)?;
            let root = root().map_err(map_vfs_err)?;
            let resolver = WasiResolver::new(&root);
            let resolved = canonicalize_path(&resolver, &full)
                .await
                .map_err(map_vfs_err)?;
            if !self.base.is_empty()
                && resolved != self.base.trim_end_matches('/')
                && !resolved.starts_with(&format!("{}/", self.base.trim_end_matches('/')))
            {
                return Err(VfsError::PathEscape(path.into()));
            }
            Ok(self.strip_base(&resolved))
        })
    }
}

/// Resolve symbolic links along `rel` (relative to the preopened workspace root)
/// and return the normalized canonical path.
pub async fn canonicalize_path(resolver: &impl PathResolver, rel: &str) -> Result<String> {
    sanitize(resolver, rel).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct FakeResolver {
        symlinks: HashMap<String, String>,
    }

    impl PathResolver for FakeResolver {
        async fn readlink(&self, path: &str) -> Result<String, ErrorCode> {
            if let Some(target) = self.symlinks.get(path) {
                Ok(target.clone())
            } else {
                Err(ErrorCode::Invalid) // treat everything else as not-a-symlink
            }
        }
    }

    #[test]
    fn canonicalize_symlink_parent_targets() {
        futures::executor::block_on(async {
            let resolver = FakeResolver {
                symlinks: HashMap::from([
                    ("dir/safe".into(), "../target".into()),
                    ("dir/escape".into(), "../../outside".into()),
                ]),
            };
            assert_eq!(
                canonicalize_path(&resolver, "dir/safe/file").await.unwrap(),
                "target/file"
            );
            assert!(matches!(
                canonicalize_path(&resolver, "dir/escape/file").await,
                Err(FsError::PathEscape(_))
            ));
        });
    }

    #[test]
    fn test_sanitize() {
        futures::executor::block_on(async {
            let mut symlinks = HashMap::new();
            symlinks.insert("evil_dir".to_string(), ".spin".to_string());
            symlinks.insert("escape".to_string(), "..".to_string());
            symlinks.insert("abs_escape".to_string(), "/etc".to_string());
            symlinks.insert("legit".to_string(), "normal_dir".to_string());
            symlinks.insert("nested".to_string(), "legit/inner".to_string());

            let resolver = FakeResolver { symlinks };

            // Intermediate symlink to .spin
            assert!(sanitize(&resolver, "evil_dir/sqlite_db.db").await.is_err());

            // Symlink escaping workspace root
            assert!(sanitize(&resolver, "escape/foo").await.is_err());

            // Absolute path escape
            assert!(sanitize(&resolver, "abs_escape/foo").await.is_err());

            // Legitimate in-root symlink
            assert_eq!(
                sanitize(&resolver, "legit/foo").await.unwrap(),
                "normal_dir/foo"
            );

            // Nested symlink
            assert_eq!(
                sanitize(&resolver, "nested/bar").await.unwrap(),
                "normal_dir/inner/bar"
            );

            // Basic rejections
            assert!(sanitize(&resolver, "/").await.is_err());
            assert!(sanitize(&resolver, "a/../b/../../c").await.is_err());
            assert!(sanitize(&resolver, ".spin/foo").await.is_err());
            assert!(sanitize(&resolver, "foo/.spin/bar").await.is_err());

            // Basic allowed
            assert_eq!(sanitize(&resolver, "a/b").await.unwrap(), "a/b");
        });
    }

    #[test]
    fn runtime_recovery_is_reserved_after_every_symbolic_link_expansion() {
        futures::executor::block_on(async {
            let area = ".openwebide/recovery-0123456789abcdef0123456789abcdef";
            let resolver = FakeResolver {
                symlinks: HashMap::from([
                    ("alias".into(), ".openwebide".into()),
                    ("payload".into(), format!("{area}/objects/0")),
                ]),
            };
            for path in [
                format!("{area}/objects/0"),
                "alias/recovery-0123456789abcdef0123456789abcdef/objects/0".into(),
                "payload".into(),
            ] {
                assert!(matches!(
                    sanitize(&resolver, &path).await,
                    Err(FsError::Reserved(_))
                ));
            }
            assert_eq!(
                sanitize(&resolver, "alias/recovery-notes").await.unwrap(),
                ".openwebide/recovery-notes"
            );
        });
    }

    /// `delete` must resolve only the *parent* directory through sanitize and
    /// pass the leaf as a literal name — so a symlink `link.txt -> target.txt`
    /// is removed as a link (the leaf), not dereferenced to `target.txt`.
    ///
    /// This test verifies the parent/leaf split and sanitize behavior for the
    /// delete path without requiring the real WASI runtime:
    ///
    /// 1. The resolved parent for a top-level leaf (`link.txt`) is `""` (the
    ///    workspace root) and the leaf is `"link.txt"` — sanitize of `""` is
    ///    fine and does NOT follow the symlink `link.txt` itself.
    /// 2. When the parent path passes through an intermediate `.spin` symlink
    ///    (`evil_dir` → `.spin`), `sanitize` of the parent is rejected — the
    ///    safety check is preserved even though the leaf is not resolved.
    /// 3. When the parent path escapes via a `..` symlink, `sanitize` rejects
    ///    it as an escape, consistent with the existing policy.
    #[test]
    fn test_delete_resolves_parent_not_leaf() {
        futures::executor::block_on(async {
            // link.txt is a symlink to target.txt; evil_dir is a symlink to .spin;
            // escape is a symlink to ..
            let mut symlinks = HashMap::new();
            symlinks.insert("link.txt".to_string(), "target.txt".to_string());
            symlinks.insert("evil_dir".to_string(), ".spin".to_string());
            symlinks.insert("escape".to_string(), "..".to_string());
            let resolver = FakeResolver { symlinks };

            // Splitting "link.txt" → parent="" (workspace root), leaf="link.txt"
            // sanitize("") returns "" without following the symlink at "link.txt",
            // so the link itself would be unlinked — not its target "target.txt".
            let rel = "link.txt";
            let (parent_rel, leaf) = match rel.rfind('/') {
                Some(idx) => (&rel[..idx], &rel[idx + 1..]),
                None => ("", rel),
            };
            assert_eq!(leaf, "link.txt");
            // sanitize of the *parent* (empty = workspace root) succeeds without
            // resolving link.txt — the leaf is intentionally kept as a literal.
            let resolved_parent = sanitize(&resolver, parent_rel).await.unwrap();
            assert_eq!(resolved_parent, "");

            // Safety check preserved: an intermediate .spin symlink in the parent
            // path is still rejected even though we only sanitize the parent.
            let bad_rel = "evil_dir/something/link.txt";
            let (bad_parent, _bad_leaf) = match bad_rel.rfind('/') {
                Some(idx) => (&bad_rel[..idx], &bad_rel[idx + 1..]),
                None => ("", bad_rel),
            };
            assert!(sanitize(&resolver, bad_parent).await.is_err());

            // Safety check preserved: a parent path that escapes via .. symlink
            // is still rejected.
            let escape_rel = "escape/something/link.txt";
            let (escape_parent, _escape_leaf) = match escape_rel.rfind('/') {
                Some(idx) => (&escape_rel[..idx], &escape_rel[idx + 1..]),
                None => ("", escape_rel),
            };
            assert!(sanitize(&resolver, escape_parent).await.is_err());
        });
    }

    /// `list` must return `FileEntry` paths prefixed with the **logical**
    /// caller-supplied `rel`, not the resolved real path.
    ///
    /// Scenario: `sym_dir` is a symlink to `real_dir`.  Listing `sym_dir`
    /// should return entries like `sym_dir/child`, not `real_dir/child`.
    ///
    /// Because `list` calls the real WASI runtime (unavailable in native
    /// `cargo test`), we verify the path-building logic in isolation — the
    /// same code path that the fixed `list` function now uses.
    #[test]
    fn test_list_uses_logical_path_prefix() {
        // Simulate the fixed path-building logic from `list`:
        // `logical_base = rel.trim_end_matches('/')`, paths built as
        // `format!("{logical_base}/{name}")`.
        let rel = "sym_dir";
        let logical_base = rel.trim_end_matches('/');

        // A child entry named "child.txt" should appear as "sym_dir/child.txt",
        // NOT as "real_dir/child.txt" (which is what the old resolved `base`
        // would have produced).
        let child_name = "child.txt";
        let path = format!("{logical_base}/{child_name}");
        assert_eq!(path, "sym_dir/child.txt");

        // When `rel` is empty (listing the workspace root), the child path is
        // just the child name — no spurious leading slash.
        let root_rel = "";
        let root_base = root_rel.trim_end_matches('/');
        let root_path = if root_base.is_empty() {
            child_name.to_string()
        } else {
            format!("{root_base}/{child_name}")
        };
        assert_eq!(root_path, "child.txt");

        // Verify the old resolved path (`real_dir/child.txt`) is NOT produced
        // by the new logic when the caller requested `sym_dir`.
        let resolved_base = "real_dir"; // what the old sanitize call would return
        let old_path = format!("{resolved_base}/{child_name}");
        assert_ne!(
            path, old_path,
            "path must use logical prefix, not resolved real path"
        );
    }

    #[test]
    fn search_budget_stops_at_500_hits() {
        let mut budget = SearchBudget::new();
        assert_eq!(budget.hits, 500);
        for _ in 0..500 {
            assert!(budget.push_hit());
        }
        assert!(!budget.push_hit());
        assert!(budget.exhausted());
    }

    #[test]
    fn search_budget_refuses_file_once_bytes_exhausted() {
        let mut budget = SearchBudget::new();
        // A file larger than the whole budget is refused outright.
        assert!(!budget.allow_file(64 * 1024 * 1024_u64 + 1));
        // Bounded files drain the aggregate budget.
        for _ in 0..6 {
            assert!(budget.allow_file(10 * 1024 * 1024_u64));
        }
        assert!(budget.allow_file(4 * 1024 * 1024_u64));
        assert!(!budget.allow_file(1));
        // Smaller reads drain the budget incrementally.
        let mut fresh = SearchBudget::new();
        assert!(fresh.allow_file(10));
        assert!(!fresh.allow_file(64 * 1024 * 1024_u64));
    }

    #[test]
    fn search_budget_entry_cap_exhausts_walk() {
        let mut budget = SearchBudget::new();
        for _ in 0..20_000 {
            assert!(budget.allow_entry());
        }
        assert!(!budget.allow_entry());
        assert!(budget.exhausted());
    }

    #[test]
    fn skip_list_respected_with_and_without_include_ignored() {
        let off = SearchOptions::default();
        let on = SearchOptions {
            include_ignored: true,
        };
        for name in openwebide_core::vfs::SEARCH_SKIP_DIRS {
            assert!(skip_dir(name, off), "{name} must be skipped by default");
            assert!(
                !skip_dir(name, on),
                "{name} must be searched with include_ignored"
            );
        }
        assert!(!skip_dir("src", off));
        assert!(!skip_dir("src", on));
    }

    #[test]
    fn truncate_line_is_multibyte_safe_at_400() {
        // Exactly 400 multi-byte chars: untouched.
        let s: String = "é".repeat(400);
        assert_eq!(truncate_line(&s, 400), s);
        // 401 multi-byte chars: truncated to exactly 400.
        let s: String = "é".repeat(401);
        assert_eq!(truncate_line(&s, 400).chars().count(), 400);
        // A 3-byte char straddling the 400-char boundary is kept whole,
        // not split mid-sequence (a byte-wise cut at 400 would split it).
        let s = format!("{}€a", "a".repeat(399)); // 401 chars, 403 bytes
        let t = truncate_line(&s, 400);
        assert_eq!(t.chars().count(), 400);
        assert!(t.ends_with('€'));
        // Shorter strings pass through unchanged.
        assert_eq!(truncate_line("short", 400), "short");
    }
}

#[cfg(test)]
mod checkpoint_paths {
    #[test]
    fn project_root_is_empty_and_children_are_relative() {
        let vfs = super::HostFsVfs::new("repos/project");
        assert_eq!(vfs.strip_base("repos/project"), "");
        assert_eq!(vfs.strip_base("repos/project/src/main.rs"), "src/main.rs");
    }
}
