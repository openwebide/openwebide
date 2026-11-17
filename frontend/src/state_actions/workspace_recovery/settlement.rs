//! Observe an already terminal command without replaying it or acknowledging effects.
use super::*;
use futures::future::{Either, abortable, select};
use openwebide_core::plugins::languages::intelligence::actions::ActionRecoveryRequest;

impl Context {
    pub(super) fn observe_published_command_settlements(self) {
        Effect::new(move |_| {
            let account = self.auth.generation.get();
            if self.auth.user.get().is_none() {
                return;
            }
            let Some(project) = self
                .projects
                .active_project
                .get()
                .and_then(|id| self.projects.project(id))
            else {
                return;
            };
            let action =
                self.workspace
                    .workspace_recovery
                    .with(|states| match states.get(&project.id) {
                        Some(WorkspaceRecoveryStatus::LanguageAction {
                            id,
                            native_settlement_pending: true,
                            phase,
                            recovering: false,
                            ..
                        }) if phase.terminal() => Some(id.clone()),
                        _ => None,
                    });
            let Some(action) = action else {
                return;
            };
            if self
                .settlement_checks
                .with_value(|checks| checks.contains_key(&project.id))
            {
                return;
            }
            // Command completion can publish this status without changing a
            // file publication's phase or explicitly checking recovery again.
            let root = EditorRoot::for_project(&project, self.projects, Some(self.settings));
            let ticket = self.reserve(project.id);
            self.observe_command_settlement(project, root, account, ticket, action);
        });
    }

    pub(super) fn observe_command_settlement(
        self,
        project: Project,
        root: EditorRoot,
        account: u64,
        ticket: u64,
        action: String,
    ) {
        let id = project.id;
        let backend = self.api.with_value(Clone::clone);
        let observer = async move {
            let current = || {
                self.current(&project, &root, account, ticket)
                    && self.projects.active_project.try_get_untracked() == Some(Some(id))
                    && self.workspace.workspace_recovery.try_with_untracked(|states| {
                        matches!(states.get(&id), Some(WorkspaceRecoveryStatus::LanguageAction {
                            id: current, native_settlement_pending: true, phase, recovering: false, ..
                        }) if current == &action && phase.terminal())
                    }) == Some(true)
            };
            loop {
                crate::util::sleep_ms(500).await;
                if !current() {
                    return;
                }
                // Poll only compact command recovery. Repeated full file journals
                // can retain or transfer tens of megabytes while cleanup retries.
                let result = backend
                    .language_action_recovery(
                        id,
                        &ActionRecoveryRequest::Load {
                            id: Some(action.clone()),
                        },
                    )
                    .await;
                if !current() {
                    return;
                }
                let refresh = match result {
                    Ok(Some(recovery)) => {
                        recovery.command.invocation.id == action
                            && recovery.command.phase.terminal()
                            && !recovery.command.native_settlement_pending
                    }
                    Ok(None) => true,
                    Err(_) => false,
                };
                if refresh && !self.workspace.editor_operation_active_untracked(id) {
                    // Re-read the complete current scope before unlocking: another
                    // window may already have begun a new file or command action.
                    self.check(id);
                    return;
                }
            }
        };
        let (observer, cancel) = abortable(observer);
        self.settlement_checks.update_value(|checks| {
            if let Some(previous) = checks.insert(id, cancel) {
                previous.abort();
            }
        });
        spawn_local(async move {
            let deadline = crate::util::sleep_ms(30_000);
            futures::pin_mut!(observer, deadline);
            match select(observer, deadline).await {
                Either::Left(_) | Either::Right(_) => {}
            }
            if self
                .tickets
                .try_with_value(|tickets| tickets.get(&id) == Some(&ticket))
                == Some(true)
            {
                self.settlement_checks.try_update_value(|checks| {
                    checks.remove(&id);
                });
            }
        });
    }
}
