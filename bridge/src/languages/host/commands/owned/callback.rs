//! One bounded reconnection policy for configured and browser candidate URLs.
//! Candidates never receive a plaintext authority or private worker preimage.
use super::*;
use crate::workspace_transactions::original_command::OriginalCommandJournal;
use openwebide_core::{
    WorkspaceMode, plugins::languages::intelligence::actions::CommandCallbackRequest,
};
impl Host {
    pub(crate) fn reconnect_command_callbacks(&self, candidate: String) {
        let coordinator = self.inner.coordinator.clone();
        let Ok(registration) = coordinator.worker() else {
            return;
        };
        let host = self.clone();
        tokio::spawn(async move {
            let _registration = registration;
            loop {
                let _ = host.reconnect_callbacks(None, &candidate).await;
                tokio::select! {biased; ()=coordinator.stopped()=>return, ()=tokio::time::sleep(Duration::from_secs(30))=>{}}
            }
        });
    }
    pub(crate) async fn reconnect_command_scope(
        &self,
        user: i64,
        project: i64,
        mode: WorkspaceMode,
        candidate: &str,
    ) -> Result<bool, String> {
        if user <= 0 || project <= 0 {
            return Err("Invalid command recovery account or project".into());
        }
        self.reconnect_callbacks(Some((user, project, mode)), candidate)
            .await
    }
    async fn reconnect_callbacks(
        &self,
        scope: Option<(i64, i64, WorkspaceMode)>,
        candidate: &str,
    ) -> Result<bool, String> {
        let candidate = CommandCallbackRequest::address(candidate)?;
        let coordinator = self.inner.coordinator.clone();
        let _registration = coordinator.worker()?;
        // Acquire before retaining private state. At most two batches retain
        // native records or contact candidates; queued HTTP bodies stay small.
        let _permit = coordinator
            .prepare(self.inner.command_callbacks.0.acquire())
            .await?
            .map_err(|_| "Command callback reconnection is unavailable")?;
        let namespace = Arc::new(coordinator.recovery_namespace()?);
        let selected = |authority: &RecoveryAuthority| {
            scope.is_none_or(|(user, project, mode)| {
                authority.user == user
                    && authority.project == project
                    && authority.root.mode == mode
            })
        };
        let mut changed = false;
        for pending in namespace.pending_commands()? {
            if !selected(&pending.authority) || pending.origin == candidate {
                continue;
            }
            let result = async {
                let backend = OriginalCommandJournal::new(
                    candidate.clone(),
                    pending.authority.clone(),
                    pending.worker.clone(),
                )?;
                let current = backend.scope().await?;
                namespace.reconnect_command_callback(&pending, &current, &candidate)
            };
            if coordinator
                .prepare(tokio::time::timeout(Duration::from_secs(5), result))
                .await
                .is_ok_and(|value| value.is_ok_and(|value| value.is_ok()))
            {
                changed = true;
            }
        }
        for pending in namespace.pending_command_settlements()? {
            if !selected(&pending.authority) || pending.origin == candidate {
                continue;
            }
            let result = async {
                let backend = OriginalCommandJournal::new(
                    candidate.clone(),
                    pending.authority.clone(),
                    pending.worker.clone(),
                )?;
                backend.acknowledge_settlement(&pending.recovery).await?;
                namespace.acknowledged_command_settlement(&pending)
            };
            if coordinator
                .prepare(tokio::time::timeout(Duration::from_secs(5), result))
                .await
                .is_ok_and(|value| value.is_ok_and(|value| value.is_ok()))
            {
                changed = true;
            }
        }
        Ok(changed)
    }
}
