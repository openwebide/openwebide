//! One ownership and preparation policy for explicit parser-aware actions.
use super::EditorActions;
use crate::state::workspace::EditorSyntaxScope;
use leptos::prelude::*;
use openwebide_core::editor::{Document, EditError, Indentation, Selection, Structure};
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
        let _ = indentation;
        let deadline = js_sys::Date::now() + 15_000.0;
        loop {
            if !current() {
                return Ok(None);
            }
            if let Some(syntax) = actions.syntax_structure(|| true)
                && (syntax.available() || !actions.has_structural_provider())
            {
                return Ok(Some(syntax));
            }
            // A source transaction can invalidate the prepared result before
            // the activation effect has changed the previous Ready status.
            let preparing = actions.worker_syntax_pending()
                || actions.languages.is_some_and(|state| {
                    matches!(
                        state.status.get_untracked(),
                        crate::state::languages::LanguageStatus::Preparing
                    )
                });
            if !preparing || js_sys::Date::now() >= deadline {
                return Err(EditError::StructureUnavailable);
            }
            crate::util::sleep_ms(16).await;
        }
    }
}
