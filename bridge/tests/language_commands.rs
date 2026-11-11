//! Authenticated command transports over an existing native service. Database
//! manual grants/claims are covered by the same storage contract in both modes.
mod common;
use common::{HttpResponse, http};
use openwebide_bridge::{ServerConfig, languages::host::Scope, run_server};
use openwebide_core::plugins::{
    languages::{
        intelligence::{
            self, Query,
            actions::{
                ActionReview, AuthorizedCommand, CommandInvocation, CommandOperation,
                CommandOutcome, CommandProgress, HostCommandRequest, origin_fingerprint,
            },
            results::apply_edits,
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

async fn send(port: u16, paired: bool, request: HostCommandRequest) -> HttpResponse {
    let body = serde_json::to_string(&request).unwrap();
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
        "POST /plugins/languages/command HTTP/1.1\r\nHost: localhost:{port}\r\n{origin}Authorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    HttpResponse::parse(&http(port, &wire).await)
}
fn progress(response: HttpResponse) -> CommandProgress {
    assert_eq!(response.status, 200, "{}", response.body);
    serde_json::from_str(&response.body).unwrap()
}

#[tokio::test]
async fn authenticated_command_transports_join_outcomes_and_fence_delayed_dispatch() {
    for paired in [false, true] {
        for (stopped, branch_changed, round_trip) in [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (false, true, true),
        ] {
            let root = tempfile::tempdir().unwrap();
            if branch_changed {
                git(root.path(), &["init", "-q", "-b", "main"]);
                git(root.path(), &["commit", "--allow-empty", "-qm", "initial"]);
            }
            std::fs::write(root.path().join("main.notes"), SOURCE).unwrap();
            let script = root.path().join("peer.py");
            std::fs::write(
                &script,
                include_str!("../../tools/fixtures/commands_server.py"),
            )
            .unwrap();
            let mut prepared = receipt();
            prepared.manifest.compatibility.plugin_api = 9;
            prepared.manifest.executable = None;
            prepared.manifest.contributions.tools.clear();
            prepared.manifest.contributions.skills.clear();
            prepared.manifest.contributions.languages=serde_json::from_value(json!([{"id":"notes","name":"Notes","extensions":["notes"],"features":{"languageServer":{"command":"python3","args":[script,"success"]}}}])).unwrap();
            let config = ServerConfig::new(
                root.path().into(),
                "dummy_secret".into(),
                Some("paired-token".into()),
            );
            let host = config.language_servers.clone();
            let scope = Scope {
                owner: if paired { "paired/user:42" } else { "user:42" }.into(),
                project: 7,
                root: root.path().into(),
                provider: prepared.storage_namespace(),
                digest: prepared.digest.clone(),
                server: intelligence::declared_server(&prepared, "notes")
                    .unwrap()
                    .clone(),
            };
            let mut input = intelligence::Request {
                version: intelligence::VERSION,
                client: "client".into(),
                document: "document".into(),
                request: 1,
                revision: 1,
                language: "notes".into(),
                path: "main.notes".into(),
                source: SOURCE.into(),
                source_digest: source_digest(SOURCE),
                query: Query::CodeActions {
                    range: Default::default(),
                },
            };
            let actions = host.query(scope.clone(), &input).await.unwrap();
            input.request += 1;
            input.query = Query::ReviewAction {
                id: actions.result[0]["id"].as_str().unwrap().into(),
            };
            let reviewed = host.query(scope.clone(), &input).await.unwrap();
            let review: ActionReview = serde_json::from_value(reviewed.result).unwrap();
            let command = review.command.as_ref().unwrap();
            let invocation = CommandInvocation {
                id: "b".repeat(32),
                client: input.client.clone(),
                command: command.id.clone(),
                server: command.server.clone(),
                origin: origin_fingerprint(&input).unwrap(),
                review: review.fingerprint().unwrap(),
                approval: "c".repeat(64),
            };
            let mut sync = input.clone();
            sync.request += 1;
            sync.revision += 1;
            sync.query = Query::Synchronize;
            sync.source = apply_edits(SOURCE, &review.edit.files[0].edits).unwrap();
            sync.source_digest = source_digest(&sync.source);
            host.query(scope, &sync).await.unwrap();
            if branch_changed {
                let head = root.path().join(".git/HEAD");
                let modified = std::fs::metadata(&head).unwrap().modified().unwrap();
                git(root.path(), &["checkout", "-q", "-b", "topic"]);
                if round_trip {
                    git(root.path(), &["checkout", "-q", "main"]);
                    std::fs::File::options()
                        .write(true)
                        .open(&head)
                        .unwrap()
                        .set_times(std::fs::FileTimes::new().set_modified(modified))
                        .unwrap();
                }
            }
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(run_server(listener, config));
            let mut request = HostCommandRequest {
                user: Some(42),
                root: root.path().to_string_lossy().into(),
                command: AuthorizedCommand {
                    selection: intelligence::Selection {
                        user: 42,
                        project: 7,
                        prepared,
                        input,
                    },
                    invocation,
                    operation: CommandOperation::Start,
                },
            };
            let mut forged = request.clone();
            forged.command.invocation.review = "d".repeat(64);
            assert_eq!(send(port, paired, forged).await.status, 400);
            let mut other = request.clone();
            other.command.selection.user = 43;
            assert_eq!(
                send(port, paired, other).await.status,
                if paired { 400 } else { 401 }
            );
            if stopped {
                request.command.operation = CommandOperation::Stop;
                request.root.clear();
                let response = send(port, paired, request.clone()).await;
                assert!(matches!(
                    progress(response),
                    CommandProgress::Finished {
                        outcome: CommandOutcome::Rejected { .. }
                    }
                ));
                request.root = root.path().to_string_lossy().into();
                request.command.operation = CommandOperation::Start;
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
            request.root.clear();
            let outcome = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let response = send(port, paired, request.clone()).await;
                    if let CommandProgress::Finished { outcome } = progress(response) {
                        return outcome;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                matches!(&outcome, CommandOutcome::Completed { .. }),
                !stopped && !branch_changed
            );
            if branch_changed {
                assert!(
                    matches!(&outcome, CommandOutcome::Rejected { message } if message.contains("Git revision changed"))
                );
            }
            // Recover after removing the process/document cache, without
            // recreating a service or replaying a command.
            host.revoke(
                if paired { "paired/user:42" } else { "user:42" },
                Some(7),
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
                progress(send(port, paired, request.clone()).await),
                CommandProgress::Finished {
                    outcome: outcome.clone()
                }
            );
            request.command.operation = CommandOperation::Start;
            request.root = root.path().to_string_lossy().into();
            assert_eq!(
                progress(send(port, paired, request).await),
                CommandProgress::Finished { outcome }
            );
            assert_eq!(
                std::fs::read_to_string(root.path().join("executions"))
                    .map_or(0, |text| text.lines().count()),
                usize::from(!stopped && !branch_changed)
            );
            assert_eq!(
                std::fs::read_to_string(root.path().join("main.notes")).unwrap(),
                SOURCE
            );
            server.abort();
            host.shutdown();
        }
    }
}
