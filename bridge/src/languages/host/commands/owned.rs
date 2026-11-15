//! One owned command producer above service/paired journal primitives. The task
//! retains the complete folder lease through RPC, native result and backend ACK.
use super::*;
mod callback;
mod cancellation;
mod recovery;
mod settlements;
use crate::{
    folder_coordination::{Coordinator, Folder, Lease},
    native_recovery::{CommandIntent, Namespace, Permit},
    workspace_transactions::JournalBackend,
};
use openwebide_core::{
    plugins::languages::intelligence::actions::{
        CommandAdmission, CommandAuthority, CommandWorkerOperation,
    },
    workspace_transaction::{
        RecoveryAuthority, WorkspaceCommandRecovery, WorkspaceJournalOperation,
        WorkspaceJournalRequest, WorkspaceTransactionWorker,
    },
};
pub(super) struct Journal<B> {
    pub backend: Arc<B>,
    pub user: i64,
    pub project: i64,
    pub invocation: CommandInvocation,
    pub worker: WorkspaceTransactionWorker,
    pub recoveries: Recoveries,
}
struct Ownership {
    namespace: Arc<Namespace>,
    folder: Folder,
    permit: Permit,
    worker: WorkspaceTransactionWorker,
    _recovery: recovery::Recovery,
}
pub(super) struct Dispatch<'a> {
    ownership: &'a Ownership,
    lease: &'a Lease,
}
impl Dispatch<'_> {
    pub(super) fn process_cleanup(&self) -> Result<crate::native_recovery::ProcessCleanup, String> {
        let o = self.ownership;
        o.namespace
            .command_process_cleanup(&o.folder, self.lease, &o.permit, &o.worker)
    }
    pub(super) fn outcome(&self, outcome: &CommandOutcome) -> Result<(), String> {
        let o = self.ownership;
        o.namespace
            .command_outcome(&o.folder, self.lease, &o.permit, &o.worker, outcome, false)
    }
    pub(super) fn stopped(&self) -> Result<(), String> {
        let o = self.ownership;
        o.namespace
            .command_process_stopped(&o.folder, self.lease, &o.permit, &o.worker)
    }
    pub(super) fn record(&self) -> Result<(), String> {
        let o = self.ownership;
        o.namespace
            .command_dispatch(&o.folder, self.lease, &o.permit, &o.worker)
    }
}
impl<B: JournalBackend> Journal<B> {
    async fn reconcile_original(
        self,
        coordinator: Coordinator,
        namespace: Arc<Namespace>,
        pending: &crate::native_recovery::PendingCommand,
        outcome: CommandOutcome,
        mode: openwebide_core::WorkspaceMode,
        recovery: recovery::Recovery,
    ) -> Result<Option<CommandOutcome>, String> {
        if !pending.present(&namespace)? {
            return Ok(None);
        }
        let (folder, permit) = pending.reopen()?;
        let lease = coordinator
            .prepare(coordinator.acquire_recovery(folder.clone(), permit.clone()))
            .await??;
        let Some(current_record) =
            namespace.pending_command(self.user, self.project, mode, &self.invocation)?
        else {
            return Ok(None);
        };
        let recorded = current_record.outcome.is_some();
        let mut result = if let Some(outcome) = current_record.outcome {
            ExecutionResult {
                outcome,
                cleaned: current_record.cleaned,
            }
        } else if current_record.can_reject || current_record.dispatched_cleanup {
            ExecutionResult {
                outcome,
                cleaned: true,
            }
        } else {
            return Ok(None);
        };
        let (current, issuer) = coordinator.prepare(self.inspect()).await??;
        if issuer != pending.authority || current.activity.file_recovery {
            return Err("Recover the original file operation and command before continuing".into());
        }
        let retained_result = !recorded && current.command.outcome_digest.is_some();
        if retained_result {
            let outcome = coordinator
                .prepare(self.backend.command_outcome(&current))
                .await??;
            if (!current_record.dispatched_cleanup
                && !matches!(outcome, CommandOutcome::Rejected { .. }))
                || (current_record.dispatched_cleanup
                    && matches!(outcome, CommandOutcome::Rejected { .. }))
            {
                return Err(
                    "The original native reservation cannot confirm a dispatched outcome".into(),
                );
            }
            result.outcome = outcome;
        }
        if !recorded
            && !retained_result
            && !(current.command.phase == protocol::actions::ActionPhase::Running
                || (!current_record.dispatched_cleanup
                    && current.command.phase == protocol::actions::ActionPhase::Approved))
        {
            return Err("Recover the original file operation and command before continuing".into());
        }
        let owner = Arc::new(Ownership {
            namespace,
            folder,
            permit,
            worker: self.worker.clone(),
            _recovery: recovery,
        });
        Arc::new(self)
            .complete(coordinator, owner, &lease, issuer, result)
            .await
            .map(Some)
    }
    async fn inspect(&self) -> Result<(WorkspaceCommandRecovery, RecoveryAuthority), String> {
        let reply = self
            .backend
            .request(&WorkspaceJournalRequest {
                user: self.user,
                project: self.project,
                id: self.invocation.id.clone(),
                operation: WorkspaceJournalOperation::CommandRecoveryScope,
            })
            .await?;
        let (recovery, authority) =
            reply.command_recovery(self.user, self.project, &self.invocation.id)?;
        if recovery.command.invocation != self.invocation {
            return Err("The original command invocation changed".into());
        }
        Ok((recovery.clone(), authority.clone()))
    }
    async fn mutate(
        &self,
        operation: CommandWorkerOperation,
    ) -> Result<WorkspaceCommandRecovery, String> {
        let reply = self
            .backend
            .request(&WorkspaceJournalRequest {
                user: self.user,
                project: self.project,
                id: self.invocation.id.clone(),
                operation: WorkspaceJournalOperation::Command {
                    operation: Box::new(operation.clone()),
                },
            })
            .await?;
        Ok(reply.command_worker(&operation)?.clone())
    }
    async fn publish(
        &self,
        current: WorkspaceCommandRecovery,
        outcome: &CommandOutcome,
        cleaned: bool,
    ) -> Result<WorkspaceCommandRecovery, String> {
        if current.command.phase.terminal()
            && !current.command.native_settlement_pending
            && current.command.command_cleanup_confirmed
            && current
                .command
                .worker
                .as_ref()
                .is_some_and(|worker| worker.id == self.worker.id && !worker.active)
            && current.command.outcome_digest.as_deref()
                == Some(
                    openwebide_core::plugins::content_digest(
                        &serde_json::to_vec(outcome).map_err(|_| "Invalid original outcome")?,
                    )
                    .as_str(),
                )
        {
            return Ok(current);
        }
        let operation = if current.command.worker.is_none()
            || current.command.phase == protocol::actions::ActionPhase::Approved
        {
            let CommandOutcome::Rejected { message } = outcome else {
                return Err("The original native command was not admitted".into());
            };
            CommandWorkerOperation::RejectUnclaimed {
                expected: Box::new(current),
                worker: self.worker.clone(),
                message: message.clone(),
            }
        } else {
            CommandWorkerOperation::Finish {
                expected: Box::new(current),
                worker: self.worker.clone(),
                outcome: outcome.clone(),
                cleaned,
            }
        };
        self.mutate(operation).await
    }
    pub(super) async fn execute(
        self,
        coordinator: Coordinator,
        entry: Arc<Entry>,
        data: Arc<CommandData>,
        revision: u64,
        authority: CommandAuthority,
    ) -> CommandOutcome {
        match self
            .execute_inner(coordinator, entry, data, revision, authority)
            .await
        {
            Ok(outcome) => outcome,
            // A native intent may already have committed. An integration error
            // never authorizes replay or invents a positive cleanup receipt.
            Err(message) => CommandOutcome::Uncertain { message },
        }
    }
    async fn execute_inner(
        self,
        coordinator: Coordinator,
        entry: Arc<Entry>,
        data: Arc<CommandData>,
        revision: u64,
        authority: CommandAuthority,
    ) -> Result<CommandOutcome, String> {
        let (before, issuer) = self.inspect().await?;
        let session = entry
            .session
            .get()
            .ok_or("The original service is unavailable")?;
        let folder = Folder::capture(&session.root)?;
        let namespace = coordinator.recovery_namespace()?;
        let permit = namespace
            .permit(&folder, &issuer, &before.scope, before.activity)?
            .with_origin(self.backend.recovery_origin())?;
        let lease = coordinator
            .acquire_recovery(folder.clone(), permit.clone())
            .await?;
        let (current, current_issuer) = self.inspect().await?;
        if current_issuer != issuer
            || current.scope.root != before.scope.root
            || current.scope.generation != before.scope.generation
            || current.scope.root_identity != before.scope.root_identity
        {
            return Err(
                "The original command recovery changed while waiting for its folder".into(),
            );
        }
        let (record, new) = namespace.command_intent(
            &folder,
            &lease,
            &permit,
            CommandIntent {
                recovery: &current,
                worker: &self.worker,
            },
        )?;
        if !new {
            // Native restart/reconnect only observes this original durable
            // record. Even a Prepared record is never permission to retry RPC.
            return Ok(record.outcome.unwrap_or_else(||CommandOutcome::Uncertain {message:"The original native command intent is pending; reconnect its original cleanup owner before continuing".into()}));
        }
        let owner = Arc::new(Ownership {
            namespace: Arc::new(namespace),
            folder,
            permit,
            worker: self.worker.clone(),
            _recovery: self
                .recoveries
                .reserve(issuer.secret_digest()?)?
                .ok_or("The original command already has a recovery owner")?,
        });
        let journal = Arc::new(self);
        let claim = journal
            .mutate(CommandWorkerOperation::Claim {
                expected: Box::new(current),
                worker: journal.worker.clone(),
                admission: CommandAdmission::Start { authority },
            })
            .await;
        let result = match claim {
            Ok(_) => {
                owner.namespace.command_admitted(
                    &owner.folder,
                    &lease,
                    &owner.permit,
                    &owner.worker,
                )?;
                leased_execute(
                    &coordinator,
                    &entry,
                    &data,
                    revision,
                    &owner.folder,
                    &lease,
                    Some(&Dispatch {
                        ownership: &owner,
                        lease: &lease,
                    }),
                )
                .await
            }
            Err(_) => ExecutionResult {
                outcome: CommandOutcome::Rejected {
                    message: "Native command admission failed before an LSP command was sent"
                        .into(),
                },
                cleaned: true,
            },
        };
        journal
            .complete(coordinator, owner, &lease, issuer, result)
            .await
    }
    async fn complete(
        self: Arc<Self>,
        coordinator: Coordinator,
        owner: Arc<Ownership>,
        lease: &Lease,
        issuer: RecoveryAuthority,
        result: ExecutionResult,
    ) -> Result<CommandOutcome, String> {
        owner.namespace.command_outcome(
            &owner.folder,
            lease,
            &owner.permit,
            &owner.worker,
            &result.outcome,
            result.cleaned,
        )?;
        if let Some(origin) = self.backend.recovery_origin() {
            let backend =
                crate::workspace_transactions::original_command::OriginalCommandJournal::new(
                    origin.to_owned(),
                    issuer.clone(),
                    self.worker.clone(),
                )?
                .with_namespace(owner.namespace.clone());
            return Arc::new(Journal {
                backend: Arc::new(backend),
                user: self.user,
                project: self.project,
                invocation: self.invocation.clone(),
                worker: self.worker.clone(),
                recoveries: self.recoveries.clone(),
            })
            .complete_recorded(coordinator, owner, lease, issuer, result)
            .await;
        }
        self.complete_recorded(coordinator, owner, lease, issuer, result)
            .await
    }
    async fn complete_recorded(
        self: Arc<Self>,
        coordinator: Coordinator,
        owner: Arc<Ownership>,
        lease: &Lease,
        issuer: RecoveryAuthority,
        result: ExecutionResult,
    ) -> Result<CommandOutcome, String> {
        // The command has a durable outcome before an HTTP callback can be
        // accepted or lost. Browser lifetime is irrelevant to publication.
        let published = match coordinator.prepare(self.inspect()).await {
            Ok(Ok((current, current_issuer))) if current_issuer == issuer => coordinator
                .prepare(self.publish(current, &result.outcome, result.cleaned))
                .await
                .and_then(|value| value),
            _ => Err("Original command outcome publication is pending".into()),
        };
        if let Ok(current) = published
            && current.command.phase.terminal()
            && result.cleaned
        {
            if self.settle(&owner, lease, current).await.is_err() {
                self.wait_for_settlement(coordinator, owner, issuer, result.outcome.clone());
            }
        } else if result.cleaned {
            self.wait_for_settlement(coordinator, owner, issuer, result.outcome.clone());
        }
        Ok(result.outcome)
    }
    async fn settle(
        &self,
        owner: &Ownership,
        lease: &Lease,
        current: WorkspaceCommandRecovery,
    ) -> Result<(), String> {
        owner.namespace.ensure_current()?;
        let settled = self
            .mutate(CommandWorkerOperation::Settle {
                expected: Box::new(current),
                worker: self.worker.clone(),
            })
            .await?;
        owner
            .namespace
            .command_settled(&owner.folder, lease, &owner.permit, &settled)
    }
    fn wait_for_settlement(
        self: Arc<Self>,
        coordinator: Coordinator,
        owner: Arc<Ownership>,
        issuer: RecoveryAuthority,
        outcome: CommandOutcome,
    ) {
        let Ok(registration) = coordinator.worker() else {
            return;
        };
        tokio::spawn(async move {
            let _registration = registration;
            loop {
                tokio::select! {biased; ()=coordinator.stopped()=>return, ()=tokio::time::sleep(Duration::from_secs(2))=>{}}
                match owner.namespace.has_original_command(
                    &owner.permit,
                    &owner.worker,
                    &self.invocation,
                ) {
                    Ok(false) => return,
                    Err(_) => continue,
                    Ok(true) => {}
                }
                let Ok(Ok((_, current_issuer))) = coordinator.prepare(self.inspect()).await else {
                    continue;
                };
                if current_issuer != issuer {
                    continue;
                }
                let lease = tokio::select! {biased; ()=coordinator.stopped()=>return, lease=coordinator.acquire_recovery(owner.folder.clone(),owner.permit.clone())=>match lease {Ok(lease)=>lease,Err(_)=>continue}};
                match owner.namespace.has_original_command(
                    &owner.permit,
                    &owner.worker,
                    &self.invocation,
                ) {
                    Ok(false) => return,
                    Err(_) => continue,
                    Ok(true) => {}
                }
                let Ok(Ok((current, current_issuer))) = coordinator.prepare(self.inspect()).await
                else {
                    continue;
                };
                if current_issuer != issuer {
                    continue;
                }
                let Ok(current) = self.publish(current, &outcome, true).await else {
                    continue;
                };
                if current.command.phase.terminal()
                    && self.settle(&owner, &lease, current).await.is_ok()
                {
                    return;
                }
                // The registry, not a lease held across a user decision, keeps
                // unrelated/native writers out while manual review is pending.
            }
        });
    }
}
