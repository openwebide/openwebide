//! Shared workspace facade; adapters supply filesystem/HTTP primitives.
use crate::{backend::Api, local_fs, state::projects::ProjectsState};
use leptos::prelude::{WithUntracked, WithValue};
use openwebide_core::{
    FileEntry, SearchHit, Vfs, VfsError, WorkspaceMode,
    vfs::{SearchOptions, VfsEntryKind, workspace_path},
};
use web_sys::FileSystemDirectoryHandle;

#[derive(Debug)]
pub enum WorkspaceError {
    FileSystem(VfsError),
    Transport(String),
}
impl WorkspaceError {
    pub fn needs_folder_access(&self) -> bool {
        matches!(self, Self::FileSystem(VfsError::PermissionRequired(_)))
    }
}
impl From<VfsError> for WorkspaceError {
    fn from(error: VfsError) -> Self {
        Self::FileSystem(error)
    }
}
impl From<String> for WorkspaceError {
    fn from(error: String) -> Self {
        Self::Transport(error)
    }
}
impl From<WorkspaceError> for String {
    fn from(error: WorkspaceError) -> Self {
        error.to_string()
    }
}
impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FileSystem(error) => error.fmt(f),
            Self::Transport(error) => f.write_str(error),
        }
    }
}
impl std::error::Error for WorkspaceError {}

pub enum Workspace {
    Remote { api: Api, project_id: i64 },
    Local { handle: FileSystemDirectoryHandle },
}
/// Normalize preview metadata once for both browser filesystem and HTTP blobs.
pub(crate) fn preview_object_url(
    blob: &web_sys::Blob,
    path: &str,
) -> Result<String, wasm_bindgen::JsValue> {
    let blob = if openwebide_core::file_type::extension(path).as_deref() == Some("pdf") {
        blob.slice_with_f64_and_f64_and_content_type(0.0, blob.size(), "application/pdf")?
    } else {
        blob.clone()
    };
    web_sys::Url::create_object_url_with_blob(&blob)
}

impl Workspace {
    /// Local bridge discovery updates metadata, not the browser folder handle.
    /// Only remote paths select a filesystem root; local roots use handle identity.
    pub fn root_identity(
        project: &openwebide_core::Project,
    ) -> (i64, WorkspaceMode, Option<String>) {
        (
            project.id,
            project.mode,
            match project.mode {
                WorkspaceMode::Remote => project.path.clone(),
                WorkspaceMode::Local => None,
            },
        )
    }
    pub fn for_project(api: Api, projects: ProjectsState, id: i64) -> Option<Self> {
        match projects.project(id)?.mode {
            WorkspaceMode::Remote => Some(Self::Remote {
                api,
                project_id: id,
            }),
            WorkspaceMode::Local => projects
                .local_handles
                .with_untracked(|handles| handles.get(&id).cloned())
                .map(|handle| Self::Local { handle }),
        }
    }
    pub async fn canonical_path(&self, path: &str) -> Result<String, WorkspaceError> {
        let path = workspace_path(path)?;
        match self {
            Self::Remote { api, project_id } => api
                .with_value(Clone::clone)
                .canonical_file_path(*project_id, &path)
                .await
                .map_err(Into::into),
            Self::Local { handle } => local_fs::BrowserFsaVfs::new(handle.clone())
                .canonicalize(&path)
                .await
                .map_err(Into::into),
        }
    }
    pub async fn list(&self, dir: &str) -> Result<Vec<FileEntry>, WorkspaceError> {
        let dir = workspace_path(dir)?;
        let mut entries = match self {
            Self::Remote { api, project_id } => api
                .with_value(Clone::clone)
                .list_files(*project_id, &dir)
                .await
                .map_err(WorkspaceError::from),
            Self::Local { handle } => local_fs::BrowserFsaVfs::new(handle.clone())
                .list(&dir)
                .await
                .map_err(WorkspaceError::from),
        }?;
        openwebide_core::vfs::sort_file_entries(&mut entries);
        Ok(entries)
    }
    /// Filename discovery has one bounded traversal policy above both list adapters.
    pub async fn discover_files(
        &self,
        current: impl Fn() -> bool,
    ) -> Result<crate::omnibar::FileDiscovery, WorkspaceError> {
        let mut walk = crate::omnibar::FileWalk::default();
        while let Some(directory) = walk.next_directory() {
            if !current() {
                break;
            }
            let entries = self.list(&directory).await?;
            if !current() {
                break;
            }
            walk.add(&directory, entries)?;
        }
        Ok(walk.finish())
    }

    /// Read an existing project-relative file without treating a transport or
    /// permission failure as absence. Shared enumeration handles missing parents.
    pub async fn read_optional_bytes(&self, path: &str) -> Result<Option<Vec<u8>>, WorkspaceError> {
        let path = workspace_path(path)?;
        // Enumerate each existing ancestor so a removed directory means absence
        // without classifying transport errors by their text.
        let (parent, _) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
        let mut dir = String::new();
        for part in parent.split('/').filter(|part| !part.is_empty()) {
            let entries = self.list(&dir).await?;
            let next = if dir.is_empty() {
                part.to_string()
            } else {
                format!("{dir}/{part}")
            };
            if !entries
                .iter()
                .any(|entry| entry.path == next && entry.is_dir)
            {
                return Ok(None);
            }
            dir = next;
        }
        let entries = self.list(parent).await?;
        if !entries.iter().any(|entry| entry.path == path) {
            return Ok(None);
        }
        Workspace::read_bytes(self, &path).await.map(Some)
    }

    pub async fn read(&self, path: &str) -> Result<String, WorkspaceError> {
        let path = workspace_path(path)?;
        match self {
            Self::Remote { api, project_id } => api
                .with_value(Clone::clone)
                .read_file(*project_id, &path)
                .await
                .map_err(Into::into),
            Self::Local { handle } => local_fs::BrowserFsaVfs::new(handle.clone())
                .read(&path)
                .await
                .map_err(Into::into),
        }
    }
    pub async fn read_bytes(&self, path: &str) -> Result<Vec<u8>, WorkspaceError> {
        let path = workspace_path(path)?;
        match self {
            Self::Remote { api, project_id } => api
                .with_value(Clone::clone)
                .read_file_bytes(*project_id, &path)
                .await
                .map_err(Into::into),
            Self::Local { handle } => local_fs::read_bytes_typed(handle, &path)
                .await
                .map_err(Into::into),
        }
    }
    /// Limit admission before either adapter loads contents. UTF-8 and binary
    /// rejection are shared above the HTTP and browser filesystem primitives.
    pub async fn read_text_with_limit(
        &self,
        path: &str,
        max: usize,
    ) -> Result<String, WorkspaceError> {
        let path = workspace_path(path)?;
        let max = (max as u64).min(openwebide_core::vfs::MAX_READ_BYTES);
        let bytes = match self {
            Self::Remote { api, project_id } => api
                .with_value(Clone::clone)
                .read_file_bytes_with_limit(*project_id, &path, max)
                .await
                .map_err(WorkspaceError::from),
            Self::Local { handle } => local_fs::read_bytes_with_limit_typed(handle, &path, max)
                .await
                .map_err(WorkspaceError::from),
        }?;
        if bytes.len() as u64 > max || bytes.contains(&0) {
            return Err(VfsError::Io(
                "File exceeds its read budget or contains binary data".into(),
            )
            .into());
        }
        String::from_utf8(bytes).map_err(|error| VfsError::Io(error.to_string()).into())
    }
    pub async fn read_lossy(&self, path: &str) -> Result<String, WorkspaceError> {
        let path = workspace_path(path)?;
        match self {
            Self::Remote { api, project_id } => api
                .with_value(Clone::clone)
                .read_file_lossy(*project_id, &path)
                .await
                .map_err(Into::into),
            Self::Local { handle } => local_fs::read_lossy_typed(handle, &path)
                .await
                .map_err(Into::into),
        }
    }
    pub async fn read_blob_url(&self, path: &str) -> Result<String, WorkspaceError> {
        let path = workspace_path(path)?;
        match self {
            Self::Remote { api, project_id } => api
                .with_value(Clone::clone)
                .read_file_object_url(*project_id, &path)
                .await
                .map_err(Into::into),
            Self::Local { handle } => local_fs::read_blob_url_typed(handle, &path)
                .await
                .map_err(Into::into),
        }
    }
    pub async fn write(&self, path: &str, content: &str) -> Result<(), WorkspaceError> {
        let path = workspace_path(path)?;
        match self {
            Self::Remote { api, project_id } => api
                .with_value(Clone::clone)
                .write_file(*project_id, &path, content)
                .await
                .map_err(Into::into),
            Self::Local { handle } => local_fs::BrowserFsaVfs::new(handle.clone())
                .write(&path, content)
                .await
                .map_err(Into::into),
        }
    }
    pub async fn write_bytes(&self, path: &str, bytes: &[u8]) -> Result<(), WorkspaceError> {
        let path = workspace_path(path)?;
        match self {
            Self::Remote { api, project_id } => api
                .with_value(Clone::clone)
                .write_file_bytes(*project_id, &path, bytes)
                .await
                .map_err(Into::into),
            Self::Local { handle } => local_fs::BrowserFsaVfs::new(handle.clone())
                .write_bytes(&path, bytes)
                .await
                .map_err(Into::into),
        }
    }
    pub async fn copy(&self, from: &str, to: &str) -> Result<(), WorkspaceError> {
        let (from, to) = (workspace_path(from)?, workspace_path(to)?);
        match self {
            Self::Remote { api, project_id } => api
                .with_value(Clone::clone)
                .copy_file(*project_id, &from, &to)
                .await
                .map_err(Into::into),
            Self::Local { handle } => local_fs::BrowserFsaVfs::new(handle.clone())
                .copy(&from, &to)
                .await
                .map_err(Into::into),
        }
    }
    pub async fn move_entry(
        &self,
        from: &str,
        to: &str,
        current: impl Fn() -> bool,
    ) -> Result<(), String> {
        openwebide_core::workspace_entries::move_entry(self, from, to, current).await
    }
    pub async fn ignore(
        &self,
        path: &str,
        is_dir: bool,
        current: impl Fn() -> bool,
    ) -> Result<(), String> {
        openwebide_core::workspace_entries::ignore_entry(self, path, is_dir, current).await
    }
    pub async fn create(&self, path: &str, kind: VfsEntryKind) -> Result<(), WorkspaceError> {
        let path = workspace_path(path)?;
        match self {
            Self::Remote { api, project_id } => api
                .with_value(Clone::clone)
                .create_file(*project_id, &path, kind.is_dir())
                .await
                .map_err(Into::into),
            Self::Local { handle } => local_fs::BrowserFsaVfs::new(handle.clone())
                .create(&path, kind)
                .await
                .map_err(Into::into),
        }
    }
    pub async fn search_content(
        &self,
        query: &str,
        dir: &str,
        opts: SearchOptions,
    ) -> Result<Vec<SearchHit>, WorkspaceError> {
        let dir = workspace_path(dir)?;
        match self {
            Self::Remote { api, project_id } => api
                .with_value(Clone::clone)
                .search_content(*project_id, query, &dir, opts)
                .await
                .map_err(Into::into),
            Self::Local { handle } => local_fs::BrowserFsaVfs::new(handle.clone())
                .search_content(query, &dir, opts)
                .await
                .map_err(Into::into),
        }
    }
    pub async fn delete(&self, path: &str) -> Result<(), WorkspaceError> {
        let path = workspace_path(path)?;
        match self {
            Self::Remote { api, project_id } => api
                .with_value(Clone::clone)
                .delete_file(*project_id, &path)
                .await
                .map_err(Into::into),
            Self::Local { handle } => local_fs::BrowserFsaVfs::new(handle.clone())
                .delete(&path)
                .await
                .map_err(Into::into),
        }
    }
}

impl openwebide_core::rewind::RewindFiles for Workspace {
    async fn validate(&self, path: &str) -> Result<(), String> {
        let canonical = self.canonical_path(path).await.map_err(String::from)?;
        if canonical
            .split('/')
            .any(|part| part.eq_ignore_ascii_case(".git") || part == ".spin")
        {
            return Err("Cannot rewind Git or runtime internals".into());
        }
        Ok(())
    }

    async fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        self.read_optional_bytes(path)
            .await
            .map_err(|error| error.to_string())
    }
    async fn write(&self, path: &str, content: &str) -> Result<(), String> {
        Workspace::write(self, path, content)
            .await
            .map_err(|error| error.to_string())
    }
    async fn write_bytes(&self, path: &str, content: &[u8]) -> Result<(), String> {
        Workspace::write_bytes(self, path, content)
            .await
            .map_err(|e| e.to_string())
    }
    async fn copy(&self, from: &str, to: &str) -> Result<(), String> {
        Workspace::copy(self, from, to)
            .await
            .map_err(|error| error.to_string())
    }
    async fn delete(&self, path: &str) -> Result<(), String> {
        Workspace::delete(self, path)
            .await
            .map_err(|error| error.to_string())
    }
}

impl openwebide_core::workspace_entries::WorkspaceEntries for Workspace {
    async fn canonicalize(&self, path: &str) -> Result<String, String> {
        Workspace::canonical_path(self, path)
            .await
            .map_err(Into::into)
    }
    async fn list(&self, path: &str) -> Result<Vec<FileEntry>, String> {
        Workspace::list(self, path).await.map_err(Into::into)
    }
    async fn read_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        Workspace::read_bytes(self, path).await.map_err(Into::into)
    }
    async fn create(&self, path: &str, kind: VfsEntryKind) -> Result<(), String> {
        Workspace::create(self, path, kind)
            .await
            .map_err(Into::into)
    }
    async fn write_bytes(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        Workspace::write_bytes(self, path, bytes)
            .await
            .map_err(Into::into)
    }
    async fn delete(&self, path: &str) -> Result<(), String> {
        Workspace::delete(self, path).await.map_err(Into::into)
    }
}
