use super::*;
use openwebide_core::{
    NewProject, UserRole,
    editor::EditorRecoveryRoot,
    workspace_transaction::{
        DelegateWorkspaceTransaction, DelegatedWorkspaceJournalOperation as Operation,
        DelegatedWorkspaceJournalRequest, EditorRecoveryPatch, EntryState, FileOperation,
        TransactionPhase, TransactionProgress, WorkspaceJournalDelegation,
        WorkspaceJournalResponse, WorkspacePlan, WorkspaceTransactionWorker,
    },
};
fn request(body: Vec<u8>) -> Request<FullBody<Bytes>> {
    Request::builder()
        .method("POST")
        .uri("http://localhost/api")
        .body(FullBody::new(Bytes::from(body)))
        .unwrap()
}
#[test]
fn account_issues_one_scoped_native_capability_and_browser_callbacks_are_rejected() {
    futures::executor::block_on(async {
        let state = AppState::new().await.unwrap();
        let user = state
            .store
            .insert_user("delegation-owner", "hash", UserRole::Admin, 1)
            .await
            .unwrap();
        let other = state
            .store
            .insert_user("delegation-other", "hash", UserRole::User, 1)
            .await
            .unwrap();
        let project = state
            .store
            .create_project(
                &NewProject {
                    name: "local".into(),
                    mode: WorkspaceMode::Local,
                    path: Some("native".into()),
                },
                user.id,
                1,
            )
            .await
            .unwrap();
        let id = "a".repeat(32);
        let prepared = state
            .store
            .begin_workspace_transaction(
                user.id,
                project.id,
                &BeginWorkspaceTransaction {
                    language_review: None,
                    id: id.clone(),
                    plan: WorkspacePlan {
                        version: 1,
                        root: "a".repeat(64),
                        operations: vec![FileOperation::Create {
                            path: "new".into(),
                            overwrite: false,
                            ignore_if_exists: false,
                        }],
                        roots: vec!["new".into()],
                        before: [("new".into(), EntryState::Missing)].into(),
                        after: [("new".into(), EntryState::file(Vec::new()))].into(),
                    },
                    editor: EditorRecoveryPatch {
                        revision: 0,
                        root: EditorRecoveryRoot::for_project(&project),
                        before: vec![],
                        after: vec![],
                        order_before: vec![],
                        order_after: vec![],
                        selected_before: None,
                        selected_after: None,
                    },
                },
                1,
            )
            .await
            .unwrap();
        let worker = WorkspaceTransactionWorker {
            id: "b".repeat(32),
            secret: "c".repeat(32),
        };
        let input = DelegateWorkspaceTransaction {
            generation: prepared.journal.generation.clone(),
            revision: 1,
            worker: worker.delegation_key(),
        };
        let path = format!(
            "/api/projects/{}/workspace-transactions/{id}/delegate",
            project.id
        );
        let owner = AuthedUser {
            id: user.id,
            role: user.role,
        };
        assert_eq!(
            delegate(
                request(serde_json::to_vec(&input).unwrap()),
                &state,
                &path,
                AuthedUser {
                    id: other.id,
                    role: other.role
                }
            )
            .await
            .unwrap_err()
            .into_response()
            .status(),
            404
        );
        let response = delegate(
            request(serde_json::to_vec(&input).unwrap()),
            &state,
            &path,
            owner,
        )
        .await
        .unwrap();
        assert_eq!(response.headers()["cache-control"], "no-store");
        let grant: WorkspaceJournalDelegation =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let mut call = DelegatedWorkspaceJournalRequest {
            delegation: grant,
            worker: worker.clone(),
            operation: Operation::Admit {
                review_digest: None,
                language_grant: None,
            },
        };
        for header in ["origin", "sec-fetch-site", "cookie"] {
            let mut req = request(serde_json::to_vec(&call).unwrap());
            req.headers_mut().insert(header, "browser".parse().unwrap());
            assert_eq!(
                delegated(req, &state)
                    .await
                    .unwrap_err()
                    .into_response()
                    .status(),
                403
            );
        }
        call.worker.secret = "d".repeat(32);
        assert_eq!(
            delegated(request(serde_json::to_vec(&call).unwrap()), &state)
                .await
                .unwrap_err()
                .into_response()
                .status(),
            409
        );
        call.worker = worker;
        let response = delegated(request(serde_json::to_vec(&call).unwrap()), &state)
            .await
            .unwrap();
        assert_eq!(response.headers()["cache-control"], "no-store");
        let reply: WorkspaceJournalResponse =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert!(matches!(
            reply,
            WorkspaceJournalResponse::WorkerClaimed { .. }
        ));
        call.operation = Operation::Advance {
            revision: 1,
            progress: TransactionProgress {
                phase: TransactionPhase::Applying,
                ..TransactionProgress::default()
            },
        };
        let response = delegated(request(serde_json::to_vec(&call).unwrap()), &state)
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(body.len() < 1024);
        assert!(
            matches!(serde_json::from_slice::<WorkspaceJournalResponse>(&body).unwrap(),WorkspaceJournalResponse::Advanced {response} if response.revision==2 && response.editor.is_none())
        );
        call.operation = Operation::Release;
        delegated(request(serde_json::to_vec(&call).unwrap()), &state)
            .await
            .unwrap();
        call.operation = Operation::Scope;
        assert_eq!(
            delegated(request(serde_json::to_vec(&call).unwrap()), &state)
                .await
                .unwrap_err()
                .into_response()
                .status(),
            409
        );
    });
}
