use std::{collections::HashMap, sync::Arc};

use leptos::prelude::*;
use openwebide_core::editor::EditorRecoveryRecord;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryPhase {
    Loading,
    Ready,
    LoadFailed(String),
    SaveFailed(String),
    Conflict(String),
}

#[derive(Clone, Debug)]
pub struct RecoveryProject {
    // Acknowledged snapshots are immutable; reads must not copy fold/selection tables.
    pub record: Option<Arc<EditorRecoveryRecord>>,
    pub phase: RecoveryPhase,
    pub ticket: u64,
    pub writing: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryFileReview {
    pub path: String,
    pub disk: Option<String>,
    pub draft: String,
    pub read_only: bool,
}

/// Database recovery state is separate from editable workspace buffers. Errors
/// retain both the last acknowledged revision and the user's current documents.
#[derive(Clone, Copy)]
pub struct EditorRecoveryState {
    pub projects: RwSignal<HashMap<i64, RecoveryProject>>,
    pub file_review: RwSignal<Option<RecoveryFileReview>>,
    pub file_review_ticket: RwSignal<u64>,
}

impl EditorRecoveryState {
    pub fn new() -> Self {
        Self {
            projects: RwSignal::new(HashMap::new()),
            file_review: RwSignal::new(None),
            file_review_ticket: RwSignal::new(0),
        }
    }
}

impl Default for EditorRecoveryState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acknowledged_snapshots_share_metadata_and_survive_new_acknowledgements() {
        use openwebide_core::editor::{
            Document, EditorRecovery, EditorRecoveryFile, FoldRange, RecoveryScroll,
        };
        let mut document = Document::new("line\n".repeat(20_001));
        document.set_fold_ranges(
            (0..10_000)
                .map(|index| FoldRange {
                    start_line: index * 2,
                    end_line: index * 2 + 1,
                })
                .collect(),
        );
        document.fold_state_mut().collapse_all();
        let mut project = RecoveryProject {
            record: Some(Arc::new(EditorRecoveryRecord {
                revision: 1,
                state: EditorRecovery {
                    files: vec![EditorRecoveryFile {
                        path: "a.rs".into(),
                        document: Some(document.recovery()),
                        scroll: RecoveryScroll::default(),
                        read_only: false,
                    }],
                    ..EditorRecovery::default()
                },
            })),
            phase: RecoveryPhase::Ready,
            ticket: 1,
            writing: false,
        };
        let previous = project.clone().record.unwrap();
        let current = project.record.as_ref().unwrap();
        assert!(Arc::ptr_eq(&previous, current));
        assert_eq!(
            previous.state.files[0]
                .document
                .as_ref()
                .unwrap()
                .collapsed
                .len(),
            10_000
        );
        project.record = Some(Arc::new(EditorRecoveryRecord {
            revision: 2,
            state: EditorRecovery::default(),
        }));
        assert_eq!(previous.revision, 1);
        assert_eq!(
            previous.state.files[0]
                .document
                .as_ref()
                .unwrap()
                .collapsed
                .len(),
            10_000
        );
        assert_eq!(project.record.as_ref().unwrap().revision, 2);
        assert!(!Arc::ptr_eq(&previous, project.record.as_ref().unwrap()));
    }
}
