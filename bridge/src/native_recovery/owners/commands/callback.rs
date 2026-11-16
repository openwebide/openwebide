//! Updating a verified callback changes no command, result, folder or lease.
use super::*;
impl Namespace {
    pub(crate) fn command_callback(
        &self,
        authority: &RecoveryAuthority,
        worker: &WorkspaceTransactionWorker,
    ) -> Result<Option<String>, String> {
        authority.validate()?;
        worker.validate()?;
        self.access(false, |_, _, db| {
            for owner in super::super::read(db, version(db)?)? {
                if owner.authority()?.as_ref() != Some(authority) {
                    continue;
                }
                let command = read(db, version(db)?)?
                    .into_iter()
                    .find(|c| c.owner == owner.id)
                    .ok_or(PENDING)?;
                if command.worker.id != worker.id || command.worker.secret != worker.secret {
                    return Err(PENDING.into());
                }
                return Ok(command.callback(&owner).map(str::to_owned));
            }
            Ok(None)
        })
    }
    pub(crate) fn reconnect_command_callback(
        &self,
        pending: &PendingCommand,
        current: &WorkspaceCommandRecovery,
        origin: &str,
    ) -> Result<(), String> {
        let origin = openwebide_core::plugins::languages::intelligence::actions::CommandCallbackRequest::address(origin)?;
        current.validate()?;
        if pending.key != self.key
            || !pending.authority.matches_scope(
                pending.authority.user,
                pending.authority.project,
                &current.scope,
            )
            || current.command.invocation != pending.invocation
            || current
                .command
                .worker
                .as_ref()
                .is_some_and(|w| w.id != pending.worker.id)
        {
            return Err(PENDING.into());
        }
        self.access(true, |parent, anchor, db| {
            let owner = super::super::read(db, version(db)?)?
                .into_iter()
                .find(|o| o.same(&pending.owner))
                .ok_or(PENDING)?;
            let mut command = read(db, version(db)?)?
                .into_iter()
                .find(|c| c.owner == owner.id)
                .ok_or(PENDING)?;
            if owner.authority()?.as_ref() != Some(&pending.authority)
                || command.worker.id != pending.worker.id
                || command.worker.secret != pending.worker.secret
                || command.invocation != pending.invocation
                || command.callback(&owner) != Some(&pending.origin)
            {
                return Err(PENDING.into());
            }
            if command.callback(&owner) == Some(origin.as_str()) {
                return Ok(());
            }
            command.callback = Some(origin);
            // Readers predating independent callbacks must fail closed rather
            // than route a retained command through its old file callback.
            if version(db)? < 12 {
                db.execute_batch(
                    "CREATE TABLE IF NOT EXISTS recovery_command_settlements(id TEXT PRIMARY KEY,record TEXT NOT NULL);
                     CREATE TABLE IF NOT EXISTS recovery_retired_owners(id TEXT PRIMARY KEY,record TEXT NOT NULL);
                     PRAGMA user_version=12;",
                ).map_err(|_| INVALID)?;
            }
            save(db, &command)?;
            self.save(parent, anchor, db)
        })
    }
}
