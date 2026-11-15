//! Compact final proofs outlive prunable public action/workspace payloads.
//! Only the original native authority may acknowledge their disposal.
use super::*;
use openwebide_core::{
    plugins::languages::intelligence::actions::{
        CommandJournalOperation, CommandWorkerOperation, OriginalCommandRequest,
    },
    workspace_transaction::{
        WorkspaceCommandRecovery, WorkspaceJournalResponse, WorkspaceTransactionWorker,
    },
};
const MAX_SETTLEMENTS: i64 = 256;
const MAX_SETTLEMENT_BYTES: usize = 64 * 1024;
fn conflict() -> StorageError {
    StorageError::Conflict(
        "The original command settlement changed; recover its original owner".into(),
    )
}
struct Settlement {
    recovery: WorkspaceCommandRecovery,
    worker_digest: String,
    legacy: bool,
}
impl Settlement {
    fn authorize(&self, request: &OriginalCommandRequest) -> Result<(), StorageError> {
        if !request.authority.matches_scope(
            request.authority.user,
            request.authority.project,
            &self.recovery.scope,
        ) || self
            .recovery
            .command
            .worker
            .as_ref()
            .is_none_or(|w| w.id != request.worker.id)
            || !(self.worker_digest == request.worker.secret_digest()
                || (self.legacy && self.worker_digest.is_empty()))
        {
            return Err(conflict());
        }
        Ok(())
    }
}
impl<D: Db> Store<D> {
    async fn command_settlement(
        &self,
        user: UserId,
        project: i64,
        id: &str,
    ) -> Result<Option<Settlement>, StorageError> {
        // The original issuer authenticates this private lookup. Project
        // deletion cascades its receipts, but a lost metadata-disposal ACK
        // must still be repeatable after that deletion. An absent receipt
        // grants no scope, worker or execution authority.
        let rows = self.db.execute("SELECT CASE WHEN length(CAST(recovery AS BLOB))<=65536 THEN recovery ELSE 'invalid' END,CASE WHEN length(worker_digest)<=64 THEN worker_digest ELSE 'invalid' END,legacy FROM language_command_settlements WHERE user_id=? AND project_id=? AND id=?", &[DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.into())]).await?;
        let Some(row) = rows.rows.first() else {
            return Ok(None);
        };
        let recovery: WorkspaceCommandRecovery =
            serde_json::from_str(row.get_text(0)?).map_err(|_| conflict())?;
        recovery
            .validate_settlement()
            .map_err(StorageError::InvalidValue)?;
        let worker_digest = row.get_text(1)?.to_owned();
        let legacy = match row.get_int(2)? {
            0 => false,
            1 => true,
            _ => return Err(conflict()),
        };
        if recovery.scope.id != id
            || !(openwebide_core::workspace_transaction::companion::fingerprint(&worker_digest)
                || (legacy && worker_digest.is_empty()))
        {
            return Err(conflict());
        }
        Ok(Some(Settlement {
            recovery,
            worker_digest,
            legacy,
        }))
    }
    pub(in crate::store) async fn ensure_command_identity_available(
        &self,
        user: UserId,
        project: i64,
        id: &str,
    ) -> Result<(), StorageError> {
        if !self.db.execute("SELECT 1 FROM language_command_settlements WHERE user_id=? AND project_id=? AND id=?", &[DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.into())]).await?.rows.is_empty() { return Err(conflict()); }
        Ok(())
    }
    pub(super) async fn reserve_command_settlement(
        &self,
        user: UserId,
        project: i64,
        id: &str,
    ) -> Result<(), StorageError> {
        // Active native workers reserve their eventual small receipt before
        // admission. Legacy terminal proofs have their prior bounded quota.
        let rows = self.db.execute("SELECT (SELECT count(*) FROM language_command_settlements s WHERE s.user_id=? AND s.legacy=0 AND NOT(s.project_id=? AND s.id=?))+(SELECT count(*) FROM language_actions a WHERE a.user_id=? AND a.native_settlement_pending=1 AND NOT(a.project_id=? AND a.id=?) AND NOT EXISTS(SELECT 1 FROM language_command_settlements s WHERE s.user_id=a.user_id AND s.project_id=a.project_id AND s.id=a.id))", &[DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.into()),DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.into())]).await?;
        if rows.rows[0].get_int(0)? >= MAX_SETTLEMENTS {
            return Err(StorageError::Conflict("Original command settlement storage is full; reconnect its native acknowledgement owners before starting more work".into()));
        }
        Ok(())
    }
    pub(super) async fn retain_command_settlement(
        &self,
        user: UserId,
        project: i64,
        recovery: &WorkspaceCommandRecovery,
        worker: &WorkspaceTransactionWorker,
        legacy: bool,
    ) -> Result<(), StorageError> {
        recovery
            .validate_settlement()
            .map_err(StorageError::InvalidValue)?;
        if recovery
            .command
            .worker
            .as_ref()
            .is_none_or(|w| w.id != worker.id)
        {
            return Err(conflict());
        }
        let id = &recovery.scope.id;
        let acknowledged = self.db.execute("SELECT command_settlement_acknowledged FROM language_actions WHERE user_id=? AND project_id=? AND id=?", &[DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.clone())]).await?;
        if acknowledged
            .rows
            .first()
            .is_some_and(|r| r.get_int(0).ok() == Some(1))
        {
            return Ok(());
        }
        if let Some(existing) = self.command_settlement(user, project, id).await? {
            if existing.recovery == *recovery
                && (existing.worker_digest == worker.secret_digest()
                    || (existing.legacy && existing.worker_digest.is_empty()))
            {
                return Ok(());
            }
            return Err(conflict());
        }
        if legacy {
            let count = self.db.execute("SELECT count(*) FROM language_command_settlements WHERE user_id=? AND legacy=1", &[DbValue::Int(user.get())]).await?;
            if count.rows[0].get_int(0)? >= MAX_SETTLEMENTS {
                return Err(conflict());
            }
        } else {
            self.reserve_command_settlement(user, project, id).await?;
        }
        let encoded = serde_json::to_string(recovery).map_err(|_| conflict())?;
        if encoded.len() > MAX_SETTLEMENT_BYTES {
            return Err(StorageError::InvalidValue(
                "Original command settlement exceeds its bound".into(),
            ));
        }
        self.db.execute("INSERT INTO language_command_settlements(user_id,project_id,id,recovery,worker_digest,legacy) VALUES(?,?,?,?,?,?)", &[DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.clone()),DbValue::Text(encoded),DbValue::Text(if legacy { String::new() } else { worker.secret_digest() }),DbValue::Int(i64::from(legacy))]).await?;
        Ok(())
    }
    pub(super) async fn original_command_settlement(
        &self,
        request: &OriginalCommandRequest,
    ) -> Result<Option<WorkspaceJournalResponse>, StorageError> {
        let Some(settlement) = self
            .command_settlement(
                UserId::new(request.authority.user),
                request.authority.project,
                &request.authority.transaction,
            )
            .await?
        else {
            return Ok(None);
        };
        settlement.authorize(request)?;
        match &request.operation {
            CommandJournalOperation::Scope => Ok(Some(WorkspaceJournalResponse::CommandScope {
                recovery: Some(Box::new(settlement.recovery)),
            })),
            CommandJournalOperation::Worker { operation }
                if matches!(**operation, CommandWorkerOperation::Settle { .. }) =>
            {
                let reply = WorkspaceJournalResponse::CommandScope {
                    recovery: Some(Box::new(settlement.recovery)),
                };
                reply
                    .command_worker(operation)
                    .map_err(StorageError::InvalidRequest)?;
                Ok(Some(reply))
            }
            // The receipt deliberately contains no command result payload.
            CommandJournalOperation::Outcome { .. } => Ok(None),
            _ => Err(conflict()),
        }
    }
    pub(super) async fn acknowledge_command_settlement(
        &self,
        request: &OriginalCommandRequest,
        expected: &WorkspaceCommandRecovery,
    ) -> Result<WorkspaceJournalResponse, StorageError> {
        expected
            .validate_settlement()
            .map_err(StorageError::InvalidRequest)?;
        let user = UserId::new(request.authority.user);
        let project = request.authority.project;
        let id = &request.authority.transaction;
        if let Some(settlement) = self.command_settlement(user, project, id).await? {
            settlement.authorize(request)?;
            if settlement.recovery != *expected {
                return Err(conflict());
            }
            self.db.execute("DELETE FROM language_command_settlements WHERE user_id=? AND project_id=? AND id=?", &[DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.clone())]).await?;
        } else {
            // A repeated metadata-disposal ACK neither reports current scope nor
            // alters a newer owner. Refuse contradictory live original work.
            let live = self
                .db
                .execute(
                    "SELECT 1 FROM language_actions WHERE user_id=? AND project_id=? AND id=?",
                    &[
                        DbValue::Int(user.get()),
                        DbValue::Int(project),
                        DbValue::Text(id.clone()),
                    ],
                )
                .await?;
            if !live.rows.is_empty()
                && let Some(current) = self.workspace_command_recovery(user, project, id).await?
                && current.scope.generation == expected.scope.generation
                && current != *expected
            {
                return Err(conflict());
            }
        }
        self.db.execute("UPDATE language_actions SET command_settlement_acknowledged=1 WHERE user_id=? AND project_id=? AND id=? AND command_worker_id=? AND native_settlement_pending=0 AND command_cleanup_confirmed=1 AND EXISTS(SELECT 1 FROM workspace_transactions w WHERE w.user_id=language_actions.user_id AND w.project_id=language_actions.project_id AND w.id=language_actions.id AND w.generation=?)", &[DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.clone()),DbValue::Text(request.worker.id.clone()),DbValue::Text(expected.scope.generation.clone())]).await?;
        Ok(WorkspaceJournalResponse::CommandSettlementAcknowledged {
            settlement: Box::new(expected.clone()),
        })
    }
}
