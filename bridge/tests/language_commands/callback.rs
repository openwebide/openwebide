//! Reconnect a real recorded owner, never disclose its private proofs to a
//! candidate that reflects requests, and retain the verified move across SIGKILL.
use super::{command_case, progress, send, send_json};
use http_body_util::BodyExt as _;
use openwebide_core::{
    UserId,
    plugins::languages::intelligence::actions::{CommandOperation, CommandProgress},
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
fn native(home: &Path) -> Option<(String, Value, Value)> {
    native_snapshot(home).ok()
}
fn native_snapshot(home: &Path) -> Result<(String, Value, Value), String> {
    // The publisher changes the head and database under this original anchor's
    // exclusive lock. Observe both under its shared lock, with the same bounded
    // contention window as normal namespace readers. A prepared or damaged
    // namespace after acquiring the lock remains an error, not a retry.
    let publication = std::fs::File::open(home.join(openwebide_core::vfs::NATIVE_RECOVERY_OWNER))
        .map_err(|error| format!("Opening publication anchor: {error}"))?;
    let deadline = std::time::Instant::now() + Duration::from_millis(512);
    loop {
        match publication.try_lock_shared() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(
                    deadline
                        .saturating_duration_since(std::time::Instant::now())
                        .min(Duration::from_millis(2)),
                );
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err("Publication lock remains busy".into());
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(format!("Locking publication anchor: {error}"));
            }
        }
    }
    let directory = home.join(openwebide_core::vfs::NATIVE_RECOVERY_DIRECTORY);
    let head = std::fs::read(directory.join("recovery-head.json"))
        .map_err(|error| format!("Reading publication head: {error}"))?;
    let data = std::fs::read(directory.join("recovery.sqlite3"))
        .map_err(|error| format!("Reading namespace: {error}"))?;
    if std::fs::read(directory.join("recovery-head.json"))
        .map_err(|error| format!("Rechecking publication head: {error}"))?
        != head
    {
        return Err("Publication head changed during observation".into());
    }
    let head: Value = serde_json::from_slice(&head)
        .map_err(|error| format!("Decoding publication head: {error}"))?;
    if head["phase"] != "committed" {
        return Err(format!("Publication phase: {}", head["phase"]));
    }
    if head["digest"].as_str() != Some(&openwebide_core::plugins::content_digest(&data)) {
        return Err("Publication digest changed during observation".into());
    }
    let mut db = rusqlite::Connection::open_in_memory()
        .map_err(|error| format!("Opening observation database: {error}"))?;
    db.deserialize_read_exact("main", std::io::Cursor::new(&data), data.len(), false)
        .map_err(|error| format!("Decoding observation database: {error}"))?;
    let key: String = db
        .query_row("SELECT key FROM recovery_namespace WHERE id=1", [], |r| {
            r.get(0)
        })
        .map_err(|error| format!("Reading namespace identity: {error}"))?;
    let owner: String = db
        .query_row("SELECT record FROM recovery_owners", [], |r| r.get(0))
        .map_err(|error| format!("Reading original owner: {error}"))?;
    let command: String = db
        .query_row("SELECT record FROM recovery_commands", [], |r| r.get(0))
        .map_err(|error| format!("Reading original command: {error}"))?;
    Ok((
        key,
        serde_json::from_str(&owner)
            .map_err(|error| format!("Decoding original owner: {error}"))?,
        serde_json::from_str(&command)
            .map_err(|error| format!("Decoding original command: {error}"))?,
    ))
}
async fn writer(case: &command_case::Case, paired: bool) -> bool {
    let response = send_json(
        case.port,
        paired,
        "/exec",
        &json!({"command":"true","cwd":case.root,"timeout_seconds":5}),
    )
    .await;
    response.status == 200
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_observation_respects_the_original_publication_lock() {
    let mut case = command_case::Case::new(false, false).await;
    case.stop_native().await;
    let expected = native_snapshot(case.recovery_home()).unwrap();
    let publication = std::fs::File::open(
        case.recovery_home()
            .join(openwebide_core::vfs::NATIVE_RECOVERY_OWNER),
    )
    .unwrap();
    publication.try_lock().unwrap();
    assert!(
        native_snapshot(case.recovery_home()).is_err(),
        "The observer read a namespace held by its original publisher"
    );
    publication.unlock().unwrap();
    assert_eq!(native_snapshot(case.recovery_home()).unwrap(), expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn current_connection_rebinds_a_running_owner_without_trusting_replays_or_other_scopes() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    const PATH: &str = "/plugins/languages/command/reconnect";
    for paired in [false, true] {
        let mut case = command_case::Case::new(paired, false).await;
        let port = case.port;
        let mut stop = case.request.clone();
        stop.command.operation = CommandOperation::Stop;
        stop.command.selection = None;
        stop.root.clear();
        assert!(matches!(
            progress(send(port, paired, stop).await),
            CommandProgress::Finished { .. }
        ));
        case.backend.block_scope_at.store(
            case.backend.original_scope_calls.load(Ordering::SeqCst) + 1,
            Ordering::SeqCst,
        );
        tokio::time::timeout(Duration::from_secs(8), async {
            while !case.backend.scope_blocked.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let before = native(case.recovery_home()).unwrap();
        let old_url = case.backend_url().to_string();
        let old_reply = case
            .backend
            .last_private_reply
            .lock()
            .unwrap()
            .clone()
            .unwrap();
        case.move_callback().await;
        let candidate = case.backend_url().to_string();
        assert_ne!(old_url, candidate);
        let hint = |user, project, callback: &str| json!({"user":user,"project":project,"callback":callback});
        let unauthenticated = super::HttpResponse::parse(&super::http(port, &format!("POST {PATH} HTTP/1.1\r\nHost: localhost:{port}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")).await);
        assert_eq!(unauthenticated.status, 401);
        assert_eq!(
            send_json(port, paired, PATH, &hint(1, 1, "file:///private"))
                .await
                .status,
            400
        );
        for (transport, user, project) in [(paired, 2, 1), (paired, 1, 2), (!paired, 1, 1)] {
            let reply = send_json(port, transport, PATH, &hint(user, project, &candidate)).await;
            assert_eq!(reply.status, 200, "{}", reply.body);
            assert_eq!(reply.body, "false");
        }
        assert_eq!(native(case.recovery_home()).unwrap(), before);
        let other = super::command_journal::Backend::new().await;
        let other_issuer = other.store.get_setting("http_test_issuer").await.unwrap();
        let wrong = super::command_journal::serve(other.clone()).await;
        let reply = send_json(port, paired, PATH, &hint(1, 1, &wrong.url)).await;
        assert_eq!(reply.status, 200, "{}", reply.body);
        assert_eq!(reply.body, "false");
        assert_eq!(
            other.store.get_setting("http_test_issuer").await.unwrap(),
            other_issuer
        );
        assert_eq!(native(case.recovery_home()).unwrap(), before);

        // Replay an authentic encrypted reply from a different request. Its
        // original nonce/ciphertext binding must prevent destination changes.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let replay_url = format!("http://{}/api", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let replay = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let body = old_reply.clone();
                let observed = observed.clone();
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(
                        move |request: hyper::Request<hyper::body::Incoming>| {
                            let body = body.clone();
                            let observed = observed.clone();
                            async move {
                                assert_eq!(
                                    request.uri().path(),
                                    "/api/language-commands/original-private"
                                );
                                assert!(!request.headers().contains_key("authorization"));
                                let _ = request.into_body().collect().await.unwrap();
                                observed.fetch_add(1, Ordering::SeqCst);
                                Ok::<_, std::io::Error>(hyper::Response::new(
                                    http_body_util::Full::new(bytes::Bytes::from(body)),
                                ))
                            }
                        },
                    );
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        let reply = send_json(port, paired, PATH, &hint(1, 1, &replay_url)).await;
        assert_eq!(reply.status, 200, "{}", reply.body);
        assert_eq!(reply.body, "false");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(native(case.recovery_home()).unwrap(), before);

        let reply = send_json(port, paired, PATH, &hint(1, 1, &candidate)).await;
        assert_eq!(reply.status, 200, "{}", reply.body);
        assert_eq!(reply.body, "true");
        let mut expected = before.clone();
        expected.1["origin"] = candidate.into();
        assert_eq!(native(case.recovery_home()).unwrap(), expected);
        assert!(!writer(&case, paired).await);
        let recovery = case
            .backend
            .store
            .workspace_command_recovery(UserId::new(1), 1, &case.request.command.invocation.id)
            .await
            .unwrap()
            .unwrap();
        case.backend
            .store
            .acknowledge_language_action(
                UserId::new(1),
                1,
                &case.request.command.invocation.id,
                recovery.command.revision,
                "Reviewed original no-dispatch rejection",
                103,
            )
            .await
            .unwrap();
        // The old request remains held. The running publisher must abandon it
        // and use the proven new origin, without a host restart or fresh claim.
        super::settlement::retired(case.recovery_home()).await;
        assert_eq!(case.port, port);
        assert!(!case.backend.scope_released.load(Ordering::SeqCst));
        assert!(writer(&case, paired).await);
        // Hyper drops the held handler when the publisher cancels its old
        // connection. Observe that cancellation instead of releasing a reply
        // whose request has already ended.
        tokio::time::timeout(Duration::from_secs(5), async {
            while !case.backend.scope_abandoned.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let calls = case.backend.original_calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(2200)).await;
        assert_eq!(case.backend.original_calls.load(Ordering::SeqCst), calls);
        assert!(!case.root.join("executions").exists());
        assert_eq!(
            std::fs::read_to_string(case.root.join("main.notes")).unwrap(),
            super::SOURCE
        );
        eprintln!("current connection rebound running owner paired={paired} port={port}");
        case.close().await;
        replay.abort();
        let _ = replay.await;
        drop(wrong);
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_callbacks_reconnect_only_after_private_proof_and_survive_another_owner_loss() {
    for paired in [false, true] {
        let mut case = command_case::Case::new(paired, false).await;
        let mut stop = case.request.clone();
        stop.command.operation = CommandOperation::Stop;
        stop.command.selection = None;
        stop.root.clear();
        assert!(matches!(
            progress(send(case.port, paired, stop).await),
            CommandProgress::Finished { .. }
        ));
        assert!(!writer(&case, paired).await);
        case.stop_native().await;
        let (key, owner, command) = native(case.recovery_home()).unwrap();
        let old_url = case.backend_url().to_string();
        assert_eq!(owner["origin"], old_url);
        let authority_secret = owner["credential"].as_str().unwrap().to_string();
        let worker_secret = command["worker"]["secret"].as_str().unwrap().to_string();
        let requests = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let observed = requests.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bad_url = format!("http://{}/api", listener.local_addr().unwrap());
        let malicious = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let observed = observed.clone();
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(
                        move |req: hyper::Request<hyper::body::Incoming>| {
                            let observed = observed.clone();
                            async move {
                                if req.uri().path() != "/api/language-commands/original-private" {
                                    return Ok::<_, std::io::Error>(
                                        hyper::Response::builder()
                                            .status(404)
                                            .body(http_body_util::Full::new(
                                                bytes::Bytes::from_static(b"{}"),
                                            ))
                                            .unwrap(),
                                    );
                                }
                                assert!(!req.headers().contains_key("authorization"));
                                assert_eq!(
                                    req.uri().path(),
                                    "/api/language-commands/original-private"
                                );
                                let bytes = req.into_body().collect().await.unwrap().to_bytes();
                                observed.lock().unwrap().push(bytes.to_vec());
                                let body: Value = serde_json::from_slice(&bytes).unwrap();
                                // The wrong direction and request binding make a
                                // reflected ciphertext unusable as an issuer proof.
                                Ok::<_,std::io::Error>(hyper::Response::new(http_body_util::Full::new(bytes::Bytes::from(serde_json::to_vec(&json!({"nonce":body["nonce"],"ciphertext":body["ciphertext"]})).unwrap()))))
                            }
                        },
                    );
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        let first = case.start_native_process_with_callback(&bad_url).await;
        tokio::time::timeout(Duration::from_secs(8), async {
            while requests.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let current = native_snapshot(case.recovery_home()).unwrap();
        assert_eq!(current.0, key);
        assert_eq!(current.1, owner);
        assert_eq!(current.2, command);
        assert!(!writer(&case, paired).await);
        case.kill_native_process().await;
        case.move_callback().await;
        let new_url = case.backend_url().to_string();
        assert_ne!(new_url, old_url);
        // Rebinding metadata requires the original namespace, not the folder's
        // current pathname. Its original write fence must remain intact.
        let parked = case.root.with_extension("parked");
        std::fs::rename(&case.root, &parked).unwrap();
        let second = case.start_native_process().await;
        assert_ne!(first, second);
        let moved = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(state) = native(case.recovery_home())
                    && state.1["origin"] == new_url
                {
                    break state;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let mut expected = owner.clone();
        expected["origin"] = new_url.clone().into();
        assert_eq!(moved.0, key);
        assert_eq!(moved.1, expected);
        assert_eq!(moved.2, command);
        case.kill_native_process().await;
        std::fs::rename(&parked, &case.root).unwrap();
        // A later configured candidate still cannot replace the persisted,
        // verified destination. Normal recovery follows the saved destination.
        let third = case.start_native_process_with_callback(&bad_url).await;
        assert_ne!(second, third);
        let recovery = case
            .backend
            .store
            .workspace_command_recovery(UserId::new(1), 1, &case.request.command.invocation.id)
            .await
            .unwrap()
            .unwrap();
        case.backend
            .store
            .acknowledge_language_action(
                UserId::new(1),
                1,
                &case.request.command.invocation.id,
                recovery.command.revision,
                "Reviewed original no-dispatch rejection",
                103,
            )
            .await
            .unwrap();
        super::settlement::retired(case.recovery_home()).await;
        assert!(writer(&case, paired).await);
        assert!(!case.root.join("executions").exists());
        assert_eq!(
            std::fs::read_to_string(case.root.join("main.notes")).unwrap(),
            super::SOURCE
        );
        for body in requests.lock().unwrap().iter() {
            let text = String::from_utf8_lossy(body);
            assert!(!text.contains(&authority_secret));
            assert!(!text.contains(&worker_secret));
        }
        eprintln!(
            "callback rebound paired={paired} original_candidate_pid={first} verified_owner_pid={second} final_pid={third}"
        );
        case.close().await;
        malicious.abort();
        let _ = malicious.await;
    }
}
