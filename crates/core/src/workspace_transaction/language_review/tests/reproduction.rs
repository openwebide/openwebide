use super::*;
use crate::{
    MemoryVfs, Vfs,
    editor::{EditorRecovery, EditorRecoveryRecord},
    plugins::languages::intelligence::workspace_edit::{WorkspaceDocument, WorkspaceDraft},
    workspace_transaction::VfsTransactionFiles,
};
use futures::executor::block_on;
use std::collections::BTreeMap;

async fn capture(
    memory: &MemoryVfs,
    before: &EditorRecoveryRecord,
    changes: Vec<WorkspaceChange>,
) -> (LanguageReview, WorkspacePlan, EditorRecoveryPatch) {
    let (mut review, _, _) = fixture();
    let files = VfsTransactionFiles {
        vfs: memory,
        identity: &"a".repeat(64),
    };
    let operations = changes
        .iter()
        .filter_map(|change| match change {
            WorkspaceChange::File { operation } => Some(operation.clone()),
            WorkspaceChange::Text { .. } => None,
        })
        .collect();
    let mut documents = BTreeMap::new();
    for file in &before.state.files {
        if let Some(source) = &file.document {
            let mut document = WorkspaceDocument::new(&file.path, source.restore().unwrap());
            document.read_only = file.read_only;
            documents.insert(file.path.clone(), document);
        }
    }
    let mut draft = WorkspaceDraft::prepare_native(
        &files,
        files.identity.into(),
        operations,
        documents,
        before
            .state
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect(),
        before.state.selected.clone(),
        || true,
    )
    .await
    .unwrap();
    for change in &changes {
        match change {
            WorkspaceChange::File { operation } => draft.resource(operation.clone()).unwrap(),
            WorkspaceChange::Text { file } => {
                draft.document(&files, &file.path, || true).await.unwrap();
                draft.text(file.clone()).unwrap();
            }
        }
    }
    let after = draft.recovery(&before.state).unwrap();
    let patch = draft.recovery_patch(before, &after).unwrap();
    assert_eq!(patch.apply_to(&before.state).unwrap(), after);
    review.result = serde_json::to_value(WorkspaceEdit {
        files: vec![],
        changes,
    })
    .unwrap();
    review.context.result_digest =
        crate::plugins::content_digest(&serde_json::to_vec(&review.result).unwrap());
    review.transaction_digest = LanguageReview::transaction_digest(&draft.plan, &patch).unwrap();
    (review, draft.plan, patch)
}
fn before(mode: WorkspaceMode, files: Vec<EditorRecoveryFile>) -> EditorRecoveryRecord {
    EditorRecoveryRecord {
        revision: 3,
        state: EditorRecovery {
            format: 1,
            root: Some(EditorRecoveryRoot {
                mode,
                path: (mode == WorkspaceMode::Remote).then(|| "project".into()),
            }),
            selected: files.first().map(|file| file.path.clone()),
            files,
        },
    }
}
fn dirty(path: &str) -> EditorRecoveryFile {
    let mut document = Document::new("disk 文😀\r\n").recovery();
    document.text = std::sync::Arc::new("draft 文😀\r\n".into());
    EditorRecoveryFile {
        path: path.into(),
        document: Some(document),
        scroll: RecoveryScroll {
            top: 7.0,
            left: 3.0,
        },
        read_only: false,
    }
}

#[test]
fn native_reproduction_retains_equal_overwrite_inputs_and_rejects_forged_editor_outputs() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let memory = MemoryVfs::new();
            memory.write("source.rs", "disk 文😀\r\n").await.unwrap();
            memory
                .write("destination.rs", "disk 文😀\r\n")
                .await
                .unwrap();
            let before = before(
                mode,
                vec![
                    dirty("source.rs"),
                    dirty("destination.rs"),
                    dirty("unrelated.rs"),
                    EditorRecoveryFile {
                        path: "image.png".into(),
                        document: None,
                        scroll: RecoveryScroll::default(),
                        read_only: true,
                    },
                ],
            );
            let changes = vec![WorkspaceChange::File {
                operation: FileOperation::Rename {
                    from: "source.rs".into(),
                    to: "destination.rs".into(),
                    overwrite: true,
                    ignore_if_exists: false,
                },
            }];
            let (review, plan, patch) = capture(&memory, &before, changes).await;
            assert_eq!(
                patch
                    .before
                    .iter()
                    .map(|file| file.path.as_str())
                    .collect::<Vec<_>>(),
                vec!["source.rs", "destination.rs"]
            );
            assert_eq!(patch.after, vec![before.state.files[1].clone()]);
            let files = VfsTransactionFiles {
                vfs: &memory,
                identity: &plan.root,
            };
            review
                .reproduce(&files, &plan, &patch, || true)
                .await
                .unwrap();
            for field in ["text", "saved", "selection", "scroll", "read_only"] {
                let mut forged = patch.clone();
                match field {
                    "text" => {
                        forged.after[0].document.as_mut().unwrap().text =
                            std::sync::Arc::new("unreviewed".into());
                    }
                    "saved" => {
                        forged.after[0].document.as_mut().unwrap().saved =
                            std::sync::Arc::new("unreviewed".into());
                    }
                    "selection" => {
                        forged.after[0].document.as_mut().unwrap().selections =
                            vec![crate::editor::Selection::caret(1)];
                    }
                    "scroll" => forged.after[0].scroll.top += 1.0,
                    _ => forged.after[0].read_only = true,
                }
                // The client's consistency digest is not authentication. Even
                // recomputing it must not authorize an arbitrary editor patch.
                let mut changed = review.clone();
                changed.transaction_digest =
                    LanguageReview::transaction_digest(&plan, &forged).unwrap();
                changed.validate_transaction(&plan, &forged).unwrap();
                assert!(
                    changed
                        .reproduce(&files, &plan, &forged, || true)
                        .await
                        .is_err(),
                    "Accepted forged {field}"
                );
            }
            assert_eq!(memory.read("source.rs").await.unwrap(), "disk 文😀\r\n");
            assert_eq!(
                memory.read("destination.rs").await.unwrap(),
                "disk 文😀\r\n"
            );
        }
    });
}

#[test]
fn no_op_text_reviews_retain_dirty_source_witnesses_and_cannot_drop_them() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let memory = MemoryVfs::new();
            memory.write("source.rs", "disk 文😀\r\n").await.unwrap();
            let before = before(mode, vec![dirty("source.rs"), dirty("unrelated.rs")]);
            let change = WorkspaceChange::Text {
                file: FileEdit {
                    path: "source.rs".into(),
                    source_digest: source_digest("draft 文😀\r\n"),
                    revision: Some(7),
                    edits: vec![TextEdit {
                        range: Range::default(),
                        new_text: String::new(),
                    }],
                },
            };
            let (review, plan, patch) = capture(&memory, &before, vec![change]).await;
            assert_eq!(patch.before, vec![before.state.files[0].clone()]);
            assert_eq!(patch.after, patch.before);
            let files = VfsTransactionFiles {
                vfs: &memory,
                identity: &plan.root,
            };
            review
                .reproduce(&files, &plan, &patch, || true)
                .await
                .unwrap();
            let mut dropped = patch.clone();
            dropped.before.clear();
            dropped.after.clear();
            let mut changed = review.clone();
            changed.transaction_digest =
                LanguageReview::transaction_digest(&plan, &dropped).unwrap();
            changed.validate_transaction(&plan, &dropped).unwrap();
            assert!(
                changed
                    .reproduce(&files, &plan, &dropped, || true)
                    .await
                    .is_err()
            );
            assert_eq!(memory.read("source.rs").await.unwrap(), "disk 文😀\r\n");
        }
    });
}

#[test]
fn a_first_editor_record_can_take_its_reviewed_root_from_the_result() {
    block_on(async {
        let memory = MemoryVfs::new();
        memory.write("source.rs", "disk 文😀\r\n").await.unwrap();
        let visible = before(
            WorkspaceMode::Local,
            vec![EditorRecoveryFile {
                document: Some(Document::new("disk 文😀\r\n").recovery()),
                ..dirty("source.rs")
            }],
        );
        let identity = "a".repeat(64);
        let files = VfsTransactionFiles {
            vfs: &memory,
            identity: &identity,
        };
        let mut draft = WorkspaceDraft::prepare_native(
            &files,
            files.identity.into(),
            vec![],
            [(
                "source.rs".into(),
                WorkspaceDocument::new("source.rs", Document::new("disk 文😀\r\n")),
            )]
            .into(),
            vec!["source.rs".into()],
            Some("source.rs".into()),
            || true,
        )
        .await
        .unwrap();
        draft
            .text(FileEdit {
                path: "source.rs".into(),
                source_digest: source_digest("disk 文😀\r\n"),
                revision: None,
                edits: vec![TextEdit {
                    range: Range::default(),
                    new_text: "new".into(),
                }],
            })
            .unwrap();
        let after = draft.recovery(&visible.state).unwrap();
        let empty = EditorRecoveryRecord::default();
        assert!(empty.state.root.is_none());
        let patch = draft.recovery_patch(&empty, &after).unwrap();
        assert_eq!(patch.apply_to(&empty.state).unwrap(), after);
    });
}
