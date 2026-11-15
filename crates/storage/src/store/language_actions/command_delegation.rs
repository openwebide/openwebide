//! Paired authentication only; native_language_command owns every transition.
use super::*;
use openwebide_core::{
    plugins::languages::intelligence::actions::{
        CommandDelegationRequest, CommandJournalOperation, CommandWorkerOperation,
        DelegatedCommandRequest,
    },
    workspace_transaction::{WorkspaceJournalDelegation, WorkspaceJournalResponse},
};
fn denied() -> StorageError {
    StorageError::Conflict("Original paired command authority changed".into())
}
impl<D: Db> Store<D> {
    pub async fn delegate_language_command(
        &self,
        user: UserId,
        project: i64,
        request: &CommandDelegationRequest,
        now: i64,
    ) -> Result<WorkspaceJournalDelegation, StorageError> {
        if !valid_id(&request.id) {
            return Err(denied());
        }
        request
            .worker
            .validate()
            .map_err(StorageError::InvalidRequest)?;
        self.db.transaction(|tx|async move {
            let store=Store::new(tx);
            if store.get_project(project,user).await?.mode!=WorkspaceMode::Local {return Err(denied());}
            let recovery=store.workspace_command_recovery(user,project,&request.id).await?.ok_or_else(denied)?;
            let reserved=recovery.command.worker.as_ref().is_some_and(|worker|worker.id==request.worker.id);
            if reserved {
                let (id,digest,dispatch)=store.command_credentials(user,project,&request.id).await?;
                if id!=request.worker.id || digest!=request.worker.secret_digest || dispatch
                    || !recovery.command.native_settlement_pending || recovery.command.outcome_digest.is_some() {
                    return Err(denied());
                }
            }
            let scope=recovery.scope;
            // Active command grants follow command ownership, never file terminal.
            store.db.execute("DELETE FROM language_command_delegations WHERE user_id=? AND ((admitted=0 AND expires_at<=?) OR (admitted=1 AND NOT EXISTS(SELECT 1 FROM language_actions a WHERE a.user_id=language_command_delegations.user_id AND a.project_id=language_command_delegations.project_id AND a.id=language_command_delegations.transaction_id AND a.native_settlement_pending=1 AND a.command_worker_id=language_command_delegations.worker_id AND a.command_worker_digest=language_command_delegations.worker_digest)))",&[DbValue::Int(user.get()),DbValue::Int(now)]).await?;
            store.db.execute("DELETE FROM language_command_delegations WHERE user_id=? AND project_id=? AND transaction_id=? AND worker_id=? AND admitted=0",&[DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(request.id.clone()),DbValue::Text(request.worker.id.clone())]).await?;
            let count=store.db.execute("SELECT COUNT(*) FROM language_command_delegations WHERE user_id=?",&[DbValue::Int(user.get())]).await?;
            let count=count.rows[0].get_int(0)?;
            if count>=128 {
                let replacing=count==128 && recovery.command.worker.as_ref().is_some_and(|worker|worker.active)
                    && !store.db.execute("SELECT 1 FROM language_command_delegations WHERE user_id=? AND project_id=? AND transaction_id=? AND admitted=1 LIMIT 1",&[DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(request.id.clone())]).await?.rows.is_empty();
                if !replacing {return Err(StorageError::Conflict("Too many paired command delegations; complete an existing recovery".into()));}
            }
            let random=store.db.execute("SELECT lower(hex(randomblob(16))),lower(hex(randomblob(16)))",&[]).await?;
            let grant=WorkspaceJournalDelegation{id:random.rows[0].get_text(0)?.into(),secret:random.rows[0].get_text(1)?.into(),user:user.get(),project,scope};
            // The original reserved worker already owns cleanup. Its callback
            // lifetime cannot depend on whether a fresh Start grant succeeds.
            store.db.execute("INSERT INTO language_command_delegations (id,secret_digest,user_id,project_id,transaction_id,generation,scope,worker_id,worker_digest,expires_at,admitted) VALUES (?,?,?,?,?,?,?,?,?,?,?)",&[DbValue::Text(grant.id.clone()),DbValue::Text(openwebide_core::plugins::content_digest(grant.secret.as_bytes())),DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(request.id.clone()),DbValue::Text(grant.scope.generation.clone()),DbValue::Text(serde_json::to_string(&grant.scope).map_err(|_|denied())?),DbValue::Text(request.worker.id.clone()),DbValue::Text(request.worker.secret_digest.clone()),DbValue::Int(now.saturating_add(45)),DbValue::Int(i64::from(reserved))]).await?;
            Ok(grant)
        }).await
    }
    pub async fn delegated_language_command(
        &self,
        request: &DelegatedCommandRequest,
        now: i64,
    ) -> Result<WorkspaceJournalResponse, StorageError> {
        request.validate().map_err(StorageError::InvalidRequest)?;
        self.db.transaction(|tx|async move {
            let store=Store::new(tx);let grant=&request.delegation;
            let rows=store.db.execute("SELECT secret_digest,user_id,project_id,scope,worker_id,worker_digest,admitted,expires_at FROM language_command_delegations WHERE id=?",&[DbValue::Text(grant.id.clone())]).await?;
            let row=rows.rows.first().ok_or_else(denied)?;
            let expected:openwebide_core::workspace_transaction::WorkspaceTransactionScope=serde_json::from_str(row.get_text(3)?).map_err(|_|denied())?;
            if row.get_text(0)?!=openwebide_core::plugins::content_digest(grant.secret.as_bytes()) || row.get_int(1)?!=grant.user || row.get_int(2)?!=grant.project || expected!=grant.scope || row.get_text(4)?!=request.worker.id || row.get_text(5)?!=request.worker.secret_digest() {return Err(denied());}
            let admitted=row.get_int(6)?!=0;
            if !admitted && row.get_int(7)?<=now {return Err(denied());}
            let user=UserId::new(grant.user);let project=grant.project;
            if store.get_project(project,user).await?.mode!=WorkspaceMode::Local {return Err(denied());}
            let current=store.workspace_command_recovery(user,project,&grant.scope.id).await?.ok_or_else(denied)?;
            if current.scope.generation!=expected.generation || current.scope.root_identity!=expected.root_identity || current.scope.root!=expected.root {return Err(denied());}
            let terminal_ack=matches!(&request.operation,CommandJournalOperation::Worker{operation} if matches!(**operation,CommandWorkerOperation::Release{..}|CommandWorkerOperation::Settle{..}));
            let owns=current.command.worker.as_ref().is_some_and(|worker|worker.id==request.worker.id && worker.active);
            if admitted && !owns && !terminal_ack {return Err(denied());}
            match &request.operation {
                CommandJournalOperation::AcknowledgeSettlement { .. }=>Err(denied()),
                CommandJournalOperation::Scope=>Ok(WorkspaceJournalResponse::CommandScope{recovery:Some(Box::new(current))}),
                CommandJournalOperation::Outcome {expected}=> {
                    if !admitted {return Err(denied());}
                    store.native_command_outcome(user,project,&grant.scope.id,expected).await
                }
                CommandJournalOperation::Worker{operation}=> {
                    if !admitted && !matches!(**operation,CommandWorkerOperation::Claim{..}|CommandWorkerOperation::RejectUnclaimed{..}) {return Err(denied());}
                    let recovery=store.native_language_command_inner(user,project,&grant.scope.id,operation,now).await?;
                    if matches!(**operation,CommandWorkerOperation::Claim{..}|CommandWorkerOperation::RejectUnclaimed{..}) {
                        store.db.execute("UPDATE language_command_delegations SET admitted=1 WHERE id=?",&[DbValue::Text(grant.id.clone())]).await?;
                        store.db.execute("DELETE FROM language_command_delegations WHERE user_id=? AND project_id=? AND transaction_id=? AND id<>?",&[DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(grant.scope.id.clone()),DbValue::Text(grant.id.clone())]).await?;
                    }
                    Ok(WorkspaceJournalResponse::CommandScope{recovery:Some(Box::new(recovery))})
                }
            }
        }).await
    }
}
