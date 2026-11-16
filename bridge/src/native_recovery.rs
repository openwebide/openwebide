//! Anchored native ownership and durable file/command recovery.
//! Original credentials, folders and process receipts fence mutations.
use crate::{file_identity::Identity, folder_coordination::Folder};
use cap_std::fs::{Dir, DirBuilder, DirBuilderExt, OpenOptions, OpenOptionsExt};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::{
    ffi::{CStr, OsStr},
    fs::{File, TryLockError},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
};

pub(crate) const DIRECTORY: &str = openwebide_core::vfs::NATIVE_RECOVERY_DIRECTORY;
pub(crate) const ANCHOR: &str = openwebide_core::vfs::NATIVE_RECOVERY_OWNER;
pub(crate) const DATABASE: &str = "recovery.sqlite3";
mod location;
mod locking;
mod owners;
pub(crate) use owners::administration;
mod persistence;
#[cfg(target_os = "macos")]
pub(crate) mod upgrade;
pub(crate) use owners::{
    CommandIntent, PendingCommand, PendingCommandSettlement, Permit, ProcessCleanup,
};

const MAX_ANCHOR_BYTES: u64 = 8192;
const MAX_DATABASE_BYTES: u64 = 16 * 1024 * 1024;
const INVALID: &str =
    "Native recovery namespace is incomplete or replaced; restore its original private state";
const BUSY: &str = "Native recovery namespace is busy; retry before changing workspace files";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Key {
    uid: u32,
    nonce: String,
    home: Identity,
    directory: Identity,
    anchor: Identity,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Anchor {
    format: u32,
    key: Key,
    database: Identity,
    digest: String,
    #[serde(skip)]
    sealed: bool,
}

pub(crate) struct Namespace {
    home: Folder,
    directory: Dir,
    anchor: File,
    key: Key,
}

/// Use trusted OS installation policy or the account directory, never profiles.
pub(crate) fn home() -> Result<PathBuf, String> {
    // SAFETY: geteuid has no preconditions and does not return borrowed memory.
    let uid = unsafe { libc::geteuid() };
    if let Some(directory) = location::configured(uid)? {
        return Ok(directory);
    }
    let mut capacity = 4096;
    loop {
        let mut bytes = vec![0u8; capacity];
        let mut account = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        // SAFETY: All output storage remains live for the call. The returned
        // string is copied before its backing buffer is dropped below.
        let status = unsafe {
            libc::getpwuid_r(
                uid,
                account.as_mut_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                &mut result,
            )
        };
        if status == libc::ERANGE && capacity < 1024 * 1024 {
            capacity *= 2;
            continue;
        }
        if status != 0 || result.is_null() {
            return Err("The native OS account directory is unavailable".into());
        }
        // SAFETY: Successful getpwuid_r initialized account and its strings.
        let account = unsafe { account.assume_init() };
        if account.pw_uid != uid || account.pw_dir.is_null() {
            return Err(INVALID.into());
        }
        // SAFETY: pw_dir is a NUL-terminated string backed by the live buffer.
        let directory = unsafe { CStr::from_ptr(account.pw_dir) };
        let path = PathBuf::from(OsStr::from_bytes(directory.to_bytes()));
        if !path.is_absolute() {
            return Err(INVALID.into());
        }
        return Ok(path);
    }
}

fn io<T>(result: std::io::Result<T>) -> Result<T, String> {
    result.map_err(|_| INVALID.into())
}
fn original(file: &File) -> Result<Identity, String> {
    let identity = io(Identity::of(file))?;
    if !identity.has_birth_time() || !identity.has_persistent_volume() {
        return Err("Native recovery requires persistent original-object observations on its private filesystem".into());
    }
    Ok(identity)
}
fn original_matches(file: &File, expected: &Identity) -> Result<bool, String> {
    Ok(expected.valid() && expected.has_birth_time() && io(expected.matches(file))?)
}
fn private(file: &File, uid: u32, directory: bool) -> Result<(), String> {
    let metadata = io(file.metadata())?;
    if metadata.uid() != uid
        || metadata.is_dir() != directory
        || (!directory && (!metadata.is_file() || metadata.nlink() != 1))
        || metadata.mode() & 0o777 != if directory { 0o700 } else { 0o600 }
    {
        return Err(INVALID.into());
    }
    Ok(())
}
fn root(folder: &Folder) -> Result<Dir, String> {
    io(Dir::reopen_dir(folder.handle.as_file()))
}
fn child(parent: &Dir, name: &str, uid: u32) -> Result<File, String> {
    if !io(parent.symlink_metadata(name))?.is_file() {
        return Err(INVALID.into());
    }
    // A concurrent type change must not turn a metadata read into a blocking
    // FIFO open. This flag does not affect reads of the required regular files.
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(libc::O_NONBLOCK);
    let file = io(parent.open_with(name, &options))?.into_std();
    private(&file, uid, false)?;
    Ok(file)
}
fn bytes(mut file: &File, limit: u64) -> Result<Vec<u8>, String> {
    io(file.seek(SeekFrom::Start(0)))?;
    let mut result = Vec::new();
    io(file.take(limit + 1).read_to_end(&mut result))?;
    if result.is_empty() || result.len() as u64 > limit {
        return Err(INVALID.into());
    }
    Ok(result)
}
// Append-only seal: even a partial append means a head is required forever.
// A rollback to the initial SQLite bytes cannot become a new empty registry.
const SEAL: &[u8] = b"\nnative-recovery-mutable-v1\n";
fn anchor_record(file: &File) -> Result<Anchor, String> {
    let data = bytes(file, MAX_ANCHOR_BYTES)?;
    let mut stream = serde_json::Deserializer::from_slice(&data).into_iter::<Anchor>();
    let mut record = stream.next().ok_or(INVALID)?.map_err(|_| INVALID)?;
    let trailing = &data[stream.byte_offset()..];
    if !SEAL.starts_with(trailing) {
        return Err(INVALID.into());
    }
    record.sealed = !trailing.is_empty();
    Ok(record)
}
fn lock(file: &File, exclusive: bool) -> Result<(), String> {
    // Publishing holds this only for synchronous bounded I/O. Brief contention
    // must not invalidate another workspace's recovery predicate; stuck holders
    // still fail closed within a bounded wait.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(512);
    let _turn = locking::turn(file, deadline, || {})?;
    loop {
        let result = if exclusive {
            file.try_lock()
        } else {
            file.try_lock_shared()
        };
        match result {
            Ok(()) => return Ok(()),
            Err(TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(
                    deadline
                        .saturating_duration_since(std::time::Instant::now())
                        .min(std::time::Duration::from_millis(2)),
                );
            }
            Err(TryLockError::WouldBlock) => return Err(BUSY.into()),
            Err(TryLockError::Error(_)) => return Err(INVALID.into()),
        }
    }
}

fn sqlite(key: &Key) -> Result<Vec<u8>, String> {
    let connection = Connection::open_in_memory().map_err(|_| INVALID)?;
    connection.execute_batch("PRAGMA page_size=4096; PRAGMA journal_mode=MEMORY; PRAGMA user_version=1; CREATE TABLE recovery_namespace (id INTEGER PRIMARY KEY CHECK(id=1), key TEXT NOT NULL);").map_err(|_| INVALID)?;
    let key = serde_json::to_string(key).map_err(|_| INVALID)?;
    connection
        .execute("INSERT INTO recovery_namespace VALUES (1, ?1)", [&key])
        .map_err(|_| INVALID)?;
    let serialized = connection.serialize("main").map_err(|_| INVALID)?;
    if serialized.len() as u64 > MAX_DATABASE_BYTES {
        return Err(INVALID.into());
    }
    Ok(serialized.to_vec())
}
fn database(data: &[u8], expected: &Key) -> Result<(Connection, u64), String> {
    let mut connection = Connection::open_in_memory().map_err(|_| INVALID)?;
    // Read the bounded original file completely before deserialization. SQLite
    // never opens an ambient database pathname or journal, including during an
    // ABA replacement of that pathname. Cursor has exactly the requested bytes.
    connection
        .deserialize_read_exact("main", std::io::Cursor::new(data), data.len(), false)
        .map_err(|_| INVALID)?;
    connection
        .execute_batch("PRAGMA trusted_schema=OFF")
        .map_err(|_| INVALID)?;
    let integrity: String = connection
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|_| INVALID)?;
    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|_| INVALID)?;
    let count: u32 = connection
        .query_row("SELECT count(*) FROM recovery_namespace", [], |row| {
            row.get(0)
        })
        .map_err(|_| INVALID)?;
    let key: String = connection
        .query_row("SELECT key FROM recovery_namespace WHERE id=1", [], |row| {
            row.get(0)
        })
        .map_err(|_| INVALID)?;
    let key: Key = serde_json::from_str(&key).map_err(|_| INVALID)?;
    if integrity != "ok" || !matches!(version, 1..=12) || count != 1 || key != *expected {
        return Err(INVALID.into());
    }
    let revision = if version == 1 {
        0
    } else {
        let (count, revision): (u32, i64) = connection
            .query_row(
                "SELECT count(*), min(revision) FROM recovery_revision WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| INVALID)?;
        if count != 1 || revision <= 0 {
            return Err(INVALID.into());
        }
        u64::try_from(revision).map_err(|_| INVALID)?
    };
    owners::validate(&connection, version)?;
    Ok((connection, revision))
}
impl Namespace {
    fn original(&self, file: &File) -> Result<Identity, String> {
        if self.key.home.uses_device_number() {
            let identity = io(Identity::legacy(file))?;
            if !identity.has_birth_time() {
                return Err(INVALID.into());
            }
            Ok(identity)
        } else {
            original(file)
        }
    }

    pub(crate) fn open(home: &Path) -> Result<Self, String> {
        Self::open_with_initialization(home, true)
    }
    pub(crate) fn open_existing(home: &Path) -> Result<Self, String> {
        Self::open_with_initialization(home, false)
    }
    fn open_with_initialization(home: &Path, initialize: bool) -> Result<Self, String> {
        let home = Folder::capture_state(home)?;
        let parent = root(&home)?;
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        if io(home.handle.as_file().metadata())?.uid() != uid {
            return Err(INVALID.into());
        }
        #[cfg(target_os = "macos")]
        upgrade::ensure_idle(&parent)?;
        let existing = match parent.symlink_metadata(ANCHOR) {
            Ok(metadata) => {
                if !metadata.is_file() {
                    return Err(INVALID.into());
                }
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => return Err(INVALID.into()),
        };
        let anchor = if existing {
            child(&parent, ANCHOR, uid)?
        } else {
            if !initialize {
                return Err("No original native recovery namespace exists".into());
            }
            // An existing registry without its external original anchor is not
            // a first run. Do not create a new, apparently empty namespace.
            match parent.symlink_metadata(DIRECTORY) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err(INVALID.into()),
            }
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true).mode(0o600);
            match parent.open_with(ANCHOR, &options) {
                Ok(file) => file.into_std(),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Err(BUSY.into());
                }
                Err(_) => return Err(INVALID.into()),
            }
        };
        lock(&anchor, !existing)?;
        #[cfg(target_os = "macos")]
        upgrade::ensure_idle(&parent)?;
        if !existing {
            // Once the external anchor name is durable, interruption can never
            // silently turn a partial initialization into a clean first run.
            io(crate::native_directory::sync(&parent))?;
            Self::initialize(&home, &parent, &anchor, uid)?;
        }
        let mut record = anchor_record(&anchor)?;
        let directory = Self::directory(&parent, &record.key)?;
        let namespace = Self {
            home,
            directory,
            anchor,
            key: record.key.clone(),
        };
        if !existing || namespace.needs_recovery()? {
            if existing {
                // Normal readers share the lock. Positive replay or migration
                // must reacquire exclusive ownership, then re-read its authority.
                io(namespace.anchor.unlock())?;
                lock(&namespace.anchor, true)?;
                record = anchor_record(&namespace.anchor)?;
            }
            namespace.recover(&parent, &record)?;
        }
        namespace.validate(&parent, &record)?;
        io(namespace.anchor.unlock())?;
        Ok(namespace)
    }
    fn initialize(home: &Folder, parent: &Dir, anchor: &File, uid: u32) -> Result<(), String> {
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        io(parent.create_dir_with(DIRECTORY, &builder))?;
        let directory = io(parent.open_dir(DIRECTORY))?;
        let key = Key {
            uid,
            nonce: format!("{:032x}", rand::random::<u128>()),
            home: original(home.handle.as_file())?,
            directory: original(&io(directory.try_clone())?.into_std_file())?,
            anchor: original(anchor)?,
        };
        let data = sqlite(&key)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true).mode(0o600);
        let mut file = io(directory.open_with(DATABASE, &options))?.into_std();
        io(file.write_all(&data))?;
        io(file.sync_all())?;
        let record = Anchor {
            format: 1,
            key,
            database: original(&file)?,
            digest: openwebide_core::plugins::content_digest(&data),
            sealed: false,
        };
        io(crate::native_directory::sync(&directory))?;
        let data = serde_json::to_vec(&record).map_err(|_| INVALID)?;
        if (data.len() + SEAL.len()) as u64 > MAX_ANCHOR_BYTES {
            return Err(INVALID.into());
        }
        let mut output = anchor;
        io(output.write_all(&data))?;
        io(anchor.sync_all())?;
        io(crate::native_directory::sync(parent))?;
        Ok(())
    }
    fn directory(parent: &Dir, key: &Key) -> Result<Dir, String> {
        if !io(parent.symlink_metadata(DIRECTORY))?.is_dir() {
            return Err(INVALID.into());
        }
        let directory = io(parent.open_dir(DIRECTORY))?;
        let file = io(directory.try_clone())?.into_std_file();
        private(&file, key.uid, true)?;
        if !original_matches(&file, &key.directory)? {
            return Err(INVALID.into());
        }
        Ok(directory)
    }
    fn original_database(&self, parent: &Dir, record: &Anchor) -> Result<File, String> {
        self.original_database_at(parent, parent, record)
    }
    fn original_database_at(
        &self,
        anchor_parent: &Dir,
        directory_parent: &Dir,
        record: &Anchor,
    ) -> Result<File, String> {
        if record.format != 1
            || record.key != self.key
            || !self.home.current()
            || !openwebide_core::workspace_transaction::valid_id(&self.key.nonce)
            || io(self.home.handle.as_file().metadata())?.uid() != self.key.uid
            || !original_matches(self.home.handle.as_file(), &self.key.home)?
            || !original_matches(&self.anchor, &self.key.anchor)?
            || !original_matches(
                &io(self.directory.try_clone())?.into_std_file(),
                &self.key.directory,
            )?
        {
            return Err(INVALID.into());
        }
        // Check current names against retained original objects. Copies with
        // identical database bytes or an identical anchor are replacements.
        let anchor = child(anchor_parent, ANCHOR, self.key.uid)?;
        if !original_matches(&anchor, &self.key.anchor)? {
            return Err(INVALID.into());
        }
        let directory = Self::directory(directory_parent, &self.key)?;
        let file = child(&directory, DATABASE, self.key.uid)?;
        if !original_matches(&file, &record.database)? {
            return Err(INVALID.into());
        }
        Ok(file)
    }
    fn validate(&self, parent: &Dir, record: &Anchor) -> Result<(), String> {
        self.validated_database(parent, record).map(|_| ())
    }
    fn validated_database(&self, parent: &Dir, record: &Anchor) -> Result<Connection, String> {
        self.validated_database_at(parent, parent, record)
    }
    fn validated_database_at(
        &self,
        anchor_parent: &Dir,
        directory_parent: &Dir,
        record: &Anchor,
    ) -> Result<Connection, String> {
        let current = anchor_record(&child(anchor_parent, ANCHOR, self.key.uid)?)?;
        if !current.sealed
            || current.key != record.key
            || current.database != record.database
            || current.digest != record.digest
            || current.format != record.format
        {
            return Err(INVALID.into());
        }
        let file = self.original_database_at(anchor_parent, directory_parent, &current)?;
        let connection = self.validate_committed(&bytes(&file, MAX_DATABASE_BYTES)?)?;
        // An earlier publisher may have lost its acknowledgement after rename.
        // Observing a committed head alone does not prove its directory flush.
        io(crate::native_directory::sync(&self.directory))?;
        Ok(connection)
    }
    pub(crate) fn ensure_current(&self) -> Result<(), String> {
        let parent = root(&self.home)?;
        let anchor = child(&parent, ANCHOR, self.key.uid)?;
        lock(&anchor, false)?;
        #[cfg(target_os = "macos")]
        upgrade::ensure_idle(&parent)?;
        let record = anchor_record(&anchor)?;
        self.validate(&parent, &record)
    }
}
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct TestHome(pub(crate) std::sync::Arc<PathBuf>);
#[cfg(test)]
impl Default for TestHome {
    fn default() -> Self {
        Self(testing_home())
    }
}
#[cfg(test)]
pub(crate) fn testing_home() -> std::sync::Arc<PathBuf> {
    // Each libtest entry has its own thread. Keep deliberate unfinished owners
    // in that test's namespace rather than poisoning unrelated fixtures. Runtime
    // objects capture this value when constructed, before moving across threads.
    thread_local! { static HOME: std::sync::Arc<PathBuf> = {
        let home = tempfile::Builder::new()
            .prefix("openwebide-native-recovery-tests-")
            .tempdir()
            .unwrap()
            .keep();
        Namespace::open(&home).unwrap();
        std::sync::Arc::new(home)
    }; }
    HOME.with(std::sync::Arc::clone)
}
#[cfg(test)]
mod tests;
