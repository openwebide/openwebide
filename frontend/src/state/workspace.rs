use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, OnceLock},
};

use leptos::prelude::*;
use openwebide_core::{EditDecision, FileDiff, FileEntry, PersistedEdit, SearchHit};

/// Immutable editor source shared by documents, buffers and project snapshots.
#[derive(Clone, Debug, Default)]
pub struct EditorText(
    Arc<String>,
    Arc<OnceLock<Option<openwebide_core::editor::EditorLimit>>>,
);

impl PartialEq for EditorText {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl Eq for EditorText {}

impl EditorText {
    pub fn shared(&self) -> Arc<String> {
        self.0.clone()
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
    pub fn editor_limit(&self) -> Option<openwebide_core::editor::EditorLimit> {
        *self
            .1
            .get_or_init(|| openwebide_core::editor::editor_limit(self.as_str()))
    }
    /// Cache only core-minted decisions for this exact retained source allocation.
    pub fn record_admission(
        &self,
        result: openwebide_core::editor::EditorAdmissionResult<'_>,
    ) -> bool {
        result.matches(self.as_str()) && *self.1.get_or_init(|| result.limit()) == result.limit()
    }
}
impl std::ops::Deref for EditorText {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}
impl std::fmt::Display for EditorText {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self)
    }
}
impl From<String> for EditorText {
    fn from(text: String) -> Self {
        Arc::new(text).into()
    }
}
impl From<&str> for EditorText {
    fn from(text: &str) -> Self {
        text.to_owned().into()
    }
}
impl From<Arc<String>> for EditorText {
    fn from(text: Arc<String>) -> Self {
        Self(text, Arc::new(OnceLock::new()))
    }
}
impl From<EditorText> for String {
    fn from(text: EditorText) -> Self {
        Arc::unwrap_or_clone(text.0)
    }
}
impl PartialEq<str> for EditorText {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}
impl PartialEq<&str> for EditorText {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}
impl PartialEq<String> for EditorText {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other
    }
}
impl PartialEq<EditorText> for str {
    fn eq(&self, other: &EditorText) -> bool {
        self == other.as_str()
    }
}
impl PartialEq<EditorText> for &str {
    fn eq(&self, other: &EditorText) -> bool {
        *self == other.as_str()
    }
}
impl PartialEq<EditorText> for String {
    fn eq(&self, other: &EditorText) -> bool {
        self == other.as_str()
    }
}

/// Immutable surrounding text and its complete source selection. The full view
/// shares existing allocations and proves provenance; only the input text is sliced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorNativeContext {
    pub(crate) key: (i64, String),
    pub(crate) epoch: u64,
    pub(crate) read: u64,
    pub(crate) account: u64,
    pub(crate) source: u64,
    pub(crate) projection_revision: u64,
    pub(crate) revision: u64,
    pub(crate) original: openwebide_core::editor::FoldProjection,
    pub(crate) projection: openwebide_core::editor::FoldProjection,
    pub(crate) selections: Vec<openwebide_core::editor::Selection>,
    pub(crate) geometry_pending: bool,
    pub(crate) geometry_failed: bool,
}

/// An insertion declared by beforeinput, bound to the unchanged source until input.
#[derive(Clone, Debug)]
pub struct EditorTextInsertion {
    pub key: (i64, String),
    pub source_revision: u64,
    /// Bounded surrounding text; native_caret remains in the current full input.
    pub projection: openwebide_core::editor::FoldProjection,
    pub retain_native_value: bool,
    pub document_revision: u64,
    pub account_generation: u64,
    pub selections: Vec<openwebide_core::editor::Selection>,
    pub native_caret: usize,
    pub text: String,
}

/// Exact publication scope shared by preparation requests and synchronous consumers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorSyntaxScope {
    pub key: (i64, String),
    pub source: std::sync::Arc<String>,
    pub source_revision: u64,
    pub epoch: u64,
    pub read_revision: u64,
    pub account_generation: u64,
    pub tab_width: usize,
}
#[derive(Clone, Debug)]
pub struct PreparedEditorSyntax {
    pub scope: EditorSyntaxScope,
    pub status: openwebide_core::editor::SyntaxStatus,
    pub analysis: Option<std::sync::Arc<openwebide_core::editor::SyntaxAnalysis>>,
}

#[derive(Clone, Debug)]
pub struct EditorFallbackPaint {
    pub scope: EditorSyntaxScope,
    pub prepared_source: bool,
    pub tokens: std::sync::Arc<openwebide_core::highlight::TokenRows>,
    pub lexical: Option<std::sync::Arc<openwebide_core::highlight::LexicalSnapshot>>,
}

/// Browser measurements are bound to one exact source/projection/layout revision.
#[derive(Clone, Debug, PartialEq)]
pub struct EditorRowMeasurements {
    pub whitespace: bool,
    pub syntax: Option<(bool, std::sync::Arc<openwebide_core::highlight::TokenRows>)>,
    pub revision: u64,
    pub account_generation: u64,
    pub metrics: String,
    pub rows: openwebide_core::editor::MeasuredRows,
}

#[derive(Clone, Debug)]
pub struct EditorRowPaint {
    pub view_revision: u64,
    pub layout_epoch: u64,
    pub font_epoch: u64,
    pub key: (i64, String),
    pub epoch: u64,
    pub read_revision: u64,
    pub account_generation: u64,
    pub metrics: String,
    pub projection: openwebide_core::editor::FoldProjection,
    pub tokens: std::sync::Arc<openwebide_core::highlight::TokenRows>,
    pub prepared_source: bool,
    pub guides: std::sync::Arc<[usize]>,
    pub indentation: openwebide_core::editor::Indentation,
    pub whitespace: bool,
    pub word_wrap: bool,
}
#[derive(Clone, Debug)]
pub struct EditorParagraphCache {
    pub paint: EditorRowPaint,
    pub rows: Vec<(usize, Arc<openwebide_core::editor::ParagraphMeasurements>)>,
}

/// Original styled paint runs, independent of completed DOM geometry. An absent
/// table records unavailable metadata for this exact source/style scope, so a
/// rejected retention budget is not scanned again on every viewport update.
#[derive(Clone, Debug)]
pub struct EditorPaintRuns {
    pub paint: EditorRowPaint,
    pub rows: Vec<(usize, Option<Arc<[usize]>>)>,
}

#[derive(Clone, Debug)]
pub struct EditorRowCache {
    pub paint: EditorRowPaint,
    pub rows: openwebide_core::editor::MeasuredRows,
}

/// Measured origin coverage retains source/style provenance and remains separate
/// from completed document extents.
#[derive(Clone, Debug)]
pub struct EditorParagraphCoverage {
    pub paint: EditorRowPaint,
    pub coverage: Arc<openwebide_core::editor::WrappedCoverage>,
}

#[derive(Clone, Debug)]
pub struct EditorRowPreparation {
    pub ticket: u64,
    pub revision: u64,
    pub syntax_revision: u64,
    pub completed: usize,
    pub total: usize,
    pub paint: Option<EditorRowPaint>,
    /// Exact completed origin rows for early paint, not complete source extents.
    pub prefix: Option<openwebide_core::editor::MeasuredRows>,
    pub paragraph_coverage: Option<Arc<openwebide_core::editor::WrappedCoverage>>,
}

/// Scroll position for a document's edit view; caret and selection live in Document.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EditorScroll {
    pub top: f64,
    pub left: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorComposition {
    pub key: (i64, String),
    pub epoch: u64,
    pub read_revision: u64,
    pub account_generation: u64,
}

/// A loaded file retained independently of the currently selected document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorBuffer {
    pub content: EditorText,
    pub dirty: bool,
    pub read_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveredFileIssue {
    Pending,
    Conflict,
    Missing,
    Unavailable(String),
}

impl RecoveredFileIssue {
    pub fn message(&self) -> String {
        match self {
            Self::Pending => "Checking recovered file against disk…".into(),
            Self::Conflict => {
                "The disk file changed since this draft was saved. Your draft is preserved.".into()
            }
            Self::Missing => {
                "The recovered file is missing from disk. Your draft is preserved.".into()
            }
            Self::Unavailable(error) => format!("Could not verify recovered file: {error}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryOverwrite {
    pub disk: Option<String>,
    pub draft: String,
}

/// Captured before an asynchronous recovery load. A hydration must not replace
/// editor activity, pending reads, or a reset that occurred while loading.
#[derive(Clone, Debug)]
pub struct EditorRecoveryGuard {
    project: i64,
    state: openwebide_core::editor::EditorRecovery,
    pub(crate) read_revision: u64,
    pub(crate) epoch: u64,
    documents: HashMap<String, u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorRecoveryHydration {
    pub selected: Option<String>,
    pub read_only: bool,
    pub needs_read: bool,
    pub revoke_urls: Vec<String>,
}

pub(crate) struct PreparedEditorRecovery {
    documents: HashMap<(i64, String), openwebide_core::editor::Document>,
    buffers: HashMap<(i64, String), EditorBuffer>,
    scroll: HashMap<(i64, String), EditorScroll>,
    paths: Vec<String>,
    content: EditorText,
    dirty: bool,
    result: EditorRecoveryHydration,
}

impl PreparedEditorRecovery {
    fn new(
        project: i64,
        recovery: &openwebide_core::editor::EditorRecovery,
    ) -> Result<Self, String> {
        let mut documents = HashMap::new();
        for file in &recovery.files {
            if let Some(saved) = &file.document {
                documents.insert((project, file.path.clone()), saved.restore()?);
            }
        }
        Ok(Self::from_documents(project, recovery, documents))
    }

    pub(crate) fn from_documents(
        project: i64,
        recovery: &openwebide_core::editor::EditorRecovery,
        documents: HashMap<(i64, String), openwebide_core::editor::Document>,
    ) -> Self {
        let mut buffers = HashMap::new();
        let mut scroll = HashMap::new();
        let mut paths = Vec::with_capacity(recovery.files.len());
        for file in &recovery.files {
            let key = (project, file.path.clone());
            paths.push(file.path.clone());
            if let Some(document) = documents.get(&key) {
                buffers.insert(
                    key.clone(),
                    EditorBuffer {
                        content: document.shared_text().into(),
                        dirty: document.is_dirty(),
                        read_only: file.read_only,
                    },
                );
            }
            scroll.insert(
                key,
                EditorScroll {
                    top: file.scroll.top,
                    left: file.scroll.left,
                },
            );
        }
        let selected = recovery
            .selected
            .as_ref()
            .and_then(|path| buffers.get(&(project, path.clone())));
        let content = selected
            .map(|buffer| buffer.content.clone())
            .unwrap_or_default();
        let dirty = selected.is_some_and(|buffer| buffer.dirty);
        let read_only = recovery
            .files
            .iter()
            .any(|file| recovery.selected.as_deref() == Some(&file.path) && file.read_only);
        let result = EditorRecoveryHydration {
            selected: recovery.selected.clone(),
            read_only,
            needs_read: recovery.selected.is_some() && selected.is_none(),
            revoke_urls: Vec::new(),
        };
        Self {
            documents,
            buffers,
            scroll,
            paths,
            content,
            dirty,
            result,
        }
    }
}

/// Workspace data preserved while a project is not the active tab.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceSnapshot {
    pub entries: HashMap<String, Vec<FileEntry>>,
    pub expanded: HashSet<String>,
    pub open_file: Option<String>,
    pub content: EditorText,
    pub dirty: bool,
    pub search: Option<Vec<SearchHit>>,
    pub active_session: Option<i64>,
    pub pending_edits: HashMap<String, FileDiff>,
    pub persisted_edits: HashMap<String, PersistedEdit>,
    pub media_url: Option<String>,
}

/// Active editor and file-browser state, with a snapshot for each project.
#[derive(Clone, Copy)]
pub struct WorkspaceState {
    pub active_project: RwSignal<Option<i64>>,
    pub entries: RwSignal<HashMap<String, Vec<FileEntry>>>,
    pub expanded: RwSignal<HashSet<String>>,
    pub open_file: RwSignal<Option<String>>,
    pub content: RwSignal<EditorText>,
    pub dirty: RwSignal<bool>,
    pub search: RwSignal<Option<Vec<SearchHit>>>,
    pub active_session: RwSignal<Option<i64>>,
    pub pending_edits: RwSignal<HashMap<String, FileDiff>>,
    pub pending_diff: Memo<Option<FileDiff>>,
    pub persisted_edits: RwSignal<HashMap<String, PersistedEdit>>,
    pub resolving_edits: RwSignal<HashSet<(i64, String)>>,
    pub pending_generation: RwSignal<HashMap<i64, u64>>,
    pub pending_epoch: RwSignal<u64>,
    pub agent_writes: RwSignal<HashMap<(i64, String), u64>>,
    pub counted_agent_writes: RwSignal<HashSet<(i64, String)>>,
    pub media_url: RwSignal<Option<String>>,
    pub snapshots: RwSignal<HashMap<i64, WorkspaceSnapshot>>,
    pub editor_tabs: RwSignal<HashMap<i64, Vec<String>>>,
    pub editor_buffers: RwSignal<HashMap<(i64, String), EditorBuffer>>,
    pub editor_loading: RwSignal<bool>,
    /// Recovered files cannot overwrite host text before their baseline is checked.
    pub editor_recovery_checks: RwSignal<HashMap<(i64, String), RecoveredFileIssue>>,
    pub editor_recovered: RwSignal<HashSet<(i64, String)>>,
    /// A one-use disk version explicitly approved in the recovered-file review.
    pub editor_recovery_overwrites: RwSignal<HashMap<(i64, String), RecoveryOverwrite>>,
    pub editor_read_revision: RwSignal<u64>,
    pub editor_worker_active: RwSignal<bool>,
    pub editor_fallback_active: RwSignal<bool>,
    pub editor_fallback_paint: RwSignal<Option<EditorFallbackPaint>>,
    pub editor_preparation: RwSignal<Option<PreparedEditorSyntax>>,
    pub(crate) editor_syntax_scope: RwSignal<Option<EditorSyntaxScope>>,
    pub editor_preparation_revision: RwSignal<u64>,
    pub editor_documents: RwSignal<HashMap<(i64, String), openwebide_core::editor::Document>>,
    // Browser parser allocation is thread-local; the wrapper enforces owner-thread access.
    pub editor_syntax: RwSignal<
        send_wrapper::SendWrapper<openwebide_core::editor::SyntaxPreparations<(i64, String)>>,
    >,
    pub editor_scroll: RwSignal<HashMap<(i64, String), EditorScroll>>,
    pub editor_native_binding: RwSignal<Option<EditorNativeContext>>,
    pub editor_native_generation: StoredValue<u64>,
    pub editor_composition: RwSignal<Option<EditorComposition>>,
    pub editor_text_insertion: RwSignal<Option<EditorTextInsertion>>,
    pub editor_rules: RwSignal<HashMap<(i64, String), openwebide_core::editor::EditorRules>>,
    pub editor_indentation: RwSignal<HashMap<(i64, String), openwebide_core::editor::Indentation>>,
    pub editor_fold_revision: RwSignal<u64>,
    pub editor_layout_epoch: RwSignal<u64>,
    pub editor_font_epoch: RwSignal<u64>,
    pub editor_source_revision: Memo<u64>,
    pub editor_projection_revision: Memo<u64>,
    pub editor_view_revision: Memo<u64>,
    pub editor_rows: RwSignal<Option<EditorRowMeasurements>>,
    pub editor_row_cache: RwSignal<Option<EditorRowCache>>,
    pub editor_paragraph_cache: RwSignal<Option<EditorParagraphCache>>,
    pub editor_paint_runs: RwSignal<Option<EditorPaintRuns>>,
    pub editor_row_preparation: RwSignal<Option<EditorRowPreparation>>,
    pub editor_row_ticket: RwSignal<u64>,
    pub editor_group: RwSignal<u64>,
    pub editor_motion: RwSignal<Option<super::editor_motion::PendingEditorMotion>>,
    pub editor_motion_ticket: RwSignal<u64>,
    pub editor_command_revision: RwSignal<u64>,
    pub editor_configuration_revision: RwSignal<u64>,
}

impl WorkspaceState {
    pub fn new() -> Self {
        Self::with_active_project(RwSignal::new(None))
    }

    pub fn with_active_project(active_project: RwSignal<Option<i64>>) -> Self {
        let open_file = RwSignal::new(None);
        let pending_edits = RwSignal::new(HashMap::new());
        let pending_diff = Memo::new(move |_| {
            let open_file = open_file.get()?;
            pending_edits.with(|pending| pending.get(&open_file).cloned())
        });
        let content = RwSignal::new(EditorText::default());
        let pending_epoch = RwSignal::new(0);
        let editor_read_revision = RwSignal::new(0);
        let editor_fold_revision = RwSignal::new(0);
        let editor_layout_epoch = RwSignal::new(0);
        let editor_font_epoch = RwSignal::new(0);
        let source_counter = StoredValue::new(0_u64);
        let editor_source_revision = Memo::new(move |_| {
            content.track();
            open_file.track();
            active_project.track();
            pending_epoch.track();
            editor_read_revision.track();
            source_counter.update_value(|value| *value = value.wrapping_add(1));
            source_counter.get_value()
        });
        let view_counter = StoredValue::new(0_u64);
        let editor_projection_revision = Memo::new(move |_| {
            let _ = editor_source_revision.get();
            editor_fold_revision.track();
            view_counter.update_value(|value| *value = value.wrapping_add(1));
            view_counter.get_value()
        });
        let layout_counter = StoredValue::new(0_u64);
        let editor_view_revision = Memo::new(move |_| {
            let _ = editor_projection_revision.get();
            editor_layout_epoch.track();
            editor_font_epoch.track();
            layout_counter.update_value(|value| *value = value.wrapping_add(1));
            layout_counter.get_value()
        });
        Self {
            active_project,
            entries: RwSignal::new(HashMap::new()),
            expanded: RwSignal::new(HashSet::new()),
            open_file,
            content,
            dirty: RwSignal::new(false),
            search: RwSignal::new(None),
            active_session: RwSignal::new(None),
            pending_edits,
            pending_diff,
            persisted_edits: RwSignal::new(HashMap::new()),
            resolving_edits: RwSignal::new(HashSet::new()),
            pending_generation: RwSignal::new(HashMap::new()),
            pending_epoch,
            agent_writes: RwSignal::new(HashMap::new()),
            counted_agent_writes: RwSignal::new(HashSet::new()),
            media_url: RwSignal::new(None),
            snapshots: RwSignal::new(HashMap::new()),
            editor_tabs: RwSignal::new(HashMap::new()),
            editor_buffers: RwSignal::new(HashMap::new()),
            editor_loading: RwSignal::new(false),
            editor_recovery_checks: RwSignal::new(HashMap::new()),
            editor_recovered: RwSignal::new(HashSet::new()),
            editor_recovery_overwrites: RwSignal::new(HashMap::new()),
            editor_read_revision,
            editor_documents: RwSignal::new(HashMap::new()),
            editor_worker_active: RwSignal::new(false),
            editor_fallback_active: RwSignal::new(false),
            editor_fallback_paint: RwSignal::new(None),
            editor_preparation: RwSignal::new(None),
            editor_syntax_scope: RwSignal::new(None),
            editor_preparation_revision: RwSignal::new(0),
            editor_syntax: RwSignal::new(send_wrapper::SendWrapper::new(Default::default())),
            editor_scroll: RwSignal::new(HashMap::new()),
            editor_native_binding: RwSignal::new(None),
            editor_native_generation: StoredValue::new(0),
            editor_composition: RwSignal::new(None),
            editor_text_insertion: RwSignal::new(None),
            editor_rules: RwSignal::new(HashMap::new()),
            editor_indentation: RwSignal::new(HashMap::new()),
            editor_fold_revision,
            editor_layout_epoch,
            editor_font_epoch,
            editor_source_revision,
            editor_projection_revision,
            editor_view_revision,
            editor_rows: RwSignal::new(None),
            editor_row_cache: RwSignal::new(None),
            editor_paragraph_cache: RwSignal::new(None),
            editor_paint_runs: RwSignal::new(None),
            editor_row_preparation: RwSignal::new(None),
            editor_row_ticket: RwSignal::new(0),
            editor_group: RwSignal::new(0),
            editor_motion: RwSignal::new(None),
            editor_motion_ticket: RwSignal::new(0),
            editor_command_revision: RwSignal::new(0),
            editor_configuration_revision: RwSignal::new(0),
        }
    }

    pub fn active_snapshot(&self) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            entries: self.entries.get_untracked(),
            expanded: self.expanded.get_untracked(),
            open_file: self.open_file.get_untracked(),
            content: self.content.get_untracked(),
            dirty: self.dirty.get_untracked(),
            search: self.search.get_untracked(),
            active_session: self.active_session.get_untracked(),
            pending_edits: self.pending_edits.get_untracked(),
            persisted_edits: self.persisted_edits.get_untracked(),
            media_url: self.media_url.get_untracked(),
        }
    }

    pub fn save_active(&self, project_id: i64) {
        let snapshot = self.active_snapshot();
        self.snapshots.update(|snapshots| {
            let _ = snapshots.insert(project_id, snapshot);
        });
    }

    pub fn restore_project(&self, project_id: i64) {
        let snapshot = self
            .snapshots
            .with_untracked(|snapshots| snapshots.get(&project_id).cloned())
            .unwrap_or_default();
        self.active_project.set(Some(project_id));
        self.apply_snapshot(snapshot);
    }

    /// Save the current workspace, then restore the target project's snapshot.
    pub fn switch_project(&self, current: Option<i64>, target: i64) {
        if current == Some(target) {
            return;
        }
        if let Some(current) = current {
            self.save_active(current);
        }
        self.restore_project(target);
    }

    pub fn clear_active(&self) {
        self.active_project.set(None);
        self.apply_snapshot(WorkspaceSnapshot::default());
    }

    /// Collect a coherent recovery candidate without publishing parser allocations
    /// or transient IME previews. Dirty text must have its real saved baseline.
    pub fn editor_recovery(
        &self,
        project: &openwebide_core::Project,
        active_read_only: bool,
    ) -> Result<openwebide_core::editor::EditorRecovery, String> {
        use openwebide_core::editor::{
            Document, EditorRecovery, EditorRecoveryFile, EditorRecoveryRoot, RecoveryScroll,
        };
        let active = self.active_project.get_untracked() == Some(project.id);
        let snapshot = (!active)
            .then(|| {
                self.snapshots.with_untracked(|snapshots| {
                    snapshots.get(&project.id).map(|snapshot| {
                        (
                            snapshot.open_file.clone(),
                            snapshot.content.clone(),
                            snapshot.dirty,
                        )
                    })
                })
            })
            .flatten();
        let selected = if active {
            self.open_file.get_untracked()
        } else {
            snapshot.as_ref().and_then(|(path, _, _)| path.clone())
        };
        let mut paths = self
            .editor_tabs
            .with_untracked(|tabs| tabs.get(&project.id).cloned())
            .unwrap_or_default();
        if let Some(path) = &selected
            && !paths.contains(path)
        {
            paths.push(path.clone());
        }
        if paths.len() > openwebide_core::editor::MAX_RECOVERY_FILES {
            return Err("Too many open files for editor recovery".into());
        }
        let mut bytes = 0_usize;
        let mut files = Vec::with_capacity(paths.len());
        for path in paths {
            let key = (project.id, path.clone());
            let buffer_read_only = self
                .editor_buffers
                .with_untracked(|buffers| buffers.get(&key).is_some_and(|buffer| buffer.read_only));
            let current = active && selected.as_deref() == Some(&path);
            let loading = current && self.editor_loading.get_untracked();
            let source = if current && !loading {
                Some((self.content.get_untracked(), self.dirty.get_untracked()))
            } else if let Some((open, content, dirty)) = &snapshot
                && open.as_deref() == Some(&path)
            {
                Some((content.clone(), *dirty))
            } else {
                self.editor_buffers.with_untracked(|buffers| {
                    buffers
                        .get(&key)
                        .map(|buffer| (buffer.content.clone(), buffer.dirty))
                })
            };
            let document = self.editor_documents.with_untracked(|documents| {
                if let Some((text, dirty)) = &source {
                    if let Some(document) = documents
                        .get(&key)
                        .filter(|document| document.matches_text(text))
                    {
                        return Ok(Some(document.recovery()));
                    }
                    if *dirty {
                        return Err(format!(
                            "The saved baseline for `{path}` is not ready for recovery"
                        ));
                    }
                    if text.editor_limit().is_some() {
                        return Ok(None);
                    }
                    return Ok(Some(
                        openwebide_core::editor::DocumentRecovery::clean_source(text.shared()),
                    ));
                }
                Ok(documents.get(&key).map(Document::recovery))
            })?;
            let scroll = self
                .editor_scroll
                .with_untracked(|positions| positions.get(&key).copied())
                .unwrap_or_default();
            let file = EditorRecoveryFile {
                path,
                document,
                scroll: RecoveryScroll {
                    top: scroll.top,
                    left: scroll.left,
                },
                read_only: if current {
                    active_read_only
                } else {
                    buffer_read_only
                },
            };
            bytes = bytes.saturating_add(file.recovery_bytes());
            if bytes > openwebide_core::editor::MAX_RECOVERY_BYTES {
                return Err(
                    "Editor recovery exceeds 128 MiB; keep these files open while saving them"
                        .into(),
                );
            }
            files.push(file);
        }
        let state = EditorRecovery {
            format: 1,
            root: Some(EditorRecoveryRoot::for_project(project)),
            selected,
            files,
        };
        state.validate()?;
        Ok(state)
    }

    pub fn editor_recovery_guard(
        &self,
        project: &openwebide_core::Project,
        active_read_only: bool,
    ) -> Result<EditorRecoveryGuard, String> {
        Ok(EditorRecoveryGuard {
            project: project.id,
            state: self.editor_recovery(project, active_read_only)?,
            read_revision: self.editor_read_revision.get_untracked(),
            epoch: self.pending_epoch.get_untracked(),
            documents: self.editor_documents.with_untracked(|documents| {
                documents
                    .iter()
                    .filter(|((id, _), _)| *id == project.id)
                    .map(|((_, path), document)| (path.clone(), document.revision()))
                    .collect()
            }),
        })
    }

    pub fn editor_recovery_guard_matches(
        &self,
        project: &openwebide_core::Project,
        guard: &EditorRecoveryGuard,
        active_read_only: bool,
    ) -> bool {
        if self.editor_composition.with_untracked(|composition| {
            composition
                .as_ref()
                .is_some_and(|composition| composition.key.0 == project.id)
        }) {
            return false;
        }
        self.editor_recovery_guard(project, active_read_only)
            .is_ok_and(|current| {
                current.project == guard.project
                    && current.epoch == guard.epoch
                    && current.read_revision == guard.read_revision
                    && current.documents == guard.documents
                    && current.state == guard.state
            })
    }

    /// Install a validated project recovery atomically. The caller guards account
    /// and root/handle identity around I/O, then verifies disk baselines before save.
    /// None means newer editor activity won; no partial state is published.
    pub fn restore_editor_recovery(
        &self,
        project: &openwebide_core::Project,
        guard: &EditorRecoveryGuard,
        recovery: &openwebide_core::editor::EditorRecovery,
        active_read_only: bool,
    ) -> Result<Option<EditorRecoveryHydration>, String> {
        if !self.can_restore_editor_recovery(project, guard, recovery, active_read_only)? {
            return Ok(None);
        }
        let prepared = PreparedEditorRecovery::new(project.id, recovery)?;
        Ok(Some(
            self.install_editor_recovery(project, recovery, prepared),
        ))
    }

    pub(crate) fn can_restore_editor_recovery(
        &self,
        project: &openwebide_core::Project,
        guard: &EditorRecoveryGuard,
        recovery: &openwebide_core::editor::EditorRecovery,
        active_read_only: bool,
    ) -> Result<bool, String> {
        recovery.validate()?;
        if recovery.root.as_ref().is_some_and(|root| {
            *root != openwebide_core::editor::EditorRecoveryRoot::for_project(project)
        }) {
            return Err("The recovered files belong to a different project folder".into());
        }
        Ok(self.editor_recovery_guard_matches(project, guard, active_read_only))
    }

    pub(crate) fn install_editor_recovery(
        &self,
        project: &openwebide_core::Project,
        recovery: &openwebide_core::editor::EditorRecovery,
        prepared: PreparedEditorRecovery,
    ) -> EditorRecoveryHydration {
        let PreparedEditorRecovery {
            documents,
            buffers,
            scroll,
            paths,
            content,
            dirty,
            mut result,
        } = prepared;
        result.revoke_urls = self.snapshots.with_untracked(|snapshots| {
            snapshots
                .get(&project.id)
                .and_then(|snapshot| snapshot.media_url.clone())
                .into_iter()
                .collect()
        });
        if self.active_project.get_untracked() == Some(project.id)
            && let Some(url) = self.media_url.get_untracked()
            && !result.revoke_urls.contains(&url)
        {
            result.revoke_urls.push(url);
        }
        batch(|| {
            self.editor_tabs.update(|tabs| {
                tabs.insert(project.id, paths);
            });
            self.editor_documents.update(|values| {
                values.retain(|(id, _), _| *id != project.id);
                values.extend(documents);
            });
            self.editor_buffers.update(|values| {
                values.retain(|(id, _), _| *id != project.id);
                values.extend(buffers);
            });
            self.editor_scroll.update(|values| {
                values.retain(|(id, _), _| *id != project.id);
                values.extend(scroll);
            });
            self.editor_syntax.update(|values| {
                values.retain(|(id, _)| *id != project.id);
            });
            self.editor_rules.update(|values| {
                values.retain(|(id, _), _| *id != project.id);
            });
            self.editor_indentation.update(|values| {
                values.retain(|(id, _), _| *id != project.id);
            });
            self.snapshots.update(|snapshots| {
                let snapshot = snapshots.entry(project.id).or_default();
                snapshot.open_file.clone_from(&recovery.selected);
                snapshot.content.clone_from(&content);
                snapshot.dirty = dirty;
                snapshot.media_url = None;
            });
            if self.active_project.get_untracked() == Some(project.id) {
                self.begin_editor_read();
                self.editor_loading.set(result.needs_read);
                self.media_url.set(None);
                self.open_file.set(recovery.selected.clone());
                self.content.set(content);
                self.dirty.set(dirty);
            }
            self.editor_fold_revision
                .update(|revision| *revision = revision.wrapping_add(1));
        });
        result
    }

    pub fn register_editor_tab(&self, project: i64, path: String) {
        self.editor_tabs.update(|tabs| {
            let paths = tabs.entry(project).or_default();
            if !paths.contains(&path) {
                paths.push(path);
            }
        });
    }

    /// Remove a document and choose an adjacent tab without losing other buffers.
    pub fn remove_editor_tab(&self, project: i64, path: &str) -> Option<String> {
        let mut next = None;
        self.editor_tabs.update(|tabs| {
            if let Some(paths) = tabs.get_mut(&project)
                && let Some(index) = paths.iter().position(|item| item == path)
            {
                paths.remove(index);
                next = paths
                    .get(index)
                    .or_else(|| index.checked_sub(1).and_then(|index| paths.get(index)))
                    .cloned();
            }
        });
        let key = (project, path.to_string());
        self.editor_recovery_overwrites.update(|permits| {
            permits.remove(&key);
        });
        self.editor_recovery_checks.update(|checks| {
            checks.remove(&key);
        });
        self.editor_recovered.update(|files| {
            files.remove(&key);
        });
        self.editor_buffers.update(|buffers| {
            buffers.remove(&key);
        });
        self.editor_documents.update(|documents| {
            documents.remove(&key);
        });
        self.editor_syntax.update(|syntax| {
            syntax.remove(&key);
        });
        self.editor_scroll.update(|positions| {
            positions.remove(&key);
        });
        self.editor_rules.update(|rules| {
            rules.remove(&key);
        });
        self.editor_indentation.update(|values| {
            values.remove(&key);
        });
        next
    }

    /// Retain text separately from Document's history, selections and folds.
    pub fn retain_editor_buffer(&self, read_only: bool) {
        if self.editor_loading.get_untracked() && !self.dirty.get_untracked() {
            return;
        }
        if let Some(key) = self
            .active_project
            .get_untracked()
            .zip(self.open_file.get_untracked())
        {
            let buffer = EditorBuffer {
                content: self.content.get_untracked(),
                dirty: self.dirty.get_untracked(),
                read_only,
            };
            self.editor_buffers.update(|buffers| {
                buffers.insert(key, buffer);
            });
        }
    }

    /// Every open has its own revision, including reopening the same path.
    pub fn begin_editor_read(&self) -> u64 {
        self.editor_read_revision
            .update(|revision| *revision = revision.wrapping_add(1));
        self.editor_read_revision.get_untracked()
    }

    pub fn editor_read_current(&self, revision: u64, project: i64, path: &str) -> bool {
        self.editor_read_revision.try_get_untracked() == Some(revision)
            && self.active_project.try_get_untracked() == Some(Some(project))
            && self
                .open_file
                .try_with_untracked(|value| value.as_deref() == Some(path))
                == Some(true)
    }

    pub fn reset(&self) {
        self.editor_font_epoch
            .update(|epoch| *epoch = epoch.wrapping_add(1));
        self.editor_rows.set(None);
        self.editor_row_cache.set(None);
        self.editor_paragraph_cache.set(None);
        self.editor_paint_runs.set(None);
        self.editor_row_preparation.set(None);
        self.editor_row_ticket
            .update(|ticket| *ticket = ticket.wrapping_add(1));
        self.editor_layout_epoch
            .update(|epoch| *epoch = epoch.wrapping_add(1));
        self.editor_tabs.set(HashMap::new());
        self.editor_buffers.set(HashMap::new());
        self.editor_loading.set(false);
        self.editor_recovery_checks.set(HashMap::new());
        self.editor_recovered.set(HashSet::new());
        self.editor_recovery_overwrites.set(HashMap::new());
        self.begin_editor_read();
        self.editor_documents.set(HashMap::new());
        self.editor_native_binding.set(None);
        self.editor_native_generation
            .update_value(|generation| *generation = generation.wrapping_add(1));
        self.editor_composition.set(None);
        self.editor_text_insertion.set(None);
        self.editor_preparation.set(None);
        self.editor_syntax_scope.set(None);
        self.editor_fallback_paint.set(None);
        self.editor_preparation_revision
            .update(|value| *value = value.wrapping_add(1));
        self.editor_syntax
            .set(send_wrapper::SendWrapper::new(Default::default()));
        self.editor_scroll.set(HashMap::new());
        self.editor_rules.set(HashMap::new());
        self.editor_indentation.set(HashMap::new());
        self.editor_fold_revision
            .update(|value| *value = value.wrapping_add(1));
        self.editor_group.set(0);
        self.editor_motion.set(None);
        self.editor_motion_ticket
            .update(|ticket| *ticket = ticket.wrapping_add(1));
        self.editor_command_revision
            .update(|revision| *revision = revision.wrapping_add(1));
        self.editor_configuration_revision
            .update(|value| *value += 1);
        self.pending_epoch.update(|epoch| *epoch += 1);
        self.pending_generation.set(HashMap::new());
        self.agent_writes.set(HashMap::new());
        self.counted_agent_writes.set(HashSet::new());
        self.resolving_edits.set(HashSet::new());
        self.clear_active();
        self.snapshots.set(HashMap::new());
    }

    /// Merge a new agent edit into the active workspace or its saved snapshot.
    pub fn merge_pending(&self, project_id: i64, diff: FileDiff) {
        if self.active_project.get_untracked() == Some(project_id) {
            self.pending_edits
                .update(|pending| crate::pending::merge_pending(pending, diff));
        } else {
            self.snapshots.update(|snapshots| {
                let snapshot = snapshots.entry(project_id).or_default();
                crate::pending::merge_pending(&mut snapshot.pending_edits, diff);
            });
        }
    }

    pub fn begin_pending_refresh(&self, project_id: i64) -> (u64, u64) {
        let mut generation = 0;
        self.pending_generation.update(|generations| {
            let current = generations.entry(project_id).or_default();
            *current += 1;
            generation = *current;
        });
        (self.pending_epoch.get_untracked(), generation)
    }

    pub fn pending_refresh_current(&self, project_id: i64, token: (u64, u64)) -> bool {
        self.pending_epoch.try_get_untracked() == Some(token.0)
            && self.pending_generation.try_with_untracked(|generations| {
                generations.get(&project_id).copied() == Some(token.1)
            }) == Some(true)
    }

    pub fn set_persisted_edits(&self, project_id: i64, edits: Vec<PersistedEdit>) {
        let records: HashMap<_, _> = edits
            .into_iter()
            .filter(|edit| edit.project_id == project_id && edit.decision == EditDecision::Pending)
            .map(|edit| (edit.path.clone(), edit))
            .collect();
        let diffs = records
            .iter()
            .map(|(path, edit)| (path.clone(), edit.diff.clone()))
            .collect();
        if self.active_project.get_untracked() == Some(project_id) {
            self.persisted_edits.set(records);
            self.pending_edits.set(diffs);
        } else {
            self.snapshots.update(|snapshots| {
                let snapshot = snapshots.entry(project_id).or_default();
                snapshot.persisted_edits = records;
                snapshot.pending_edits = diffs;
            });
        }
    }

    pub fn clear_resolved_edit(&self, edit: &PersistedEdit) {
        self.begin_pending_refresh(edit.project_id);
        if self.active_project.get_untracked() == Some(edit.project_id) {
            if self.persisted_edits.with_untracked(|edits| {
                edits
                    .get(&edit.path)
                    .is_some_and(|current| current.revision == edit.revision)
            }) {
                self.persisted_edits.update(|edits| {
                    edits.remove(&edit.path);
                });
                self.pending_edits.update(|edits| {
                    edits.remove(&edit.path);
                });
            }
        } else {
            self.snapshots.update(|snapshots| {
                if let Some(snapshot) = snapshots.get_mut(&edit.project_id)
                    && snapshot
                        .persisted_edits
                        .get(&edit.path)
                        .is_some_and(|current| current.revision == edit.revision)
                {
                    snapshot.persisted_edits.remove(&edit.path);
                    snapshot.pending_edits.remove(&edit.path);
                }
            });
        }
    }

    pub fn is_resolving(&self) -> bool {
        let Some(project_id) = self.active_project.get() else {
            return false;
        };
        let Some(path) = self.open_file.get() else {
            return false;
        };
        self.resolving_edits
            .with(|edits| edits.contains(&(project_id, path)))
    }

    fn apply_snapshot(&self, snapshot: WorkspaceSnapshot) {
        self.begin_editor_read();
        self.editor_loading.set(false);
        self.entries.set(snapshot.entries);
        self.expanded.set(snapshot.expanded);
        self.open_file.set(snapshot.open_file);
        self.content.set(snapshot.content);
        self.dirty.set(snapshot.dirty);
        self.search.set(snapshot.search);
        self.active_session.set(snapshot.active_session);
        self.pending_edits.set(snapshot.pending_edits);
        self.persisted_edits.set(snapshot.persisted_edits);
        self.media_url.set(snapshot.media_url);
    }
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_recovery_capture_without_documents_preserves_both_project_modes() {
        use openwebide_core::{Project, WorkspaceMode, editor::DocumentRecovery};
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let owner = Owner::new();
            owner.with(|| {
                let workspace = WorkspaceState::with_active_project(RwSignal::new(Some(1)));
                let project = Project {
                    id: 1,
                    user_id: None,
                    created_at: 1,
                    name: "project".into(),
                    path: Some("project".into()),
                    mode,
                };
                workspace.open_file.set(Some("clean.txt".into()));
                workspace.content.set("文😀\r\nsecond".into());
                let recovery = workspace.editor_recovery(&project, false).unwrap();
                assert_eq!(recovery.files.len(), 1);
                assert_eq!(
                    recovery.files[0].document,
                    Some(DocumentRecovery::clean("文😀\r\nsecond"))
                );
                assert!(workspace.editor_documents.get_untracked().is_empty());
                workspace.dirty.set(true);
                assert!(workspace.editor_recovery(&project, false).is_err());
            });
        }
    }

    #[test]
    fn admission_cache_is_shared_only_by_the_immutable_source() {
        use openwebide_core::editor::{Document, Edit, Selection};
        let mut document = Document::new("one\r\ntwo");
        let source = EditorText::from(document.shared_text());
        let equal_source = EditorText::from(source.as_str());
        let clone = source.clone();
        assert!(source.1.get().is_none());
        assert!(!equal_source.record_admission(document.admission()));
        assert!(equal_source.1.get().is_none());
        assert!(source.record_admission(document.admission()));
        assert_eq!(clone.1.get(), Some(&None));
        assert_eq!(source, equal_source);
        document
            .apply(
                vec![Edit::replace(0..3, "changed")],
                vec![Selection::caret(0)],
                None,
            )
            .unwrap();
        let edited = EditorText::from(document.shared_text());
        assert!(edited.1.get().is_none());
        assert!(!source.record_admission(document.admission()));
        assert!(edited.record_admission(document.admission()));
        assert_eq!(String::from(clone), "one\r\ntwo");
        assert_eq!(edited.editor_limit(), None);
    }

    #[test]
    fn admission_cache_retains_completed_limit_decisions() {
        use openwebide_core::editor::{EditorAdmission, EditorLimit, MAX_EDITOR_LINE_BYTES};
        let source = EditorText::from("x".repeat(MAX_EDITOR_LINE_BYTES + 1));
        let mut preparation = EditorAdmission::new(source.as_str());
        while !preparation.advance(1) {}
        assert!(source.record_admission(preparation.finish_with_source().unwrap()));
        assert_eq!(source.editor_limit(), Some(EditorLimit::LineBytes));
        assert_eq!(source.clone().1.get(), Some(&Some(EditorLimit::LineBytes)));
    }

    fn diff(old: &str, new: &str) -> FileDiff {
        FileDiff {
            path: "main.rs".into(),
            old: Some(old.into()),
            new: new.into(),
            old_unavailable: false,
            backup_path: None,
        }
    }

    #[test]
    fn measured_row_scope_changes_with_source_project_layout_and_account() {
        use openwebide_core::editor::MeasuredRows;
        let owner = Owner::new();
        owner.with(|| {
            let workspace = WorkspaceState::with_active_project(RwSignal::new(Some(1)));
            workspace.open_file.set(Some("same.rs".into()));
            workspace.content.set("one\ntwo".into());
            let revision = workspace.editor_view_revision.get();
            workspace.editor_rows.set(Some(EditorRowMeasurements {
                whitespace: false,
                syntax: None,
                revision,
                account_generation: 0,
                metrics: "font".into(),
                rows: MeasuredRows::new([19.5, 39.0]).unwrap(),
            }));
            workspace.content.set("one\nchanged".into());
            assert_ne!(workspace.editor_view_revision.get(), revision);
            let revision = workspace.editor_view_revision.get();
            workspace.active_project.set(Some(2));
            assert_ne!(workspace.editor_view_revision.get(), revision);
            let revision = workspace.editor_view_revision.get();
            workspace
                .editor_layout_epoch
                .update(|epoch| *epoch = epoch.wrapping_add(1));
            assert_ne!(workspace.editor_view_revision.get(), revision);
            let revision = workspace.editor_view_revision.get();
            workspace.reset();
            assert_ne!(workspace.editor_view_revision.get(), revision);
            assert!(workspace.editor_rows.get_untracked().is_none());
        });
    }

    #[test]
    fn recovery_hydration_is_atomic_scoped_and_preserves_the_saved_baseline() {
        use openwebide_core::{
            Project, WorkspaceMode,
            editor::{
                Document, EditorRecovery, EditorRecoveryFile, EditorRecoveryRoot, RecoveryScroll,
                Selection,
            },
        };
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let owner = Owner::new();
            owner.with(|| {
                let workspace = WorkspaceState::new();
                let project = Project {
                    id: 1,
                    user_id: None,
                    created_at: 1,
                    name: "project".into(),
                    path: Some("project".into()),
                    mode,
                };
                workspace.restore_project(1);
                workspace.active_session.set(Some(9));
                let mut document = Document::new("base\r\n");
                document.replace_selections("文", None).unwrap();
                document.set_selections(vec![Selection::caret(3)]).unwrap();
                let recovery = EditorRecovery {
                    format: 1,
                    root: Some(EditorRecoveryRoot::for_project(&project)),
                    selected: Some("a.rs".into()),
                    files: vec![
                        EditorRecoveryFile {
                            path: "a.rs".into(),
                            document: Some(document.recovery()),
                            scroll: RecoveryScroll {
                                top: 75.0,
                                left: 9.0,
                            },
                            read_only: false,
                        },
                        EditorRecoveryFile {
                            path: "b.txt".into(),
                            document: Some(Document::new("kept").recovery()),
                            scroll: RecoveryScroll::default(),
                            read_only: true,
                        },
                    ],
                };
                workspace.editor_buffers.update(|buffers| {
                    buffers.insert(
                        (3, "other".into()),
                        EditorBuffer {
                            content: "other".into(),
                            dirty: true,
                            read_only: false,
                        },
                    );
                });
                let guard = workspace.editor_recovery_guard(&project, false).unwrap();
                let hydrated = workspace
                    .restore_editor_recovery(&project, &guard, &recovery, false)
                    .unwrap()
                    .unwrap();
                assert_eq!(hydrated.selected.as_deref(), Some("a.rs"));
                assert!(!hydrated.needs_read);
                assert!(!hydrated.read_only);
                assert_eq!(workspace.content.get_untracked(), "文base\r\n");
                assert!(workspace.dirty.get_untracked());
                assert_eq!(workspace.active_session.get_untracked(), Some(9));
                assert!(
                    workspace
                        .editor_buffers
                        .with_untracked(|buffers| buffers.contains_key(&(3, "other".into())))
                );
                assert_eq!(
                    workspace.editor_recovery(&project, false).unwrap(),
                    recovery
                );
                workspace.editor_documents.with_untracked(|documents| {
                    let mut document = documents[&(1, "a.rs".into())].clone();
                    assert!(document.undo());
                    assert_eq!(document.text(), "base\r\n");
                });
                // Capture another load while the project is inactive. Its result
                // must update only that project's snapshot and retained buffers.
                workspace.save_active(1);
                workspace.clear_active();
                let guard = workspace.editor_recovery_guard(&project, false).unwrap();
                workspace
                    .restore_editor_recovery(&project, &guard, &recovery, false)
                    .unwrap()
                    .unwrap();
                assert_eq!(workspace.active_project.get_untracked(), None);
                assert_eq!(workspace.content.get_untracked(), "");
                assert_eq!(
                    workspace.editor_recovery(&project, false).unwrap(),
                    recovery
                );
                workspace.restore_project(1);
                assert_eq!(workspace.content.get_untracked(), "文base\r\n");
                let guard = workspace.editor_recovery_guard(&project, false).unwrap();
                workspace.begin_editor_read();
                assert!(
                    workspace
                        .restore_editor_recovery(
                            &project,
                            &guard,
                            &EditorRecovery::default(),
                            false
                        )
                        .unwrap()
                        .is_none()
                );
                assert_eq!(workspace.open_file.get_untracked().as_deref(), Some("a.rs"));
                let guard = workspace.editor_recovery_guard(&project, false).unwrap();
                let mut invalid = recovery.clone();
                invalid.files[0].scroll.top = f64::NAN;
                assert!(
                    workspace
                        .restore_editor_recovery(&project, &guard, &invalid, false)
                        .is_err()
                );
                assert_eq!(
                    workspace.editor_recovery(&project, false).unwrap(),
                    recovery
                );
                workspace
                    .restore_editor_recovery(&project, &guard, &EditorRecovery::default(), false)
                    .unwrap()
                    .unwrap();
                assert!(workspace.open_file.get_untracked().is_none());
                assert!(
                    workspace
                        .editor_buffers
                        .with_untracked(|buffers| buffers.keys().all(|(id, _)| *id != 1))
                );
                assert_eq!(workspace.active_session.get_untracked(), Some(9));
            });
            owner.cleanup();
        }
    }

    #[test]
    fn hydration_rejects_project_reset_and_composition_and_clears_old_media() {
        use openwebide_core::editor::{
            EditorRecovery, EditorRecoveryFile, EditorRecoveryRoot, RecoveryScroll,
        };
        let owner = Owner::new();
        owner.with(|| {
            let workspace = WorkspaceState::new();
            let project = openwebide_core::Project {
                id: 1,
                name: "one".into(),
                mode: openwebide_core::WorkspaceMode::Remote,
                path: Some("one".into()),
                user_id: None,
                created_at: 1,
            };
            let guard = workspace.editor_recovery_guard(&project, false).unwrap();
            let mut other = project.clone();
            other.id = 2;
            assert!(
                workspace
                    .restore_editor_recovery(&other, &guard, &EditorRecovery::default(), false)
                    .unwrap()
                    .is_none()
            );
            workspace.editor_composition.set(Some(EditorComposition {
                key: (1, "one.rs".into()),
                epoch: 0,
                read_revision: 0,
                account_generation: 0,
            }));
            assert!(
                workspace
                    .restore_editor_recovery(&project, &guard, &EditorRecovery::default(), false)
                    .unwrap()
                    .is_none()
            );
            workspace.editor_composition.set(None);
            workspace.reset();
            assert!(
                workspace
                    .restore_editor_recovery(&project, &guard, &EditorRecovery::default(), false)
                    .unwrap()
                    .is_none()
            );
            workspace.restore_project(1);
            workspace.media_url.set(Some("blob:active".into()));
            workspace.snapshots.update(|snapshots| {
                snapshots.entry(1).or_default().media_url = Some("blob:hidden".into());
            });
            let guard = workspace.editor_recovery_guard(&project, false).unwrap();
            let recovery = EditorRecovery {
                format: 1,
                root: Some(EditorRecoveryRoot::for_project(&project)),
                selected: Some("one.pdf".into()),
                files: vec![EditorRecoveryFile {
                    path: "one.pdf".into(),
                    document: None,
                    scroll: RecoveryScroll::default(),
                    read_only: true,
                }],
            };
            let mut wrong = recovery.clone();
            wrong.root.as_mut().unwrap().path = Some("other".into());
            assert!(
                workspace
                    .restore_editor_recovery(&project, &guard, &wrong, false)
                    .is_err()
            );
            let result = workspace
                .restore_editor_recovery(&project, &guard, &recovery, false)
                .unwrap()
                .unwrap();
            assert!(result.needs_read);
            assert!(result.read_only);
            assert_eq!(result.revoke_urls, vec!["blob:hidden", "blob:active"]);
            assert!(workspace.editor_loading.get_untracked());
            assert!(workspace.media_url.get_untracked().is_none());
            assert!(
                workspace
                    .snapshots
                    .with_untracked(|snapshots| snapshots[&1].media_url.is_none())
            );
        });
        owner.cleanup();
    }

    #[test]
    fn switching_projects_saves_and_restores_workspace() {
        Owner::new().with(|| {
            let active_project = RwSignal::new(Some(1));
            let workspace = WorkspaceState::with_active_project(active_project);
            workspace.open_file.set(Some("one.rs".into()));
            workspace.content.set("project one".into());
            workspace.dirty.set(true);

            workspace.switch_project(Some(1), 2);
            assert_eq!(active_project.get_untracked(), Some(2));
            assert!(workspace.open_file.get_untracked().is_none());
            workspace.open_file.set(Some("two.rs".into()));
            workspace.content.set("project two".into());

            workspace.switch_project(Some(2), 1);
            assert_eq!(active_project.get_untracked(), Some(1));
            assert_eq!(
                workspace.open_file.get_untracked().as_deref(),
                Some("one.rs")
            );
            assert_eq!(workspace.content.get_untracked(), "project one");
            assert!(workspace.dirty.get_untracked());
        });
    }

    #[test]
    fn file_buffers_and_read_revisions_are_account_and_project_scoped() {
        Owner::new().with(|| {
            let workspace = WorkspaceState::with_active_project(RwSignal::new(Some(1)));
            workspace.open_file.set(Some("one.rs".into()));
            workspace.content.set("draft".into());
            workspace.dirty.set(true);
            workspace.retain_editor_buffer(false);
            let old = workspace.begin_editor_read();
            let latest = workspace.begin_editor_read();
            assert!(!workspace.editor_read_current(old, 1, "one.rs"));
            assert!(workspace.editor_read_current(latest, 1, "one.rs"));
            workspace.switch_project(Some(1), 2);
            assert!(!workspace.editor_read_current(latest, 1, "one.rs"));
            assert_eq!(
                workspace.editor_buffers.get_untracked()[&(1, "one.rs".into())].content,
                "draft"
            );
            workspace.reset();
            assert!(workspace.editor_buffers.get_untracked().is_empty());
            workspace.active_project.set(Some(1));
            workspace.open_file.set(Some("one.rs".into()));
            assert!(!workspace.editor_read_current(latest, 1, "one.rs"));
        });
    }

    #[test]
    fn closing_a_tab_keeps_other_documents_and_project_order() {
        Owner::new().with(|| {
            let workspace = WorkspaceState::new();
            for path in ["one.rs", "two.rs", "three.rs", "two.rs"] {
                workspace.register_editor_tab(1, path.into());
                workspace.editor_documents.update(|documents| {
                    documents.insert(
                        (1, path.into()),
                        openwebide_core::editor::Document::new(path),
                    );
                });
            }
            workspace.register_editor_tab(2, "two.rs".into());
            assert_eq!(workspace.editor_tabs.get_untracked()[&1].len(), 3);
            assert_eq!(
                workspace.remove_editor_tab(1, "two.rs").as_deref(),
                Some("three.rs")
            );
            assert!(
                workspace
                    .editor_documents
                    .get_untracked()
                    .contains_key(&(1, "one.rs".into()))
            );
            assert!(
                !workspace
                    .editor_documents
                    .get_untracked()
                    .contains_key(&(1, "two.rs".into()))
            );
            assert_eq!(workspace.editor_tabs.get_untracked()[&2], vec!["two.rs"]);
            assert_eq!(
                workspace.remove_editor_tab(1, "three.rs").as_deref(),
                Some("one.rs")
            );
            assert!(workspace.remove_editor_tab(1, "one.rs").is_none());
            workspace.reset();
            assert!(workspace.editor_tabs.get_untracked().is_empty());
        });
    }

    #[test]
    fn recovery_collects_inactive_project_with_its_actual_saved_baseline() {
        Owner::new().with(|| {
            let workspace = WorkspaceState::with_active_project(RwSignal::new(Some(1)));
            let project = openwebide_core::Project {
                id: 1,
                name: "one".into(),
                mode: openwebide_core::WorkspaceMode::Local,
                path: None,
                user_id: None,
                created_at: 1,
            };
            let mut document = openwebide_core::editor::Document::new("base");
            document.replace_selections("draft ", None).unwrap();
            workspace.register_editor_tab(1, "one.rs".into());
            workspace.open_file.set(Some("one.rs".into()));
            workspace.content.set(document.text().into());
            workspace.dirty.set(true);
            workspace.editor_documents.update(|documents| {
                documents.insert((1, "one.rs".into()), document);
            });
            let active = workspace.editor_recovery(&project, false).unwrap();
            workspace.switch_project(Some(1), 2);
            workspace.register_editor_tab(2, "other.rs".into());
            workspace.open_file.set(Some("other.rs".into()));
            workspace.content.set("another project".into());
            let inactive = workspace.editor_recovery(&project, false).unwrap();
            assert_eq!(inactive, active);
            assert_eq!(
                inactive.files[0].document.as_ref().unwrap().saved.as_str(),
                "base"
            );
            assert_eq!(
                inactive.files[0].document.as_ref().unwrap().text.as_str(),
                "draft base"
            );
            for index in 0..openwebide_core::editor::MAX_RECOVERY_FILES {
                workspace.register_editor_tab(1, format!("extra_{index}.rs"));
            }
            assert_eq!(
                workspace.editor_recovery(&project, false).unwrap_err(),
                "Too many open files for editor recovery"
            );
        });
    }

    #[test]
    fn merge_pending_preserves_the_first_old_value() {
        Owner::new().with(|| {
            let active_project = RwSignal::new(Some(1));
            let workspace = WorkspaceState::with_active_project(active_project);

            workspace.merge_pending(1, diff("v0", "v1"));
            workspace.merge_pending(1, diff("v1", "v2"));

            let pending = workspace.pending_edits.get_untracked();
            let merged = &pending["main.rs"];
            assert_eq!(merged.old.as_deref(), Some("v0"));
            assert_eq!(merged.new, "v2");
        });
    }
    fn record(project_id: i64, revision: i64) -> PersistedEdit {
        PersistedEdit {
            file: None,
            project_id,
            path: "main.rs".into(),
            revision,
            decision: EditDecision::Pending,
            diff: diff("original", "new"),
        }
    }

    #[test]
    fn persisted_snapshots_isolate_projects_and_old_resolution_cannot_clear_new_revision() {
        Owner::new().with(|| {
            let workspace = WorkspaceState::with_active_project(RwSignal::new(Some(1)));
            workspace.set_persisted_edits(1, vec![record(1, 2)]);
            workspace.set_persisted_edits(2, vec![record(2, 1)]);
            workspace.clear_resolved_edit(&record(1, 1));
            assert_eq!(
                workspace.persisted_edits.get_untracked()["main.rs"].revision,
                2
            );
            workspace.switch_project(Some(1), 2);
            assert_eq!(
                workspace.persisted_edits.get_untracked()["main.rs"].project_id,
                2
            );
            workspace.clear_resolved_edit(&record(1, 2));
            workspace.switch_project(Some(2), 1);
            assert!(workspace.pending_edits.get_untracked().is_empty());
        });
    }

    #[test]
    fn refresh_tokens_reject_older_responses_and_reset() {
        Owner::new().with(|| {
            let workspace = WorkspaceState::new();
            let old = workspace.begin_pending_refresh(1);
            let latest = workspace.begin_pending_refresh(1);
            assert!(!workspace.pending_refresh_current(1, old));
            assert!(workspace.pending_refresh_current(1, latest));
            workspace.reset();
            workspace.begin_pending_refresh(1);
            assert!(!workspace.pending_refresh_current(1, latest));
        });
    }
}
