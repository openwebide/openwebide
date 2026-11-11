//! Path-based fallback retains the existing platform I/O boundary. Descriptor
//! anchoring is supplied by the Linux/macOS adapter; Windows execution remains
//! a separate native verification requirement.
use super::*;
use std::fs::File;
pub(super) struct Native {
    root: PathBuf,
    handle: same_file::Handle,
}
impl Native {
    pub fn new(root: &Path) -> Result<Self, String> {
        Ok(Self {
            root: root.into(),
            handle: same_file::Handle::from_path(root).map_err(text)?,
        })
    }
    pub fn current(&self, root: &Path) -> bool {
        root.canonicalize().is_ok_and(|path| path == root)
            && same_file::Handle::from_path(root).is_ok_and(|now| now == self.handle)
    }
    pub fn revisions(&self, path: &str) -> Result<Vec<(String, String, String)>, String> {
        let mut result = Vec::new();
        let mut relative = String::new();
        for (depth, part) in std::iter::once("")
            .chain(path.split('/').filter(|part| !part.is_empty()))
            .enumerate()
        {
            if depth > 128 {
                return Err("Affected-file path exceeds its depth limit".into());
            }
            if !relative.is_empty() {
                relative.push('/');
            }
            relative.push_str(part);
            let path = self.root.join(&relative);
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(value) => value,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    result.push((relative, "missing".into(), String::new()));
                    break;
                }
                Err(error) => return Err(text(error)),
            };
            let value = format!(
                "{:?}:{:?}:{:?}",
                files::FileRevision::from(&metadata),
                metadata.file_type(),
                metadata.permissions()
            );
            #[cfg(windows)]
            let (value, identity) = {
                let (current, identity) = generation::windows::read(&path)?;
                if files::FileRevision::from(&current) != files::FileRevision::from(&metadata) {
                    return Err("An affected object changed while reading its identity".into());
                }
                (
                    format!("{value}:{identity:?}"),
                    if metadata.file_type().is_symlink() {
                        String::new()
                    } else {
                        identity.fingerprint()
                    },
                )
            };
            #[cfg(not(windows))]
            let identity = String::new();
            result.push((relative.clone(), value, identity));
            if metadata.file_type().is_symlink() {
                break;
            }
        }
        Ok(result)
    }
    pub fn read(&self, path: &str, limit: u64) -> Result<Vec<u8>, String> {
        use std::io::Read;
        let metadata = std::fs::metadata(self.root.join(path)).map_err(text)?;
        if !metadata.is_file() || metadata.len() > limit.min(openwebide_core::vfs::MAX_READ_BYTES) {
            return Err("Affected file must be regular and within its read limit".into());
        }
        let mut bytes = Vec::new();
        File::open(self.root.join(path))
            .map_err(text)?
            .take(limit.min(openwebide_core::vfs::MAX_READ_BYTES) + 1)
            .read_to_end(&mut bytes)
            .map_err(text)?;
        if bytes.len() as u64 > limit.min(openwebide_core::vfs::MAX_READ_BYTES) {
            return Err("Affected file exceeds its read limit".into());
        }
        Ok(bytes)
    }
    pub fn list(&self, path: &str) -> Result<Vec<openwebide_core::FileEntry>, String> {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(self.root.join(path)).map_err(text)? {
            if entries.len() >= protocol::MAX_ANALYSIS_FILES {
                return Err("Affected directory exceeds its entry limit".into());
            }
            let entry = entry.map_err(text)?;
            let metadata = entry.metadata().map_err(text)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "Affected names must be UTF-8")?;
            entries.push(openwebide_core::FileEntry {
                path: if path.is_empty() {
                    name.clone()
                } else {
                    format!("{path}/{name}")
                },
                name,
                is_dir: metadata.is_dir(),
                size: metadata.len(),
            });
        }
        Ok(entries)
    }
}
