//! Authenticated command transports over real reviewed file/command ownership.
//! Service and paired HTTP callbacks use production Store grants and journal CAS.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "language_commands/callback.rs"]
mod callback;
#[path = "language_commands/cancellation.rs"]
mod cancellation;
#[path = "common/command_case.rs"]
mod command_case;
#[path = "common/command_journal.rs"]
mod command_journal;
mod common;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "language_commands/deleted_project.rs"]
mod deleted_project;
#[path = "language_commands/discard.rs"]
mod discard;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "language_commands/dispatch.rs"]
mod dispatch;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "language_commands/peer_retirement.rs"]
mod peer_retirement;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "language_commands/prepared.rs"]
mod prepared;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "language_commands/retained.rs"]
mod retained;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "language_commands/settlement.rs"]
mod settlement;
use common::{HttpResponse, http};
use openwebide_bridge::{ServerConfig, run_server};
use openwebide_core::plugins::{
    languages::{
        intelligence::{
            self, Query,
            actions::{
                AuthorizedCommand, CommandInvocation, CommandOperation, CommandOutcome,
                CommandProgress, HostCommandRequest, origin_fingerprint,
            },
        },
        source_digest,
    },
    testing::receipt,
};
use serde_json::json;
use std::time::Duration;

const SOURCE: &str = "text 文😀\r\n";

fn git(root: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.org",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn send_json(
    port: u16,
    paired: bool,
    path: &str,
    request: &impl serde::Serialize,
) -> HttpResponse {
    let body = serde_json::to_string(request).unwrap();
    let origin = if paired {
        "Origin: http://localhost:3000\r\n"
    } else {
        ""
    };
    let token = if paired {
        "paired-token"
    } else {
        "dummy_secret"
    };
    let wire = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost:{port}\r\n{origin}Authorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    HttpResponse::parse(&http(port, &wire).await)
}
async fn send(port: u16, paired: bool, request: HostCommandRequest) -> HttpResponse {
    send_json(port, paired, "/plugins/languages/command", &request).await
}
fn progress(response: HttpResponse) -> CommandProgress {
    assert_eq!(response.status, 200, "{}", response.body);
    serde_json::from_str(&response.body).unwrap()
}

#[tokio::test]
async fn authenticated_command_transports_join_outcomes_and_fence_delayed_dispatch() {
    for paired in [false, true] {
        for (stopped, branch_changed, round_trip, restarted, recorded_restart) in [
            (false, false, false, false, false),
            (true, false, false, false, false),
            (true, false, false, true, false),
            (true, false, false, false, true),
            (false, true, false, false, false),
            (false, true, true, false, false),
        ] {
            let mut case = command_case::Case::new(paired, branch_changed).await;
            if restarted {
                case.restart_native().await;
            }
            let mut port = case.port;
            let root = case.root.clone();
            if branch_changed {
                let head = root.join(".git/HEAD");
                let modified = std::fs::metadata(&head).unwrap().modified().unwrap();
                git(&root, &["checkout", "-q", "-b", "topic"]);
                if round_trip {
                    git(&root, &["checkout", "-q", "main"]);
                    std::fs::File::options()
                        .write(true)
                        .open(&head)
                        .unwrap()
                        .set_times(std::fs::FileTimes::new().set_modified(modified))
                        .unwrap();
                }
            }
            let mut request = case.request.clone();
            let mut forged = request.clone();
            forged.command.invocation.review = "d".repeat(64);
            assert_eq!(send(port, paired, forged).await.status, 400);
            let mut other = request.clone();
            other.command.user = 2;
            other.command.selection.as_mut().unwrap().user = 2;
            assert_eq!(
                send(port, paired, other).await.status,
                if paired { 400 } else { 401 }
            );
            if stopped {
                request.command.operation = CommandOperation::Stop;
                request.command.selection = None;
                request.root.clear();
                assert!(matches!(
                    progress(send(port, paired, request.clone()).await),
                    CommandProgress::Finished {
                        outcome: CommandOutcome::Rejected { .. }
                    }
                ));
                request = case.request.clone();
            }
            let first = progress(send(port, paired, request.clone()).await);
            if stopped {
                assert!(matches!(
                    first,
                    CommandProgress::Finished {
                        outcome: CommandOutcome::Rejected { .. }
                    }
                ));
            }
            request.command.operation = CommandOperation::Status;
            request.command.selection = None;
            request.root.clear();
            request.authority = None;
            request.journal = None;
            let outcome = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let CommandProgress::Finished { outcome } =
                        progress(send(port, paired, request.clone()).await)
                    {
                        break outcome;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                matches!(&outcome, CommandOutcome::Completed { .. }),
                !stopped && !branch_changed,
                "{outcome:?}"
            );
            if branch_changed {
                assert!(
                    matches!(&outcome,CommandOutcome::Rejected {message} if message.contains("Git revision changed")),
                    "{outcome:?}"
                );
            }
            if recorded_restart {
                case.restart_native().await;
                port = case.port;
            }
            // Removing the process/document cache must preserve the original
            // result. Controls carry no provider/source/folder payload.
            case.host.revoke(
                if paired { "paired/user:1" } else { "user:1" },
                Some(1),
                None,
            );
            assert_eq!(
                progress(send(port, paired, request.clone()).await),
                CommandProgress::Finished {
                    outcome: outcome.clone()
                }
            );
            request.command.operation = CommandOperation::Stop;
            assert_eq!(
                progress(send(port, paired, request).await),
                CommandProgress::Finished {
                    outcome: outcome.clone()
                }
            );
            assert_eq!(
                progress(send(port, paired, case.request.clone()).await),
                CommandProgress::Finished {
                    outcome: outcome.clone()
                }
            );
            assert_eq!(
                std::fs::read_to_string(root.join("executions"))
                    .map_or(0, |text| text.lines().count()),
                usize::from(!stopped && !branch_changed)
            );
            assert_eq!(
                std::fs::read_to_string(root.join("main.notes")).unwrap(),
                SOURCE
            );
            {
                let current = case
                    .backend
                    .store
                    .workspace_command_recovery(
                        openwebide_core::UserId::new(1),
                        1,
                        &case.request.command.invocation.id,
                    )
                    .await
                    .unwrap()
                    .unwrap();
                if current.command.phase == intelligence::actions::ActionPhase::Rejected {
                    case.backend
                        .store
                        .acknowledge_language_action(
                            openwebide_core::UserId::new(1),
                            1,
                            &case.request.command.invocation.id,
                            current.command.revision,
                            "Reviewed original applied edit; no command was sent",
                            103,
                        )
                        .await
                        .unwrap();
                }
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let current = case
                            .backend
                            .store
                            .workspace_command_recovery(
                                openwebide_core::UserId::new(1),
                                1,
                                &case.request.command.invocation.id,
                            )
                            .await
                            .unwrap()
                            .unwrap();
                        if !current.command.native_settlement_pending {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                assert!(
                    case.backend
                        .store
                        .workspace_activity(openwebide_core::UserId::new(1), 1)
                        .await
                        .unwrap()
                        .ensure_idle()
                        .is_ok()
                );
                // Backend idle alone is insufficient: an ordinary native
                // process must also pass the original physical folder gate.
                let resumed = send_json(
                    port,
                    paired,
                    "/exec",
                    &json!({"command":"true","cwd":root.to_string_lossy(),"timeout_seconds":5}),
                )
                .await;
                assert_eq!(resumed.status, 200, "{}", resumed.body);
                let resumed: openwebide_core::CommandOutcome =
                    serde_json::from_str(&resumed.body).unwrap();
                assert!(resumed.is_success(), "{resumed:?}");
            }
            case.close().await;
        }
    }
}

#[tokio::test]
async fn recorded_commands_resume_without_a_browser_after_original_context_returns() {
    use std::sync::atomic::Ordering;
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Fault {
        None,
        Callback,
        Folder,
        LateFolder,
        SettlementAck,
    }
    for paired in [false, true] {
        for fault in [
            Fault::None,
            Fault::Callback,
            Fault::Folder,
            Fault::LateFolder,
            Fault::SettlementAck,
        ] {
            let mut case = command_case::Case::new(paired, false).await;
            let root = case.root.clone();
            let original = same_file::Handle::from_path(&root).unwrap();
            case.stop_native().await;
            let original_pid = case.start_native_process().await;
            let mut control = case.request.clone();
            control.command.operation = CommandOperation::Stop;
            control.command.selection = None;
            control.root.clear();
            let outcome = match progress(send(case.port, paired, control.clone()).await) {
                CommandProgress::Finished {
                    outcome: outcome @ CommandOutcome::Rejected { .. },
                } => outcome,
                other => panic!("Expected the original no-dispatch rejection: {other:?}"),
            };
            case.kill_native_process().await;
            let calls = case.backend.original_calls.load(Ordering::SeqCst);
            if fault == Fault::Callback {
                case.backend
                    .original_unavailable
                    .store(true, Ordering::SeqCst);
            }
            if fault == Fault::SettlementAck {
                case.backend
                    .drop_settlement_ack
                    .store(true, Ordering::SeqCst);
            }
            if fault == Fault::LateFolder {
                case.backend.original_scope_calls.store(0, Ordering::SeqCst);
                case.backend.block_scope_at.store(3, Ordering::SeqCst);
            }
            let parked = root.with_file_name("parked-original");
            let replacement = root.with_file_name("retained-replacement");
            if fault == Fault::Folder {
                std::fs::rename(&root, &parked).unwrap();
                std::fs::create_dir(&root).unwrap();
                std::fs::write(root.join("sentinel"), "replacement must remain untouched").unwrap();
            }
            let successor_pid = case.start_native_process().await;
            assert_ne!(original_pid, successor_pid);
            if fault == Fault::LateFolder {
                tokio::time::timeout(Duration::from_secs(5), async {
                    while !case.backend.scope_blocked.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                // The publisher has already entered its settlement wait. Its
                // first observation is held without a physical folder lease.
                std::fs::rename(&root, &parked).unwrap();
                std::fs::create_dir(&root).unwrap();
                std::fs::write(root.join("sentinel"), "replacement must remain untouched").unwrap();
                case.backend.release_scope.add_permits(1);
                tokio::time::timeout(Duration::from_secs(5), async {
                    while case.backend.original_scope_calls.load(Ordering::SeqCst) < 4 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
            }
            if fault == Fault::Callback {
                tokio::time::timeout(Duration::from_secs(5), async {
                    while case.backend.original_calls.load(Ordering::SeqCst) == calls {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
            }
            if matches!(fault, Fault::Callback | Fault::Folder | Fault::LateFolder) {
                control.command.operation = CommandOperation::Status;
                control.authority = None;
                control.journal = None;
                for _ in 0..8 {
                    assert_eq!(
                        progress(send(case.port, paired, control.clone()).await),
                        CommandProgress::Finished {
                            outcome: outcome.clone()
                        }
                    );
                }
                let blocked = send_json(
                    case.port,
                    paired,
                    "/exec",
                    &json!({"command":"true","cwd":root,"timeout_seconds":5}),
                )
                .await;
                assert_eq!(blocked.status, 400, "{}", blocked.body);
            }
            let current = case
                .backend
                .store
                .workspace_command_recovery(
                    openwebide_core::UserId::new(1),
                    1,
                    &case.request.command.invocation.id,
                )
                .await
                .unwrap()
                .unwrap();
            assert!(current.command.native_settlement_pending);
            case.backend
                .store
                .acknowledge_language_action(
                    openwebide_core::UserId::new(1),
                    1,
                    &case.request.command.invocation.id,
                    current.command.revision,
                    "Reviewed the original no-dispatch result",
                    103,
                )
                .await
                .unwrap();
            if fault == Fault::SettlementAck {
                tokio::time::timeout(Duration::from_secs(5), async {
                    while case.backend.lost_settlement_acks.load(Ordering::SeqCst) == 0 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                assert!(
                    !case
                        .backend
                        .store
                        .workspace_command_recovery(
                            openwebide_core::UserId::new(1),
                            1,
                            &case.request.command.invocation.id
                        )
                        .await
                        .unwrap()
                        .unwrap()
                        .command
                        .native_settlement_pending
                );
                let blocked = send_json(
                    case.port,
                    paired,
                    "/exec",
                    &json!({"command":"true","cwd":root,"timeout_seconds":5}),
                )
                .await;
                assert_eq!(
                    blocked.status, 400,
                    "A committed backend settlement without its native ACK released the folder: {}",
                    blocked.body
                );
            }
            if matches!(fault, Fault::Folder | Fault::LateFolder) {
                assert_eq!(
                    std::fs::read_to_string(root.join("sentinel")).unwrap(),
                    "replacement must remain untouched"
                );
                std::fs::rename(&root, &replacement).unwrap();
                std::fs::rename(&parked, &root).unwrap();
            }
            case.backend
                .original_unavailable
                .store(false, Ordering::SeqCst);
            // No further command control or language request wakes recovery.
            // Startup must finish through the original callback and native owner.
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let response = send_json(
                        case.port,
                        paired,
                        "/exec",
                        &json!({"command":"true","cwd":root,"timeout_seconds":5}),
                    )
                    .await;
                    if response.status == 200 {
                        let output: openwebide_core::CommandOutcome =
                            serde_json::from_str(&response.body).unwrap();
                        assert!(output.is_success(), "{output:?}");
                        break;
                    }
                    assert_eq!(response.status, 400, "{}", response.body);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            case.backend
                .store
                .workspace_activity(openwebide_core::UserId::new(1), 1)
                .await
                .unwrap()
                .ensure_idle()
                .unwrap();
            assert_eq!(same_file::Handle::from_path(&root).unwrap(), original);
            assert!(!root.join("executions").exists());
            assert_eq!(
                std::fs::read_to_string(root.join("main.notes")).unwrap(),
                SOURCE
            );
            if matches!(fault, Fault::Folder | Fault::LateFolder) {
                assert_eq!(
                    std::fs::read_to_string(replacement.join("sentinel")).unwrap(),
                    "replacement must remain untouched"
                );
            }
            println!(
                "original restart recovery paired={paired} fault={fault:?} killed_pid={original_pid} successor_pid={successor_pid}: no RPC replay, original result settled and native writer resumed"
            );
            case.close().await;
        }
    }
}

#[tokio::test]
async fn a_peer_settlement_retires_the_other_original_publisher_without_new_callbacks() {
    use std::sync::atomic::Ordering;
    for paired in [false, true] {
        let mut case = command_case::Case::new(paired, false).await;
        let mut control = case.request.clone();
        control.command.operation = CommandOperation::Stop;
        control.command.selection = None;
        control.root.clear();
        assert!(matches!(
            progress(send(case.port, paired, control).await),
            CommandProgress::Finished {
                outcome: CommandOutcome::Rejected { .. }
            }
        ));
        case.stop_native().await;
        case.backend.original_scope_calls.store(0, Ordering::SeqCst);
        case.backend.block_scope_at.store(3, Ordering::SeqCst);
        let first_pid = case.start_native_process().await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !case.backend.scope_blocked.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // The first actor is waiting on its first settlement-loop observation,
        // without a physical lease. A second real process can settle the owner.
        let peer = case.spawn_native_peer().await;
        assert_ne!(first_pid, peer.pid);
        tokio::time::timeout(Duration::from_secs(5), async {
            while case.backend.original_scope_calls.load(Ordering::SeqCst) < 5 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let current = case
            .backend
            .store
            .workspace_command_recovery(
                openwebide_core::UserId::new(1),
                1,
                &case.request.command.invocation.id,
            )
            .await
            .unwrap()
            .unwrap();
        case.backend
            .store
            .acknowledge_language_action(
                openwebide_core::UserId::new(1),
                1,
                &case.request.command.invocation.id,
                current.command.revision,
                "Reviewed the original peer-owned result",
                103,
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let response = send_json(
                    peer.port,
                    paired,
                    "/exec",
                    &json!({"command":"true","cwd":case.root,"timeout_seconds":5}),
                )
                .await;
                if response.status == 200 {
                    let output: openwebide_core::CommandOutcome =
                        serde_json::from_str(&response.body).unwrap();
                    assert!(output.is_success(), "{output:?}");
                    break;
                }
                assert_eq!(response.status, 400, "{}", response.body);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        case.backend
            .store
            .workspace_activity(openwebide_core::UserId::new(1), 1)
            .await
            .unwrap()
            .ensure_idle()
            .unwrap();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        settlement::retired(case.recovery_home()).await;
        let callbacks = case.backend.original_calls.load(Ordering::SeqCst);
        // Deliver the older pre-settlement observation only after the peer's
        // native retirement. The stale actor must recheck native presence.
        case.backend.release_scope.add_permits(1);
        tokio::time::timeout(Duration::from_secs(5), async {
            while !case.backend.scope_released.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let response = send_json(
            case.port,
            paired,
            "/exec",
            &json!({"command":"true","cwd":case.root,"timeout_seconds":5}),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);
        // Observe quiescence with both processes alive, across another normal
        // retry opportunity. This detects a stale publisher retaining its job.
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(
            case.backend.original_calls.load(Ordering::SeqCst),
            callbacks,
            "A retired original publisher kept making callbacks after its peer settled"
        );
        assert!(!case.root.join("executions").exists());
        assert_eq!(
            std::fs::read_to_string(case.root.join("main.notes")).unwrap(),
            SOURCE
        );
        println!(
            "peer retirement paired={paired} original_pid={first_pid} peer_pid={}: stale observation joined without further callbacks or RPC replay",
            peer.pid
        );
        peer.stop().await;
        case.close().await;
    }
}

#[tokio::test]
async fn command_preparation_requires_authentication_for_service_and_paired_transports() {
    let root = tempfile::tempdir().unwrap();
    let mut prepared = receipt();
    prepared.manifest.compatibility.plugin_api = 9;
    prepared.manifest.executable = None;
    prepared.manifest.contributions.tools.clear();
    prepared.manifest.contributions.skills.clear();
    prepared.manifest.contributions.languages=serde_json::from_value(json!([{"id":"notes","name":"Notes","extensions":["notes"],"features":{"languageServer":{"command":"unused-lsp"}}}])).unwrap();
    let input = intelligence::Request {
        version: intelligence::VERSION,
        client: "client".into(),
        document: "document".into(),
        request: 1,
        revision: 1,
        language: "notes".into(),
        path: "main.notes".into(),
        source: SOURCE.into(),
        source_digest: source_digest(SOURCE),
        query: Query::ReviewAction { id: "a".repeat(32) },
    };
    let invocation = CommandInvocation {
        id: "b".repeat(32),
        client: input.client.clone(),
        command: "a".repeat(32),
        server: "missing-original-service".into(),
        origin: origin_fingerprint(&input).unwrap(),
        review: "c".repeat(64),
        approval: "d".repeat(64),
    };
    let mut request = HostCommandRequest {
        user: Some(42),
        root: root.path().to_string_lossy().into(),
        command: AuthorizedCommand {
            user: 42,
            project: 7,
            selection: Some(intelligence::Selection {
                user: 42,
                project: 7,
                prepared,
                input,
            }),
            invocation,
            operation: CommandOperation::Start,
            progress: None,
        },
        authority: None,
        journal: None,
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let recovery_home = tempfile::tempdir().unwrap();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let config = ServerConfig::new_for_test(
        root.path().into(),
        "dummy_secret".into(),
        Some("paired-token".into()),
        recovery_home.path().into(),
    )
    .unwrap();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let config = ServerConfig::new(
        root.path().into(),
        "dummy_secret".into(),
        Some("paired-token".into()),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(run_server(listener, config));
    for paired in [false, true] {
        request.user = (!paired).then_some(42);
        let body = serde_json::to_string(&request).unwrap();
        let origin = if paired {
            "Origin: http://localhost:3000\r\n"
        } else {
            ""
        };
        for token in [
            None,
            Some("invalid-token"),
            Some(if paired {
                "paired-token"
            } else {
                "dummy_secret"
            }),
        ] {
            let authorization = token.map_or_else(String::new, |token| {
                format!("Authorization: Bearer {token}\r\n")
            });
            let wire = format!(
                "POST /plugins/languages/command/prepare HTTP/1.1\r\nHost: localhost:{port}\r\n{origin}{authorization}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let response = HttpResponse::parse(&http(port, &wire).await);
            let valid = token
                == Some(if paired {
                    "paired-token"
                } else {
                    "dummy_secret"
                });
            assert_eq!(
                response.status,
                if valid { 400 } else { 401 },
                "paired={paired} valid={valid}: {}",
                response.body
            );
            if valid {
                assert!(response.body.contains("original"), "{}", response.body);
            }
        }
    }
    server.abort();
}
