//! Versioned recovery data: hosts persist it, shared Rust validates and restores it.
use super::{
    Document, Edit, FoldRange, MAX_DOCUMENT_BYTES, MAX_SELECTIONS, Selection, normalize_selections,
};
use serde::{Deserialize, Serialize};

pub const MAX_RECOVERY_FILES: usize = 64;
pub const MAX_RECOVERY_BYTES: usize = 128 * 1024 * 1024;
const RECOVERY_BODY_LIMIT: usize = 192 * 1024 * 1024;
const MAX_RECOVERY_FOLDS: usize = 100_000;

pub const fn recovery_body_limit() -> usize {
    RECOVERY_BODY_LIMIT
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentRecovery {
    #[serde(with = "encoded_text")]
    pub text: std::sync::Arc<String>,
    #[serde(with = "encoded_text")]
    pub saved: std::sync::Arc<String>,
    pub selections: Vec<Selection>,
    pub collapsed: Vec<FoldRange>,
}

impl Document {
    /// Compare committed source and baseline without materializing a persistence
    /// record. Live documents already maintain valid selection/fold metadata.
    pub fn recovery_disk_state(&self, disk: Option<&str>) -> Result<RecoveryDiskState, String> {
        let document = self
            .composition
            .as_ref()
            .map_or(self, |composition| composition.committed_document());
        validate_sources(document.text(), &document.saved)?;
        classify_disk(document.text(), &document.saved, disk)
    }

    /// IME previews are transient; persistence captures the last committed state.
    pub fn recovery(&self) -> DocumentRecovery {
        let document = self
            .composition
            .as_ref()
            .map_or(self, |composition| composition.committed_document());
        DocumentRecovery {
            text: document.shared_text(),
            saved: document.saved.clone(),
            selections: document.selections.clone(),
            collapsed: document
                .folds
                .ranges()
                .iter()
                .filter(|range| document.folds.collapsed_at(range.start_line).is_some())
                .copied()
                .collect(),
        }
    }
}

/// Disk reconciliation never rebases a draft onto changed host text silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryDiskState {
    Current,
    AlreadySaved,
    Reloaded,
    Conflict,
    Missing,
}

impl DocumentRecovery {
    /// Capture a clean source that has no live editor document. Recovery needs
    /// source and baseline, not coordinate indexes or an undo history.
    pub fn clean(source: &str) -> Self {
        Self::clean_source(std::sync::Arc::new(source.to_owned()))
    }

    /// Capture an immutable clean source without copying its bytes.
    pub fn clean_source(source: std::sync::Arc<String>) -> Self {
        Self {
            text: source.clone(),
            saved: source,
            selections: vec![Selection::caret(0)],
            collapsed: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        self.validate_with_lines(|| self.text.bytes().filter(|byte| *byte == b'\n').count() + 1)
    }

    fn validate_with_lines(&self, lines: impl FnOnce() -> usize) -> Result<(), String> {
        if self.text.len() > MAX_DOCUMENT_BYTES || self.saved.len() > MAX_DOCUMENT_BYTES {
            return Err("Recovered document exceeds the editor's size limit".into());
        }
        if self.selections.is_empty() || self.selections.len() > MAX_SELECTIONS {
            return Err("Recovered selections exceed the editor's limits".into());
        }
        super::validate_selections(&self.text, &self.selections)
            .map_err(|error| error.to_string())?;
        if self.collapsed.len() > MAX_RECOVERY_FOLDS {
            return Err("Too many recovered folds".into());
        }
        if !self.collapsed.is_empty() && !super::folds::are_normalized(&self.collapsed, lines()) {
            return Err("Recovered folds are outside the document or overlap".into());
        }
        Ok(())
    }

    /// Compare the fresh disk read with the saved baseline before permitting a
    /// recovered draft to overwrite it. Hosts handle unavailable reads separately.
    pub fn reconcile_disk(
        &self,
        disk: Option<&str>,
    ) -> Result<(Document, RecoveryDiskState), String> {
        let state = self.disk_state(disk)?;
        let mut document = self.restore()?;
        match state {
            RecoveryDiskState::AlreadySaved => document.mark_saved(),
            RecoveryDiskState::Reloaded => {
                document =
                    Document::for_editor(disk.unwrap()).map_err(|error| error.to_string())?;
            }
            _ => {}
        }
        Ok((document, state))
    }

    /// Classify disk changes without rebuilding recovered document indexes.
    /// Save verification and hydration use the same reconciliation decisions.
    pub fn disk_state(&self, disk: Option<&str>) -> Result<RecoveryDiskState, String> {
        self.validate_editor()?;
        classify_disk(&self.text, &self.saved, disk)
    }

    fn validate_editor(&self) -> Result<(), String> {
        self.validate()?;
        validate_sources(&self.text, &self.saved)
    }

    /// The recovered draft is one undoable change against its original disk text.
    pub fn restore(&self) -> Result<Document, String> {
        self.validate_editor()?;
        let saved = Document::from_shared_text(self.saved.clone());
        let draft = if self.text == self.saved {
            saved.clone()
        } else {
            Document::from_shared_text(self.text.clone())
        };
        self.restore_prepared(saved, draft)
    }

    /// Restore from complete source-owned indexes prepared by the host. Validate
    /// admission and metadata before installing the draft as one undoable edit;
    /// no document indexes are rebuilt during this final recovery transition.
    pub fn restore_prepared(
        &self,
        mut document: Document,
        draft: Document,
    ) -> Result<Document, String> {
        let saved_matches = document.matches_text(&self.saved);
        let draft_matches = draft.matches_text(&self.text);
        self.validate_with_lines(|| {
            if draft_matches {
                draft.line_count()
            } else {
                self.text.bytes().filter(|byte| *byte == b'\n').count() + 1
            }
        })?;
        // Complete source-owned indexes prove admission without repeating the
        // preparation scan. Unmatched inputs retain the original validation
        // errors before reporting the source mismatch.
        if saved_matches && draft_matches {
            for prepared in [&document, &draft] {
                if let Some(limit) = prepared.admission().limit() {
                    return Err(super::EditError::Capacity(limit).to_string());
                }
            }
        } else {
            validate_sources(&self.text, &self.saved)?;
        }
        if !saved_matches || !draft_matches {
            return Err("Prepared recovery sources do not match the recovered document".into());
        }
        for prepared in [&document, &draft] {
            if prepared.revision != 0
                || !prepared.history.is_empty()
                || prepared.is_composing()
                || prepared.is_dirty()
            {
                return Err("Recovery preparation must contain fresh document indexes".into());
            }
        }
        document.enforce_editor_limits();
        document.selections = vec![Selection::caret(0)];
        document.selection_history.clear();
        document.motion_columns = None;
        document.folds = Default::default();
        document.projection = Default::default();
        let after = normalize_selections(&self.text, self.selections.clone())
            .map_err(|error| error.to_string())?;
        if self.text != self.saved {
            let prepared = (
                super::HistorySource {
                    text: document.text.clone(),
                    index: document.line_index.clone(),
                },
                super::HistorySource {
                    text: draft.text.clone(),
                    index: draft.line_index.clone(),
                },
            );
            let forward = vec![Edit {
                range: 0..self.saved.len(),
                text: draft.text.clone(),
            }];
            let inverse = vec![Edit {
                range: 0..self.text.len(),
                text: document.text.clone(),
            }];
            document.text = draft.text;
            document.line_index = draft.line_index;
            document.record_transaction(forward, inverse, after, None, Some(prepared));
        } else {
            document.selections = after;
        }
        let lines = document.line_count();
        document
            .fold_state_mut()
            .set_ranges(self.collapsed.clone(), lines);
        document.fold_state_mut().collapse_all();
        Ok(document)
    }
}

fn validate_sources(text: &str, saved: &str) -> Result<(), String> {
    if text.len() > MAX_DOCUMENT_BYTES || saved.len() > MAX_DOCUMENT_BYTES {
        return Err("Recovered document exceeds the editor's size limit".into());
    }
    for source in [saved, text] {
        if let Some(limit) = super::editor_limit(source) {
            return Err(super::EditError::Capacity(limit).to_string());
        }
    }
    Ok(())
}

fn classify_disk(text: &str, saved: &str, disk: Option<&str>) -> Result<RecoveryDiskState, String> {
    match disk {
        None => Ok(RecoveryDiskState::Missing),
        Some(source) if source == saved => Ok(RecoveryDiskState::Current),
        Some(source) if source == text => Ok(RecoveryDiskState::AlreadySaved),
        Some(source) if text == saved => {
            if source.len() > MAX_DOCUMENT_BYTES {
                return Err("Disk document exceeds the editor's size limit".into());
            }
            if let Some(limit) = super::editor_limit(source) {
                return Err(super::EditError::Capacity(limit).to_string());
            }
            Ok(RecoveryDiskState::Reloaded)
        }
        Some(_) => Ok(RecoveryDiskState::Conflict),
    }
}

#[cfg(test)]
mod prepared_tests {
    use super::*;

    #[test]
    fn replacing_recovered_redo_releases_prepared_version_indexes() {
        let saved = Document::for_editor("saved\nbody\n").unwrap();
        let draft = Document::for_editor("draft 文😀\r\nbody\n").unwrap();
        let saved_index = std::sync::Arc::downgrade(&saved.line_index);
        let draft_index = std::sync::Arc::downgrade(&draft.line_index);
        let recovery = DocumentRecovery {
            text: draft.shared_text(),
            saved: saved.shared_text(),
            selections: vec![Selection::caret(draft.text().len())],
            collapsed: Vec::new(),
        };
        let mut document = recovery.restore_prepared(saved, draft).unwrap();
        assert!(document.undo());
        assert!(saved_index.upgrade().is_some());
        assert!(draft_index.upgrade().is_some());
        document.replace_selections("new", None).unwrap();
        assert!(!document.can_redo());
        assert_eq!(document.history.len(), 1);
        assert!(document.history[0].transactions[0].prepared.is_none());
        assert!(saved_index.upgrade().is_none());
        assert!(draft_index.upgrade().is_none());
        assert!(document.undo());
        assert_eq!(document.text(), recovery.saved.as_str());
        assert!(!document.is_dirty());
    }

    #[test]
    fn recovered_history_retains_prepared_sources_through_edits_and_snapshot_undo() {
        let baseline = Document::for_editor("base 😀\r\n".repeat(20_000)).unwrap();
        let draft = Document::for_editor(format!("{}tail 文\r\n", baseline.text())).unwrap();
        let saved_source = baseline.shared_text();
        let draft_source = draft.shared_text();
        let saved_index = baseline.line_index.clone();
        let draft_index = draft.line_index.clone();
        let recovery = DocumentRecovery {
            text: draft.shared_text(),
            saved: baseline.shared_text(),
            selections: vec![Selection::caret(draft.text().len())],
            collapsed: Vec::new(),
        };
        let mut restored = recovery.restore_prepared(baseline, draft).unwrap();
        let transaction = restored.history[0].transactions[0].clone();
        for (edit, expected) in [
            (&transaction.forward[0], &draft_source),
            (&transaction.inverse[0], &saved_source),
        ] {
            let super::super::HistoryText::Shared(source) = &edit.text else {
                panic!("recovery history copied a prepared source");
            };
            assert!(std::sync::Arc::ptr_eq(source, expected));
        }
        assert_eq!(
            restored.history_bytes,
            saved_source.len() + draft_source.len()
        );
        assert!(std::sync::Arc::ptr_eq(
            &restored.shared_text(),
            &draft_source
        ));
        assert!(std::sync::Arc::ptr_eq(&restored.saved, &saved_source));
        let mut snapshot = restored.clone();
        restored.replace_selections("new", None).unwrap();
        assert!(matches!(
            restored.history[1].transactions[0].forward[0].text,
            super::super::HistoryText::Owned(_)
        ));
        assert_eq!(draft_source.as_str(), recovery.text.as_str());
        assert_eq!(saved_source.as_str(), recovery.saved.as_str());
        assert_eq!(snapshot.text(), recovery.text.as_str());
        assert!(restored.undo());
        assert_eq!(restored.text(), recovery.text.as_str());
        for document in [&mut restored, &mut snapshot] {
            assert!(document.undo());
            assert_eq!(document.text(), recovery.saved.as_str());
            assert!(std::sync::Arc::ptr_eq(&document.text, &saved_source));
            assert!(std::sync::Arc::ptr_eq(&document.line_index, &saved_index));
            assert!(!document.is_dirty());
            assert!(document.redo());
            assert_eq!(document.text(), recovery.text.as_str());
            assert!(std::sync::Arc::ptr_eq(&document.text, &draft_source));
            assert!(std::sync::Arc::ptr_eq(&document.line_index, &draft_index));
            assert_eq!(document.selections(), recovery.selections);
            assert!(document.is_dirty());
        }
    }

    #[test]
    fn prepared_recovery_matches_transaction_restore_and_retains_indexes() {
        for (saved, text) in [
            (String::new(), String::new()),
            ("base\r\n".into(), "文😀\r\ntail\n".into()),
            ("same\n".into(), "same\n".into()),
            ("word\n".repeat(2000), "文😀\r\n".repeat(3000)),
            (
                "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl".into(),
                "a\nb\nc\nd\nchanged 文😀\r\ne\nf\ng\nh\ni\nj\nk\nl".into(),
            ),
        ] {
            let selections = vec![Selection {
                anchor: text.len(),
                head: 0,
            }];
            let collapsed = if text.contains('\n') {
                vec![FoldRange {
                    start_line: 0,
                    end_line: text.bytes().filter(|byte| *byte == b'\n').count(),
                }]
            } else {
                vec![]
            };
            let recovery = DocumentRecovery {
                text: text.into(),
                saved: saved.into(),
                selections,
                collapsed,
            };
            let baseline = Document::for_editor(recovery.saved.as_str()).unwrap();
            let draft = Document::for_editor(recovery.text.as_str()).unwrap();
            let index = draft.line_index.clone();
            let mut expected = baseline.clone();
            if recovery.text != recovery.saved {
                expected
                    .apply(
                        vec![Edit::replace(
                            0..recovery.saved.len(),
                            recovery.text.as_str(),
                        )],
                        recovery.selections.clone(),
                        None,
                    )
                    .unwrap();
            } else {
                expected
                    .set_selections(recovery.selections.clone())
                    .unwrap();
            }
            let rows = expected.line_count();
            expected
                .fold_state_mut()
                .set_ranges(recovery.collapsed.clone(), rows);
            expected.fold_state_mut().collapse_all();
            let mut prepared = recovery.restore_prepared(baseline, draft).unwrap();
            assert_eq!(prepared.text(), expected.text());
            assert_eq!(prepared.selections(), expected.selections());
            assert_eq!(prepared.is_dirty(), expected.is_dirty());
            assert_eq!(prepared.revision(), expected.revision());
            if recovery.text != recovery.saved {
                assert!(std::sync::Arc::ptr_eq(&prepared.line_index, &index));
            }
            assert_eq!(prepared.fold_state().ranges(), recovery.collapsed);
            assert_eq!(prepared.undo(), expected.undo());
            assert_eq!(prepared.fold_state(), expected.fold_state());
            assert_eq!(prepared.text(), recovery.saved.as_str());
            assert!(!prepared.is_dirty());
            assert_eq!(prepared.redo(), expected.redo());
            assert_eq!(prepared.fold_state(), expected.fold_state());
            assert_eq!(prepared.text(), expected.text());
            assert_eq!(prepared.selections(), expected.selections());
        }
    }

    #[test]
    fn prepared_recovery_rejects_mismatched_or_edited_documents() {
        let recovery = Document::new("base").recovery();
        assert!(
            recovery
                .restore_prepared(Document::new("wrong"), Document::new("base"))
                .is_err()
        );
        let mut edited = Document::new("bas");
        edited
            .apply(
                vec![Edit::replace(3..3, "e")],
                vec![Selection::caret(4)],
                None,
            )
            .unwrap();
        edited.mark_saved();
        assert!(
            recovery
                .restore_prepared(edited, Document::new("base"))
                .is_err()
        );
    }

    #[test]
    fn indexed_recovery_validation_preserves_admission_metadata_and_error_precedence() {
        let clean = |source: String| DocumentRecovery::clean_source(std::sync::Arc::new(source));
        let mut cases = vec![
            clean("head\r\nbody\rstandalone\nend".into()),
            clean("head\rbody".into()),
            clean("文😀\nend".into()),
            clean("x".repeat(super::super::MAX_EDITOR_BYTES + 1)),
            clean("x\n".repeat(super::super::MAX_EDITOR_LINES)),
            clean("x".repeat(super::super::MAX_EDITOR_LINE_BYTES + 1)),
        ];
        cases[0].collapsed = vec![FoldRange {
            start_line: 0,
            end_line: 2,
        }];
        // Standalone CR contributes to admission but does not create an LF
        // logical row for persisted folding metadata.
        cases[1].collapsed = vec![FoldRange {
            start_line: 0,
            end_line: 1,
        }];
        cases[2].selections = vec![Selection::caret(1)];
        let mut crossing = clean("a\nb\nc\nd\ne".into());
        crossing.collapsed = vec![
            FoldRange {
                start_line: 0,
                end_line: 2,
            },
            FoldRange {
                start_line: 1,
                end_line: 3,
            },
        ];
        cases.push(crossing);
        let mut competing = clean("x".repeat(super::super::MAX_EDITOR_BYTES + 1));
        competing.saved = "x".repeat(super::super::MAX_EDITOR_LINE_BYTES + 1).into();
        cases.push(competing.clone());
        // Metadata failures still precede either source's capacity rejection.
        competing.selections.clear();
        cases.push(competing);
        for recovery in cases {
            let expected = recovery.restore();
            let saved = Document::from_shared_text(recovery.saved.clone());
            let draft = Document::from_shared_text(recovery.text.clone());
            let prepared = recovery.restore_prepared(saved, draft);
            match (expected, prepared) {
                (Ok(expected), Ok(prepared)) => {
                    assert_eq!(prepared.recovery(), expected.recovery());
                    assert_eq!(prepared.admission().limit(), None);
                }
                (Err(expected), Err(prepared)) => assert_eq!(prepared, expected),
                _ => panic!("Indexed validation changed the recovery decision"),
            }
            // A mismatched index may never prove admission or fold validity
            // for the actual payload, and retains prior validation precedence.
            let expected_error = recovery.validate_editor().err().unwrap_or_else(|| {
                "Prepared recovery sources do not match the recovered document".into()
            });
            assert_eq!(
                recovery
                    .restore_prepared(Document::new("wrong"), Document::new("wrong"))
                    .err()
                    .unwrap(),
                expected_error
            );
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryScroll {
    pub top: f64,
    pub left: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorRecoveryFile {
    pub path: String,
    pub document: Option<DocumentRecovery>,
    pub scroll: RecoveryScroll,
    pub read_only: bool,
}

impl EditorRecoveryFile {
    /// Conservative metadata costs keep encoded payloads within the wire budget.
    pub fn recovery_bytes(&self) -> usize {
        self.document.as_ref().map_or(self.path.len(), |document| {
            self.path
                .len()
                .saturating_add(document.text.len())
                .saturating_add(document.saved.len())
                .saturating_add(document.collapsed.len().saturating_mul(96))
                .saturating_add(document.selections.len().saturating_mul(64))
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorRecoveryRoot {
    pub mode: crate::WorkspaceMode,
    pub path: Option<String>,
}
impl EditorRecoveryRoot {
    /// Host capability boundary: local roots are origin-bound directory handles.
    pub fn for_project(project: &crate::Project) -> Self {
        Self {
            mode: project.mode,
            path: match project.mode {
                crate::WorkspaceMode::Remote => project.path.clone(),
                crate::WorkspaceMode::Local => None,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorRecovery {
    pub format: u32,
    pub root: Option<EditorRecoveryRoot>,
    pub selected: Option<String>,
    pub files: Vec<EditorRecoveryFile>,
}
impl Default for EditorRecovery {
    fn default() -> Self {
        Self {
            format: 1,
            root: None,
            selected: None,
            files: Vec::new(),
        }
    }
}
impl EditorRecovery {
    pub fn validate(&self) -> Result<(), String> {
        if self.format != 1 {
            return Err("Unsupported editor recovery format".into());
        }
        if !self.files.is_empty() && self.root.is_none() {
            return Err("Recovered files have no project root".into());
        }
        if self.root.as_ref().is_some_and(|root| {
            root.path
                .as_ref()
                .is_some_and(|path| path.len() > 4096 || path.contains('\0'))
        }) {
            return Err("Recovered project root is invalid".into());
        }
        if self.files.len() > MAX_RECOVERY_FILES {
            return Err("Too many open files for editor recovery".into());
        }
        let mut paths = std::collections::HashSet::new();
        let mut bytes = 0_usize;
        for file in &self.files {
            if file.path.len() > 4096
                || crate::vfs::workspace_path(&file.path).map_err(|error| error.to_string())?
                    != file.path
                || file.path.is_empty()
                || !paths.insert(&file.path)
            {
                return Err("Recovered file paths must be unique project-relative paths".into());
            }
            if !file.scroll.top.is_finite()
                || !file.scroll.left.is_finite()
                || file.scroll.top < 0.0
                || file.scroll.left < 0.0
            {
                return Err("Recovered scroll position is invalid".into());
            }
            if let Some(document) = &file.document {
                document.validate()?;
            }
            bytes = bytes.saturating_add(file.recovery_bytes());
            if bytes > MAX_RECOVERY_BYTES {
                return Err(
                    "Editor recovery exceeds 128 MiB; keep these files open while saving them"
                        .into(),
                );
            }
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|selected| !paths.contains(selected))
        {
            return Err("Selected recovered file is not an open tab".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorRecoveryRecord {
    pub revision: i64,
    pub state: EditorRecovery,
}
// Encoding bounds escaped Unicode/control-heavy files without a second JSON layer.
mod encoded_text {
    use super::MAX_DOCUMENT_BYTES;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use serde::{
        Deserializer, Serializer,
        de::{Error, Visitor},
    };
    pub fn serialize<S: Serializer>(value: &str, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(value.as_bytes()))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<std::sync::Arc<String>, D::Error> {
        deserializer.deserialize_str(EncodedText)
    }

    struct EncodedText;
    impl<'de> Visitor<'de> for EncodedText {
        type Value = std::sync::Arc<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("base64-encoded UTF-8 editor text")
        }

        fn visit_str<E: Error>(self, value: &str) -> Result<Self::Value, E> {
            if value.len() > MAX_DOCUMENT_BYTES.div_ceil(3) * 4 {
                return Err(E::custom("Recovered text exceeds document size limit"));
            }
            let bytes = STANDARD.decode(value).map_err(E::custom)?;
            if bytes.len() > MAX_DOCUMENT_BYTES {
                return Err(E::custom("Recovered text exceeds document size limit"));
            }
            String::from_utf8(bytes)
                .map(std::sync::Arc::new)
                .map_err(E::custom)
        }

        #[cfg(test)]
        fn visit_borrowed_str<E: Error>(self, value: &'de str) -> Result<Self::Value, E> {
            BORROWED_FIELDS.with(|fields| {
                fields
                    .borrow_mut()
                    .push((value.as_ptr() as usize, value.len()));
            });
            self.visit_str(value)
        }
    }

    #[cfg(test)]
    std::thread_local! {
        pub(super) static BORROWED_FIELDS: std::cell::RefCell<Vec<(usize, usize)>> = const { std::cell::RefCell::new(Vec::new()) };
    }
}

#[cfg(test)]
mod tests {
    use super::super::NativeInputKind;
    use super::*;

    #[test]
    fn borrowed_selection_validation_matches_normalization_without_reordering() {
        let source = "a文😀\r\nz";
        for anchor in 0..=source.len() + 1 {
            for head in 0..=source.len() + 1 {
                let selections = vec![
                    Selection { anchor, head },
                    Selection::caret(0),
                    Selection {
                        anchor: source.len(),
                        head: 0,
                    },
                ];
                let expected = normalize_selections(source, selections.clone())
                    .map(|_| ())
                    .map_err(|error| error.to_string());
                let mut recovery = DocumentRecovery::clean(source);
                recovery.selections = selections.clone();
                assert_eq!(recovery.validate(), expected);
                assert_eq!(recovery.selections, selections);
            }
        }
        for selections in [Vec::new(), vec![Selection::caret(0); MAX_SELECTIONS + 1]] {
            let mut recovery = DocumentRecovery::clean(source);
            recovery.selections = selections;
            assert_eq!(
                recovery.validate().unwrap_err(),
                "Recovered selections exceed the editor's limits"
            );
        }
    }

    #[test]
    fn recovery_decode_borrows_encoded_json_fields_and_owns_decoded_sources() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let saved = "\0文😀\"\\\r\n".repeat(8192);
        let draft = format!("{saved}changed");
        let recovery = DocumentRecovery {
            text: draft.clone().into(),
            saved: saved.clone().into(),
            selections: vec![Selection::caret(draft.len())],
            collapsed: vec![],
        };
        let json = serde_json::to_string(&recovery).unwrap();
        let expected: Vec<_> = [("text", &draft), ("saved", &saved)]
            .into_iter()
            .map(|(field, source)| {
                let marker = format!("\"{field}\":\"");
                let start = json.find(&marker).unwrap() + marker.len();
                let encoded = STANDARD.encode(source);
                assert_eq!(&json[start..start + encoded.len()], encoded);
                (json.as_ptr() as usize + start, encoded.len())
            })
            .collect();
        encoded_text::BORROWED_FIELDS.with(|fields| fields.borrow_mut().clear());
        let decoded: DocumentRecovery = serde_json::from_slice(json.as_bytes()).unwrap();
        encoded_text::BORROWED_FIELDS.with(|fields| assert_eq!(*fields.borrow(), expected));
        assert_eq!(decoded, recovery);
        assert!(!std::sync::Arc::ptr_eq(&decoded.text, &recovery.text));
        assert!(!std::sync::Arc::ptr_eq(&decoded.saved, &recovery.saved));
        drop(json);
        drop(recovery);
        assert_eq!(decoded.text.as_str(), draft);
        assert_eq!(decoded.saved.as_str(), saved);
        decoded.validate().unwrap();
    }

    #[test]
    fn recovery_decode_accepts_owned_and_escaped_fields_and_rejects_wrong_types() {
        let recovery = DocumentRecovery::clean("文😀\0\r\n");
        let value = serde_json::to_value(&recovery).unwrap();
        assert_eq!(
            serde_json::from_value::<DocumentRecovery>(value.clone()).unwrap(),
            recovery
        );
        let json = serde_json::to_string(&recovery).unwrap();
        let escaped = json.replace("\"text\":\"5", "\"text\":\"\\u0035");
        assert_ne!(escaped, json);
        assert_eq!(
            serde_json::from_str::<DocumentRecovery>(&escaped).unwrap(),
            recovery
        );
        for text in [
            serde_json::Value::Null,
            serde_json::json!(true),
            serde_json::json!(42),
            serde_json::json!([]),
            serde_json::json!({}),
        ] {
            let mut invalid = value.clone();
            invalid["text"] = text;
            assert!(serde_json::from_value::<DocumentRecovery>(invalid).is_err());
        }
    }

    #[test]
    fn recovery_capture_clone_and_restore_share_immutable_sources_and_keep_wire_format() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let mut document = Document::new("base 😀\r\n");
        let baseline = document.shared_text();
        document.replace_selections("文", None).unwrap();
        let source = document.shared_text();
        let captured = document.recovery();
        assert!(std::sync::Arc::ptr_eq(&captured.text, &source));
        assert!(std::sync::Arc::ptr_eq(&captured.saved, &baseline));
        let retained = captured.clone();
        assert!(std::sync::Arc::ptr_eq(&retained.text, &captured.text));
        assert!(std::sync::Arc::ptr_eq(&retained.saved, &captured.saved));
        let restored = retained.restore().unwrap();
        assert!(std::sync::Arc::ptr_eq(
            &restored.shared_text(),
            &retained.text
        ));
        assert!(std::sync::Arc::ptr_eq(&restored.saved, &retained.saved));
        let serialized = serde_json::to_value(&captured).unwrap();
        assert_eq!(
            serialized,
            serde_json::json!({
                "text": STANDARD.encode(source.as_bytes()),
                "saved": STANDARD.encode(baseline.as_bytes()),
                "selections": captured.selections,
                "collapsed": [],
            })
        );
        let decoded: DocumentRecovery = serde_json::from_value(serialized).unwrap();
        assert_eq!(decoded, captured);
        document.replace_selections("new", None).unwrap();
        assert_eq!(retained.text.as_str(), "文base 😀\r\n");
        assert_eq!(retained.saved.as_str(), "base 😀\r\n");
        let clean = DocumentRecovery::clean_source(source.clone());
        assert!(std::sync::Arc::ptr_eq(&clean.text, &source));
        assert!(std::sync::Arc::ptr_eq(&clean.saved, &source));
    }

    #[test]
    fn live_disk_classification_matches_persisted_policy_and_preserves_sources() {
        for dirty in [false, true] {
            let mut document = Document::new("base 😀\r\n");
            if dirty {
                document.replace_selections("文", None).unwrap();
            }
            let committed = document.recovery();
            document.begin_composition(None);
            document
                .native_input(
                    "preview",
                    Selection::caret(7),
                    NativeInputKind::Insert,
                    None,
                )
                .unwrap();
            let source = document.shared_text();
            let baseline = document.saved.clone();
            for disk in [
                None,
                Some(committed.saved.as_str()),
                Some(committed.text.as_str()),
                Some("external 😀\r\n"),
                Some("preview"),
            ] {
                assert_eq!(
                    document.recovery_disk_state(disk),
                    committed.disk_state(disk)
                );
                assert!(std::sync::Arc::ptr_eq(&source, &document.shared_text()));
                assert!(std::sync::Arc::ptr_eq(&baseline, &document.saved));
            }
            document.mark_saved_version("late write");
            let committed = document.recovery();
            for disk in [None, Some("late write"), Some(committed.text.as_str())] {
                assert_eq!(
                    document.recovery_disk_state(disk),
                    committed.disk_state(disk)
                );
            }
        }
        let clean = Document::new("base");
        let oversized_row = "x".repeat(super::super::MAX_EDITOR_LINE_BYTES + 1);
        assert_eq!(
            clean.recovery_disk_state(Some(&oversized_row)),
            clean.recovery().disk_state(Some(&oversized_row))
        );
        assert!(clean.recovery_disk_state(Some(&oversized_row)).is_err());
        let oversized_baseline = Document::new(oversized_row);
        assert_eq!(
            oversized_baseline.recovery_disk_state(None),
            oversized_baseline.recovery().disk_state(None)
        );
        assert!(oversized_baseline.recovery_disk_state(None).is_err());
    }

    #[test]
    fn disk_reconciliation_preserves_conflicting_drafts_and_detects_completed_writes() {
        let mut draft = Document::new("base\r\n");
        draft.replace_selections("文", None).unwrap();
        let recovery = draft.recovery();
        for (disk, expected) in [
            (Some("base\r\n"), RecoveryDiskState::Current),
            (Some("external\r\n"), RecoveryDiskState::Conflict),
            (None, RecoveryDiskState::Missing),
        ] {
            let (mut restored, state) = recovery.reconcile_disk(disk).unwrap();
            assert_eq!(state, expected);
            assert_eq!(restored.recovery(), recovery);
            assert!(restored.undo());
            assert_eq!(restored.text(), recovery.saved.as_str());
        }
        let (mut restored, state) = recovery.reconcile_disk(Some(&recovery.text)).unwrap();
        assert_eq!(state, RecoveryDiskState::AlreadySaved);
        assert!(!restored.is_dirty());
        assert!(restored.undo());
        assert!(restored.is_dirty());
        let clean = Document::new("old").recovery();
        let (restored, state) = clean.reconcile_disk(Some("new 文")).unwrap();
        assert_eq!(state, RecoveryDiskState::Reloaded);
        assert_eq!(restored.text(), "new 文");
        assert!(!restored.is_dirty());
        assert!(!restored.can_undo());
        assert_eq!(
            clean.reconcile_disk(None).unwrap().1,
            RecoveryDiskState::Missing
        );
    }

    #[test]
    fn clean_source_capture_matches_document_recovery_without_indexes() {
        for source in ["", "文😀\r\nsecond\rthird\n", "one\n"] {
            let recovery = DocumentRecovery::clean(source);
            assert_eq!(recovery, Document::new(source).recovery());
            recovery.validate_editor().unwrap();
            let restored = recovery.restore().unwrap();
            assert_eq!(restored.text(), source);
            assert!(!restored.is_dirty());
            assert!(!restored.can_undo());
        }
        let mut recovery = DocumentRecovery::clean("one\n");
        recovery.collapsed.push(FoldRange {
            start_line: 0,
            end_line: 2,
        });
        assert!(recovery.validate().is_err());
    }

    #[test]
    fn recovery_preserves_unicode_crlf_baseline_selections_folds_and_undo() {
        let mut document = Document::new("fn f() {\r\n  α\r\n}\r\n");
        document.set_selections(vec![Selection::caret(14)]).unwrap();
        document.replace_selections("😀", None).unwrap();
        document.folds.set_ranges(
            vec![FoldRange {
                start_line: 0,
                end_line: 2,
            }],
            4,
        );
        document.folds.toggle(0);
        let snapshot = document.recovery();
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(!json.contains('α'));
        let decoded: DocumentRecovery = serde_json::from_str(&json).unwrap();
        let mut restored = decoded.restore().unwrap();
        assert_eq!(restored.text(), document.text());
        assert_eq!(restored.selections(), document.selections());
        assert!(restored.is_dirty());
        assert!(restored.folds.collapsed_at(0).is_some());
        assert!(restored.undo());
        assert_eq!(restored.text(), snapshot.saved.as_str());
        assert!(!restored.is_dirty());
        assert!(restored.redo());
        assert_eq!(restored.text(), snapshot.text.as_str());
    }

    #[test]
    fn recovery_omits_composition_previews_and_keeps_completed_writes() {
        let mut document = Document::new("base");
        document.replace_selections("x", None).unwrap();
        let before = document.recovery();
        document.begin_composition(None);
        document
            .native_input(
                "文xbase",
                Selection::caret(3),
                NativeInputKind::Insert,
                None,
            )
            .unwrap();
        assert_eq!(document.recovery(), before);
        document.mark_saved_version("written");
        assert_eq!(document.recovery().saved.as_str(), "written");
        assert_eq!(document.recovery().text.as_str(), "xbase");
    }

    #[test]
    fn recovery_rejects_malformed_text_selection_folds_paths_versions_and_scroll() {
        let mut snapshot = Document::new("α\nline\n").recovery();
        snapshot.selections = vec![Selection::caret(1)];
        assert!(snapshot.validate().is_err());
        snapshot.selections = vec![Selection::caret(0)];
        snapshot.collapsed = vec![FoldRange {
            start_line: 0,
            end_line: 9,
        }];
        assert!(snapshot.validate().is_err());
        let invalid_utf8 =
            r#"{"text":"/w==","saved":"","selections":[{"anchor":0,"head":0}],"collapsed":[]}"#;
        assert!(serde_json::from_str::<DocumentRecovery>(invalid_utf8).is_err());
        let invalid_base64 = invalid_utf8.replace("/w==", "garbage!");
        assert!(serde_json::from_str::<DocumentRecovery>(&invalid_base64).is_err());
        let file = EditorRecoveryFile {
            path: "a.rs".into(),
            document: None,
            scroll: RecoveryScroll::default(),
            read_only: false,
        };
        let mut state = EditorRecovery {
            format: 1,
            root: Some(EditorRecoveryRoot {
                mode: crate::WorkspaceMode::Local,
                path: None,
            }),
            selected: Some(file.path.clone()),
            files: vec![file],
        };
        state.validate().unwrap();
        state.files[0].path = "../outside".into();
        assert!(state.validate().is_err());
        state.files[0].path = "a.rs".into();
        state.files.push(state.files[0].clone());
        assert!(state.validate().is_err());
        state.files.pop();
        state.selected = Some("missing.rs".into());
        assert!(state.validate().is_err());
        state.selected = None;
        state.files[0].scroll.top = f64::INFINITY;
        assert!(state.validate().is_err());
        state.files[0].scroll.top = -1.0;
        assert!(state.validate().is_err());
        state.files[0].scroll.top = 0.0;
        state.format = 2;
        assert!(state.validate().is_err());
        assert_eq!(
            serde_json::from_str::<EditorRecovery>(
                &serde_json::to_string(&EditorRecovery::default()).unwrap()
            )
            .unwrap(),
            EditorRecovery::default()
        );
    }
}
