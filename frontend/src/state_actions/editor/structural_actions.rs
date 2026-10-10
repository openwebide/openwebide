//! One ownership and preparation policy for explicit parser-aware actions.
use super::EditorActions;
use crate::state::workspace::EditorSyntaxScope;
use leptos::prelude::*;
use openwebide_core::editor::{
    Document, EditError, Indentation, Selection, Structure, SyntaxAdmission, SyntaxAdmissionStatus,
    SyntaxReply, SyntaxRequest,
};
use std::sync::Arc;

pub(super) struct StructuralAction {
    pub(super) scope: EditorSyntaxScope,
    pub(super) source: Arc<String>,
    pub(super) rules: Indentation,
    selections: Vec<Selection>,
    revision: Option<u64>,
    command_revision: u64,
}

impl EditorActions {
    pub(super) fn structural_action(
        self,
        selection: Selection,
    ) -> Result<Option<StructuralAction>, EditError> {
        if self.key().is_none() {
            return Ok(None);
        }
        if self.is_composing() {
            return Err(EditError::CompositionActive);
        }
        self.record_native_selection(selection)?;
        self.workspace.editor_command_revision.update(|revision| {
            *revision = revision.wrapping_add(1);
        });
        let Some(scope) = self.syntax_scope() else {
            return Ok(None);
        };
        let revision = self
            .workspace
            .editor_documents
            .with_untracked(|documents| documents.get(&scope.key).map(Document::revision));
        Ok(Some(StructuralAction {
            scope,
            source: self.source().shared(),
            selections: self.current_selections(),
            rules: self.rules_untracked().indentation,
            revision,
            command_revision: self.workspace.editor_command_revision.get_untracked(),
        }))
    }
}

impl StructuralAction {
    pub(super) fn current(&self, actions: EditorActions) -> bool {
        actions.syntax_scope_current(&self.scope)
            && actions.workspace.editor_command_revision.get_untracked() == self.command_revision
            && !actions.is_composing()
            && actions
                .workspace
                .content
                .with_untracked(|source| Arc::ptr_eq(&source.shared(), &self.source))
            && actions
                .workspace
                .editor_documents
                .with_untracked(|documents| {
                    documents.get(&self.scope.key).is_some_and(|document| {
                        Some(document.revision()) == self.revision
                            && document.selections() == self.selections
                    })
                })
            && actions.rules_untracked().indentation == self.rules
    }

    pub(super) async fn prepare(
        &self,
        actions: EditorActions,
        indentation: Indentation,
    ) -> Result<Option<Arc<Structure>>, EditError> {
        let current = || self.current(actions);
        if !current() {
            return Ok(None);
        }
        let mut syntax = actions.syntax_structure(|| true);
        if syntax.is_none() {
            let mut admission = SyntaxAdmission::new(self.scope.source.clone());
            while admission.status() == SyntaxAdmissionStatus::Pending {
                if !current() {
                    return Ok(None);
                }
                admission.advance(openwebide_core::highlight::LEXICAL_BATCH_BYTES);
                if admission.status() == SyntaxAdmissionStatus::Pending {
                    crate::util::yield_task().await;
                }
            }
            if admission.status() == SyntaxAdmissionStatus::TooLarge {
                return Err(EditError::StructureUnavailable);
            }
            let request = SyntaxRequest::new(
                1,
                "editor-command".into(),
                openwebide_core::highlight::language_from_path(&self.scope.key.1),
                &self.scope.source,
                indentation.tab_width(),
                None,
            );
            let message = serde_json::to_string(&request).expect("syntax request is serializable");
            let reply = crate::editor_worker::CooperativeClient::default()
                .request_while(message, current)
                .await
                .map_err(|_| EditError::StructureUnavailable)?;
            if !current() {
                return Ok(None);
            }
            syntax = SyntaxReply::receive_shared(&reply, 1, self.scope.source.clone(), None)
                .and_then(|(_, analysis)| analysis)
                .and_then(|analysis| analysis.structure().cloned());
        }
        if !current() {
            return Ok(None);
        }
        syntax.map(Some).ok_or(EditError::StructureUnavailable)
    }
}
