use super::*;
use openwebide_core::workspace_transaction::{
    WorkspaceTransactionScope, WorkspaceTransactionWorker,
};

fn worker(value: u128) -> WorkspaceTransactionWorker {
    WorkspaceTransactionWorker {
        id: format!("{value:032x}"),
        secret: format!("{:032x}", value + 100),
    }
}
async fn prepared(
    store: &Store<RusqliteDb>,
    mode: WorkspaceMode,
) -> (Fixture, WorkspaceTransactionScope) {
    let fixture = fixture(store, mode).await;
    let journal = journal(store, &fixture);
    journal.prepare(&fixture.plan).await.unwrap();
    let scope = store
        .workspace_transaction_scope(fixture.user, fixture.project.id, &journal.id)
        .await
        .unwrap()
        .unwrap();
    (fixture, scope)
}
fn advance(scope: &WorkspaceTransactionScope) -> AdvanceWorkspaceTransaction {
    AdvanceWorkspaceTransaction {
        id: scope.id.clone(),
        generation: scope.generation.clone(),
        revision: scope.revision,
        progress: TransactionProgress {
            phase: TransactionPhase::Applying,
            ..TransactionProgress::default()
        },
    }
}

#[test]
fn delayed_workers_cannot_advance_release_or_reclaim_a_new_owner_in_either_mode() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let (fixture, original) = prepared(&store, mode).await;
            let first = worker(1);
            let second = worker(2);
            let claim = store
                .claim_workspace_transaction_worker(
                    fixture.user,
                    fixture.project.id,
                    &original,
                    &first,
                    10,
                )
                .await
                .unwrap();
            assert_eq!(claim.worker, Some(first.state()));
            assert_eq!(
                store
                    .claim_workspace_transaction_worker(
                        fixture.user,
                        fixture.project.id,
                        &original,
                        &first,
                        11
                    )
                    .await
                    .unwrap(),
                claim
            );
            assert!(
                store
                    .claim_workspace_transaction_worker(
                        fixture.other,
                        fixture.project.id,
                        &original,
                        &second,
                        12
                    )
                    .await
                    .is_err()
            );
            for credential in [
                None,
                Some(second.clone()),
                Some(WorkspaceTransactionWorker {
                    id: first.id.clone(),
                    secret: second.secret.clone(),
                }),
            ] {
                assert!(matches!(
                    store
                        .advance_workspace_transaction_for_worker(
                            fixture.user,
                            fixture.project.id,
                            &advance(&claim),
                            credential.as_ref(),
                            13
                        )
                        .await,
                    Err(StorageError::Conflict(_))
                ));
            }
            let updated = store
                .advance_workspace_transaction_for_worker(
                    fixture.user,
                    fixture.project.id,
                    &advance(&claim),
                    Some(&first),
                    14,
                )
                .await
                .unwrap();
            let mut current = claim.clone();
            current.revision = updated.revision;
            // The runtime must obtain physical exclusion before this replacement.
            let replaced = store
                .claim_workspace_transaction_worker(
                    fixture.user,
                    fixture.project.id,
                    &current,
                    &second,
                    15,
                )
                .await
                .unwrap();
            assert!(
                store
                    .claim_workspace_transaction_worker(
                        fixture.user,
                        fixture.project.id,
                        &original,
                        &first,
                        16
                    )
                    .await
                    .is_err()
            );
            assert!(
                store
                    .release_workspace_transaction_worker(
                        fixture.user,
                        fixture.project.id,
                        &original.id,
                        &original.generation,
                        &first,
                        17
                    )
                    .await
                    .is_err()
            );
            assert!(
                store
                    .advance_workspace_transaction_for_worker(
                        fixture.user,
                        fixture.project.id,
                        &advance(&replaced),
                        Some(&first),
                        18
                    )
                    .await
                    .is_err()
            );
            let response = store
                .advance_workspace_transaction_for_worker(
                    fixture.user,
                    fixture.project.id,
                    &advance(&replaced),
                    Some(&second),
                    19,
                )
                .await
                .unwrap();
            assert_eq!(response.revision, replaced.revision + 1);
            store
                .release_workspace_transaction_worker(
                    fixture.user,
                    fixture.project.id,
                    &original.id,
                    &original.generation,
                    &second,
                    20,
                )
                .await
                .unwrap();
            store
                .release_workspace_transaction_worker(
                    fixture.user,
                    fixture.project.id,
                    &original.id,
                    &original.generation,
                    &second,
                    21,
                )
                .await
                .unwrap();
            let released = store
                .workspace_transaction_scope(fixture.user, fixture.project.id, &original.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(released.worker.as_ref().unwrap().id, second.id);
            assert!(!released.worker.as_ref().unwrap().active);
            assert!(
                store
                    .claim_workspace_transaction_worker(
                        fixture.user,
                        fixture.project.id,
                        &released,
                        &second,
                        22
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
                        &second,
                        23
                    )
                    .await
                    .is_err()
            );
            assert!(
                store
                    .advance_workspace_transaction(
                        fixture.user,
                        fixture.project.id,
                        &advance(&released),
                        24
                    )
                    .await
                    .is_err()
            );
            store
                .claim_workspace_transaction_worker(
                    fixture.user,
                    fixture.project.id,
                    &released,
                    &worker(3),
                    25,
                )
                .await
                .unwrap();
        }
    });
}

#[test]
fn admission_is_bound_to_record_identity_and_does_not_disclose_or_load_snapshots() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let (fixture, original) = prepared(&store, mode).await;
            let owner = worker(7);
            let claimed = store
                .claim_workspace_transaction_worker(
                    fixture.user,
                    fixture.project.id,
                    &original,
                    &owner,
                    10,
                )
                .await
                .unwrap();
            let serialized = serde_json::to_string(&claimed).unwrap();
            assert!(!serialized.contains(&owner.secret));
            assert!(!format!("{owner:?}").contains(&owner.secret));
            let stored = store
                .db
                .execute("SELECT worker_digest FROM workspace_transactions", &[])
                .await
                .unwrap();
            assert_eq!(stored.rows[0].get_text(0).unwrap(), owner.secret_digest());
            store.db.execute("UPDATE workspace_transactions SET plan='corrupt snapshot',editor='corrupt snapshot'",&[]).await.unwrap();
            assert!(matches!(
                store
                    .advance_workspace_transaction_for_worker(
                        fixture.user,
                        fixture.project.id,
                        &advance(&claimed),
                        Some(&worker(8)),
                        11
                    )
                    .await,
                Err(StorageError::Conflict(_))
            ));
            // Compact ownership operations never deserialize either large payload.
            store
                .release_workspace_transaction_worker(
                    fixture.user,
                    fixture.project.id,
                    &original.id,
                    &original.generation,
                    &owner,
                    12,
                )
                .await
                .unwrap();
            let released = store
                .workspace_transaction_scope(fixture.user, fixture.project.id, &original.id)
                .await
                .unwrap()
                .unwrap();
            store
                .claim_workspace_transaction_worker(
                    fixture.user,
                    fixture.project.id,
                    &released,
                    &worker(8),
                    13,
                )
                .await
                .unwrap();
            store
                .db
                .execute(
                    "UPDATE workspace_transactions SET generation=lower(hex(randomblob(16)))",
                    &[],
                )
                .await
                .unwrap();
            assert!(
                store
                    .claim_workspace_transaction_worker(
                        fixture.user,
                        fixture.project.id,
                        &released,
                        &owner,
                        14
                    )
                    .await
                    .is_err()
            );
            assert!(
                store
                    .release_workspace_transaction_worker(
                        fixture.user,
                        fixture.project.id,
                        &released.id,
                        &released.generation,
                        &worker(8),
                        15
                    )
                    .await
                    .is_err()
            );
        }
    });
}

#[test]
fn active_workers_survive_database_reopen_and_never_expire_by_age() {
    block_on(async {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.db");
        let owner = worker(1);
        let (user, project, scope) = {
            let store = Store::new(RusqliteDb::open(&path).unwrap());
            let (fixture, original) = prepared(&store, WorkspaceMode::Remote).await;
            let scope = store
                .claim_workspace_transaction_worker(
                    fixture.user,
                    fixture.project.id,
                    &original,
                    &owner,
                    1,
                )
                .await
                .unwrap();
            (fixture.user, fixture.project.id, scope)
        };
        let store = Store::new(RusqliteDb::open(&path).unwrap());
        store.migrate().await.unwrap();
        assert_eq!(
            store
                .workspace_transaction_scope(user, project, &scope.id)
                .await
                .unwrap(),
            Some(scope.clone())
        );
        assert!(
            store
                .advance_workspace_transaction(user, project, &advance(&scope), i64::MAX / 2)
                .await
                .is_err()
        );
        assert_eq!(
            store
                .workspace_transaction_scope(user, project, &scope.id)
                .await
                .unwrap(),
            Some(scope.clone())
        );
        store
            .advance_workspace_transaction_for_worker(
                user,
                project,
                &advance(&scope),
                Some(&owner),
                i64::MAX / 2,
            )
            .await
            .unwrap();
    });
}
