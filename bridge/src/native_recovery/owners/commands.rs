//! Fixed-capacity original command slots in the anchored native database. An
//! intent materializes space for the complete result before backend CAS or RPC.
use super::*;
mod callback;
mod cleanup;
mod settlements;
pub(crate) use cleanup::ProcessCleanup;
use openwebide_core::{
    plugins::languages::intelligence::actions::{
        CommandInvocation, CommandOutcome, MAX_COMMAND_BYTES,
    },
    workspace_transaction::{WorkspaceCommandRecovery, WorkspaceTransactionWorker},
};
pub(crate) use settlements::PendingCommandSettlement;
const MAX_COMMANDS: usize = 32;
const SLOT_BYTES: usize = MAX_COMMAND_BYTES + 16 * 1024;

pub(crate) struct PendingCommand {
    pub authority: RecoveryAuthority,
    pub origin: String,
    pub worker: WorkspaceTransactionWorker,
    pub invocation: CommandInvocation,
    pub outcome: Option<CommandOutcome>,
    pub cleaned: bool,
    pub can_reject: bool,
    pub can_cancel: bool,
    pub cancelled: bool,
    pub dispatched_cleanup: bool,
    pub process_cleanup_confirmed: bool,
    owner: Owner,
    key: Key,
}
impl PendingCommand {
    pub(crate) fn present(&self, namespace: &Namespace) -> Result<bool, String> {
        namespace.has_original_command(
            &Permit {
                key: self.key.clone(),
                owner: self.owner.clone(),
                user: self.authority.user,
                project: self.authority.project,
            },
            &self.worker,
            &self.invocation,
        )
    }
    pub(crate) fn reopen(&self) -> Result<(Folder, Permit), String> {
        Ok((
            self.owner.folder.reopen()?,
            Permit {
                key: self.key.clone(),
                owner: self.owner.clone(),
                user: self.authority.user,
                project: self.authority.project,
            },
        ))
    }
}

pub(crate) struct CommandIntent<'a> {
    pub recovery: &'a WorkspaceCommandRecovery,
    pub worker: &'a WorkspaceTransactionWorker,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommandStage {
    /// Result space and the private worker exist before any file admission.
    Reserved,
    Prepared,
    Admitted,
    Dispatched,
    Finished,
    Cancelled,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommandRecord {
    format: u32,
    pub(super) owner: String,
    pub invocation: CommandInvocation,
    pub worker: WorkspaceTransactionWorker,
    pub stage: CommandStage,
    pub outcome: Option<CommandOutcome>,
    pub cleaned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cleanup: Option<cleanup::Receipt>,
    // Command reconnection must not replace the file journal's callback.
    // Older records use the original owner's address until verified reconnect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    callback: Option<String>,
}
impl std::fmt::Debug for CommandRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandRecord")
            .field("stage", &self.stage)
            .field("cleaned", &self.cleaned)
            .finish_non_exhaustive()
    }
}
impl CommandRecord {
    fn callback<'a>(&'a self, owner: &'a Owner) -> Option<&'a str> {
        self.callback.as_deref().or(owner.origin.as_deref())
    }
    pub(super) fn retirement_blocked(&self) -> Option<String> {
        if matches!(
            self.stage,
            CommandStage::Dispatched | CommandStage::Finished
        ) && !self.cleaned
        {
            return Some("Original command process cleanup is not confirmed".into());
        }
        if self
            .cleanup
            .as_ref()
            .is_some_and(|receipt| !receipt.confirmed)
        {
            return Some("The original process watcher still owes its cleanup receipt".into());
        }
        None
    }
    fn validate(&self) -> Result<(), String> {
        self.invocation.validate()?;
        self.worker.validate()?;
        if !matches!(
            (self.format, self.stage),
            (
                1,
                CommandStage::Reserved
                    | CommandStage::Prepared
                    | CommandStage::Admitted
                    | CommandStage::Dispatched
                    | CommandStage::Finished
            ) | (2, CommandStage::Cancelled)
                | (
                    3,
                    CommandStage::Admitted | CommandStage::Dispatched | CommandStage::Finished
                )
        ) || !fingerprint(&self.owner)
            || (self.format == 3) != self.cleanup.is_some()
            || (self.stage == CommandStage::Finished) != self.outcome.is_some()
            || (self.cleaned && self.outcome.is_none() && self.stage != CommandStage::Cancelled)
            || (self.stage == CommandStage::Cancelled && !self.cleaned)
        {
            return Err(INVALID.into());
        }
        if let Some(outcome) = &self.outcome {
            outcome.validate()?;
        }
        if let Some(cleanup) = &self.cleanup {
            cleanup.validate()?;
            if self.cleaned && !cleanup.confirmed
                || (cleanup.confirmed && self.outcome.is_some() && !self.cleaned)
            {
                return Err(INVALID.into());
            }
        }
        if let Some(callback) = &self.callback {
            openwebide_core::plugins::languages::intelligence::actions::CommandCallbackRequest::address(callback)?;
        }
        Ok(())
    }
}
pub(super) fn read(connection: &Connection, schema: u32) -> Result<Vec<CommandRecord>, String> {
    if schema < 4 {
        let tables: u32 = connection.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='recovery_commands'",[],|r|r.get(0)).map_err(|_|INVALID)?;
        if tables != 0 {
            return Err(INVALID.into());
        }
        return Ok(vec![]);
    }
    let mut query = connection
        .prepare("SELECT id,record FROM recovery_commands LIMIT 33")
        .map_err(|_| INVALID)?;
    let mut rows = query.query([]).map_err(|_| INVALID)?;
    let mut result = vec![];
    while let Some(row) = rows.next().map_err(|_| INVALID)? {
        if result.len() >= MAX_COMMANDS {
            return Err(INVALID.into());
        }
        let id: String = row.get(0).map_err(|_| INVALID)?;
        let encoded: String = row.get(1).map_err(|_| INVALID)?;
        if encoded.len() != SLOT_BYTES {
            return Err(INVALID.into());
        }
        let record: CommandRecord = serde_json::from_str(&encoded).map_err(|_| INVALID)?;
        record.validate()?;
        if id != record.owner
            || (schema < 5 && record.stage == CommandStage::Reserved)
            || (schema < 7 && record.stage == CommandStage::Cancelled)
            || (schema < 8 && record.format == 3)
            || (schema < 12 && record.callback.is_some())
        {
            return Err(INVALID.into());
        }
        result.push(record);
    }
    Ok(result)
}
pub(super) fn administrative_settlements(
    connection: &Connection,
    schema: u32,
    nonce: &str,
) -> Result<Vec<super::administration::Entry>, String> {
    settlements::administrative_entries(connection, schema, nonce)
}
pub(super) fn retirement_binding(
    connection: &Connection,
    schema: u32,
    id: &str,
) -> Result<String, String> {
    settlements::retirement_binding(connection, schema, id)
}
pub(super) fn validate(
    connection: &Connection,
    schema: u32,
    owners: &[Owner],
) -> Result<(), String> {
    let commands = read(connection, schema)?;
    let settlements = settlements::read(connection, schema)?;
    if commands.len() + settlements.len() > MAX_COMMANDS
        || settlements
            .iter()
            .any(|(id, _)| owners.iter().any(|owner| owner.id == *id))
    {
        return Err(INVALID.into());
    }
    for command in commands {
        if !owners
            .iter()
            .any(|owner| owner.id == command.owner && owner.command)
        {
            return Err(INVALID.into());
        }
    }
    Ok(())
}
fn save(connection: &Connection, record: &CommandRecord) -> Result<(), String> {
    record.validate()?;
    let mut encoded = serde_json::to_string(record).map_err(|_| INVALID)?;
    if encoded.len() > SLOT_BYTES {
        return Err("Native command result exceeds its original reservation".into());
    }
    encoded.extend(std::iter::repeat_n(' ', SLOT_BYTES - encoded.len()));
    connection.execute("INSERT INTO recovery_commands(id,record) VALUES (?1,?2) ON CONFLICT(id) DO UPDATE SET record=excluded.record",[&record.owner,&encoded]).map_err(|_|INVALID)?;
    Ok(())
}
fn owns(folder: &Folder, lease: &Lease, permit: &Permit) -> Result<(), String> {
    if !lease.retains(folder)
        || lease
            .recovery()
            .is_none_or(|held| held.key != permit.key || !held.owner.same(&permit.owner))
    {
        return Err("Original command publication requires its complete recovery lease".into());
    }
    Ok(())
}
impl Namespace {
    /// Presence ends only a stale local publisher; absence grants no workspace
    /// access. Unlike callback discovery, this also covers legacy/no-origin owners.
    pub(crate) fn has_original_command(
        &self,
        permit: &Permit,
        worker: &WorkspaceTransactionWorker,
        invocation: &CommandInvocation,
    ) -> Result<bool, String> {
        if permit.key != self.key {
            return Err(PENDING.into());
        }
        worker.validate()?;
        invocation.validate()?;
        self.access(false, |_, _, connection| {
            let Some(owner) = super::read(connection, version(connection)?)?
                .into_iter()
                .find(|owner| owner.same(&permit.owner))
            else {
                return Ok(false);
            };
            let record = read(connection, version(connection)?)?
                .into_iter()
                .find(|record| record.owner == owner.id)
                .ok_or(PENDING)?;
            if record.worker.id != worker.id
                || record.worker.secret != worker.secret
                || record.invocation != *invocation
            {
                return Err(PENDING.into());
            }
            Ok(true)
        })
    }
    pub(crate) fn check_command_origin(
        &self,
        user: i64,
        project: i64,
        mode: openwebide_core::WorkspaceMode,
        invocation: &CommandInvocation,
        origin: &str,
    ) -> Result<(), String> {
        if let Some(pending) = self.pending_command(user, project, mode, invocation)?
            && pending.origin != origin.trim_end_matches('/')
        {
            return Err("Reconnect the original command callback before continuing".into());
        }
        Ok(())
    }
    pub(crate) fn pending_command(
        &self,
        user: i64,
        project: i64,
        mode: openwebide_core::WorkspaceMode,
        invocation: &CommandInvocation,
    ) -> Result<Option<PendingCommand>, String> {
        Ok(self
            .pending_commands_matching(|authority, command| {
                authority.user == user
                    && authority.project == project
                    && authority.root.mode == mode
                    && command == invocation
            })?
            .into_iter()
            .next())
    }
    pub(crate) fn pending_commands(&self) -> Result<Vec<PendingCommand>, String> {
        self.pending_commands_matching(|_, _| true)
    }
    fn pending_commands_matching(
        &self,
        matches: impl Fn(&RecoveryAuthority, &CommandInvocation) -> bool,
    ) -> Result<Vec<PendingCommand>, String> {
        self.access(false, |_, _, connection| {
            let owners = super::read(connection, version(connection)?)?;
            let commands = read(connection, version(connection)?)?;
            let mut pending = vec![];
            for owner in owners {
                let Some(authority) = owner.authority()? else {
                    continue;
                };
                let Some(command) = commands.iter().find(|c| c.owner == owner.id) else {
                    continue;
                };
                if !matches(&authority, &command.invocation) {
                    continue;
                }
                let Some(origin) = command.callback(&owner) else {
                    continue;
                };
                pending.push(PendingCommand {
                    authority,
                    origin: origin.to_owned(),
                    worker: command.worker.clone(),
                    invocation: command.invocation.clone(),
                    outcome: command.outcome.clone(),
                    cleaned: command.cleaned,
                    can_reject: owner.files == FileStage::Finished
                        // Prepared precedes backend Claim and native admission.
                        // Recovery must still reacquire the full original lease
                        // and reread this stage before recording any rejection.
                        && (matches!(command.stage, CommandStage::Reserved | CommandStage::Prepared)
                            || (command.cleanup.as_ref().is_some_and(|c| c.confirmed)
                                && command.stage == CommandStage::Admitted)),
                    can_cancel: matches!(owner.files, FileStage::Reserved | FileStage::Finished)
                        && command.stage == CommandStage::Reserved,
                    cancelled: command.stage == CommandStage::Cancelled,
                    dispatched_cleanup: owner.files == FileStage::Finished
                        && command.stage == CommandStage::Dispatched
                        && command.cleanup.as_ref().is_some_and(|c| c.confirmed),
                    process_cleanup_confirmed: command
                        .cleanup
                        .as_ref()
                        .is_some_and(|c| c.confirmed),
                    owner,
                    key: self.key.clone(),
                });
            }
            Ok(pending)
        })
    }
    #[cfg(test)]
    pub(crate) fn reserved_command_worker(
        &self,
        invocation: &CommandInvocation,
    ) -> Result<openwebide_core::workspace_transaction::WorkspaceTransactionWorkerKey, String> {
        self.access(false, |_, _, connection| {
            let record = read(connection, version(connection)?)?
                .into_iter()
                .find(|record| {
                    &record.invocation == invocation && record.stage == CommandStage::Reserved
                })
                .ok_or(PENDING)?;
            Ok(record.worker.delegation_key())
        })
    }
    pub(crate) fn reserve_command(
        &self,
        folder: &Folder,
        lease: &Lease,
        permit: &Permit,
        intent: CommandIntent<'_>,
    ) -> Result<(), String> {
        self.enroll_command(folder, lease, permit, intent, CommandStage::Reserved)?;
        Ok(())
    }
    fn enroll_command(
        &self,
        folder: &Folder,
        lease: &Lease,
        permit: &Permit,
        intent: CommandIntent<'_>,
        stage: CommandStage,
    ) -> Result<(CommandRecord, bool), String> {
        self.enrollment_lease(folder, lease, permit)?;
        intent.recovery.validate()?;
        intent.worker.validate()?;
        let binding = RecoveryAuthority::binding_for_scope(
            permit.user,
            permit.project,
            &intent.recovery.scope,
        )?;
        if binding != permit.owner.binding
            || !permit.owner.command
            || !intent.recovery.activity.language_command
        {
            return Err("The original native command intent changed".into());
        }
        let folder = self.original_folder(folder, true)?;
        self.access(true, |parent, anchor, connection| {
            let (owner, owner_changed) = self.enroll_owner(connection, &folder, permit)?;
            let commands = read(connection, version(connection)?)?;
            let (record, new) = if let Some(mut existing) =
                commands.iter().find(|c| c.owner == owner.id).cloned()
            {
                if existing.invocation != intent.recovery.command.invocation {
                    return Err(PENDING.into());
                }
                if existing.stage == CommandStage::Reserved {
                    if existing.worker.id != intent.worker.id
                        || existing.worker.secret != intent.worker.secret
                    {
                        return Err("The reserved native command belongs to its original private worker".into());
                    }
                    if stage == CommandStage::Prepared {
                        if intent.recovery.activity.file_recovery
                            || intent.recovery.scope.worker.as_ref().is_some_and(|w| w.active)
                        {
                            return Err("Finish the original file operation before starting its command".into());
                        }
                        existing.stage = CommandStage::Prepared;
                        (existing, true)
                    } else {
                        (existing, false)
                    }
                } else {
                    // Prepared is already a single-use execution intent.
                    (existing, false)
                }
            } else {
                if commands.len() + settlements::read(connection, version(connection)?)?.len() >= MAX_COMMANDS {
                    return Err("Native command recovery capacity is full".into());
                }
                (
                    CommandRecord {
                        format: 1,
                        owner: owner.id,
                        invocation: intent.recovery.command.invocation.clone(),
                        worker: intent.worker.clone(),
                        stage,
                        outcome: None,
                        cleaned: false,
                        cleanup: None,
                        callback: None,
                    },
                    true,
                )
            };
            if !new && !owner_changed {
                return Ok((record, false));
            }
            connection.execute_batch("CREATE TABLE IF NOT EXISTS recovery_commands(id TEXT PRIMARY KEY,record TEXT NOT NULL);").map_err(|_| INVALID)?;
            if version(connection)? < 5 { connection.execute_batch("PRAGMA user_version=5;").map_err(|_| INVALID)?; }
            save(connection, &record)?;
            self.save(parent, anchor, connection)?;
            Ok((record, new))
        })
    }
    fn command_access<T>(
        &self,
        folder: &Folder,
        lease: &Lease,
        permit: &Permit,
        work: impl FnOnce(&Connection, &Owner, Option<CommandRecord>) -> Result<T, String>,
    ) -> Result<T, String> {
        owns(folder, lease, permit)?;
        // The original lease and permit retain the actual folder even when
        // its pathname disappears. Private outcome publication performs no
        // workspace I/O; dispatch/retirement still require current coverage.
        let folder = permit.owner.folder.clone();
        self.access(true, |parent, anchor, connection| {
            let owners = super::read(connection, version(connection)?)?;
            self.check(connection, &owners, &folder, Some(permit))?;
            let owner = owners
                .iter()
                .find(|o| o.same(&permit.owner))
                .ok_or(PENDING)?;
            if !owner.command {
                return Err(PENDING.into());
            }
            let original = read(connection, version(connection)?)?
                .into_iter()
                .find(|c| c.owner == owner.id);
            let result = work(connection, owner, original)?;
            self.save(parent, anchor, connection)?;
            Ok(result)
        })
    }
    pub(crate) fn command_intent(
        &self,
        folder: &Folder,
        lease: &Lease,
        permit: &Permit,
        intent: CommandIntent<'_>,
    ) -> Result<(CommandRecord, bool), String> {
        self.enroll_command(folder, lease, permit, intent, CommandStage::Prepared)
    }
    pub(crate) fn command_admitted(
        &self,
        folder: &Folder,
        lease: &Lease,
        permit: &Permit,
        worker: &WorkspaceTransactionWorker,
    ) -> Result<(), String> {
        if !lease.covers(folder) {
            return Err(PENDING.into());
        }
        self.command_change(folder, lease, permit, worker, |record| {
            if record.stage != CommandStage::Prepared {
                return Err(PENDING.into());
            }
            record.stage = CommandStage::Admitted;
            Ok(())
        })
    }
    pub(crate) fn command_dispatch(
        &self,
        folder: &Folder,
        lease: &Lease,
        permit: &Permit,
        worker: &WorkspaceTransactionWorker,
    ) -> Result<(), String> {
        if !lease.covers(folder) {
            return Err(PENDING.into());
        }
        self.command_change(folder, lease, permit, worker, |record| {
            if record.stage != CommandStage::Admitted
                || record.cleanup.as_ref().is_some_and(|c| c.confirmed)
            {
                return Err("This original command cannot be dispatched again".into());
            }
            record.stage = CommandStage::Dispatched;
            Ok(())
        })
    }
    pub(crate) fn command_outcome(
        &self,
        folder: &Folder,
        lease: &Lease,
        permit: &Permit,
        worker: &WorkspaceTransactionWorker,
        outcome: &CommandOutcome,
        cleaned: bool,
    ) -> Result<(), String> {
        outcome.validate()?;
        self.command_change(folder, lease, permit, worker, |record| {
            let cleaned = cleaned || record.cleanup.as_ref().is_some_and(|c| c.confirmed);
            if record.stage == CommandStage::Cancelled {
                return Err("A cancelled native command cannot acquire an outcome".into());
            }
            if cleaned && record.cleanup.as_ref().is_some_and(|c| !c.confirmed) {
                return Err("The original process cleanup has not been confirmed".into());
            }
            if let Some(original) = &record.outcome {
                if original != outcome {
                    return Err("The original native command outcome cannot be replaced".into());
                }
                record.cleaned |= cleaned;
                return Ok(());
            }
            if (matches!(outcome, CommandOutcome::Completed { .. })
                && record.stage != CommandStage::Dispatched)
                || (matches!(outcome, CommandOutcome::Rejected { .. })
                    && record.stage == CommandStage::Dispatched)
                || (!cleaned
                    && record.cleanup.is_none()
                    && !matches!(outcome, CommandOutcome::Uncertain { .. }))
            {
                return Err(
                    "The command outcome does not match its original dispatch boundary".into(),
                );
            }
            record.stage = CommandStage::Finished;
            record.outcome = Some(outcome.clone());
            record.cleaned = cleaned;
            Ok(())
        })
    }
    fn command_change(
        &self,
        folder: &Folder,
        lease: &Lease,
        permit: &Permit,
        worker: &WorkspaceTransactionWorker,
        change: impl FnOnce(&mut CommandRecord) -> Result<(), String>,
    ) -> Result<(), String> {
        self.command_access(folder, lease, permit, |connection, _, record| {
            let mut record = record.ok_or(PENDING)?;
            if record.worker.id != worker.id || record.worker.secret != worker.secret {
                return Err(PENDING.into());
            }
            change(&mut record)?;
            save(connection, &record)
        })
    }
    pub(crate) fn command_cancelled(
        &self,
        folder: &Folder,
        lease: &Lease,
        permit: &Permit,
        worker: &WorkspaceTransactionWorker,
        recovery: &WorkspaceCommandRecovery,
    ) -> Result<(), String> {
        use openwebide_core::plugins::languages::intelligence::actions::ActionPhase;
        recovery.validate()?;
        if !lease.covers(folder)
            || recovery.command.phase != ActionPhase::Cancelled
            || recovery.command.outcome_digest.is_some()
            || recovery.activity.file_recovery
            || RecoveryAuthority::binding_for_scope(permit.user, permit.project, &recovery.scope)?
                != permit.owner.binding
        {
            return Err(PENDING.into());
        }
        self.command_access(folder, lease, permit, |connection, owner, record| {
            let mut record = record.ok_or(PENDING)?;
            if record.worker.id != worker.id
                || record.worker.secret != worker.secret
                || record.invocation != recovery.command.invocation
            {
                return Err(PENDING.into());
            }
            if record.stage == CommandStage::Cancelled {
                return Ok(());
            }
            // No file admission ever committed only when both the durable
            // native reservation and the original backend observation prove it.
            let unadmitted = owner.files == FileStage::Reserved
                && recovery.scope.worker.is_none()
                && recovery.command.worker.is_none()
                && !recovery.command.native_settlement_pending
                && !recovery.command.command_cleanup_confirmed;
            if record.stage != CommandStage::Reserved
                || !(owner.files == FileStage::Finished || unadmitted)
            {
                return Err(PENDING.into());
            }
            record.format = 2;
            record.stage = CommandStage::Cancelled;
            record.cleaned = true;
            if version(connection)? < 7 {
                connection
                    .execute_batch("PRAGMA user_version=7;")
                    .map_err(|_| INVALID)?;
            }
            save(connection, &record)
        })
    }
    pub(crate) fn command_settled(
        &self,
        folder: &Folder,
        lease: &Lease,
        permit: &Permit,
        recovery: &WorkspaceCommandRecovery,
    ) -> Result<(), String> {
        recovery.validate()?;
        if RecoveryAuthority::binding_for_scope(permit.user, permit.project, &recovery.scope)?
            != permit.owner.binding
            || recovery.activity.ensure_idle().is_err()
            || recovery.command.native_settlement_pending
            || !recovery.command.command_cleanup_confirmed
        {
            return Err(PENDING.into());
        }
        owns(folder, lease, permit)?;
        if !lease.covers(folder) {
            return Err(PENDING.into());
        }
        let folder = self.original_folder(folder, true)?;
        self.access(true, |parent, anchor, connection| {
            let owners = super::read(connection, version(connection)?)?;
            self.check(connection, &owners, &folder, Some(permit))?;
            let owner = owners
                .iter()
                .find(|o| o.same(&permit.owner))
                .ok_or(PENDING)?;
            let record = read(connection, version(connection)?)?
                .into_iter()
                .find(|c| c.owner == owner.id)
                .ok_or(PENDING)?;
            let result_matches = if record.stage == CommandStage::Cancelled {
                recovery.command.phase == openwebide_core::plugins::languages::intelligence::actions::ActionPhase::Cancelled
                    && record.outcome.is_none() && recovery.command.outcome_digest.is_none()
            } else {
                let outcome = record.outcome.as_ref().ok_or(PENDING)?;
                let digest = openwebide_core::plugins::content_digest(
                    &serde_json::to_vec(outcome).map_err(|_| INVALID)?,
                );
                recovery.command.outcome_digest.as_deref() == Some(digest.as_str())
            };
            if !record.cleaned
                || record.invocation != recovery.command.invocation
                || !result_matches
                || recovery
                    .command
                    .worker
                    .as_ref()
                    .is_none_or(|w| w.active || w.id != record.worker.id)
            {
                return Err(PENDING.into());
            }
            settlements::retain(connection, owner, &record, recovery)?;
            connection
                .execute("DELETE FROM recovery_commands WHERE id=?1", [&owner.id])
                .map_err(|_| INVALID)?;
            connection
                .execute("DELETE FROM recovery_owners WHERE id=?1", [&owner.id])
                .map_err(|_| INVALID)?;
            self.save(parent, anchor, connection)
        })
    }
}
#[cfg(test)]
mod tests;
