use super::*;
use openwebide_core::{
    plugins::{
        PluginPackage, RecordPlugin,
        execution::PluginExecutionContext,
        languages::{LanguageSettings, ProviderPreference},
    },
    workspace_transaction::{
        DelegateWorkspaceTransaction, DelegatedWorkspaceJournalOperation as Operation,
        DelegatedWorkspaceJournalRequest, WorkspaceJournalResponse, WorkspaceTransactionScope,
        WorkspaceTransactionWorker,
    },
};

fn worker(number: u128) -> WorkspaceTransactionWorker {
    WorkspaceTransactionWorker {
        id: format!("{number:032x}"),
        secret: format!("{:032x}", number + 100),
    }
}
async fn setup(
    store: &Store<RusqliteDb>,
    mode: WorkspaceMode,
) -> (
    Fixture,
    BeginWorkspaceTransaction,
    WorkspaceTransactionScope,
) {
    let mut fixture = fixture(store, mode).await;
    fixture.plan.root = "a".repeat(64);
    fixture.files.root = fixture.plan.root.clone();
    let mut input = input(&fixture);
    let review = input.language_review.as_mut().unwrap();
    store
        .record_plugin(
            fixture.user,
            &RecordPlugin {
                prepared: review.prepared.clone(),
                package: Some(Box::new(PluginPackage {
                    prepared: review.prepared.clone(),
                    skills: vec![],
                })),
                approved_capabilities: vec![],
                revision: None,
                update_policy: None,
            },
            100,
        )
        .await
        .unwrap();
    review.binding_revision = store
        .project_plugins(fixture.user, fixture.project.id)
        .await
        .unwrap()
        .into_iter()
        .find(|binding| binding.prepared.source == review.prepared.source)
        .unwrap()
        .revision;
    store
        .begin_workspace_transaction(fixture.user, fixture.project.id, &input, 100)
        .await
        .unwrap();
    let scope = store
        .workspace_transaction_scope(fixture.user, fixture.project.id, &input.id)
        .await
        .unwrap()
        .unwrap();
    (fixture, input, scope)
}
async fn grant(
    store: &Store<RusqliteDb>,
    fixture: &Fixture,
    input: &BeginWorkspaceTransaction,
    manual: bool,
    token: &str,
) {
    store
        .issue_plugin_context_grant(
            fixture.user,
            &PluginExecutionContext {
                project_id: Some(fixture.project.id),
                user_action: manual,
                ..Default::default()
            },
            &input.language_review.as_ref().unwrap().prepared,
            token,
            100,
        )
        .await
        .unwrap();
}

#[test]
fn a_review_bearing_transaction_cannot_claim_a_worker_without_native_review_and_current_authority()
{
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let fixture = fixture(&store, mode).await;
            let input = input(&fixture);
            store
                .begin_workspace_transaction(fixture.user, fixture.project.id, &input, 100)
                .await
                .unwrap();
            let scope = store
                .workspace_transaction_scope(fixture.user, fixture.project.id, &input.id)
                .await
                .unwrap()
                .unwrap();
            let worker = WorkspaceTransactionWorker {
                id: "b".repeat(32),
                secret: "c".repeat(32),
            };
            assert!(
                store
                    .claim_workspace_transaction_worker(
                        fixture.user,
                        fixture.project.id,
                        &scope,
                        &worker,
                        101,
                    )
                    .await
                    .is_err(),
                "A language review was admitted without native verification or current manual authority"
            );
            assert_eq!(
                store
                    .workspace_transaction_scope(fixture.user, fixture.project.id, &input.id)
                    .await
                    .unwrap(),
                Some(scope)
            );
        }
    });
}

#[test]
fn initial_admission_requires_the_exact_review_and_fresh_scoped_manual_provider_authority() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let (fixture, input, scope) = setup(&store, mode).await;
            let digest = input
                .language_review
                .as_ref()
                .unwrap()
                .fingerprint()
                .unwrap();
            let manual = "d".repeat(32);
            let background = "e".repeat(32);
            grant(&store, &fixture, &input, true, &manual).await;
            grant(&store, &fixture, &input, false, &background).await;
            for (verified, token, now) in [
                ("0".repeat(64), manual.clone(), 101),
                (digest.clone(), background, 101),
                (digest.clone(), "f".repeat(32), 101),
                (digest.clone(), manual.clone(), 100_000),
            ] {
                assert!(
                    store
                        .claim_workspace_transaction_worker_reviewed(
                            fixture.user,
                            fixture.project.id,
                            &scope,
                            &worker(1),
                            Some((&verified, &token)),
                            now
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
            }
            let mut settings = LanguageSettings::default();
            settings
                .defaults
                .providers
                .insert("custom".into(), ProviderPreference::Disabled);
            store
                .set_user_setting(
                    fixture.user,
                    "language_settings",
                    &serde_json::to_string(&settings).unwrap(),
                )
                .await
                .unwrap();
            assert!(
                store
                    .claim_workspace_transaction_worker_reviewed(
                        fixture.user,
                        fixture.project.id,
                        &scope,
                        &worker(1),
                        Some((&digest, &manual)),
                        101
                    )
                    .await
                    .is_err()
            );
            store
                .set_user_setting(fixture.user, "language_settings", "{}")
                .await
                .unwrap();
            let accepted = store
                .claim_workspace_transaction_worker_reviewed(
                    fixture.user,
                    fixture.project.id,
                    &scope,
                    &worker(1),
                    Some((&digest, &manual)),
                    102,
                )
                .await
                .unwrap();
            assert_eq!(
                accepted.accepted_language_review_digest.as_ref(),
                Some(&digest)
            );
            assert_eq!(accepted.worker, Some(worker(1).state()));
            let loaded = store
                .workspace_transaction(fixture.user, fixture.project.id, Some(&scope.id))
                .await
                .unwrap()
                .unwrap();
            assert!(accepted.matches_state(&loaded));
            assert!(!serde_json::to_string(&loaded).unwrap().contains(&manual));
        }
    });
}

#[test]
fn accepted_review_identity_survives_revocation_expiry_reopen_and_migration_replay_without_replaying_commands()
 {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let directory = tempfile::tempdir().unwrap();
            let database = directory.path().join("accepted.db");
            let store = Store::new(RusqliteDb::open(&database).unwrap());
            let (fixture, input, scope) = setup(&store, mode).await;
            let digest = input
                .language_review
                .as_ref()
                .unwrap()
                .fingerprint()
                .unwrap();
            let token = "d".repeat(32);
            grant(&store, &fixture, &input, true, &token).await;
            let accepted = store
                .claim_workspace_transaction_worker_reviewed(
                    fixture.user,
                    fixture.project.id,
                    &scope,
                    &worker(1),
                    Some((&digest, &token)),
                    101,
                )
                .await
                .unwrap();
            store
                .set_user_setting(fixture.user, "language_settings", "{bad configuration")
                .await
                .unwrap();
            store
                .db
                .execute("DELETE FROM plugin_execution_grants", &[])
                .await
                .unwrap();
            // Lost acceptance acknowledgements use the same credential/digest;
            // this must not require the now-revoked short-lived manual grant.
            assert_eq!(
                store
                    .claim_workspace_transaction_worker_reviewed(
                        fixture.user,
                        fixture.project.id,
                        &scope,
                        &worker(1),
                        Some((&digest, &token)),
                        100_000
                    )
                    .await
                    .unwrap(),
                accepted
            );
            let charged = store
                .db
                .execute("SELECT bytes FROM workspace_transactions", &[])
                .await
                .unwrap()
                .rows[0]
                .get_int(0)
                .unwrap();
            crate::migrations::apply_through(&store.db, 66)
                .await
                .unwrap();
            store
                .db
                .execute("PRAGMA user_version=66", &[])
                .await
                .unwrap();
            store.migrate().await.unwrap();
            assert_eq!(
                store
                    .db
                    .execute("SELECT bytes FROM workspace_transactions", &[])
                    .await
                    .unwrap()
                    .rows[0]
                    .get_int(0)
                    .unwrap(),
                charged
            );
            drop(store);
            let store = Store::new(RusqliteDb::open(&database).unwrap());
            store.migrate().await.unwrap();
            let restored = store
                .workspace_transaction_scope(fixture.user, fixture.project.id, &scope.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(restored, accepted);
            let successor = store
                .claim_workspace_transaction_worker(
                    fixture.user,
                    fixture.project.id,
                    &restored,
                    &worker(2),
                    100_001,
                )
                .await
                .unwrap();
            assert_eq!(successor.accepted_language_review_digest, Some(digest));
            assert!(
                store
                    .release_workspace_transaction_worker(
                        fixture.user,
                        fixture.project.id,
                        &scope.id,
                        &scope.generation,
                        &worker(1),
                        100_002
                    )
                    .await
                    .is_err()
            );
            assert!(
                store
                    .claim_workspace_transaction_worker(
                        fixture.user,
                        fixture.project.id,
                        &scope,
                        &worker(3),
                        100_002
                    )
                    .await
                    .is_err()
            );
            assert!(
                store
                    .db
                    .execute("SELECT 1 FROM language_actions", &[])
                    .await
                    .unwrap()
                    .rows
                    .is_empty()
            );
            assert_eq!(
                store
                    .workspace_transaction(fixture.user, fixture.project.id, Some(&scope.id))
                    .await
                    .unwrap()
                    .unwrap()
                    .language_review,
                input.language_review
            );
        }
    });
}

#[test]
fn paired_initial_admission_and_recovery_share_the_same_atomic_review_acceptance() {
    block_on(async {
        let store = Store::new(RusqliteDb::open_in_memory().unwrap());
        let (fixture, input, scope) = setup(&store, WorkspaceMode::Local).await;
        let digest = input
            .language_review
            .as_ref()
            .unwrap()
            .fingerprint()
            .unwrap();
        let token = "d".repeat(32);
        grant(&store, &fixture, &input, true, &token).await;
        let delegation = store
            .delegate_workspace_transaction(
                fixture.user,
                fixture.project.id,
                &scope.id,
                &DelegateWorkspaceTransaction {
                    generation: scope.generation.clone(),
                    revision: scope.revision,
                    worker: worker(1).delegation_key(),
                },
                101,
            )
            .await
            .unwrap();
        let mut request = DelegatedWorkspaceJournalRequest {
            delegation,
            worker: worker(1),
            operation: Operation::Admit {
                review_digest: None,
                language_grant: None,
            },
        };
        assert!(
            store
                .delegated_workspace_journal(&request, 102)
                .await
                .is_err()
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
        request.operation = Operation::Admit {
            review_digest: Some(digest.clone()),
            language_grant: Some(token),
        };
        let WorkspaceJournalResponse::WorkerClaimed { scope: accepted } = store
            .delegated_workspace_journal(&request, 103)
            .await
            .unwrap()
        else {
            panic!("Missing worker acceptance")
        };
        assert_eq!(
            accepted.accepted_language_review_digest,
            Some(digest.clone())
        );
        store
            .db
            .execute("DELETE FROM plugin_execution_grants", &[])
            .await
            .unwrap();
        store
            .delegated_workspace_journal(&request, 100_000)
            .await
            .unwrap();
        let next = store
            .delegate_workspace_transaction(
                fixture.user,
                fixture.project.id,
                &scope.id,
                &DelegateWorkspaceTransaction {
                    generation: scope.generation,
                    revision: scope.revision,
                    worker: worker(2).delegation_key(),
                },
                100_000,
            )
            .await
            .unwrap();
        let recovery = DelegatedWorkspaceJournalRequest {
            delegation: next,
            worker: worker(2),
            operation: Operation::Admit {
                review_digest: None,
                language_grant: None,
            },
        };
        let WorkspaceJournalResponse::WorkerClaimed { scope: recovered } = store
            .delegated_workspace_journal(&recovery, 100_001)
            .await
            .unwrap()
        else {
            panic!("Missing recovery worker")
        };
        assert_eq!(recovered.accepted_language_review_digest, Some(digest));
        assert!(
            store
                .delegated_workspace_journal(&request, 100_002)
                .await
                .is_err()
        );
    });
}

#[test]
fn provider_rebinding_and_wrong_project_grants_cannot_become_initial_review_authority() {
    use openwebide_core::plugins::ProjectPluginCommand;
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let (fixture, input, scope) = setup(&store, mode).await;
            let review = input.language_review.as_ref().unwrap();
            let digest = review.fingerprint().unwrap();
            let token = "d".repeat(32);
            grant(&store, &fixture, &input, true, &token).await;
            let other = store
                .create_project(
                    &NewProject {
                        name: "other-project".into(),
                        mode,
                        path: Some("other".into()),
                    },
                    fixture.user,
                    100,
                )
                .await
                .unwrap();
            let wrong_project = "e".repeat(32);
            store
                .issue_plugin_context_grant(
                    fixture.user,
                    &PluginExecutionContext {
                        user_action: true,
                        project_id: Some(other.id),
                        ..Default::default()
                    },
                    &review.prepared,
                    &wrong_project,
                    100,
                )
                .await
                .unwrap();
            assert!(
                store
                    .claim_workspace_transaction_worker_reviewed(
                        fixture.user,
                        fixture.project.id,
                        &scope,
                        &worker(1),
                        Some((&digest, &wrong_project)),
                        101
                    )
                    .await
                    .is_err()
            );
            let binding = store
                .project_plugins(fixture.user, fixture.project.id)
                .await
                .unwrap()
                .into_iter()
                .find(|binding| binding.prepared.source == review.prepared.source)
                .unwrap();
            let disabled = store
                .project_plugin_command(
                    fixture.user,
                    fixture.project.id,
                    &ProjectPluginCommand::Disable {
                        id: binding.id,
                        revision: binding.revision,
                    },
                    101,
                )
                .await
                .unwrap();
            assert!(
                store
                    .claim_workspace_transaction_worker_reviewed(
                        fixture.user,
                        fixture.project.id,
                        &scope,
                        &worker(1),
                        Some((&digest, &token)),
                        102
                    )
                    .await
                    .is_err()
            );
            let binding = disabled
                .into_iter()
                .find(|binding| binding.prepared.source == review.prepared.source)
                .unwrap();
            let installation = store
                .plugin_installations(fixture.user)
                .await
                .unwrap()
                .into_iter()
                .find(|installation| installation.prepared.source == review.prepared.source)
                .unwrap();
            store
                .project_plugin_command(
                    fixture.user,
                    fixture.project.id,
                    &ProjectPluginCommand::Enable {
                        package: Box::new(PluginPackage {
                            prepared: review.prepared.clone(),
                            skills: vec![],
                        }),
                        installation_revision: installation.revision,
                        revision: Some(binding.revision),
                    },
                    103,
                )
                .await
                .unwrap();
            // Restoring the same package does not restore its original binding.
            assert!(
                store
                    .claim_workspace_transaction_worker_reviewed(
                        fixture.user,
                        fixture.project.id,
                        &scope,
                        &worker(1),
                        Some((&digest, &token)),
                        104
                    )
                    .await
                    .is_err()
            );
            assert_eq!(
                store
                    .workspace_transaction_scope(fixture.user, fixture.project.id, &scope.id)
                    .await
                    .unwrap(),
                Some(scope)
            );
        }
    });
}

#[test]
fn accepted_identity_cannot_be_borrowed_by_changed_evidence_or_another_worker() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let (fixture, input, scope) = setup(&store, mode).await;
            let digest = input
                .language_review
                .as_ref()
                .unwrap()
                .fingerprint()
                .unwrap();
            let token = "d".repeat(32);
            grant(&store, &fixture, &input, true, &token).await;
            let accepted = store
                .claim_workspace_transaction_worker_reviewed(
                    fixture.user,
                    fixture.project.id,
                    &scope,
                    &worker(1),
                    Some((&digest, &token)),
                    101,
                )
                .await
                .unwrap();
            let mut forged = accepted.clone();
            forged.accepted_language_review_digest = Some("0".repeat(64));
            assert!(
                store
                    .claim_workspace_transaction_worker(
                        fixture.user,
                        fixture.project.id,
                        &forged,
                        &worker(2),
                        102
                    )
                    .await
                    .is_err()
            );
            store.db.execute("UPDATE workspace_transactions SET language_review=json_set(language_review,'$.input.request',900)", &[]).await.unwrap();
            assert!(
                store
                    .workspace_transaction(fixture.user, fixture.project.id, Some(&scope.id))
                    .await
                    .is_err()
            );
            // Both a successor and the original worker's lost-ack retry must
            // compare the complete immutable review to the accepted digest.
            assert!(
                store
                    .claim_workspace_transaction_worker(
                        fixture.user,
                        fixture.project.id,
                        &accepted,
                        &worker(2),
                        103
                    )
                    .await
                    .is_err()
            );
            assert!(
                store
                    .claim_workspace_transaction_worker_reviewed(
                        fixture.user,
                        fixture.project.id,
                        &scope,
                        &worker(1),
                        Some((&digest, &token)),
                        103
                    )
                    .await
                    .is_err()
            );
            assert_eq!(
                store
                    .workspace_transaction_scope(fixture.user, fixture.project.id, &scope.id)
                    .await
                    .unwrap(),
                Some(accepted)
            );
        }
    });
}

#[test]
fn unaccepted_review_cannot_advance_but_untouched_cancellation_never_needs_a_fresh_grant() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let (fixture, _, scope) = setup(&store, mode).await;
            let progress = AdvanceWorkspaceTransaction {
                id: scope.id.clone(),
                generation: scope.generation.clone(),
                revision: scope.revision,
                progress: TransactionProgress {
                    phase: TransactionPhase::Applying,
                    ..Default::default()
                },
            };
            assert!(
                store
                    .advance_workspace_transaction(fixture.user, fixture.project.id, &progress, 101)
                    .await
                    .is_err()
            );
            store
                .advance_workspace_transaction(
                    fixture.user,
                    fixture.project.id,
                    &AdvanceWorkspaceTransaction {
                        progress: TransactionProgress::prepared_cancellation(),
                        ..progress
                    },
                    102,
                )
                .await
                .unwrap();
            let cancelled = store
                .workspace_transaction_scope(fixture.user, fixture.project.id, &scope.id)
                .await
                .unwrap()
                .unwrap();
            let cleanup = store
                .claim_workspace_transaction_worker(
                    fixture.user,
                    fixture.project.id,
                    &cancelled,
                    &worker(2),
                    100_000,
                )
                .await
                .unwrap();
            assert!(cleanup.accepted_language_review_digest.is_none());
            assert_eq!(
                store
                    .workspace_transaction(fixture.user, fixture.project.id, Some(&scope.id))
                    .await
                    .unwrap()
                    .unwrap()
                    .journal
                    .progress,
                TransactionProgress::prepared_cancellation()
            );
        }
    });
}

#[test]
fn schema_66_upgrade_reserves_acceptance_without_accepting_existing_evidence() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let (fixture, input, _) = setup(&store, mode).await;
            let charged = store
                .db
                .execute("SELECT bytes FROM workspace_transactions", &[])
                .await
                .unwrap()
                .rows[0]
                .get_int(0)
                .unwrap();
            store.db.execute("ALTER TABLE workspace_transactions DROP COLUMN accepted_language_review_digest", &[]).await.unwrap();
            store
                .db
                .execute("UPDATE workspace_transactions SET bytes=bytes-64", &[])
                .await
                .unwrap();
            store
                .db
                .execute("PRAGMA user_version=66", &[])
                .await
                .unwrap();
            store.migrate().await.unwrap();
            let scope = store
                .workspace_transaction_scope(fixture.user, fixture.project.id, &input.id)
                .await
                .unwrap()
                .unwrap();
            assert!(scope.accepted_language_review_digest.is_none());
            assert!(scope.worker.is_none());
            assert_eq!(
                store
                    .db
                    .execute("SELECT bytes FROM workspace_transactions", &[])
                    .await
                    .unwrap()
                    .rows[0]
                    .get_int(0)
                    .unwrap(),
                charged
            );
            assert!(
                store
                    .claim_workspace_transaction_worker(
                        fixture.user,
                        fixture.project.id,
                        &scope,
                        &worker(1),
                        103
                    )
                    .await
                    .is_err()
            );
        }
    });
}

#[test]
fn protocol_two_accepted_evidence_survives_schema_68_reopen_and_successor_claim() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let directory = tempfile::tempdir().unwrap();
            let database = directory.path().join("archived.db");
            let store = Store::new(RusqliteDb::open(&database).unwrap());
            let (fixture, input, scope) = setup(&store, mode).await;
            let token = "d".repeat(32);
            grant(&store, &fixture, &input, true, &token).await;
            let digest = input
                .language_review
                .as_ref()
                .unwrap()
                .fingerprint()
                .unwrap();
            store
                .claim_workspace_transaction_worker_reviewed(
                    fixture.user,
                    fixture.project.id,
                    &scope,
                    &worker(1),
                    Some((&digest, &token)),
                    101,
                )
                .await
                .unwrap();
            // Model an already accepted schema-67 row with its exact original
            // protocol-2 body/digest. Migration must neither rewrite nor ask a
            // newer live verifier to authenticate that old service again.
            let mut archived = input.language_review.as_ref().unwrap().as_ref().clone();
            archived.input.version = 2;
            let body = serde_json::to_string(&archived).unwrap();
            let old_digest = archived.fingerprint().unwrap();
            store.db.execute("UPDATE workspace_transactions SET language_review=?,accepted_language_review_digest=?",&[crate::DbValue::Text(body.clone()),crate::DbValue::Text(old_digest.clone())]).await.unwrap();
            store
                .db
                .execute("DELETE FROM plugin_execution_grants", &[])
                .await
                .unwrap();
            store
                .set_user_setting(fixture.user, "language_settings", "{expired configuration")
                .await
                .unwrap();
            store
                .db
                .execute("PRAGMA user_version=67", &[])
                .await
                .unwrap();
            drop(store);
            let store = Store::new(RusqliteDb::open(&database).unwrap());
            store.migrate().await.unwrap();
            let loaded = store
                .workspace_transaction(fixture.user, fixture.project.id, None)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                serde_json::to_string(loaded.language_review.as_ref().unwrap()).unwrap(),
                body
            );
            let scope = store
                .workspace_transaction_scope(fixture.user, fixture.project.id, &scope.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                scope.accepted_language_review_digest,
                Some(old_digest.clone())
            );
            let successor = store
                .claim_workspace_transaction_worker(
                    fixture.user,
                    fixture.project.id,
                    &scope,
                    &worker(2),
                    100_000,
                )
                .await
                .unwrap();
            assert_eq!(successor.accepted_language_review_digest, Some(old_digest));
        }
    });
}
