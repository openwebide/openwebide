//! Durable snapshots of the original SQLite file, without ambient SQLite I/O.
//! An interrupted, published intent may restore only its exact original snapshot.
use super::*;

pub(super) const HEAD: &str = "recovery-head.json";
const MAX_FILES: usize = 32;
const MAX_PRIVATE_BYTES: u64 = MAX_DATABASE_BYTES * 4;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Snapshot {
    pub(super) name: String,
    pub(super) identity: Identity,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Head {
    pub(super) format: u32,
    pub(super) key: Key,
    pub(super) identity: Identity,
    pub(super) revision: u64,
    pub(super) digest: String,
    pub(super) snapshot: Snapshot,
    pub(super) phase: Phase,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Prepared,
    Committed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Barrier {
    Snapshot,
    Prepared,
    PartialDatabase,
    Database,
    Committed,
}
fn digest(data: &[u8]) -> String {
    openwebide_core::plugins::content_digest(data)
}
impl Namespace {
    fn head(&self) -> Result<Option<Head>, String> {
        match self.directory.symlink_metadata(HEAD) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Ok(metadata) if metadata.is_file() => {}
            _ => return Err(INVALID.into()),
        }
        let file = child(&self.directory, HEAD, self.key.uid)?;
        let head: Head =
            serde_json::from_slice(&bytes(&file, MAX_ANCHOR_BYTES)?).map_err(|_| INVALID)?;
        let id = head
            .snapshot
            .name
            .strip_prefix("snapshot-")
            .and_then(|name| name.strip_suffix(".sqlite3"))
            .ok_or(INVALID)?;
        if head.format != 1
            || head.key != self.key
            || !original_matches(&file, &head.identity)?
            || head.revision == 0
            || head.revision > i64::MAX as u64
            || !openwebide_core::workspace_transaction::valid_id(id)
        {
            return Err(INVALID.into());
        }
        Ok(Some(head))
    }
    pub(super) fn needs_recovery(&self) -> Result<bool, String> {
        let Some(head) = self.head()? else {
            return Ok(true);
        };
        if head.phase == Phase::Prepared {
            return Ok(true);
        }
        match self.directory.symlink_metadata(&head.snapshot.name) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            _ => Ok(true),
        }
    }
    fn seal(&self, parent: &Dir, anchor: &Anchor) -> Result<(), String> {
        self.original_database(parent, anchor)?;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
        let mut file = io(parent.open_with(ANCHOR, &options))?.into_std();
        private(&file, self.key.uid, false)?;
        if !original_matches(&file, &self.key.anchor)? {
            return Err(INVALID.into());
        }
        let data = bytes(&file, MAX_ANCHOR_BYTES)?;
        let mut stream = serde_json::Deserializer::from_slice(&data).into_iter::<Anchor>();
        let current = stream.next().ok_or(INVALID)?.map_err(|_| INVALID)?;
        if current.key != anchor.key
            || current.database != anchor.database
            || current.digest != anchor.digest
            || current.format != anchor.format
        {
            return Err(INVALID.into());
        }
        let trailing = &data[stream.byte_offset()..];
        if !SEAL.starts_with(trailing) {
            return Err(INVALID.into());
        }
        io(file.seek(SeekFrom::End(0)))?;
        io(file.write_all(&SEAL[trailing.len()..]))?;
        io(file.sync_all())?;
        io(crate::native_directory::sync(parent))
    }
    fn quota(&self, extra: u64) -> Result<(), String> {
        let mut total = extra;
        let mut count = 0;
        for entry in io(self.directory.entries())? {
            let entry = io(entry)?;
            let metadata = io(entry.metadata())?;
            count += 1;
            total = total.checked_add(metadata.len()).ok_or(INVALID)?;
            if count >= MAX_FILES || total > MAX_PRIVATE_BYTES || !metadata.is_file() {
                return Err(
                    "Native recovery private storage needs explicit repair before another update"
                        .into(),
                );
            }
        }
        Ok(())
    }
    fn create(&self, name: &str) -> Result<File, String> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
        let file = io(self.directory.open_with(name, &options))?.into_std();
        private(&file, self.key.uid, false)?;
        Ok(file)
    }
    fn publish(&self, mut head: Head) -> Result<(), String> {
        self.quota(MAX_ANCHOR_BYTES)?;
        let name = format!("head-{:032x}.json", rand::random::<u128>());
        let mut file = self.create(&name)?;
        head.identity = self.original(&file)?;
        let data = serde_json::to_vec(&head).map_err(|_| INVALID)?;
        if data.len() as u64 > MAX_ANCHOR_BYTES {
            return Err(INVALID.into());
        }
        io(file.write_all(&data))?;
        io(file.sync_all())?;
        // The temporary file's own identity is part of the record, so copying
        // an identical-looking head into this name cannot authorize recovery.
        io(self.directory.rename(&name, &self.directory, HEAD))?;
        io(crate::native_directory::sync(&self.directory))
    }
    pub(super) fn validate_committed(&self, data: &[u8]) -> Result<Connection, String> {
        let head = self.head()?.ok_or(INVALID)?;
        let (connection, revision) = database(data, &self.key)?;
        if head.phase != Phase::Committed
            || digest(data) != head.digest
            || revision != head.revision
        {
            return Err(INVALID.into());
        }
        Ok(connection)
    }
    fn remove_snapshot(&self, snapshot: &Snapshot) -> Result<(), String> {
        match self.directory.symlink_metadata(&snapshot.name) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Ok(metadata) if metadata.is_file() => {}
            _ => return Err(INVALID.into()),
        }
        let file = child(&self.directory, &snapshot.name, self.key.uid)?;
        if !original_matches(&file, &snapshot.identity)? {
            return Err(INVALID.into());
        }
        io(self.directory.remove_file(&snapshot.name))?;
        io(crate::native_directory::sync(&self.directory))
    }
    fn rewrite(
        &self,
        parent: &Dir,
        anchor: &Anchor,
        data: &[u8],
        barrier: &impl Fn(Barrier) -> Result<(), String>,
    ) -> Result<(), String> {
        // Validate against the retained objects before obtaining a writable
        // handle. Never create or truncate the database during this open.
        let observed = self.original_database(parent, anchor)?;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
        let mut file = io(self.directory.open_with(DATABASE, &options))?.into_std();
        private(&file, self.key.uid, false)?;
        if self.original(&file)? != self.original(&observed)? {
            return Err(INVALID.into());
        }
        let middle = data.len() / 2;
        io(file.write_all(&data[..middle]))?;
        barrier(Barrier::PartialDatabase)?;
        io(file.write_all(&data[middle..]))?;
        io(file.set_len(data.len() as u64))?;
        io(file.sync_all())?;
        barrier(Barrier::Database)?;
        self.original_database(parent, anchor)?;
        Ok(())
    }
    pub(super) fn persist(
        &self,
        parent: &Dir,
        anchor: &Anchor,
        data: &[u8],
        barrier: impl Fn(Barrier) -> Result<(), String>,
    ) -> Result<(), String> {
        let current = self.original_database(parent, anchor)?;
        let old = bytes(&current, MAX_DATABASE_BYTES)?;
        let old_revision = database(&old, &self.key)?.1;
        match self.head()? {
            None if !anchor.sealed && digest(&old) == anchor.digest && old_revision == 0 => {}
            Some(head)
                if head.phase == Phase::Committed
                    && head.digest == digest(&old)
                    && head.revision == old_revision => {}
            _ => return Err(INVALID.into()),
        }
        if data.len() as u64 > MAX_DATABASE_BYTES {
            return Err(INVALID.into());
        }
        let revision = database(data, &self.key)?.1;
        if revision != old_revision.checked_add(1).ok_or(INVALID)? {
            return Err(INVALID.into());
        }
        self.quota(data.len() as u64 + MAX_ANCHOR_BYTES * 2)?;
        let name = format!("snapshot-{:032x}.sqlite3", rand::random::<u128>());
        let mut file = self.create(&name)?;
        io(file.write_all(data))?;
        io(file.sync_all())?;
        io(crate::native_directory::sync(&self.directory))?;
        let snapshot = Snapshot {
            name,
            identity: self.original(&file)?,
        };
        barrier(Barrier::Snapshot)?;
        self.original_database(parent, anchor)?;
        let head = || Head {
            format: 1,
            key: self.key.clone(),
            identity: self.key.anchor.clone(),
            revision,
            digest: digest(data),
            snapshot: snapshot.clone(),
            phase: Phase::Prepared,
        };
        self.publish(head())?;
        barrier(Barrier::Prepared)?;
        self.seal(parent, anchor)?;
        self.rewrite(parent, anchor, data, &barrier)?;
        let mut committed = head();
        committed.phase = Phase::Committed;
        self.publish(committed)?;
        barrier(Barrier::Committed)?;
        self.validate(parent, anchor)?;
        self.remove_snapshot(&snapshot)
    }
    fn migrate(
        &self,
        parent: &Dir,
        anchor: &Anchor,
        file: &File,
        barrier: impl Fn(Barrier) -> Result<(), String>,
    ) -> Result<(), String> {
        let data = bytes(file, MAX_DATABASE_BYTES)?;
        if anchor.sealed || digest(&data) != anchor.digest {
            return Err(INVALID.into());
        }
        let (connection, revision) = database(&data, &self.key)?;
        if revision != 0 {
            return Err(INVALID.into());
        }
        connection.execute_batch("BEGIN IMMEDIATE; CREATE TABLE recovery_revision (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL CHECK(revision>0)); INSERT INTO recovery_revision VALUES (1,1); PRAGMA user_version=2; COMMIT;").map_err(|_| INVALID)?;
        let data = connection.serialize("main").map_err(|_| INVALID)?;
        self.persist(parent, anchor, &data, barrier)
    }
    pub(super) fn recover(&self, parent: &Dir, anchor: &Anchor) -> Result<(), String> {
        let file = self.original_database(parent, anchor)?;
        match self.head()? {
            Some(mut head) if head.phase == Phase::Prepared => {
                let file = child(&self.directory, &head.snapshot.name, self.key.uid)?;
                if !original_matches(&file, &head.snapshot.identity)? {
                    return Err(INVALID.into());
                }
                let data = bytes(&file, MAX_DATABASE_BYTES)?;
                if digest(&data) != head.digest || database(&data, &self.key)?.1 != head.revision {
                    return Err(INVALID.into());
                }
                self.seal(parent, anchor)?;
                self.rewrite(parent, anchor, &data, &|_| Ok(()))?;
                head.phase = Phase::Committed;
                self.publish(head)?;
                self.validate(parent, anchor)?;
                self.remove_snapshot(&self.head()?.ok_or(INVALID)?.snapshot)
            }
            Some(head) => {
                self.validate(parent, anchor)?;
                self.remove_snapshot(&head.snapshot)
            }
            None => self.migrate(parent, anchor, &file, |_| Ok(())),
        }
    }
}
#[cfg(test)]
mod tests;
