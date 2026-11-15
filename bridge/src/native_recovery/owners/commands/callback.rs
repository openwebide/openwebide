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
                return Ok(owner.origin);
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
        super::super::context::validate_origin(origin)?;
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
            let mut owner = super::super::read(db, version(db)?)?
                .into_iter()
                .find(|o| o.same(&pending.owner))
                .ok_or(PENDING)?;
            let command = read(db, version(db)?)?
                .into_iter()
                .find(|c| c.owner == owner.id)
                .ok_or(PENDING)?;
            if owner.authority()?.as_ref() != Some(&pending.authority)
                || command.worker.id != pending.worker.id
                || command.worker.secret != pending.worker.secret
                || command.invocation != pending.invocation
                || owner.origin.as_deref() != Some(&pending.origin)
            {
                return Err(PENDING.into());
            }
            if owner.origin.as_deref() == Some(origin) {
                return Ok(());
            }
            owner.origin = Some(origin.into());
            super::super::context::save_owner(db, &owner)?;
            self.save(parent, anchor, db)
        })
    }
}
