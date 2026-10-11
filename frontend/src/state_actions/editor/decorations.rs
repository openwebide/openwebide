//! Source coordinates and ownership for active-line and matching-bracket paint.
use super::{EditorActions, EditorPresentationScope};
use leptos::prelude::*;
use openwebide_core::editor::Selection;

/// Deferred paint retains coordinates and ownership, never a complete source copy.
#[derive(Clone, Debug)]
pub struct EditorDecorations {
    scope: EditorPresentationScope,
    view: u64,
    preparation: u64,
    selection: Selection,
    pub active_line: usize,
    pub brackets: Option<[(usize, u32); 2]>,
}

impl EditorActions {
    pub fn decorations(self, include_brackets: bool) -> Option<EditorDecorations> {
        let scope = untrack(|| self.presentation_scope())?;
        let view = self.workspace.editor_view_revision.get_untracked();
        let preparation = self.workspace.editor_preparation_revision.get_untracked();
        let selection = self.current_selection().unwrap_or_default();
        let brackets = include_brackets
            .then(|| self.matching_bracket(selection.head))
            .flatten();
        let (active_line, brackets) = self.workspace.content.with_untracked(|source| {
            self.workspace.editor_documents.with_untracked(|documents| {
                let document = documents
                    .get(&(scope.project, scope.path.clone()))
                    .filter(|document| document.matches_text(source));
                let position = |offset| {
                    document
                        .and_then(|document| document.native_line_column(offset).ok())
                        .or_else(|| {
                            let prefix = source.get(..offset)?;
                            let start = prefix.rfind('\n').map_or(0, |at| at + 1);
                            Some((
                                prefix.bytes().filter(|byte| *byte == b'\n').count() + 1,
                                prefix[start..].encode_utf16().count(),
                            ))
                        })
                        .map(|(line, column)| (line, u32::try_from(column).unwrap_or(u32::MAX)))
                };
                let active_line = position(selection.head)?.0;
                let brackets = brackets
                    .and_then(|(first, second)| Some([position(first)?, position(second)?]));
                Some((active_line, brackets))
            })
        })?;
        let decorations = EditorDecorations {
            scope,
            view,
            preparation,
            selection,
            active_line,
            brackets,
        };
        self.decorations_current(&decorations)
            .then_some(decorations)
    }

    pub fn decorations_current(self, decorations: &EditorDecorations) -> bool {
        untrack(|| self.presentation_scope()).as_ref() == Some(&decorations.scope)
            && self.workspace.editor_view_revision.get_untracked() == decorations.view
            && self.workspace.editor_preparation_revision.get_untracked() == decorations.preparation
            && self.current_selection().unwrap_or_default() == decorations.selection
    }
}
