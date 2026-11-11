//! Scoped Local-host credentials. Issuing a bootstrap requires the account;
//! using it also requires the paired worker's private preimage. No deployment
//! secret is shared with that host, and admitted credentials never expire by age.
use super::*;
use openwebide_core::workspace_transaction::{
    DelegateWorkspaceTransaction, DelegatedWorkspaceJournalOperation as Operation,
    DelegatedWorkspaceJournalRequest, WorkspaceJournalDelegation, WorkspaceJournalResponse,
    WorkspaceTransactionScope,
};

const BOOTSTRAP_SECONDS: i64 = 45;
const MAX_DELEGATIONS: i64 = 128;
fn denied() -> StorageError {
    StorageError::Conflict(
        "Paired workspace authority changed; reconnect the original folder".into(),
    )
}
struct Context {
    user: UserId,
    project: i64,
    expected: WorkspaceTransactionScope,
    current: WorkspaceTransactionScope,
    admitted: bool,
}
impl<D: Db> Store<D> {
    pub async fn delegate_workspace_transaction(
        &self,
        user: UserId,
        project: i64,
        id: &str,
        request: &DelegateWorkspaceTransaction,
        now: i64,
    ) -> Result<WorkspaceJournalDelegation, StorageError> {
        request.validate().map_err(StorageError::InvalidRequest)?;
        self.db.transaction(|tx| async move {
            let store=Store::new(tx);
            if store.get_project(project,user).await?.mode != WorkspaceMode::Local { return Err(denied()); }
            let scope=store.workspace_transaction_scope(user,project,id).await?.ok_or_else(denied)?;
            if scope.generation!=request.generation || scope.revision!=request.revision || scope.root.mode!=WorkspaceMode::Local || !openwebide_core::workspace_transaction::companion::fingerprint(&scope.root_identity) { return Err(denied()); }
            if scope.worker.as_ref().is_some_and(|worker|worker.id==request.worker.id) { return Err(denied()); }
            let serialized=serde_json::to_string(&scope).map_err(|e|StorageError::Db(e.to_string()))?;
            if serialized.len()>openwebide_core::workspace_transaction::RECOVERY_SCOPE_BODY_LIMIT { return Err(denied()); }
            // Terminal grants no longer authorize future native calls. Revoking
            // them does not release a physical gate or assert process liveness.
            // Active nonterminal grants never expire by age.
            store.db.execute("DELETE FROM workspace_transaction_delegations WHERE user_id=? AND ((admitted=0 AND expires_at<=?) OR (admitted=1 AND NOT EXISTS (SELECT 1 FROM workspace_transactions t WHERE t.user_id=workspace_transaction_delegations.user_id AND t.project_id=workspace_transaction_delegations.project_id AND t.id=workspace_transaction_delegations.transaction_id AND t.generation=workspace_transaction_delegations.generation AND t.worker_id=workspace_transaction_delegations.worker_id AND t.worker_digest=workspace_transaction_delegations.worker_digest AND t.terminal=0)))", &[DbValue::Int(user.get()),DbValue::Int(now)]).await?;
            // A lost bootstrap reply may be replaced before admission. The
            // obsolete token cannot subsequently win the admission transaction.
            store.db.execute("DELETE FROM workspace_transaction_delegations WHERE user_id=? AND project_id=? AND transaction_id=? AND generation=? AND worker_id=? AND admitted=0", &[DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.into()),DbValue::Text(scope.generation.clone()),DbValue::Text(request.worker.id.clone())]).await?;
            let count=store.db.execute("SELECT COUNT(*) FROM workspace_transaction_delegations WHERE user_id=?",&[DbValue::Int(user.get())]).await?;
            let count=count.rows[0].get_int(0)?;
            if count>=MAX_DELEGATIONS {
                // One bounded replacement slot lets a full set of admitted
                // workers recover. Admission removes the replaced grant. It
                // cannot grow the active set beyond the ordinary quota.
                let replacing=if count==MAX_DELEGATIONS && scope.worker.as_ref().is_some_and(|worker|worker.active) {
                    !store.db.execute("SELECT 1 FROM workspace_transaction_delegations WHERE user_id=? AND project_id=? AND transaction_id=? AND generation=? AND admitted=1 LIMIT 1",&[DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.into()),DbValue::Text(scope.generation.clone())]).await?.rows.is_empty()
                } else {false};
                if !replacing {return Err(StorageError::Conflict("Too many paired workspace delegations; finish an existing operation".into()));}
            }
            let random=store.db.execute("SELECT lower(hex(randomblob(16))),lower(hex(randomblob(16)))",&[]).await?;
            let grant=WorkspaceJournalDelegation {id:random.rows[0].get_text(0)?.into(),secret:random.rows[0].get_text(1)?.into(),user:user.get(),project,scope};
            store.db.execute("INSERT INTO workspace_transaction_delegations (id,secret_digest,user_id,project_id,transaction_id,generation,scope,worker_id,worker_digest,expires_at) VALUES (?,?,?,?,?,?,?,?,?,?)",&[
                DbValue::Text(grant.id.clone()),DbValue::Text(openwebide_core::plugins::content_digest(grant.secret.as_bytes())),DbValue::Int(user.get()),DbValue::Int(project),DbValue::Text(id.into()),DbValue::Text(grant.scope.generation.clone()),DbValue::Text(serialized),DbValue::Text(request.worker.id.clone()),DbValue::Text(request.worker.secret_digest.clone()),DbValue::Int(now.saturating_add(BOOTSTRAP_SECONDS)),
            ]).await?;
            Ok(grant)
        }).await
    }
    async fn delegation_context(
        &self,
        request: &DelegatedWorkspaceJournalRequest,
        now: i64,
    ) -> Result<Context, StorageError> {
        request.validate().map_err(StorageError::InvalidRequest)?;
        let result=self.db.execute("SELECT secret_digest,user_id,project_id,scope,worker_id,worker_digest,admitted,expires_at FROM workspace_transaction_delegations WHERE id=?",&[DbValue::Text(request.delegation.id.clone())]).await?;
        let row = result.rows.first().ok_or_else(denied)?;
        if row.get_text(0)?
            != openwebide_core::plugins::content_digest(request.delegation.secret.as_bytes())
            || row.get_text(4)? != request.worker.id
            || row.get_text(5)? != request.worker.secret_digest()
        {
            return Err(denied());
        }
        let admitted = row.get_int(6)? != 0;
        if !admitted && row.get_int(7)? <= now {
            return Err(denied());
        }
        let expected: WorkspaceTransactionScope =
            serde_json::from_str(row.get_text(3)?).map_err(|_| denied())?;
        if expected != request.delegation.scope
            || request.delegation.user != row.get_int(1)?
            || request.delegation.project != row.get_int(2)?
        {
            return Err(denied());
        }
        let user = UserId::new(row.get_int(1)?);
        let project = row.get_int(2)?;
        if self.get_project(project, user).await?.mode != WorkspaceMode::Local {
            return Err(denied());
        }
        let current = self
            .workspace_transaction_scope(user, project, &expected.id)
            .await?
            .ok_or_else(denied)?;
        if current.generation != expected.generation
            || current.root_identity != expected.root_identity
            || current.root != expected.root
        {
            return Err(denied());
        }
        Ok(Context {
            user,
            project,
            expected,
            current,
            admitted,
        })
    }
    /// Native admission must run while the caller holds physical exclusion.
    /// Activate the bootstrap in the same commit as the durable worker CAS.
    pub async fn delegated_workspace_journal(
        &self,
        request: &DelegatedWorkspaceJournalRequest,
        now: i64,
    ) -> Result<WorkspaceJournalResponse, StorageError> {
        if let Operation::Admit {
            review_digest,
            language_grant,
        } = &request.operation
        {
            let review = openwebide_core::workspace_transaction::language_review_admission(
                review_digest.as_deref(),
                language_grant.as_deref(),
            )
            .map_err(StorageError::InvalidRequest)?;
            return self.db.transaction(|tx| async move {
                let store=Store::new(tx);let context=store.delegation_context(request,now).await?;
                let scope=store.claim_workspace_transaction_worker_inner(context.user,context.project,&context.expected,&request.worker,review,now).await?;
                store.db.execute("UPDATE workspace_transaction_delegations SET admitted=1 WHERE id=?",&[DbValue::Text(request.delegation.id.clone())]).await?;
                store.db.execute("DELETE FROM workspace_transaction_delegations WHERE user_id=? AND project_id=? AND transaction_id=? AND id<>?",&[DbValue::Int(context.user.get()),DbValue::Int(context.project),DbValue::Text(context.expected.id),DbValue::Text(request.delegation.id.clone())]).await?;
                Ok(WorkspaceJournalResponse::WorkerClaimed {scope})
            }).await;
        }
        let context = self.delegation_context(request, now).await?;
        let preflight = matches!(request.operation, Operation::Inspect | Operation::Load);
        if !context.admitted && (!preflight || context.current != context.expected) {
            return Err(denied());
        }
        let same_worker = context
            .current
            .worker
            .as_ref()
            .is_some_and(|worker| worker.id == request.worker.id);
        let active = context
            .current
            .worker
            .as_ref()
            .is_some_and(|worker| worker.active);
        if context.admitted
            && (!same_worker || (!active && !matches!(request.operation, Operation::Release)))
        {
            return Err(denied());
        }
        match &request.operation {
            Operation::Inspect => Ok(WorkspaceJournalResponse::Scope {
                scope: Some(context.current),
            }),
            Operation::Load => {
                let state = self
                    .workspace_transaction(
                        context.user,
                        context.project,
                        Some(&context.expected.id),
                    )
                    .await?
                    .ok_or_else(denied)?;
                if !context.current.matches_state(&state) {
                    return Err(denied());
                }
                Ok(WorkspaceJournalResponse::Loaded {
                    state: Some(Box::new(state)),
                })
            }
            Operation::MutationScope => {
                // Scope is checked under the caller's native physical gate.
                // Bind this second compact read to the exact worker/revision
                // so a concurrent Prepared cancellation cannot authorize I/O.
                let result=self.db.execute("SELECT progress FROM workspace_transactions WHERE user_id=? AND project_id=? AND id=? AND generation=? AND revision=? AND worker_id=? AND worker_digest=?",&[
                    DbValue::Int(context.user.get()),DbValue::Int(context.project),DbValue::Text(context.current.id.clone()),
                    DbValue::Text(context.current.generation.clone()),DbValue::Int(i64::try_from(context.current.revision).map_err(|_|denied())?),
                    DbValue::Text(request.worker.id.clone()),DbValue::Text(request.worker.secret_digest()),
                ]).await?;
                let progress = result.rows.first().ok_or_else(denied)?.get_text(0)?;
                if progress.len()
                    > openwebide_core::workspace_transaction::RECOVERY_SCOPE_BODY_LIMIT
                {
                    return Err(denied());
                }
                let progress: TransactionProgress =
                    serde_json::from_str(progress).map_err(|_| denied())?;
                if !progress.permits_resource_mutation() {
                    return Err(denied());
                }
                Ok(WorkspaceJournalResponse::Scope {
                    scope: Some(context.current),
                })
            }
            Operation::Scope => Ok(WorkspaceJournalResponse::Scope {
                scope: Some(context.current),
            }),
            Operation::Advance { revision, progress } => {
                let mut response = self
                    .advance_workspace_transaction_for_worker(
                        context.user,
                        context.project,
                        &AdvanceWorkspaceTransaction {
                            id: context.expected.id,
                            generation: context.expected.generation,
                            revision: *revision,
                            progress: progress.clone(),
                        },
                        Some(&request.worker),
                        now,
                    )
                    .await?;
                // The browser reloads authoritative editor recovery separately.
                // Keep this native cursor response bounded to metadata, rather
                // than proxying another full editor snapshot through the host.
                response.editor = None;
                Ok(WorkspaceJournalResponse::Advanced { response })
            }
            Operation::Release => {
                self.release_workspace_transaction_worker(
                    context.user,
                    context.project,
                    &context.expected.id,
                    &context.expected.generation,
                    &request.worker,
                    now,
                )
                .await?;
                Ok(WorkspaceJournalResponse::WorkerReleased {
                    generation: context.expected.generation,
                    worker: request.worker.id.clone(),
                })
            }
            Operation::Admit { .. } => unreachable!(),
        }
    }
}
