//! Explicit metadata selection using the same bounded generation publisher as
//! project sources. Selection never discovers watches in a kernel callback.
use super::*;
use std::collections::BTreeSet;

#[derive(Clone)]
pub(in crate::languages::host) struct MetadataFiles {
    pub files: BTreeSet<PathBuf>,
    pub parents: Vec<PathBuf>,
    pub boundaries: BTreeSet<PathBuf>,
}
impl FileSelection for MetadataFiles {
    fn project_sources(&self) -> bool {
        false
    }

    fn collect(
        &self,
        _root: &Path,
        cancelled: &AtomicBool,
        limit: usize,
    ) -> Result<Inventory, String> {
        let mut paths = BTreeMap::new();
        for path in &self.files {
            paths.insert(path.clone(), false);
        }
        for parent in &self.parents {
            let mut path = parent.as_path();
            let mut depth = 0;
            // Observe the nearest existing reference namespace, never the
            // whole Git directory (ordinary index refreshes do not change refs).
            while !self.boundaries.contains(path) {
                check(cancelled)?;
                if depth >= MAX_DEPTH {
                    return Err("Git metadata parent selection exceeds its depth limit".into());
                }
                depth += 1;
                match std::fs::symlink_metadata(path) {
                    Ok(_) => {
                        paths.insert(path.to_path_buf(), true);
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.to_string()),
                }
                path = path.parent().ok_or("Git metadata escaped its directory")?;
            }
        }
        let mut inventory = Inventory::default();
        let mut bytes = 0;
        for (path, directory) in paths {
            check(cancelled)?;
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.to_string()),
            };
            let canonical = Arc::new(path.canonicalize().map_err(|error| error.to_string())?);
            let target = metadata
                .file_type()
                .is_symlink()
                .then(|| std::fs::metadata(canonical.as_path()).map_err(|error| error.to_string()))
                .transpose()?;
            let actual = target.as_ref().unwrap_or(&metadata);
            let directory_link = !directory && metadata.file_type().is_symlink() && actual.is_dir();
            if !directory_link
                && !(if directory {
                    actual.is_dir()
                } else {
                    actual.is_file()
                })
            {
                return Err("A Git metadata input changed its file type".into());
            }
            let entry = Entry {
                stamp: Stamp::read(&path, &metadata)?,
                // A symbolic Git-directory marker binds the link itself.
                // Its target directory identity is retained by Repository;
                // target namespace timestamps also change on index refreshes.
                target: target
                    .as_ref()
                    .filter(|_| !directory_link)
                    .map(|metadata| Stamp::read(&canonical, metadata))
                    .transpose()?,
                canonical: Some(canonical),
                watchable: !directory_link,
            };
            insert(&mut inventory, path, entry, &mut bytes, limit)?;
        }
        check(cancelled)?;
        Ok(inventory)
    }
}
