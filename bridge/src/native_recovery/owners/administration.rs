//! Explicit OS-owner retirement is independent of a deleted app account or issuer.
//! It never fabricates a command result or certifies unconfirmed process cleanup.
use super::*;
use openwebide_core::{
    WorkspaceMode,
    plugins::languages::intelligence::actions::{CommandOutcome, MAX_COMMAND_BYTES},
};
const MAX_RETIRED: usize = 1024;
const STALE: &str = "Native recovery changed; inspect the original record again before retirement";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub(super) id: String,
    pub(super) expected: String,
    pub(super) kind: String,
    pub(super) folder: Option<String>,
    pub(super) user: Option<i64>,
    pub(super) project: Option<i64>,
    pub(super) transaction: Option<String>,
    pub(super) mode: Option<WorkspaceMode>,
    pub(super) files: Option<String>,
    pub(super) command_stage: Option<String>,
    pub(super) outcome: Option<CommandOutcome>,
    pub(super) outcome_digest: Option<String>,
    pub(super) retirement_blocked: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Retired {
    format: u32,
    original: Entry,
    id: String,
    binding_digest: String,
    expected: String,
    note: String,
    recorded_at: u64,
}
fn note_valid(note: &str) -> bool {
    !note.trim().is_empty() && note.len() <= 4096 && !note.contains('\0')
}
pub(super) fn record_digest(value: &impl Serialize) -> Result<String, String> {
    Ok(openwebide_core::plugins::content_digest(
        &serde_json::to_vec(value).map_err(|_| INVALID)?,
    ))
}
fn read_retired(connection: &Connection, schema: u32) -> Result<Vec<Retired>, String> {
    if schema < 10 {
        let count: u32 = connection.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='recovery_retired_owners'", [], |row| row.get(0)).map_err(|_| INVALID)?;
        return if count == 0 {
            Ok(vec![])
        } else {
            Err(INVALID.into())
        };
    }
    let mut query = connection
        .prepare("SELECT id,record FROM recovery_retired_owners LIMIT 1025")
        .map_err(|_| INVALID)?;
    let mut rows = query.query([]).map_err(|_| INVALID)?;
    let mut result = vec![];
    while let Some(row) = rows.next().map_err(|_| INVALID)? {
        let id: String = row.get(0).map_err(|_| INVALID)?;
        let text: String = row.get(1).map_err(|_| INVALID)?;
        if result.len() >= MAX_RETIRED || text.len() > MAX_COMMAND_BYTES + 128 * 1024 {
            return Err(INVALID.into());
        }
        let retired: Retired = serde_json::from_str(&text).map_err(|_| INVALID)?;
        if retired.format != 1
            || retired.original.id != id
            || retired.original.expected != retired.expected
            || !matches!(retired.original.kind.as_str(), "owner" | "settlement")
            || retired.original.retirement_blocked.is_some()
            || retired.id != id
            || !fingerprint(&id)
            || !fingerprint(&retired.binding_digest)
            || !fingerprint(&retired.expected)
            || !note_valid(&retired.note)
        {
            return Err(INVALID.into());
        }
        if let Some(outcome) = &retired.original.outcome {
            outcome.validate()?;
            if retired.original.outcome_digest.as_ref() != Some(&record_digest(outcome)?) {
                return Err(INVALID.into());
            }
        }
        if retired
            .original
            .outcome_digest
            .as_ref()
            .is_some_and(|digest| !fingerprint(digest))
        {
            return Err(INVALID.into());
        }
        result.push(retired);
    }
    Ok(result)
}
pub(super) fn validate(
    connection: &Connection,
    schema: u32,
    owners: &[Owner],
) -> Result<(), String> {
    for retired in read_retired(connection, schema)? {
        for owner in owners {
            if owner.id == retired.id || record_digest(&owner.binding)? == retired.binding_digest {
                return Err(INVALID.into());
            }
        }
        let pending: u32 = connection
            .query_row(
                "SELECT count(*) FROM recovery_command_settlements WHERE id=?1",
                [&retired.id],
                |row| row.get(0),
            )
            .map_err(|_| INVALID)?;
        if pending != 0 {
            return Err(INVALID.into());
        }
    }
    Ok(())
}
pub(super) fn check_retired(connection: &Connection, owner: &Owner) -> Result<(), String> {
    let binding = record_digest(&owner.binding)?;
    if read_retired(connection, version(connection)?)?
        .iter()
        .any(|r| r.id == owner.id || r.binding_digest == binding)
    {
        return Err("This original workspace operation was retired by its OS owner; start a new transaction".into());
    }
    Ok(())
}
fn owner_entry(
    namespace: &Namespace,
    owner: &Owner,
    command: Option<&commands::CommandRecord>,
) -> Result<Entry, String> {
    let authority = owner.authority()?;
    let blocked = if let Some(command) = command {
        command.retirement_blocked()
    } else if owner.command {
        Some("Legacy command has no original cleanup record".into())
    } else {
        None
    };
    Ok(Entry {
        id: owner.id.clone(),
        expected: record_digest(&(&namespace.key.nonce, "owner", owner, command))?,
        kind: "owner".into(),
        folder: Some(owner.folder.path().to_string_lossy().into_owned()),
        user: authority.as_ref().map(|a| a.user),
        project: authority.as_ref().map(|a| a.project),
        transaction: authority.as_ref().map(|a| a.transaction.clone()),
        mode: authority.as_ref().map(|a| a.root.mode),
        files: Some(
            match owner.files {
                FileStage::Unknown => "unknown",
                FileStage::Reserved => "reserved",
                FileStage::Admitted => "admitted",
                FileStage::Finished => "finished",
            }
            .into(),
        ),
        command_stage: command.map(|c| format!("{:?}", c.stage).to_lowercase()),
        outcome: command.and_then(|c| c.outcome.clone()),
        outcome_digest: command
            .and_then(|c| c.outcome.as_ref())
            .map(record_digest)
            .transpose()?,
        retirement_blocked: blocked,
    })
}
pub(crate) fn inspect(namespace: &Namespace) -> Result<serde_json::Value, String> {
    namespace.access(false, |_, _, connection| {
        let schema = version(connection)?;
        let commands = commands::read(connection, schema)?;
        let owners = read(connection, schema)?.iter().map(|owner| owner_entry(namespace, owner, commands.iter().find(|c| c.owner == owner.id))).collect::<Result<Vec<_>, _>>()?;
        let settlements = commands::administrative_settlements(connection, schema, &namespace.key.nonce)?;
        Ok(serde_json::json!({"owners": owners, "settlements": settlements, "retired": read_retired(connection, schema)?}))
    })
}
fn retire_locked(
    namespace: &Namespace,
    folder: Option<&Folder>,
    id: &str,
    expected: &str,
    note: &str,
) -> Result<serde_json::Value, String> {
    namespace.access(true, |parent, anchor, connection| {
        let schema = version(connection)?;
        let retired = read_retired(connection, schema)?;
        if let Some(previous) = retired.iter().find(|r| r.id == id) {
            return if previous.expected == expected && previous.note == note {
                serde_json::to_value(previous).map_err(|_| INVALID.into())
            } else { Err(STALE.into()) };
        }
        if retired.len() >= MAX_RETIRED { return Err("Native recovery retirement audit capacity is full; retain its original state".into()); }
        let owner = read(connection, schema)?.into_iter().find(|o| o.id == id);
        let (entry, binding_digest) = if let Some(owner) = owner {
            let folder = folder.ok_or(STALE)?;
            if namespace.original_folder(folder, true)? != owner.folder { return Err(STALE.into()); }
            let commands = commands::read(connection, schema)?;
            let entry = owner_entry(namespace, &owner, commands.iter().find(|c| c.owner == id))?;
            (entry, record_digest(&owner.binding)?)
        } else {
            let entry = commands::administrative_settlements(connection, schema, &namespace.key.nonce)?.into_iter().find(|e| e.id == id).ok_or(STALE)?;
            let binding = commands::retirement_binding(connection, schema, id)?;
            (entry, binding)
        };
        if entry.expected != expected { return Err(STALE.into()); }
        if let Some(message) = &entry.retirement_blocked { return Err(message.clone()); }
        let record = Retired { format: 1, original: entry.clone(), id: id.into(), binding_digest, expected: expected.into(), note: note.into(), recorded_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_| INVALID)?.as_secs() };
        // An existing metadata table is required by schema 9 validation, even
        // when this older owner never had a callback acknowledgement outbox.
        connection.execute_batch("CREATE TABLE IF NOT EXISTS recovery_commands(id TEXT PRIMARY KEY,record TEXT NOT NULL); CREATE TABLE IF NOT EXISTS recovery_command_settlements(id TEXT PRIMARY KEY,record TEXT NOT NULL); CREATE TABLE IF NOT EXISTS recovery_retired_owners(id TEXT PRIMARY KEY,record TEXT NOT NULL);").map_err(|_| INVALID)?;
        if schema < 10 { connection.execute_batch("PRAGMA user_version=10;").map_err(|_| INVALID)?; }
        connection.execute("INSERT INTO recovery_retired_owners(id,record) VALUES(?1,?2)", [id, &serde_json::to_string(&record).map_err(|_| INVALID)?]).map_err(|_| INVALID)?;
        if entry.kind == "owner" {
            if schema >= 4 { connection.execute("DELETE FROM recovery_commands WHERE id=?1", [id]).map_err(|_| INVALID)?; }
            connection.execute("DELETE FROM recovery_owners WHERE id=?1", [id]).map_err(|_| INVALID)?;
        } else {
            connection.execute("DELETE FROM recovery_command_settlements WHERE id=?1", [id]).map_err(|_| INVALID)?;
        }
        namespace.save(parent, anchor, connection)?;
        serde_json::to_value(record).map_err(|_| INVALID.into())
    })
}
pub(crate) fn retire(
    namespace: &Namespace,
    id: &str,
    expected: &str,
    note: &str,
) -> Result<serde_json::Value, String> {
    if !fingerprint(id) || !fingerprint(expected) || !note_valid(note) {
        return Err("Provide the inspected owner ID, exact expected digest, and a review note of 1–4096 bytes".into());
    }
    let original = namespace.access(false, |_, _, connection| {
        Ok(read(connection, version(connection)?)?
            .into_iter()
            .find(|o| o.id == id))
    })?;
    if let Some(owner) = original {
        let folder = owner.folder.reopen()?;
        folder.with_exclusive_access(|| retire_locked(namespace, Some(&folder), id, expected, note))
    } else {
        // A settlement outbox already has no folder claim; retirement discards
        // only its metadata capability. It never certifies a live command.
        retire_locked(namespace, None, id, expected, note)
    }
}
