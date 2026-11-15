//! Explicitly uncoordinated WASI primitives for companion-disabled deployments.
//! The shared FileMutation interpreter owns operation dispatch. These calls stay
//! bound to one opened project directory and never retry a native request.
use super::{
    Descriptor, DescriptorFlags, DescriptorType, ErrorCode, FsError, HostFsVfs, OpenFlags,
    PathFlags, Result, WasiResolver, fs_error, fs_error_at, map_vfs_err, read_dir_entries,
    sanitize,
};
use openwebide_core::{FileEntry, Vfs, VfsError, VfsFuture, vfs::VfsEntryKind};

pub(super) struct DirectFiles {
    root: Descriptor,
    reads: HostFsVfs,
}
impl DirectFiles {
    pub(super) async fn open(base: &str) -> std::result::Result<Self, VfsError> {
        let root = super::root().map_err(map_vfs_err)?;
        let path = sanitize(&WasiResolver::new(&root), base)
            .await
            .map_err(map_vfs_err)?;
        let root = if path.is_empty() {
            root
        } else {
            root.open_at(
                PathFlags::empty(),
                path,
                OpenFlags::DIRECTORY,
                DescriptorFlags::MUTATE_DIRECTORY,
            )
            .await
            .map_err(fs_error)
            .map_err(map_vfs_err)?
        };
        Ok(Self {
            root,
            reads: HostFsVfs::new(base),
        })
    }
    async fn file_at_read(&self, rel: &str) -> Result<Descriptor> {
        let rel = sanitize(&WasiResolver::new(&self.root), rel).await?;
        self.root
            .open_at(
                PathFlags::empty(),
                rel.clone(),
                OpenFlags::empty(),
                DescriptorFlags::READ,
            )
            .await
            .map_err(|code| fs_error_at(code, &rel))
    }
    async fn file_at_write(&self, rel: &str) -> Result<Descriptor> {
        let root = &self.root;
        let resolver = WasiResolver::new(root);
        let rel = sanitize(&resolver, rel).await?;
        self.ensure_parent_dirs(&rel).await?;
        root.open_at(
            PathFlags::empty(),
            rel.clone(),
            OpenFlags::CREATE | OpenFlags::TRUNCATE,
            DescriptorFlags::WRITE,
        )
        .await
        .map_err(|code| fs_error_at(code, &rel))
    }

    /// Create every ancestor directory of `rel`, ignoring "already exists".
    async fn ensure_parent_dirs(&self, rel: &str) -> Result<()> {
        let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
        if parts.len() <= 1 {
            return Ok(());
        }
        let root = &self.root;
        let mut prefix = String::new();
        for part in &parts[..parts.len() - 1] {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            match root.create_directory_at(prefix.clone()).await {
                Ok(()) => {}
                Err(ErrorCode::Exist) => {}
                Err(code) => return Err(fs_error_at(code, &prefix)),
            }
        }
        Ok(())
    }

    async fn write_bytes_impl(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        let file = self.file_at_write(rel).await?;
        write_stream(&file, bytes.to_vec()).await
    }

    /// Create an empty file or a directory at `rel`.
    async fn create_impl(&self, rel: &str, is_dir: bool) -> Result<()> {
        let root_desc = &self.root;
        let resolver = WasiResolver::new(root_desc);
        let rel = sanitize(&resolver, rel).await?;

        if is_dir {
            self.ensure_parent_dirs(&rel).await?;
            match root_desc.create_directory_at(rel.clone()).await {
                Ok(()) => Ok(()),
                Err(ErrorCode::Exist) => Ok(()),
                Err(code) => Err(fs_error_at(code, &rel)),
            }
        } else {
            self.ensure_parent_dirs(&rel).await?;
            let root = &self.root;
            let file = root
                .open_at(
                    PathFlags::empty(),
                    rel.clone(),
                    OpenFlags::CREATE | OpenFlags::EXCLUSIVE,
                    DescriptorFlags::WRITE,
                )
                .await
                .map_err(|code| match code {
                    ErrorCode::Exist => FsError::AlreadyExists(rel.clone()),
                    other => fs_error_at(other, &rel),
                })?;
            write_stream(&file, Vec::new()).await
        }
    }

    /// Copy `from` to `to`, overwriting `to` if it exists.
    /// Copy `from` to `to`, overwriting `to` if it exists.
    async fn copy_impl(&self, from: &str, to: &str) -> Result<()> {
        let from_file = self.file_at_read(from).await?;
        let to_file = self.file_at_write(to).await?;

        // Using `write_via_stream` with the stream from `read_via_stream`
        // instructs the WASI runtime to pipe the data efficiently.
        let (from_stream, read_fut) = from_file.read_via_stream(0);
        let write_fut = to_file.write_via_stream(from_stream, 0);

        match write_fut.await {
            Ok(()) => {}
            Err(code) => return Err(fs_error(code)),
        }
        match read_fut.await {
            Ok(()) => Ok(()),
            Err(code) => Err(fs_error(code)),
        }
    }

    /// Delete a file, symlink, or directory tree at `rel`.
    ///
    /// Only the **parent directory** is resolved through the PathResolver, so all
    /// `.spin`/escape checks run on every ancestor component. The final path
    /// component is used as a *literal* name within that resolved parent — meaning
    /// a symlink is removed as a link rather than having its target deleted.
    async fn delete_impl(&self, rel: &str) -> Result<()> {
        // Split into parent path and leaf name.  Reject any leaf that looks like
        // a traversal (e.g. an empty leaf, `.`, or `..`) since those shouldn't
        // appear as real filesystem names and could be confusing.
        let rel = rel.trim_matches('/');
        let (parent_rel, leaf) = match rel.rfind('/') {
            Some(idx) => (&rel[..idx], &rel[idx + 1..]),
            None => ("", rel),
        };
        if leaf.is_empty() || leaf == "." || leaf == ".." {
            return Err(FsError::Io(format!("invalid path component: {leaf:?}")));
        }
        if leaf.contains('/') {
            return Err(FsError::Io(format!(
                "leaf name must not contain '/': {leaf:?}"
            )));
        }

        // Resolve the *parent* through sanitize so its `.spin`/escape checks run.
        let root_desc = &self.root;
        let resolver = WasiResolver::new(root_desc);
        let resolved_parent = sanitize(&resolver, parent_rel).await?;
        openwebide_core::vfs::workspace_path(&format!("{resolved_parent}/{leaf}"))?;

        // Open the resolved parent directory and unlink the leaf by its literal name.
        let parent = if resolved_parent.is_empty() {
            None
        } else {
            Some(
                root_desc
                    .open_at(
                        PathFlags::empty(),
                        resolved_parent.clone(),
                        OpenFlags::DIRECTORY,
                        DescriptorFlags::MUTATE_DIRECTORY,
                    )
                    .await
                    .map_err(|code| fs_error_at(code, &resolved_parent))?,
            )
        };
        let parent_dir = parent.as_ref().unwrap_or(root_desc);

        remove_entry(parent_dir, leaf).await
    }
}
impl Vfs for DirectFiles {
    fn read<'a>(&'a self, path: &'a str) -> VfsFuture<'a, String> {
        self.reads.read(path)
    }
    fn read_bytes<'a>(&'a self, path: &'a str) -> VfsFuture<'a, Vec<u8>> {
        self.reads.read_bytes(path)
    }
    fn list<'a>(&'a self, path: &'a str) -> VfsFuture<'a, Vec<FileEntry>> {
        self.reads.list(path)
    }
    fn canonicalize<'a>(&'a self, path: &'a str) -> VfsFuture<'a, String> {
        self.reads.canonicalize(path)
    }
    fn write<'a>(&'a self, path: &'a str, content: &'a str) -> VfsFuture<'a, ()> {
        self.write_bytes(path, content.as_bytes())
    }
    fn write_bytes<'a>(&'a self, path: &'a str, bytes: &'a [u8]) -> VfsFuture<'a, ()> {
        Box::pin(async move {
            self.write_bytes_impl(path, bytes)
                .await
                .map_err(map_vfs_err)
        })
    }
    fn create<'a>(&'a self, path: &'a str, kind: VfsEntryKind) -> VfsFuture<'a, ()> {
        Box::pin(async move {
            self.create_impl(path, kind.is_dir())
                .await
                .map_err(map_vfs_err)
        })
    }
    fn copy<'a>(&'a self, from: &'a str, to: &'a str) -> VfsFuture<'a, ()> {
        Box::pin(async move { self.copy_impl(from, to).await.map_err(map_vfs_err) })
    }
    fn delete<'a>(&'a self, path: &'a str) -> VfsFuture<'a, ()> {
        Box::pin(async move { self.delete_impl(path).await.map_err(map_vfs_err) })
    }
}

async fn write_stream(file: &Descriptor, bytes: Vec<u8>) -> Result<()> {
    let total = bytes.len();
    let (mut writer, reader) = crate::wit_stream::new::<u8>();
    let drain = file.write_via_stream(reader, 0);

    let remaining = writer.write_all(bytes).await;
    if !remaining.is_empty() {
        return Err(FsError::Io(format!(
            "stream closed before all {total} bytes were written ({} delivered)",
            total - remaining.len()
        )));
    }

    drop(writer);
    match drain.await {
        Ok(()) => Ok(()),
        Err(code) => Err(fs_error(code)),
    }
}

fn remove_entry<'a>(
    parent: &'a Descriptor,
    leaf: &'a str,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
        let metadata = parent
            .stat_at(PathFlags::empty(), leaf.to_string())
            .await
            .map_err(|code| fs_error_at(code, leaf))?;
        if matches!(metadata.type_, DescriptorType::Directory) {
            let directory = parent
                .open_at(
                    PathFlags::empty(),
                    leaf.to_string(),
                    OpenFlags::DIRECTORY,
                    DescriptorFlags::MUTATE_DIRECTORY,
                )
                .await
                .map_err(|code| fs_error_at(code, leaf))?;
            for entry in read_dir_entries(&directory).await? {
                if entry.name != "." && entry.name != ".." {
                    remove_entry(&directory, &entry.name).await?;
                }
            }
            parent
                .remove_directory_at(leaf.to_string())
                .await
                .map_err(|code| fs_error_at(code, leaf))
        } else {
            // Do not follow symlinks: only unlink their literal directory entry.
            parent
                .unlink_file_at(leaf.to_string())
                .await
                .map_err(|code| fs_error_at(code, leaf))
        }
    })
}
