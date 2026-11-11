use super::*;
use crate::workspace_entries::{contains_path, parent};

impl MemoryVfs {
    pub(super) fn move_entry(
        &self,
        from: &str,
        to: &str,
        kind: VfsEntryKind,
    ) -> Result<(), VfsError> {
        let from = workspace_path(from)?;
        let to = workspace_path(to)?;
        if from.is_empty() || to.is_empty() {
            return Err(VfsError::PathEscape(from));
        }
        // Match the existing files -> directories lock order, and retain every
        // map lock through the absence check and transfer. There is no yield or
        // detached writer which can publish after a caller starts recovery.
        let mut files = self
            .files
            .write()
            .map_err(|error| VfsError::Io(error.to_string()))?;
        let mut dirs = self
            .dirs
            .write()
            .map_err(|error| VfsError::Io(error.to_string()))?;
        let mut links = self
            .symlinks
            .write()
            .map_err(|error| VfsError::Io(error.to_string()))?;
        if links
            .keys()
            .any(|link| contains_path(link, &from) || contains_path(link, parent(&to)))
        {
            return Err(VfsError::PathEscape(from));
        }
        let source_file = files.contains_key(&from);
        fn directory(
            path: &str,
            files: &BTreeMap<String, Vec<u8>>,
            dirs: &BTreeSet<String>,
            links: &BTreeMap<String, String>,
        ) -> bool {
            dirs.contains(path)
                || files
                    .keys()
                    .chain(dirs.iter())
                    .chain(links.keys())
                    .any(|entry| entry != path && contains_path(path, entry))
        }
        let source_dir = directory(&from, &files, &dirs, &links);
        if !source_file && !source_dir {
            return Err(VfsError::NotFound(from));
        }
        if source_file == source_dir || source_dir != kind.is_dir() {
            return Err(VfsError::Io("Workspace entry kind changed".into()));
        }
        if files.contains_key(&to)
            || directory(&to, &files, &dirs, &links)
            || links.contains_key(&to)
        {
            return Err(VfsError::AlreadyExists(to));
        }
        if contains_path(&from, &to) || contains_path(&to, &from) {
            return Err(VfsError::PathEscape(to));
        }
        let mut ancestor = parent(&to);
        while !ancestor.is_empty() {
            if !directory(ancestor, &files, &dirs, &links) || files.contains_key(ancestor) {
                return Err(VfsError::NotFound(ancestor.into()));
            }
            ancestor = parent(ancestor);
        }
        let keys = files
            .keys()
            .filter(|path| contains_path(&from, path))
            .cloned()
            .collect::<Vec<_>>();
        for path in keys {
            let bytes = files.remove(&path).unwrap();
            files.insert(format!("{to}{}", &path[from.len()..]), bytes);
        }
        let keys = dirs
            .iter()
            .filter(|path| contains_path(&from, path))
            .cloned()
            .collect::<Vec<_>>();
        for path in keys {
            dirs.remove(&path);
            dirs.insert(format!("{to}{}", &path[from.len()..]));
        }
        if source_dir {
            dirs.insert(to.clone());
        }
        let keys = links
            .keys()
            .filter(|path| contains_path(&from, path))
            .cloned()
            .collect::<Vec<_>>();
        for path in keys {
            let target = links.remove(&path).unwrap();
            links.insert(format!("{to}{}", &path[from.len()..]), target);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn memory_object_moves_obey_the_shared_transfer_contract() {
        futures::executor::block_on(crate::testing::workspace_resource_transfer_contract(
            &super::MemoryVfs::new(),
        ));
    }
}
