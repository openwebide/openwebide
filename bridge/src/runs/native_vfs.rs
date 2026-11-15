use std::path::PathBuf;

use openwebide_core::vfs::VfsFuture;
use openwebide_core::{FileEntry, Vfs, VfsError};
use tokio::fs;
use tokio::io::AsyncReadExt;

use crate::paths::resolve_file_in_root;

mod mutation;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod resources;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod rooted;
mod transfer;
pub(crate) mod workspace;

#[derive(Clone, Debug)]
pub struct NativeFsVfs {
    pub root: PathBuf,
}

fn io_error(error: std::io::Error, path: &str) -> VfsError {
    match error.kind() {
        std::io::ErrorKind::NotFound => VfsError::NotFound(path.into()),
        std::io::ErrorKind::AlreadyExists => VfsError::AlreadyExists(path.into()),
        std::io::ErrorKind::PermissionDenied => VfsError::PermissionDenied(path.into()),
        _ => VfsError::Io(error.to_string()),
    }
}

fn workspace_root(path: &std::path::Path) -> Result<(), VfsError> {
    if openwebide_core::vfs::private_workspace_path(&path.to_string_lossy().replace('\\', "/")) {
        return Err(VfsError::PathEscape("Private runtime folder".into()));
    }
    Ok(())
}

impl NativeFsVfs {
    fn canonical_relative(&self, path: &str) -> Result<String, VfsError> {
        let root = std::fs::canonicalize(&self.root).map_err(|e| io_error(e, path))?;
        workspace_root(&root)?;
        let lexical = openwebide_core::vfs::workspace_path(path)?;
        let mut full = if lexical.is_empty() {
            root.clone()
        } else {
            resolve_file_in_root(&root, &lexical).map_err(VfsError::PathEscape)?
        };
        let mut missing = Vec::new();
        let mut symlinks = 0;
        let mut resolved = loop {
            match std::fs::canonicalize(&full) {
                Ok(resolved) => break resolved,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if std::fs::symlink_metadata(&full)
                        .is_ok_and(|meta| meta.file_type().is_symlink())
                    {
                        symlinks += 1;
                        if symlinks > 32 {
                            return Err(VfsError::Io("too many symbolic links".into()));
                        }
                        let target = std::fs::read_link(&full).map_err(|e| io_error(e, path))?;
                        full.pop();
                        full.push(target);
                        continue;
                    }
                    let name = full.file_name().ok_or_else(|| io_error(error, path))?;
                    missing.push(name.to_os_string());
                    full.pop();
                }
                Err(error) => return Err(io_error(error, path)),
            }
        };
        for name in missing.into_iter().rev() {
            resolved.push(name);
        }
        let rel = resolved
            .strip_prefix(root)
            .map_err(|_| VfsError::PathEscape(path.into()))?;
        openwebide_core::vfs::workspace_path(&rel.to_string_lossy().replace('\\', "/"))
    }
    fn resolve(&self, rel: &str) -> Result<PathBuf, VfsError> {
        self.canonical_relative(rel)?;
        resolve_file_in_root(&self.root, &openwebide_core::vfs::workspace_path(rel)?)
            .map_err(VfsError::PathEscape)
    }
    fn directory(&self, rel: &str) -> Result<PathBuf, VfsError> {
        let rel = openwebide_core::vfs::workspace_path(rel)?;
        if rel.is_empty() {
            Ok(self.root.clone())
        } else {
            self.resolve(&rel)
        }
    }
}

impl Vfs for NativeFsVfs {
    fn move_atomic<'a>(
        &'a self,
        from: &'a str,
        to: &'a str,
        kind: openwebide_core::vfs::VfsEntryKind,
    ) -> VfsFuture<'a, ()> {
        // Preserve the non-yielding raw primitive for already-owned interpreters.
        Box::pin(async move { self.move_atomic_sync(from, to, kind) })
    }

    fn canonicalize<'a>(&'a self, path: &'a str) -> VfsFuture<'a, String> {
        Box::pin(async move { self.canonical_relative(path) })
    }
    fn read<'a>(&'a self, path: &'a str) -> VfsFuture<'a, String> {
        Box::pin(async move {
            String::from_utf8(self.read_bytes(path).await?)
                .map_err(|_| VfsError::Io("file is not valid UTF-8".into()))
        })
    }
    fn read_bytes<'a>(&'a self, path: &'a str) -> VfsFuture<'a, Vec<u8>> {
        Box::pin(async move {
            const CAP: u64 = openwebide_core::vfs::MAX_READ_BYTES;
            let file = fs::File::open(self.resolve(path)?)
                .await
                .map_err(|e| io_error(e, path))?;
            let mut bytes = Vec::new();
            file.take(CAP + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|e| io_error(e, path))?;
            if bytes.len() as u64 > CAP {
                return Err(VfsError::Io("file exceeds 10 MiB".into()));
            }
            Ok(bytes)
        })
    }
    fn write<'a>(&'a self, path: &'a str, content: &'a str) -> VfsFuture<'a, ()> {
        self.write_bytes(path, content.as_bytes())
    }
    fn write_bytes<'a>(&'a self, path: &'a str, content: &'a [u8]) -> VfsFuture<'a, ()> {
        Box::pin(async move {
            let files = self.clone();
            let path = path.to_string();
            let content = content.to_vec();
            mutation::blocking(move || files.write_bytes_sync(&path, &content)).await
        })
    }

    fn replace_atomic<'a>(&'a self, path: &'a str, content: &'a [u8]) -> VfsFuture<'a, ()> {
        // A raw journaled replacement still completes before yielding.
        Box::pin(async move { self.replace_atomic_sync(path, content) })
    }

    fn remove_entry<'a>(
        &'a self,
        path: &'a str,
        kind: openwebide_core::vfs::VfsEntryKind,
    ) -> VfsFuture<'a, ()> {
        Box::pin(async move {
            let files = self.clone();
            let path = path.to_string();
            mutation::blocking(move || files.remove_entry_sync(&path, kind)).await
        })
    }

    fn list<'a>(&'a self, dir: &'a str) -> VfsFuture<'a, Vec<FileEntry>> {
        Box::pin(async move {
            let canonical = self.canonical_relative(dir)?;
            let mut reader = fs::read_dir(self.directory(dir)?)
                .await
                .map_err(|e| io_error(e, dir))?;
            let mut entries = Vec::new();
            while let Some(entry) = reader.next_entry().await.map_err(|e| io_error(e, dir))? {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name == ".spin" {
                    continue;
                }
                let path = if dir.is_empty() {
                    name.clone()
                } else {
                    format!("{}/{name}", dir.trim_end_matches('/'))
                };
                if openwebide_core::vfs::private_workspace_path(&path)
                    || openwebide_core::vfs::private_workspace_path(&format!("{canonical}/{name}"))
                {
                    continue;
                }
                let metadata = match entry.metadata().await {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(io_error(error, &path)),
                };
                entries.push(FileEntry {
                    name,
                    path,
                    is_dir: metadata.is_dir(),
                    size: if metadata.is_dir() { 0 } else { metadata.len() },
                });
            }
            Ok(entries)
        })
    }
    fn create<'a>(
        &'a self,
        path: &'a str,
        kind: openwebide_core::vfs::VfsEntryKind,
    ) -> VfsFuture<'a, ()> {
        Box::pin(async move {
            let files = self.clone();
            let path = path.to_string();
            mutation::blocking(move || files.create_sync(&path, kind)).await
        })
    }

    fn delete<'a>(&'a self, path: &'a str) -> VfsFuture<'a, ()> {
        Box::pin(async move {
            let files = self.clone();
            let path = path.to_string();
            mutation::blocking(move || files.delete_sync(&path)).await
        })
    }

    fn copy<'a>(&'a self, from: &'a str, to: &'a str) -> VfsFuture<'a, ()> {
        Box::pin(async move {
            let files = self.clone();
            let (from, to) = (from.to_string(), to.to_string());
            mutation::blocking(move || files.copy_sync(&from, &to)).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn plugin_authoring_uses_the_shared_workspace_contract() {
        let directory = tempfile::tempdir().unwrap();
        let vfs = NativeFsVfs {
            root: directory.path().canonicalize().unwrap(),
        };
        openwebide_core::plugins::authoring::testing::contract(&vfs).await;
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                "plugin.json",
                directory.path().join("authored-plugin/alias.json"),
            )
            .unwrap();
            assert!(
                openwebide_core::plugins::authoring::validate_workspace(
                    &vfs,
                    "authored-plugin",
                    || true
                )
                .await
                .is_err()
            );
        }
    }

    use openwebide_core::SearchHit;
    use openwebide_core::vfs::SearchOptions;

    #[cfg(unix)]
    #[tokio::test]
    async fn git_alias_writes_are_rejected_for_existing_and_new_files() {
        use openwebide_agent::{ToolExecutor, VfsToolExecutor};
        use openwebide_core::ToolCall;

        let dir = tempfile::tempdir().unwrap();
        let vfs = NativeFsVfs {
            root: dir.path().canonicalize().unwrap(),
        };
        vfs.write(".git/hooks/pre-commit", "original")
            .await
            .unwrap();
        std::os::unix::fs::symlink(".git/hooks", dir.path().join("hooks")).unwrap();
        let executor = VfsToolExecutor::new(vfs.clone());
        for path in ["hooks/pre-commit", "hooks/new/sub/hook"] {
            assert_eq!(
                vfs.canonicalize(path).await.unwrap(),
                format!(".git/{path}")
            );
            let outcome = executor
                .execute(&ToolCall {
                    id: "edit".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({"path": path, "content": "unwanted"}).to_string(),
                })
                .await;
            assert!(!outcome.ok);
            assert!(outcome.content.contains("refusing to write inside .git"));
        }
        assert_eq!(vfs.read(".git/hooks/pre-commit").await.unwrap(), "original");
        assert!(!dir.path().join(".git/hooks/new").exists());
        assert_eq!(
            vfs.canonicalize("ordinary/new/file").await.unwrap(),
            "ordinary/new/file"
        );
        std::os::unix::fs::symlink(".git/new", dir.path().join("new-hooks")).unwrap();
        assert_eq!(
            vfs.canonicalize("new-hooks/sub/hook").await.unwrap(),
            ".git/new/sub/hook"
        );
        std::os::unix::fs::symlink("ordinary/new", dir.path().join("new-files")).unwrap();
        assert_eq!(
            vfs.canonicalize("new-files/file").await.unwrap(),
            "ordinary/new/file"
        );
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("outside")).unwrap();
        assert!(matches!(
            vfs.canonicalize("outside/new").await,
            Err(VfsError::PathEscape(_))
        ));
    }

    #[tokio::test]
    async fn search_caps_hits_and_retained_line_characters() {
        let dir = tempfile::tempdir().unwrap();
        let vfs = NativeFsVfs {
            root: dir.path().to_path_buf(),
        };
        let line = format!("needle{}\n", "é".repeat(500));
        vfs.write("matches", &line.repeat(501)).await.unwrap();
        let hits = vfs
            .search_content("NEEDLE", "", SearchOptions::default())
            .await
            .unwrap();
        assert_eq!(hits.len(), 500);
        assert_eq!(hits.last().unwrap().line, 500);
        assert!(hits.iter().all(|hit| hit.text.chars().count() == 400));
    }

    #[tokio::test]
    async fn search_stops_at_aggregate_read_budget() {
        let dir = tempfile::tempdir().unwrap();
        let vfs = NativeFsVfs {
            root: dir.path().to_path_buf(),
        };
        let mut content = "x\n".repeat(5 * 1024 * 1024 - 4);
        content.push_str("needle!\n");
        assert_eq!(content.len(), 10 * 1024 * 1024);
        for index in 0..7 {
            vfs.write(&format!("file{index}"), &content).await.unwrap();
        }
        let hits = vfs
            .search_content("needle", "", SearchOptions::default())
            .await
            .unwrap();
        assert_eq!(hits.len(), 6);
    }

    #[tokio::test]
    async fn search_skips_files_exceeding_remaining_budget_and_visits_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let vfs = NativeFsVfs {
            root: dir.path().to_path_buf(),
        };
        let content = "x\n".repeat(5 * 1024 * 1024);
        for index in 0..7 {
            vfs.write(&format!("file{index}"), &content).await.unwrap();
        }
        vfs.write("src/small.txt", "needle").await.unwrap();
        let hits = vfs
            .search_content("needle", "", SearchOptions::default())
            .await
            .unwrap();
        assert_eq!(
            hits,
            vec![SearchHit {
                path: "src/small.txt".into(),
                line: 1,
                text: "needle".into(),
            }]
        );
    }

    #[tokio::test]
    async fn crud_copy_search_and_confinement() {
        let dir = tempfile::tempdir().unwrap();
        let vfs = NativeFsVfs {
            root: dir.path().canonicalize().unwrap(),
        };
        vfs.create("folder", openwebide_core::vfs::VfsEntryKind::Directory)
            .await
            .unwrap();
        vfs.create("folder/a", openwebide_core::vfs::VfsEntryKind::File)
            .await
            .unwrap();
        vfs.write("folder/a", "Needle\nsecond needle")
            .await
            .unwrap();
        assert!(matches!(
            vfs.create("folder/a", openwebide_core::vfs::VfsEntryKind::File)
                .await,
            Err(VfsError::AlreadyExists(_))
        ));
        assert_eq!(vfs.read("folder/a").await.unwrap(), "Needle\nsecond needle");
        assert_eq!(vfs.list("").await.unwrap()[0].name, "folder");
        vfs.copy("folder/a", "backup/new/a").await.unwrap();
        assert_eq!(
            vfs.read("backup/new/a").await.unwrap(),
            vfs.read("folder/a").await.unwrap()
        );
        vfs.write("node_modules/a", "needle").await.unwrap();
        let hits = vfs
            .search_content("needle", "", SearchOptions::default())
            .await
            .unwrap();
        assert_eq!(hits.len(), 4);
        assert_eq!(
            vfs.search_content(
                "needle",
                "",
                SearchOptions {
                    include_ignored: true
                }
            )
            .await
            .unwrap()
            .len(),
            5
        );
        for path in ["../outside", ".spin/db", "a/.spin/db"] {
            assert!(vfs.read(path).await.is_err());
            assert!(vfs.write(path, "x").await.is_err());
            assert!(
                vfs.create(path, openwebide_core::vfs::VfsEntryKind::File)
                    .await
                    .is_err()
            );
            assert!(vfs.delete(path).await.is_err());
            assert!(vfs.copy("folder/a", path).await.is_err());
        }
        vfs.write("/rooted", "project-relative").await.unwrap();
        assert_eq!(vfs.read("rooted").await.unwrap(), "project-relative");
        assert!(matches!(
            vfs.read("missing").await,
            Err(VfsError::NotFound(_))
        ));
        assert!(matches!(
            vfs.delete("missing").await,
            Err(VfsError::NotFound(_))
        ));
        fs::write(dir.path().join("binary"), [0xff]).await.unwrap();
        assert!(vfs.read("binary").await.is_err());
        vfs.copy("binary", "binary-copy").await.unwrap();
        assert_eq!(
            fs::read(dir.path().join("binary-copy")).await.unwrap(),
            [0xff]
        );
        fs::write(dir.path().join("large"), vec![b'x'; 10 * 1024 * 1024 + 1])
            .await
            .unwrap();
        assert!(vfs.read("large").await.is_err());
        vfs.delete("folder").await.unwrap();
        assert!(matches!(
            vfs.read("folder/a").await,
            Err(VfsError::NotFound(_))
        ));
    }
}

#[cfg(test)]
mod creation_contract {
    #[tokio::test]
    async fn transaction_primitives() {
        let directory = tempfile::tempdir().unwrap();
        let vfs = super::NativeFsVfs {
            root: directory.path().canonicalize().unwrap(),
        };
        openwebide_core::testing::workspace_transaction_primitives_contract(&vfs).await;
        openwebide_core::testing::workspace_transaction_contract(&vfs).await;
        #[cfg(unix)]
        {
            use openwebide_core::Vfs;
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(directory.path().join("executable"), "before").unwrap();
            std::fs::set_permissions(
                directory.path().join("executable"),
                std::fs::Permissions::from_mode(0o751),
            )
            .unwrap();
            vfs.replace_atomic("executable", b"after").await.unwrap();
            assert_eq!(
                std::fs::metadata(directory.path().join("executable"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o751
            );
            std::os::unix::fs::symlink("executable", directory.path().join("alias")).unwrap();
            assert!(
                vfs.remove_entry("alias", openwebide_core::vfs::VfsEntryKind::File)
                    .await
                    .is_err()
            );
            assert_eq!(
                std::fs::read_to_string(directory.path().join("executable")).unwrap(),
                "after"
            );
        }
        assert!(std::fs::read_dir(directory.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".openwebide-replace-")
        }));
    }
    #[tokio::test]
    async fn native() {
        let dir = tempfile::tempdir().unwrap();
        let vfs = super::NativeFsVfs {
            root: dir.path().canonicalize().unwrap(),
        };
        openwebide_core::testing::vfs_creation_contract(&vfs).await;
        openwebide_core::testing::project_checkpoint_contract(&vfs).await;
        openwebide_core::testing::checkpoint_coverage_contract(&vfs).await;
    }
}
