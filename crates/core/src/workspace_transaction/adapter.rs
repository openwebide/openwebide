//! A filesystem adapter; all transaction policy remains in the shared engine.
use super::{TransactionFiles, WorkspaceEntries};
use crate::{FileEntry, Vfs, vfs::VfsEntryKind};

pub struct VfsTransactionFiles<'a> {
    pub vfs: &'a dyn Vfs,
    pub identity: &'a str,
}
impl WorkspaceEntries for VfsTransactionFiles<'_> {
    async fn canonicalize(&self, path: &str) -> Result<String, String> {
        self.vfs.canonicalize(path).await.map_err(|e| e.to_string())
    }
    async fn list(&self, path: &str) -> Result<Vec<FileEntry>, String> {
        self.vfs.list(path).await.map_err(|e| e.to_string())
    }
    async fn read_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        self.vfs.read_bytes(path).await.map_err(|e| e.to_string())
    }
    async fn create(&self, path: &str, kind: VfsEntryKind) -> Result<(), String> {
        self.vfs.create(path, kind).await.map_err(|e| e.to_string())
    }
    async fn write_bytes(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        self.vfs
            .write_bytes(path, bytes)
            .await
            .map_err(|e| e.to_string())
    }
    async fn delete(&self, path: &str) -> Result<(), String> {
        self.vfs.delete(path).await.map_err(|e| e.to_string())
    }
}
impl TransactionFiles for VfsTransactionFiles<'_> {
    fn root_identity(&self) -> &str {
        self.identity
    }
    async fn replace_atomic(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        self.vfs
            .replace_atomic(path, bytes)
            .await
            .map_err(|e| e.to_string())
    }
    async fn remove_entry(&self, path: &str, kind: VfsEntryKind) -> Result<(), String> {
        self.vfs
            .remove_entry(path, kind)
            .await
            .map_err(|e| e.to_string())
    }
}
