use super::*;
use openwebide_core::workspace_transaction::{
    WorkspaceTransactionScope, WorkspaceTransactionWorker,
};

async fn admitted(
    store: &Store<RusqliteDb>,
    mode: WorkspaceMode,
) -> (
    Fixture,
    WorkspaceTransactionScope,
    WorkspaceTransactionWorker,
) {
    let fixture = fixture(store, mode).await;
    let journal = journal(store, &fixture);
    journal.prepare(&fixture.plan).await.unwrap();
    let scope = store
        .workspace_transaction_scope(fixture.user, fixture.project.id, &journal.id)
        .await
        .unwrap()
        .unwrap();
    let worker = WorkspaceTransactionWorker {
        id: "1".repeat(32),
        secret: "2".repeat(32),
    };
    let scope = store
        .claim_workspace_transaction_worker(fixture.user, fixture.project.id, &scope, &worker, 100)
        .await
        .unwrap();
    (fixture, scope, worker)
}
fn cancel(scope: &WorkspaceTransactionScope) -> AdvanceWorkspaceTransaction {
    AdvanceWorkspaceTransaction {
        id: scope.id.clone(),
        generation: scope.generation.clone(),
        revision: scope.revision,
        progress: TransactionProgress::prepared_cancellation(),
    }
}
#[test]
fn account_cancels_admitted_prepared_work_without_a_folder_and_fences_old_workers() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let (fixture, scope, worker) = admitted(&store, mode).await;
            let editor = store
                .editor_recovery(fixture.user, fixture.project.id)
                .await
                .unwrap();
            let before = fixture.files.memory.read("a.rs").await.unwrap();
            let response = store
                .advance_workspace_transaction(
                    fixture.user,
                    fixture.project.id,
                    &cancel(&scope),
                    101,
                )
                .await
                .unwrap();
            response
                .validate(&scope.generation, scope.revision)
                .unwrap();
            assert!(response.editor.is_none());
            let state = store
                .workspace_transaction(fixture.user, fixture.project.id, Some(&scope.id))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                state.journal.progress,
                TransactionProgress::prepared_cancellation()
            );
            let current = store
                .workspace_transaction_scope(fixture.user, fixture.project.id, &scope.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(current.worker.as_ref().unwrap().id, worker.id);
            assert!(!current.worker.as_ref().unwrap().active);
            assert_eq!(
                store
                    .editor_recovery(fixture.user, fixture.project.id)
                    .await
                    .unwrap(),
                editor
            );
            assert_eq!(fixture.files.memory.read("a.rs").await.unwrap(), before);
            assert!(fixture.files.memory.read("b.rs").await.is_err());
            let intent = AdvanceWorkspaceTransaction {
                progress: TransactionProgress {
                    phase: TransactionPhase::Applying,
                    ..Default::default()
                },
                ..cancel(&scope)
            };
            assert!(
                store
                    .advance_workspace_transaction_for_worker(
                        fixture.user,
                        fixture.project.id,
                        &intent,
                        Some(&worker),
                        102
                    )
                    .await
                    .is_err()
            );
            assert!(
                store
                    .claim_workspace_transaction_worker(
                        fixture.user,
                        fixture.project.id,
                        &current,
                        &worker,
                        102
                    )
                    .await
                    .is_err()
            );
            let next = WorkspaceTransactionWorker {
                id: "3".repeat(32),
                secret: "4".repeat(32),
            };
            assert!(
                store
                    .claim_workspace_transaction_worker(
                        fixture.user,
                        fixture.project.id,
                        &scope,
                        &next,
                        102
                    )
                    .await
                    .is_err()
            );
            store
                .claim_workspace_transaction_worker(
                    fixture.user,
                    fixture.project.id,
                    &current,
                    &next,
                    102,
                )
                .await
                .unwrap();
            assert!(
                store
                    .release_workspace_transaction_worker(
                        fixture.user,
                        fixture.project.id,
                        &scope.id,
                        &scope.generation,
                        &worker,
                        103
                    )
                    .await
                    .is_err()
            );
            assert_eq!(
                store
                    .workspace_transaction_scope(fixture.user, fixture.project.id, &scope.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .worker,
                Some(next.state())
            );
        }
    });
}
#[test]
fn cancellation_is_owned_and_revision_bound_and_cannot_cancel_after_first_intent() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let (fixture, scope, worker) = admitted(&store, mode).await;
            assert!(
                store
                    .advance_workspace_transaction(
                        fixture.other,
                        fixture.project.id,
                        &cancel(&scope),
                        101
                    )
                    .await
                    .is_err()
            );
            let mut wrong = cancel(&scope);
            wrong.generation = "f".repeat(32);
            assert!(
                store
                    .advance_workspace_transaction(fixture.user, fixture.project.id, &wrong, 101)
                    .await
                    .is_err()
            );
            wrong = cancel(&scope);
            wrong.revision += 1;
            assert!(
                store
                    .advance_workspace_transaction(fixture.user, fixture.project.id, &wrong, 101)
                    .await
                    .is_err()
            );
            let mut bad = worker.clone();
            bad.secret = "5".repeat(32);
            assert!(
                store
                    .advance_workspace_transaction_for_worker(
                        fixture.user,
                        fixture.project.id,
                        &cancel(&scope),
                        Some(&bad),
                        101
                    )
                    .await
                    .is_err()
            );
            let intent = AdvanceWorkspaceTransaction {
                progress: TransactionProgress {
                    phase: TransactionPhase::Applying,
                    ..Default::default()
                },
                ..cancel(&scope)
            };
            let applied = store
                .advance_workspace_transaction_for_worker(
                    fixture.user,
                    fixture.project.id,
                    &intent,
                    Some(&worker),
                    101,
                )
                .await
                .unwrap();
            let mut later = cancel(&scope);
            later.revision = applied.revision;
            assert!(
                store
                    .advance_workspace_transaction(fixture.user, fixture.project.id, &later, 102)
                    .await
                    .is_err()
            );
            assert_eq!(
                store
                    .workspace_transaction_scope(fixture.user, fixture.project.id, &scope.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .worker,
                Some(worker.state())
            );
        }
    });
}
#[test]
fn prepared_cancel_does_not_deserialize_or_rewrite_file_or_editor_snapshots() {
    block_on(async {
        let store = Store::new(RusqliteDb::open_in_memory().unwrap());
        let (fixture, scope, _worker) = admitted(&store, WorkspaceMode::Local).await;
        store.db.execute("UPDATE workspace_transactions SET plan='invalid file snapshot',editor='invalid editor snapshot' WHERE user_id=? AND project_id=? AND id=?",&[DbValue::Int(fixture.user.get()),DbValue::Int(fixture.project.id),DbValue::Text(scope.id.clone())]).await.unwrap();
        store
            .advance_workspace_transaction(fixture.user, fixture.project.id, &cancel(&scope), 101)
            .await
            .unwrap();
        let raw=store.db.execute("SELECT plan,editor,worker_digest FROM workspace_transactions WHERE user_id=? AND project_id=? AND id=?",&[DbValue::Int(fixture.user.get()),DbValue::Int(fixture.project.id),DbValue::Text(scope.id)]).await.unwrap();
        assert_eq!(raw.rows[0].get_text(0).unwrap(), "invalid file snapshot");
        assert_eq!(raw.rows[0].get_text(1).unwrap(), "invalid editor snapshot");
        assert_eq!(raw.rows[0].get_text(2).unwrap(), "");
    });
}
#[test]
fn legacy_unowned_zero_step_rollback_still_reaches_restored() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let fixture = fixture(&store, mode).await;
            let journal = journal(&store, &fixture);
            let prepared = journal.prepare(&fixture.plan).await.unwrap();
            let mut input = AdvanceWorkspaceTransaction {
                id: prepared.id,
                generation: prepared.generation,
                revision: prepared.revision,
                progress: TransactionProgress {
                    phase: TransactionPhase::Applying,
                    ..Default::default()
                },
            };
            input.revision = store
                .advance_workspace_transaction(fixture.user, fixture.project.id, &input, 101)
                .await
                .unwrap()
                .revision;
            input.progress = TransactionProgress {
                phase: TransactionPhase::Reverting,
                rollback_from: Some(0),
                ..Default::default()
            };
            input.revision = store
                .advance_workspace_transaction(fixture.user, fixture.project.id, &input, 102)
                .await
                .unwrap()
                .revision;
            input.progress = TransactionProgress::prepared_cancellation();
            store
                .advance_workspace_transaction(fixture.user, fixture.project.id, &input, 103)
                .await
                .unwrap();
        }
    });
}
