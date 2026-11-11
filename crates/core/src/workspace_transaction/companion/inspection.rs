//! Read-only native observations for server-owned projects. Both inspection and
//! proven browser capabilities feed the same workspace planner.
use super::{FileAction, FileValue, fingerprint};
use crate::{FileEntry, vfs::VfsEntryKind, workspace_entries::WorkspaceEntries};
use serde::{Deserialize, Serialize};
use std::future::Future;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderInspection {
    /// Physical root returned by the authenticated owning-project root endpoint.
    pub root: String,
    pub action: FileAction,
}
impl FolderInspection {
    pub fn validate(&self) -> Result<(), String> {
        if !fingerprint(&self.root)
            || !matches!(
                self.action,
                FileAction::Canonicalize { .. } | FileAction::List { .. } | FileAction::Read { .. }
            )
        {
            return Err("Invalid read-only workspace inspection".into());
        }
        self.action.validate()
    }
}

/// Only the backend service may choose this filesystem address. Browser callers
/// submit `FolderInspection`; the backend supplies authenticated ownership/path.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeFolderInspection {
    pub user: i64,
    pub project: i64,
    pub path: String,
    pub inspection: FolderInspection,
}
impl NativeFolderInspection {
    pub fn validate(&self) -> Result<(), String> {
        if self.user <= 0
            || self.project <= 0
            || self.path.len() > 4096
            || self.path.chars().any(char::is_control)
        {
            return Err("Invalid native inspection address".into());
        }
        self.inspection.validate()
    }
}

pub trait FolderInspector {
    fn inspect(
        &self,
        request: &FolderInspection,
    ) -> impl Future<Output = Result<FileValue, String>>;
}

pub struct InspectedFolder<T> {
    transport: T,
    root: String,
}
impl<T: FolderInspector> InspectedFolder<T> {
    pub fn new(transport: T, root: String) -> Result<Self, String> {
        if !fingerprint(&root) {
            return Err("Invalid native inspection root".into());
        }
        Ok(Self { transport, root })
    }
    pub fn root_identity(&self) -> &str {
        &self.root
    }
    async fn call<V>(
        &self,
        action: FileAction,
        decode: impl FnOnce(FileValue) -> Result<V, String>,
    ) -> Result<V, String> {
        let request = FolderInspection {
            root: self.root.clone(),
            action,
        };
        request.validate()?;
        decode(self.transport.inspect(&request).await?)
    }
}
impl<T: FolderInspector> WorkspaceEntries for InspectedFolder<T> {
    async fn canonicalize(&self, path: &str) -> Result<String, String> {
        self.call(
            FileAction::Canonicalize { path: path.into() },
            super::files::path,
        )
        .await
    }
    async fn list(&self, path: &str) -> Result<Vec<FileEntry>, String> {
        self.call(
            FileAction::List { path: path.into() },
            super::files::entries,
        )
        .await
    }
    async fn read_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        self.call(FileAction::Read { path: path.into() }, super::files::bytes)
            .await
    }
    async fn create(&self, _path: &str, _kind: VfsEntryKind) -> Result<(), String> {
        Err("Workspace inspection is read-only".into())
    }
    async fn write_bytes(&self, _path: &str, _bytes: &[u8]) -> Result<(), String> {
        Err("Workspace inspection is read-only".into())
    }
    async fn delete(&self, _path: &str) -> Result<(), String> {
        Err("Workspace inspection is read-only".into())
    }
}
