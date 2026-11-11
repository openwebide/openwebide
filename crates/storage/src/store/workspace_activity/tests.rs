use super::*;
use crate::rusqlite_db::RusqliteDb;
use futures::executor::block_on;

#[test]
fn recovery_status_is_owned_compact_and_includes_every_pending_phase() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store = Store::new(RusqliteDb::open_in_memory().unwrap());
            store.migrate().await.unwrap();
            let owner = store
                .insert_user("owner", "hash", UserRole::Admin, 0)
                .await
                .unwrap()
                .id;
            let other = store
                .insert_user("other", "hash", UserRole::User, 0)
                .await
                .unwrap()
                .id;
            let project = store
                .create_project(
                    &NewProject {
                        name: "project".into(),
                        mode,
                        path: Some("root".into()),
                    },
                    owner,
                    0,
                )
                .await
                .unwrap()
                .id;
            assert_eq!(
                store.workspace_activity(owner, project).await.unwrap(),
                WorkspaceActivity::default()
            );
            assert!(matches!(
                store.workspace_activity(other, project).await,
                Err(StorageError::NotFound(_))
            ));
            assert!(matches!(
                store.workspace_activity(owner, project + 1).await,
                Err(StorageError::NotFound(_))
            ));
            let unaffected = store
                .create_project(
                    &NewProject {
                        name: "unaffected".into(),
                        mode,
                        path: Some("other".into()),
                    },
                    owner,
                    0,
                )
                .await
                .unwrap()
                .id;

            // Payloads are deliberately unreadable. Status must use durable
            // dispatch columns, rather than load or decode multi-MiB journals.
            store.db.execute("INSERT INTO workspace_transactions (user_id,project_id,id,revision,plan,editor,progress,editor_revision,terminal,bytes,created_at,updated_at) VALUES (?,?,?,1,'invalid','invalid','invalid',0,0,0,0,0)", &[DbValue::Int(owner.get()),DbValue::Int(project),DbValue::Text("a".repeat(32))]).await.unwrap();
            let status = store.workspace_activity(owner, project).await.unwrap();
            assert!(status.file_recovery);
            assert!(status.ensure_idle().is_err());
            assert!(
                store
                    .workspace_transaction(owner, project, None)
                    .await
                    .is_err()
            );
            store
                .db
                .execute("UPDATE workspace_transactions SET terminal=1", &[])
                .await
                .unwrap();

            store.db.execute("INSERT INTO language_actions (user_id,project_id,id,revision,server,command_id,approval,phase,bytes,created_at,updated_at) VALUES (?,?,?,1,'server','command','invalid','approved',0,0,0)", &[DbValue::Int(owner.get()),DbValue::Int(project),DbValue::Text("a".repeat(32))]).await.unwrap();
            for (phase, pending) in [
                ("approved", true),
                ("running", true),
                ("rejected", true),
                ("uncertain", true),
                ("completed", false),
                ("acknowledged", false),
                ("cancelled", false),
            ] {
                store
                    .db
                    .execute(
                        "UPDATE language_actions SET phase=?",
                        &[DbValue::Text(phase.into())],
                    )
                    .await
                    .unwrap();
                let status = store.workspace_activity(owner, project).await.unwrap();
                assert_eq!(
                    status,
                    WorkspaceActivity {
                        file_recovery: false,
                        language_command: pending
                    },
                    "{mode:?} {phase}"
                );
                assert_eq!(status.ensure_idle().is_err(), pending);
                assert!(serde_json::to_vec(&status).unwrap().len() < 64);
                assert_eq!(
                    store.workspace_activity(owner, unaffected).await.unwrap(),
                    WorkspaceActivity::default()
                );
                assert!(matches!(
                    store.workspace_activity(other, project).await,
                    Err(StorageError::NotFound(_))
                ));
            }
        }
    });
}
