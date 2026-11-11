//! Native I/O primitives. Policy and observation accumulation live above these.
use super::*;
use crate::runs::native_vfs::rooted::{
    Rooted,
    revision::{Observation, Revision},
};
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
use std::{fs::File, io::Read};

pub(super) struct Native(Rooted);
impl Native {
    pub fn new(root: &Path) -> Result<Self, String> {
        Ok(Self(Rooted(File::open(root).map_err(text)?)))
    }
    pub fn current(&self, root: &Path) -> bool {
        root.canonicalize().is_ok_and(|path| path == root)
            && self
                .0
                .0
                .try_clone()
                .and_then(same_file::Handle::from_file)
                .and_then(|original| same_file::Handle::from_path(root).map(|now| now == original))
                .unwrap_or(false)
    }
    pub fn revisions(&self, path: &str) -> Result<Vec<(String, String, String)>, String> {
        fn stamp(metadata: &fs::Stat) -> Result<String, String> {
            Revision::of(metadata)
                .and_then(|revision| revision.encode())
                .map_err(text)
        }
        let root = Observation::of(&self.0.0).map_err(text)?;
        let mut result = vec![(
            String::new(),
            root.revision.encode().map_err(text)?,
            root.identity.fingerprint(),
        )];
        let mut directory = self.0.0.try_clone().map_err(text)?;
        let mut relative = String::new();
        let parts = path
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        for (index, part) in parts.iter().enumerate() {
            if index >= 128 {
                return Err("Affected-file path exceeds its depth limit".into());
            }
            if !relative.is_empty() {
                relative.push('/');
            }
            relative.push_str(part);
            let metadata = match fs::statat(&directory, *part, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(value) => value,
                Err(rustix::io::Errno::NOENT) => {
                    result.push((relative, "missing".into(), String::new()));
                    break;
                }
                Err(error) => return Err(text(error)),
            };
            let kind = FileType::from_raw_mode(metadata.st_mode);
            let mut revision = stamp(&metadata)?;
            let mut next = None;
            let mut identity = String::new();
            if matches!(kind, FileType::Directory | FileType::RegularFile) {
                let flags = OFlags::RDONLY
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC
                    | if kind == FileType::Directory {
                        OFlags::DIRECTORY
                    } else {
                        OFlags::NONBLOCK
                    };
                let object =
                    File::from(fs::openat(&directory, *part, flags, Mode::empty()).map_err(text)?);
                if stamp(&fs::fstat(&object).map_err(text)?)? != revision {
                    return Err("An affected object changed while opening it".into());
                }
                let observed = Observation::of(&object).map_err(text)?;
                if stamp(&fs::fstat(&object).map_err(text)?)? != revision {
                    return Err("An affected object changed while reading its identity".into());
                }
                identity = observed.identity.fingerprint();
                revision = observed.revision.encode().map_err(text)?;
                if kind == FileType::Directory {
                    next = Some(object);
                }
            } else if kind == FileType::Symlink {
                let target = fs::readlinkat(&directory, *part, Vec::new()).map_err(text)?;
                if target.to_bytes().len() > 4096 {
                    return Err("Affected-file link exceeds its limit".into());
                }
                revision.push_str(&format!("{:?}", target.to_bytes()));
            }
            result.push((relative.clone(), revision, identity));
            if index + 1 == parts.len() {
                break;
            }
            let Some(next) = next else {
                break;
            };
            directory = next;
        }
        Ok(result)
    }
    pub fn read(&self, path: &str, limit: u64) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        let file = self.0.regular(path).map_err(text)?;
        let before = files::FileRevision::from(&file.metadata().map_err(text)?);
        file.take(limit.min(openwebide_core::vfs::MAX_READ_BYTES) + 1)
            .read_to_end(&mut bytes)
            .map_err(text)?;
        if bytes.len() as u64 > limit.min(openwebide_core::vfs::MAX_READ_BYTES) {
            return Err("Affected file exceeds its read limit".into());
        }
        // The surrounding observation also checks the named object; a retained
        // descriptor alone cannot detect a replacement at the same name.
        let after = self.0.regular(path).map_err(text)?;
        if before != files::FileRevision::from(&after.metadata().map_err(text)?) {
            return Err("An affected file changed while reading it".into());
        }
        Ok(bytes)
    }
    pub fn list(&self, path: &str) -> Result<Vec<openwebide_core::FileEntry>, String> {
        self.0
            .list(path, protocol::MAX_ANALYSIS_FILES)
            .map_err(text)
    }
}
