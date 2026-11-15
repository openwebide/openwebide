//! Ordinary native workspace writers share the resource/command folder gate.
//! Transaction interpreters reuse their real outer lease.
use super::{NativeFsVfs, mutation::NativeMutations};
use crate::folder_coordination::{Coordinator, Folder, Lease, Worker};
use openwebide_core::{FileEntry, Vfs, VfsError, VfsFuture, vfs::VfsEntryKind};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
enum Access {
    Queue(Coordinator),
    Held(Arc<Lease>),
}

#[derive(Clone)]
pub(crate) struct WorkspaceFiles {
    files: NativeFsVfs,
    folder: Result<Folder, String>,
    access: Access,
}
struct Write {
    folder: Folder,
    lease: Arc<Lease>,
    _worker: Option<Worker>,
}
impl WorkspaceFiles {
    /// Check the captured physical object, not a pathname resolved before capture.
    pub(crate) fn in_workspace(
        root: PathBuf,
        workspace: &Path,
        coordinator: Coordinator,
    ) -> Result<Self, VfsError> {
        let files = Self::new(root, coordinator);
        let folder = files.current()?;
        super::workspace_root(&folder.path)?;
        if !folder.path.starts_with(workspace) {
            return Err(VfsError::PathEscape(
                "Project folder escapes the workspace root".into(),
            ));
        }
        Ok(files)
    }
    pub(crate) fn new(root: PathBuf, coordinator: Coordinator) -> Self {
        let folder = Folder::capture(&root);
        let files = NativeFsVfs {
            root: folder.as_ref().map_or(root, |folder| folder.path.clone()),
        };
        Self {
            files,
            folder,
            access: Access::Queue(coordinator),
        }
    }
    /// Reuse the caller's real capability; never recursively acquire its gate.
    pub(crate) fn with_lease(folder: Folder, lease: Arc<Lease>) -> Result<Self, VfsError> {
        if !lease.covers(&folder) {
            return Err(VfsError::Io(
                "File access requires the original folder lease".into(),
            ));
        }
        Ok(Self {
            files: NativeFsVfs {
                root: folder.path.clone(),
            },
            folder: Ok(folder),
            access: Access::Held(lease),
        })
    }
    fn current(&self) -> Result<&Folder, VfsError> {
        let folder = self
            .folder
            .as_ref()
            .map_err(|error| VfsError::Io(error.clone()))?;
        if !folder.current() {
            return Err(VfsError::Io(
                "The original native workspace folder changed".into(),
            ));
        }
        Ok(folder)
    }
    fn observe<'a, T: Send + 'a>(&'a self, work: VfsFuture<'a, T>) -> VfsFuture<'a, T> {
        Box::pin(async move {
            self.current()?;
            let result = work.await;
            self.current()?;
            result
        })
    }
    async fn acquire(&self) -> Result<Write, VfsError> {
        let folder = self.current()?.clone();
        let (lease, worker) = match &self.access {
            Access::Held(lease) => {
                if !lease.covers(&folder) {
                    return Err(VfsError::Io("The original folder lease changed".into()));
                }
                (lease.clone(), None)
            }
            Access::Queue(coordinator) => {
                let worker = coordinator.worker().map_err(VfsError::Conflict)?;
                let lease = coordinator
                    .acquire_ordinary(folder.clone())
                    .await
                    .map_err(VfsError::Conflict)?;
                (Arc::new(lease), Some(worker))
            }
        };
        Ok(Write {
            folder,
            lease,
            _worker: worker,
        })
    }
    fn mutate<'a, F, Work>(&'a self, prepare: F) -> VfsFuture<'a, ()>
    where
        F: FnOnce(NativeMutations) -> Work + Send + 'a,
        Work: FnOnce() -> Result<(), VfsError> + Send + 'static,
    {
        Box::pin(async move {
            // Borrow inputs while queued. Allocate the owned I/O payload only
            // after admission, before spawning and without another await.
            let write = self.acquire().await?;
            let work = prepare(NativeMutations::original(&write.folder)?);
            let worker = tokio::task::spawn_blocking(move || {
                let Write {
                    folder,
                    lease,
                    _worker,
                } = write;
                if !lease.permits_files(&folder) {
                    return Err(VfsError::Io(
                        "The original native workspace folder changed".into(),
                    ));
                }
                // The actual blocking operation owns the lease. Losing the
                // observer or runtime cannot drop it while I/O continues.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
                match result {
                    Ok(result) => {
                        if !lease.permits_files(&folder) {
                            return Err(VfsError::Io(
                                "The native workspace folder changed during file access".into(),
                            ));
                        }
                        result
                    }
                    Err(_) => {
                        lease.mark_uncertain();
                        Err(VfsError::Io(
                            "Native file execution failed; workspace cleanup is unconfirmed".into(),
                        ))
                    }
                }
            });
            worker
                .await
                .map_err(|_| VfsError::Io("The native file worker stopped unexpectedly".into()))?
        })
    }
}
impl Vfs for WorkspaceFiles {
    fn read<'a>(&'a self, path: &'a str) -> VfsFuture<'a, String> {
        self.observe(self.files.read(path))
    }
    fn read_bytes<'a>(&'a self, path: &'a str) -> VfsFuture<'a, Vec<u8>> {
        self.observe(self.files.read_bytes(path))
    }
    fn list<'a>(&'a self, path: &'a str) -> VfsFuture<'a, Vec<FileEntry>> {
        self.observe(self.files.list(path))
    }
    fn canonicalize<'a>(&'a self, path: &'a str) -> VfsFuture<'a, String> {
        self.observe(self.files.canonicalize(path))
    }
    fn write<'a>(&'a self, path: &'a str, contents: &'a str) -> VfsFuture<'a, ()> {
        self.write_bytes(path, contents.as_bytes())
    }
    fn write_bytes<'a>(&'a self, path: &'a str, contents: &'a [u8]) -> VfsFuture<'a, ()> {
        self.mutate(move |files| {
            let path = path.to_string();
            let contents = contents.to_vec();
            move || files.write_bytes_sync(&path, &contents)
        })
    }
    fn replace_atomic<'a>(&'a self, path: &'a str, contents: &'a [u8]) -> VfsFuture<'a, ()> {
        self.mutate(move |files| {
            let path = path.to_string();
            let contents = contents.to_vec();
            move || files.replace_atomic_sync(&path, &contents)
        })
    }
    fn create<'a>(&'a self, path: &'a str, kind: VfsEntryKind) -> VfsFuture<'a, ()> {
        self.mutate(move |files| {
            let path = path.to_string();
            move || files.create_sync(&path, kind)
        })
    }
    fn delete<'a>(&'a self, path: &'a str) -> VfsFuture<'a, ()> {
        self.mutate(move |files| {
            let path = path.to_string();
            move || files.delete_sync(&path)
        })
    }
    fn remove_entry<'a>(&'a self, path: &'a str, kind: VfsEntryKind) -> VfsFuture<'a, ()> {
        self.mutate(move |files| {
            let path = path.to_string();
            move || files.remove_entry_sync(&path, kind)
        })
    }
    fn copy<'a>(&'a self, from: &'a str, to: &'a str) -> VfsFuture<'a, ()> {
        self.mutate(move |files| {
            let (from, to) = (from.to_string(), to.to_string());
            move || files.copy_sync(&from, &to)
        })
    }
    fn move_atomic<'a>(
        &'a self,
        from: &'a str,
        to: &'a str,
        kind: VfsEntryKind,
    ) -> VfsFuture<'a, ()> {
        self.mutate(move |files| {
            let (from, to) = (from.to_string(), to.to_string());
            move || files.move_atomic_sync(&from, &to, kind)
        })
    }
}

#[cfg(test)]
mod tests;
