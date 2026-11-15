//! Keep a real retry registered on a paused runtime while a peer settles it.
//! Missing folders after retirement must release jobs, not keep retrying reopen.
use super::{command_case, progress, send, send_json};
use openwebide_bridge::ServerConfig;
use openwebide_core::{
    UserId,
    plugins::languages::intelligence::actions::{
        CommandInvocation, CommandOperation, CommandOutcome, CommandProgress,
    },
};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{atomic::Ordering, mpsc},
    thread,
    time::Duration,
};

struct Paused {
    resume: Option<mpsc::Sender<()>>,
    thread: Option<thread::JoinHandle<(usize, usize, bool)>>,
}
impl Drop for Paused {
    fn drop(&mut self) {
        if let Some(resume) = self.resume.take() {
            let _ = resume.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Paused {
    async fn start(
        home: PathBuf,
        workspace: PathBuf,
        owner: String,
        invocation: CommandInvocation,
        operation: CommandOperation,
        expected: CommandProgress,
    ) -> Self {
        let (resume, resumed) = mpsc::channel();
        let (ready, started) = tokio::sync::oneshot::channel();
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let config = ServerConfig::new_for_test(
                workspace,
                "dummy_secret".into(),
                Some("paired-token".into()),
                home,
            )
            .unwrap();
            let host = config.language_servers.clone();
            let observed = runtime
                .block_on(host.control_command(&owner, 1, &invocation, operation))
                .unwrap();
            assert_eq!(observed, expected);
            // Poll the spawned original actor with its folder absent. Then
            // stop driving this runtime; a peer cannot race this actor's timer.
            runtime.block_on(async {
                tokio::time::sleep(Duration::from_millis(25)).await;
            });
            let before = host.registered_workspace_workers();
            let _ = ready.send(before);
            let _ = resumed.recv();
            let ended = runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(6), async {
                    while host.registered_workspace_workers() != 0 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .is_ok()
            });
            let after = host.registered_workspace_workers();
            host.shutdown();
            drop(config);
            drop(runtime);
            (before, after, ended)
        });
        let actor = Self {
            resume: Some(resume),
            thread: Some(thread),
        };
        let count = tokio::time::timeout(Duration::from_secs(8), started)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(count, 1, "The original recovery job was not registered");
        actor
    }
    async fn finish(mut self) {
        self.resume.take().unwrap().send(()).unwrap();
        let thread = self.thread.take().unwrap();
        let (before, after, ended) = tokio::task::spawn_blocking(move || thread.join().unwrap())
            .await
            .unwrap();
        assert_eq!(before, 1);
        assert!(
            ended,
            "A retired original kept its registered retry with the folder absent"
        );
        assert_eq!(after, 0);
    }
}

#[tokio::test]
async fn peer_retirement_ends_registered_result_and_cancellation_retries_without_the_folder() {
    for paired in [false, true] {
        for variant in [
            "result",
            "cancelled",
            "unadmitted",
            "completed",
            "uncertain",
        ] {
            let cancelled = matches!(variant, "cancelled" | "unadmitted");
            let dispatched = matches!(variant, "completed" | "uncertain");
            let evidence = tempfile::tempdir().unwrap();
            let mut case = if dispatched {
                command_case::Case::retained(paired, evidence.path(), variant == "uncertain").await
            } else if cancelled {
                command_case::Case::cancelled(paired, variant == "unadmitted").await
            } else {
                command_case::Case::new(paired, false).await
            };
            let mut control = case.request.clone();
            control.root.clear();
            control.command.selection = None;
            control.command.operation = if cancelled {
                CommandOperation::Cancel
            } else {
                CommandOperation::Stop
            };
            if cancelled || dispatched {
                case.backend
                    .reject_initial_settlement
                    .store(true, Ordering::SeqCst);
                case.backend
                    .original_unavailable
                    .store(false, Ordering::SeqCst);
            }
            let expected = if dispatched {
                assert!(matches!(
                    progress(send(case.port, paired, case.request.clone()).await),
                    CommandProgress::Running
                ));
                tokio::time::timeout(Duration::from_secs(10), async {
                    while !["owner.json", "child-ready", "heartbeat"]
                        .iter()
                        .all(|name| case.root.join(name).exists())
                    {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                std::fs::write(
                    case.root.join("reply-now"),
                    "receive the real server result",
                )
                .unwrap();
                let outcome = tokio::time::timeout(Duration::from_secs(12), async {
                    loop {
                        if let Some(outcome) = case
                            .backend
                            .store
                            .language_action(
                                UserId::new(1),
                                1,
                                Some(&case.request.command.invocation.id),
                            )
                            .await
                            .unwrap()
                            .unwrap()
                            .outcome
                        {
                            break outcome;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                if variant == "completed" {
                    let CommandOutcome::Completed { result } = &outcome else {
                        panic!("Expected the real completed result")
                    };
                    assert!(
                        result["kept"]
                            .as_str()
                            .unwrap()
                            .contains("原本 😀 \"quoted\"")
                    );
                } else {
                    assert!(matches!(outcome, CommandOutcome::Uncertain { .. }));
                }
                CommandProgress::Finished { outcome }
            } else {
                progress(send(case.port, paired, control).await)
            };
            if cancelled {
                assert_eq!(expected, CommandProgress::Cancelled);
            }
            if cancelled || variant == "completed" {
                tokio::time::timeout(Duration::from_secs(12), async {
                    while case
                        .backend
                        .rejected_initial_settlements
                        .load(Ordering::SeqCst)
                        == 0
                    {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
            }
            if !cancelled {
                assert!(matches!(expected, CommandProgress::Finished { .. }));
            }
            if variant == "uncertain" {
                // Uncertain effects stop the original group before publication.
                // Completed commands deliberately preserve their warm service.
                super::retained::quiet(&case.root).await;
                // An uncertain result waits for manual review; it must not try
                // to settle merely because its original group has stopped.
                tokio::time::timeout(Duration::from_secs(12), async {
                    loop {
                        let current = case
                            .backend
                            .store
                            .workspace_command_recovery(
                                UserId::new(1),
                                1,
                                &case.request.command.invocation.id,
                            )
                            .await
                            .unwrap()
                            .unwrap();
                        if current.command.command_cleanup_confirmed {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                assert_eq!(
                    case.backend
                        .rejected_initial_settlements
                        .load(Ordering::SeqCst),
                    0
                );
            }
            let needs_ack = matches!(
                &expected,
                CommandProgress::Finished {
                    outcome: CommandOutcome::Rejected { .. } | CommandOutcome::Uncertain { .. }
                }
            );
            let id = case.request.command.invocation.id.clone();
            let original = case
                .backend
                .store
                .workspace_command_recovery(UserId::new(1), 1, &id)
                .await
                .unwrap()
                .unwrap();
            assert!(
                original.command.native_settlement_pending
                    && original.command.command_cleanup_confirmed
            );
            assert_eq!(original.command.outcome_digest.is_none(), cancelled);
            case.stop_native().await;
            if dispatched {
                // Closing that host must stop even a SIGTERM-resistant child
                // before we remove its original folder and recover elsewhere.
                super::retained::quiet(&case.root).await;
            }
            let source = std::fs::read(case.root.join("main.notes")).unwrap();
            let saved_editor = case
                .backend
                .store
                .editor_recovery(UserId::new(1), 1)
                .await
                .unwrap();
            let parked = case.root.with_extension("parked");
            std::fs::rename(&case.root, &parked).unwrap();
            let actor = Paused::start(
                case.recovery_home().into(),
                case.root.parent().unwrap().into(),
                if paired { "paired/user:1" } else { "user:1" }.into(),
                case.request.command.invocation.clone(),
                if cancelled {
                    CommandOperation::Cancel
                } else {
                    CommandOperation::Status
                },
                expected,
            )
            .await;
            std::fs::rename(&parked, &case.root).unwrap();
            case.backend
                .reject_initial_settlement
                .store(false, Ordering::SeqCst);
            case.start_native().await;
            if needs_ack {
                let current = case
                    .backend
                    .store
                    .workspace_command_recovery(UserId::new(1), 1, &id)
                    .await
                    .unwrap()
                    .unwrap();
                case.backend
                    .store
                    .acknowledge_language_action(
                        UserId::new(1),
                        1,
                        &id,
                        current.command.revision,
                        "Reviewed the original retained command effects",
                        103,
                    )
                    .await
                    .unwrap();
            }
            super::settlement::retired(case.recovery_home()).await;
            let writer = send_json(
                case.port,
                paired,
                "/exec",
                &json!({"command":"true","cwd":case.root,"timeout_seconds":5}),
            )
            .await;
            assert_eq!(writer.status, 200, "{}", writer.body);
            let calls = case.backend.original_calls.load(Ordering::SeqCst);
            std::fs::rename(&case.root, &parked).unwrap();
            actor.finish().await;
            assert!(!case.root.exists());
            assert_eq!(
                case.backend.original_calls.load(Ordering::SeqCst),
                calls,
                "The retired actor contacted its backend again"
            );
            std::fs::rename(&parked, &case.root).unwrap();
            let retired = case
                .backend
                .store
                .workspace_command_recovery(UserId::new(1), 1, &id)
                .await
                .unwrap()
                .unwrap();
            assert!(!retired.command.native_settlement_pending);
            assert_eq!(
                retired.command.outcome_digest,
                original.command.outcome_digest
            );
            assert_eq!(
                case.backend
                    .store
                    .editor_recovery(UserId::new(1), 1)
                    .await
                    .unwrap(),
                saved_editor
            );
            assert_eq!(std::fs::read(case.root.join("main.notes")).unwrap(), source);
            let executions = case.root.join("executions");
            if dispatched {
                assert_eq!(
                    std::fs::read_to_string(executions).unwrap().lines().count(),
                    1,
                    "Recovery replayed the original command"
                );
            } else {
                assert!(!executions.exists());
            }
            eprintln!(
                "retired retry ended paired={paired} variant={variant} registered=1 remaining=0 folder=absent"
            );
            case.close().await;
        }
    }
}
