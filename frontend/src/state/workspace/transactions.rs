//! Ephemeral editor I/O ownership; recoverable user state remains in the database.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceRecoveryStatus {
    Checking,
    Ready,
    Unavailable(String),
    EditorConflict(String),
    Pending {
        id: String,
        phase: openwebide_core::workspace_transaction::TransactionPhase,
        recovering: bool,
        error: Option<String>,
    },
    LanguageAction {
        id: String,
        title: String,
        phase: openwebide_core::plugins::languages::intelligence::actions::ActionPhase,
        recovering: bool,
        error: Option<String>,
    },
}
impl WorkspaceRecoveryStatus {
    pub fn from_records(
        workspace: Option<&openwebide_core::workspace_transaction::WorkspaceTransactionState>,
        action: Option<&openwebide_core::plugins::languages::intelligence::actions::ActionRecord>,
    ) -> Self {
        use openwebide_core::plugins::languages::intelligence::actions::CommandOutcome;
        let status = Self::from_state(workspace);
        if status.blocks_saves() {
            return status;
        }
        match action.filter(|action| !action.phase.terminal()) {
            Some(action) => Self::LanguageAction {
                id: action.approval.id.clone(),
                title: action.approval.review.title.clone(),
                phase: action.phase,
                recovering: false,
                error: match &action.outcome {
                    Some(
                        CommandOutcome::Rejected { message }
                        | CommandOutcome::Uncertain { message },
                    ) => Some(message.clone()),
                    _ => None,
                },
            },
            None => status,
        }
    }
    pub fn from_state(
        state: Option<&openwebide_core::workspace_transaction::WorkspaceTransactionState>,
    ) -> Self {
        use openwebide_core::workspace_transaction::TransactionPhase;
        state.map_or(Self::Ready, |state| {
            if matches!(
                state.journal.progress.phase,
                TransactionPhase::Committed | TransactionPhase::Restored
            ) {
                Self::Ready
            } else {
                Self::Pending {
                    id: state.journal.id.clone(),
                    phase: state.journal.progress.phase,
                    recovering: false,
                    error: None,
                }
            }
        })
    }
    pub fn blocks_saves(&self) -> bool {
        !matches!(self, Self::Ready)
    }
    pub fn message(&self) -> String {
        match self {
            Self::Checking => "Checking for interrupted workspace changes…".into(),
            Self::Ready => String::new(),
            Self::Unavailable(error) => format!("File saves are paused until workspace recovery can be checked: {error}"),
            Self::EditorConflict(error) => format!("File saves are paused until saved editor files are reviewed: {error}"),
            Self::Pending { phase:openwebide_core::workspace_transaction::TransactionPhase::Finalizing | openwebide_core::workspace_transaction::TransactionPhase::Committed,recovering:true,.. }=>"Finishing workspace recovery…".into(),
            Self::Pending { recovering: true, .. } => "Restoring interrupted workspace changes…".into(),
            Self::Pending { error: Some(error), .. } => format!("File saves are paused. Workspace recovery needs attention: {error}"),
            Self::Pending {phase:openwebide_core::workspace_transaction::TransactionPhase::Finalizing | openwebide_core::workspace_transaction::TransactionPhase::Committed,..}=>"The operation’s result is saved. Finish recovery before saving files. Your drafts remain editable.".into(),
            Self::Pending { .. } => "An interrupted workspace operation needs recovery before files can be saved. Your drafts remain editable.".into(),
            Self::LanguageAction {title,recovering:true,..}=>format!("Checking the approved command: {title}…"),
            Self::LanguageAction {title,error:Some(error),..}=>format!("File saves are paused while the command outcome is reviewed: {title}. {error}"),
            Self::LanguageAction {title,..}=>format!("The approved command needs review before files can be saved: {title}. Your drafts remain editable."),
        }
    }
    pub fn recovery_label(&self) -> &'static str {
        if matches!(
            self,
            Self::Pending {
                phase: openwebide_core::workspace_transaction::TransactionPhase::Finalizing
                    | openwebide_core::workspace_transaction::TransactionPhase::Committed,
                ..
            }
        ) {
            "Finish workspace recovery"
        } else {
            "Restore interrupted changes"
        }
    }
}

pub struct WorkspaceEditGuard {
    workspace: WorkspaceState,
    project: i64,
    token: Arc<()>,
}
pub struct WorkspaceSaveGuard {
    workspace: WorkspaceState,
    project: i64,
    token: Arc<()>,
}
impl WorkspaceState {
    pub fn workspace_saves_blocked(&self, project: i64) -> bool {
        self.workspace_recovery
            .try_with_untracked(|states| {
                states
                    .get(&project)
                    .is_some_and(WorkspaceRecoveryStatus::blocks_saves)
            })
            .unwrap_or(true)
    }
    pub fn editor_operation_active(&self, project: i64) -> bool {
        self.editor_transactions
            .with(|operations| operations.contains_key(&project))
    }
    pub fn editor_operation_active_untracked(&self, project: i64) -> bool {
        self.editor_transactions
            .try_with_untracked(|operations| operations.contains_key(&project))
            .unwrap_or(true)
    }
    pub fn begin_workspace_edit(&self, project: i64) -> Result<WorkspaceEditGuard, String> {
        let token = Arc::new(());
        let mut owns = false;
        self.editor_transactions.try_update(|operations| {
            if let std::collections::hash_map::Entry::Vacant(entry) = operations.entry(project) {
                entry.insert(token.clone());
                owns = true;
            }
        });
        if !owns {
            return Err("Another workspace operation is still running".into());
        }
        Ok(WorkspaceEditGuard {
            workspace: *self,
            project,
            token,
        })
    }
    pub fn begin_editor_save(&self, project: i64) -> Option<WorkspaceSaveGuard> {
        if self.editor_operation_active_untracked(project) || self.workspace_saves_blocked(project)
        {
            return None;
        }
        let token = Arc::new(());
        self.editor_saves.try_update(|saves| {
            saves.entry(project).or_default().push(token.clone());
        })?;
        Some(WorkspaceSaveGuard {
            workspace: *self,
            project,
            token,
        })
    }
}
impl WorkspaceEditGuard {
    pub fn is_current(&self) -> bool {
        self.workspace
            .editor_transactions
            .try_with_untracked(|operations| {
                operations
                    .get(&self.project)
                    .is_some_and(|owner| Arc::ptr_eq(owner, &self.token))
            })
            == Some(true)
    }
    #[cfg(target_arch = "wasm32")]
    pub async fn wait_for_saves(&self, current: impl Fn() -> bool) -> Result<(), String> {
        let deadline = js_sys::Date::now() + 30_000.0;
        loop {
            if !self.is_current() || !current() {
                return Err("The workspace changed while waiting for its saves".into());
            }
            if self
                .workspace
                .editor_saves
                .try_with_untracked(|saves| !saves.contains_key(&self.project))
                == Some(true)
            {
                return Ok(());
            }
            if js_sys::Date::now() > deadline {
                return Err("A file save is still running; try the workspace operation again when it finishes".into());
            }
            crate::util::sleep_ms(10).await;
        }
    }
}
impl Drop for WorkspaceEditGuard {
    fn drop(&mut self) {
        self.workspace.editor_transactions.try_update(|operations| {
            if operations
                .get(&self.project)
                .is_some_and(|owner| Arc::ptr_eq(owner, &self.token))
            {
                operations.remove(&self.project);
            }
        });
    }
}
impl Drop for WorkspaceSaveGuard {
    fn drop(&mut self) {
        self.workspace.editor_saves.try_update(|saves| {
            if let Some(active) = saves.get_mut(&self.project) {
                active.retain(|owner| !Arc::ptr_eq(owner, &self.token));
                if active.is_empty() {
                    saves.remove(&self.project);
                }
            }
        });
    }
}
