//! Retain the symbolic links which selected an executable, including links in
//! parent directories and intermediate targets. Checking only the final file
//! misses a replaced alias which happens to resolve to the same executable.
use super::{CHANGED, FileRevision, MAX_PATH_BYTES};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

const MAX_LINKS: usize = 128;

struct Link {
    target: PathBuf,
    revision: FileRevision,
}

pub(super) struct Links(BTreeMap<PathBuf, Link>);
impl Links {
    pub(super) fn read(path: &Path) -> Result<Self, String> {
        let mut links = BTreeMap::new();
        let mut visited = BTreeSet::new();
        let mut pending = vec![path.to_path_buf()];
        let mut path_bytes = 0_usize;
        while let Some(path) = pending.pop() {
            let mut prefix = PathBuf::new();
            for component in path.components() {
                prefix.push(component);
                if !visited.insert(prefix.clone()) {
                    continue;
                }
                path_bytes = path_bytes.saturating_add(prefix.as_os_str().len());
                if path_bytes > MAX_PATH_BYTES {
                    return Err(
                        "Language executable link paths exceed their inspection budget".into(),
                    );
                }
                let metadata =
                    std::fs::symlink_metadata(&prefix).map_err(|error| error.to_string())?;
                if !metadata.file_type().is_symlink() {
                    continue;
                }
                if links.len() >= MAX_LINKS {
                    return Err("Language executable links exceed their inspection limit".into());
                }
                let target = std::fs::read_link(&prefix).map_err(|error| error.to_string())?;
                let resolved = if target.is_absolute() {
                    target.clone()
                } else {
                    prefix.parent().unwrap_or(Path::new("")).join(&target)
                };
                pending.push(resolved);
                links.insert(
                    prefix.clone(),
                    Link {
                        target,
                        revision: FileRevision::from(&metadata),
                    },
                );
            }
        }
        let result = Self(links);
        if !result.unchanged() {
            return Err(CHANGED.into());
        }
        Ok(result)
    }

    pub(super) fn unchanged(&self) -> bool {
        self.0.iter().all(|(path, link)| {
            std::fs::symlink_metadata(path).is_ok_and(|metadata| {
                metadata.file_type().is_symlink() && FileRevision::from(&metadata) == link.revision
            }) && std::fs::read_link(path).is_ok_and(|target| target == link.target)
        })
    }
}
