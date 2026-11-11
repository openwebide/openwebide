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
        record.invocation()?;
        if operation == CommandOperation::Start
            && record.phase
                != openwebide_core::plugins::languages::intelligence::actions::ActionPhase::Running
        {
            return Err("A command with an uncertain outcome cannot be started again".into());
        }
        let call = if operation == CommandOperation::Start {
            Some(Box::new(self.call(&record.approval).await?))
        } else {
            None
        };
        let progress = self
            .host
            .language_command(&CommandDispatch {
                id: record.approval.id.clone(),
                operation,
                call,
            })
            .await?;
        if let CommandProgress::Finished { outcome } = &progress {
            outcome.validate()?;
        }
        self.current()?;
        Ok(progress)
    }
}
