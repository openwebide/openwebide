//! Reconnect recovery never starts a server command or rolls back its effects.
use super::*;
use openwebide_core::plugins::languages::intelligence::actions::{
    ActionJournalRequest, ActionPhase, ActionRecoveryRequest, CommandOperation, CommandProgress,
};

#[derive(Clone, Copy)]
pub(super) enum CommandRecovery {
    Check,
    Stop,
    Acknowledge,
    Discard,
}

impl Context {
    pub(super) fn command(self, id: i64, operation: CommandRecovery) {
        let Some(WorkspaceRecoveryStatus::LanguageAction {
            id: action_id,
            recovering: false,
            ..
        }) = self
            .workspace
            .workspace_recovery
            .with_untracked(|states| states.get(&id).cloned())
        else {
            return;
        };
        let Some(project) = self.projects.project(id) else {
            return;
        };
        let account = self.auth.generation.get_untracked();
        let root = EditorRoot::for_project(&project, self.projects, Some(self.settings));
        let ticket = self.reserve(id);
        let lock = match self.workspace.begin_workspace_edit(id) {
            Ok(lock) => lock,
            Err(error) => {
                self.ui.notify(error);
                return;
            }
        };
        self.workspace.workspace_recovery.update(|states| {
            if let Some(WorkspaceRecoveryStatus::LanguageAction {
                recovering, error, ..
            }) = states.get_mut(&id)
            {
                *recovering = true;
                *error = None;
            }
        });
        spawn_local(async move {
            let current: Rc<dyn Fn() -> bool> =
                Rc::new(move || self.current(&project, &root, account, ticket));
            let result=async {
                lock.wait_for_saves(||current()).await?;
                let host=self.host.ok_or("The language execution host is unavailable")?;
                let guarded=current.clone();
                let client=crate::plugin_actions::project_language_recovery(self.api,host,id,move ||guarded())?;
                let record=client.recovery(&ActionRecoveryRequest::Load {id:Some(action_id.clone())}).await?.ok_or("The approved command is unavailable")?;
                match operation {
                    CommandRecovery::Discard=> {
                        let record=client.request(&ActionJournalRequest::Load {id:Some(action_id.clone())}).await?.ok_or("The approved command is unavailable")?;
                        if record.phase!=ActionPhase::Approved {return Err("A claimed command must be stopped and reviewed instead of discarded".into());}
                        let workspace=self.api.with_value(Clone::clone).workspace_transaction(id,Some(&action_id)).await.map_err(|e|e.to_string())?.ok_or("Workspace recovery is unavailable")?;
                        client.discard_unstarted(&record,workspace.journal.progress.phase).await?;
                    }
                    CommandRecovery::Check|CommandRecovery::Stop=> {
                        if record.command.phase==ActionPhase::Cancelled {
                            client.recover_command(&record,CommandOperation::Cancel).await?;
                        }
                        if matches!(record.command.phase,ActionPhase::Running|ActionPhase::Uncertain)
                            || (record.command.phase==ActionPhase::Rejected && record.command.native_settlement_pending && !record.command.command_cleanup_confirmed) {
                            let command=if matches!(operation,CommandRecovery::Stop) {CommandOperation::Stop} else {CommandOperation::Status};
                            let mut progress=client.recover_command(&record,command).await?;
                            if matches!(operation,CommandRecovery::Stop) {
                                let deadline=js_sys::Date::now()+5000.0;
                                while progress==CommandProgress::Running && js_sys::Date::now()<deadline {
                                    crate::util::sleep_ms(50).await;
                                    progress=client.recover_command(&record,CommandOperation::Status).await?;
                                }
                            }
                            if record.command.phase==ActionPhase::Running && let CommandProgress::Finished {outcome}=progress {
                                client.recovery(&ActionRecoveryRequest::Finish {id:action_id.clone(),revision:record.command.revision,outcome}).await?;
                            }
                        }
                    }
                    CommandRecovery::Acknowledge=> {
                        client.acknowledge_command(&record,"Reviewed the command outcome and any changes to the project").await?;
                    }
                }
                Ok::<_,String>(())
            }.await;
            drop(lock);
            if !current() {
                return;
            }
            if let Err(error) = result {
                self.workspace.workspace_recovery.try_update(|states| {
                    if let Some(WorkspaceRecoveryStatus::LanguageAction {
                        recovering,
                        error: issue,
                        ..
                    }) = states.get_mut(&id)
                    {
                        *recovering = false;
                        *issue = Some(error);
                    }
                });
            } else {
                self.check(id);
                self.recovery.check_files.run(id);
            }
        });
    }
}
