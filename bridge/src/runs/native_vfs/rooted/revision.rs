//! Metadata read from one owned descriptor, interpreted only by native I/O.
use super::*;
use openwebide_core::workspace_transaction::OriginalMutation;
mod attributes;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Revision {
    mode: u32,
    links: u64,
    uid: u32,
    gid: u32,
    size: i64,
    modified: (i64, i64),
    changed: (i64, i64),
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attributes: Option<String>,
}
impl Revision {
    pub fn of(metadata: &fs::Stat) -> io::Result<Self> {
        // Native stat widths and signedness vary by OS and architecture.
        // Preserve their values in the durable representation without casts.
        fn integer<T: TryInto<U>, U>(value: T) -> io::Result<U> {
            value
                .try_into()
                .map_err(|_| io::Error::other("Native metadata exceeds its supported range"))
        }
        Ok(Self {
            mode: integer(metadata.st_mode)?,
            links: integer(metadata.st_nlink)?,
            uid: metadata.st_uid,
            gid: metadata.st_gid,
            size: integer(metadata.st_size)?,
            modified: (
                integer(metadata.st_mtime)?,
                integer(metadata.st_mtime_nsec)?,
            ),
            changed: (
                integer(metadata.st_ctime)?,
                integer(metadata.st_ctime_nsec)?,
            ),
            attributes: None,
        })
    }
    fn of_file(file: &File) -> io::Result<Self> {
        let metadata = fs::fstat(file)?;
        let mut revision = Self::of(&metadata)?;
        let attributes = attributes::digest(file, &metadata)?;
        if revision != Self::of(&fs::fstat(file)?)? {
            return Err(io::Error::other(
                "An object changed while reading its attributes",
            ));
        }
        revision.attributes = attributes;
        Ok(revision)
    }
    pub fn has_attributes(&self) -> bool {
        self.attributes.is_some()
    }
    pub fn with_added_link(mut self) -> io::Result<Self> {
        self.links = self
            .links
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Native link count exceeds its supported range"))?;
        Ok(self)
    }
    pub fn matches_without_link(&self, actual: &Self, mutation: OriginalMutation) -> bool {
        let Some(links) = self.links.checked_sub(1) else {
            return false;
        };
        Self {
            links,
            ..self.clone()
        }
        .matches(actual, mutation)
    }
    pub fn encode(&self) -> io::Result<String> {
        serde_json::to_string(self).map_err(io::Error::other)
    }
    pub fn decode(value: &str) -> io::Result<Self> {
        serde_json::from_str(value).map_err(io::Error::other)
    }
    pub fn matches(&self, actual: &Self, mutation: OriginalMutation) -> bool {
        let namespace = mutation.directory_changed && self.mode & 0o170000 == 0o040000;
        self.mode == actual.mode
            && self.attributes == actual.attributes
            && self.uid == actual.uid
            && self.gid == actual.gid
            && (namespace
                || (self.links == actual.links
                    && self.size == actual.size
                    && self.modified == actual.modified))
            && (namespace || mutation.moved || self.changed == actual.changed)
    }
}
pub(crate) struct Observation {
    pub identity: Identity,
    pub revision: Revision,
}
impl Observation {
    pub fn of(file: &File) -> io::Result<Self> {
        let before = Revision::of_file(file)?;
        let identity = Identity::of(file)?;
        let after = Revision::of_file(file)?;
        if before != after {
            return Err(io::Error::other(
                "An object changed while reading its metadata",
            ));
        }
        Ok(Self {
            identity,
            revision: after,
        })
    }
}
