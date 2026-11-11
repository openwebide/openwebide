//! Database transport only. The shared core transaction engine owns execution
//! order, validation, rollback and interrupted-operation recovery in both modes.
use crate::backend::Backend;
use openwebide_core::{
    editor::EditorRecoveryRecord,
    workspace_transaction::{
        AdvanceWorkspaceTransaction, EditorRecoveryPatch, JournalRecord, LanguageReview,
        PreparedJournal, TransactionProgress, WorkspaceJournal, WorkspacePlan, valid_id,
    },
};
use std::{cell::RefCell, rc::Rc};

pub struct BackendWorkspaceJournal {
    backend: Rc<dyn Backend>,
    project: i64,
    id: String,
    generation: String,
    root: String,
    editor: EditorRecoveryPatch,
    language_review: Option<Box<LanguageReview>>,
    committed_editor: RefCell<Option<EditorRecoveryRecord>>,
}
impl BackendWorkspaceJournal {
    pub fn new(
        backend: Rc<dyn Backend>,
        project: i64,
        id: String,
        generation: String,
        root: String,
        editor: EditorRecoveryPatch,
        language_review: Option<Box<LanguageReview>>,
    ) -> Result<Self, String> {
        if project <= 0 || !valid_id(&id) || !valid_id(&generation) || root.is_empty() {
            return Err("Invalid workspace recovery scope".into());
        }
        editor.validate()?;
        if let Some(review) = &language_review {
            review.validate()?;
        }
        Ok(Self {
            backend,
            project,
            id,
            generation,
            root,
            editor,
            language_review,
            committed_editor: RefCell::new(None),
        })
    }
    pub fn take_committed_editor(&self) -> Option<EditorRecoveryRecord> {
        self.committed_editor.borrow_mut().take()
    }
    pub async fn delegate<T: openwebide_core::workspace_transaction::companion::FolderTransport>(
        &self,
        files: &mut openwebide_core::workspace_transaction::companion::CompanionFiles<T>,
        revision: u64,
    ) -> Result<(), String> {
        let (url, delegation) = self.delegation(files, revision).await?;
        files.admit(url, delegation).await
    }
    pub async fn delegation<
        T: openwebide_core::workspace_transaction::companion::FolderTransport,
    >(
        &self,
        files: &openwebide_core::workspace_transaction::companion::CompanionFiles<T>,
        revision: u64,
    ) -> Result<
        (
            String,
            openwebide_core::workspace_transaction::WorkspaceJournalDelegation,
        ),
        String,
    > {
        let (url, delegation) = self
            .backend
            .delegate_workspace_transaction(
                self.project,
                &self.id,
                &openwebide_core::workspace_transaction::DelegateWorkspaceTransaction {
                    generation: self.generation.clone(),
                    revision,
                    worker: files.worker().clone(),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok((url, delegation))
    }
}
impl WorkspaceJournal for BackendWorkspaceJournal {
    async fn prepare(&self, plan: &WorkspacePlan) -> Result<PreparedJournal, String> {
        if plan.root != self.root {
            return Err("The reviewed workspace folder changed".into());
        }
        self.load(Some(&self.id))
            .await?
            .ok_or("Workspace recovery is unavailable")?
            .prepared_for(plan)
    }
    async fn advance(
        &self,
        id: &str,
        generation: &str,
        revision: u64,
        progress: &TransactionProgress,
    ) -> Result<u64, String> {
        if id != self.id || generation != self.generation {
            return Err("Workspace recovery identity changed".into());
        }
        let response = self
            .backend
            .advance_workspace_transaction(
                self.project,
                &AdvanceWorkspaceTransaction {
                    id: id.into(),
                    generation: generation.into(),
                    revision,
                    progress: progress.clone(),
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        response.validate(generation, revision)?;
        if let Some(editor) = response.editor {
            editor.state.validate()?;
            if editor.revision
                != self
                    .editor
                    .revision
                    .checked_add(1)
                    .ok_or("Editor recovery revision overflow")?
                || editor.state.root.as_ref() != Some(&self.editor.root)
            {
                return Err("Workspace recovery returned a different editor snapshot".into());
            }
            *self.committed_editor.borrow_mut() = Some(editor);
        }
        Ok(response.revision)
    }
    async fn load(&self, id: Option<&str>) -> Result<Option<JournalRecord>, String> {
        if id.is_some_and(|id| id != self.id) {
            return Err("Workspace recovery identity changed".into());
        }
        let state = self
            .backend
            .workspace_transaction(self.project, Some(&self.id))
            .await
            .map_err(|e| e.to_string())?;
        let Some(state) = state else {
            return Ok(None);
        };
        if state.journal.id != self.id
            || state.journal.generation != self.generation
            || state.journal.plan.root != self.root
            || state.editor != self.editor
            || state.language_review != self.language_review
        {
            return Err("Workspace recovery belongs to another reviewed operation".into());
        }
        state.journal.validate()?;
        if let Some(review) = &state.language_review {
            review.validate_transaction(&state.journal.plan, &state.editor)?;
        }
        Ok(Some(state.journal))
    }
}
