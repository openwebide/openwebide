//! Cancellation retains no result. Only the original durable native owner can
//! certify no dispatch and retire after the original backend acknowledges it.
use super::*;
use crate::workspace_transactions::original_command::OriginalCommandJournal;
use protocol::actions::ActionPhase;

impl<B: JournalBackend> Journal<B> {
    async fn reconcile_cancelled(
        &self,
        coordinator: &Coordinator,
        namespace: &Namespace,
        pending: &crate::native_recovery::PendingCommand,
    ) -> Result<(), String> {
        if !pending.present(namespace)? {
            return Ok(());
        }
        let (folder, permit) = pending.reopen()?;
        let lease = coordinator
            .prepare(coordinator.acquire_recovery(folder.clone(), permit.clone()))
            .await??;
        if !pending.present(namespace)? {
            return Ok(());
        }
        let (mut current, issuer) = coordinator.prepare(self.inspect()).await??;
        if issuer != pending.authority
            || current.command.phase != ActionPhase::Cancelled
            || current.activity.file_recovery
            || current.command.outcome_digest.is_some()
        {
            return Err("Recover the original cancelled action and files before continuing".into());
        }
        namespace.command_cancelled(&folder, &lease, &permit, &self.worker, &current)?;
        if current.command.native_settlement_pending || !current.command.command_cleanup_confirmed {
            if !current.command.command_cleanup_confirmed {
                current = self
                    .mutate(CommandWorkerOperation::ConfirmCancellation {
                        expected: Box::new(current),
                        worker: self.worker.clone(),
                    })
                    .await?;
            }
            current = self
                .mutate(CommandWorkerOperation::Settle {
                    expected: Box::new(current),
                    worker: self.worker.clone(),
                })
                .await?;
        }
        namespace.command_settled(&folder, &lease, &permit, &current)
    }
}

impl Host {
    pub(super) fn observe_unstarted(
        &self,
        namespace: Arc<Namespace>,
        pending: crate::native_recovery::PendingCommand,
    ) -> Result<(), String> {
        let Some(observation) = self
            .inner
            .command_observations
            .reserve(pending.authority.secret_digest()?)?
        else {
            return Ok(());
        };
        let registration = self.inner.coordinator.worker()?;
        let host = self.clone();
        tokio::spawn(async move {
            let _observation = observation;
            let _registration = registration;
            if pending.cancelled {
                let _ = host.recover_cancelled(namespace, pending).await;
                return;
            }
            if let Err(error) = host.observe_unstarted_inner(namespace, pending).await {
                tracing::debug!(%error,"Original unstarted command observation will retry");
            }
        });
        Ok(())
    }
    async fn observe_unstarted_inner(
        &self,
        namespace: Arc<Namespace>,
        pending: crate::native_recovery::PendingCommand,
    ) -> Result<(), String> {
        if !pending.present(&namespace)? {
            return Ok(());
        }
        let backend = OriginalCommandJournal::new(
            pending.origin.clone(),
            pending.authority.clone(),
            pending.worker.clone(),
        )?
        .with_namespace(namespace.clone());
        let journal = Journal {
            backend: Arc::new(backend),
            user: pending.authority.user,
            project: pending.authority.project,
            invocation: pending.invocation.clone(),
            worker: pending.worker.clone(),
            recoveries: self.inner.command_recoveries.clone(),
        };
        let (current, issuer) = self.inner.coordinator.prepare(journal.inspect()).await??;
        if issuer != pending.authority {
            return Err("Original command issuer changed".into());
        }
        match current.command.phase {
            ActionPhase::Cancelled => {
                self.recover_cancelled(namespace, pending).await?;
            }
            ActionPhase::Rejected | ActionPhase::Acknowledged if pending.can_reject => {
                let authority = pending.authority.clone();
                let invocation = pending.invocation.clone();
                if let Some(progress) = self.recover_pending(namespace, pending, true, None).await?
                {
                    self.retain_pending_progress(&authority, &invocation, &progress)
                        .await?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    pub(crate) async fn cancel_original_control(
        &self,
        owner: &str,
        project: i64,
        invocation: &CommandInvocation,
    ) -> Result<CommandProgress, String> {
        let (user, mode) = if let Some(user) = owner.strip_prefix("paired/user:") {
            (user, openwebide_core::WorkspaceMode::Local)
        } else if let Some(user) = owner.strip_prefix("user:") {
            (user, openwebide_core::WorkspaceMode::Remote)
        } else {
            return Err("Original command owner is unavailable".into());
        };
        let user = user
            .parse::<i64>()
            .map_err(|_| "Original command owner is invalid")?;
        let namespace = Arc::new(self.inner.coordinator.recovery_namespace()?);
        let pending = namespace
            .pending_command(user, project, mode, invocation)?
            .ok_or("Original cancelled command reservation is unavailable")?;
        if self.recover_cancelled(namespace, pending).await? {
            Ok(CommandProgress::Cancelled)
        } else {
            Err("The original unstarted action has not been cancelled".into())
        }
    }
    pub(super) async fn recover_cancelled(
        &self,
        namespace: Arc<Namespace>,
        pending: crate::native_recovery::PendingCommand,
    ) -> Result<bool, String> {
        if !(pending.cancelled || pending.can_cancel) {
            return Ok(false);
        }
        if !pending.present(&namespace)? {
            return Ok(true);
        }
        let coordinator = self.inner.coordinator.clone();
        let journal = Journal {
            backend: Arc::new(
                OriginalCommandJournal::new(
                    pending.origin.clone(),
                    pending.authority.clone(),
                    pending.worker.clone(),
                )?
                .with_namespace(namespace.clone()),
            ),
            user: pending.authority.user,
            project: pending.authority.project,
            invocation: pending.invocation.clone(),
            worker: pending.worker.clone(),
            recoveries: self.inner.command_recoveries.clone(),
        };
        // Observing an Approved reservation never claims the producer guard or
        // acquires a folder lease, so a legitimate later Start remains possible.
        if !pending.cancelled {
            let (current, issuer) = coordinator.prepare(journal.inspect()).await??;
            if issuer != pending.authority || current.command.phase != ActionPhase::Cancelled {
                return Ok(false);
            }
        }
        let Some(recovery) = self
            .inner
            .command_recoveries
            .reserve(pending.authority.secret_digest()?)?
        else {
            return Ok(true);
        };
        let registration = coordinator.worker()?;
        let host = self.clone();
        tokio::spawn(async move {
            let _registration = registration;
            let _recovery = recovery;
            loop {
                if let Ok(false) = pending.present(&namespace) {
                    return;
                }
                if journal
                    .reconcile_cancelled(&coordinator, &namespace, &pending)
                    .await
                    .is_ok()
                {
                    let _ = host
                        .retain_pending_progress(
                            &pending.authority,
                            &pending.invocation,
                            &CommandProgress::Cancelled,
                        )
                        .await;
                    return;
                }
                tokio::select! { biased;
                    ()=coordinator.stopped()=>return,
                    ()=tokio::time::sleep(Duration::from_secs(2))=>{}
                }
            }
        });
        Ok(true)
    }
}
