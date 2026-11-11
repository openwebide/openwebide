use super::*;
use openwebide_core::{
    plugins::languages::{
        intelligence::{
            self, AnalysisContext, Position, Query, Request,
            results::{WorkspaceChange, WorkspaceEdit},
        },
        source_digest,
    },
    workspace_transaction::LanguageReview,
};
use serde_json::json;
mod admission;

fn review(fixture: &Fixture) -> LanguageReview {
    let mut prepared = openwebide_core::plugins::testing::receipt();
    prepared.manifest.compatibility.plugin_api = 9;
    prepared.manifest.executable = None;
    prepared.manifest.contributions.skills.clear();
    prepared.manifest.contributions.tools.clear();
    prepared.manifest.contributions.languages = serde_json::from_value(json!([{
        "id":"custom","name":"Custom","extensions":["rs"],
        "features":{"languageServer":{"command":"custom-lsp"}}
    }]))
    .unwrap();
    let source = fixture.editor.before[0]
        .document
        .as_ref()
        .unwrap()
        .text
        .to_string();
    let result = serde_json::to_value(WorkspaceEdit {
        files: vec![],
        changes: fixture
            .plan
            .operations
            .iter()
            .cloned()
            .map(|operation| WorkspaceChange::File { operation })
            .collect(),
    })
    .unwrap();
    LanguageReview {
        prepared,
        binding_revision: 1,
        input: Request {
            version: intelligence::VERSION,
            client: "original-client".into(),
            document: "original-document".into(),
            request: 11,
            revision: 7,
            language: "custom".into(),
            path: "a.rs".into(),
            source_digest: source_digest(&source),
            source,
            query: Query::Rename {
                position: Position::default(),
                new_name: "new".into(),
            },
        },
        context: AnalysisContext {
            identities: vec![],
            revisions: vec![],
            files: vec![],
            server: "original-service".into(),
            result_digest: openwebide_core::plugins::content_digest(
                &serde_json::to_vec(&result).unwrap(),
            ),
            proof: "opaque.signature".into(),
        },
        result,
        transaction_digest: LanguageReview::transaction_digest(&fixture.plan, &fixture.editor)
            .unwrap(),
    }
}
fn input(fixture: &Fixture) -> BeginWorkspaceTransaction {
    BeginWorkspaceTransaction {
        id: "a".repeat(32),
        plan: fixture.plan.clone(),
        editor: fixture.editor.clone(),
        language_review: Some(Box::new(review(fixture))),
    }
}

#[test]
fn retained_language_review_roundtrips_counts_quota_and_cannot_be_replaced_or_downgraded() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let fixture = fixture(&store, mode).await;
            let input = input(&fixture);
            let prepared = store
                .begin_workspace_transaction(fixture.user, fixture.project.id, &input, 100)
                .await
                .unwrap();
            let loaded = store
                .workspace_transaction(fixture.user, fixture.project.id, Some(&input.id))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(loaded.language_review, input.language_review);
            assert_eq!(loaded.journal.generation, prepared.journal.generation);
            let serialized =
                serde_json::to_string(input.language_review.as_ref().unwrap()).unwrap();
            let counted = store.db.execute("SELECT language_review,bytes,length(CAST(plan AS BLOB))+length(CAST(editor AS BLOB))+length(CAST(root_identity AS BLOB))+length(CAST(editor_root AS BLOB))+length(CAST(generation AS BLOB))+96 FROM workspace_transactions", &[]).await.unwrap();
            assert_eq!(counted.rows[0].get_text(0).unwrap(), serialized);
            assert_eq!(
                counted.rows[0].get_int(1).unwrap() - counted.rows[0].get_int(2).unwrap(),
                i64::try_from(serialized.len() + 64).unwrap()
            );
            let retry = store
                .begin_workspace_transaction(fixture.user, fixture.project.id, &input, 101)
                .await
                .unwrap();
            assert_eq!(retry.journal, prepared.journal);
            let mut changed = input.clone();
            changed.language_review.as_mut().unwrap().input.request += 1;
            let mut provider = input.clone();
            provider.language_review.as_mut().unwrap().binding_revision += 1;
            let mut downgraded = input.clone();
            downgraded.language_review = None;
            for changed in [changed, provider, downgraded] {
                assert!(matches!(
                    store
                        .begin_workspace_transaction(
                            fixture.user,
                            fixture.project.id,
                            &changed,
                            102
                        )
                        .await,
                    Err(StorageError::Conflict(_))
                ));
            }
            assert_eq!(
                store
                    .workspace_transaction(fixture.user, fixture.project.id, Some(&input.id))
                    .await
                    .unwrap()
                    .unwrap()
                    .language_review,
                input.language_review
            );
            assert!(matches!(
                store
                    .workspace_transaction(fixture.other, fixture.project.id, Some(&input.id))
                    .await,
                Err(StorageError::NotFound(_))
            ));
        }
    });
}

#[test]
fn mismatched_language_results_and_editor_patches_never_create_intent_and_corrupt_rows_fail_closed()
{
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let fixture = fixture(&store, mode).await;
            let input = input(&fixture);
            let mut result = input.clone();
            result.language_review.as_mut().unwrap().result = json!({"files":[]});
            let mut editor = input.clone();
            editor.editor.after[0].read_only = true;
            for changed in [result, editor] {
                assert!(matches!(
                    store
                        .begin_workspace_transaction(
                            fixture.user,
                            fixture.project.id,
                            &changed,
                            100
                        )
                        .await,
                    Err(StorageError::InvalidRequest(_))
                ));
                assert!(
                    store
                        .workspace_transaction(fixture.user, fixture.project.id, None)
                        .await
                        .unwrap()
                        .is_none()
                );
            }
            store
                .begin_workspace_transaction(fixture.user, fixture.project.id, &input, 100)
                .await
                .unwrap();
            store
                .db
                .execute(
                    "UPDATE workspace_transactions SET language_review='{}'",
                    &[],
                )
                .await
                .unwrap();
            assert!(
                store
                    .workspace_transaction(fixture.user, fixture.project.id, Some(&input.id))
                    .await
                    .is_err()
            );
        }
    });
}

#[test]
fn schema_65_journals_keep_recovery_and_quota_and_review_migration_is_idempotent() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let fixture = fixture(&store, mode).await;
            let mut input = input(&fixture);
            let review = input.language_review.take().unwrap();
            store
                .begin_workspace_transaction(fixture.user, fixture.project.id, &input, 100)
                .await
                .unwrap();
            let original = store
                .workspace_transaction(fixture.user, fixture.project.id, Some(&input.id))
                .await
                .unwrap()
                .unwrap();
            let bytes = store
                .db
                .execute("SELECT bytes FROM workspace_transactions", &[])
                .await
                .unwrap()
                .rows[0]
                .get_int(0)
                .unwrap();
            store
                .db
                .execute(
                    "ALTER TABLE workspace_transactions DROP COLUMN language_review",
                    &[],
                )
                .await
                .unwrap();
            store
                .db
                .execute("PRAGMA user_version=65", &[])
                .await
                .unwrap();
            store.migrate().await.unwrap();
            let loaded = store
                .workspace_transaction(fixture.user, fixture.project.id, Some(&input.id))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(loaded.journal, original.journal);
            assert_eq!(loaded.editor, original.editor);
            assert!(loaded.language_review.is_none());
            assert_eq!(
                store
                    .db
                    .execute("SELECT bytes FROM workspace_transactions", &[])
                    .await
                    .unwrap()
                    .rows[0]
                    .get_int(0)
                    .unwrap(),
                bytes
            );
            let serialized = serde_json::to_string(&review).unwrap();
            let extra = i64::try_from(serialized.len()).unwrap();
            store
                .db
                .execute(
                    "UPDATE workspace_transactions SET language_review=?,bytes=bytes+?",
                    &[DbValue::Text(serialized), DbValue::Int(extra)],
                )
                .await
                .unwrap();
            store
                .db
                .execute("PRAGMA user_version=65", &[])
                .await
                .unwrap();
            store.migrate().await.unwrap();
            let loaded = store
                .workspace_transaction(fixture.user, fixture.project.id, Some(&input.id))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(loaded.language_review, Some(review));
            assert_eq!(
                store
                    .db
                    .execute("SELECT bytes FROM workspace_transactions", &[])
                    .await
                    .unwrap()
                    .rows[0]
                    .get_int(0)
                    .unwrap(),
                bytes + extra + 64
            );
        }
    });
}

#[test]
fn retained_language_evidence_survives_database_close_and_reopen_in_both_modes() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("review.db");
            let (user, project, input, generation, bytes) = {
                let store = Store::new(RusqliteDb::open(&path).unwrap());
                let fixture = fixture(&store, mode).await;
                let input = input(&fixture);
                let prepared = store
                    .begin_workspace_transaction(fixture.user, fixture.project.id, &input, 100)
                    .await
                    .unwrap();
                let bytes = store
                    .db
                    .execute("SELECT bytes FROM workspace_transactions", &[])
                    .await
                    .unwrap()
                    .rows[0]
                    .get_int(0)
                    .unwrap();
                (
                    fixture.user,
                    fixture.project.id,
                    input,
                    prepared.journal.generation,
                    bytes,
                )
            };
            let store = Store::new(RusqliteDb::open(&path).unwrap());
            store.migrate().await.unwrap();
            let loaded = store
                .workspace_transaction(user, project, Some(&input.id))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(loaded.language_review, input.language_review);
            assert_eq!(loaded.journal.plan, input.plan);
            assert_eq!(loaded.editor, input.editor);
            assert_eq!(loaded.journal.generation, generation);
            assert_eq!(
                store
                    .db
                    .execute("SELECT bytes FROM workspace_transactions", &[])
                    .await
                    .unwrap()
                    .rows[0]
                    .get_int(0)
                    .unwrap(),
                bytes
            );
        }
    });
}

#[test]
fn replaying_earlier_accounting_migrations_cannot_erase_retained_review_quota() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let fixture = fixture(&store, mode).await;
            let input = input(&fixture);
            store
                .begin_workspace_transaction(fixture.user, fixture.project.id, &input, 100)
                .await
                .unwrap();
            let charged = store
                .db
                .execute("SELECT bytes FROM workspace_transactions", &[])
                .await
                .unwrap()
                .rows[0]
                .get_int(0)
                .unwrap();
            // Replaying shipped schema 63 restores its old base-only charge.
            // The new step must derive the complete charge from retained data.
            crate::migrations::apply_through(&store.db, 63)
                .await
                .unwrap();
            let base = store
                .db
                .execute("SELECT bytes FROM workspace_transactions", &[])
                .await
                .unwrap()
                .rows[0]
                .get_int(0)
                .unwrap();
            assert!(base < charged);
            store
                .db
                .execute("PRAGMA user_version=63", &[])
                .await
                .unwrap();
            store.migrate().await.unwrap();
            let restored = store
                .db
                .execute("SELECT bytes FROM workspace_transactions", &[])
                .await
                .unwrap()
                .rows[0]
                .get_int(0)
                .unwrap();
            assert_eq!(
                restored, charged,
                "Migration replay lost the retained language review charge"
            );
            assert_eq!(
                store
                    .workspace_transaction(fixture.user, fixture.project.id, Some(&input.id))
                    .await
                    .unwrap()
                    .unwrap()
                    .language_review,
                input.language_review
            );
        }
    });
}
