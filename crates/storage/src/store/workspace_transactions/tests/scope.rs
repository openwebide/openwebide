use super::*;
use openwebide_core::workspace_transaction::WorkspaceTransactionScope;

#[test]
fn owned_scope_uses_small_metadata_and_keeps_current_revision_in_both_modes() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let fixture = fixture(&store, mode).await;
            let adapter = journal(&store, &fixture);
            adapter.prepare(&fixture.plan).await.unwrap();
            let state = store
                .workspace_transaction(fixture.user, fixture.project.id, Some(&adapter.id))
                .await
                .unwrap()
                .unwrap();
            let original = store
                .workspace_transaction_scope(fixture.user, fixture.project.id, &adapter.id)
                .await
                .unwrap()
                .unwrap();
            assert!(original.matches_state(&state));
            assert!(
                store
                    .workspace_transaction_scope(fixture.other, fixture.project.id, &adapter.id)
                    .await
                    .is_err()
            );
            assert!(
                store
                    .workspace_transaction_scope(fixture.user, fixture.project.id, "invalid")
                    .await
                    .is_err()
            );
            assert!(
                store
                    .workspace_transaction_scope(fixture.user, fixture.project.id, &"b".repeat(32))
                    .await
                    .unwrap()
                    .is_none()
            );
            let revision = adapter
                .advance(
                    &adapter.id,
                    &original.generation,
                    original.revision,
                    &TransactionProgress::prepared_cancellation(),
                )
                .await
                .unwrap();
            let current = WorkspaceTransactionScope {
                revision,
                ..original
            };
            // Scope reads do not deserialize either payload. The later full
            // load must still fail before execution when snapshots are corrupt.
            store.db.execute("UPDATE workspace_transactions SET plan='invalid snapshot',editor='invalid snapshot'", &[]).await.unwrap();
            assert_eq!(
                store
                    .workspace_transaction_scope(fixture.user, fixture.project.id, &adapter.id)
                    .await
                    .unwrap(),
                Some(current.clone())
            );
            assert!(
                store
                    .workspace_transaction(fixture.user, fixture.project.id, Some(&adapter.id))
                    .await
                    .is_err()
            );
            let serialized = serde_json::to_string(&current).unwrap();
            assert!(serialized.len() < 1024);
            assert!(!serialized.contains("before") && !serialized.contains("draft"));
            store
                .db
                .execute("UPDATE workspace_transactions SET root_identity=''", &[])
                .await
                .unwrap();
            assert!(
                store
                    .workspace_transaction_scope(fixture.user, fixture.project.id, &adapter.id)
                    .await
                    .is_err()
            );
        }
    });
}

#[test]
fn schema_61_recovery_backfills_scope_without_losing_snapshots_or_double_counting_bytes() {
    block_on(async {
        let store = Store::new(RusqliteDb::open_in_memory().unwrap());
        let fixture = fixture(&store, WorkspaceMode::Remote).await;
        let adapter = journal(&store, &fixture);
        adapter.prepare(&fixture.plan).await.unwrap();
        let original = store
            .workspace_transaction(fixture.user, fixture.project.id, Some(&adapter.id))
            .await
            .unwrap()
            .unwrap();
        for column in ["root_identity", "editor_root", "generation"] {
            store
                .db
                .execute(
                    &format!("ALTER TABLE workspace_transactions DROP COLUMN {column}"),
                    &[],
                )
                .await
                .unwrap();
        }
        store.db.execute("UPDATE workspace_transactions SET bytes=length(CAST(plan AS BLOB))+length(CAST(editor AS BLOB))", &[]).await.unwrap();
        store
            .db
            .execute("PRAGMA user_version=61", &[])
            .await
            .unwrap();
        store.migrate().await.unwrap();
        let scope = store
            .workspace_transaction_scope(fixture.user, fixture.project.id, &adapter.id)
            .await
            .unwrap()
            .unwrap();
        let recovered = store
            .workspace_transaction(fixture.user, fixture.project.id, Some(&adapter.id))
            .await
            .unwrap()
            .unwrap();
        assert!(scope.matches_state(&recovered));
        assert_ne!(recovered.journal.generation, original.journal.generation);
        assert_eq!(
            recovered.journal,
            JournalRecord {
                generation: recovered.journal.generation.clone(),
                ..original.journal.clone()
            }
        );
        assert_eq!(recovered.editor, original.editor);
        let counted = store.db.execute("SELECT bytes,length(CAST(plan AS BLOB))+length(CAST(editor AS BLOB))+length(CAST(root_identity AS BLOB))+length(CAST(editor_root AS BLOB))+length(CAST(generation AS BLOB))+96 FROM workspace_transactions", &[]).await.unwrap();
        assert_eq!(
            counted.rows[0].get_int(0).unwrap(),
            counted.rows[0].get_int(1).unwrap()
        );
        store
            .db
            .execute("PRAGMA user_version=61", &[])
            .await
            .unwrap();
        store.migrate().await.unwrap();
        assert_eq!(
            store
                .workspace_transaction_scope(fixture.user, fixture.project.id, &adapter.id)
                .await
                .unwrap(),
            Some(scope)
        );
        let unchanged = store
            .db
            .execute("SELECT bytes FROM workspace_transactions", &[])
            .await
            .unwrap();
        assert_eq!(
            unchanged.rows[0].get_int(0).unwrap(),
            counted.rows[0].get_int(0).unwrap()
        );
    });
}

#[test]
fn replaced_records_reject_stale_phase_updates_even_when_id_and_revision_repeat() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            let fixture = fixture(&store, mode).await;
            let adapter = journal(&store, &fixture);
            adapter.prepare(&fixture.plan).await.unwrap();
            let before = store
                .workspace_transaction_scope(fixture.user, fixture.project.id, &adapter.id)
                .await
                .unwrap()
                .unwrap();
            let request = AdvanceWorkspaceTransaction {
                id: adapter.id.clone(),
                generation: before.generation.clone(),
                revision: before.revision,
                progress: TransactionProgress::prepared_cancellation(),
            };
            store.db.execute("UPDATE workspace_transactions SET generation=lower(hex(randomblob(16))) WHERE user_id=? AND project_id=? AND id=?", &[
                DbValue::Int(fixture.user.get()), DbValue::Int(fixture.project.id), DbValue::Text(adapter.id.clone()),
            ]).await.unwrap();
            let replacement = store
                .workspace_transaction_scope(fixture.user, fixture.project.id, &adapter.id)
                .await
                .unwrap()
                .unwrap();
            assert_ne!(before.generation, replacement.generation);
            assert_eq!(before.id, replacement.id);
            assert_eq!(before.revision, replacement.revision);
            assert!(
                store
                    .advance_workspace_transaction(fixture.user, fixture.project.id, &request, 100)
                    .await
                    .is_err()
            );
            assert_eq!(
                store
                    .workspace_transaction_scope(fixture.user, fixture.project.id, &adapter.id)
                    .await
                    .unwrap(),
                Some(replacement)
            );
            assert_eq!(
                store
                    .workspace_transaction(fixture.user, fixture.project.id, Some(&adapter.id))
                    .await
                    .unwrap()
                    .unwrap()
                    .journal
                    .progress,
                TransactionProgress::default()
            );
        }
    });
}
