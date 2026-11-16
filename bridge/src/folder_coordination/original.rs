//! Persisted original roots. Lexical overlap remains excluded after replacement.
use super::*;
use crate::file_identity::Identity;
use serde::{Deserialize, Serialize};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OriginalFolder {
    path: Vec<u8>,
    root: Identity,
    ancestors: Vec<Identity>,
}
// The persistent filesystem retains its own original ancestry across a new
// container/mount namespace. Enclosing runtime mounts still participate in the
// live physical gate, but are not the enduring workspace's identity.
impl PartialEq for OriginalFolder {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
            && self.root == other.root
            && self.workspace_ancestors() == other.workspace_ancestors()
    }
}
impl Eq for OriginalFolder {}
impl OriginalFolder {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn retained_by(&self, descriptors: &[std::os::fd::OwnedFd]) -> Result<(), String> {
        use std::os::fd::AsRawFd;
        self.validate()?;
        if descriptors.len() != self.ancestors.len() + 1 {
            return Err("Original process cleanup needs its complete folder claim".into());
        }
        for (descriptor, expected) in descriptors
            .iter()
            .rev()
            .zip(std::iter::once(&self.root).chain(&self.ancestors))
        {
            let file = std::fs::File::from(crate::exec::process_owner::duplicate(
                descriptor.as_raw_fd(),
            )?);
            if !file.metadata().map_err(|e| e.to_string())?.is_dir()
                || !expected.matches(&file).map_err(|e| e.to_string())?
            {
                return Err("The original process folder descriptors changed".into());
            }
        }
        Ok(())
    }
    pub(crate) fn reopen(&self) -> Result<Folder, String> {
        let folder = Folder::capture(&self.path())?;
        if Self::capture_mode(&folder, true, self.root.uses_device_number())? != *self {
            return Err("Reconnect the original physical recovery folder".into());
        }
        Ok(folder)
    }
    fn workspace_ancestors(&self) -> &[Identity] {
        let count = self
            .ancestors
            .iter()
            .take_while(|a| a.same_device(&self.root))
            .count();
        &self.ancestors[..count]
    }
    pub(crate) fn capture(folder: &Folder, enduring: bool) -> Result<Self, String> {
        Self::capture_mode(folder, enduring, false)
    }
    pub(crate) fn capture_mode(
        folder: &Folder,
        enduring: bool,
        legacy: bool,
    ) -> Result<Self, String> {
        if !folder.current() {
            return Err("The original recovery folder changed".into());
        }
        let value = Self {
            path: folder.path.as_os_str().as_bytes().into(),
            root: folder.original.clone(),
            ancestors: folder.original_ancestors.clone(),
        };
        #[cfg(target_os = "macos")]
        let value = if legacy {
            Self {
                root: folder.legacy_original.clone(),
                ancestors: folder.legacy_ancestors.clone(),
                ..value
            }
        } else {
            value
        };
        if enduring && !legacy && !value.root.has_persistent_volume() {
            return Err("Durable native recovery needs persistent volume identity".into());
        }
        if enduring {
            value.validate()?;
        }
        Ok(value)
    }
    pub(crate) fn validate(&self) -> Result<(), String> {
        let path = self.path();
        if self.path.len() > 16 * 1024
            || !path.is_absolute()
            || self.path.contains(&0)
            || self.ancestors.len() >= 64
            || path.ancestors().count() != self.ancestors.len() + 1
            || path.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::CurDir | std::path::Component::ParentDir
                )
            })
            || !self.root.has_birth_time()
            || !self.root.valid()
            || self.ancestors.iter().any(|identity| !identity.valid())
            || self
                .workspace_ancestors()
                .iter()
                .any(|a| !a.has_birth_time())
        {
            return Err(
                "Durable native recovery needs original folder and ancestor observations".into(),
            );
        }
        Ok(())
    }
    pub(crate) fn path(&self) -> PathBuf {
        std::ffi::OsString::from_vec(self.path.clone()).into()
    }
    pub(crate) fn overlaps(&self, other: &Self) -> bool {
        self.root.may_alias(&other.root)
            || self.ancestors.iter().any(|a| a.may_alias(&other.root))
            || other.ancestors.iter().any(|a| a.may_alias(&self.root))
            || self.path().starts_with(other.path())
            || other.path().starts_with(self.path())
    }
}
