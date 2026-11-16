//! Explicit inspection of an empty legacy namespace before a volume upgrade.
//! Candidate observations never authorize ordinary recovery or initialization.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

mod inspection;
mod plan;
mod storage;
mod transaction;
pub(crate) use storage::ensure_idle;
pub(crate) use transaction::{cancel, execute};

const UPGRADE_INVALID: &str =
    "Legacy recovery state cannot be safely upgraded; preserve its original files";
const RECORDS: [&str; 4] = [
    "recovery_owners",
    "recovery_commands",
    "recovery_command_settlements",
    "recovery_retired_owners",
];

#[derive(Serialize, Deserialize)]
pub(crate) struct Inspection {
    pub eligible: bool,
    pub expected: Option<String>,
    pub reason: String,
    pub schema: u32,
    pub revision: u64,
    pub records: BTreeMap<String, u64>,
    pub device_changed: bool,
    pub phase: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    home: Vec<u8>,
    observations: BTreeMap<String, Identity>,
    hashes: BTreeMap<String, String>,
    schema: u32,
    revision: u64,
    records: BTreeMap<String, u64>,
}

struct Legacy {
    home: Folder,
    directory: Dir,
    anchor: File,
    snapshot: Snapshot,
    device_changed: bool,
}

pub(crate) fn inspect(home: &Path) -> Result<Inspection, String> {
    if let Some(report) = transaction::status(home)? {
        return Ok(report);
    }
    capture(home, false)?.report()
}

fn capture(home: &Path, exclusive: bool) -> Result<Legacy, String> {
    let home = Folder::capture_state(home)?;
    let parent = root(&home)?;
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    if io(home.handle.as_file().metadata())?.uid() != uid {
        return Err(UPGRADE_INVALID.into());
    }
    let anchor = child(&parent, ANCHOR, uid)?;
    lock(&anchor, exclusive)?;
    let record = anchor_record(&anchor)?;
    if record.format != 1
        || record.key.uid != uid
        || !record.key.home.uses_device_number()
        || !openwebide_core::workspace_transaction::valid_id(&record.key.nonce)
        || !openwebide_core::workspace_transaction::companion::fingerprint(&record.digest)
    {
        return Err(UPGRADE_INVALID.into());
    }
    let mut snapshot = Snapshot {
        home: home.path.as_os_str().as_bytes().into(),
        observations: BTreeMap::new(),
        hashes: BTreeMap::new(),
        schema: 0,
        revision: 0,
        records: BTreeMap::new(),
    };
    observe(
        &mut snapshot,
        "home",
        &record.key.home,
        &record.key.home,
        home.handle.as_file(),
    )?;
    observe(
        &mut snapshot,
        ANCHOR,
        &record.key.anchor,
        &record.key.home,
        &anchor,
    )?;
    snapshot
        .hashes
        .insert(ANCHOR.into(), digest(&bytes(&anchor, MAX_ANCHOR_BYTES)?));
    if !io(parent.symlink_metadata(DIRECTORY))?.is_dir() {
        return Err(UPGRADE_INVALID.into());
    }
    let directory = io(parent.open_dir(DIRECTORY))?;
    let directory_file = io(directory.try_clone())?.into_std_file();
    private(&directory_file, uid, true)?;
    observe(
        &mut snapshot,
        DIRECTORY,
        &record.key.directory,
        &record.key.home,
        &directory_file,
    )?;
    let files = inventory(&directory)?;
    let database_file = child(&directory, DATABASE, uid)?;
    observe(
        &mut snapshot,
        DATABASE,
        &record.database,
        &record.key.home,
        &database_file,
    )?;
    let data = bytes(&database_file, MAX_DATABASE_BYTES)?;
    snapshot.hashes.insert(DATABASE.into(), digest(&data));
    let (connection, revision) = database(&data, &record.key)?;
    snapshot.schema = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|_| UPGRADE_INVALID)?;
    snapshot.revision = revision;
    snapshot.records = inspection::records(&connection)?;
    if revision == 0 {
        if record.sealed
            || files != BTreeSet::from([DATABASE.to_owned()])
            || digest(&data) != record.digest
        {
            return Err(UPGRADE_INVALID.into());
        }
    } else {
        inspection::head(&mut snapshot, &record, &directory, &files, uid)?;
    }
    // Recheck every named object and its bytes while retaining the original
    // shared anchor lock. External replacement cannot turn a read into a plan.
    if !home.current()
        || Identity::of(home.handle.as_file()).map_err(|_| UPGRADE_INVALID)?
            != snapshot.observations["home"]
        || Identity::of(&child(&parent, ANCHOR, uid)?).map_err(|_| UPGRADE_INVALID)?
            != snapshot.observations[ANCHOR]
        || Identity::of(&io(parent.open_dir(DIRECTORY))?.into_std_file())
            .map_err(|_| UPGRADE_INVALID)?
            != snapshot.observations[DIRECTORY]
        || inventory(&directory)? != files
    {
        return Err(UPGRADE_INVALID.into());
    }
    for (name, expected) in &snapshot.hashes {
        let (selected, limit) = if name == ANCHOR {
            (&parent, MAX_ANCHOR_BYTES)
        } else {
            (&directory, MAX_DATABASE_BYTES)
        };
        let file = child(selected, name, uid)?;
        if Identity::of(&file).map_err(|_| UPGRADE_INVALID)? != snapshot.observations[name]
            || digest(&bytes(&file, limit)?) != *expected
        {
            return Err(UPGRADE_INVALID.into());
        }
    }
    let device_changed = !record
        .key
        .home
        .matches(home.handle.as_file())
        .map_err(|_| UPGRADE_INVALID)?;
    Ok(Legacy {
        home,
        directory,
        anchor,
        snapshot,
        device_changed,
    })
}

impl Snapshot {
    fn expected(&self) -> Result<String, String> {
        serde_json::to_vec(self)
            .map(|data| digest(&data))
            .map_err(|_| UPGRADE_INVALID.into())
    }
}
impl Legacy {
    fn report(&self) -> Result<Inspection, String> {
        let eligible = self.snapshot.records.values().all(|count| *count == 0);
        Ok(Inspection {
            eligible,
            expected: eligible.then(|| self.snapshot.expected()).transpose()?,
            reason: if eligible {
                "Empty legacy registry can be upgraded while preserving its original files".into()
            } else {
                "Original records or retirement history must be recovered and preserved before upgrading".into()
            },
            schema: self.snapshot.schema,
            revision: self.snapshot.revision,
            records: self.snapshot.records.clone(),
            device_changed: self.device_changed,
            phase: "legacy".into(),
        })
    }
}

fn digest(data: &[u8]) -> String {
    openwebide_core::plugins::content_digest(data)
}

fn observe(
    snapshot: &mut Snapshot,
    name: &str,
    expected: &Identity,
    original_home: &Identity,
    file: &File,
) -> Result<(), String> {
    if !expected.same_device(original_home) {
        return Err(UPGRADE_INVALID.into());
    }
    let current = expected.legacy_upgrade(file).map_err(|_| UPGRADE_INVALID)?;
    if snapshot
        .observations
        .get("home")
        .is_some_and(|home| !home.same_device(&current))
    {
        return Err(UPGRADE_INVALID.into());
    }
    snapshot.observations.insert(name.into(), current);
    Ok(())
}

fn inventory(directory: &Dir) -> Result<BTreeSet<String>, String> {
    let mut names = BTreeSet::new();
    for entry in io(directory.entries())? {
        let entry = io(entry)?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| UPGRADE_INVALID)?;
        if !matches!(name.as_str(), DATABASE | persistence::HEAD)
            || !io(entry.file_type())?.is_file()
            || !names.insert(name)
        {
            return Err(UPGRADE_INVALID.into());
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests;
