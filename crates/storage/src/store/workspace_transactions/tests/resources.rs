//! Database decision tests deliberately stop before filesystem cleanup. The
//! core resource adapter contracts exercise every actual primitive boundary.
use super::*;
use std::sync::Arc;

async fn begin(store: &Store<RusqliteDb>, fixture: &Fixture) -> JournalRecord {
    let mut plan = fixture.plan.clone();
    plan.version = 2;
    store
        .begin_workspace_transaction(
            fixture.user,
            fixture.project.id,
            &BeginWorkspaceTransaction {
                language_review: None,
                id: "a".repeat(32),
                plan,
                editor: fixture.editor.clone(),
            },
            100,
        )
        .await
        .unwrap();
    store
        .workspace_transaction(fixture.user, fixture.project.id, None)
        .await
        .unwrap()
        .unwrap()
        .journal
}
async fn advance(
    store: &Store<RusqliteDb>,
    fixture: &Fixture,
    record: &mut JournalRecord,
    progress: TransactionProgress,
) -> WorkspaceTransactionAdvanced {
    let result = store
        .advance_workspace_transaction(
            fixture.user,
            fixture.project.id,
            &AdvanceWorkspaceTransaction {
                id: record.id.clone(),
                generation: record.generation.clone(),
                revision: record.revision,
                progress: progress.clone(),
            },
            100,
        )
        .await
        .unwrap();
    record.revision = result.revision;
    record.progress = progress;
    result
}
async fn ready(store: &Store<RusqliteDb>, fixture: &Fixture) -> JournalRecord {
    let mut record = begin(store, fixture).await;
    advance(
        store,
        fixture,
        &mut record,
        TransactionProgress {
            phase: TransactionPhase::Applying,
            ..TransactionProgress::default()
        },
    )
    .await;
    advance(
        store,
        fixture,
        &mut record,
        TransactionProgress {
            phase: TransactionPhase::Applying,
            active: true,
            ..TransactionProgress::default()
        },
    )
    .await;
    advance(
        store,
        fixture,
        &mut record,
        TransactionProgress {
            phase: TransactionPhase::Applying,
            completed: 1,
            ..TransactionProgress::default()
        },
    )
    .await;
    record
}

#[test]
fn resource_decision_commits_editor_once_and_blocks_replacement_until_cleanup_in_both_modes() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let fixture = fixture(&store, mode).await;
            let mut record = ready(&store, &fixture).await;
            assert_eq!(
                store
                    .editor_recovery(fixture.user, fixture.project.id)
                    .await
                    .unwrap()
                    .revision,
                1
            );
            let decision = TransactionProgress {
                phase: TransactionPhase::Finalizing,
                ..record.progress.clone()
            };
            let response = advance(&store, &fixture, &mut record, decision).await;
            let editor = response.editor.unwrap();
            assert_eq!(editor.revision, 2);
            assert_eq!(editor.state.selected.as_deref(), Some("b.rs"));
            assert_eq!(editor.state.files[0], fixture.editor.after[0]);
            assert!(matches!(
                store.delete_project(fixture.project.id, fixture.user).await,
                Err(StorageError::Conflict(_))
            ));
            let second = BeginWorkspaceTransaction {
                language_review: None,
                id: "b".repeat(32),
                plan: record.plan.clone(),
                editor: fixture.editor.clone(),
            };
            assert!(matches!(
                store
                    .begin_workspace_transaction(fixture.user, fixture.project.id, &second, 101)
                    .await,
                Err(StorageError::Conflict(_))
            ));
            let illegal = TransactionProgress {
                phase: TransactionPhase::Reverting,
                completed: 0,
                active: false,
                rollback_from: Some(1),
            };
            assert!(
                store
                    .advance_workspace_transaction(
                        fixture.user,
                        fixture.project.id,
                        &AdvanceWorkspaceTransaction {
                            id: record.id.clone(),
                            generation: record.generation.clone(),
                            revision: record.revision,
                            progress: illegal
                        },
                        101
                    )
                    .await
                    .is_err()
            );
            let mut newer = editor.state;
            newer.files[0].document.as_mut().unwrap().text =
                Arc::new("typed while cleanup was pending".into());
            assert_eq!(
                store
                    .save_editor_recovery(fixture.user, fixture.project.id, 2, &newer)
                    .await
                    .unwrap(),
                3
            );
            let terminal = TransactionProgress {
                phase: TransactionPhase::Committed,
                ..record.progress.clone()
            };
            let result = advance(&store, &fixture, &mut record, terminal).await;
            assert!(result.editor.is_none());
            let retained = store
                .editor_recovery(fixture.user, fixture.project.id)
                .await
                .unwrap();
            assert_eq!(retained.revision, 3);
            assert_eq!(retained.state, newer);
            store
                .delete_project(fixture.project.id, fixture.user)
                .await
                .unwrap();
        }
    });
}

#[test]
fn a_newer_draft_prevents_the_irreversible_resource_decision_in_both_modes() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let fixture = fixture(&store, mode).await;
            let record = ready(&store, &fixture).await;
            let previous = store
                .editor_recovery(fixture.user, fixture.project.id)
                .await
                .unwrap();
            let mut newer = previous.state;
            newer.files[0].document.as_mut().unwrap().text =
                Arc::new("newer draft before decision".into());
            store
                .save_editor_recovery(fixture.user, fixture.project.id, previous.revision, &newer)
                .await
                .unwrap();
            let result = store
                .advance_workspace_transaction(
                    fixture.user,
                    fixture.project.id,
                    &AdvanceWorkspaceTransaction {
                        id: record.id.clone(),
                        generation: record.generation.clone(),
                        revision: record.revision,
                        progress: TransactionProgress {
                            phase: TransactionPhase::Finalizing,
                            ..record.progress.clone()
                        },
                    },
                    101,
                )
                .await;
            assert!(matches!(result, Err(StorageError::Conflict(_))));
            let saved = store
                .workspace_transaction(fixture.user, fixture.project.id, None)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(saved.journal, record);
            assert!(!saved.journal.progress.commits_editor());
            assert_eq!(
                store
                    .editor_recovery(fixture.user, fixture.project.id)
                    .await
                    .unwrap()
                    .state,
                newer
            );
        }
    });
}

#[test]
fn restored_resource_decision_leaves_editor_drafts_unchanged_in_both_modes() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let fixture = fixture(&store, mode).await;
            let mut record = begin(&store, &fixture).await;
            let before = store
                .editor_recovery(fixture.user, fixture.project.id)
                .await
                .unwrap();
            advance(
                &store,
                &fixture,
                &mut record,
                TransactionProgress {
                    phase: TransactionPhase::Reverting,
                    completed: 0,
                    active: false,
                    rollback_from: Some(0),
                },
            )
            .await;
            let finalizing = TransactionProgress {
                phase: TransactionPhase::Finalizing,
                ..record.progress.clone()
            };
            assert!(
                advance(&store, &fixture, &mut record, finalizing)
                    .await
                    .editor
                    .is_none()
            );
            let restored = TransactionProgress {
                phase: TransactionPhase::Restored,
                ..record.progress.clone()
            };
            assert!(
                advance(&store, &fixture, &mut record, restored)
                    .await
                    .editor
                    .is_none()
            );
            assert_eq!(
                store
                    .editor_recovery(fixture.user, fixture.project.id)
                    .await
                    .unwrap(),
                before
            );
        }
    });
}
