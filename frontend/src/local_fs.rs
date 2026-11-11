//! File System Access API helpers for local-mode workspaces. Paths are
//! project-relative (the picked directory is the project root, path "").

use send_wrapper::SendWrapper;

use openwebide_core::{FileEntry, SearchHit, Vfs, VfsError, VfsFuture, vfs::SearchOptions};
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    Blob, DirectoryPickerOptions, File, FileSystemDirectoryHandle, FileSystemFileHandle,
    FileSystemGetDirectoryOptions, FileSystemHandle, FileSystemHandleKind,
    FileSystemPermissionMode, FileSystemWritableFileStream,
};

/// Extract a human-readable message from a rejected JS value. File System
/// Access API and IndexedDB failures reject with a `DOMException` (an
/// object), whose `as_string()` is `None`; defaulting that to `""` would
/// surface an empty error. Fall back to the object's `name`/`message` fields.
pub fn js_error(e: &JsValue) -> String {
    if let Some(s) = e.as_string() {
        return s;
    }
    let get = |k: &str| {
        js_sys::Reflect::get(e, &JsValue::from_str(k))
            .ok()
            .and_then(|v| v.as_string())
            .filter(|s| !s.is_empty())
    };
    match (get("name"), get("message")) {
        (Some(n), Some(m)) if n != "Error" => format!("{n}: {m}"),
        (Some(n), None) => n,
        (None, Some(m)) => m,
        _ => "operation failed".to_string(),
    }
}

fn js_vfs_error(error: JsValue) -> VfsError {
    let name = js_sys::Reflect::get(&error, &JsValue::from_str("name"))
        .ok()
        .and_then(|value| value.as_string());
    let detail = js_error(&error);
    match name.as_deref() {
        Some("NotFoundError") => VfsError::NotFound(detail),
        Some("NotAllowedError" | "SecurityError") => VfsError::PermissionDenied(detail),
        Some("InvalidModificationError") => VfsError::AlreadyExists(detail),
        _ => VfsError::Io(detail),
    }
}

pub fn display_error(error: VfsError) -> String {
    match error {
        VfsError::PermissionRequired(detail) => detail,
        error => error.to_string(),
    }
}

/// Call a `FileSystemHandle` permission method with an explicit readwrite
/// descriptor and await its promise. web-sys 0.3.x only exposes the no-arg
/// overloads (which default to "read"), but writes need readwrite, so the JS
/// methods are called directly.
async fn call_permission_method(
    handle: &FileSystemDirectoryHandle,
    method: &str,
) -> Result<JsValue, VfsError> {
    let desc = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        &desc,
        &JsValue::from_str("mode"),
        &JsValue::from_str("readwrite"),
    );
    let this: JsValue = handle.clone().unchecked_into();
    let fn_value = js_sys::Reflect::get(&this, &JsValue::from_str(method)).map_err(js_vfs_error)?;
    let fn_value: js_sys::Function = fn_value
        .dyn_into()
        .map_err(|_| VfsError::Io(format!("{method} is not a function")))?;
    let promise_value = fn_value.call1(&this, &desc).map_err(js_vfs_error)?;
    let promise: js_sys::Promise = promise_value
        .dyn_into()
        .map_err(|_| VfsError::Io("permission method did not return a promise".to_string()))?;
    JsFuture::from(promise).await.map_err(js_vfs_error)
}

pub const PERMISSION_NEEDED: &str = "folder access needs to be granted";

/// Ensure the handle has readwrite permission without prompting.
/// A freshly picked handle is already granted; one restored from IndexedDB
/// after a reload reverts to "prompt" and must be re-requested before use.
pub(crate) async fn ensure_permission(handle: &FileSystemDirectoryHandle) -> Result<(), VfsError> {
    let query = call_permission_method(handle, "queryPermission").await?;
    if query.as_string().as_deref() == Some("granted") {
        return Ok(());
    }
    Err(VfsError::PermissionRequired(PERMISSION_NEEDED.to_string()))
}

/// List a directory's entries, returning project-relative paths.
async fn list_typed(
    root: &FileSystemDirectoryHandle,
    dir: &str,
) -> Result<Vec<FileEntry>, VfsError> {
    let dir = openwebide_core::vfs::workspace_path(dir)?;
    ensure_permission(root).await?;
    let dir_handle = resolve_dir(root, &dir).await?;
    dir_entries(&dir_handle, &dir).await
}

/// Read a file's contents as text.
async fn read_typed(root: &FileSystemDirectoryHandle, path: &str) -> Result<String, VfsError> {
    let path = openwebide_core::vfs::workspace_path(path)?;
    ensure_permission(root).await?;
    let (parent, name) = split_path(&path);
    let parent_dir = resolve_dir(root, &parent).await?;
    let file_handle = file_handle(&parent_dir, &name).await?;
    file_handle_text(&file_handle).await
}

pub(crate) async fn read_lossy_typed(
    root: &FileSystemDirectoryHandle,
    path: &str,
) -> Result<String, VfsError> {
    read_bytes_typed(root, path)
        .await
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) async fn read_bytes_typed(
    root: &FileSystemDirectoryHandle,
    path: &str,
) -> Result<Vec<u8>, VfsError> {
    let path = openwebide_core::vfs::workspace_path(path)?;
    ensure_permission(root).await?;
    let (parent, name) = split_path(&path);
    let parent_dir = resolve_dir(root, &parent).await?;
    let file_handle = file_handle(&parent_dir, &name).await?;
    let file_value = JsFuture::from(file_handle.get_file())
        .await
        .map_err(js_vfs_error)?;
    let blob: Blob = file_value
        .dyn_into()
        .map_err(|_| VfsError::Io(format!("no such file: {path}")))?;
    check_read_size(blob.size())?;
    let buffer = JsFuture::from(blob.array_buffer())
        .await
        .map_err(js_vfs_error)?;
    let u8_array = js_sys::Uint8Array::new(&buffer);
    Ok(u8_array.to_vec())
}

/// Create an object URL (blob:...) for a local file to display media assets.
pub(crate) async fn read_blob_url_typed(
    root: &FileSystemDirectoryHandle,
    path: &str,
) -> Result<String, VfsError> {
    let path = openwebide_core::vfs::workspace_path(path)?;
    ensure_permission(root).await?;
    let (parent, name) = split_path(&path);
    let parent_dir = resolve_dir(root, &parent).await?;
    let file_handle = file_handle(&parent_dir, &name).await?;
    let file_value = JsFuture::from(file_handle.get_file())
        .await
        .map_err(js_vfs_error)?;
    let blob: Blob = file_value
        .dyn_into()
        .map_err(|_| VfsError::Io(format!("no such file: {path}")))?;
    check_read_size(blob.size())?;
    crate::workspace::preview_object_url(&blob, &path).map_err(js_vfs_error)
}

/// Write text to a file, creating parent directories as needed.
pub(crate) async fn write_bytes_typed(
    root: &FileSystemDirectoryHandle,
    path: &str,
    contents: &[u8],
) -> Result<(), VfsError> {
    let path = openwebide_core::vfs::workspace_path(path)?;
    ensure_permission(root).await?;
    let (parent, name) = split_path(&path);
    let parent = ensure_dir(root, &parent).await?;
    let handle = file_handle_create(&parent, &name).await?;
    let writable: FileSystemWritableFileStream = JsFuture::from(handle.create_writable())
        .await
        .map_err(js_vfs_error)?
        .unchecked_into();
    let parts = js_sys::Array::new();
    parts.push(&js_sys::Uint8Array::from(contents));
    let blob = Blob::new_with_u8_array_sequence(&parts).map_err(js_vfs_error)?;
    JsFuture::from(writable.write_with_blob(&blob).map_err(js_vfs_error)?)
        .await
        .map_err(js_vfs_error)?;
    JsFuture::from(writable.close())
        .await
        .map_err(js_vfs_error)?;
    Ok(())
}

async fn write_typed(
    root: &FileSystemDirectoryHandle,
    path: &str,
    content: &str,
) -> Result<(), VfsError> {
    let path = openwebide_core::vfs::workspace_path(path)?;
    ensure_permission(root).await?;
    let (parent, name) = split_path(&path);
    let parent_dir = ensure_dir(root, &parent).await?;
    let fh = file_handle_create(&parent_dir, &name).await?;
    write_file_handle(&fh, content).await
}

/// Create an empty file or a directory, creating parent directories as needed.
///
/// For files: exclusive — fails with an "already exists" error if the file
/// already exists, leaving it intact. For directories: idempotent.
async fn create_typed(
    root: &FileSystemDirectoryHandle,
    path: &str,
    is_dir: bool,
) -> Result<(), VfsError> {
    let path = openwebide_core::vfs::workspace_path(path)?;
    ensure_permission(root).await?;
    if is_dir {
        ensure_dir(root, &path).await?;
        return Ok(());
    }
    let (parent, name) = split_path(&path);
    let parent_dir = ensure_dir(root, &parent).await?;
    match file_handle(&parent_dir, &name).await {
        Ok(_) => return Err(VfsError::AlreadyExists(path.to_string())),
        Err(VfsError::NotFound(_)) => {}
        Err(error) => return Err(error),
    }
    let fh = file_handle_create(&parent_dir, &name).await?;
    write_file_handle(&fh, "").await
}

/// Delete an entry and its descendants, matching the shared VFS contract.
async fn delete_typed(root: &FileSystemDirectoryHandle, path: &str) -> Result<(), VfsError> {
    let path = openwebide_core::vfs::workspace_path(path)?;
    ensure_permission(root).await?;
    let (parent, name) = split_path(&path);
    let parent_dir = resolve_dir(root, &parent).await?;
    JsFuture::from(parent_dir.remove_entry_with_options(&name, &{
        let options = web_sys::FileSystemRemoveOptions::new();
        options.set_recursive(true);
        options
    }))
    .await
    .map_err(js_vfs_error)?;
    Ok(())
}

/// Full-text search: return the lines of every readable text file under `dir`
/// whose content contains `query` (case-insensitive).
pub async fn search_content(
    root: &FileSystemDirectoryHandle,
    query: &str,
    dir: &str,
    opts: SearchOptions,
) -> Result<Vec<SearchHit>, String> {
    openwebide_core::search::content(&BrowserFsaVfs::new(root.clone()), query, dir, opts)
        .await
        .map_err(display_error)
}

fn check_read_size(size: f64) -> Result<(), VfsError> {
    if size > openwebide_core::vfs::MAX_READ_BYTES as f64 {
        Err(VfsError::Io("file exceeds 10 MiB".into()))
    } else {
        Ok(())
    }
}

pub async fn request_access(handle: &FileSystemDirectoryHandle) -> Result<bool, String> {
    crate::idb::request_permission(handle).await
}

/// Keep the current capability object when a picker/IDB read returns another
/// wrapper for the same accessible entry. Actual root changes still replace it
/// and invalidate captured editor scopes.
pub async fn retain_folder_handle(
    previous: Option<&FileSystemDirectoryHandle>,
    granted: &FileSystemDirectoryHandle,
) -> Result<FileSystemDirectoryHandle, String> {
    if let Some(previous) = previous {
        let same = JsFuture::from(previous.is_same_entry(granted))
            .await
            .map_err(|e| js_error(&e))?;
        if same.as_bool() == Some(true)
            && call_permission_method(previous, "queryPermission")
                .await
                .map_err(display_error)?
                .as_string()
                .as_deref()
                == Some("granted")
        {
            return Ok(previous.clone());
        }
    }
    Ok(granted.clone())
}

/// Prompt the user to pick a directory to work on.
///
/// Returns `Ok(Some(handle))` on a pick, `Ok(None)` if the user dismissed the
/// picker, and `Err` on a real failure. The dismissal case matters: the
/// picker rejects with a `DOMException` whose `name` is `"AbortError"`, and a
/// rejected promise's reason is an *object* (so `JsValue::as_string` is
/// `None`) — treating it as a string would surface an empty error.
pub async fn pick_directory() -> Result<Option<FileSystemDirectoryHandle>, String> {
    pick_directory_from(None).await
}

pub async fn pick_directory_from(
    previous: Option<&FileSystemDirectoryHandle>,
) -> Result<Option<FileSystemDirectoryHandle>, String> {
    let window = web_sys::window().ok_or_else(|| "no window".to_string())?;
    let options = DirectoryPickerOptions::new();
    if let Some(handle) = previous {
        js_sys::Reflect::set(
            options.as_ref(),
            &JsValue::from_str("startIn"),
            handle.as_ref(),
        )
        .map_err(|error| js_error(&error))?;
    }
    options.set_mode(FileSystemPermissionMode::Readwrite);
    let promise = window
        .show_directory_picker_with_options(&options)
        .map_err(|e| {
            e.as_string()
                .unwrap_or_else(|| "directory picker failed".to_string())
        })?;
    match JsFuture::from(promise).await {
        Ok(handle) => Ok(Some(handle)),
        Err(e) => {
            let name = js_sys::Reflect::get(&e, &JsValue::from_str("name"))
                .ok()
                .and_then(|v| v.as_string());
            if name.as_deref() == Some("AbortError") {
                Ok(None)
            } else {
                Err(e
                    .as_string()
                    .unwrap_or_else(|| "directory picker failed".to_string()))
            }
        }
    }
}

pub async fn list(root: &FileSystemDirectoryHandle, dir: &str) -> Result<Vec<FileEntry>, String> {
    BrowserFsaVfs::new(root.clone())
        .list(dir)
        .await
        .map_err(display_error)
}
pub async fn read(root: &FileSystemDirectoryHandle, path: &str) -> Result<String, String> {
    BrowserFsaVfs::new(root.clone())
        .read(path)
        .await
        .map_err(display_error)
}
pub async fn write(
    root: &FileSystemDirectoryHandle,
    path: &str,
    content: &str,
) -> Result<(), String> {
    BrowserFsaVfs::new(root.clone())
        .write(path, content)
        .await
        .map_err(display_error)
}
pub async fn create(
    root: &FileSystemDirectoryHandle,
    path: &str,
    kind: openwebide_core::vfs::VfsEntryKind,
) -> Result<(), String> {
    BrowserFsaVfs::new(root.clone())
        .create(path, kind)
        .await
        .map_err(display_error)
}
pub async fn delete(root: &FileSystemDirectoryHandle, path: &str) -> Result<(), String> {
    BrowserFsaVfs::new(root.clone())
        .delete(path)
        .await
        .map_err(display_error)
}
pub async fn read_lossy(root: &FileSystemDirectoryHandle, path: &str) -> Result<String, String> {
    read_lossy_typed(root, path).await.map_err(display_error)
}
pub async fn read_blob_url(root: &FileSystemDirectoryHandle, path: &str) -> Result<String, String> {
    read_blob_url_typed(root, path).await.map_err(display_error)
}

// -- internals ---------------------------------------------------------------

/// Get a file handle by name, or fail if it does not exist.
async fn file_handle(
    dir: &FileSystemDirectoryHandle,
    name: &str,
) -> Result<FileSystemFileHandle, VfsError> {
    let value = JsFuture::from(dir.get_file_handle(name))
        .await
        .map_err(js_vfs_error)?;
    value
        .dyn_into::<FileSystemFileHandle>()
        .map_err(|_| VfsError::Io(format!("no such file: {name}")))
}

/// Get a file handle by name, creating it if it does not exist.
async fn file_handle_create(
    dir: &FileSystemDirectoryHandle,
    name: &str,
) -> Result<FileSystemFileHandle, VfsError> {
    let options = web_sys::FileSystemGetFileOptions::new();
    options.set_create(true);
    let value = JsFuture::from(dir.get_file_handle_with_options(name, &options))
        .await
        .map_err(js_vfs_error)?;
    value
        .dyn_into::<FileSystemFileHandle>()
        .map_err(|_| VfsError::Io(format!("cannot create file: {name}")))
}

/// Get an existing directory handle by name, or fail if it does not exist.
async fn dir_handle(
    dir: &FileSystemDirectoryHandle,
    name: &str,
) -> Result<FileSystemDirectoryHandle, VfsError> {
    let value = JsFuture::from(dir.get_directory_handle(name))
        .await
        .map_err(js_vfs_error)?;
    value
        .dyn_into::<FileSystemDirectoryHandle>()
        .map_err(|_| VfsError::Io(format!("no such directory: {name}")))
}

/// Get a directory handle by name, creating it if it does not exist.
async fn dir_handle_create(
    dir: &FileSystemDirectoryHandle,
    name: &str,
) -> Result<FileSystemDirectoryHandle, VfsError> {
    let options = FileSystemGetDirectoryOptions::new();
    options.set_create(true);
    let value = JsFuture::from(dir.get_directory_handle_with_options(name, &options))
        .await
        .map_err(js_vfs_error)?;
    value
        .dyn_into::<FileSystemDirectoryHandle>()
        .map_err(|_| VfsError::Io(format!("cannot create directory: {name}")))
}

async fn write_file_handle(
    file_handle: &FileSystemFileHandle,
    content: &str,
) -> Result<(), VfsError> {
    let writable_value = JsFuture::from(file_handle.create_writable())
        .await
        .map_err(js_vfs_error)?;
    let writable: FileSystemWritableFileStream = writable_value
        .dyn_into()
        .map_err(|_| VfsError::Io("failed to open writable stream".to_string()))?;
    let write_promise = writable.write_with_str(content).map_err(js_vfs_error)?;
    JsFuture::from(write_promise).await.map_err(js_vfs_error)?;
    // web-sys does not expose `FileSystemWritableFileStream::close`, so call it
    // through Reflect.
    let writable_js: JsValue = writable.unchecked_into();
    let close_fn_value =
        js_sys::Reflect::get(&writable_js, &JsValue::from_str("close")).map_err(js_vfs_error)?;
    let close_fn: js_sys::Function = close_fn_value
        .dyn_into()
        .map_err(|_| VfsError::Io("close is not a function".to_string()))?;
    let close_promise_value =
        js_sys::Reflect::apply(&close_fn, &writable_js, &js_sys::Array::new())
            .map_err(js_vfs_error)?;
    let close_promise: js_sys::Promise = close_promise_value.unchecked_into();
    JsFuture::from(close_promise).await.map_err(js_vfs_error)?;
    Ok(())
}

/// Walk `dir` (project-relative) from `root`, returning the directory handle.
async fn resolve_dir(
    root: &FileSystemDirectoryHandle,
    dir: &str,
) -> Result<FileSystemDirectoryHandle, VfsError> {
    let mut current = root.clone();
    let dir = openwebide_core::vfs::workspace_path(dir)?;
    for part in dir.split('/').filter(|p| !p.is_empty()) {
        current = dir_handle(&current, part).await?;
    }
    Ok(current)
}

/// Walk `path` from `root`, creating any missing directories.
async fn ensure_dir(
    root: &FileSystemDirectoryHandle,
    path: &str,
) -> Result<FileSystemDirectoryHandle, VfsError> {
    let mut current = root.clone();
    let path = openwebide_core::vfs::workspace_path(path)?;
    for part in path.split('/').filter(|p| !p.is_empty()) {
        current = dir_handle_create(&current, part).await?;
    }
    Ok(current)
}

/// List a directory's entries with their handles (for recursive search).
async fn dir_entries_with_handles(
    dir_handle: &FileSystemDirectoryHandle,
    prefix: &str,
) -> Result<Vec<(FileEntry, FileSystemHandle)>, VfsError> {
    let iter = dir_handle.values();
    let mut entries = Vec::new();
    loop {
        let next_promise = iter.next().map_err(js_vfs_error)?;
        let result = JsFuture::from(next_promise).await.map_err(js_vfs_error)?;
        let done =
            js_sys::Reflect::get(&result, &JsValue::from_str("done")).map_err(js_vfs_error)?;
        if done.as_bool().unwrap_or(true) {
            break;
        }
        let value =
            js_sys::Reflect::get(&result, &JsValue::from_str("value")).map_err(js_vfs_error)?;
        let handle: FileSystemHandle = value
            .dyn_into()
            .map_err(|_| VfsError::Io("not a file system handle".to_string()))?;
        let name = handle.name();
        let is_dir = handle.kind() == FileSystemHandleKind::Directory;
        if name == ".spin" {
            continue;
        }
        let size = if is_dir {
            0
        } else {
            let file: FileSystemFileHandle = handle.clone().unchecked_into();
            let metadata = match file_handle_file(&file).await {
                Ok(file) => file,
                // Directory iteration and metadata reads are not atomic. A file may
                // disappear between them (including browser writable swap files).
                Err(VfsError::NotFound(_)) => continue,
                Err(error) => return Err(error),
            };
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            // Browser File.size is a nonnegative integer within JS's exact range.
            {
                metadata.size() as u64
            }
        };
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if openwebide_core::vfs::private_workspace_path(&path) {
            continue;
        }
        entries.push((
            FileEntry {
                name,
                path,
                is_dir,
                size,
            },
            handle,
        ));
    }
    Ok(entries)
}

async fn dir_entries(
    dir_handle: &FileSystemDirectoryHandle,
    prefix: &str,
) -> Result<Vec<FileEntry>, VfsError> {
    dir_entries_with_handles(dir_handle, prefix)
        .await
        .map(|pairs| pairs.into_iter().map(|(entry, _)| entry).collect())
}

/// Cap on total hits a local content search returns, across the whole walk.
/// Fetch a file handle's `File`, so its size can be checked before reading.
async fn file_handle_file(file: &FileSystemFileHandle) -> Result<File, VfsError> {
    let file_value = JsFuture::from(file.get_file())
        .await
        .map_err(js_vfs_error)?;
    file_value
        .dyn_into()
        .map_err(|_| VfsError::Io("not a file".to_string()))
}

/// Read a `File`'s contents as text.
async fn file_to_text(file: &File) -> Result<String, VfsError> {
    check_read_size(file.size())?;
    let buffer = JsFuture::from(file.array_buffer())
        .await
        .map_err(js_vfs_error)?;
    let u8_array = js_sys::Uint8Array::new(&buffer);
    let vec = u8_array.to_vec();
    String::from_utf8(vec).map_err(|_| VfsError::Io("file is not valid UTF-8".to_string()))
}

/// Read a file handle's contents as text.
async fn file_handle_text(file: &FileSystemFileHandle) -> Result<String, VfsError> {
    let file = file_handle_file(file).await?;
    file_to_text(&file).await
}

/// Split `path` into its parent directory and file name.
fn split_path(path: &str) -> (String, String) {
    match path.rfind('/') {
        Some(i) => (path[..i].to_string(), path[i + 1..].to_string()),
        None => (String::new(), path.to_string()),
    }
}

/// A Vfs implementation backed by the browser File System Access API.
#[derive(Clone)]
pub struct BrowserFsaVfs {
    // Browser agents create, use and drop this on the spawn_local thread.
    // SendWrapper checks that invariant for handles and the futures below;
    // shared native Vfs contracts remain Send + Sync without unchecked claims.
    root: SendWrapper<FileSystemDirectoryHandle>,
}

impl BrowserFsaVfs {
    pub fn new(root: FileSystemDirectoryHandle) -> Self {
        Self {
            root: SendWrapper::new(root),
        }
    }
}

impl Vfs for BrowserFsaVfs {
    fn read_bytes<'a>(&'a self, path: &'a str) -> VfsFuture<'a, Vec<u8>> {
        let root = self.root.clone();
        Box::pin(SendWrapper::new(async move {
            read_bytes_typed(&root, path).await
        }))
    }
    fn write_bytes<'a>(&'a self, path: &'a str, contents: &'a [u8]) -> VfsFuture<'a, ()> {
        let root = self.root.clone();
        Box::pin(SendWrapper::new(async move {
            write_bytes_typed(&root, path, contents).await
        }))
    }
    fn replace_atomic<'a>(&'a self, path: &'a str, contents: &'a [u8]) -> VfsFuture<'a, ()> {
        let root = self.root.clone();
        Box::pin(SendWrapper::new(async move {
            let path = openwebide_core::vfs::workspace_path(path)?;
            ensure_permission(&root).await?;
            let (parent, name) = split_path(&path);
            let parent = resolve_dir(&root, &parent).await?;
            let handle = file_handle(&parent, &name).await?;
            let writable: FileSystemWritableFileStream = JsFuture::from(handle.create_writable())
                .await
                .map_err(js_vfs_error)?
                .unchecked_into();
            let parts = js_sys::Array::new();
            parts.push(&js_sys::Uint8Array::from(contents));
            let blob = Blob::new_with_u8_array_sequence(&parts).map_err(js_vfs_error)?;
            JsFuture::from(writable.write_with_blob(&blob).map_err(js_vfs_error)?)
                .await
                .map_err(js_vfs_error)?;
            // File System Access publishes its swap file only when close succeeds.
            JsFuture::from(writable.close())
                .await
                .map_err(js_vfs_error)?;
            Ok(())
        }))
    }
    fn remove_entry<'a>(
        &'a self,
        path: &'a str,
        kind: openwebide_core::vfs::VfsEntryKind,
    ) -> VfsFuture<'a, ()> {
        let root = self.root.clone();
        Box::pin(SendWrapper::new(async move {
            let path = openwebide_core::vfs::workspace_path(path)?;
            ensure_permission(&root).await?;
            let (parent, name) = split_path(&path);
            let parent = resolve_dir(&root, &parent).await?;
            if kind.is_dir() {
                dir_handle(&parent, &name).await?;
            } else {
                file_handle(&parent, &name).await?;
            }
            // The default is nonrecursive; unexpected children survive a retry.
            JsFuture::from(parent.remove_entry(&name))
                .await
                .map_err(js_vfs_error)?;
            Ok(())
        }))
    }
    fn copy<'a>(&'a self, from: &'a str, to: &'a str) -> VfsFuture<'a, ()> {
        let root = self.root.clone();
        let from = from.to_string();
        let to = to.to_string();
        Box::pin(SendWrapper::new(async move {
            ensure_permission(&root).await?;
            let from = openwebide_core::vfs::workspace_path(&from)?;
            let to = openwebide_core::vfs::workspace_path(&to)?;
            let (from_parent, from_name) = split_path(&from);
            let from_dir = resolve_dir(&root, &from_parent).await?;
            let from_handle = file_handle(&from_dir, &from_name).await?;

            let file_value = JsFuture::from(from_handle.get_file())
                .await
                .map_err(js_vfs_error)?;
            let file: web_sys::File = file_value
                .dyn_into()
                .map_err(|_| VfsError::Io(format!("no such file: {from}")))?;

            let (to_parent, to_name) = split_path(&to);
            let to_dir = ensure_dir(&root, &to_parent).await?;
            let to_handle = file_handle_create(&to_dir, &to_name).await?;

            let writable_value = JsFuture::from(to_handle.create_writable())
                .await
                .map_err(js_vfs_error)?;
            let writable: FileSystemWritableFileStream = writable_value
                .dyn_into()
                .map_err(|_| VfsError::Io("failed to open writable stream".to_string()))?;

            let write_promise = writable.write_with_blob(&file).map_err(js_vfs_error)?;
            JsFuture::from(write_promise).await.map_err(js_vfs_error)?;

            let writable_js: JsValue = writable.unchecked_into();
            let close_fn_value = js_sys::Reflect::get(&writable_js, &JsValue::from_str("close"))
                .map_err(js_vfs_error)?;
            let close_fn: js_sys::Function = close_fn_value
                .dyn_into()
                .map_err(|_| VfsError::Io("close is not a function".to_string()))?;
            let close_promise_value =
                js_sys::Reflect::apply(&close_fn, &writable_js, &js_sys::Array::new())
                    .map_err(js_vfs_error)?;
            let close_promise: js_sys::Promise = close_promise_value.unchecked_into();
            JsFuture::from(close_promise).await.map_err(js_vfs_error)?;

            Ok(())
        }))
    }

    fn read<'a>(&'a self, path: &'a str) -> VfsFuture<'a, String> {
        let root = self.root.clone();
        let path = path.to_string();
        Box::pin(SendWrapper::new(
            async move { read_typed(&root, &path).await },
        ))
    }

    fn write<'a>(&'a self, path: &'a str, content: &'a str) -> VfsFuture<'a, ()> {
        let root = self.root.clone();
        let path = path.to_string();
        let content = content.to_string();
        Box::pin(SendWrapper::new(async move {
            write_typed(&root, &path, &content).await
        }))
    }

    fn list<'a>(&'a self, dir: &'a str) -> VfsFuture<'a, Vec<FileEntry>> {
        let root = self.root.clone();
        let dir = dir.to_string();
        Box::pin(SendWrapper::new(
            async move { list_typed(&root, &dir).await },
        ))
    }

    fn create<'a>(
        &'a self,
        path: &'a str,
        kind: openwebide_core::vfs::VfsEntryKind,
    ) -> VfsFuture<'a, ()> {
        let root = self.root.clone();
        let path = path.to_string();
        Box::pin(SendWrapper::new(async move {
            create_typed(&root, &path, kind.is_dir()).await
        }))
    }

    fn delete<'a>(&'a self, path: &'a str) -> VfsFuture<'a, ()> {
        let root = self.root.clone();
        let path = path.to_string();
        Box::pin(SendWrapper::new(
            async move { delete_typed(&root, &path).await },
        ))
    }
}
