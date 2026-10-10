//! Shared editor facade. DOM adapters provide text/selection/events; editing policy
//! lives in the Rust document engine without filesystem-mode branches.
use crate::state::workspace::EditorText;
use leptos::prelude::*;
use openwebide_core::editor::{Document, Edit, EditError, Indentation, Selection};

use crate::state::workspace::WorkspaceState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorCommand {
    Tab,
    Outdent,
    Newline,
    Undo,
    Redo,
    ConvertIndentation,
    TypeCharacter(char),
    DeletePair,
    Line(openwebide_core::editor::LineCommand),
    DuplicateSelection,
    LineComment,
    BlockComment,
    Reindent,
}

mod columns;
pub use columns::EditorColumnSelection;
mod decorations;
pub use decorations::EditorDecorations;
mod input;
mod motion;
pub use input::{EditorNativeCommit, EditorNativeContext, InitialNativeContextPreparation};
mod loading;
mod preparation;
mod rows;
#[cfg(feature = "test-support")]
pub fn take_paint_run_segment_bytes() -> usize {
    rows::take_paint_run_segment_bytes()
}
pub use rows::{
    EditorFragmentCache, EditorFragmentWindow, EditorParagraphSuffix, EditorRowSourceSlice,
};

/// Identity of a retained editor presentation; source revisions can advance
/// within it, while replacing a document or account releases the old frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorPresentationScope {
    project: i64,
    path: String,
    epoch: u64,
    read: u64,
    account: u64,
}

/// Facts established by a guarded native insertion transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeTextCommit {
    pub selection: Selection,
    pub retain_native_value: bool,
}

type TypingState = Option<((i64, String), String, f64)>;

const NATIVE_CONTEXT_BYTES: usize = 16 * 1024;

/// The browser inserts at the beginning of its selected range, independent of
/// selection direction. Bound that input mapping without clipping source edits.
fn insertion_projection(document: &Document) -> Option<openwebide_core::editor::FoldProjection> {
    document
        .projection()
        .input_context(
            Selection::caret(document.selections()[0].range().start),
            NATIVE_CONTEXT_BYTES,
        )
        .ok()
}

fn resolve_current_rules(
    workspace: WorkspaceState,
    preferences: Option<crate::state::settings::SettingsState>,
) -> openwebide_core::editor::EditorRules {
    let key = workspace
        .active_project
        .get()
        .zip(workspace.open_file.get());
    let defaults = preferences
        .map(|settings| settings.editor_preferences.get())
        .unwrap_or_default();
    let mut rules = workspace
        .editor_rules
        .with(|rules| key.as_ref().and_then(|key| rules.get(key).cloned()))
        .unwrap_or_else(|| {
            #[cfg(test)]
            source_tests::RULE_DETECTIONS.with(|count| count.set(count.get() + 1));
            workspace
                .content
                .with(|source| openwebide_core::editor::resolve_rules("", source, defaults, &[]).0)
        });
    if let Some(indentation) = workspace
        .editor_indentation
        .with(|values| key.as_ref().and_then(|key| values.get(key).copied()))
    {
        rules.indentation = indentation;
    }
    rules
}

#[derive(Clone, Copy)]
pub struct EditorActions {
    workspace: WorkspaceState,
    auth: Option<crate::state::auth::AuthState>,
    preferences: Option<crate::state::settings::SettingsState>,
    capacity: Memo<Option<openwebide_core::editor::EditorLimit>>,
    rules: Memo<openwebide_core::editor::EditorRules>,
    group: RwSignal<u64>,
    typing: RwSignal<TypingState>,
    native_binding: RwSignal<Option<EditorNativeContext>>,
    native_binding_generation: StoredValue<u64>,
}

impl EditorActions {
    pub fn presentation_scope(self) -> Option<EditorPresentationScope> {
        Some(EditorPresentationScope {
            project: self.workspace.active_project.get()?,
            path: self.workspace.open_file.get()?,
            epoch: self.workspace.pending_epoch.get(),
            read: self.workspace.editor_read_revision.get(),
            account: self.auth.map_or(0, |auth| auth.generation.get()),
        })
    }

    pub fn account_generation(self) -> u64 {
        self.auth.map_or(0, |auth| auth.generation.get_untracked())
    }

    pub fn new(workspace: WorkspaceState) -> Self {
        let preferences = use_context::<crate::state::settings::SettingsState>();
        let capacity_documents = ArcRwSignal::from(workspace.editor_documents);
        Self {
            workspace,
            auth: use_context::<crate::state::auth::AuthState>(),
            preferences,
            rules: Memo::new(move |_| resolve_current_rules(workspace, preferences)),
            capacity: Memo::new(move |_| {
                workspace.content.with(|source| {
                    if let Some(key) = workspace
                        .active_project
                        .get_untracked()
                        .zip(workspace.open_file.get_untracked())
                    {
                        // A document update may evaluate this memo while holding
                        // the write guard. Its immutable source cache remains
                        // valid even when indexed totals cannot be borrowed.
                        capacity_documents.try_with_untracked(|documents| {
                            if let Some(document) = documents.get(&key)
                                && std::ptr::eq(document.text(), source.as_str())
                            {
                                source.record_admission(document.admission());
                            }
                        });
                    }
                    source.editor_limit()
                })
            }),
            group: workspace.editor_group,
            typing: RwSignal::new(None),
            native_binding: workspace.editor_native_binding,
            native_binding_generation: workspace.editor_native_generation,
        }
    }

    pub fn rules(self) -> openwebide_core::editor::EditorRules {
        self.rules.get()
    }

    pub fn preferences(self) -> openwebide_core::editor::EditorPreferences {
        self.preferences
            .map(|settings| settings.editor_preferences.get())
            .unwrap_or_default()
    }

    pub fn set_indentation(self, mut indentation: Indentation) {
        indentation.width = indentation.width();
        indentation.tab_width = indentation.tab_width();
        if let Some(key) = self.key() {
            self.workspace.editor_indentation.update(|values| {
                values.insert(key, indentation);
            });
        }
    }

    pub fn rules_untracked(self) -> openwebide_core::editor::EditorRules {
        untrack(|| self.rules())
    }

    pub fn prepare_save(
        self,
        rules: &openwebide_core::editor::EditorRules,
    ) -> Result<Option<EditorText>, EditError> {
        let Some(key) = self.key() else {
            return Ok(None);
        };
        self.typing.set(None);
        self.group.update(|group| *group = group.wrapping_add(1));
        let result = self
            .workspace
            .editor_documents
            .try_update(|documents| {
                let document = self.document(documents, key.clone());
                document.prepare_save(rules)?;
                Ok(Some(EditorText::from(document.shared_text())))
            })
            .unwrap_or(Ok(None));
        if let Ok(Some(text)) = &result {
            self.workspace.content.set(text.clone());
            self.publish_dirty(key);
        }
        result
    }

    pub fn begin_composition(self) {
        self.cancel_queued_motion(None);
        if self.is_composing() {
            return;
        }
        self.cancel_composition();
        let Some(key) = self.key() else {
            return;
        };
        self.group.update(|group| *group = group.wrapping_add(1));
        self.typing.set(None);
        let group = self.group.get_untracked();
        self.workspace.editor_documents.update(|documents| {
            self.document(documents, key.clone())
                .begin_composition(Some(group));
        });
        self.workspace
            .editor_composition
            .set(Some(crate::state::workspace::EditorComposition {
                key,
                epoch: self.workspace.pending_epoch.get_untracked(),
                read_revision: self.workspace.editor_read_revision.get_untracked(),
                account_generation: self.account_generation(),
            }));
    }

    pub fn composition_owner_current(
        self,
        owner: &crate::state::workspace::EditorComposition,
    ) -> bool {
        self.presentation_scope().is_some_and(|scope| {
            scope.project == owner.key.0
                && scope.path == owner.key.1
                && scope.epoch == owner.epoch
                && scope.read == owner.read_revision
                && scope.account == owner.account_generation
        })
    }

    pub fn is_composing(self) -> bool {
        self.workspace.editor_composition.with_untracked(|owner| {
            owner
                .as_ref()
                .is_some_and(|owner| untrack(|| self.composition_owner_current(owner)))
        })
    }

    pub fn cancel_composition(self) {
        let Some(owner) = self.workspace.editor_composition.get_untracked() else {
            return;
        };
        self.workspace.editor_composition.set(None);
        self.typing.set(None);
        let _ = self.workspace.editor_documents.try_update(|documents| {
            let Some(document) = documents.get_mut(&owner.key) else {
                return;
            };
            let _ = document.cancel_composition_with(|preview, restored| {
                if self.workspace.pending_epoch.get_untracked() != owner.epoch
                    || self.account_generation() != owner.account_generation
                {
                    return;
                }
                let current_read =
                    self.workspace.editor_read_revision.get_untracked() == owner.read_revision;
                self.workspace.snapshots.update(|snapshots| {
                    if let Some(snapshot) = snapshots.get_mut(&owner.key.0)
                        && snapshot.open_file.as_deref() == Some(&owner.key.1)
                        && snapshot.content == preview
                        && (self.key().as_ref() != Some(&owner.key) || current_read)
                    {
                        snapshot.content = restored.shared_text().into();
                        snapshot.dirty = restored.is_dirty();
                    }
                });
                if current_read
                    && self.key().as_ref() == Some(&owner.key)
                    && self.source_matches(preview)
                {
                    self.workspace
                        .content
                        .set(EditorText::from(restored.shared_text()));
                    self.workspace.dirty.set(restored.is_dirty());
                }
            });
        });
        self.release_failed_initial_native_context();
    }

    pub fn end_composition(self) -> Result<Option<Selection>, EditError> {
        let Some(owner) = self.workspace.editor_composition.get_untracked() else {
            return Ok(None);
        };
        if !untrack(|| self.composition_owner_current(&owner)) {
            self.cancel_composition();
            return Ok(None);
        }
        self.workspace.editor_composition.set(None);
        self.typing.set(None);
        let result = self
            .workspace
            .editor_documents
            .try_update(|documents| {
                let document = documents.get_mut(&owner.key)?;
                if !self.source_matches(document.text()) {
                    document.cancel_composition();
                    return None;
                }
                let outcome = document.end_composition();
                Some((
                    outcome,
                    EditorText::from(document.shared_text()),
                    document.selections()[0],
                    document.is_dirty(),
                ))
            })
            .flatten();
        if let Some((outcome, source, selection, dirty)) = result {
            self.workspace.content.set(source);
            self.workspace.dirty.set(dirty);
            self.release_failed_initial_native_context();
            return outcome.map(|_| Some(selection));
        }
        Ok(None)
    }

    pub fn projection_revision(self) -> u64 {
        self.workspace.editor_projection_revision.get()
    }
    pub fn view_revision(self) -> u64 {
        self.workspace.editor_view_revision.get()
    }
    pub fn layout_epoch(self) -> u64 {
        self.workspace.editor_layout_epoch.get()
    }
    pub fn font_epoch(self) -> u64 {
        self.workspace.editor_font_epoch.get()
    }
    pub fn measured_rows(self) -> Option<crate::state::workspace::EditorRowMeasurements> {
        let revision = self.view_revision();
        let account = self.auth.map_or(0, |auth| auth.generation.get());
        self.workspace.editor_rows.with(|rows| {
            rows.as_ref()
                .filter(|rows| rows.revision == revision && rows.account_generation == account)
                .cloned()
        })
    }
    pub fn invalidate_measured_rows(self) {
        self.workspace.editor_rows.set(None);
        self.workspace
            .editor_layout_epoch
            .update(|epoch| *epoch = epoch.wrapping_add(1));
    }
    pub fn begin_row_preparation(self, revision: u64, total: usize) -> Option<u64> {
        if self.workspace.editor_view_revision.get_untracked() != revision
            || total > openwebide_core::editor::MAX_EDITOR_LINES
        {
            return None;
        }
        self.workspace
            .editor_row_ticket
            .update(|ticket| *ticket = ticket.wrapping_add(1));
        let ticket = self.workspace.editor_row_ticket.get_untracked();
        self.workspace.editor_row_preparation.set(Some(
            crate::state::workspace::EditorRowPreparation {
                ticket,
                revision,
                syntax_revision: self.workspace.editor_preparation_revision.get_untracked(),
                completed: 0,
                total,
                paint: None,
                prefix: None,
                paragraph_coverage: None,
            },
        ));
        Some(ticket)
    }
    pub fn row_preparation_current(self, ticket: u64) -> bool {
        self.workspace
            .editor_row_preparation
            .with_untracked(|preparation| {
                preparation.as_ref().is_some_and(|preparation| {
                    preparation.ticket == ticket
                        && preparation.revision
                            == self.workspace.editor_view_revision.get_untracked()
                })
            })
    }
    /// Cold paint waits for the current source geometry instead of shaping the
    /// same long row a second time while its height/width probe is still active.
    pub fn row_geometry_is_pending(self) -> bool {
        let revision = self.view_revision();
        self.workspace.editor_row_preparation.with(|preparation| {
            preparation
                .as_ref()
                .is_some_and(|preparation| preparation.revision == revision)
        }) && self.measured_rows().is_none()
    }
    pub fn report_row_preparation(self, ticket: u64, completed: usize) {
        if !self.row_preparation_current(ticket) {
            return;
        }
        self.workspace.editor_row_preparation.update(|preparation| {
            if let Some(preparation) = preparation.as_mut()
                && completed >= preparation.completed
                && completed <= preparation.total
            {
                preparation.completed = completed;
            }
        });
    }
    pub fn end_row_preparation(self, ticket: u64) {
        self.workspace
            .editor_row_preparation
            .try_update(|preparation| {
                if preparation
                    .as_ref()
                    .is_some_and(|preparation| preparation.ticket == ticket)
                {
                    *preparation = None;
                }
            });
    }
    pub fn publish_measured_rows(
        self,
        revision: u64,
        metrics: String,
        rows: openwebide_core::editor::MeasuredRows,
    ) -> bool {
        self.publish_measured_paint(
            revision,
            metrics,
            rows,
            None,
            self.preferences().show_whitespace,
        )
    }
    pub fn publish_measured_paint(
        self,
        revision: u64,
        metrics: String,
        rows: openwebide_core::editor::MeasuredRows,
        syntax: Option<(bool, std::sync::Arc<openwebide_core::highlight::TokenRows>)>,
        whitespace: bool,
    ) -> bool {
        if self.workspace.editor_view_revision.get_untracked() != revision
            || self
                .projection()
                .is_none_or(|projection| projection.lines().len() != rows.len())
        {
            return false;
        }
        self.workspace
            .editor_rows
            .set(Some(crate::state::workspace::EditorRowMeasurements {
                syntax,
                whitespace,
                revision,
                account_generation: self.account_generation(),
                metrics,
                rows,
            }));
        true
    }

    pub fn limit(self) -> Option<openwebide_core::editor::EditorLimit> {
        self.capacity.get()
    }

    fn key(self) -> Option<(i64, String)> {
        if self.capacity.get_untracked().is_some() {
            return None;
        }
        Some((
            self.workspace.active_project.get_untracked()?,
            self.workspace.open_file.get_untracked()?,
        ))
    }

    pub fn is_current(self, project: i64, path: &str) -> bool {
        self.key().is_some_and(|(current_project, current_path)| {
            current_project == project && current_path == path
        })
    }

    fn document(
        self,
        documents: &mut std::collections::HashMap<(i64, String), Document>,
        key: (i64, String),
    ) -> &mut Document {
        let document = documents.entry(key).or_insert_with(|| {
            self.workspace
                .content
                .with_untracked(|text| Document::from_shared_text(text.shared()))
        });
        self.workspace.content.with_untracked(|text| {
            if !document.matches_text(text) {
                *document = Document::from_shared_text(text.shared());
            }
        });
        document.enforce_editor_limits();
        if !self.workspace.dirty.get_untracked() {
            document.mark_saved();
        }
        document
    }

    pub fn record_selection(self, selection: Selection) -> Result<(), EditError> {
        self.cancel_native_text();
        self.cancel_queued_motion(None);
        let Some(key) = self.key() else {
            return Ok(());
        };
        self.workspace
            .editor_documents
            .try_update(|documents| {
                let document = self.document(documents, key);
                if document.selections() != [selection] {
                    self.typing.set(None);
                }
                document.set_selections(vec![selection])
            })
            .unwrap_or(Ok(()))
    }

    /// Native select/scroll notifications also fire after programmatic restores.
    /// Keep secondary selections when the primary did not move; IME previews own
    /// their selections until the composition commits or cancels.
    pub fn record_native_selection(self, selection: Selection) -> Result<(), EditError> {
        let Some(key) = self.key() else {
            return Ok(());
        };
        if self
            .workspace
            .editor_text_insertion
            .with_untracked(|insertion| {
                insertion
                    .as_ref()
                    .is_some_and(|insertion| insertion.key == key)
            })
        {
            return Ok(());
        }
        self.workspace
            .editor_documents
            .try_update(|documents| {
                let document = self.document(documents, key);
                if document.is_composing() || document.selections().first() == Some(&selection) {
                    return Ok(());
                }
                self.cancel_queued_motion(None);
                self.typing.set(None);
                document.set_selections(vec![selection])
            })
            .unwrap_or(Ok(()))
    }

    /// The browser adapter supplies measurements only for the currently mounted document.
    pub fn record_scroll(self, project: i64, path: &str, top: f64, left: f64) {
        self.record_scroll_position(project, path, top, left, false);
    }

    /// Preserve user-requested source positions while native measurements are pending.
    pub fn request_scroll(
        self,
        project: i64,
        path: &str,
        view: u64,
        account: u64,
        top: f64,
        left: f64,
    ) {
        if view != self.view_revision()
            || account != self.account_generation()
            || openwebide_core::editor::DocumentExtent::new(left.max(0.0), top.max(0.0)).is_none()
        {
            return;
        }
        self.record_scroll_position(project, path, top, left, true);
    }

    /// A current source scroll surface can scroll while native geometry is pending.
    pub fn record_source_scroll(
        self,
        project: i64,
        path: &str,
        top: f64,
        left: f64,
        complete: bool,
    ) {
        // A proven vertical extent does not authorize a clamped horizontal echo.
        let left = if !complete && self.native_geometry_pending() {
            self.scroll().left
        } else {
            left
        };
        self.record_scroll_position(project, path, top, left, true);
    }

    fn record_scroll_position(self, project: i64, path: &str, top: f64, left: f64, source: bool) {
        if !self.is_current(project, path)
            || (!source && self.native_geometry_pending())
            || !top.is_finite()
            || !left.is_finite()
        {
            return;
        }
        if self.workspace.editor_scroll.with_untracked(|positions| {
            positions
                .get(&(project, path.to_owned()))
                .is_some_and(|position| {
                    position.top.to_bits() == top.max(0.0).to_bits()
                        && position.left.to_bits() == left.max(0.0).to_bits()
                })
        }) {
            return;
        }
        self.workspace.editor_scroll.update(|positions| {
            positions.insert(
                (project, path.to_string()),
                crate::state::workspace::EditorScroll {
                    top: top.max(0.0),
                    left: left.max(0.0),
                },
            );
        });
    }
    pub fn finish_row_preparation(
        self,
        ticket: u64,
        paint: crate::state::workspace::EditorRowPaint,
        result: Result<Option<openwebide_core::editor::MeasuredRows>, ()>,
    ) -> Option<&'static str> {
        if !self.row_preparation_current(ticket) || !self.row_paint_current(&paint) {
            self.end_row_preparation(ticket);
            return None;
        }
        let message = match result {
            Ok(Some(rows)) => {
                if self.publish_measured_paint(
                    self.workspace.editor_view_revision.get_untracked(),
                    paint.metrics.clone(),
                    rows.clone(),
                    Some((paint.prepared_source, paint.tokens.clone())),
                    paint.whitespace,
                ) {
                    self.workspace.editor_row_cache.set(Some(
                        crate::state::workspace::EditorRowCache { paint, rows },
                    ));
                }
                None
            }
            Ok(None) => None,
            Err(()) => {
                self.fail_initial_native_context();
                Some(
                    "The highlighted view is unavailable for this layout. You can keep editing the file.",
                )
            }
        };
        self.end_row_preparation(ticket);
        message
    }

    pub fn scroll(self) -> crate::state::workspace::EditorScroll {
        self.key()
            .and_then(|key| {
                self.workspace
                    .editor_scroll
                    .with_untracked(|positions| positions.get(&key).copied())
            })
            .unwrap_or_default()
    }

    /// Map native UTF-16 positions through the active document's shared index.
    pub fn native_selection(self, source: &str, selection: Selection) -> Selection {
        let indexed = self.key().and_then(|key| {
            self.workspace.editor_documents.with_untracked(|documents| {
                documents
                    .get(&key)
                    .filter(|document| document.matches_text(source))
                    .map(|document| document.native_selection(selection))
            })
        });
        indexed.unwrap_or_else(|| Selection {
            anchor: openwebide_core::editor::textarea_to_byte(source, selection.anchor),
            head: openwebide_core::editor::textarea_to_byte(source, selection.head),
        })
    }

    pub fn selection(self, text: &str) -> Option<Selection> {
        let key = self.key()?;
        self.workspace.editor_documents.with_untracked(|documents| {
            documents
                .get(&key)
                .filter(|document| document.matches_text(text))
                .and_then(|document| document.selections().first().copied())
        })
    }

    pub fn selections(self, text: &str) -> Vec<Selection> {
        let Some(key) = self.key() else {
            return Vec::new();
        };
        self.workspace.editor_documents.with_untracked(|documents| {
            documents
                .get(&key)
                .filter(|document| document.matches_text(text))
                .map(|document| document.selections().to_vec())
                .unwrap_or_default()
        })
    }

    /// Borrow current source for visual selection measurements, without publishing a file copy.
    pub fn current_visual_caret(
        self,
        index: usize,
        identity: &str,
    ) -> Option<openwebide_core::editor::VisualCaret> {
        self.workspace
            .content
            .with_untracked(|source| self.visual_caret(source, index, identity))
    }

    pub fn visual_caret(
        self,
        source: &str,
        index: usize,
        identity: &str,
    ) -> Option<openwebide_core::editor::VisualCaret> {
        let key = self.key()?;
        self.workspace.editor_documents.with_untracked(|documents| {
            documents
                .get(&key)
                .filter(|document| document.matches_text(source))
                .and_then(|document| document.visual_caret(index, identity))
        })
    }

    /// Source/scope checked selection commands share the document policy in both modes.
    pub fn selection_command(
        self,
        project: i64,
        path: &str,
        source: &str,
        command: openwebide_core::editor::SelectionCommand,
    ) -> Result<Option<Vec<Selection>>, openwebide_core::editor::SelectionError> {
        let rules = self.rules_untracked();
        let language = openwebide_core::highlight::language_from_path(path);
        if !self.is_current(project, path) || !self.source_matches(source) {
            return Ok(None);
        }
        let syntax = (command == openwebide_core::editor::SelectionCommand::Expand)
            .then(|| self.syntax_structure(|| true))
            .flatten();
        self.operate_selections(project, path, source, move |document| {
            if let Some(context) = &syntax {
                document.selection_command_with_context(command, rules.indentation, context)
            } else {
                document.selection_command(command, language, rules.indentation)
            }
        })
    }

    pub fn move_selections(
        self,
        project: i64,
        path: &str,
        source: &str,
        motion: openwebide_core::editor::SelectionMotion,
        extend: bool,
    ) -> Result<Option<Vec<Selection>>, openwebide_core::editor::SelectionError> {
        self.move_selections_in(project, path, source, motion, extend, None)
    }

    pub fn move_selections_with_layout(
        self,
        project: i64,
        path: &str,
        source: &str,
        motion: openwebide_core::editor::SelectionMotion,
        extend: bool,
        layout: &openwebide_core::editor::VisualLayout,
    ) -> Result<Option<Vec<Selection>>, openwebide_core::editor::SelectionError> {
        self.move_selections_in(project, path, source, motion, extend, Some(layout))
    }

    fn move_selections_in(
        self,
        project: i64,
        path: &str,
        source: &str,
        motion: openwebide_core::editor::SelectionMotion,
        extend: bool,
        layout: Option<&openwebide_core::editor::VisualLayout>,
    ) -> Result<Option<Vec<Selection>>, openwebide_core::editor::SelectionError> {
        let indentation = self.rules_untracked().indentation;
        self.operate_selections(project, path, source, move |document| {
            if let Some(layout) = layout {
                document.move_selections_with_layout(motion, extend, indentation, layout)
            } else {
                document.move_selections(motion, extend, indentation)
            }
        })
    }

    /// Pointer adapters borrow current source instead of retaining a file copy.
    pub fn begin_current_pointer_selection(
        self,
        project: i64,
        path: &str,
        offset: usize,
        clicks: u32,
        extend: bool,
    ) -> Result<
        Option<openwebide_core::editor::PointerSelection>,
        openwebide_core::editor::SelectionError,
    > {
        self.workspace.content.with_untracked(|source| {
            self.begin_pointer_selection(project, path, source, offset, clicks, extend)
        })
    }

    pub fn drag_current_pointer_selection(
        self,
        project: i64,
        path: &str,
        pointer: &openwebide_core::editor::PointerSelection,
        offset: usize,
    ) -> Result<Option<Vec<Selection>>, openwebide_core::editor::SelectionError> {
        self.workspace.content.with_untracked(|source| {
            self.drag_pointer_selection(project, path, source, pointer, offset)
        })
    }

    pub fn current_selection(self) -> Option<Selection> {
        self.workspace
            .content
            .with_untracked(|source| self.selection(source))
    }

    pub fn begin_pointer_selection(
        self,
        project: i64,
        path: &str,
        source: &str,
        offset: usize,
        clicks: u32,
        extend: bool,
    ) -> Result<
        Option<openwebide_core::editor::PointerSelection>,
        openwebide_core::editor::SelectionError,
    > {
        let mut pointer = None;
        self.operate_selections(project, path, source, |document| {
            pointer = Some(document.begin_pointer_selection(offset, clicks, extend)?);
            Ok(true)
        })?;
        Ok(pointer)
    }

    pub fn drag_pointer_selection(
        self,
        project: i64,
        path: &str,
        source: &str,
        pointer: &openwebide_core::editor::PointerSelection,
        offset: usize,
    ) -> Result<Option<Vec<Selection>>, openwebide_core::editor::SelectionError> {
        self.operate_selections(project, path, source, |document| {
            Ok(document.drag_pointer_selection(pointer, offset)?)
        })
    }

    pub fn toggle_cursor(
        self,
        project: i64,
        path: &str,
        source: &str,
        offset: usize,
    ) -> Result<Option<Vec<Selection>>, openwebide_core::editor::SelectionError> {
        self.operate_selections(project, path, source, move |document| {
            Ok(document.toggle_cursor(offset)?)
        })
    }

    pub fn select_columns(
        self,
        project: i64,
        path: &str,
        source: &str,
        anchor: usize,
        head: usize,
    ) -> Result<Option<Vec<Selection>>, openwebide_core::editor::SelectionError> {
        let indentation = self.rules_untracked().indentation;
        self.operate_selections(project, path, source, move |document| {
            document.select_columns(anchor, head, indentation)
        })
    }

    fn operate_selections(
        self,
        project: i64,
        path: &str,
        source: &str,
        operation: impl FnOnce(&mut Document) -> Result<bool, openwebide_core::editor::SelectionError>,
    ) -> Result<Option<Vec<Selection>>, openwebide_core::editor::SelectionError> {
        if !self.is_current(project, path) || !self.source_matches(source) {
            return Ok(None);
        }
        self.cancel_queued_motion(None);
        let Some(key) = self.key() else {
            return Ok(None);
        };
        let mut folds_changed = false;
        let result = self
            .workspace
            .editor_documents
            .try_update(|documents| {
                let document = self.document(documents, key);
                let folds = document.fold_state().clone();
                operation(document)?;
                folds_changed = document.fold_state() != &folds;
                Ok(Some(document.selections().to_vec()))
            })
            .unwrap_or(Ok(None));
        if matches!(result, Ok(Some(_))) {
            self.typing.set(None);
            if folds_changed {
                self.workspace
                    .editor_fold_revision
                    .update(|value| *value = value.wrapping_add(1));
            }
        }
        result
    }

    /// Parser-backed folding provider for the active buffer. DOM/worker adapters
    /// supply a deadline or cancellation primitive; all parsing policy is shared.
    pub fn syntax_folds(
        self,
        should_continue: impl FnMut() -> bool,
    ) -> Option<(
        openwebide_core::editor::SyntaxStatus,
        Vec<openwebide_core::editor::FoldRange>,
    )> {
        if self.capacity.get_untracked().is_some() {
            let key = self
                .workspace
                .active_project
                .get_untracked()
                .zip(self.workspace.open_file.get_untracked())?;
            self.workspace
                .editor_syntax
                .update(|cache| cache.remove(&key));
            return Some((openwebide_core::editor::SyntaxStatus::TooLarge, Vec::new()));
        }
        self.analyze_syntax(should_continue, |document, status| {
            (
                status,
                document.map_or_else(Vec::new, |document| document.folds().to_vec()),
            )
        })
    }

    pub fn syntax_structure(
        self,
        should_continue: impl FnMut() -> bool,
    ) -> Option<std::sync::Arc<openwebide_core::editor::Structure>> {
        self.analyze_syntax(should_continue, |document, _| {
            document.and_then(|document| document.structure().cloned())
        })
        .flatten()
    }

    pub fn preparation_revision(self) -> u64 {
        self.workspace.editor_worker_active.track();
        self.workspace.editor_fallback_active.track();
        self.workspace.editor_fallback_paint.track();
        self.workspace.editor_preparation_revision.get()
    }

    pub fn syntax_highlights(
        self,
    ) -> Option<std::sync::Arc<openwebide_core::highlight::TokenRows>> {
        self.analyze_syntax(
            || true,
            |document, _| document.and_then(|document| document.highlights().cloned()),
        )
        .flatten()
    }

    fn analyze_syntax<T>(
        self,
        mut should_continue: impl FnMut() -> bool,
        result_for: impl FnOnce(
            Option<&openwebide_core::editor::SyntaxAnalysis>,
            openwebide_core::editor::SyntaxStatus,
        ) -> T,
    ) -> Option<T> {
        let key = self.key()?;
        let epoch = self.workspace.pending_epoch.get_untracked();
        let read_revision = self.workspace.editor_read_revision.get_untracked();
        let account_generation = self.auth.map_or(0, |auth| auth.generation.get_untracked());
        let language = openwebide_core::highlight::language_from_path(&key.1);
        let worker_active = self.workspace.editor_worker_active.get_untracked();
        let worker_prepared = worker_active
            .then(|| self.workspace.editor_preparation.get_untracked())
            .flatten();
        let shared_source = worker_prepared
            .as_ref()
            .filter(|prepared| self.syntax_scope_current(&prepared.scope))
            .map(|prepared| prepared.scope.source.clone());
        let source =
            shared_source.unwrap_or_else(|| self.workspace.content.get_untracked().shared());
        let text = source.as_str();
        let tab_width = self.rules_untracked().indentation.tab_width();
        let result = if worker_active {
            if !should_continue() {
                Some(result_for(
                    None,
                    openwebide_core::editor::SyntaxStatus::Cancelled,
                ))
            } else {
                worker_prepared
                    .as_ref()
                    .filter(|prepared| self.syntax_scope_current(&prepared.scope))
                    .map(|prepared| result_for(prepared.analysis.as_deref(), prepared.status))
            }
        } else {
            let deadline = js_sys::Date::now() + 12.0;
            self.workspace
                .editor_syntax
                .try_update(|documents| {
                    let (status, prepared) = documents.prepare_shared(
                        key.clone(),
                        language,
                        source.clone(),
                        tab_width,
                        || js_sys::Date::now() <= deadline && should_continue(),
                    );
                    Some(result_for(prepared.as_deref(), status))
                })
                .flatten()
        };
        (self.key() == Some(key)
            && self.workspace.pending_epoch.get_untracked() == epoch
            && self.workspace.editor_read_revision.get_untracked() == read_revision
            && self.auth.map_or(0, |auth| auth.generation.get_untracked()) == account_generation
            && self.rules_untracked().indentation.tab_width() == tab_width
            && self
                .workspace
                .content
                .with_untracked(|current| current == text))
        .then_some(result)
        .flatten()
    }

    pub fn refresh_fold_ranges(
        self,
        should_continue: impl FnMut() -> bool,
    ) -> Option<openwebide_core::editor::SyntaxStatus> {
        let key = self.key()?;
        let epoch = self.workspace.pending_epoch.get_untracked();
        let text = self.workspace.content.get_untracked();
        let result = self.syntax_folds(should_continue)?;
        if self.key() != Some(key.clone())
            || self.workspace.pending_epoch.get_untracked() != epoch
            || self
                .workspace
                .content
                .with_untracked(|current| current != &text)
        {
            return None;
        }
        if result.0 == openwebide_core::editor::SyntaxStatus::Cancelled {
            return Some(result.0);
        }
        let ranges = result.1;
        let changed = self
            .workspace
            .editor_documents
            .try_update(|documents| self.document(documents, key).set_fold_ranges(ranges))
            .unwrap_or(false);
        if changed {
            self.workspace
                .editor_fold_revision
                .update(|value| *value = value.wrapping_add(1));
        }
        Some(result.0)
    }

    pub fn cursor_status(self) -> (usize, usize, usize) {
        self.workspace.content.with_untracked(|source| {
            let selection = self.selection(source).unwrap_or_default();
            let position = self
                .key()
                .and_then(|key| {
                    self.workspace.editor_documents.with_untracked(|documents| {
                        documents
                            .get(&key)
                            .filter(|document| document.matches_text(source))
                            .map(|document| document.line_column(selection.head))
                    })
                })
                .unwrap_or_else(|| openwebide_core::editor::line_column(source, selection.head));
            (
                position.0,
                position.1,
                source[selection.range()].chars().count(),
            )
        })
    }

    pub fn search(
        self,
        source: &str,
        query: &str,
        options: openwebide_core::editor::SearchOptions,
        scope: Option<std::ops::Range<usize>>,
    ) -> Result<Vec<openwebide_core::editor::SearchMatch>, openwebide_core::editor::SearchError>
    {
        openwebide_core::editor::SearchPattern::new(query, options)?.find(source, scope)
    }

    pub fn replace_search(
        self,
        source: &str,
        query: &str,
        options: openwebide_core::editor::SearchOptions,
        scope: Option<std::ops::Range<usize>>,
        replacement: &str,
        index: Option<usize>,
    ) -> Result<Option<Selection>, openwebide_core::editor::SearchError> {
        use openwebide_core::editor::{SearchError, SearchPattern};
        if self.workspace.is_resolving() || self.workspace.pending_diff.get_untracked().is_some() {
            return Err(SearchError::ReadOnly);
        }
        if !self.source_matches(source) {
            return Err(SearchError::ChangedDocument);
        }
        let Some(key) = self.key() else {
            return Ok(None);
        };
        let pattern = SearchPattern::new(query, options)?;
        self.typing.set(None);
        let result = self
            .workspace
            .editor_documents
            .try_update(|documents| {
                let document = self.document(documents, key.clone());
                if document.replace_search(&pattern, scope, replacement, index)? == 0 {
                    return Ok(None);
                }
                Ok(Some((
                    EditorText::from(document.shared_text()),
                    document.selections()[0],
                )))
            })
            .unwrap_or(Ok(None));
        self.publish_edit(key, result)
    }

    pub fn navigation_target(self, query: &str) -> Option<usize> {
        self.workspace
            .content
            .with_untracked(|source| openwebide_core::editor::navigation_target(source, query))
    }

    pub fn matching_bracket(self, offset: usize) -> Option<(usize, usize)> {
        let key = self.key()?;
        if !self
            .workspace
            .content
            .with_untracked(|source| openwebide_core::editor::has_adjacent_bracket(source, offset))
        {
            return None;
        }
        let scope = untrack(|| self.presentation_scope())?;
        let revision = self.workspace.editor_source_revision.get_untracked();
        let syntax = self.syntax_structure(|| true);
        if untrack(|| self.presentation_scope()).as_ref() != Some(&scope)
            || self.workspace.editor_source_revision.get_untracked() != revision
        {
            return None;
        }
        self.workspace.content.with_untracked(|source| {
            if let Some(context) = syntax {
                openwebide_core::editor::matching_bracket_with_context(source, &context, offset)
            } else {
                openwebide_core::editor::matching_bracket(
                    source,
                    openwebide_core::highlight::language_from_path(&key.1),
                    offset,
                )
            }
        })
    }

    /// Inspect source ownership without allocating another complete file value.
    pub fn source_matches(self, expected: &str) -> bool {
        self.workspace
            .content
            .with_untracked(|source| source == expected)
    }

    pub fn current_selections(self) -> Vec<Selection> {
        self.workspace
            .content
            .with_untracked(|source| self.selections(source))
    }

    pub fn source_len(self) -> usize {
        self.workspace.content.with_untracked(|text| text.len())
    }

    pub fn selection_count(self) -> usize {
        let Some(key) = self.key() else {
            return 0;
        };
        self.workspace.content.with_untracked(|source| {
            self.workspace.editor_documents.with_untracked(|documents| {
                documents
                    .get(&key)
                    .filter(|document| document.matches_text(source))
                    .map_or(0, |document| document.selections().len())
            })
        })
    }

    pub fn source(self) -> EditorText {
        self.workspace.content.get_untracked()
    }

    pub fn navigate(self, offset: usize) -> Result<Selection, EditError> {
        let source = self.workspace.content.get_untracked();
        let selection = Selection::caret(offset);
        self.record_selection(selection)?;
        let (line, _) = openwebide_core::editor::line_column(&source, offset);
        self.fold_command(openwebide_core::editor::FoldCommand::Reveal(line - 1));
        Ok(selection)
    }

    pub fn prepare_edit(self, selection: Selection) -> Result<(), EditError> {
        self.record_native_selection(selection)?;
        let Some(key) = self.key() else {
            return Ok(());
        };
        let changed = self
            .workspace
            .editor_documents
            .try_update(|documents| self.document(documents, key).reveal_selection())
            .unwrap_or(false);
        if changed {
            self.workspace
                .editor_fold_revision
                .update(|value| *value = value.wrapping_add(1));
        }
        Ok(())
    }

    pub fn fold_command(
        self,
        command: openwebide_core::editor::FoldCommand,
    ) -> Option<(openwebide_core::editor::FoldProjection, Selection)> {
        let key = self.key()?;
        self.cancel_queued_motion(None);
        self.typing.set(None);
        let result = self.workspace.editor_documents.try_update(|documents| {
            let document = self.document(documents, key);
            let changed = document.fold_command(command);
            (changed, (document.projection(), document.selections()[0]))
        });
        if result.as_ref().is_some_and(|(changed, _)| *changed) {
            self.workspace
                .editor_fold_revision
                .update(|value| *value = value.wrapping_add(1));
        }
        result.map(|(_, view)| view)
    }

    /// Install the source document before geometry/native input needs its rows.
    /// Syntax preparation can finish independently of this shared document index.
    pub fn prepare_projection(self) -> Option<openwebide_core::editor::FoldProjection> {
        if let Some(projection) = self.projection() {
            return Some(projection);
        }
        let key = self.key()?;
        self.workspace
            .editor_documents
            .try_update(|documents| self.document(documents, key).projection())
    }

    /// Source rows include hidden folds and the terminal empty row. Query the
    /// current document index instead of rescanning source for gutter digits.
    pub fn source_line_count(self) -> Option<usize> {
        let key = self.key()?;
        self.workspace.content.with_untracked(|text| {
            self.workspace.editor_documents.with_untracked(|documents| {
                documents
                    .get(&key)
                    .filter(|document| document.matches_text(text))
                    .map(Document::line_count)
            })
        })
    }

    pub fn projection(self) -> Option<openwebide_core::editor::FoldProjection> {
        let key = self.key()?;
        self.workspace.content.with_untracked(|text| {
            self.workspace.editor_documents.with_untracked(|documents| {
                documents
                    .get(&key)
                    .filter(|document| document.matches_text(text))
                    .map(Document::projection)
            })
        })
    }

    pub fn fold_state(self) -> Option<openwebide_core::editor::FoldState> {
        let key = self.key()?;
        self.workspace.content.with_untracked(|text| {
            self.workspace.editor_documents.with_untracked(|documents| {
                documents
                    .get(&key)
                    .filter(|document| document.matches_text(text))
                    .map(|document| document.fold_state().clone())
            })
        })
    }

    pub fn projected_input(
        self,
        value: String,
        selection: Selection,
        input_type: &str,
        timestamp: f64,
    ) -> Result<(), EditError> {
        self.refresh_native_binding();
        if let Some(context) = self.native_binding.get_untracked() {
            let commit =
                self.native_context_input(&context, &value, selection, input_type, timestamp)?;
            self.set_native_binding(Some(commit.context));
            return Ok(());
        }
        let Some(projection) = self.projection() else {
            return self.native_input(value, selection, input_type, timestamp);
        };
        self.replay_projected_input(
            &projection,
            &projection,
            &value,
            selection,
            input_type,
            timestamp,
        )
    }

    fn replay_projected_input(
        self,
        projection: &openwebide_core::editor::FoldProjection,
        original: &openwebide_core::editor::FoldProjection,
        value: &str,
        selection: Selection,
        input_type: &str,
        timestamp: f64,
    ) -> Result<(), EditError> {
        let key = self.key().ok_or(EditError::StaleContext)?;
        let revision = self
            .workspace
            .editor_documents
            .with_untracked(|documents| documents.get(&key).map(Document::revision))
            .ok_or(EditError::StaleContext)?;
        let (edit, selection) = self
            .workspace
            .content
            .with_untracked(|source| {
                projection.replay_edit(
                    source,
                    value,
                    selection,
                    input_type,
                    self.selection(source).unwrap_or_default(),
                )
            })
            .map_err(|_| EditError::InvalidRange)?;
        if edit.is_none() && input_type == "insertFromComposition" && !self.is_composing() {
            return Ok(());
        }
        self.native_input_prepared(input_type, timestamp, |document| {
            if document.revision() != revision
                || !input::same_projection(&document.projection(), original)
            {
                return Err(EditError::StaleContext);
            }
            Ok((edit, selection))
        })
    }

    fn native_history_group(
        self,
        key: &(i64, String),
        input_type: &str,
        timestamp: f64,
    ) -> (bool, Option<u64>) {
        let coalesces = matches!(
            input_type,
            "insertText"
                | "deleteContentBackward"
                | "deleteContentForward"
                | "insertCompositionText"
        );
        let same = coalesces
            && self.typing.with_untracked(|typing| {
                typing.as_ref().is_some_and(|(previous, kind, time)| {
                    previous == key
                        && kind == input_type
                        && timestamp >= *time
                        && timestamp - time <= 750.0
                })
            });
        let composing = self.is_composing();
        if !same && !composing {
            self.group.update(|group| *group = group.wrapping_add(1));
        }
        let group = (coalesces || composing).then(|| self.group.get_untracked());
        (coalesces, group)
    }

    pub fn cancel_native_text(self) {
        self.workspace.editor_text_insertion.set(None);
    }

    /// Capture the declared insertion while the browser still owns its original caret.
    pub fn begin_native_text(self, text: String) {
        self.cancel_native_text();
        if self.is_composing() || self.native_binding.with_untracked(Option::is_some) {
            return;
        }
        let Some(key) = self.key() else {
            return;
        };
        let insertion = self.workspace.editor_documents.with_untracked(|documents| {
            let document = documents.get(&key)?;
            let projection = insertion_projection(document)?;
            let visible = projection
                .visible_selection(Selection::caret(document.selections()[0].range().start))
                .ok()?;
            let at =
                projection.textarea_origin() + projection.byte_to_textarea(visible.head).ok()?;
            let added = text
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .encode_utf16()
                .count();
            Some(crate::state::workspace::EditorTextInsertion {
                key: key.clone(),
                source_revision: self.workspace.editor_source_revision.get_untracked(),
                retain_native_value: document.selections().len() == 1
                    && !document.projection().is_folded(),
                projection,
                document_revision: document.revision(),
                account_generation: self.auth.map_or(0, |auth| auth.generation.get_untracked()),
                selections: document.selections().to_vec(),
                native_caret: at.checked_add(added)?,
                text,
            })
        });
        self.workspace.editor_text_insertion.set(insertion);
    }

    /// Consume a matching native commit without copying/diffing the complete DOM value.
    /// Source/account changes reject the commit; browser transformations use replay.
    pub fn finish_native_text(
        self,
        data: Option<&str>,
        native: Selection,
        timestamp: f64,
    ) -> Result<Option<NativeTextCommit>, EditError> {
        let Some(insertion) = self.workspace.editor_text_insertion.get_untracked() else {
            return Ok(None);
        };
        self.cancel_native_text();
        let current = self.key().as_ref() == Some(&insertion.key)
            && self.workspace.editor_source_revision.get_untracked() == insertion.source_revision
            && self.auth.map_or(0, |auth| auth.generation.get_untracked())
                == insertion.account_generation
            && !self.is_composing()
            && self.workspace.editor_documents.with_untracked(|documents| {
                documents.get(&insertion.key).is_some_and(|document| {
                    document.revision() == insertion.document_revision
                        && insertion_projection(document).as_ref() == Some(&insertion.projection)
                        && (document.selections().len() == 1 && !document.projection().is_folded())
                            == insertion.retain_native_value
                        && document.selections() == insertion.selections
                })
            });
        if !current {
            return Err(EditError::StaleContext);
        }
        if data != Some(insertion.text.as_str())
            || native != Selection::caret(insertion.native_caret)
        {
            return Ok(None);
        }
        let retain_native_value = insertion.retain_native_value;
        self.insert_native_text(&insertion.text, timestamp)
            .map(|selection| {
                selection.map(|selection| NativeTextCommit {
                    selection,
                    retain_native_value,
                })
            })
    }

    /// Cancellable text events supply their insertion, not a complete DOM value.
    pub fn insert_native_text(
        self,
        text: &str,
        timestamp: f64,
    ) -> Result<Option<Selection>, EditError> {
        if self.is_composing() {
            return Err(EditError::CompositionActive);
        }
        let Some(key) = self.key() else {
            return Ok(None);
        };
        let (_, group) = self.native_history_group(&key, "insertText", timestamp);
        let result = self
            .workspace
            .editor_documents
            .try_update(|documents| {
                let document = self.document(documents, key.clone());
                document.insert_native_text(text, group)?;
                Ok((
                    EditorText::from(document.shared_text()),
                    document.selections()[0],
                ))
            })
            .unwrap_or(Err(EditError::InvalidSelection));
        match result {
            Ok((source, selection)) => {
                self.workspace.content.set(source);
                self.publish_dirty(key.clone());
                self.typing.set(Some((key, "insertText".into(), timestamp)));
                Ok(Some(selection))
            }
            Err(error) => {
                self.typing.set(None);
                Err(error)
            }
        }
    }

    pub fn native_input(
        self,
        text: String,
        selection: Selection,
        input_type: &str,
        timestamp: f64,
    ) -> Result<(), EditError> {
        if input_type == "insertFromComposition"
            && !self.is_composing()
            && self.workspace.content.with_untracked(|source| {
                openwebide_core::editor::textarea_value_matches(source, &text)
            })
        {
            return Ok(());
        }
        self.native_input_prepared(input_type, timestamp, |document| {
            let edit = document.native_replacement(&text);
            let native = Selection {
                anchor: openwebide_core::editor::byte_to_utf16(&text, selection.anchor)?,
                head: openwebide_core::editor::byte_to_utf16(&text, selection.head)?,
            };
            let after = document.native_selection_after(edit.as_ref(), native)?;
            Ok((edit, after))
        })
    }

    fn native_input_prepared(
        self,
        input_type: &str,
        timestamp: f64,
        prepare: impl FnOnce(&Document) -> Result<(Option<Edit>, Selection), EditError>,
    ) -> Result<(), EditError> {
        let Some(key) = self.key() else {
            return Ok(());
        };
        let composing = self.is_composing();
        if matches!(
            input_type,
            "insertCompositionText" | "insertFromComposition"
        ) && !composing
        {
            return Err(EditError::UnsupportedNativeInput);
        }
        if composing
            && !self.workspace.editor_documents.with_untracked(|documents| {
                documents.get(&key).is_some_and(|document| {
                    document.is_composing() && self.source_matches(document.text())
                })
            })
        {
            self.cancel_composition();
            return Err(EditError::UnsupportedNativeInput);
        }
        if !composing
            && self
                .workspace
                .editor_documents
                .with_untracked(|documents| documents.get(&key).is_some_and(Document::is_composing))
        {
            self.cancel_composition();
            return Err(EditError::StaleContext);
        }
        let (coalesces, group) = self.native_history_group(&key, input_type, timestamp);
        let result = self
            .workspace
            .editor_documents
            .try_update(|documents| {
                let document = self.document(documents, key.clone());
                let outcome = (|| {
                    let (edit, after) = prepare(document)?;
                    document.native_edit(
                        edit,
                        after,
                        openwebide_core::editor::NativeInputKind::from_input_type(input_type),
                        group,
                    )?;
                    Ok(())
                })();
                if outcome.is_err() {
                    document.cancel_composition();
                }
                Some((outcome, EditorText::from(document.shared_text())))
            })
            .flatten();
        if let Some((outcome, text)) = result {
            self.workspace.content.set(text);
            self.publish_dirty(key.clone());
            self.typing
                .set((outcome.is_ok() && coalesces).then(|| (key, input_type.into(), timestamp)));
            if outcome.is_err() && composing {
                self.workspace.editor_composition.set(None);
            }
            return outcome;
        }
        Ok(())
    }

    pub fn command(
        self,
        command: EditorCommand,
        selection: Selection,
        indentation: Indentation,
    ) -> Result<Option<Selection>, EditError> {
        let Some(key) = self.key() else {
            return Ok(None);
        };
        let syntax = if matches!(
            command,
            EditorCommand::TypeCharacter(_)
                | EditorCommand::DeletePair
                | EditorCommand::Newline
                | EditorCommand::Reindent
                | EditorCommand::BlockComment
                | EditorCommand::LineComment
        ) {
            self.syntax_structure(|| true)
        } else {
            None
        };
        if self.key() != Some(key.clone()) {
            return Ok(None);
        }
        self.workspace
            .editor_command_revision
            .update(|revision| *revision = revision.wrapping_add(1));
        self.command_prepared(key, command, selection, indentation, syntax)
    }

    fn command_prepared(
        self,
        key: (i64, String),
        command: EditorCommand,
        selection: Selection,
        indentation: Indentation,
        syntax: Option<std::sync::Arc<openwebide_core::editor::Structure>>,
    ) -> Result<Option<Selection>, EditError> {
        self.typing.set(None);
        let result = self
            .workspace
            .editor_documents
            .try_update(|documents| {
                let document = self.document(documents, key.clone());
                if document.selections().first() != Some(&selection) {
                    document.set_selections(vec![selection])?;
                }
                match command {
                    EditorCommand::ConvertIndentation => {
                        document.convert_indentation(indentation, indentation.tab_width())?;
                    }
                    EditorCommand::Tab => {
                        document.tab(indentation)?;
                    }
                    EditorCommand::Outdent => {
                        document.indent_lines(indentation, true)?;
                    }
                    EditorCommand::Newline => {
                        if let Some(syntax) = &syntax {
                            document.newline_with_context(
                                indentation,
                                self.rules_untracked().line_ending,
                                syntax,
                            )?;
                        } else {
                            document.newline_with_structure(
                                indentation,
                                self.rules_untracked().line_ending,
                                openwebide_core::highlight::language_from_path(&key.1),
                            )?;
                        }
                    }
                    EditorCommand::TypeCharacter(ch) => {
                        if let Some(syntax) = &syntax {
                            document.type_character_with_context(ch, syntax)?;
                        } else {
                            document.type_character(
                                ch,
                                openwebide_core::highlight::language_from_path(&key.1),
                            )?;
                        }
                    }
                    EditorCommand::DeletePair => {
                        let changed = if let Some(syntax) = &syntax {
                            document.delete_pairs_with_context(syntax)?
                        } else {
                            document.delete_empty_pairs(
                                openwebide_core::highlight::language_from_path(&key.1),
                            )?
                        };
                        if !changed {
                            return Ok(None);
                        }
                    }
                    EditorCommand::Line(command) => {
                        document.line_command(command, self.rules_untracked().line_ending)?;
                    }
                    EditorCommand::DuplicateSelection => {
                        document.duplicate_selections()?;
                    }
                    EditorCommand::LineComment => {
                        if let Some(syntax) = &syntax {
                            document.toggle_line_comments_with_context(syntax)?;
                        } else {
                            document.toggle_line_comments(
                                openwebide_core::highlight::language_from_path(&key.1),
                            )?;
                        }
                    }
                    EditorCommand::BlockComment => {
                        if let Some(syntax) = &syntax {
                            document.toggle_block_comments_with_context(syntax)?;
                        } else {
                            document.toggle_block_comments(
                                openwebide_core::highlight::language_from_path(&key.1),
                            )?;
                        }
                    }
                    EditorCommand::Reindent => {
                        if let Some(syntax) = &syntax {
                            document.reindent_with_context(indentation, syntax)?;
                        } else {
                            document.reindent(
                                indentation,
                                openwebide_core::highlight::language_from_path(&key.1),
                            )?;
                        }
                    }
                    EditorCommand::Undo => {
                        document.undo();
                    }
                    EditorCommand::Redo => {
                        document.redo();
                    }
                }
                Ok(Some((
                    EditorText::from(document.shared_text()),
                    document.selections()[0],
                )))
            })
            .unwrap_or(Ok(None));
        self.publish_edit(key, result)
    }

    pub fn reindent_when_ready(
        self,
        selection: Selection,
        indentation: Indentation,
    ) -> impl std::future::Future<Output = Result<Option<Selection>, EditError>> {
        self.command_when_ready(EditorCommand::Reindent, selection, indentation)
    }

    /// Explicit structural actions wait for complete context when their behavior
    /// depends on parsed language bodies. Single-language comments retain their
    /// immediate fallback, including admitted files above the structure budget.
    pub fn command_requires_structure(self, command: EditorCommand) -> bool {
        let language = self
            .key()
            .map_or(openwebide_core::highlight::Language::Plain, |key| {
                openwebide_core::highlight::language_from_path(&key.1)
            });
        command == EditorCommand::Reindent
            || (matches!(
                command,
                EditorCommand::LineComment | EditorCommand::BlockComment
            ) && openwebide_core::editor::syntax_provider(language)
                .is_some_and(|provider| provider.injection.is_some()))
    }

    /// Reactive eligibility query. Reuse complete syntax only; opening a menu or
    /// moving a selection must not start grammar work on the UI thread.
    pub fn command_supported(self, command: EditorCommand) -> bool {
        self.workspace.active_project.track();
        self.workspace.open_file.track();
        self.workspace.content.track();
        self.workspace.editor_documents.track();
        self.workspace.editor_preparation.track();
        self.workspace.editor_syntax.track();
        self.workspace.editor_source_revision.track();
        self.workspace.editor_read_revision.track();
        self.workspace.pending_epoch.track();
        self.workspace.editor_rules.track();
        self.workspace.editor_indentation.track();
        self.preparation_revision();
        if let Some(auth) = self.auth {
            auth.generation.track();
        }
        let Some(key) = self.key() else {
            return false;
        };
        let language = openwebide_core::highlight::language_from_path(&key.1);
        let supported: fn(openwebide_core::highlight::Language) -> bool = match command {
            EditorCommand::LineComment => openwebide_core::editor::supports_line_comment,
            EditorCommand::BlockComment => openwebide_core::editor::supports_block_comment,
            EditorCommand::Reindent => return openwebide_core::editor::supports_reindent(language),
            _ => return true,
        };
        if !self.command_requires_structure(command) {
            return supported(language);
        }
        let prepared = self
            .workspace
            .editor_preparation
            .get_untracked()
            .filter(|prepared| self.syntax_scope_current(&prepared.scope));
        if prepared.as_ref().is_some_and(|prepared| {
            matches!(
                prepared.status,
                openwebide_core::editor::SyntaxStatus::TooLarge
            )
        }) {
            return false;
        }
        let structure = prepared
            .as_ref()
            .and_then(|prepared| prepared.analysis.as_ref())
            .and_then(|analysis| analysis.structure().cloned())
            .or_else(|| {
                let source = self
                    .workspace
                    .editor_syntax_scope
                    .get_untracked()
                    .filter(|scope| self.syntax_scope_current(scope))
                    .map_or_else(|| self.source().shared(), |scope| scope.source);
                self.workspace.editor_syntax.with_untracked(|cache| {
                    cache
                        .cached_analysis(
                            &key,
                            language,
                            &source,
                            self.rules_untracked().indentation.tab_width(),
                        )
                        .and_then(|analysis| analysis.structure().cloned())
                })
            });
        let Some(structure) = structure else {
            // Pending container actions can prepare their context on demand.
            return supported(language);
        };
        let selections = self.current_selections();
        if selections.is_empty() {
            return supported(structure.language_at(0));
        }
        selections
            .iter()
            .all(|selection| supported(structure.language_at(selection.range().start)))
    }

    pub fn command_when_ready(
        self,
        command: EditorCommand,
        selection: Selection,
        indentation: Indentation,
    ) -> impl std::future::Future<Output = Result<Option<Selection>, EditError>> {
        use openwebide_core::editor::{
            SyntaxAdmission, SyntaxAdmissionStatus, SyntaxReply, SyntaxRequest,
        };
        // Capture ownership when the action is requested, before its future is
        // first polled. Queued UI tasks must not acquire a different file's scope.
        let request = (|| -> Result<_, EditError> {
            if self.key().is_none() {
                return Ok(None);
            }
            let requires_structure = self.command_requires_structure(command);
            if self.is_composing() {
                return Err(EditError::CompositionActive);
            }
            self.record_native_selection(selection)?;
            self.workspace
                .editor_command_revision
                .update(|revision| *revision = revision.wrapping_add(1));
            let command_revision = self.workspace.editor_command_revision.get_untracked();
            let Some(scope) = self.syntax_scope() else {
                return Ok(None);
            };
            let selections = self.current_selections();
            let rules = self.rules_untracked().indentation;
            let source = self.source().shared();
            let revision = self
                .workspace
                .editor_documents
                .with_untracked(|documents| documents.get(&scope.key).map(Document::revision));
            Ok(Some((
                scope,
                selections,
                rules,
                source,
                revision,
                command_revision,
                requires_structure,
            )))
        })();
        async move {
            let Some((
                scope,
                selections,
                rules,
                source,
                revision,
                command_revision,
                requires_structure,
            )) = request?
            else {
                return Ok(None);
            };
            let current = || {
                self.syntax_scope_current(&scope)
                    && self.workspace.editor_command_revision.get_untracked() == command_revision
                    && !self.is_composing()
                    && self.workspace.content.with_untracked(|current| {
                        std::sync::Arc::ptr_eq(&current.shared(), &source)
                    })
                    && self.workspace.editor_documents.with_untracked(|documents| {
                        documents.get(&scope.key).is_some_and(|document| {
                            Some(document.revision()) == revision
                                && document.selections() == selections
                        })
                    })
                    && self.rules_untracked().indentation == rules
            };
            if !current() {
                return Ok(None);
            }
            if !requires_structure {
                return self.command(command, selection, indentation);
            }
            let mut syntax = self.syntax_structure(|| true);
            if syntax.is_none() {
                let mut admission = SyntaxAdmission::new(scope.source.clone());
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
                    openwebide_core::highlight::language_from_path(&scope.key.1),
                    &scope.source,
                    indentation.tab_width(),
                    None,
                );
                let message =
                    serde_json::to_string(&request).expect("syntax request is serializable");
                let reply = crate::editor_worker::CooperativeClient::default()
                    .request_while(message, current)
                    .await
                    .map_err(|_| EditError::StructureUnavailable)?;
                if !current() {
                    return Ok(None);
                }
                syntax = SyntaxReply::receive_shared(&reply, 1, scope.source.clone(), None)
                    .and_then(|(_, analysis)| analysis)
                    .and_then(|analysis| analysis.structure().cloned());
            }
            if !current() {
                return Ok(None);
            }
            let syntax = syntax.ok_or(EditError::StructureUnavailable)?;
            self.command_prepared(scope.key, command, selection, indentation, Some(syntax))
        }
    }

    pub fn clipboard_content(
        self,
        selection: Selection,
    ) -> Result<Option<openwebide_core::editor::ClipboardContent>, EditError> {
        if self.is_composing() {
            return Err(EditError::CompositionActive);
        }
        self.record_native_selection(selection)?;
        let Some(key) = self.key() else {
            return Ok(None);
        };
        self.workspace.editor_documents.with_untracked(|documents| {
            documents
                .get(&key)
                .filter(|document| self.source_matches(document.text()))
                .map(Document::clipboard_content)
                .transpose()
        })
    }

    pub fn paste(self, text: &str, selection: Selection) -> Result<Option<Selection>, EditError> {
        self.paste_clipboard(text, None, selection)
    }

    pub fn paste_clipboard(
        self,
        text: &str,
        metadata: Option<&str>,
        selection: Selection,
    ) -> Result<Option<Selection>, EditError> {
        self.edit_clipboard(Some((text, metadata)), selection)
    }

    pub fn cut(self, selection: Selection) -> Result<Option<Selection>, EditError> {
        self.edit_clipboard(None, selection)
    }

    fn edit_clipboard(
        self,
        pasted: Option<(&str, Option<&str>)>,
        selection: Selection,
    ) -> Result<Option<Selection>, EditError> {
        self.record_native_selection(selection)?;
        let Some(key) = self.key() else {
            return Ok(None);
        };
        self.typing.set(None);
        let result = self
            .workspace
            .editor_documents
            .try_update(|documents| {
                let document = self.document(documents, key.clone());
                if let Some((text, metadata)) = pasted {
                    document.paste_clipboard(text, metadata)?;
                } else {
                    document.replace_selections("", None)?;
                }
                Ok(Some((
                    EditorText::from(document.shared_text()),
                    document.selections()[0],
                )))
            })
            .unwrap_or(Ok(None));
        self.publish_edit(key, result)
    }

    pub fn paste_with_indentation(
        self,
        text: &str,
        selection: Selection,
    ) -> Result<Option<Selection>, EditError> {
        self.paste_clipboard_with_indentation(text, None, selection)
    }

    pub fn paste_clipboard_with_indentation(
        self,
        text: &str,
        metadata: Option<&str>,
        selection: Selection,
    ) -> Result<Option<Selection>, EditError> {
        if self.is_composing() {
            return Err(EditError::CompositionActive);
        }
        let Some(key) = self.key() else {
            return Ok(None);
        };
        self.typing.set(None);
        let rules = self.rules_untracked();
        let result = self
            .workspace
            .editor_documents
            .try_update(|documents| {
                let document = self.document(documents, key.clone());
                if document.selections().first() != Some(&selection) {
                    document.set_selections(vec![selection])?;
                }
                document.paste_clipboard_with_indentation(
                    text,
                    metadata,
                    rules.indentation,
                    rules.line_ending,
                )?;
                Ok(Some((
                    EditorText::from(document.shared_text()),
                    document.selections()[0],
                )))
            })
            .unwrap_or(Ok(None));
        self.publish_edit(key, result)
    }

    /// Publish edited text once; callers only need the resulting caret selection.
    fn publish_edit<E>(
        self,
        key: (i64, String),
        result: Result<Option<(EditorText, Selection)>, E>,
    ) -> Result<Option<Selection>, E> {
        result.map(|result| {
            result.map(|(text, selection)| {
                self.workspace.content.set(text);
                self.publish_dirty(key);
                selection
            })
        })
    }

    fn publish_dirty(self, key: (i64, String)) {
        let dirty = self
            .workspace
            .editor_documents
            .with_untracked(|documents| documents.get(&key).is_some_and(Document::is_dirty));
        self.workspace.dirty.set(dirty);
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;
    use std::sync::Arc;

    thread_local! {
        pub(super) static RULE_DETECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn rule_queries_share_detection_and_refresh_source_defaults_overrides_and_file_ownership() {
        Owner::new().with(|| {
            let workspace = crate::state::workspace::WorkspaceState::new();
            let settings = crate::state::settings::SettingsState::new(
                crate::state::settings::Theme::Dark,
                String::new(),
            );
            provide_context(settings);
            workspace.active_project.set(Some(1));
            workspace.open_file.set(Some("fallback.rs".into()));
            workspace.content.set("x\n".repeat(99_999).into());
            let actions = EditorActions::new(workspace);
            RULE_DETECTIONS.with(|count| count.set(0));
            let initial = actions.rules_untracked();
            for _ in 0..1000 {
                assert_eq!(actions.rules_untracked(), initial);
            }
            RULE_DETECTIONS.with(|count| assert_eq!(count.get(), 1));
            workspace.content.set("x\r\n".into());
            assert_eq!(actions.rules_untracked().line_ending, initial.line_ending);
            RULE_DETECTIONS.with(|count| assert_eq!(count.get(), 2));
            settings
                .editor_preferences
                .update(|preferences| preferences.indentation.width = 8);
            assert_eq!(actions.rules_untracked().indentation.width(), 8);
            RULE_DETECTIONS.with(|count| assert_eq!(count.get(), 3));
            let mut loaded = initial.clone();
            loaded.indentation.width = 3;
            workspace.editor_rules.update(|rules| {
                rules.insert((1, "fallback.rs".into()), loaded);
            });
            assert_eq!(actions.rules_untracked().indentation.width(), 3);
            let mut override_indent = initial.indentation;
            override_indent.width = 6;
            actions.set_indentation(override_indent);
            assert_eq!(actions.rules_untracked().indentation.width(), 6);
            workspace.open_file.set(Some("other.rs".into()));
            assert_eq!(actions.rules_untracked().indentation.width(), 8);
            RULE_DETECTIONS.with(|count| assert_eq!(count.get(), 4));
            workspace.active_project.set(Some(2));
            assert_eq!(actions.rules_untracked().indentation.width(), 8);
            RULE_DETECTIONS.with(|count| assert_eq!(count.get(), 5));
        });
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn publication_shares_document_buffer_and_project_source_and_preserves_retained_views() {
        Owner::new().with(|| {
            let workspace = crate::state::workspace::WorkspaceState::new();
            workspace.active_project.set(Some(1));
            workspace.open_file.set(Some("source.rs".into()));
            workspace.content.set("文😀\r\nbody\n".into());
            let actions = EditorActions::new(workspace);
            actions.record_selection(Selection::caret(0)).unwrap();
            let source = actions.source();
            let document_source = workspace.editor_documents.with_untracked(|documents| {
                documents
                    .get(&(1, "source.rs".into()))
                    .unwrap()
                    .shared_text()
            });
            assert!(Arc::ptr_eq(&source.shared(), &document_source));
            workspace.retain_editor_buffer(false);
            let buffer = workspace
                .editor_buffers
                .get_untracked()
                .remove(&(1, "source.rs".into()))
                .unwrap();
            let snapshot = workspace.active_snapshot();
            assert!(Arc::ptr_eq(&source.shared(), &buffer.content.shared()));
            assert!(Arc::ptr_eq(&source.shared(), &snapshot.content.shared()));
            actions
                .command(
                    EditorCommand::TypeCharacter('x'),
                    Selection::caret(0),
                    Indentation::default(),
                )
                .unwrap();
            let edited = actions.source();
            let document_source = workspace.editor_documents.with_untracked(|documents| {
                documents
                    .get(&(1, "source.rs".into()))
                    .unwrap()
                    .shared_text()
            });
            assert!(Arc::ptr_eq(&edited.shared(), &document_source));
            assert!(!Arc::ptr_eq(&source.shared(), &edited.shared()));
            assert_eq!(source, "文😀\r\nbody\n");
            assert_eq!(buffer.content, source);
            assert_eq!(snapshot.content, source);
            assert_eq!(edited, "x文😀\r\nbody\n");
            let written = actions
                .prepare_save(&actions.rules_untracked())
                .unwrap()
                .unwrap();
            let prepared = workspace.editor_documents.with_untracked(|documents| {
                documents
                    .get(&(1, "source.rs".into()))
                    .unwrap()
                    .shared_text()
            });
            assert!(Arc::ptr_eq(&written.shared(), &prepared));
            assert!(Arc::ptr_eq(&written.shared(), &actions.source().shared()));
            assert_eq!(source, "文😀\r\nbody\n");
        });
    }
}
