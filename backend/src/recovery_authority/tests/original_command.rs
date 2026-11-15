use super::*;
use crate::api::language_actions::{original as original_plain, original_private};
use openwebide_core::plugins::languages::intelligence::actions::{
    ActionPhase, CommandAdmission, CommandAuthority, CommandInvocation, CommandJournalOperation,
    CommandRecoveryScope, CommandWorkerOperation, OriginalCommandRequest,
};
use openwebide_storage::db::{Db, DbValue};

// Both real backend transports must satisfy the same original-owner contract.
async fn original(
    input: Request<FullBody<Bytes>>,
    state: &AppState,
    private: bool,
) -> Result<crate::error::JsonResp, ApiError> {
    if !private {
        return original_plain(input, state).await;
    }
    use openwebide_core::plugins::languages::intelligence::actions::{
        COMMAND_RECOVERY_PLAINTEXT_LIMIT, COMMAND_RECOVERY_REPLY_DOMAIN,
        COMMAND_RECOVERY_REQUEST_DOMAIN, CommandRecoveryMessage, CommandRecoveryReply,
    };
    let (parts, body) = input.into_parts();
    let body = body.collect().await.unwrap().to_bytes();
    let request: OriginalCommandRequest = serde_json::from_slice(&body).unwrap();
    let mut message = CommandRecoveryMessage::for_authority(&request.authority).unwrap();
    let (nonce, ciphertext) = openwebide_auth::private_messages::seal(
        request.authority.secret.as_bytes(),
        COMMAND_RECOVERY_REQUEST_DOMAIN,
        &message.context().unwrap(),
        &body,
    )
    .unwrap();
    message.nonce = nonce;
    message.ciphertext = ciphertext;
    let wire = serde_json::to_vec(&message).unwrap();
    assert!(!String::from_utf8_lossy(&wire).contains(&request.authority.secret));
    assert!(!String::from_utf8_lossy(&wire).contains(&request.worker.secret));
    let response = original_private(
        Request::from_parts(parts, FullBody::new(Bytes::from(wire))),
        state,
    )
    .await?;
    let (parts, body) = response.into_parts();
    let wire = body.collect().await.unwrap().to_bytes();
    assert!(!String::from_utf8_lossy(&wire).contains(&request.authority.secret));
    let reply: CommandRecoveryReply = serde_json::from_slice(&wire).unwrap();
    let plaintext = openwebide_auth::private_messages::open(
        request.authority.secret.as_bytes(),
        COMMAND_RECOVERY_REPLY_DOMAIN,
        &message.reply_context().unwrap(),
        &reply.nonce,
        &reply.ciphertext,
        COMMAND_RECOVERY_PLAINTEXT_LIMIT,
    )
    .unwrap();
    Ok(spin_sdk::http::Response::from_parts(
        parts,
        spin_sdk::http::box_body(FullBody::new(Bytes::from(plaintext))),
    ))
}

#[test]
fn original_command_callback_requires_the_issuer_and_worker_and_cannot_start() {
    futures::executor::block_on(async {
        for private in [false, true] {
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
                // A frozen command record exercises the recovery transport without
                // installing a provider or granting fresh execution permission.
                let directory = std::env::temp_dir()
                    .join(format!("original-command-{:032x}", rand::random::<u128>()));
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    std::fs::DirBuilder::new()
                        .mode(0o700)
                        .create(&directory)
                        .unwrap();
                }
                #[cfg(not(unix))]
                std::fs::create_dir(&directory).unwrap();
                let path = directory.join("original.sqlite");
                let primitive = crate::state::AppDb::open(&path).unwrap();
                let state = AppState {
                    store: openwebide_storage::Store::new(
                        crate::state::AppDb::open(&path).unwrap(),
                    ),
                };
                state.store.migrate().await.unwrap();
                let (state, user, project, scope) = fixture_in(state, mode).await;
                let authority = fetch(&state, user, project, &scope).await;
                let worker = WorkspaceTransactionWorker {
                    id: "e".repeat(32),
                    secret: "f".repeat(32),
                };
                let command = CommandRecoveryScope {
                    invocation: CommandInvocation {
                        id: scope.id.clone(),
                        client: "original-window".into(),
                        command: "1".repeat(32),
                        server: "original-service".into(),
                        origin: "2".repeat(64),
                        review: "3".repeat(64),
                        approval: "4".repeat(64),
                    },
                    revision: 2,
                    phase: ActionPhase::Approved,
                    outcome_digest: None,
                    worker: Some(worker.state()),
                    native_settlement_pending: true,
                    command_cleanup_confirmed: false,
                };
                primitive.execute("INSERT INTO language_actions(user_id,project_id,id,revision,server,command_id,approval,phase,bytes,created_at,updated_at,invocation,command_worker_id,command_worker_digest,native_settlement_pending) VALUES(?,?,?,2,'original-service',?,'frozen provider payload','approved',0,1,1,?,?,?,1)", &[
                DbValue::Int(user.get()), DbValue::Int(project), DbValue::Text(scope.id.clone()), DbValue::Text(command.invocation.command.clone()), DbValue::Text(serde_json::to_string(&command.invocation).unwrap()), DbValue::Text(worker.id.clone()), DbValue::Text(worker.secret_digest()),
            ]).await.unwrap();
                let expected = state
                    .store
                    .workspace_command_recovery(user, project, &scope.id)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(expected.command, command);
                let input = OriginalCommandRequest {
                    authority: authority.clone(),
                    worker: worker.clone(),
                    operation: CommandJournalOperation::Scope,
                };
                for header in ["origin", "sec-fetch-site", "cookie"] {
                    let mut browser = request(&input);
                    browser
                        .headers_mut()
                        .insert(header, "present".parse().unwrap());
                    assert_eq!(
                        original(browser, &state, private)
                            .await
                            .unwrap_err()
                            .into_response()
                            .status(),
                        403
                    );
                }
                let mut forged = input.clone();
                forged.authority.secret = "A".repeat(43);
                assert_eq!(
                    original(request(&forged), &state, private)
                        .await
                        .unwrap_err()
                        .into_response()
                        .status(),
                    401
                );
                forged = input.clone();
                forged.authority.project += 1;
                assert_eq!(
                    original(request(&forged), &state, private)
                        .await
                        .unwrap_err()
                        .into_response()
                        .status(),
                    401
                );
                forged = input.clone();
                forged.worker.secret = "0".repeat(32);
                assert_eq!(
                    original(request(&forged), &state, private)
                        .await
                        .unwrap_err()
                        .into_response()
                        .status(),
                    409
                );
                for admission in [
                    CommandAdmission::Recover,
                    CommandAdmission::Start {
                        authority: CommandAuthority {
                            grant: "a".repeat(32),
                            session_id: None,
                        },
                    },
                ] {
                    let mut start = input.clone();
                    start.operation = CommandJournalOperation::Worker {
                        operation: Box::new(CommandWorkerOperation::Claim {
                            expected: Box::new(expected.clone()),
                            worker: worker.clone(),
                            admission,
                        }),
                    };
                    if let CommandJournalOperation::Worker { operation } = &start.operation {
                        operation.validate().unwrap();
                    }
                    assert_eq!(
                        original(request(&start), &state, private)
                            .await
                            .unwrap_err()
                            .into_response()
                            .status(),
                        400
                    );
                }
                let mut reject = input.clone();
                reject.operation = CommandJournalOperation::Worker {
                    operation: Box::new(CommandWorkerOperation::RejectUnclaimed {
                        expected: Box::new(expected.clone()),
                        worker,
                        message: "Files are still pending".into(),
                    }),
                };
                assert_eq!(
                    original(request(&reject), &state, private)
                        .await
                        .unwrap_err()
                        .into_response()
                        .status(),
                    409
                );
                for key in ["auth_secret", "bridge_secret_cache"] {
                    state
                        .store
                        .set_setting(key, "rotated transport credential")
                        .await
                        .unwrap();
                }
                let response = original(request(&input), &state, private).await.unwrap();
                assert_eq!(response.headers()["cache-control"], "no-store");
                let bytes = response.into_body().collect().await.unwrap().to_bytes();
                let value: WorkspaceJournalResponse = serde_json::from_slice(&bytes).unwrap();
                let (observed, issuer) = value
                    .command_recovery(user.get(), project, &scope.id)
                    .unwrap();
                assert_eq!(*observed, expected);
                assert_eq!(*issuer, authority);
                assert_eq!(
                    state
                        .store
                        .workspace_command_recovery(user, project, &scope.id)
                        .await
                        .unwrap()
                        .unwrap(),
                    expected
                );
                // The original callback also recovers a cancelled reservation
                // after the provider payload is no longer readable. Cancellation
                // retains no synthetic command result and needs no new grant.
                state.store.advance_workspace_transaction(user, project,
                &openwebide_core::workspace_transaction::AdvanceWorkspaceTransaction {
                    id: scope.id.clone(), generation: scope.generation.clone(), revision: scope.revision,
                    progress: openwebide_core::workspace_transaction::TransactionProgress::prepared_cancellation(),
                }, now(),
            ).await.unwrap();
                primitive.execute("UPDATE language_actions SET phase='cancelled',revision=3 WHERE user_id=? AND project_id=? AND id=?", &[
                DbValue::Int(user.get()), DbValue::Int(project), DbValue::Text(scope.id.clone()),
            ]).await.unwrap();
                let cancelled = state
                    .store
                    .workspace_command_recovery(user, project, &scope.id)
                    .await
                    .unwrap()
                    .unwrap();
                let operation = CommandWorkerOperation::ConfirmCancellation {
                    expected: Box::new(cancelled.clone()),
                    worker: input.worker.clone(),
                };
                let mut cleanup = input.clone();
                cleanup.operation = CommandJournalOperation::Worker {
                    operation: Box::new(operation.clone()),
                };
                let mut confirmed = None;
                for _ in 0..2 {
                    let response = original(request(&cleanup), &state, private).await.unwrap();
                    assert_eq!(response.headers()["cache-control"], "no-store");
                    let bytes = response.into_body().collect().await.unwrap().to_bytes();
                    let reply: WorkspaceJournalResponse = serde_json::from_slice(&bytes).unwrap();
                    let value = reply.command_worker(&operation).unwrap();
                    assert_eq!(value.command.phase, ActionPhase::Cancelled);
                    assert!(value.command.outcome_digest.is_none());
                    assert!(value.command.command_cleanup_confirmed);
                    if let Some(previous) = &confirmed {
                        assert_eq!(value, previous);
                    }
                    confirmed = Some(value.clone());
                }
                let operation = CommandWorkerOperation::Settle {
                    expected: Box::new(confirmed.unwrap()),
                    worker: input.worker.clone(),
                };
                cleanup.operation = CommandJournalOperation::Worker {
                    operation: Box::new(operation.clone()),
                };
                let mut final_scope = None;
                for _ in 0..2 {
                    let response = original(request(&cleanup), &state, private).await.unwrap();
                    let bytes = response.into_body().collect().await.unwrap().to_bytes();
                    let reply: WorkspaceJournalResponse = serde_json::from_slice(&bytes).unwrap();
                    let settled = reply.command_worker(&operation).unwrap();
                    assert_eq!(settled.command.phase, ActionPhase::Cancelled);
                    assert!(settled.command.outcome_digest.is_none());
                    assert!(!settled.command.native_settlement_pending);
                    assert!(settled.activity.ensure_idle().is_ok());
                    final_scope = Some(settled.clone());
                }
                let settled = final_scope.unwrap();
                // Ordinary user-wide pruning runs when another project begins
                // work. The original callback must not depend on public history.
                let next_project = state
                    .store
                    .create_project(
                        &NewProject {
                            name: "later work".into(),
                            mode,
                            path: Some("later".into()),
                        },
                        user,
                        now(),
                    )
                    .await
                    .unwrap();
                let next = BeginWorkspaceTransaction {
                    id: "9".repeat(32),
                    language_review: None,
                    plan: WorkspacePlan {
                        version: 1,
                        root: "8".repeat(64),
                        operations: vec![FileOperation::Create {
                            path: "later".into(),
                            overwrite: false,
                            ignore_if_exists: false,
                        }],
                        roots: vec!["later".into()],
                        before: [("later".into(), EntryState::Missing)].into(),
                        after: [("later".into(), EntryState::file(Vec::new()))].into(),
                    },
                    editor: EditorRecoveryPatch {
                        revision: 0,
                        root: EditorRecoveryRoot::for_project(&next_project),
                        before: vec![],
                        after: vec![],
                        order_before: vec![],
                        order_after: vec![],
                        selected_before: None,
                        selected_after: None,
                    },
                };
                state
                    .store
                    .begin_workspace_transaction(user, next_project.id, &next, now() + 1_000_000)
                    .await
                    .unwrap();
                assert!(
                    state
                        .store
                        .workspace_transaction(user, project, Some(&scope.id))
                        .await
                        .unwrap()
                        .is_none()
                );
                assert!(
                    state
                        .store
                        .language_action(user, project, Some(&scope.id))
                        .await
                        .unwrap()
                        .is_none()
                );
                let response = original(request(&input), &state, private).await.unwrap();
                assert_eq!(response.headers()["cache-control"], "no-store");
                let bytes = response.into_body().collect().await.unwrap().to_bytes();
                let reply: WorkspaceJournalResponse = serde_json::from_slice(&bytes).unwrap();
                let (recovered, issuer) = reply
                    .command_recovery(user.get(), project, &scope.id)
                    .unwrap();
                assert_eq!(*recovered, settled);
                assert_eq!(*issuer, authority);
                let before = state
                    .store
                    .workspace_transaction_scope(user, next_project.id, &next.id)
                    .await
                    .unwrap();
                cleanup.operation = CommandJournalOperation::AcknowledgeSettlement {
                    expected: Box::new(settled.clone()),
                };
                for header in ["origin", "sec-fetch-site", "cookie"] {
                    let mut browser = request(&cleanup);
                    browser
                        .headers_mut()
                        .insert(header, "present".parse().unwrap());
                    assert_eq!(
                        original(browser, &state, private)
                            .await
                            .unwrap_err()
                            .into_response()
                            .status(),
                        403
                    );
                }
                for mutation in ["issuer", "worker"] {
                    let mut forged = cleanup.clone();
                    if mutation == "issuer" {
                        forged.authority.secret = URL_SAFE_NO_PAD.encode([9u8; 32]);
                    } else {
                        forged.worker.secret = "0".repeat(32);
                    }
                    assert!(original(request(&forged), &state, private).await.is_err());
                }
                for attempt in 0..2 {
                    if attempt == 1 {
                        // A lost disposal ACK may be retried after the user has
                        // deleted the now-idle project and its cascading metadata.
                        state.store.delete_project(project, user).await.unwrap();
                    }
                    let response = original(request(&cleanup), &state, private).await.unwrap();
                    assert_eq!(response.headers()["cache-control"], "no-store");
                    let bytes = response.into_body().collect().await.unwrap().to_bytes();
                    assert!(!String::from_utf8_lossy(&bytes).contains(&authority.secret));
                    let reply: WorkspaceJournalResponse = serde_json::from_slice(&bytes).unwrap();
                    reply.command_settlement_acknowledged(&settled).unwrap();
                    assert!(
                        reply
                            .command_recovery(user.get(), project, &scope.id)
                            .is_err()
                    );
                }
                assert_eq!(
                    state
                        .store
                        .workspace_transaction_scope(user, next_project.id, &next.id)
                        .await
                        .unwrap(),
                    before
                );
                assert!(original(request(&input), &state, private).await.is_err());
                let rows = primitive
                    .execute(
                        "SELECT count(*) FROM language_command_settlements WHERE user_id=?",
                        &[DbValue::Int(user.get())],
                    )
                    .await
                    .unwrap();
                assert_eq!(rows.rows[0].get_int(0).unwrap(), 0);
            }
        }
    });
}

#[test]
fn original_verification_never_initializes_or_replaces_a_missing_or_corrupt_issuer() {
    futures::executor::block_on(async {
        let (state, user, project, scope) = fixture(WorkspaceMode::Remote).await;
        let supplied =
            RecoveryAuthority::for_scope(user.get(), project, &scope, "A".repeat(43)).unwrap();
        assert_eq!(
            verify(&state, &supplied)
                .await
                .unwrap_err()
                .into_response()
                .status(),
            401
        );
        assert!(state.store.get_setting(SECRET_KEY).await.unwrap().is_none());
        let original = fetch(&state, user, project, &scope).await;
        verify(&state, &original).await.unwrap();
        let other = AppState::new().await.unwrap();
        original_secret(&other).await.unwrap();
        assert_eq!(
            verify(&other, &original)
                .await
                .unwrap_err()
                .into_response()
                .status(),
            401
        );
        state
            .store
            .set_setting(SECRET_KEY, "corrupt issuer")
            .await
            .unwrap();
        assert_eq!(
            verify(&state, &original)
                .await
                .unwrap_err()
                .into_response()
                .status(),
            500
        );
        assert_eq!(
            state
                .store
                .get_setting(SECRET_KEY)
                .await
                .unwrap()
                .as_deref(),
            Some("corrupt issuer")
        );
    });
}

#[test]
fn private_callback_cannot_initialize_a_replacement_issuer_or_accept_request_tampering() {
    futures::executor::block_on(async {
        use openwebide_core::plugins::languages::intelligence::actions::{
            COMMAND_RECOVERY_REQUEST_DOMAIN, CommandRecoveryMessage,
        };
        let (state, user, project, scope) = fixture(WorkspaceMode::Remote).await;
        let authority = fetch(&state, user, project, &scope).await;
        let input = OriginalCommandRequest {
            authority: authority.clone(),
            worker: WorkspaceTransactionWorker {
                id: "e".repeat(32),
                secret: "f".repeat(32),
            },
            operation: CommandJournalOperation::Scope,
        };
        let mut message = CommandRecoveryMessage::for_authority(&authority).unwrap();
        let (nonce, ciphertext) = openwebide_auth::private_messages::seal(
            authority.secret.as_bytes(),
            COMMAND_RECOVERY_REQUEST_DOMAIN,
            &message.context().unwrap(),
            &serde_json::to_vec(&input).unwrap(),
        )
        .unwrap();
        message.nonce = nonce;
        message.ciphertext = ciphertext;
        let before = state
            .store
            .workspace_transaction_scope(user, project, &scope.id)
            .await
            .unwrap();
        for change in ["scope", "user", "nonce", "ciphertext"] {
            let mut forged = message.clone();
            match change {
                "scope" => forged.scope.generation = "f".repeat(32),
                "user" => forged.user += 1,
                "nonce" => forged.nonce = URL_SAFE_NO_PAD.encode([0u8; 24]),
                _ => {
                    forged.ciphertext.replace_range(
                        ..1,
                        if forged.ciphertext.starts_with('A') {
                            "B"
                        } else {
                            "A"
                        },
                    );
                }
            }
            assert_eq!(
                original_private(request(&forged), &state)
                    .await
                    .unwrap_err()
                    .into_response()
                    .status(),
                401
            );
        }
        assert_eq!(
            state
                .store
                .workspace_transaction_scope(user, project, &scope.id)
                .await
                .unwrap(),
            before
        );
        let empty = AppState::new().await.unwrap();
        assert_eq!(
            original_private(request(&message), &empty)
                .await
                .unwrap_err()
                .into_response()
                .status(),
            401
        );
        assert!(empty.store.get_setting(SECRET_KEY).await.unwrap().is_none());
    });
}
