//! Reviewed-action policy shared by editor, reconnect and recovery callers.
//! File/editor transactions remain the shared workspace engine's responsibility.
use super::*;
use openwebide_core::plugins::languages::intelligence::{
    Call, Request,
    actions::{
        ActionApproval, ActionJournalRequest, ActionRecord, ActionReview, CommandDispatch,
        CommandOperation, CommandProgress,
    },
};

#[derive(Clone)]
pub struct LanguageActionClient<H> {
    host: H,
    registry: LanguageRegistry,
    bindings: Arc<[ProjectPlugin]>,
    preferences: LanguagePreferences,
}
impl<H: PluginActionHost> LanguageActionClient<H> {
    pub fn new(
        host: H,
        bindings: Vec<ProjectPlugin>,
        preferences: LanguagePreferences,
    ) -> Result<Self, String> {
        preferences.validate()?;
        Ok(Self {
            host,
            registry: LanguageRegistry::from_bindings(&bindings),
            bindings: bindings.into(),
            preferences,
        })
    }
    fn current(&self) -> Result<(), String> {
        if self.host.is_current() {
            Ok(())
        } else {
            Err("Language action context changed".into())
        }
    }
    pub async fn prepare(
        &self,
        id: String,
        namespace: &str,
        input: Request,
        review: ActionReview,
    ) -> Result<ActionApproval, String> {
        self.current()?;
        let provider = self
            .registry
            .find(namespace)
            .ok_or("The reviewed language provider is unavailable")?;
        if provider.definition.id != input.language {
            return Err("The code action language changed".into());
        }
        let selected = select_language_provider(&self.bindings, provider)?;
        let prepared = self.host.prepare(selected).await?;
        super::super::execution::validate_prepared_selection(selected, &prepared)?;
        let approval = ActionApproval {
            id,
            prepared,
            binding_revision: provider.installation_revision,
            input,
            review,
        };
        approval.validate()?;
        approval.validate_provider(&self.registry, &self.preferences)?;
        self.current()?;
        Ok(approval)
    }
    async fn call(&self, approval: &ActionApproval) -> Result<Call, String> {
        self.current()?;
        approval.validate()?;
        approval.validate_provider(&self.registry, &self.preferences)?;
        let prepared = self.host.prepare(&approval.prepared).await?;
        super::super::execution::validate_prepared_selection(&approval.prepared, &prepared)?;
        if prepared.host_id != approval.prepared.host_id {
            return Err("The reviewed language execution host changed".into());
        }
        let grant = self.host.grant(&prepared).await?;
        self.current()?;
        if grant.is_empty() {
            return Err("Manual language action authority is unavailable".into());
        }
        Ok(Call {
            grant,
            session_id: None,
            prepared,
            input: approval.input.clone(),
        })
    }
    pub async fn request(
        &self,
        request: &ActionJournalRequest,
    ) -> Result<Option<ActionRecord>, String> {
        self.current()?;
        let record = self.host.language_action_request(request).await?;
        if let Some(record) = &record {
            record.validate()?;
        }
        self.current()?;
        if let Some(record) = &record
            && record.phase == openwebide_core::plugins::languages::intelligence::actions::ActionPhase::Cancelled
        {
            // Durable cancellation is independent of folder/service availability.
            // The original native owner retries cleanup without fresh authority.
            let _ = self.dispatch_command(&record.approval.id, CommandOperation::Cancel, None).await;
        }
        if let Some(record) = &record
            && matches!(record.phase,
                openwebide_core::plugins::languages::intelligence::actions::ActionPhase::Cancelled
                | openwebide_core::plugins::languages::intelligence::actions::ActionPhase::Acknowledged)
        {
            // Durable cancellation takes precedence over this best-effort native
            // cache cleanup. Missing/disabled services require no replacement.
            let _ = self.release_synchronization(&record.approval).await;
        }
        self.current()?;
        Ok(record)
    }
    async fn release_synchronization(&self, approval: &ActionApproval) -> Result<(), String> {
        let action = openwebide_core::plugins::languages::intelligence::actions::ActionSynchronization::from_approval(approval)?;
        let mut call = self.call(approval).await?;
        // Control messages never advance or replace a source's revision. The
        // durable action ID makes this bounded watermark distinct from drafts.
        call.input.document = format!("release:{}", approval.id);
        call.input.request = 1;
        call.input.revision = 0;
        call.input.source.clear();
        call.input.source_digest = source_digest("");
        call.input.query =
            openwebide_core::plugins::languages::intelligence::Query::ReleaseAction { action };
        call.input.validate()?;
        let response = self.host.language_server(&call).await?;
        response.validate(&call.input)?;
        self.current()
    }
    pub async fn approve(&self, approval: &ActionApproval) -> Result<ActionRecord, String> {
        let call = self.call(approval).await?;
        self.request(&ActionJournalRequest::Approve {
            call: Box::new(call),
            approval: Box::new(approval.clone()),
        })
        .await?
        .filter(|record| record.approval.fingerprint().ok() == approval.fingerprint().ok())
        .ok_or_else(|| "The approved language action was not acknowledged".into())
    }
    pub async fn claim(&self, record: &ActionRecord) -> Result<ActionRecord, String> {
        record.validate()?;
        let call = self.call(&record.approval).await?;
        self.request(&ActionJournalRequest::Claim {
            id: record.approval.id.clone(),
            revision: record.revision,
            call: Box::new(call),
        })
        .await?
        .filter(|claimed| {
            claimed.approval.fingerprint().ok() == record.approval.fingerprint().ok()
                && claimed.phase==openwebide_core::plugins::languages::intelligence::actions::ActionPhase::Running
        })
        .ok_or_else(|| "The language command claim was not acknowledged; load its outcome".into())
    }
    /// Dispatch is deliberately one attempt. Reconnect callers inspect or stop
    /// the durable claim; they never automatically start an approved command.
    pub async fn command(
        &self,
        record: &ActionRecord,
        operation: CommandOperation,
    ) -> Result<CommandProgress, String> {
        self.current()?;
        operation.invocation(record)?;
        let call = if operation == CommandOperation::Start {
            Some(Box::new(self.call(&record.approval).await?))
        } else {
            None
        };
        self.dispatch_command(&record.approval.id, operation, call)
            .await
    }
    pub async fn recovery(
        &self,
        request: &openwebide_core::plugins::languages::intelligence::actions::ActionRecoveryRequest,
    ) -> Result<
        Option<openwebide_core::plugins::languages::intelligence::actions::ActionRecovery>,
        String,
    > {
        use openwebide_core::plugins::languages::intelligence::actions::{
            ActionPhase, ActionRecoveryRequest,
        };
        self.current()?;
        let result = self.host.language_action_recovery(request).await?;
        if result.is_none() && !matches!(request, ActionRecoveryRequest::Load { .. }) {
            return Err("Command recovery change was not acknowledged".into());
        }
        if let Some(value) = &result {
            value.validate()?;
            let expected = match request {
                ActionRecoveryRequest::Load { id } => id.as_deref(),
                ActionRecoveryRequest::Finish { id, outcome, .. } => {
                    if value.outcome.as_ref() != Some(outcome) {
                        return Err("Command recovery outcome changed".into());
                    }
                    Some(id.as_str())
                }
                ActionRecoveryRequest::Acknowledge { id, revision, note } => {
                    if value.command.phase != ActionPhase::Acknowledged
                        || Some(value.command.revision) != revision.checked_add(1)
                        || value.acknowledgement.as_ref() != Some(note)
                    {
                        return Err("Command recovery acknowledgement changed".into());
                    }
                    Some(id.as_str())
                }
            };
            if expected.is_some_and(|id| id != value.command.invocation.id) {
                return Err("Command recovery belongs to another action".into());
            }
        }
        self.current()?;
        Ok(result)
    }
    pub async fn recover_command(
        &self,
        record: &openwebide_core::plugins::languages::intelligence::actions::ActionRecovery,
        operation: CommandOperation,
    ) -> Result<CommandProgress, String> {
        use openwebide_core::plugins::languages::intelligence::actions::ActionPhase;
        self.current()?;
        record.validate()?;
        if operation == CommandOperation::Start
            || (record.command.phase == ActionPhase::Approved
                && !record.command.native_settlement_pending)
            || ((record.command.phase == ActionPhase::Cancelled)
                != (operation == CommandOperation::Cancel))
        {
            return Err("Recovery cannot start an approved command".into());
        }
        self.dispatch_command(&record.command.invocation.id, operation, None)
            .await
    }
    pub async fn acknowledge_command(
        &self,
        record: &openwebide_core::plugins::languages::intelligence::actions::ActionRecovery,
        note: &str,
    ) -> Result<openwebide_core::plugins::languages::intelligence::actions::ActionRecovery, String>
    {
        use openwebide_core::plugins::languages::intelligence::actions::{
            ActionPhase, ActionRecoveryRequest,
        };
        self.current()?;
        record.validate()?;
        if !matches!(
            record.command.phase,
            ActionPhase::Rejected | ActionPhase::Uncertain
        ) {
            return Err("Review the original command outcome before acknowledging it".into());
        }
        let mut current = record.clone();
        if current.command.native_settlement_pending && !current.command.command_cleanup_confirmed {
            if !matches!(
                self.recover_command(&current, CommandOperation::Stop)
                    .await?,
                CommandProgress::Finished { .. }
            ) {
                return Err(
                    "Wait for the original command to stop before acknowledging its effects".into(),
                );
            }
            current = self
                .recovery(&ActionRecoveryRequest::Load {
                    id: Some(record.command.invocation.id.clone()),
                })
                .await?
                .ok_or("Original command recovery is unavailable")?;
            if current.command.invocation != record.command.invocation
                || current.command.outcome_digest != record.command.outcome_digest
                || current.command.revision != record.command.revision
                || !current.command.command_cleanup_confirmed
            {
                return Err(
                    "Wait for original native cleanup before acknowledging this command".into(),
                );
            }
        }
        self.recovery(&ActionRecoveryRequest::Acknowledge {
            id: current.command.invocation.id,
            revision: current.command.revision,
            note: note.into(),
        })
        .await?
        .ok_or_else(|| "Command acknowledgement was not retained".into())
    }
    /// Discard only the unstarted command. Its files have already reached the
    /// supplied terminal decision; neither branch grants dispatch permission.
    pub async fn discard_unstarted(
        &self,
        record: &ActionRecord,
        phase: openwebide_core::workspace_transaction::TransactionPhase,
    ) -> Result<(), String> {
        use openwebide_core::{
            plugins::languages::intelligence::actions::{
                ActionPhase, ActionRecoveryRequest, CommandOutcome,
            },
            workspace_transaction::TransactionPhase,
        };
        self.current()?;
        record.validate()?;
        if record.phase != ActionPhase::Approved {
            return Err(
                "A claimed command must be stopped and reviewed instead of discarded".into(),
            );
        }
        let id = &record.approval.id;
        match phase {
            TransactionPhase::Restored => {
                self.request(&ActionJournalRequest::Cancel {
                    id: id.clone(),
                    revision: record.revision,
                })
                .await?
                .ok_or("Command cancellation was not acknowledged")?;
            }
            TransactionPhase::Committed => {
                let current = self
                    .recovery(&ActionRecoveryRequest::Load {
                        id: Some(id.clone()),
                    })
                    .await?
                    .ok_or("Original command recovery is unavailable")?;
                if current.command.invocation != record.approval.command_invocation()?
                    || current.command.revision != record.revision
                    || current.command.phase != ActionPhase::Approved
                {
                    return Err("The unstarted command changed before discard".into());
                }
                let revision = if current.command.native_settlement_pending {
                    let progress = self
                        .recover_command(&current, CommandOperation::Stop)
                        .await?;
                    let CommandProgress::Finished {
                        outcome: outcome @ CommandOutcome::Rejected { .. },
                    } = progress
                    else {
                        return Err(
                            "Wait for original native cleanup before acknowledging this command"
                                .into(),
                        );
                    };
                    let confirmed = self
                        .recovery(&ActionRecoveryRequest::Load {
                            id: Some(id.clone()),
                        })
                        .await?
                        .ok_or("Original command recovery is unavailable")?;
                    if confirmed.command.invocation != current.command.invocation
                        || confirmed.command.phase != ActionPhase::Rejected
                        || !confirmed.command.command_cleanup_confirmed
                        || confirmed.outcome.as_ref() != Some(&outcome)
                    {
                        return Err(
                            "Wait for original native cleanup before acknowledging this command"
                                .into(),
                        );
                    }
                    confirmed.command.revision
                } else {
                    self.request(&ActionJournalRequest::RejectUnstarted {
                        id: id.clone(),
                        revision: record.revision,
                        message: "The approved edit was kept; its unstarted command was discarded"
                            .into(),
                    })
                    .await?
                    .ok_or("Command discard was not acknowledged")?
                    .revision
                };
                self.recovery(&ActionRecoveryRequest::Acknowledge {
                    id: id.clone(),
                    revision,
                    note: "Keep the committed edit without running its attached command".into(),
                })
                .await?;
            }
            _ => return Err("Restore the interrupted file operation first".into()),
        }
        self.current()
    }
    async fn dispatch_command(
        &self,
        id: &str,
        operation: CommandOperation,
        call: Option<Box<Call>>,
    ) -> Result<CommandProgress, String> {
        let progress = self
            .host
            .language_command(&CommandDispatch {
                id: id.into(),
                operation,
                call,
            })
            .await?;
        if operation == CommandOperation::Cancel && progress != CommandProgress::Cancelled {
            return Err("Native cancellation acknowledgement changed".into());
        }
        if let CommandProgress::Finished { outcome } = &progress {
            outcome.validate()?;
        }
        self.current()?;
        Ok(progress)
    }
}
