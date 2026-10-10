//! Scoped native text contexts; document transactions and policy stay shared.
use super::{EditorActions, NATIVE_CONTEXT_BYTES};
use leptos::prelude::*;
use openwebide_core::editor::{EditError, FoldProjection, ProjectionError, Selection};

pub use crate::state::workspace::EditorNativeContext;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitialNativeContextPreparation {
    WaitingForDocument,
    NotNeeded,
    Bound,
}

/// Native ownership can continue when the committed source still matches the
/// browser's value. Other replicas or a full context request reconciliation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorNativeCommit {
    pub context: EditorNativeContext,
    pub retain_native_value: bool,
}

const CONTEXT_HEADROOM_BYTES: usize = 4 * 1024;

pub(super) fn same_projection(first: &FoldProjection, second: &FoldProjection) -> bool {
    first.shares_text_version(second)
}

impl EditorNativeContext {
    pub fn projection(&self) -> &FoldProjection {
        &self.projection
    }

    /// Local UTF-16 offsets for the browser; source selections remain unclipped.
    pub fn native_selection(&self) -> Result<Selection, ProjectionError> {
        let visible = self.projection.input_selection(self.selections[0])?;
        let native = |offset| {
            self.projection
                .byte_to_textarea(offset)
                .map_err(|_| ProjectionError::InvalidOffset)
        };
        Ok(Selection {
            anchor: native(visible.anchor)?,
            head: native(visible.head)?,
        })
    }

    /// Programmatic selection echoes must not truncate a large source selection.
    pub fn source_selection(&self, native: Selection) -> Result<Selection, ProjectionError> {
        if native == self.native_selection()? {
            return Ok(self.selections[0]);
        }
        self.projection.source_native_selection(native)
    }

    fn current(&self, actions: EditorActions) -> bool {
        actions.key().as_ref() == Some(&self.key)
            && actions.workspace.pending_epoch.get_untracked() == self.epoch
            && actions.workspace.editor_read_revision.get_untracked() == self.read
            && actions.account_generation() == self.account
            && actions.workspace.editor_source_revision.get_untracked() == self.source
            && actions.workspace.editor_projection_revision.get_untracked()
                == self.projection_revision
            && actions.workspace.content.with_untracked(|source| {
                actions
                    .workspace
                    .editor_documents
                    .with_untracked(|documents| {
                        documents.get(&self.key).is_some_and(|document| {
                            document.matches_text(source)
                                && document.revision() == self.revision
                                && document.selections() == self.selections
                                && same_projection(&document.projection(), &self.original)
                        })
                    })
            })
    }
}

impl EditorActions {
    /// Install source-owned surrounding text before initial layout.
    /// Geometry remains explicitly pending; this does not publish source extents.
    pub fn begin_initial_native_context(self) -> bool {
        self.prepare_initial_native_context() == InitialNativeContextPreparation::Bound
    }

    /// Document preparation may finish after the input node mounts. Only that
    /// unfinished preparation is retryable; a released geometry fallback must
    /// not restart initial binding indefinitely.
    pub fn prepare_initial_native_context(self) -> InitialNativeContextPreparation {
        if self.is_composing() || self.bound_native_context().is_some() || self.limit().is_some() {
            return InitialNativeContextPreparation::NotNeeded;
        }
        let Some(mut context) = self.native_context() else {
            return InitialNativeContextPreparation::WaitingForDocument;
        };
        if !context.projection.is_windowed() {
            return InitialNativeContextPreparation::NotNeeded;
        }
        context.geometry_pending = true;
        self.set_native_binding(Some(context));
        InitialNativeContextPreparation::Bound
    }

    pub fn native_geometry_pending(self) -> bool {
        self.native_binding.with_untracked(|context| {
            context
                .as_ref()
                .is_some_and(|context| context.geometry_pending && context.current(self))
        })
    }

    /// Fixed CSS row boxes provide vertical source extent before glyph widths.
    pub fn initial_native_height(self, row_height: f64, padding: f64) -> Option<f64> {
        if self.preferences().word_wrap || !self.native_geometry_pending() {
            return None;
        }
        let context = self.bound_native_context()?;
        if context.geometry_failed || !context.original.has_uniform_rows() {
            return None;
        }
        openwebide_core::editor::DocumentExtent::uniform_height(
            context.original.lines().len(),
            row_height,
            padding,
        )
    }

    /// The DOM adapter confirms the current measured extent and source paint.
    pub fn finish_initial_native_context(self, revision: u64) -> bool {
        if self.view_revision() != revision || self.measured_rows().is_none() {
            return false;
        }
        let Some(mut context) = self.bound_native_context().filter(|context| {
            context.geometry_pending && !context.geometry_failed && context.current(self)
        }) else {
            return false;
        };
        context.geometry_pending = false;
        self.set_native_binding(Some(context));
        true
    }

    pub(super) fn fail_initial_native_context(self) {
        let Some(mut context) = self
            .bound_native_context()
            .filter(|context| context.geometry_pending && context.current(self))
        else {
            return;
        };
        if self.is_composing() {
            // Keep the installed native value until the input method commits or cancels.
            context.geometry_failed = true;
            self.set_native_binding(Some(context));
        } else {
            self.release_native_context();
        }
    }

    pub(super) fn release_failed_initial_native_context(self) {
        if !self.is_composing()
            && self.native_binding.with_untracked(|context| {
                context
                    .as_ref()
                    .is_some_and(|context| context.geometry_failed)
            })
        {
            self.release_native_context();
        }
    }

    pub(super) fn set_native_binding(self, context: Option<EditorNativeContext>) {
        if self
            .native_binding
            .with_untracked(|previous| previous != &context)
        {
            self.native_binding_generation
                .update_value(|generation| *generation = generation.wrapping_add(1));
            self.native_binding.set(context);
        }
    }

    pub fn native_input_generation(self) -> Option<u64> {
        self.native_binding
            .with_untracked(Option::is_some)
            .then(|| self.native_binding_generation.get_value())
    }

    /// The DOM stamps a context only after installing its value and selection.
    /// Events from a previously installed native window cannot edit its replacement.
    pub fn native_input_current(self, bound: bool, generation: Option<&str>) -> bool {
        match (bound, self.native_input_generation()) {
            (true, Some(current)) => {
                generation.and_then(|value| value.parse::<u64>().ok()) == Some(current)
            }
            (false, None) => true,
            _ => false,
        }
    }

    /// Complete native source geometry can bridge cold unwrapped row preparation.
    /// Never read a local window as if it described the whole document.
    pub fn cold_native_extent(
        self,
        frame_scope: u64,
        width: f64,
        height: f64,
        native: impl FnOnce() -> String,
    ) -> Option<openwebide_core::editor::DocumentExtent> {
        if frame_scope != self.projection_revision()
            || self.bound_native_context().is_some()
            || self.is_composing()
            || self.preferences().word_wrap
        {
            return None;
        }
        let extent = openwebide_core::editor::DocumentExtent::new(width, height)?;
        let projection = self.projection()?;
        if !projection.has_uniform_rows() || native() != projection.textarea_text() {
            return None;
        }
        Some(extent)
    }

    /// Enable a native window once the view can supply document geometry.
    /// Composition retains its existing context until its native commit rebases it.
    pub fn bind_native_context(self) -> bool {
        if self.is_composing() {
            return self.native_binding.with_untracked(Option::is_some);
        }
        let context = self.native_context();
        if context
            .as_ref()
            .is_some_and(|context| !context.projection.is_windowed())
        {
            // Complete native text supplies its own extents. Keeping an old
            // window binding here would retain the larger document's dimensions.
            self.release_native_context();
            return false;
        }
        let bound = context.is_some();
        self.set_native_binding(context);
        bound
    }

    pub fn track_native_context(self) {
        self.native_binding.track();
    }

    pub fn release_native_context(self) {
        self.set_native_binding(None);
    }

    pub fn bound_native_context(self) -> Option<EditorNativeContext> {
        self.native_binding.get_untracked()
    }

    pub(super) fn refresh_native_binding(self) {
        let Some(mut context) = self.native_binding.get_untracked() else {
            return;
        };
        let previous = context.projection_revision;
        context.projection_revision = self.workspace.editor_projection_revision.get_untracked();
        // Provider metadata can change without changing native text or its mapping.
        // Full source, selection and immutable projection provenance still apply.
        if previous != context.projection_revision && context.current(self) {
            self.set_native_binding(Some(context));
        }
    }

    pub fn input_projection(self) -> Option<FoldProjection> {
        self.release_failed_initial_native_context();
        self.refresh_native_binding();
        if let Some(context) = self.native_binding.get_untracked() {
            if context.current(self) {
                return Some(context.projection);
            }
            if self.bind_native_context() {
                return self.native_binding.with_untracked(|context| {
                    context.as_ref().map(|context| context.projection.clone())
                });
            }
        }
        self.projection()
    }

    pub fn input_source_selection(self, native: Selection) -> Option<Selection> {
        self.refresh_native_binding();
        self.native_binding.with_untracked(|context| {
            context.as_ref().map(|context| {
                if context.current(self) {
                    context
                        .source_selection(native)
                        .unwrap_or(context.selections[0])
                } else {
                    self.workspace
                        .content
                        .with_untracked(|source| self.selection(source).unwrap_or_default())
                }
            })
        })
    }

    /// Map browser offsets through the installed window, folded view or source.
    /// Only folded unbound input needs its value read to prove its mapping.
    pub fn input_selection(self, native: Selection, value: impl FnOnce() -> String) -> Selection {
        if let Some(selection) = self.input_source_selection(native) {
            return selection;
        }
        if let Some(projection) = self.projection().filter(FoldProjection::is_folded)
            && value() == projection.textarea_text()
            && let Ok(selection) = projection.source_native_selection(native)
        {
            return selection;
        }
        self.workspace
            .content
            .with_untracked(|source| self.native_selection(source, native))
    }

    pub fn native_context(self) -> Option<EditorNativeContext> {
        let key = self.key()?;
        self.workspace.content.with_untracked(|source| {
            self.workspace.editor_documents.with_untracked(|documents| {
                let document = documents
                    .get(&key)
                    .filter(|document| document.matches_text(source))?;
                if document.is_composing() != self.is_composing() {
                    return None;
                }
                let original = document.projection();
                let projection = document
                    .input_context(NATIVE_CONTEXT_BYTES - CONTEXT_HEADROOM_BYTES)
                    .ok()?;
                let geometry_pending = self.native_binding.with_untracked(|previous| {
                    previous.as_ref().is_some_and(|previous| {
                        previous.geometry_pending
                            && previous.key == key
                            && previous.read == self.workspace.editor_read_revision.get_untracked()
                            && previous.account == self.account_generation()
                    })
                });
                let geometry_failed = geometry_pending
                    && self.native_binding.with_untracked(|previous| {
                        previous
                            .as_ref()
                            .is_some_and(|previous| previous.geometry_failed)
                    });
                Some(EditorNativeContext {
                    key,
                    epoch: self.workspace.pending_epoch.get_untracked(),
                    read: self.workspace.editor_read_revision.get_untracked(),
                    account: self.account_generation(),
                    source: self.workspace.editor_source_revision.get_untracked(),
                    projection_revision: self.workspace.editor_projection_revision.get_untracked(),
                    revision: document.revision(),
                    original,
                    projection,
                    selections: document.selections().to_vec(),
                    geometry_pending,
                    geometry_failed,
                })
            })
        })
    }

    /// Replay a bounded browser value using the same transaction/history path as
    /// the full projection. A stale context cannot edit another document/account.
    pub fn native_context_input(
        self,
        context: &EditorNativeContext,
        value: &str,
        selection: Selection,
        input_type: &str,
        timestamp: f64,
    ) -> Result<EditorNativeCommit, EditError> {
        if !context.current(self) {
            return Err(EditError::StaleContext);
        }
        self.replay_projected_input(
            &context.projection,
            &context.original,
            value,
            selection,
            input_type,
            timestamp,
        )?;
        let mut context = self.native_context().ok_or(EditError::StaleContext)?;
        let retained = (|| {
            if value.len() > NATIVE_CONTEXT_BYTES && !self.is_composing() {
                return None;
            }
            let native_head = openwebide_core::editor::byte_to_utf16(value, selection.head).ok()?;
            let visible_head = context
                .original
                .visible_selection(context.selections[0])
                .ok()?
                .head;
            let global_head = context.original.byte_to_textarea(visible_head).ok()?;
            let start = global_head.checked_sub(native_head)?;
            let end = start.checked_add(value.encode_utf16().count())?;
            let projection = context
                .original
                .window(
                    context.original.textarea_to_byte(start)
                        ..context.original.textarea_to_byte(end),
                )
                .ok()?;
            (projection.textarea_text() == value).then_some(projection)
        })();
        let retain_native_value = retained.is_some();
        if let Some(projection) = retained {
            context.projection = projection;
        }
        Ok(EditorNativeCommit {
            context,
            retain_native_value,
        })
    }
}
