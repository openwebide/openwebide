use super::*;
use crate::workspace_transactions::original_command::OriginalCommandJournal;
use openwebide_core::WorkspaceMode;

#[derive(Clone)]
pub(super) struct Recovery {
    _claim: Arc<Claim>,
}
struct Claim {
    recoveries: Arc<Mutex<std::collections::BTreeSet<String>>>,
    key: String,
}
impl Drop for Claim {
    fn drop(&mut self) {
        self.recoveries.lock().unwrap().remove(&self.key);
    }
}
impl Recoveries {
    pub(super) fn reserve(&self, key: String) -> Result<Option<Recovery>, String> {
        let mut recoveries = self.0.lock().unwrap();
        if recoveries.contains(&key) {
            return Ok(None);
        }
        if recoveries.len() >= MAX_COMMANDS {
            return Err("Too many original commands are recovering".into());
        }
        recoveries.insert(key.clone());
        Ok(Some(Recovery {
            _claim: Arc::new(Claim {
                recoveries: self.0.clone(),
                key,
            }),
        }))
    }
}
impl Host {
    pub(super) async fn retain_pending_progress(
        &self,
        authority: &RecoveryAuthority,
        invocation: &CommandInvocation,
        progress: &CommandProgress,
    ) -> Result<(), String> {
        if !matches!(
            progress,
            CommandProgress::Cancelled
                | CommandProgress::Finished {
                    outcome: CommandOutcome::Rejected { .. }
                }
        ) {
            return Ok(());
        }
        let owner = match authority.root.mode {
            WorkspaceMode::Local => format!("paired/user:{}", authority.user),
            WorkspaceMode::Remote => format!("user:{}", authority.user),
        };
        if let Some(scope) = self.command_control_scope(&owner, authority.project, invocation)? {
            self.retain_original_control(scope, invocation, progress)
                .await?;
        }
        Ok(())
    }
    pub(crate) async fn recover_original_control(
        &self,
        owner: &str,
        project: i64,
        invocation: &CommandInvocation,
        stop: bool,
        outcome: Option<CommandOutcome>,
    ) -> Result<Option<CommandProgress>, String> {
        let (user, mode) = if let Some(user) = owner.strip_prefix("paired/user:") {
            (user, WorkspaceMode::Local)
        } else if let Some(user) = owner.strip_prefix("user:") {
            (user, WorkspaceMode::Remote)
        } else {
            return Ok(None);
        };
        let Ok(user) = user.parse::<i64>() else {
            return Ok(None);
        };
        let namespace = self.inner.coordinator.recovery_namespace()?;
        let Some(pending) = namespace.pending_command(user, project, mode, invocation)? else {
            return Ok(None);
        };
        if pending.cancelled {
            self.recover_cancelled(Arc::new(namespace), pending).await?;
            return Ok(Some(CommandProgress::Cancelled));
        }
        self.recover_pending(Arc::new(namespace), pending, stop, outcome)
            .await
    }
    pub(super) async fn recover_pending(
        &self,
        namespace: Arc<Namespace>,
        pending: crate::native_recovery::PendingCommand,
        stop: bool,
        outcome: Option<CommandOutcome>,
    ) -> Result<Option<CommandProgress>, String> {
        let known = pending
            .outcome
            .clone()
            .map(|outcome| CommandProgress::Finished { outcome });
        if known.is_none()
            && !pending.dispatched_cleanup
            && (!pending.can_reject || (!stop && !pending.process_cleanup_confirmed))
        {
            return Ok(None);
        }
        let outcome = if pending.dispatched_cleanup && known.is_none() {
            CommandOutcome::Uncertain {
                message: "The original execution host was lost after command dispatch. Its original process cleanup is confirmed; review the command effects before continuing".into(),
            }
        } else if pending.process_cleanup_confirmed && known.is_none() {
            CommandOutcome::Rejected {
                message: "The original execution host stopped before command dispatch; its process cleanup is confirmed".into(),
            }
        } else {
            pending
                .outcome
                .clone()
                .or(outcome)
                .unwrap_or_else(|| CommandOutcome::Rejected {
                    message: "The original native host stopped before command dispatch".into(),
                })
        };
        if known.is_none()
            && !pending.dispatched_cleanup
            && !matches!(outcome, CommandOutcome::Rejected { .. })
        {
            return Ok(None);
        }
        let start = (|| -> Result<_, String> {
            let Some(recovery) = self
                .inner
                .command_recoveries
                .reserve(pending.authority.secret_digest()?)?
            else {
                return Ok(None);
            };
            let backend = Arc::new(
                OriginalCommandJournal::new(
                    pending.origin.clone(),
                    pending.authority.clone(),
                    pending.worker.clone(),
                )?
                .with_namespace(namespace.clone()),
            );
            let coordinator = self.inner.coordinator.clone();
            let registration = coordinator.worker()?;
            let retry = known.is_some() || pending.process_cleanup_confirmed;
            let recoveries = self.inner.command_recoveries.clone();
            let task = tokio::spawn(async move {
                let _registration = registration;
                loop {
                    let journal = Journal {
                        backend: backend.clone(),
                        user: pending.authority.user,
                        project: pending.authority.project,
                        invocation: pending.invocation.clone(),
                        worker: pending.worker.clone(),
                        recoveries: recoveries.clone(),
                    };
                    let result = journal
                        .reconcile_original(
                            coordinator.clone(),
                            namespace.clone(),
                            &pending,
                            outcome.clone(),
                            pending.authority.root.mode,
                            recovery.clone(),
                        )
                        .await;
                    if !retry || result.is_ok() {
                        return result;
                    }
                    tokio::select! {
                        biased;
                        ()=coordinator.stopped()=>return Err("Original command recovery is stopping".into()),
                        ()=tokio::time::sleep(Duration::from_secs(2))=>{}
                    }
                }
            });
            Ok(Some(task))
        })();
        // A known native outcome stays readable during callback/root failure.
        // The registered task and original claim outlive this HTTP observer.
        if known.is_some() {
            return Ok(known);
        }
        let Some(task) = start? else {
            return Ok(Some(CommandProgress::Running));
        };
        Ok(task
            .await
            .map_err(|_| "Original command recovery worker stopped")??
            .map(|outcome| CommandProgress::Finished { outcome }))
    }
    pub(crate) fn recover_recorded_commands(&self) {
        let coordinator = self.inner.coordinator.clone();
        let Ok(registration) = coordinator.worker() else {
            return;
        };
        let host = self.clone();
        tokio::spawn(async move {
            let _registration = registration;
            loop {
                if let Ok(namespace) = coordinator.recovery_namespace()
                    && let Ok(commands) = namespace.pending_commands()
                {
                    let namespace = Arc::new(namespace);
                    host.recover_command_settlements(namespace.clone());
                    for pending in commands {
                        if pending.outcome.is_some() || pending.process_cleanup_confirmed {
                            let authority = pending.authority.clone();
                            let invocation = pending.invocation.clone();
                            if let Ok(Some(progress)) = host
                                .recover_pending(namespace.clone(), pending, false, None)
                                .await
                            {
                                let _ = host
                                    .retain_pending_progress(&authority, &invocation, &progress)
                                    .await;
                            }
                        } else if pending.cancelled || pending.can_cancel {
                            let _ = host.observe_unstarted(namespace.clone(), pending);
                        }
                    }
                }
                tokio::select! {
                    biased;
                    ()=coordinator.stopped()=>return,
                    ()=tokio::time::sleep(Duration::from_secs(2))=>{}
                }
            }
        });
    }
}
