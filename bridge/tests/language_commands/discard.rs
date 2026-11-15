use super::*;
use openwebide_agent::plugins::{
    actions::PluginActionHost,
    execution::{GrantedHost, PluginTransport},
    languages::LanguageActionClient,
};
use openwebide_core::{
    UserId,
    plugins::{
        PreparedPlugin,
        execution::{ContinuePlugin, InvokePlugin, PluginHostRequest, PluginInvocation},
        languages::LanguagePreferences,
        languages::intelligence::actions::{
            ActionJournalRequest, ActionRecord, ActionRecovery, ActionRecoveryRequest,
            CommandDispatch,
        },
    },
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone)]
struct Client {
    backend: Arc<command_journal::Backend>,
    port: Arc<std::sync::atomic::AtomicU16>,
    paired: bool,
    fresh: Arc<AtomicUsize>,
}
impl PluginTransport for Client {
    async fn ensure_prepared(&self, _: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        Err("No new plugin preparation during recovery".into())
    }
    async fn start(&self, _: InvokePlugin) -> Result<PluginInvocation, String> {
        Err("No new plugin execution during recovery".into())
    }
    async fn resume(&self, _: ContinuePlugin) -> Result<PluginInvocation, String> {
        Err("No new plugin execution during recovery".into())
    }
    fn cancel(&self, _: String) {}
}
impl GrantedHost for Client {
    async fn request(&self, _: &PluginHostRequest) -> Result<String, String> {
        Err("No new plugin authority during recovery".into())
    }
}
impl PluginActionHost for Client {
    fn is_current(&self) -> bool {
        true
    }
    async fn prepare(&self, _: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        self.fresh.fetch_add(1, Ordering::SeqCst);
        Err("Provider revoked".into())
    }
    async fn grant(&self, _: &PreparedPlugin) -> Result<String, String> {
        self.fresh.fetch_add(1, Ordering::SeqCst);
        Err("Manual authority expired".into())
    }
    async fn language_action_request(
        &self,
        request: &ActionJournalRequest,
    ) -> Result<Option<ActionRecord>, String> {
        futures::executor::block_on(self.backend.store.language_action_request(
            UserId::new(1),
            1,
            request,
            1_000_000,
        ))
        .map_err(|e| e.to_string())
    }
    async fn language_action_recovery(
        &self,
        request: &ActionRecoveryRequest,
    ) -> Result<Option<ActionRecovery>, String> {
        futures::executor::block_on(self.backend.store.language_action_recovery(
            UserId::new(1),
            1,
            request,
            1_000_000,
        ))
        .map_err(|e| e.to_string())
    }
    async fn language_command(&self, request: &CommandDispatch) -> Result<CommandProgress, String> {
        assert_ne!(request.operation, CommandOperation::Start);
        assert!(request.call.is_none());
        let command = futures::executor::block_on(self.backend.store.authorize_language_command(
            UserId::new(1),
            1,
            request,
            1_000_000,
        ))
        .map_err(|e| e.to_string())?;
        if let Some(progress) = command.progress {
            return Ok(progress);
        }
        let reply = send(
            self.port.load(Ordering::SeqCst),
            self.paired,
            HostCommandRequest {
                user: (!self.paired).then_some(1),
                root: String::new(),
                command,
                authority: None,
                journal: None,
            },
        )
        .await;
        if reply.status != 200 {
            return Err(reply.body);
        }
        serde_json::from_str(&reply.body).map_err(|e| e.to_string())
    }
}
fn client(
    case: &command_case::Case,
    paired: bool,
) -> (
    LanguageActionClient<Client>,
    Arc<AtomicUsize>,
    Arc<std::sync::atomic::AtomicU16>,
) {
    let fresh = Arc::new(AtomicUsize::new(0));
    let port = Arc::new(std::sync::atomic::AtomicU16::new(case.port));
    (
        LanguageActionClient::new(
            Client {
                backend: case.backend.clone(),
                port: port.clone(),
                paired,
                fresh: fresh.clone(),
            },
            vec![],
            LanguagePreferences::default(),
        )
        .unwrap(),
        fresh,
        port,
    )
}

#[tokio::test]
async fn reserved_approved_commands_can_stop_through_the_shared_recovery_facade_without_fresh_authority()
 {
    for paired in [false, true] {
        let case = command_case::Case::unclaimed(paired).await;
        case.backend
            .store
            .set_user_setting(UserId::new(1), "language_settings", "{revoked provider")
            .await
            .unwrap();
        let (client, fresh, _) = client(&case, paired);
        let record = client
            .recovery(&ActionRecoveryRequest::Load {
                id: Some(case.request.command.invocation.id.clone()),
            })
            .await
            .unwrap()
            .unwrap();
        let result = client
            .recover_command(&record, CommandOperation::Stop)
            .await;
        assert!(
            matches!(
                result,
                Ok(CommandProgress::Finished {
                    outcome: CommandOutcome::Rejected { .. }
                })
            ),
            "{result:?}"
        );
        assert_eq!(fresh.load(Ordering::SeqCst), 0);
        assert!(!case.root.join("executions").exists());
        case.close().await;
    }
}

async fn load(client: &LanguageActionClient<Client>, id: &str) -> ActionRecovery {
    client
        .recovery(&ActionRecoveryRequest::Load {
            id: Some(id.into()),
        })
        .await
        .unwrap()
        .unwrap()
}
async fn writer(case: &command_case::Case, paired: bool) -> HttpResponse {
    let response = send_json(
        case.port,
        paired,
        "/exec",
        &json!({"command":"true","cwd":case.root.to_string_lossy(),"timeout_seconds":5}),
    )
    .await;
    if response.status == 200 {
        let outcome: openwebide_core::CommandOutcome =
            serde_json::from_str(&response.body).unwrap();
        assert!(outcome.is_success(), "{outcome:?}");
    }
    response
}
async fn wait_count(counter: &AtomicUsize) {
    tokio::time::timeout(Duration::from_secs(8), async {
        while counter.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn committed_discard_and_original_rejections_keep_edits_and_recover_without_new_authority() {
    use openwebide_core::{
        plugins::languages::intelligence::actions::ActionPhase,
        workspace_transaction::TransactionPhase,
    };
    for paired in [false, true] {
        for prior_rejection in [false, true] {
            for fault in 0..if prior_rejection { 5 } else { 4 } {
                let mut case = command_case::Case::unclaimed(paired).await;
                case.backend
                    .original_unavailable
                    .store(true, Ordering::SeqCst);
                case.backend
                    .store
                    .set_user_setting(UserId::new(1), "language_settings", "{revoked provider")
                    .await
                    .unwrap();
                let id = case.request.command.invocation.id.clone();
                let message = format!(
                    "Keep the original edit: 文😀 {}",
                    "quoted \\\" outcome ".repeat(400)
                );
                if prior_rejection {
                    case.backend
                        .store
                        .reject_unstarted_language_action(UserId::new(1), 1, &id, 1, &message, 103)
                        .await
                        .unwrap();
                    let pending = case
                        .backend
                        .store
                        .workspace_command_recovery(UserId::new(1), 1, &id)
                        .await
                        .unwrap()
                        .unwrap();
                    assert!(
                        !pending.command.command_cleanup_confirmed
                            && pending.command.native_settlement_pending
                    );
                    assert!(
                        case.backend
                            .store
                            .acknowledge_language_action(
                                UserId::new(1),
                                1,
                                &id,
                                pending.command.revision,
                                "Keep edits",
                                104
                            )
                            .await
                            .is_err()
                    );
                }
                let disk = std::fs::read(case.root.join("main.notes")).unwrap();
                let editor = case
                    .backend
                    .store
                    .editor_recovery(UserId::new(1), 1)
                    .await
                    .unwrap();
                let original_path = case.root.with_extension("original");
                let mut native_pid = None;
                if matches!(fault, 1 | 3 | 4) {
                    case.stop_native().await;
                    native_pid = Some(case.start_native_process().await);
                }
                let (client, fresh, endpoint) = client(&case, paired);
                if fault == 2 {
                    std::fs::rename(&case.root, &original_path).unwrap();
                    std::fs::create_dir(&case.root).unwrap();
                    std::fs::write(case.root.join("sentinel"), "untouched").unwrap();
                    case.backend
                        .original_unavailable
                        .store(false, Ordering::SeqCst);
                    let record = load(&client, &id).await;
                    let failed = if prior_rejection {
                        client
                            .acknowledge_command(&record, "Keep edits")
                            .await
                            .map(|_| ())
                    } else {
                        let action = client
                            .request(&ActionJournalRequest::Load {
                                id: Some(id.clone()),
                            })
                            .await
                            .unwrap()
                            .unwrap();
                        client
                            .discard_unstarted(&action, TransactionPhase::Committed)
                            .await
                    };
                    assert!(failed.is_err());
                    assert_eq!(writer(&case, paired).await.status, 400);
                    assert_eq!(
                        std::fs::read_to_string(case.root.join("sentinel")).unwrap(),
                        "untouched"
                    );
                    std::fs::remove_file(case.root.join("sentinel")).unwrap();
                    std::fs::remove_dir(&case.root).unwrap();
                    std::fs::rename(&original_path, &case.root).unwrap();
                }
                if fault == 3 {
                    case.backend
                        .drop_settlement_ack
                        .store(true, Ordering::SeqCst);
                }
                if fault == 4 {
                    case.backend
                        .drop_outcome_reply
                        .store(true, Ordering::SeqCst);
                }
                case.backend
                    .original_unavailable
                    .store(false, Ordering::SeqCst);
                if fault == 4 {
                    let record = load(&client, &id).await;
                    let _ = client.acknowledge_command(&record, "Keep edits").await;
                    wait_count(&case.backend.lost_outcome_replies).await;
                    let current = load(&client, &id).await;
                    assert_eq!(current.command.phase, ActionPhase::Rejected);
                    assert!(!current.command.command_cleanup_confirmed);
                    assert_eq!(
                        current.outcome,
                        Some(CommandOutcome::Rejected {
                            message: message.clone()
                        })
                    );
                    let killed = native_pid.unwrap();
                    case.kill_native_process().await;
                    let successor = case.start_native_process().await;
                    assert_ne!(killed, successor);
                    endpoint.store(case.port, Ordering::SeqCst);
                    eprintln!(
                        "discard paired={paired} prior={prior_rejection} fault={fault} killed={killed} successor={successor}"
                    );
                    assert_eq!(writer(&case, paired).await.status, 400);
                    case.backend
                        .original_unavailable
                        .store(false, Ordering::SeqCst);
                }
                // A concurrent original observer can own the first Stop. Join
                // its published cleanup without changing the retained result.
                tokio::time::timeout(Duration::from_secs(8), async {
                    loop {
                        let record = load(&client, &id).await;
                        if record.command.phase == ActionPhase::Acknowledged {
                            break;
                        }
                        let result = if record.command.phase == ActionPhase::Approved {
                            let action = client
                                .request(&ActionJournalRequest::Load {
                                    id: Some(id.clone()),
                                })
                                .await
                                .unwrap()
                                .unwrap();
                            client
                                .discard_unstarted(&action, TransactionPhase::Committed)
                                .await
                        } else {
                            client
                                .acknowledge_command(&record, "Keep edits")
                                .await
                                .map(|_| ())
                        };
                        if result.is_ok() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                .unwrap();
                if fault == 3 {
                    wait_count(&case.backend.lost_settlement_acks).await;
                    assert_eq!(writer(&case, paired).await.status, 400);
                    let killed = native_pid.unwrap();
                    case.kill_native_process().await;
                    let successor = case.start_native_process().await;
                    assert_ne!(killed, successor);
                    endpoint.store(case.port, Ordering::SeqCst);
                    eprintln!(
                        "discard paired={paired} prior={prior_rejection} fault={fault} killed={killed} successor={successor}"
                    );
                    assert_eq!(writer(&case, paired).await.status, 400);
                    case.backend
                        .original_unavailable
                        .store(false, Ordering::SeqCst);
                }
                tokio::time::timeout(Duration::from_secs(8), async {
                    while writer(&case, paired).await.status != 200 {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                .unwrap();
                let after = load(&client, &id).await;
                assert_eq!(after.command.phase, ActionPhase::Acknowledged);
                assert!(!after.pending() && after.command.command_cleanup_confirmed);
                if prior_rejection {
                    let exact = CommandProgress::Finished {
                        outcome: CommandOutcome::Rejected {
                            message: message.clone(),
                        },
                    };
                    assert_eq!(
                        after.outcome,
                        Some(CommandOutcome::Rejected {
                            message: message.clone()
                        })
                    );
                    assert_eq!(
                        client
                            .recover_command(&after, CommandOperation::Status)
                            .await
                            .unwrap(),
                        exact
                    );
                    if matches!(fault, 0 | 2) {
                        let mut status = case.request.clone();
                        status.command.operation = CommandOperation::Status;
                        assert_eq!(progress(send(case.port, paired, status).await), exact);
                        case.host.revoke(
                            if paired { "paired/user:1" } else { "user:1" },
                            Some(1),
                            None,
                        );
                        let mut status = case.request.clone();
                        status.command.operation = CommandOperation::Status;
                        assert_eq!(progress(send(case.port, paired, status).await), exact);
                    }
                }
                assert_eq!(std::fs::read(case.root.join("main.notes")).unwrap(), disk);
                assert_eq!(
                    serde_json::to_value(
                        case.backend
                            .store
                            .editor_recovery(UserId::new(1), 1)
                            .await
                            .unwrap()
                    )
                    .unwrap(),
                    serde_json::to_value(&editor).unwrap()
                );
                assert!(!case.root.join("executions").exists());
                assert_eq!(fresh.load(Ordering::SeqCst), 0);
                case.close().await;
            }
        }
    }
}
