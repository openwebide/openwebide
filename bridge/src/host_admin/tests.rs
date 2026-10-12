use super::*;
use crate::exec::{ExecOutput, ExecutionFuture, GitRequest, GitResponse, SpawnSpec};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, body::Incoming, server::conn::http1, service::service_fn};
use openwebide_core::{
    CommandOutcome,
    host_admin::{HostCommand, HostConnection, HostPlan},
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::{net::TcpListener, sync::Notify};

struct Journal {
    connection: HostConnection,
    operation: HostOperation,
    fail_save: bool,
}
impl Journal {
    fn command(&mut self, command: HostJournalCommand) -> Result<HostJournalResult, String> {
        match command {
            HostJournalCommand::Connection => {
                Ok(HostJournalResult::Connection(self.connection.clone()))
            }
            HostJournalCommand::List { user, session }
            | HostJournalCommand::Get { user, session, .. }
                if user != 7 || session != 8 =>
            {
                Err("wrong scope".into())
            }
            HostJournalCommand::List { .. } => {
                Ok(HostJournalResult::Operations(vec![self.operation.clone()]))
            }
            HostJournalCommand::Get { .. } => Ok(HostJournalResult::Operation(Box::new(
                self.operation.clone(),
            ))),
            HostJournalCommand::Claim { .. } => {
                assert_eq!(self.operation.state, OperationState::Prepared);
                self.operation.state = OperationState::Running;
                Ok(HostJournalResult::Operation(Box::new(
                    self.operation.clone(),
                )))
            }
            HostJournalCommand::Save { operation } => {
                if self.fail_save {
                    return Err("journal unavailable".into());
                }
                self.operation = operation;
                Ok(HostJournalResult::Saved)
            }
            HostJournalCommand::Active => Ok(HostJournalResult::Operations(
                if self.operation.state.active() {
                    vec![self.operation.clone()]
                } else {
                    vec![]
                },
            )),
            HostJournalCommand::Prepare { .. } => panic!("Fixture starts with a prepared plan"),
        }
    }
}
#[derive(Default)]
struct Execution {
    expected_destination: Option<String>,
    probes: AtomicUsize,
    changes: AtomicUsize,
    checks: AtomicUsize,
    new_boot: AtomicBool,
    hold_change: AtomicBool,
    released: Arc<Notify>,
}
impl ToolExecution for Execution {
    fn run_command(&self, spec: SpawnSpec) -> ExecutionFuture<ExecOutput> {
        assert_eq!(spec.command, "ssh");
        if let Some(destination) = &self.expected_destination {
            let separator = spec.args.iter().position(|arg| arg == "--").unwrap();
            assert_eq!(&spec.args[separator + 1], destination);
        }
        assert!(spec.args.contains(&"-oStrictHostKeyChecking=yes".into()));
        let script = spec.args.last().unwrap();
        let (output, hold) = if script.starts_with("sh -c ") {
            self.probes.fetch_add(1, Ordering::SeqCst);
            (
                format!(
                    "host\nDebian Linux\nmachine\n{}\nCPU\n8\n16000\n8000\n/home/user\n/bin/bash\n",
                    if self.new_boot.load(Ordering::SeqCst) {
                        "new-boot"
                    } else {
                        "boot"
                    }
                ),
                false,
            )
        } else if script.contains("'change'") {
            self.changes.fetch_add(1, Ordering::SeqCst);
            ("changed".into(), self.hold_change.load(Ordering::SeqCst))
        } else {
            assert!(script.contains("'verify'"), "Unexpected SSH command");
            self.checks.fetch_add(1, Ordering::SeqCst);
            ("verified".into(), false)
        };
        let released = self.released.clone();
        Box::pin(async move {
            if hold {
                released.notified().await;
            }
            Ok(CommandOutcome {
                exit_code: Some(0),
                stdout: output,
                stderr: String::new(),
            })
        })
    }
    fn git(&self, _request: GitRequest) -> ExecutionFuture<GitResponse> {
        panic!("Host administration must not call project Git")
    }
}
struct Fixture {
    backend: Arc<crate::backend_client::BackendClient>,
    journal: Arc<Mutex<Journal>>,
    execution: Arc<Execution>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
fn command(program: &str) -> HostCommand {
    HostCommand {
        program: program.into(),
        args: vec![],
        cwd: "/tmp".into(),
        elevated: false,
        interactive: false,
        timeout_seconds: 30,
    }
}
async fn fixture() -> Fixture {
    let now = now();
    let operation = HostOperation {
        id: i64::from(rand::random::<u32>()),
        user_id: 7,
        session_id: 8,
        request_id: "request".into(),
        plan: HostPlan {
            target: "host@machine".into(),
            title: "Change and verify".into(),
            steps: vec![command("change")],
            checks: vec![command("verify")],
            expects_reboot: false,
        },
        boot_id: "boot".into(),
        connection_revision: 1,
        steps_started: 0,
        input_token: None,
        live_output: String::new(),
        state: OperationState::Prepared,
        outputs: vec![],
        detail: String::new(),
        created_at: now,
        updated_at: now,
    };
    let journal = Arc::new(Mutex::new(Journal {
        connection: HostConnection {
            destination: "admin@host".into(),
            revision: 1,
            ..Default::default()
        },
        operation,
        fail_save: false,
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = journal.clone();
    let server = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let state = state.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request: Request<Incoming>| {
                    let state = state.clone();
                    async move {
                        assert_eq!(request.uri().path(), "/api/host/journal");
                        assert_eq!(
                            request.headers()["authorization"],
                            "Bearer test-service-secret"
                        );
                        let bytes = request.into_body().collect().await.unwrap().to_bytes();
                        let command = serde_json::from_slice(&bytes).unwrap();
                        let result = state.lock().unwrap().command(command);
                        let (status, body) = match result {
                            Ok(value) => (200, serde_json::to_string(&value).unwrap()),
                            Err(error) => (503, serde_json::json!({"error":error}).to_string()),
                        };
                        Ok::<_, std::convert::Infallible>(
                            Response::builder()
                                .status(status)
                                .body(Full::new(Bytes::from(body)))
                                .unwrap(),
                        )
                    }
                });
                let _ = http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(socket), service)
                    .await;
            });
        }
    });
    Fixture {
        backend: Arc::new(crate::backend_client::BackendClient::new(
            format!("http://127.0.0.1:{port}/api"),
            "test-service-secret".into(),
            crate::http_client::ReqwestHttpClient::default(),
        )),
        journal,
        execution: Arc::new(Execution::default()),
        server,
    }
}
async fn wait_for(fixture: &Fixture, state: OperationState) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if fixture.journal.lock().unwrap().operation.state == state {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn resident_host_operation_survives_request_completion_and_recovery_does_not_duplicate_it() {
    let fixture = fixture().await;
    fixture.execution.hold_change.store(true, Ordering::SeqCst);
    let id = fixture.journal.lock().unwrap().operation.id;
    let response = request(
        fixture.backend.clone(),
        fixture.execution.clone(),
        7,
        8,
        HostRequest::Apply { id },
    )
    .await
    .unwrap();
    assert!(
        matches!(response, HostResponse::Operation(operation) if operation.state == OperationState::Running)
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while fixture.execution.changes.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    recover(fixture.backend.clone(), fixture.execution.clone())
        .await
        .unwrap();
    assert_eq!(fixture.execution.changes.load(Ordering::SeqCst), 1);
    fixture.execution.released.notify_one();
    wait_for(&fixture, OperationState::Succeeded).await;
    recover(fixture.backend.clone(), fixture.execution.clone())
        .await
        .unwrap();
    assert_eq!(fixture.execution.changes.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.execution.checks.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn reboot_recovery_verifies_once_and_ordinary_restart_never_replays_changes() {
    for reboot in [false, true] {
        let fixture = fixture().await;
        {
            let mut journal = fixture.journal.lock().unwrap();
            journal.operation.state = OperationState::Running;
            journal.operation.plan.expects_reboot = reboot;
            journal.operation.steps_started = 1;
            journal.operation.input_token = Some("stale".into());
        }
        fixture.execution.new_boot.store(true, Ordering::SeqCst);
        recover(fixture.backend.clone(), fixture.execution.clone())
            .await
            .unwrap();
        wait_for(
            &fixture,
            if reboot {
                OperationState::Succeeded
            } else {
                OperationState::Interrupted
            },
        )
        .await;
        recover(fixture.backend.clone(), fixture.execution.clone())
            .await
            .unwrap();
        assert_eq!(fixture.execution.changes.load(Ordering::SeqCst), 0);
        assert_eq!(
            fixture.execution.checks.load(Ordering::SeqCst),
            usize::from(reboot)
        );
        assert!(
            fixture
                .journal
                .lock()
                .unwrap()
                .operation
                .input_token
                .is_none()
        );
    }
}
#[tokio::test]
async fn offline_reboot_expiry_and_unconfigured_history_do_not_need_ssh() {
    let fixture = fixture().await;
    {
        let mut journal = fixture.journal.lock().unwrap();
        journal.operation.state = OperationState::AwaitingReconnect;
        journal.operation.plan.expects_reboot = true;
        journal.operation.updated_at = 0;
    }
    recover(fixture.backend.clone(), fixture.execution.clone())
        .await
        .unwrap();
    wait_for(&fixture, OperationState::Interrupted).await;
    assert_eq!(fixture.execution.probes.load(Ordering::SeqCst), 0);
    fixture.journal.lock().unwrap().connection = HostConnection::default();
    assert!(matches!(
        request(
            fixture.backend.clone(),
            fixture.execution.clone(),
            7,
            8,
            HostRequest::Operations
        )
        .await
        .unwrap(),
        HostResponse::Operations(_)
    ));
    assert!(
        request(
            fixture.backend.clone(),
            fixture.execution.clone(),
            9,
            8,
            HostRequest::Inspect
        )
        .await
        .is_err()
    );
    assert_eq!(fixture.execution.probes.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn journal_outage_prevents_execution_and_retry_does_not_replay_the_operation() {
    let fixture = fixture().await;
    fixture.journal.lock().unwrap().fail_save = true;
    let id = fixture.journal.lock().unwrap().operation.id;
    request(
        fixture.backend.clone(),
        fixture.execution.clone(),
        7,
        8,
        HostRequest::Apply { id },
    )
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while live_operations().lock().unwrap().contains(&id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(fixture.execution.changes.load(Ordering::SeqCst), 0);
    fixture.journal.lock().unwrap().fail_save = false;
    recover(fixture.backend.clone(), fixture.execution.clone())
        .await
        .unwrap();
    wait_for(&fixture, OperationState::Interrupted).await;
    assert_eq!(fixture.execution.changes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn paired_in_process_adapter_rejects_host_tools_before_ssh_or_journal_access() {
    use openwebide_agent::BridgeClient;
    let fixture = fixture().await;
    let client = crate::agent_host::HostInfoClient {
        host_administration: false,
        execution: fixture.execution.clone(),
        backend: fixture.backend.clone(),
        user: 7,
        session: 8,
    };
    let id = fixture.journal.lock().unwrap().operation.id;
    assert!(client.host_admin(&HostRequest::Apply { id }).await.is_err());
    assert_eq!(
        fixture.journal.lock().unwrap().operation.state,
        OperationState::Prepared
    );
    assert_eq!(fixture.execution.probes.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.execution.changes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn default_host_addresses_use_the_same_ssh_adapter_and_pinned_key_policy() {
    for address in [
        "host.docker.internal",
        "host.containers.internal",
        "127.0.0.1",
    ] {
        let execution = Arc::new(Execution {
            expected_destination: Some(format!("admin@{address}")),
            ..Default::default()
        });
        let host = SshHost::with_default_host(
            HostConnection {
                username: "admin".into(),
                ..Default::default()
            },
            execution.clone(),
            address,
        )
        .unwrap();
        assert!(host.overview().await.is_ok());
        assert_eq!(execution.probes.load(Ordering::SeqCst), 1);
    }
}
