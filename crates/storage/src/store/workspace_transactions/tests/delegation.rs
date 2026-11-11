use super::*;
mod preflight;
use openwebide_core::workspace_transaction::{
    DelegateWorkspaceTransaction, DelegatedWorkspaceJournalOperation as Operation,
    DelegatedWorkspaceJournalRequest, WorkspaceJournalDelegation, WorkspaceJournalResponse,
    WorkspaceTransactionScope, WorkspaceTransactionWorker,
};
fn worker(value: u128) -> WorkspaceTransactionWorker {
    WorkspaceTransactionWorker {
        id: format!("{value:032x}"),
        secret: format!("{:032x}", value + 100),
    }
}
async fn setup(
    store: &Store<RusqliteDb>,
    mode: WorkspaceMode,
) -> (Fixture, WorkspaceTransactionScope) {
    let mut fixture = fixture(store, mode).await;
    fixture.plan.root = "a".repeat(64);
    fixture.files.root = fixture.plan.root.clone();
    let journal = journal(store, &fixture);
    journal.prepare(&fixture.plan).await.unwrap();
    let scope = store
        .workspace_transaction_scope(fixture.user, fixture.project.id, &journal.id)
        .await
        .unwrap()
        .unwrap();
    (fixture, scope)
}
async fn grant(
    store: &Store<RusqliteDb>,
    fixture: &Fixture,
    scope: &WorkspaceTransactionScope,
    worker: &WorkspaceTransactionWorker,
    now: i64,
) -> WorkspaceJournalDelegation {
    store
        .delegate_workspace_transaction(
            fixture.user,
            fixture.project.id,
            &scope.id,
            &DelegateWorkspaceTransaction {
                generation: scope.generation.clone(),
                revision: scope.revision,
                worker: worker.delegation_key(),
            },
            now,
        )
        .await
        .unwrap()
}
fn call(
    delegation: &WorkspaceJournalDelegation,
    worker: &WorkspaceTransactionWorker,
    operation: Operation,
) -> DelegatedWorkspaceJournalRequest {
    DelegatedWorkspaceJournalRequest {
        delegation: delegation.clone(),
        worker: worker.clone(),
        operation,
    }
}
#[test]
fn mutation_authority_requires_a_durable_execution_phase_and_stops_at_terminal() {
    block_on(async {
        let store = Store::new(RusqliteDb::open_in_memory().unwrap());
        let (fixture, scope) = setup(&store, WorkspaceMode::Local).await;
        let actor = worker(1);
        let token = grant(&store, &fixture, &scope, &actor, 100).await;
        store
            .delegated_workspace_journal(
                &call(
                    &token,
                    &actor,
                    Operation::Admit {
                        review_digest: None,
                        language_grant: None,
                    },
                ),
                101,
            )
            .await
            .unwrap();
        assert!(
            store
                .delegated_workspace_journal(&call(&token, &actor, Operation::MutationScope), 101)
                .await
                .is_err()
        );
        let mut revision = scope.revision;
        for (progress, allowed) in [
            (
                TransactionProgress {
                    phase: TransactionPhase::Applying,
                    ..Default::default()
                },
                true,
            ),
            (
                TransactionProgress {
                    phase: TransactionPhase::Reverting,
                    rollback_from: Some(0),
                    ..Default::default()
                },
                true,
            ),
            (TransactionProgress::prepared_cancellation(), false),
        ] {
            let result = store
                .delegated_workspace_journal(
                    &call(&token, &actor, Operation::Advance { revision, progress }),
                    102,
                )
                .await
                .unwrap();
            let WorkspaceJournalResponse::Advanced { response } = result else {
                panic!()
            };
            revision = response.revision;
            assert_eq!(
                store
                    .delegated_workspace_journal(
                        &call(&token, &actor, Operation::MutationScope),
                        102
                    )
                    .await
                    .is_ok(),
                allowed
            );
        }
        assert!(
            store
                .workspace_transaction_scope(fixture.user, fixture.project.id, &scope.id)
                .await
                .unwrap()
                .unwrap()
                .worker
                .unwrap()
                .active
        );
    });
}
#[test]
fn a_full_active_grant_quota_still_allows_one_bounded_recovery_bootstrap() {
    block_on(async {
        let store = Store::new(RusqliteDb::open_in_memory().unwrap());
        let (fixture, original) = setup(&store, WorkspaceMode::Local).await;
        let first = worker(1);
        let original_grant = grant(&store, &fixture, &original, &first, 100).await;
        store
            .delegated_workspace_journal(
                &call(
                    &original_grant,
                    &first,
                    Operation::Admit {
                        review_digest: None,
                        language_grant: None,
                    },
                ),
                101,
            )
            .await
            .unwrap();
        let editor = store
            .editor_recovery(fixture.user, fixture.project.id)
            .await
            .unwrap();
        for number in 2..=129_u128 {
            let project = store
                .create_project(
                    &NewProject {
                        name: format!("quota-{number}"),
                        mode: WorkspaceMode::Local,
                        path: None,
                    },
                    fixture.user,
                    100,
                )
                .await
                .unwrap();
            store
                .save_editor_recovery(fixture.user, project.id, 0, &editor.state)
                .await
                .unwrap();
            let mut plan = fixture.plan.clone();
            plan.root = format!("{number:064x}");
            let prepared = store
                .begin_workspace_transaction(
                    fixture.user,
                    project.id,
                    &BeginWorkspaceTransaction {
                        language_review: None,
                        id: original.id.clone(),
                        plan,
                        editor: fixture.editor.clone(),
                    },
                    100,
                )
                .await
                .unwrap();
            let actor = worker(number);
            let token = store
                .delegate_workspace_transaction(
                    fixture.user,
                    project.id,
                    &original.id,
                    &DelegateWorkspaceTransaction {
                        generation: prepared.journal.generation,
                        revision: 1,
                        worker: actor.delegation_key(),
                    },
                    100,
                )
                .await;
            if number == 129 {
                assert!(token.is_err());
                continue;
            }
            let token = token.unwrap();
            store
                .delegated_workspace_journal(
                    &call(
                        &token,
                        &actor,
                        Operation::Admit {
                            review_digest: None,
                            language_grant: None,
                        },
                    ),
                    101,
                )
                .await
                .unwrap();
        }
        let count = || async {
            store
                .db
                .execute(
                    "SELECT COUNT(*) FROM workspace_transaction_delegations",
                    &[],
                )
                .await
                .unwrap()
                .rows[0]
                .get_int(0)
                .unwrap()
        };
        assert_eq!(count().await, 128);
        let recovery = worker(1000);
        let request = DelegateWorkspaceTransaction {
            generation: original.generation.clone(),
            revision: original.revision,
            worker: recovery.delegation_key(),
        };
        // All 128 credentials remain admitted far beyond bootstrap expiry.
        // Replacing one must not require expiring or stealing an active worker.
        let token = store
            .delegate_workspace_transaction(
                fixture.user,
                fixture.project.id,
                &original.id,
                &request,
                10000,
            )
            .await
            .unwrap();
        assert_eq!(count().await, 129);
        let another = DelegateWorkspaceTransaction {
            worker: worker(2000).delegation_key(),
            ..request.clone()
        };
        assert!(
            store
                .delegate_workspace_transaction(
                    fixture.user,
                    fixture.project.id,
                    &original.id,
                    &another,
                    10000
                )
                .await
                .is_err()
        );
        // A lost bootstrap reply can be replaced without taking another slot.
        let retry = store
            .delegate_workspace_transaction(
                fixture.user,
                fixture.project.id,
                &original.id,
                &request,
                10000,
            )
            .await
            .unwrap();
        assert_eq!(count().await, 129);
        assert!(
            store
                .delegated_workspace_journal(
                    &call(
                        &token,
                        &recovery,
                        Operation::Admit {
                            review_digest: None,
                            language_grant: None
                        }
                    ),
                    10001
                )
                .await
                .is_err()
        );
        store
            .delegated_workspace_journal(
                &call(
                    &retry,
                    &recovery,
                    Operation::Admit {
                        review_digest: None,
                        language_grant: None,
                    },
                ),
                10001,
            )
            .await
            .unwrap();
        assert_eq!(count().await, 128);
        assert!(
            store
                .delegated_workspace_journal(
                    &call(&original_grant, &first, Operation::Release),
                    10001
                )
                .await
                .is_err()
        );
    });
}
#[test]
fn terminal_grants_are_revoked_without_claiming_their_workers_are_quiescent() {
    block_on(async {
        let store = Store::new(RusqliteDb::open_in_memory().unwrap());
        let (fixture, original) = setup(&store, WorkspaceMode::Local).await;
        let actor = worker(1);
        let token = grant(&store, &fixture, &original, &actor, 100).await;
        store
            .delegated_workspace_journal(
                &call(
                    &token,
                    &actor,
                    Operation::Admit {
                        review_digest: None,
                        language_grant: None,
                    },
                ),
                101,
            )
            .await
            .unwrap();
        store
            .delegated_workspace_journal(
                &call(
                    &token,
                    &actor,
                    Operation::Advance {
                        revision: original.revision,
                        progress: TransactionProgress::prepared_cancellation(),
                    },
                ),
                102,
            )
            .await
            .unwrap();
        let next = store
            .begin_workspace_transaction(
                fixture.user,
                fixture.project.id,
                &BeginWorkspaceTransaction {
                    language_review: None,
                    id: "b".repeat(32),
                    plan: fixture.plan.clone(),
                    editor: fixture.editor.clone(),
                },
                103,
            )
            .await
            .unwrap();
        let next_token = store
            .delegate_workspace_transaction(
                fixture.user,
                fixture.project.id,
                &next.journal.id,
                &DelegateWorkspaceTransaction {
                    generation: next.journal.generation,
                    revision: next.journal.revision,
                    worker: worker(2).delegation_key(),
                },
                104,
            )
            .await
            .unwrap();
        let rows = store
            .db
            .execute("SELECT id FROM workspace_transaction_delegations", &[])
            .await
            .unwrap();
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(rows.rows[0].get_text(0).unwrap(), next_token.id);
        assert!(
            store
                .delegated_workspace_journal(&call(&token, &actor, Operation::Scope), 104)
                .await
                .is_err()
        );
        // Token revocation is not worker release: only native exclusion may
        // establish quiescence and replace/release that durable worker identity.
        assert!(
            store
                .workspace_transaction_scope(fixture.user, fixture.project.id, &original.id)
                .await
                .unwrap()
                .unwrap()
                .worker
                .unwrap()
                .active
        );
    });
}
#[test]
fn local_delegation_requires_account_and_private_worker_then_fences_every_successor() {
    block_on(async {
        let store = Store::new(RusqliteDb::open_in_memory().unwrap());
        let (fixture, original) = setup(&store, WorkspaceMode::Local).await;
        let first = worker(1);
        let second = worker(2);
        let input = DelegateWorkspaceTransaction {
            generation: original.generation.clone(),
            revision: original.revision,
            worker: first.delegation_key(),
        };
        assert!(
            store
                .delegate_workspace_transaction(
                    fixture.other,
                    fixture.project.id,
                    &original.id,
                    &input,
                    10
                )
                .await
                .is_err()
        );
        let delegation = grant(&store, &fixture, &original, &first, 10).await;
        assert!(!format!("{delegation:?}").contains(&delegation.secret));
        let stored = store
            .db
            .execute(
                "SELECT secret_digest,worker_digest FROM workspace_transaction_delegations",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(
            stored.rows[0].get_text(0).unwrap(),
            openwebide_core::plugins::content_digest(delegation.secret.as_bytes())
        );
        assert_eq!(stored.rows[0].get_text(1).unwrap(), first.secret_digest());
        for operation in [
            Operation::Scope,
            Operation::Advance {
                revision: 1,
                progress: TransactionProgress::default(),
            },
            Operation::Release,
        ] {
            assert!(
                store
                    .delegated_workspace_journal(&call(&delegation, &first, operation), 11)
                    .await
                    .is_err()
            );
        }
        for field in [0, 1] {
            let mut wrong = call(
                &delegation,
                &first,
                Operation::Admit {
                    review_digest: None,
                    language_grant: None,
                },
            );
            if field == 0 {
                wrong.delegation.user += 1;
            } else {
                wrong.delegation.project += 1;
            }
            assert!(store.delegated_workspace_journal(&wrong, 11).await.is_err());
        }
        let mut wrong = call(
            &delegation,
            &first,
            Operation::Admit {
                review_digest: None,
                language_grant: None,
            },
        );
        wrong.worker.secret = second.secret.clone();
        assert!(store.delegated_workspace_journal(&wrong, 11).await.is_err());
        let mut wrong = call(
            &delegation,
            &first,
            Operation::Admit {
                review_digest: None,
                language_grant: None,
            },
        );
        wrong.delegation.scope.root_identity = "another-folder".into();
        assert!(store.delegated_workspace_journal(&wrong, 11).await.is_err());
        assert!(matches!(
            store
                .delegated_workspace_journal(
                    &call(
                        &delegation,
                        &first,
                        Operation::Admit {
                            review_digest: None,
                            language_grant: None
                        }
                    ),
                    12
                )
                .await
                .unwrap(),
            WorkspaceJournalResponse::WorkerClaimed { .. }
        ));
        // Expiry governs only unused bootstraps, never an admitted worker.
        let advanced = store
            .delegated_workspace_journal(
                &call(
                    &delegation,
                    &first,
                    Operation::Advance {
                        revision: 1,
                        progress: TransactionProgress {
                            phase: TransactionPhase::Applying,
                            ..TransactionProgress::default()
                        },
                    },
                ),
                10000,
            )
            .await
            .unwrap();
        assert!(
            matches!(advanced,WorkspaceJournalResponse::Advanced {response} if response.revision==2 && response.editor.is_none())
        );
        let current = store
            .workspace_transaction_scope(fixture.user, fixture.project.id, &original.id)
            .await
            .unwrap()
            .unwrap();
        let successor = grant(&store, &fixture, &current, &second, 10001).await;
        store
            .delegated_workspace_journal(
                &call(
                    &successor,
                    &second,
                    Operation::Admit {
                        review_digest: None,
                        language_grant: None,
                    },
                ),
                10002,
            )
            .await
            .unwrap();
        for operation in [
            Operation::Admit {
                review_digest: None,
                language_grant: None,
            },
            Operation::Scope,
            Operation::Advance {
                revision: 2,
                progress: TransactionProgress::default(),
            },
            Operation::Release,
        ] {
            assert!(
                store
                    .delegated_workspace_journal(&call(&delegation, &first, operation), 10003)
                    .await
                    .is_err()
            );
        }
        store
            .delegated_workspace_journal(&call(&successor, &second, Operation::Release), 10004)
            .await
            .unwrap();
        store
            .delegated_workspace_journal(&call(&successor, &second, Operation::Release), 10005)
            .await
            .unwrap();
        assert!(
            store
                .delegated_workspace_journal(&call(&successor, &second, Operation::Scope), 10006)
                .await
                .is_err()
        );
        assert!(
            store
                .delegated_workspace_journal(
                    &call(
                        &successor,
                        &second,
                        Operation::Admit {
                            review_digest: None,
                            language_grant: None
                        }
                    ),
                    10006
                )
                .await
                .is_err()
        );
    });
}

#[test]
fn expired_changed_replaced_and_wrong_mode_bootstraps_never_acquire_a_worker() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let (fixture, scope) = setup(&store, mode).await;
            let first = worker(1);
            let request = DelegateWorkspaceTransaction {
                generation: scope.generation.clone(),
                revision: scope.revision,
                worker: first.delegation_key(),
            };
            if mode == WorkspaceMode::Remote {
                assert!(
                    store
                        .delegate_workspace_transaction(
                            fixture.user,
                            fixture.project.id,
                            &scope.id,
                            &request,
                            10
                        )
                        .await
                        .is_err()
                );
                continue;
            }
            let expired = grant(&store, &fixture, &scope, &first, 10).await;
            assert!(
                store
                    .delegated_workspace_journal(
                        &call(
                            &expired,
                            &first,
                            Operation::Admit {
                                review_digest: None,
                                language_grant: None
                            }
                        ),
                        55
                    )
                    .await
                    .is_err()
            );
            let replaced = grant(&store, &fixture, &scope, &first, 60).await;
            let latest = grant(&store, &fixture, &scope, &first, 61).await;
            assert!(
                store
                    .delegated_workspace_journal(
                        &call(
                            &replaced,
                            &first,
                            Operation::Admit {
                                review_digest: None,
                                language_grant: None
                            }
                        ),
                        62
                    )
                    .await
                    .is_err()
            );
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
                    .delegated_workspace_journal(
                        &call(
                            &latest,
                            &first,
                            Operation::Admit {
                                review_digest: None,
                                language_grant: None
                            }
                        ),
                        62
                    )
                    .await
                    .is_err()
            );
            let scope = store
                .workspace_transaction_scope(fixture.user, fixture.project.id, &scope.id)
                .await
                .unwrap()
                .unwrap();
            assert!(scope.worker.is_none());
        }
    });
}

#[test]
fn delegation_activation_is_durable_bounded_and_independent_of_large_snapshots() {
    block_on(async {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("delegated.db");
        let first = worker(1);
        let (delegation, user, project) = {
            let store = Store::new(RusqliteDb::open(&path).unwrap());
            let (fixture, scope) = setup(&store, WorkspaceMode::Local).await;
            let delegation = grant(&store, &fixture, &scope, &first, 10).await;
            // Admission needs only the compact durable identity.
            store
                .db
                .execute(
                    "UPDATE workspace_transactions SET plan='invalid',editor='invalid'",
                    &[],
                )
                .await
                .unwrap();
            store
                .delegated_workspace_journal(
                    &call(
                        &delegation,
                        &first,
                        Operation::Admit {
                            review_digest: None,
                            language_grant: None,
                        },
                    ),
                    11,
                )
                .await
                .unwrap();
            (delegation, fixture.user, fixture.project.id)
        };
        let store = Store::new(RusqliteDb::open(&path).unwrap());
        store.migrate().await.unwrap();
        assert!(matches!(
            store
                .delegated_workspace_journal(
                    &call(&delegation, &first, Operation::Scope),
                    i64::MAX / 2
                )
                .await
                .unwrap(),
            WorkspaceJournalResponse::Scope { .. }
        ));
        let scope = store
            .workspace_transaction_scope(user, project, &delegation.scope.id)
            .await
            .unwrap()
            .unwrap();
        for index in 2..=129 {
            store
                .delegate_workspace_transaction(
                    user,
                    project,
                    &scope.id,
                    &DelegateWorkspaceTransaction {
                        generation: scope.generation.clone(),
                        revision: scope.revision,
                        worker: worker(index).delegation_key(),
                    },
                    20,
                )
                .await
                .unwrap();
        }
        assert!(
            store
                .delegate_workspace_transaction(
                    user,
                    project,
                    &scope.id,
                    &DelegateWorkspaceTransaction {
                        generation: scope.generation.clone(),
                        revision: scope.revision,
                        worker: worker(130).delegation_key()
                    },
                    20
                )
                .await
                .is_err()
        );
        assert_eq!(
            store
                .db
                .execute(
                    "SELECT COUNT(*) FROM workspace_transaction_delegations",
                    &[]
                )
                .await
                .unwrap()
                .rows[0]
                .get_int(0)
                .unwrap(),
            129
        );
    });
}

#[test]
fn worker_admission_and_bootstrap_activation_commit_or_rollback_together() {
    block_on(async {
        let store = Store::new(RusqliteDb::open_in_memory().unwrap());
        let (fixture, scope) = setup(&store, WorkspaceMode::Local).await;
        let first = worker(1);
        let delegation = grant(&store, &fixture, &scope, &first, 10).await;
        store.db.execute("CREATE TRIGGER fail_delegation_activation BEFORE UPDATE OF admitted ON workspace_transaction_delegations WHEN NEW.admitted=1 BEGIN SELECT RAISE(ABORT,'activation unavailable'); END",&[]).await.unwrap();
        assert!(
            store
                .delegated_workspace_journal(
                    &call(
                        &delegation,
                        &first,
                        Operation::Admit {
                            review_digest: None,
                            language_grant: None
                        }
                    ),
                    11
                )
                .await
                .is_err()
        );
        assert_eq!(
            store
                .workspace_transaction_scope(fixture.user, fixture.project.id, &scope.id)
                .await
                .unwrap(),
            Some(scope.clone())
        );
        assert_eq!(
            store
                .db
                .execute(
                    "SELECT admitted FROM workspace_transaction_delegations",
                    &[]
                )
                .await
                .unwrap()
                .rows[0]
                .get_int(0)
                .unwrap(),
            0
        );
        store
            .db
            .execute("DROP TRIGGER fail_delegation_activation", &[])
            .await
            .unwrap();
        store
            .delegated_workspace_journal(
                &call(
                    &delegation,
                    &first,
                    Operation::Admit {
                        review_digest: None,
                        language_grant: None,
                    },
                ),
                12,
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .workspace_transaction_scope(fixture.user, fixture.project.id, &scope.id)
                .await
                .unwrap()
                .unwrap()
                .worker,
            Some(first.state())
        );
    });
}
