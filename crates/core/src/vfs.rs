//! Unified Virtual File System (VFS) abstraction.
//!
//! Provides a single async interface for file operations across both
//! remote mode (WASI filesystem / Spin preopens) and local mode
//! (browser File System Access API), plus in-memory testing.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::{FileEntry, SearchHit};

/// Errors encountered during Virtual File System operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VfsError {
    NotFound(String),
    PermissionDenied(String),
    PermissionRequired(String),
    AlreadyExists(String),
    PathEscape(String),
    Io(String),
}

impl std::fmt::Display for VfsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(p) => write!(f, "path not found: {p}"),
            Self::PermissionRequired(detail) => f.write_str(detail),
            Self::PermissionDenied(p) => write!(f, "permission denied: {p}"),
            Self::AlreadyExists(p) => write!(f, "already exists: {p}"),
            Self::PathEscape(p) => write!(f, "path escapes workspace root: {p}"),
            Self::Io(msg) => write!(f, "I/O error: {msg}"),
        }
    }
}

impl std::error::Error for VfsError {}

/// A pinned, heap-allocated, Send-safe future returning a Vfs result.
pub type VfsFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, VfsError>> + Send + 'a>>;

/// Maximum allocation for reading one workspace file.
pub const MAX_READ_BYTES: u64 = 10 * 1024 * 1024;

/// Explorer order at every level: directories first, then natural, case-insensitive names.
/// Raw names and paths break ties so enumeration order never affects the result.
pub fn sort_file_entries(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| natural_name_cmp(&a.name.to_lowercase(), &b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.path.cmp(&b.path))
    });
}

fn natural_name_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    while let (Some(&left), Some(&right)) = (a.first(), b.first()) {
        let order = if left.is_ascii_digit() && right.is_ascii_digit() {
            let left_len = a.iter().take_while(|byte| byte.is_ascii_digit()).count();
            let right_len = b.iter().take_while(|byte| byte.is_ascii_digit()).count();
            let (left_digits, left_rest) = a.split_at(left_len);
            let (right_digits, right_rest) = b.split_at(right_len);
            // Compare magnitude without parsing: filenames can contain arbitrarily long numbers.
            let left_digits = &left_digits[left_digits
                .iter()
                .position(|&byte| byte != b'0')
                .unwrap_or(left_digits.len())..];
            let right_digits = &right_digits[right_digits
                .iter()
                .position(|&byte| byte != b'0')
                .unwrap_or(right_digits.len())..];
            a = left_rest;
            b = right_rest;
            left_digits
                .len()
                .cmp(&right_digits.len())
                .then_with(|| left_digits.cmp(right_digits))
        } else {
            a = &a[1..];
            b = &b[1..];
            left.cmp(&right)
        };
        if order != std::cmp::Ordering::Equal {
            return order;
        }
    }
    a.len().cmp(&b.len())
}

/// Normalize a raw user or model-supplied path into a canonical workspace-relative POSIX path.
///
/// Rules:
/// 1. Replaces Windows backslashes with `/`.
/// 2. Resolves current-dir `.` and parent-dir `..` segments.
/// 3. Strips leading slashes (e.g. `/src/main.rs` -> `src/main.rs`).
/// 4. Rejects any path where `..` escapes above the workspace root.
/// 5. Root is represented as empty string `""`.
pub fn normalize_vfs_path(raw: &str) -> Result<String, VfsError> {
    let clean = raw.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();

    for seg in clean.split('/') {
        match seg {
            "" | "." => continue,
            ".." => {
                if parts.pop().is_none() {
                    return Err(VfsError::PathEscape(raw.to_string()));
                }
            }
            s => parts.push(s),
        }
    }

    Ok(parts.join("/"))
}

/// Shared lexical path contract; runtime adapters additionally resolve symlinks.
pub fn workspace_path(raw: &str) -> Result<String, VfsError> {
    let path = normalize_vfs_path(raw)?;
    if path.split('/').any(|part| part == ".spin") {
        return Err(VfsError::PathEscape(raw.to_string()));
    }
    Ok(path)
}

/// Format a Unix timestamp (seconds since epoch) into a human-readable UTC string.
///
/// Example: `Tuesday, September 22, 2026 12:42 UTC`
pub fn format_utc_timestamp(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem_secs = u32::try_from(secs.rem_euclid(86400)).expect("seconds within one day fit u32");
    let hour = rem_secs / 3600;
    let minute = (rem_secs % 3600) / 60;

    // Howard Hinnant's civil day calculation
    // Shift epoch from 1970-01-01 to 0000-03-01
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = u32::try_from(z - era * 146097).expect("days within a 400-year era fit u32");
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = i64::from(yoe) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    let weekday = match (days + 4).rem_euclid(7) {
        0 => "Sunday",
        1 => "Monday",
        2 => "Tuesday",
        3 => "Wednesday",
        4 => "Thursday",
        5 => "Friday",
        6 => "Saturday",
        _ => "Unknown",
    };

    let month_name = match m {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => "Unknown",
    };

    format!("{weekday}, {month_name} {d}, {y} {hour:02}:{minute:02} UTC")
}

/// Agent backups of files it overwrote without being able to read them; one sub-directory per tool step.
pub const AGENT_BACKUP_DIR: &str = ".openwebide/backups";

/// Directory names that content search skips by default.
///
/// Every walker (backend, agent, local mode) uses this list unless
/// `SearchOptions::include_ignored` is set.
pub const SEARCH_SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", "dist", ".spin"];

/// Per-query options for content search.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchOptions {
    /// Also search the directories in `SEARCH_SKIP_DIRS`.
    pub include_ignored: bool,
}

/// Whether a directory named `name` should be skipped by content search
/// under `opts`.
pub fn skip_dir(name: &str, opts: SearchOptions) -> bool {
    !opts.include_ignored && SEARCH_SKIP_DIRS.contains(&name)
}

/// The workspace entry to create. Transport adapters retain the legacy `is_dir` wire field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsEntryKind {
    File,
    Directory,
}

impl VfsEntryKind {
    pub fn is_dir(self) -> bool {
        self == Self::Directory
    }
}

/// The core asynchronous Virtual File System trait.
pub trait Vfs: Send + Sync {
    /// Read the full UTF-8 contents of a workspace file.
    fn read<'a>(&'a self, path: &'a str) -> VfsFuture<'a, String>;

    /// Read bytes without UTF-8 conversion, retaining the shared read limit.
    fn read_bytes<'a>(&'a self, path: &'a str) -> VfsFuture<'a, Vec<u8>> {
        Box::pin(async move { self.read(path).await.map(String::into_bytes) })
    }
    /// Write bytes without changing their encoding.
    fn write_bytes<'a>(&'a self, path: &'a str, contents: &'a [u8]) -> VfsFuture<'a, ()> {
        Box::pin(async move {
            let text =
                std::str::from_utf8(contents).map_err(|error| VfsError::Io(error.to_string()))?;
            self.write(path, text).await
        })
    }

    /// Write or overwrite a file with the given full contents.
    fn write<'a>(&'a self, path: &'a str, content: &'a str) -> VfsFuture<'a, ()>;

    /// List entries in a directory relative to workspace root (`""` for root).
    fn list<'a>(&'a self, dir: &'a str) -> VfsFuture<'a, Vec<FileEntry>>;

    /// Create an empty file or directory.
    ///
    /// For files (`is_dir = false`): exclusive — returns `VfsError::AlreadyExists` if
    /// a file already exists at `path`; content is left untouched.
    /// For directories (`is_dir = true`): idempotent — returns `Ok(())` if the
    /// directory already exists.
    fn create<'a>(&'a self, path: &'a str, kind: VfsEntryKind) -> VfsFuture<'a, ()>;

    /// Delete a file or recursively delete a directory.
    fn delete<'a>(&'a self, path: &'a str) -> VfsFuture<'a, ()>;

    /// Copy a file.
    fn copy<'a>(&'a self, from: &'a str, to: &'a str) -> VfsFuture<'a, ()> {
        let _ = from;
        let _ = to;
        Box::pin(async {
            Err(VfsError::Io(
                "copy is not supported by this workspace".into(),
            ))
        })
    }

    /// Full-text search across file contents under the given directory (`""` for root).
    fn search_content<'a>(
        &'a self,
        query: &'a str,
        dir: &'a str,
        opts: SearchOptions,
    ) -> VfsFuture<'a, Vec<SearchHit>> {
        Box::pin(crate::search::content(self, query, dir, opts))
    }

    /// Resolve symbolic links and return the canonical workspace-relative path.
    ///
    /// The default implementation falls back to lexical normalization via [`normalize_vfs_path`].
    fn canonicalize<'a>(&'a self, path: &'a str) -> VfsFuture<'a, String> {
        Box::pin(async move { normalize_vfs_path(path) })
    }
}

impl<V: Vfs + ?Sized> Vfs for &V {
    fn read_bytes<'a>(&'a self, path: &'a str) -> VfsFuture<'a, Vec<u8>> {
        (**self).read_bytes(path)
    }
    fn write_bytes<'a>(&'a self, path: &'a str, bytes: &'a [u8]) -> VfsFuture<'a, ()> {
        (**self).write_bytes(path, bytes)
    }
    fn read<'a>(&'a self, path: &'a str) -> VfsFuture<'a, String> {
        (**self).read(path)
    }

    fn write<'a>(&'a self, path: &'a str, content: &'a str) -> VfsFuture<'a, ()> {
        (**self).write(path, content)
    }

    fn list<'a>(&'a self, dir: &'a str) -> VfsFuture<'a, Vec<FileEntry>> {
        (**self).list(dir)
    }

    fn create<'a>(&'a self, path: &'a str, kind: VfsEntryKind) -> VfsFuture<'a, ()> {
        (**self).create(path, kind)
    }

    fn delete<'a>(&'a self, path: &'a str) -> VfsFuture<'a, ()> {
        (**self).delete(path)
    }

    fn copy<'a>(&'a self, from: &'a str, to: &'a str) -> VfsFuture<'a, ()> {
        (**self).copy(from, to)
    }

    fn search_content<'a>(
        &'a self,
        query: &'a str,
        dir: &'a str,
        opts: SearchOptions,
    ) -> VfsFuture<'a, Vec<SearchHit>> {
        (**self).search_content(query, dir, opts)
    }

    fn canonicalize<'a>(&'a self, path: &'a str) -> VfsFuture<'a, String> {
        (**self).canonicalize(path)
    }
}

impl<V: Vfs + ?Sized> Vfs for Arc<V> {
    fn read_bytes<'a>(&'a self, path: &'a str) -> VfsFuture<'a, Vec<u8>> {
        (**self).read_bytes(path)
    }
    fn write_bytes<'a>(&'a self, path: &'a str, bytes: &'a [u8]) -> VfsFuture<'a, ()> {
        (**self).write_bytes(path, bytes)
    }
    fn read<'a>(&'a self, path: &'a str) -> VfsFuture<'a, String> {
        (**self).read(path)
    }

    fn write<'a>(&'a self, path: &'a str, content: &'a str) -> VfsFuture<'a, ()> {
        (**self).write(path, content)
    }

    fn list<'a>(&'a self, dir: &'a str) -> VfsFuture<'a, Vec<FileEntry>> {
        (**self).list(dir)
    }

    fn create<'a>(&'a self, path: &'a str, kind: VfsEntryKind) -> VfsFuture<'a, ()> {
        (**self).create(path, kind)
    }

    fn delete<'a>(&'a self, path: &'a str) -> VfsFuture<'a, ()> {
        (**self).delete(path)
    }

    fn copy<'a>(&'a self, from: &'a str, to: &'a str) -> VfsFuture<'a, ()> {
        (**self).copy(from, to)
    }

    fn search_content<'a>(
        &'a self,
        query: &'a str,
        dir: &'a str,
        opts: SearchOptions,
    ) -> VfsFuture<'a, Vec<SearchHit>> {
        (**self).search_content(query, dir, opts)
    }

    fn canonicalize<'a>(&'a self, path: &'a str) -> VfsFuture<'a, String> {
        (**self).canonicalize(path)
    }
}

/// An in-memory Vfs implementation for unit tests and local staging.
#[derive(Debug, Clone, Default)]
pub struct MemoryVfs {
    files: Arc<RwLock<BTreeMap<String, Vec<u8>>>>,
    dirs: Arc<RwLock<BTreeSet<String>>>,
    symlinks: Arc<RwLock<BTreeMap<String, String>>>,
}

impl MemoryVfs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a symbolic link pointing from `link` to `target`.
    pub fn add_symlink(&self, link: &str, target: &str) -> Result<(), VfsError> {
        let norm_link = normalize_vfs_path(link)?;
        let mut symlinks = self
            .symlinks
            .write()
            .map_err(|e| VfsError::Io(e.to_string()))?;
        symlinks.insert(norm_link, target.replace('\\', "/"));
        Ok(())
    }
}

impl Vfs for MemoryVfs {
    fn canonicalize<'a>(&'a self, path: &'a str) -> VfsFuture<'a, String> {
        Box::pin(async move {
            let mut current = normalize_vfs_path(path)?;
            let symlinks = self
                .symlinks
                .read()
                .map_err(|e| VfsError::Io(e.to_string()))?;
            if symlinks.is_empty() {
                return Ok(current);
            }

            let mut iterations = 0;
            const MAX_SYMLINK_EXPANSIONS: usize = 32;

            'outer: loop {
                let parts: Vec<&str> = current.split('/').filter(|p| !p.is_empty()).collect();
                let mut prefix = String::new();

                for (idx, seg) in parts.iter().enumerate() {
                    if !prefix.is_empty() {
                        prefix.push('/');
                    }
                    prefix.push_str(seg);

                    if let Some(target) = symlinks.get(&prefix) {
                        iterations += 1;
                        if iterations > MAX_SYMLINK_EXPANSIONS {
                            return Err(VfsError::Io("too many levels of symbolic links".into()));
                        }

                        let parent = prefix.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
                        let resolved_target = if parent.is_empty() {
                            target.clone()
                        } else if target.starts_with('/') {
                            target.trim_start_matches('/').to_string()
                        } else {
                            format!("{parent}/{target}")
                        };

                        let remainder = &parts[idx + 1..];
                        let next = if remainder.is_empty() {
                            resolved_target
                        } else {
                            format!("{resolved_target}/{}", remainder.join("/"))
                        };

                        current = normalize_vfs_path(&next)?;
                        continue 'outer;
                    }
                }

                break;
            }

            Ok(current)
        })
    }

    fn read<'a>(&'a self, path: &'a str) -> VfsFuture<'a, String> {
        Box::pin(async move {
            let bytes = self.read_bytes(path).await?;
            String::from_utf8(bytes).map_err(|e| VfsError::Io(e.to_string()))
        })
    }

    fn read_bytes<'a>(&'a self, path: &'a str) -> VfsFuture<'a, Vec<u8>> {
        Box::pin(async move {
            let norm = self.canonicalize(path).await?;
            let files = self.files.read().map_err(|e| VfsError::Io(e.to_string()))?;
            files.get(&norm).cloned().ok_or(VfsError::NotFound(norm))
        })
    }

    fn write<'a>(&'a self, path: &'a str, content: &'a str) -> VfsFuture<'a, ()> {
        self.write_bytes(path, content.as_bytes())
    }

    fn write_bytes<'a>(&'a self, path: &'a str, contents: &'a [u8]) -> VfsFuture<'a, ()> {
        Box::pin(async move {
            let norm = self.canonicalize(path).await?;
            if norm.is_empty() {
                return Err(VfsError::Io("cannot write to root".into()));
            }

            // Ensure parent directory components exist in dirs set
            if let Some(parent) = norm.rsplit_once('/').map(|(p, _)| p) {
                let mut dirs = self.dirs.write().map_err(|e| VfsError::Io(e.to_string()))?;
                let mut current = String::new();
                for part in parent.split('/') {
                    if !current.is_empty() {
                        current.push('/');
                    }
                    current.push_str(part);
                    dirs.insert(current.clone());
                }
            }

            let mut files = self
                .files
                .write()
                .map_err(|e| VfsError::Io(e.to_string()))?;
            files.insert(norm, contents.to_vec());
            Ok(())
        })
    }

    fn copy<'a>(&'a self, from: &'a str, to: &'a str) -> VfsFuture<'a, ()> {
        Box::pin(async move {
            let from_norm = self.canonicalize(from).await?;
            let to_norm = self.canonicalize(to).await?;

            let content = {
                let files = self.files.read().map_err(|e| VfsError::Io(e.to_string()))?;
                files
                    .get(&from_norm)
                    .cloned()
                    .ok_or(VfsError::NotFound(from_norm))?
            };

            if to_norm.is_empty() {
                return Err(VfsError::Io("cannot write to root".into()));
            }

            // Ensure parent directory components exist in dirs set
            if let Some(parent) = to_norm.rsplit_once('/').map(|(p, _)| p) {
                let mut dirs = self.dirs.write().map_err(|e| VfsError::Io(e.to_string()))?;
                let mut current = String::new();
                for part in parent.split('/') {
                    if !current.is_empty() {
                        current.push('/');
                    }
                    current.push_str(part);
                    dirs.insert(current.clone());
                }
            }

            let mut files = self
                .files
                .write()
                .map_err(|e| VfsError::Io(e.to_string()))?;
            files.insert(to_norm, content);
            Ok(())
        })
    }

    fn list<'a>(&'a self, dir: &'a str) -> VfsFuture<'a, Vec<FileEntry>> {
        Box::pin(async move {
            let norm = normalize_vfs_path(dir)?;
            let files = self.files.read().map_err(|e| VfsError::Io(e.to_string()))?;
            let dirs = self.dirs.read().map_err(|e| VfsError::Io(e.to_string()))?;

            let prefix = if norm.is_empty() {
                String::new()
            } else {
                format!("{norm}/")
            };

            let mut seen_children = BTreeSet::new();
            let mut entries = Vec::new();

            // Collect immediate subdirectories
            for d in dirs.iter() {
                if let Some(sub) = d.strip_prefix(&prefix)
                    && !sub.is_empty()
                {
                    let direct_child = sub.split('/').next().unwrap_or(sub);
                    if seen_children.insert((direct_child.to_string(), true)) {
                        let child_path = if norm.is_empty() {
                            direct_child.to_string()
                        } else {
                            format!("{norm}/{direct_child}")
                        };
                        entries.push(FileEntry {
                            name: direct_child.to_string(),
                            path: child_path,
                            is_dir: true,
                            size: 0,
                        });
                    }
                }
            }

            // Collect immediate files
            for (f, content) in files.iter() {
                if let Some(sub) = f.strip_prefix(&prefix)
                    && !sub.is_empty()
                {
                    if !sub.contains('/') {
                        if seen_children.insert((sub.to_string(), false)) {
                            entries.push(FileEntry {
                                name: sub.to_string(),
                                path: f.clone(),
                                is_dir: false,
                                size: content.len() as u64,
                            });
                        }
                    } else {
                        // Subdirectory implied by file path
                        let direct_child = sub.split('/').next().unwrap_or(sub);
                        if seen_children.insert((direct_child.to_string(), true)) {
                            let child_path = if norm.is_empty() {
                                direct_child.to_string()
                            } else {
                                format!("{norm}/{direct_child}")
                            };
                            entries.push(FileEntry {
                                name: direct_child.to_string(),
                                path: child_path,
                                is_dir: true,
                                size: 0,
                            });
                        }
                    }
                }
            }

            sort_file_entries(&mut entries);

            Ok(entries)
        })
    }

    fn create<'a>(&'a self, path: &'a str, kind: VfsEntryKind) -> VfsFuture<'a, ()> {
        Box::pin(async move {
            let norm = normalize_vfs_path(path)?;
            if norm.is_empty() {
                return Ok(());
            }

            if kind.is_dir() {
                let mut dirs = self.dirs.write().map_err(|e| VfsError::Io(e.to_string()))?;
                dirs.insert(norm);
            } else {
                let mut files = self
                    .files
                    .write()
                    .map_err(|e| VfsError::Io(e.to_string()))?;
                if files.contains_key(&norm) {
                    return Err(VfsError::AlreadyExists(norm));
                }
                files.insert(norm, Vec::new());
            }
            Ok(())
        })
    }

    fn delete<'a>(&'a self, path: &'a str) -> VfsFuture<'a, ()> {
        Box::pin(async move {
            let norm = normalize_vfs_path(path)?;
            if norm.is_empty() {
                return Err(VfsError::Io("cannot delete workspace root".into()));
            }

            let mut files = self
                .files
                .write()
                .map_err(|e| VfsError::Io(e.to_string()))?;
            let mut dirs = self.dirs.write().map_err(|e| VfsError::Io(e.to_string()))?;

            let prefix = format!("{norm}/");
            files.retain(|k, _| k != &norm && !k.starts_with(&prefix));
            dirs.retain(|k| k != &norm && !k.starts_with(&prefix));
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_vfs_path_basic() {
        assert_eq!(normalize_vfs_path("src/main.rs").unwrap(), "src/main.rs");
        assert_eq!(normalize_vfs_path("./src/main.rs").unwrap(), "src/main.rs");
        assert_eq!(normalize_vfs_path("/src/main.rs").unwrap(), "src/main.rs");
        assert_eq!(normalize_vfs_path("src//main.rs").unwrap(), "src/main.rs");
        assert_eq!(normalize_vfs_path("src/./main.rs").unwrap(), "src/main.rs");
        assert_eq!(
            normalize_vfs_path("src/utils/../main.rs").unwrap(),
            "src/main.rs"
        );
        assert_eq!(
            normalize_vfs_path("crates\\core\\src\\lib.rs").unwrap(),
            "crates/core/src/lib.rs"
        );
    }

    #[test]
    fn test_normalize_vfs_path_root() {
        assert_eq!(normalize_vfs_path("").unwrap(), "");
        assert_eq!(normalize_vfs_path(".").unwrap(), "");
        assert_eq!(normalize_vfs_path("./").unwrap(), "");
        assert_eq!(normalize_vfs_path("/").unwrap(), "");
        assert_eq!(normalize_vfs_path("///").unwrap(), "");
    }

    #[test]
    fn test_normalize_vfs_path_escape_rejected() {
        assert!(matches!(
            normalize_vfs_path(".."),
            Err(VfsError::PathEscape(_))
        ));
        assert!(matches!(
            normalize_vfs_path("../foo"),
            Err(VfsError::PathEscape(_))
        ));
        assert!(matches!(
            normalize_vfs_path("foo/../../bar"),
            Err(VfsError::PathEscape(_))
        ));
        assert!(matches!(
            normalize_vfs_path("/.."),
            Err(VfsError::PathEscape(_))
        ));
    }

    #[test]
    fn test_skip_dir() {
        let default = SearchOptions::default();
        let include = SearchOptions {
            include_ignored: true,
        };

        for name in SEARCH_SKIP_DIRS {
            assert!(
                skip_dir(name, default),
                "{name} should be skipped by default"
            );
            assert!(
                !skip_dir(name, include),
                "{name} should not be skipped with include_ignored"
            );
        }

        assert!(!skip_dir("src", default));
        assert!(!skip_dir("src", include));
    }

    #[test]
    fn test_memory_vfs_crud() {
        futures::executor::block_on(async {
            let vfs = MemoryVfs::new();

            // Write and read
            vfs.write("src/lib.rs", "pub fn hello() {}").await.unwrap();
            let content = vfs.read("src/lib.rs").await.unwrap();
            assert_eq!(content, "pub fn hello() {}");

            // Read non-existent
            assert!(matches!(
                vfs.read("src/missing.rs").await,
                Err(VfsError::NotFound(_))
            ));

            // Listing root
            vfs.write("README.md", "# Project").await.unwrap();
            let entries = vfs.list("").await.unwrap();
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].name, "src");
            assert!(entries[0].is_dir);
            assert_eq!(entries[1].name, "README.md");
            assert!(!entries[1].is_dir);

            // Listing subdirectory
            let src_entries = vfs.list("src").await.unwrap();
            assert_eq!(src_entries.len(), 1);
            assert_eq!(src_entries[0].name, "lib.rs");
            assert!(!src_entries[0].is_dir);

            // Search
            let hits = vfs
                .search_content("hello", "", SearchOptions::default())
                .await
                .unwrap();
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].path, "src/lib.rs");
            assert_eq!(hits[0].line, 1);

            // Delete
            vfs.delete("src").await.unwrap();
            assert!(matches!(
                vfs.read("src/lib.rs").await,
                Err(VfsError::NotFound(_))
            ));
            let entries_after = vfs.list("").await.unwrap();
            assert_eq!(entries_after.len(), 1);
            assert_eq!(entries_after[0].name, "README.md");
        });
    }

    #[test]
    fn test_format_utc_timestamp() {
        assert_eq!(
            format_utc_timestamp(0),
            "Thursday, January 1, 1970 00:00 UTC"
        );
        // 2026-09-22 18:14:05 UTC -> 1790100845
        assert_eq!(
            format_utc_timestamp(1790100845),
            "Tuesday, September 22, 2026 18:14 UTC"
        );
    }

    #[test]
    fn test_memory_vfs_symlink_canonicalization() {
        futures::executor::block_on(async {
            let vfs = MemoryVfs::new();
            vfs.add_symlink("foo", ".git").unwrap();
            assert_eq!(vfs.canonicalize("foo/config").await.unwrap(), ".git/config");

            vfs.add_symlink("sub/bar", "../.git").unwrap();
            assert_eq!(
                vfs.canonicalize("sub/bar/hooks/pre-commit").await.unwrap(),
                ".git/hooks/pre-commit"
            );

            vfs.add_symlink("link_src", "src").unwrap();
            vfs.write("link_src/main.rs", "fn main() {}").await.unwrap();
            assert_eq!(vfs.read("src/main.rs").await.unwrap(), "fn main() {}");

            // Escaping symlink
            vfs.add_symlink("escape", "../../etc").unwrap();
            assert!(matches!(
                vfs.canonicalize("escape/passwd").await,
                Err(VfsError::PathEscape(_))
            ));
        });
    }

    #[test]
    fn test_memory_vfs_create_exclusive() {
        futures::executor::block_on(async {
            let vfs = MemoryVfs::new();

            // create on a new path succeeds and leaves an empty file
            vfs.create("src/new.rs", VfsEntryKind::File).await.unwrap();
            assert_eq!(vfs.read("src/new.rs").await.unwrap(), "");

            // write some content, then create again → AlreadyExists, content unchanged
            vfs.write("src/existing.rs", "hello").await.unwrap();
            let err = vfs
                .create("src/existing.rs", VfsEntryKind::File)
                .await
                .unwrap_err();
            assert!(matches!(err, VfsError::AlreadyExists(_)));
            assert_eq!(vfs.read("src/existing.rs").await.unwrap(), "hello");

            // create on an existing directory is idempotent
            vfs.create("src", VfsEntryKind::Directory).await.unwrap();
            vfs.create("src", VfsEntryKind::Directory).await.unwrap();
        });
    }
}

#[cfg(test)]
mod ordering_tests {
    use super::*;

    #[test]
    fn explorer_order_is_natural_and_independent_of_enumeration() {
        let mut entries: Vec<_> = [
            ("file10", false),
            ("z2", true),
            ("file2", false),
            ("alpha", false),
            ("a10", true),
            ("a2", true),
            ("file02", false),
            ("Alpha", false),
            ("file999999999999999999999999999999", false),
            ("file1000000000000000000000000000000", false),
        ]
        .into_iter()
        .map(|(name, is_dir)| FileEntry {
            name: name.into(),
            path: name.into(),
            is_dir,
            size: 0,
        })
        .collect();
        sort_file_entries(&mut entries);
        let expected = entries.clone();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            [
                "a2",
                "a10",
                "z2",
                "Alpha",
                "alpha",
                "file02",
                "file2",
                "file10",
                "file999999999999999999999999999999",
                "file1000000000000000000000000000000"
            ]
        );
        entries.reverse();
        sort_file_entries(&mut entries);
        assert_eq!(entries, expected);
    }
}

#[cfg(test)]
mod creation_contract {
    #[test]
    fn memory() {
        futures::executor::block_on(crate::testing::vfs_creation_contract(
            &super::MemoryVfs::new(),
        ));
    }
}
