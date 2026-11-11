//! One journal/authority contract for the browser and native transports.
use super::*;
use openwebide_core::plugins::languages::intelligence::{
    Selection,
    actions::{ActionJournalRequest, AuthorizedCommand, CommandDispatch, CommandOperation},
};

impl<D: Db> Store<D> {
    pub async fn language_action_request(
        &self,
        user: UserId,
        project: i64,
        request: &ActionJournalRequest,
        now: i64,
    ) -> Result<Option<ActionRecord>, StorageError> {
        use ActionJournalRequest as Request;
        let record = match request {
            Request::Load { id } => {
                return self.language_action(user, project, id.as_deref()).await;
            }
            Request::Approve { call, approval } => {
                self.approve_language_action(user, project, call, approval, now)
                    .await?
            }
            Request::Claim { id, revision, call } => {
                self.claim_language_action(user, project, id, *revision, call, now)
                    .await?
            }
            Request::Finish {
                id,
                revision,
                outcome,
            } => {
                self.finish_language_action(user, project, id, *revision, outcome, now)
                    .await?
            }
            Request::RejectUnstarted {
                id,
                revision,
                message,
            } => {
                self.reject_unstarted_language_action(user, project, id, *revision, message, now)
                    .await?
            }
            Request::Acknowledge { id, revision, note } => {
                self.acknowledge_language_action(user, project, id, *revision, note, now)
                    .await?
            }
            Request::Cancel { id, revision } => {
                self.cancel_language_action(user, project, id, *revision, now)
                    .await?
            }
        };
        Ok(Some(record))
    }

    pub async fn authorize_language_command(
        &self,
        user: UserId,
        project: i64,
        request: &CommandDispatch,
        now: i64,
    ) -> Result<AuthorizedCommand, StorageError> {
        self.db.transaction(|tx| async move {
            let store = Store::new(tx);
            let record = store.language_action(user, project, Some(&request.id)).await?
                .ok_or_else(|| StorageError::Conflict("The approved language action is unavailable".into()))?;
            let invocation = record.invocation().map_err(StorageError::Conflict)?;
            if request.operation == CommandOperation::Start {
                if record.phase!=ActionPhase::Running {return Err(StorageError::Conflict("A command with an uncertain outcome cannot be restarted".into()));}
                let call = request.call.as_ref().ok_or_else(|| StorageError::Conflict("Starting a language command requires fresh manual authority".into()))?;
                store.action_authority(user, project, call, &record.approval, now).await?;
                let workspace = store.action_workspace(user, project, &request.id).await?;
                if workspace.journal.progress.phase != TransactionPhase::Committed {
                    return Err(StorageError::Conflict("The complete reviewed edit must remain committed before command dispatch".into()));
                }
                store.validate_action_editors(user,project,&workspace).await?;
            }
            Ok(AuthorizedCommand {
                selection: Selection { user: user.get(), project, prepared: record.approval.prepared, input: record.approval.input },
                invocation,
                operation: request.operation,
            })
        }).await
    }
}
