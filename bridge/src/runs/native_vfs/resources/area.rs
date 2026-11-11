use super::*;

const LIMIT: u64 = METADATA_LIMIT;
const FOLDERS: [&str; 4] = ["objects", "new", "receipts", "anchors"];
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Owner {
    version: u32,
    root: String,
    scope: ResourceScope,
    directory: Identity,
}

pub(super) struct Area {
    directory: Rooted,
}
impl Area {
    fn names(scope: &ResourceScope) -> (String, String, String, String) {
        let base = format!(".openwebide/recovery-{}", scope.transaction);
        (
            base.clone(),
            format!("{base}.init"),
            format!("{base}.owner"),
            format!("{base}.owner.part"),
        )
    }
    pub fn directory(&self, name: &str) -> io::Result<Rooted> {
        self.directory.directory(name)
    }
    pub fn optional_directory(&self, name: &str) -> io::Result<Option<Rooted>> {
        match self.directory.kind(name)? {
            None => Ok(None),
            Some(FileType::Directory) => self.directory(name).map(Some),
            _ => Err(invalid("A recovery control folder changed its kind")),
        }
    }
    fn verify_folders(&self) -> io::Result<()> {
        for entry in self.directory.list("", FOLDERS.len() + 1)? {
            if !FOLDERS.contains(&entry.name.as_str()) || !entry.is_dir {
                return Err(invalid("The recovery area contains an unknown entry"));
            }
        }
        Ok(())
    }
    fn empty_bootstrap(root: &Rooted, path: &str) -> io::Result<()> {
        let area = Self {
            directory: root.directory(path)?,
        };
        area.verify_folders()?;
        for folder in FOLDERS {
            if area.directory.kind(folder)?.is_some() && !area.directory.list(folder, 1)?.is_empty()
            {
                return Err(invalid("An incomplete recovery area contains unowned data"));
            }
        }
        Ok(())
    }
    pub fn open(
        root: &Rooted,
        identity: &str,
        scope: &ResourceScope,
        create: bool,
    ) -> io::Result<Option<Self>> {
        let (base, init, owner, part) = Self::names(scope);
        if let Some(kind) = root.kind(&owner)? {
            if kind != FileType::RegularFile {
                return Err(invalid("Recovery ownership metadata is not a regular file"));
            }
            let metadata: Owner =
                serde_json::from_slice(&root.read(&owner, LIMIT)?).map_err(invalid)?;
            if metadata.version != 1 || metadata.root != identity || metadata.scope != *scope {
                return Err(invalid(
                    "The recovery area belongs to another folder or transaction",
                ));
            }
            if create && root.kind(&part)?.is_some() {
                write_metadata(root, &part, &owner, &root.read(&owner, LIMIT)?)?;
            }
            if root.kind(&base)?.is_some() {
                if root.kind(&init)?.is_some() {
                    return Err(invalid("A second recovery staging folder was preserved"));
                }
                let directory = root.directory(&base)?;
                if Identity::of(&directory.0)? != metadata.directory {
                    return Err(invalid("The physical recovery area was replaced"));
                }
                let area = Self { directory };
                area.verify_folders()?;
                return Ok(Some(area));
            }
            if root.kind(&init)?.is_some() {
                if root.identity(&init)? != metadata.directory {
                    return Err(invalid("The recovery staging area was replaced"));
                }
                Self::empty_bootstrap(root, &init)?;
                if create {
                    root.transfer(&init, root, &base, VfsEntryKind::Directory)?;
                    return Self::open(root, identity, scope, false);
                }
                return Ok(None);
            }
            if create {
                return Err(invalid("The original recovery area became unavailable"));
            }
            return Ok(None);
        }
        if root.kind(&base)?.is_some() {
            return Err(invalid("An unowned folder collides with the recovery area"));
        }
        if !create {
            if root.kind(&init)?.is_some() {
                Self::empty_bootstrap(root, &init)?;
            }
            return Ok(None);
        }
        match root.kind(".openwebide")? {
            None => root.create(".openwebide", VfsEntryKind::Directory, true)?,
            Some(FileType::Directory) => {}
            _ => return Err(invalid("The recovery parent is not a regular directory")),
        }
        if root.kind(&init)?.is_none() {
            root.create(&init, VfsEntryKind::Directory, true)?;
        }
        Self::empty_bootstrap(root, &init)?;
        let directory = root.directory(&init)?;
        for name in FOLDERS {
            if directory.kind(name)?.is_none() {
                directory.create(name, VfsEntryKind::Directory, true)?;
            }
        }
        let metadata = Owner {
            version: 1,
            root: identity.into(),
            scope: scope.clone(),
            directory: Identity::of(&directory.0)?,
        };
        let bytes = serde_json::to_vec(&metadata).map_err(invalid)?;
        write_metadata(root, &part, &owner, &bytes)?;
        root.transfer(&init, root, &base, VfsEntryKind::Directory)?;
        Self::open(root, identity, scope, false)
    }
    pub fn finish(root: &Rooted, identity: &str, scope: &ResourceScope) -> io::Result<()> {
        let (base, init, owner, part) = Self::names(scope);
        if root.kind(&init)?.is_some() || root.kind(&part)?.is_some() {
            // Initialization may have stopped before publishing any originals.
            // Complete its ownership publication before deleting owned metadata.
            Self::open(root, identity, scope, true)?;
        }
        if let Some(area) = Self::open(root, identity, scope, false)? {
            for name in FOLDERS {
                if area.directory.kind(name)?.is_some() {
                    if !area.directory.list(name, 1)?.is_empty() {
                        return Err(invalid(
                            "Recovery payloads remain; the journal must finish their cleanup",
                        ));
                    }
                    area.directory.remove(name, VfsEntryKind::Directory)?;
                }
            }
            root.remove(&base, VfsEntryKind::Directory)?;
        }
        if root.kind(&owner)?.is_some() {
            root.remove(&owner, VfsEntryKind::File)?;
        }
        Ok(())
    }
}

pub(super) fn write_metadata(
    root: &Rooted,
    part: &str,
    destination: &str,
    bytes: &[u8],
) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    if bytes.len() as u64 > LIMIT {
        return Err(invalid("Recovery metadata exceeds its size limit"));
    }
    if root.kind(destination)?.is_some() {
        if root.read(destination, LIMIT)? != bytes {
            return Err(invalid(
                "Recovery ownership metadata does not match this intent",
            ));
        }
        if root.kind(part)?.is_some() {
            if root.regular(part)?.metadata()?.nlink() != 1
                || !bytes.starts_with(&root.read(part, LIMIT)?)
            {
                return Err(invalid("Unknown recovery metadata was preserved"));
            }
            root.remove(part, VfsEntryKind::File)?;
        }
        return Ok(());
    }
    if root.kind(part)?.is_some() {
        let file = root.regular(part)?;
        let previous = root.read(part, LIMIT)?;
        if file.metadata()?.nlink() != 1 || !bytes.starts_with(&previous) {
            return Err(invalid("Unknown recovery metadata was preserved"));
        }
        root.remove(part, VfsEntryKind::File)?;
    }
    root.write_new(part, bytes)?;
    root.transfer(part, root, destination, VfsEntryKind::File)
}
