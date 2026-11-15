//! One command-worker admission/outcome policy for both native transports.
use super::*;
use openwebide_core::{
    plugins::languages::intelligence::actions::{CommandAdmission, CommandWorkerOperation},
    workspace_transaction::WorkspaceCommandRecovery,
};

fn conflict() -> StorageError {
    StorageError::Conflict(
        "Original native command ownership changed; inspect its recovery before continuing".into(),
    )
}
impl<D: Db> Store<D> {
    /// The transport verifies the enduring issuer; this shared policy also
    /// pins admitted workers and limits unadmitted owners to observation and
    /// cancellation. Cleanup never grants dispatch permission.
    pub async fn original_language_command(
        &self,
        request: &openwebide_core::plugins::languages::intelligence::actions::OriginalCommandRequest,
        now: i64,
    ) -> Result<openwebide_core::workspace_transaction::WorkspaceJournalResponse, StorageError>
    {
        use openwebide_core::plugins::languages::intelligence::actions::CommandJournalOperation;
        use openwebide_core::workspace_transaction::WorkspaceJournalResponse;
        request.validate().map_err(StorageError::InvalidRequest)?;
        self.db
            .transaction(|tx| async move {
                let store = Store::new(tx);
                let authority = &request.authority;
                let user = UserId::new(authority.user);
                if let CommandJournalOperation::AcknowledgeSettlement { expected } = &request.operation {
                    return store.acknowledge_command_settlement(request, expected).await;
                }
                if let Some(reply) = store.original_command_settlement(request).await? {
                    return Ok(reply);
                }
                let current = store
                    .original_command(user, authority.project, &authority.transaction)
                    .await?;
                if !authority.matches_scope(authority.user, authority.project, &current.scope) {
                    return Err(conflict());
                }
                let (id, digest, may_dispatch) = store
                    .command_credentials(user, authority.project, &authority.transaction)
                    .await?;
                let terminal_receipt = digest.is_empty()
                    && current.command.phase.terminal()
                    && !current.command.native_settlement_pending
                    && current.command.command_cleanup_confirmed;
                let unadmitted = id.is_empty() && digest.is_empty() && !may_dispatch
                    && current.command.worker.is_none()
                    && !current.command.native_settlement_pending
                    && !current.command.command_cleanup_confirmed
                    && current.command.outcome_digest.is_none()
                    && matches!(current.command.phase, ActionPhase::Approved | ActionPhase::Cancelled)
                    && (matches!(request.operation, CommandJournalOperation::Scope)
                        || (current.command.phase == ActionPhase::Cancelled
                            && matches!(&request.operation, CommandJournalOperation::Worker { operation } if matches!(**operation, CommandWorkerOperation::ConfirmCancellation { .. }))));
                if !unadmitted && (id != request.worker.id
                    || (digest != request.worker.secret_digest() && !terminal_receipt)
                ) {
                    return Err(conflict());
                }
                let recovery = match &request.operation {
                    CommandJournalOperation::AcknowledgeSettlement { .. } => unreachable!("handled before public history lookup"),
                    CommandJournalOperation::Scope => current,
                    CommandJournalOperation::Outcome { expected } => {
                        return store.native_command_outcome(user,authority.project,&authority.transaction,expected).await;
                    }
                    CommandJournalOperation::Worker { operation } => {
                        store
                            .native_language_command_inner(
                                user,
                                authority.project,
                                &authority.transaction,
                                operation,
                                now,
                            )
                            .await?
                    }
                };
                Ok(WorkspaceJournalResponse::CommandScope {
                    recovery: Some(Box::new(recovery)),
                })
            })
            .await
    }
    pub(super) async fn native_command_outcome(
        &self,
        user: UserId,
        project: i64,
        id: &str,
        expected: &WorkspaceCommandRecovery,
    ) -> Result<openwebide_core::workspace_transaction::WorkspaceJournalResponse, StorageError>
    {
        use openwebide_core::workspace_transaction::WorkspaceJournalResponse;
        expected.validate().map_err(StorageError::InvalidRequest)?;
        let current = self.original_command(user, project, id).await?;
        if &current != expected {
            return Err(conflict());
        }
        let retained = self
            .read_action_recovery(user, project, Some(id))
            .await?
            .ok_or_else(conflict)?;
        let outcome = retained.outcome.ok_or_else(conflict)?;
        let reply = WorkspaceJournalResponse::CommandOutcome {
            recovery: Box::new(current),
            outcome,
        };
        reply
            .command_outcome(expected)
            .map_err(StorageError::InvalidValue)?;
        Ok(reply)
    }
    pub(super) async fn command_credentials(
        &self,
        user: UserId,
        project: i64,
        id: &str,
    ) -> Result<(String, String, bool), StorageError> {
        let rows=self.db.execute("SELECT command_worker_id,command_worker_digest,command_worker_may_dispatch FROM language_actions WHERE user_id=? AND project_id=? AND id=?",&[DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.into())]).await?;
        let row = rows.rows.first().ok_or_else(conflict)?;
        let id = row.get_text(0)?;
        let digest = row.get_text(1)?;
        if (!id.is_empty() && !valid_id(id))
            || (!digest.is_empty()
                && !openwebide_core::workspace_transaction::companion::fingerprint(digest))
        {
            return Err(conflict());
        }
        let dispatch = match row.get_int(2)? {
            0 => false,
            1 => true,
            _ => return Err(conflict()),
        };
        Ok((id.into(), digest.into(), dispatch))
    }
    pub(in crate::store) async fn reserve_workspace_command(
        &self,
        user: UserId,
        project: i64,
        before: &openwebide_core::workspace_transaction::WorkspaceTransactionScope,
        claimed: &openwebide_core::workspace_transaction::WorkspaceTransactionScope,
        reservation: Option<
            &openwebide_core::plugins::languages::intelligence::actions::CommandReservation,
        >,
        now: i64,
    ) -> Result<(), StorageError> {
        let (_, activity) = self
            .workspace_transaction_activity(user, project, &before.id)
            .await?
            .ok_or_else(conflict)?;
        if !activity.language_command {
            return if reservation.is_none() {
                Ok(())
            } else {
                Err(conflict())
            };
        }
        let current = self
            .workspace_command_recovery(user, project, &before.id)
            .await?;
        let Some(reservation) = reservation else {
            if current.as_ref().is_some_and(|r| {
                r.activity.file_recovery
                    && r.command.phase == ActionPhase::Approved
                    && before.accepted_language_review_digest.is_none()
                    && before.worker.is_none()
                    && before.revision == 1
            }) {
                return Err(StorageError::Conflict(
                    "Reserve the original attached command with its first file admission".into(),
                ));
            }
            return Ok(());
        };
        let current = current.ok_or_else(conflict)?;
        let (id, digest, dispatch) = self.command_credentials(user, project, &before.id).await?;
        let mut expected = (*reservation.expected).clone();
        expected.scope = claimed.clone();
        if current.scope != *claimed
            || current.command.invocation != expected.command.invocation
            || current.command.phase != ActionPhase::Approved
            || current.command.revision != expected.command.revision
            || current.command.outcome_digest.is_some()
            || current.command.command_cleanup_confirmed
        {
            return Err(conflict());
        }
        if current.command.worker.is_some() {
            if id != reservation.worker.id
                || digest != reservation.worker.secret_digest()
                || dispatch
                || !current.command.native_settlement_pending
            {
                return Err(conflict());
            }
            return Ok(());
        }
        if current != expected || !id.is_empty() || !digest.is_empty() || dispatch {
            return Err(conflict());
        }
        self.reserve_command_settlement(user, project, &before.id)
            .await?;
        self.db.execute("UPDATE language_actions SET command_worker_id=?,command_worker_digest=?,command_worker_may_dispatch=0,native_settlement_pending=1,updated_at=? WHERE user_id=? AND project_id=? AND id=?", &[
            DbValue::Text(reservation.worker.id.clone()),DbValue::Text(reservation.worker.secret_digest()),DbValue::Int(now),DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(before.id.clone()),
        ]).await?;
        Ok(())
    }
    async fn original_command(
        &self,
        user: UserId,
        project: i64,
        id: &str,
    ) -> Result<WorkspaceCommandRecovery, StorageError> {
        self.workspace_command_recovery(user, project, id)
            .await?
            .ok_or_else(conflict)
    }
    pub async fn native_language_command(
        &self,
        user: UserId,
        project: i64,
        id: &str,
        operation: &CommandWorkerOperation,
        now: i64,
    ) -> Result<WorkspaceCommandRecovery, StorageError> {
        self.db
            .transaction(|tx| async move {
                Store::new(tx)
                    .native_language_command_inner(user, project, id, operation, now)
                    .await
            })
            .await
    }
    pub(super) async fn native_language_command_inner(
        &self,
        user: UserId,
        project: i64,
        id: &str,
        operation: &CommandWorkerOperation,
        now: i64,
    ) -> Result<WorkspaceCommandRecovery, StorageError> {
        operation.validate().map_err(StorageError::InvalidRequest)?;
        if operation.expected().scope.id != id {
            return Err(conflict());
        }
        let store = self;
        let current = store.original_command(user, project, id).await?;
        let (old, digest, may_dispatch) = store.command_credentials(user, project, id).await?;
        let worker = operation.worker();
        let expected = operation.expected();
        let original = current.scope.generation == expected.scope.generation
            && current.scope.root_identity == expected.scope.root_identity
            && current.scope.root == expected.scope.root
            && current.command.invocation == expected.command.invocation;
        if !original {
            return Err(conflict());
        }
        let owned = old == worker.id && digest == worker.secret_digest();
        let reserved = owned
            && !may_dispatch
            && current.command.native_settlement_pending
            && current
                .command
                .worker
                .as_ref()
                .is_some_and(|worker| worker.active)
            && current.command.outcome_digest.is_none()
            && !current.command.command_cleanup_confirmed;
        match operation {
            CommandWorkerOperation::ConfirmCancellation { .. } => {
                let unadmitted = old.is_empty()
                    && digest.is_empty()
                    && current.command.worker.is_none()
                    && !current.command.native_settlement_pending
                    && !current.command.command_cleanup_confirmed
                    && current.scope.worker.is_none();
                if (!owned && !unadmitted)
                    || may_dispatch
                    || current.command.phase != ActionPhase::Cancelled
                    || expected.command.phase != ActionPhase::Cancelled
                    || current.scope != expected.scope
                    || current.command.revision != expected.command.revision
                    || (!current.command.native_settlement_pending && !unadmitted)
                    || current.command.outcome_digest.is_some()
                    || current.activity.file_recovery
                    || (!unadmitted
                        && !current
                            .command
                            .worker
                            .as_ref()
                            .is_some_and(|worker| worker.active))
                {
                    return Err(conflict());
                }
                // Only the original reserved owner may confirm no dispatch.
                // Repeat ACKs join the receipt without creating an outcome or
                // changing the user's cancellation revision.
                if !current.command.command_cleanup_confirmed {
                    if &current != expected {
                        return Err(conflict());
                    }
                    store.reserve_command_settlement(user, project, id).await?;
                    store.db.execute("UPDATE language_actions SET command_worker_id=?,command_worker_digest=?,command_worker_may_dispatch=0,native_settlement_pending=1,command_cleanup_confirmed=1,updated_at=? WHERE user_id=? AND project_id=? AND id=?", &[DbValue::Text(worker.id.clone()),DbValue::Text(worker.secret_digest()),DbValue::Int(now),DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.into())]).await?;
                }
            }
            CommandWorkerOperation::RejectUnclaimed { message, .. } => {
                let outcome = CommandOutcome::Rejected {
                    message: message.clone(),
                };
                let encoded = serde_json::to_string(&outcome).map_err(|_| conflict())?;
                let fingerprint = openwebide_core::plugins::content_digest(encoded.as_bytes());
                // A lost rejection acknowledgement can join only its original
                // private worker and exact outcome, never become a dispatch.
                if owned
                    && !may_dispatch
                    && current.command.phase == ActionPhase::Rejected
                    && current.command.command_cleanup_confirmed
                    && current.command.native_settlement_pending
                    && current.command.outcome_digest.as_deref() == Some(fingerprint.as_str())
                {
                    return Ok(current);
                }
                if &current != expected
                    || (current.command.worker.is_some() && !reserved)
                    || (current.command.native_settlement_pending && !reserved)
                    || current.activity.file_recovery
                    || !(current.command.phase == ActionPhase::Running
                        || (reserved && current.command.phase == ActionPhase::Approved))
                {
                    return Err(conflict());
                }
                let revision = current
                    .command
                    .revision
                    .checked_add(1)
                    .and_then(|v| i64::try_from(v).ok())
                    .ok_or_else(conflict)?;
                store.reserve_command_settlement(user, project, id).await?;
                store.db.execute("UPDATE language_actions SET revision=?,phase='rejected',outcome=?,outcome_digest=?,command_worker_id=?,command_worker_digest=?,command_worker_may_dispatch=0,native_settlement_pending=1,command_cleanup_confirmed=1,updated_at=? WHERE user_id=? AND project_id=? AND id=?", &[DbValue::Int(revision),DbValue::Text(encoded),DbValue::Text(fingerprint),DbValue::Text(worker.id.clone()),DbValue::Text(worker.secret_digest()),DbValue::Int(now),DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.into())]).await?;
            }
            CommandWorkerOperation::Claim { admission, .. } => {
                let mut claimed = expected.clone();
                claimed.command.worker = Some(worker.state());
                claimed.command.native_settlement_pending = true;
                claimed.activity.language_command = true;
                if current == claimed
                    && owned
                    && (!matches!(admission, CommandAdmission::Start { .. }) || may_dispatch)
                {
                    return Ok(current);
                }
                if &current != expected
                    || (old == worker.id && !reserved)
                    || current
                        .scope
                        .worker
                        .as_ref()
                        .is_some_and(|file| file.id == worker.id)
                {
                    return Err(conflict());
                }
                match admission {
                    CommandAdmission::Start { authority } => {
                        if (current.command.worker.is_some() && !reserved)
                            || (current.command.native_settlement_pending && !reserved)
                            || current.command.phase != ActionPhase::Running
                            || current.activity.file_recovery
                        {
                            return Err(conflict());
                        }
                        let record = store
                            .language_action(user, project, Some(id))
                            .await?
                            .ok_or_else(conflict)?;
                        if record
                            .approval
                            .command_invocation()
                            .map_err(StorageError::InvalidValue)?
                            != current.command.invocation
                        {
                            return Err(conflict());
                        }
                        let call = Call {
                            grant: authority.grant.clone(),
                            session_id: authority.session_id,
                            prepared: record.approval.prepared.clone(),
                            input: record.approval.input.clone(),
                        };
                        store
                            .action_authority(user, project, &call, &record.approval, now)
                            .await?;
                        let workspace = store.action_workspace(user, project, id).await?;
                        if workspace.journal.progress.phase != TransactionPhase::Committed {
                            return Err(conflict());
                        }
                        store
                            .validate_action_editors(user, project, &workspace)
                            .await?;
                    }
                    CommandAdmission::Recover => {
                        if !current.command.native_settlement_pending
                            || current.command.worker.is_none()
                        {
                            return Err(conflict());
                        }
                    }
                }
                store.reserve_command_settlement(user, project, id).await?;
                store.db.execute("UPDATE language_actions SET command_worker_id=?,command_worker_digest=?,command_worker_may_dispatch=?,native_settlement_pending=1,updated_at=? WHERE user_id=? AND project_id=? AND id=?",&[DbValue::Text(worker.id.clone()),DbValue::Text(worker.secret_digest()),DbValue::Int(i64::from(matches!(admission,CommandAdmission::Start{..}))),DbValue::Int(now),DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.into())]).await?;
            }
            CommandWorkerOperation::Finish {
                outcome, cleaned, ..
            } => {
                if !owned || !current.command.native_settlement_pending {
                    return Err(conflict());
                }
                let encoded = serde_json::to_string(outcome).map_err(|_| conflict())?;
                let fingerprint = openwebide_core::plugins::content_digest(encoded.as_bytes());
                if current.command.outcome_digest.as_deref() == Some(&fingerprint) {
                    // Retry may add positive cleanup, but cannot undo it or
                    // replace the originally published outcome.
                    if *cleaned && !current.command.command_cleanup_confirmed {
                        store.db.execute("UPDATE language_actions SET command_cleanup_confirmed=1,updated_at=? WHERE user_id=? AND project_id=? AND id=?",&[DbValue::Int(now),DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.into())]).await?;
                    }
                } else {
                    if &current != expected
                        || current.command.outcome_digest.is_some()
                        || current.command.phase != ActionPhase::Running
                        || (matches!(
                            outcome,
                            CommandOutcome::Completed { .. } | CommandOutcome::Rejected { .. }
                        ) && !*cleaned)
                    {
                        return Err(conflict());
                    }
                    let next = current
                        .command
                        .revision
                        .checked_add(1)
                        .and_then(|v| i64::try_from(v).ok())
                        .ok_or_else(conflict)?;
                    store.db.execute("UPDATE language_actions SET revision=?,phase=?,outcome=?,outcome_digest=?,command_cleanup_confirmed=?,updated_at=? WHERE user_id=? AND project_id=? AND id=?",&[DbValue::Int(next),DbValue::Text(phase_text(outcome.phase())?),DbValue::Text(encoded),DbValue::Text(fingerprint),DbValue::Int(i64::from(*cleaned || current.command.command_cleanup_confirmed)),DbValue::Int(now),DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.into())]).await?;
                }
            }
            CommandWorkerOperation::Release { .. } | CommandWorkerOperation::Settle { .. } => {
                let settle = matches!(operation, CommandWorkerOperation::Settle { .. });
                // A released ID remains a tombstone. Only a repeated final
                // acknowledgement is permitted; it cannot inspect/dispatch.
                if old == worker.id
                    && current.command.command_cleanup_confirmed
                    && !current.activity.file_recovery
                    && (!settle || current.command.phase.terminal())
                    && digest.is_empty()
                    && ((!current.command.native_settlement_pending && settle)
                        || (!settle && current.command.native_settlement_pending))
                {
                    if settle {
                        store
                            .retain_command_settlement(user, project, &current, worker, true)
                            .await?;
                    }
                    return Ok(current);
                }
                if !owned
                    || &current != expected
                    || !current.command.command_cleanup_confirmed
                    || current.activity.file_recovery
                    || (settle && !current.command.phase.terminal())
                    || (!settle
                        && !matches!(
                            current.command.phase,
                            ActionPhase::Rejected | ActionPhase::Uncertain
                        ))
                {
                    return Err(conflict());
                }
                store.db.execute("UPDATE language_actions SET command_worker_digest='',command_worker_may_dispatch=0,native_settlement_pending=?,updated_at=? WHERE user_id=? AND project_id=? AND id=?",&[DbValue::Int(i64::from(!settle)),DbValue::Int(now),DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.into())]).await?;
            }
        }
        let current = store.original_command(user, project, id).await?;
        if matches!(operation, CommandWorkerOperation::Settle { .. }) {
            store
                .retain_command_settlement(user, project, &current, worker, false)
                .await?;
        }
        Ok(current)
    }
}
