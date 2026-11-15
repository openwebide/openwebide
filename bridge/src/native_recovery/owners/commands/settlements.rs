//! Durable metadata acknowledgements do not retain workspace owner/folder gates.
//! Their exact original private authority is discarded only after its callback ACK.
use super::*;
const MAX_BYTES: usize = 64 * 1024;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Settlement {
    format: u32,
    authority: RecoveryAuthority,
    worker: WorkspaceTransactionWorker,
    origin: String,
    recovery: WorkspaceCommandRecovery,
}
impl Settlement {
    fn validate(&self) -> Result<(), String> {
        self.authority.validate()?;
        self.worker.validate()?;
        super::super::context::validate_origin(&self.origin)?;
        self.recovery.validate_settlement()?;
        if self.format != 1
            || !self.authority.matches_scope(
                self.authority.user,
                self.authority.project,
                &self.recovery.scope,
            )
            || self
                .recovery
                .command
                .worker
                .as_ref()
                .is_none_or(|w| w.id != self.worker.id)
        {
            return Err(INVALID.into());
        }
        Ok(())
    }
    fn matches(&self, other: &Self) -> bool {
        self.format == other.format
            && self.authority == other.authority
            && self.worker.id == other.worker.id
            && self.worker.secret == other.worker.secret
            && self.origin == other.origin
            && self.recovery == other.recovery
    }
}
#[derive(Clone)]
pub(crate) struct PendingCommandSettlement {
    pub id: String,
    pub authority: RecoveryAuthority,
    pub worker: WorkspaceTransactionWorker,
    pub origin: String,
    pub recovery: WorkspaceCommandRecovery,
    key: Key,
}
impl PendingCommandSettlement {
    fn record(&self) -> Settlement {
        Settlement {
            format: 1,
            authority: self.authority.clone(),
            worker: self.worker.clone(),
            origin: self.origin.clone(),
            recovery: self.recovery.clone(),
        }
    }
}
pub(super) fn read(
    connection: &Connection,
    schema: u32,
) -> Result<Vec<(String, Settlement)>, String> {
    if schema < 9 {
        let tables: u32 = connection.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='recovery_command_settlements'", [], |r| r.get(0)).map_err(|_| INVALID)?;
        return if tables == 0 {
            Ok(vec![])
        } else {
            Err(INVALID.into())
        };
    }
    let mut query = connection
        .prepare("SELECT id,record FROM recovery_command_settlements LIMIT 33")
        .map_err(|_| INVALID)?;
    let mut rows = query.query([]).map_err(|_| INVALID)?;
    let mut result = vec![];
    while let Some(row) = rows.next().map_err(|_| INVALID)? {
        let id: String = row.get(0).map_err(|_| INVALID)?;
        let data: String = row.get(1).map_err(|_| INVALID)?;
        if result.len() >= MAX_COMMANDS || !fingerprint(&id) || data.len() > MAX_BYTES {
            return Err(INVALID.into());
        }
        let record: Settlement = serde_json::from_str(&data).map_err(|_| INVALID)?;
        record.validate()?;
        result.push((id, record));
    }
    Ok(result)
}
pub(super) fn retain(
    connection: &Connection,
    owner: &Owner,
    command: &CommandRecord,
    recovery: &WorkspaceCommandRecovery,
) -> Result<(), String> {
    // Legacy trusted embedding has no durable callback. Preserve its existing
    // retirement behavior; application producers always retain their origin.
    let Some(origin) = &owner.origin else {
        return Ok(());
    };
    let record = Settlement {
        format: 1,
        authority: owner.authority()?.ok_or(PENDING)?,
        worker: command.worker.clone(),
        origin: origin.clone(),
        recovery: recovery.clone(),
    };
    record.validate()?;
    let pending = read(connection, version(connection)?)?;
    if let Some((_, existing)) = pending.iter().find(|(id, _)| id == &owner.id) {
        return if existing.matches(&record) {
            Ok(())
        } else {
            Err(PENDING.into())
        };
    }
    if pending.len() >= MAX_COMMANDS {
        return Err("Original command settlement acknowledgement capacity is full".into());
    }
    let data = serde_json::to_string(&record).map_err(|_| INVALID)?;
    if data.len() > MAX_BYTES {
        return Err("Original command settlement metadata exceeds its reservation".into());
    }
    connection.execute_batch("CREATE TABLE IF NOT EXISTS recovery_command_settlements(id TEXT PRIMARY KEY,record TEXT NOT NULL);").map_err(|_| INVALID)?;
    if version(connection)? < 9 {
        connection
            .execute_batch("PRAGMA user_version=9;")
            .map_err(|_| INVALID)?;
    }
    connection
        .execute(
            "INSERT INTO recovery_command_settlements(id,record) VALUES(?1,?2)",
            [&owner.id, &data],
        )
        .map_err(|_| INVALID)?;
    Ok(())
}
impl Namespace {
    pub(crate) fn pending_command_settlements(
        &self,
    ) -> Result<Vec<PendingCommandSettlement>, String> {
        self.access(false, |_, _, connection| {
            Ok(read(connection, version(connection)?)?
                .into_iter()
                .map(|(id, s)| PendingCommandSettlement {
                    id,
                    authority: s.authority,
                    worker: s.worker,
                    origin: s.origin,
                    recovery: s.recovery,
                    key: self.key.clone(),
                })
                .collect())
        })
    }
    pub(crate) fn has_command_settlement(
        &self,
        pending: &PendingCommandSettlement,
    ) -> Result<bool, String> {
        if self.key != pending.key {
            return Err(INVALID.into());
        }
        self.access(false, |_, _, connection| {
            let rows = read(connection, version(connection)?)?;
            let current = rows.iter().find(|(id, _)| id == &pending.id);
            match current {
                Some((_, s)) if s.matches(&pending.record()) => Ok(true),
                None => Ok(false),
                _ => Err(PENDING.into()),
            }
        })
    }
    /// Called after the original callback's distinct metadata-disposal ACK.
    /// No directory in the workspace, owner, command or live outcome is changed.
    pub(crate) fn acknowledged_command_settlement(
        &self,
        pending: &PendingCommandSettlement,
    ) -> Result<(), String> {
        if self.key != pending.key {
            return Err(INVALID.into());
        }
        self.access(true, |parent, anchor, connection| {
            let rows = read(connection, version(connection)?)?;
            let Some((_, s)) = rows.iter().find(|(id, _)| id == &pending.id) else {
                return Ok(());
            };
            if !s.matches(&pending.record()) {
                return Err(PENDING.into());
            }
            connection
                .execute(
                    "DELETE FROM recovery_command_settlements WHERE id=?1",
                    [&pending.id],
                )
                .map_err(|_| INVALID)?;
            self.save(parent, anchor, connection)
        })
    }
}

pub(super) fn administrative_entries(
    connection: &Connection,
    schema: u32,
    nonce: &str,
) -> Result<Vec<super::super::administration::Entry>, String> {
    use super::super::administration::{Entry, record_digest};
    read(connection, schema)?
        .into_iter()
        .map(|(id, s)| {
            Ok(Entry {
                expected: record_digest(&(nonce, "settlement", &id, &s))?,
                id,
                kind: "settlement".into(),
                folder: None,
                user: Some(s.authority.user),
                project: Some(s.authority.project),
                transaction: Some(s.authority.transaction.clone()),
                mode: Some(s.authority.root.mode),
                files: None,
                command_stage: Some("settled".into()),
                outcome: None,
                outcome_digest: s.recovery.command.outcome_digest.clone(),
                retirement_blocked: None,
            })
        })
        .collect()
}
pub(super) fn retirement_binding(
    connection: &Connection,
    schema: u32,
    id: &str,
) -> Result<String, String> {
    let (_, s) = read(connection, schema)?
        .into_iter()
        .find(|(key, _)| key == id)
        .ok_or(PENDING)?;
    super::super::administration::record_digest(&RecoveryAuthority::binding_for_scope(
        s.authority.user,
        s.authority.project,
        &s.recovery.scope,
    )?)
}
