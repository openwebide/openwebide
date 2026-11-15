//! Preserve received command results across actual owner loss while the original
//! watcher cannot acknowledge cleanup. All state and processes belong to fixtures.
use super::{command_case, command_journal, progress, send, send_json};
use openwebide_core::{
    UserId,
    plugins::languages::intelligence::actions::{
        ActionPhase, CommandOperation, CommandOutcome, CommandProgress, HostCommandRequest,
    },
    workspace_transaction::WorkspaceCommandRecovery,
};
use openwebide_storage::{Store, rusqlite_db::RusqliteDb};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::Duration,
};

const CHILD: &str = "OPENWEBIDE_RETAINED_COMMAND_FIXTURE";
const TEST: &str = "retained::received_results_survive_owner_and_cleanup_acknowledgement_loss";
#[derive(Serialize, Deserialize)]
struct Original {
    root: PathBuf,
    recovery_home: PathBuf,
    callback: String,
    request: HostCommandRequest,
    port: u16,
    watcher: i32,
    server: i32,
}
#[derive(Deserialize)]
struct Process {
    watcher: i32,
    server: i32,
}
#[derive(Deserialize)]
struct Receipt {
    confirmed: bool,
}
#[derive(Deserialize)]
struct Record {
    outcome: Option<CommandOutcome>,
    cleaned: bool,
    cleanup: Option<Receipt>,
}
struct Native {
    key: String,
    command: Option<Record>,
}
fn native(home: &Path) -> Option<Native> {
    // Observe a copied snapshot in memory. Never open/create an ambient private
    // SQLite file or write a test result into the native authority namespace.
    let directory = home.join(openwebide_core::vfs::NATIVE_RECOVERY_DIRECTORY);
    let head = std::fs::read(directory.join("recovery-head.json")).ok()?;
    let data = std::fs::read(directory.join("recovery.sqlite3")).ok()?;
    if std::fs::read(directory.join("recovery-head.json")).ok()? != head {
        return None;
    }
    let head: serde_json::Value = serde_json::from_slice(&head).ok()?;
    if head["phase"] != "committed"
        || head["digest"].as_str()? != openwebide_core::plugins::content_digest(&data)
    {
        return None;
    }
    let mut db = rusqlite::Connection::open_in_memory().ok()?;
    db.deserialize_read_exact("main", std::io::Cursor::new(&data), data.len(), false)
        .ok()?;
    let key: String = db
        .query_row("SELECT key FROM recovery_namespace WHERE id=1", [], |r| {
            r.get(0)
        })
        .ok()?;
    if serde_json::from_str::<serde_json::Value>(&key).ok()? != head["key"] {
        return None;
    }
    let command: Option<String> = db
        .query_row("SELECT record FROM recovery_commands", [], |r| r.get(0))
        .optional()
        .ok()?;
    Some(Native {
        key,
        command: command.map(|s| serde_json::from_str(&s)).transpose().ok()?,
    })
}
struct PausedWatcher {
    pid: i32,
    paused: bool,
}
impl PausedWatcher {
    fn pause(original: &Original, owner: u32) -> Self {
        let parent = std::process::Command::new("ps")
            .args(["-o", "ppid=", "-p", &original.watcher.to_string()])
            .output()
            .unwrap();
        assert!(parent.status.success());
        assert_eq!(
            String::from_utf8(parent.stdout)
                .unwrap()
                .trim()
                .parse::<u32>()
                .unwrap(),
            owner
        );
        assert!(original.watcher > 1 && original.server > 1);
        // SAFETY: this is the live fixture's own independent direct child,
        // identified by the LSP child and verified against its original parent.
        assert_eq!(unsafe { libc::getpgid(original.server) }, original.server);
        assert_eq!(unsafe { libc::getpgid(original.watcher) }, original.watcher);
        assert_eq!(unsafe { libc::kill(original.watcher, libc::SIGSTOP) }, 0);
        Self {
            pid: original.watcher,
            paused: true,
        }
    }
    async fn lose_owner(mut self, child: &mut tokio::process::Child) {
        child.start_kill().unwrap();
        // A stopped orphaned watcher group receives SIGHUP/SIGCONT on Linux
        // and macOS (the existing process-owner contract covers this). Do not
        // signal its numeric PID after the original parent has died.
        self.paused = false;
        use std::os::unix::process::ExitStatusExt;
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status.signal(), Some(libc::SIGKILL));
    }
}
impl Drop for PausedWatcher {
    fn drop(&mut self) {
        if self.paused {
            // SAFETY: before lose_owner, the fixture's parent is still owned
            // and live; its stopped child cannot have exited or reused its PID.
            unsafe {
                libc::kill(self.pid, libc::SIGCONT);
            }
        }
    }
}
async fn original(directory: &Path) {
    let (paired, uncertain): (bool, bool) =
        serde_json::from_slice(&std::fs::read(directory.join("case.json")).unwrap()).unwrap();
    let case = command_case::Case::retained(paired, directory, uncertain).await;
    assert_eq!(
        progress(send(case.port, paired, case.request.clone()).await),
        CommandProgress::Running
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while !case.root.join("owner.json").exists()
            || !case.root.join("child-ready").exists()
            || !case.root.join("heartbeat").exists()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let process: Process =
        serde_json::from_slice(&std::fs::read(case.root.join("owner.json")).unwrap()).unwrap();
    let data = Original {
        root: case.root.clone(),
        recovery_home: case.recovery_home().into(),
        callback: case.backend_url().into(),
        request: case.request.clone(),
        port: case.port,
        watcher: process.watcher,
        server: process.server,
    };
    std::fs::write(
        directory.join("ready.tmp"),
        serde_json::to_vec(&data).unwrap(),
    )
    .unwrap();
    std::fs::rename(directory.join("ready.tmp"), directory.join("ready.json")).unwrap();
    tokio::time::sleep(Duration::from_secs(120)).await;
    panic!("The retained-result owner was not killed by its supervisor");
}
async fn scope(store: &Store<RusqliteDb>, id: &str) -> WorkspaceCommandRecovery {
    store
        .workspace_command_recovery(UserId::new(1), 1, id)
        .await
        .unwrap()
        .unwrap()
}
async fn writer(port: u16, root: &Path) -> bool {
    let reply = tokio::time::timeout(
        Duration::from_secs(8),
        send_json(
            port,
            false,
            "/exec",
            &serde_json::json!({"command":"true","cwd":root,"timeout_seconds":5}),
        ),
    )
    .await
    .expect("Ordinary writer did not return after folder recovery");
    if reply.status != 200 {
        assert_eq!(reply.status, 400, "{}", reply.body);
        return false;
    }
    let outcome: openwebide_core::CommandOutcome = serde_json::from_str(&reply.body).unwrap();
    assert!(outcome.is_success(), "{outcome:?}");
    true
}
pub(super) async fn quiet(root: &Path) {
    // Test observation only; production uses its original durable group receipt.
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut previous = std::fs::read(root.join("heartbeat")).unwrap();
        let mut stable = 0;
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let current = std::fs::read(root.join("heartbeat")).unwrap();
            stable = if current == previous { stable + 1 } else { 0 };
            previous = current;
            if stable == 8 {
                break;
            }
        }
    })
    .await
    .expect("The original watcher did not stop its managed descendant");
}
async fn retained(home: &Path, cleaned: bool) -> (String, CommandOutcome) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(state) = native(home)
                && let Some(record) = state.command
                && record.cleaned == cleaned
                && record.cleanup.is_some_and(|r| r.confirmed == cleaned)
                && let Some(outcome) = record.outcome
            {
                break (state.key, outcome);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("The original native result/cleanup receipt was not retained")
}
async fn settled(store: &Store<RusqliteDb>, id: &str) {
    tokio::time::timeout(Duration::from_secs(8), async {
        while scope(store, id).await.command.native_settlement_pending {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn received_results_survive_owner_and_cleanup_acknowledgement_loss() {
    if let Some(directory) = std::env::var_os(CHILD) {
        original(&PathBuf::from(directory)).await;
        return;
    }
    for paired in [false, true] {
        for uncertain in [false, true] {
            for fault in ["handoff", "namespace", "folder", "settlement"] {
                // Keep failed fixture state so an original watcher can finish
                // its receipt; remove only after verified successful shutdown.
                let directory = tempfile::tempdir().unwrap().keep();
                eprintln!(
                    "retained-result fixture={directory:?} paired={paired} uncertain={uncertain} fault={fault}"
                );
                std::fs::write(
                    directory.join("case.json"),
                    serde_json::to_vec(&(paired, uncertain)).unwrap(),
                )
                .unwrap();
                let log = std::fs::File::create(directory.join("original.log")).unwrap();
                let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", TEST, "--nocapture"])
                    .env(CHILD, &directory)
                    .stdout(log.try_clone().unwrap())
                    .stderr(log)
                    .kill_on_drop(true)
                    .spawn()
                    .unwrap();
                let owner = child.id().unwrap();
                tokio::time::timeout(Duration::from_secs(20), async {
                    while !directory.join("ready.json").exists() {
                        if let Some(status) = child.try_wait().unwrap() {
                            panic!(
                                "Original exited {status}: {}",
                                std::fs::read_to_string(directory.join("original.log")).unwrap()
                            );
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("Original command did not begin");
                let original: Original =
                    serde_json::from_slice(&std::fs::read(directory.join("ready.json")).unwrap())
                        .unwrap();
                let paused = PausedWatcher::pause(&original, owner);
                std::fs::write(original.root.join("reply-now"), "reply").unwrap();
                let (key, expected) = retained(&original.recovery_home, false).await;
                match &expected {
                    CommandOutcome::Completed { result } if !uncertain => {
                        assert_eq!(result["marker"], 17);
                        assert_eq!(result["source"], "next 文😀\r\n");
                        assert_eq!(
                            result["kept"],
                            "原本 😀 \"quoted\" \\ exact\r\n".repeat(1024)
                        );
                    }
                    CommandOutcome::Uncertain { message } if uncertain => {
                        assert!(message.contains("Retained command error 文😀"), "{message}");
                    }
                    other => panic!("Unexpected original result: {other:?}"),
                }
                assert!(!writer(original.port, &original.root).await);
                let parked_namespace = original.recovery_home.join("parked-namespace");
                let namespace = original
                    .recovery_home
                    .join(openwebide_core::vfs::NATIVE_RECOVERY_DIRECTORY);
                let parked_folder = original.root.with_extension("parked");
                if fault == "namespace" {
                    std::fs::rename(&namespace, &parked_namespace).unwrap();
                }
                if fault == "folder" {
                    std::fs::rename(&original.root, &parked_folder).unwrap();
                }
                paused.lose_owner(&mut child).await;
                let backend =
                    command_journal::Backend::persisted(&directory.join("journal.sqlite")).await;
                let store = Store::new(RusqliteDb::open(directory.join("journal.sqlite")).unwrap());
                backend.now.store(103, Ordering::SeqCst);
                backend.original_unavailable.store(true, Ordering::SeqCst);
                store
                    .set_user_setting(UserId::new(1), "language_settings", "{revoked provider")
                    .await
                    .unwrap();
                let id = &original.request.command.invocation.id;
                let editors = store.editor_recovery(UserId::new(1), 1).await.unwrap();
                let visible_root = if fault == "folder" {
                    &parked_folder
                } else {
                    &original.root
                };
                let source = std::fs::read(visible_root.join("main.notes")).unwrap();
                let url = url::Url::parse(&original.callback).unwrap();
                assert_eq!(url.host_str(), Some("127.0.0.1"));
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", url.port().unwrap()))
                    .await
                    .unwrap();
                let callbacks = command_journal::serve_on(backend.clone(), listener);
                let peer = command_case::spawn_native_peer(
                    original.root.parent().unwrap(),
                    &original.recovery_home,
                    &callbacks.url,
                )
                .await;
                assert_ne!(peer.pid, owner);
                let blocked_write = writer(peer.port, &original.root);
                tokio::pin!(blocked_write);
                let queued = match tokio::time::timeout(
                    Duration::from_millis(200),
                    &mut blocked_write,
                )
                .await
                {
                    Ok(allowed) => {
                        assert!(!allowed);
                        false
                    }
                    Err(_) => true,
                };
                quiet(visible_root).await;
                if fault == "namespace" {
                    let original_folder = std::fs::File::open(&original.root).unwrap();
                    assert!(
                        matches!(
                            original_folder.try_lock(),
                            Err(std::fs::TryLockError::WouldBlock)
                        ),
                        "The watcher dropped its original folder descriptor before its cleanup receipt was durable"
                    );
                    assert!(
                        !namespace.exists(),
                        "A successor initialized replacement private state"
                    );
                    std::fs::rename(&parked_namespace, &namespace).unwrap();
                }
                if fault == "folder" {
                    std::fs::rename(&parked_folder, &original.root).unwrap();
                }
                if queued {
                    assert!(!blocked_write.await);
                }
                let (recovered_key, recovered) = retained(&original.recovery_home, true).await;
                assert_eq!(recovered_key, key);
                assert_eq!(recovered, expected);
                assert!(!writer(peer.port, &original.root).await);
                let mut control = original.request.clone();
                control.root.clear();
                control.command.operation = CommandOperation::Status;
                control.command.selection = None;
                control.authority = None;
                control.journal = None;
                assert_eq!(
                    progress(send(peer.port, paired, control.clone()).await),
                    CommandProgress::Finished {
                        outcome: expected.clone()
                    }
                );
                let other = if fault == "settlement" {
                    let other = command_case::spawn_native_peer(
                        original.root.parent().unwrap(),
                        &original.recovery_home,
                        &callbacks.url,
                    )
                    .await;
                    assert_ne!(other.pid, peer.pid);
                    assert_eq!(
                        progress(send(other.port, paired, control).await),
                        CommandProgress::Finished {
                            outcome: expected.clone()
                        }
                    );
                    backend.drop_settlement_ack.store(true, Ordering::SeqCst);
                    Some(other)
                } else {
                    None
                };
                backend.original_unavailable.store(false, Ordering::SeqCst);
                let current = tokio::time::timeout(Duration::from_secs(8), async {
                    loop {
                        let current = scope(&store, id).await;
                        if current.command.outcome_digest.is_some()
                            && current.command.command_cleanup_confirmed
                        {
                            break current;
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                .unwrap();
                assert_eq!(
                    current.command.outcome_digest,
                    Some(openwebide_core::plugins::content_digest(
                        &serde_json::to_vec(&expected).unwrap()
                    ))
                );
                assert_eq!(
                    store
                        .language_action(UserId::new(1), 1, Some(id))
                        .await
                        .unwrap()
                        .unwrap()
                        .outcome,
                    Some(expected.clone())
                );
                if uncertain {
                    assert_eq!(current.command.phase, ActionPhase::Uncertain);
                    assert!(current.command.native_settlement_pending);
                    assert!(!writer(peer.port, &original.root).await);
                    store
                        .acknowledge_language_action(
                            UserId::new(1),
                            1,
                            id,
                            current.command.revision,
                            "Reviewed the exact retained command outcome",
                            104,
                        )
                        .await
                        .unwrap();
                } else {
                    assert_eq!(current.command.phase, ActionPhase::Completed);
                }
                if fault == "settlement" {
                    tokio::time::timeout(Duration::from_secs(8), async {
                        while backend.lost_settlement_acks.load(Ordering::SeqCst) != 1 {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .unwrap();
                    assert!(!scope(&store, id).await.command.native_settlement_pending);
                    let (_, native_outcome) = retained(&original.recovery_home, true).await;
                    assert_eq!(native_outcome, expected);
                    assert!(!writer(peer.port, &original.root).await);
                    assert!(!writer(other.as_ref().unwrap().port, &original.root).await);
                    backend.original_unavailable.store(false, Ordering::SeqCst);
                }
                settled(&store, id).await;
                for port in std::iter::once(peer.port).chain(other.as_ref().map(|p| p.port)) {
                    tokio::time::timeout(Duration::from_secs(8), async {
                        while !writer(port, &original.root).await {
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                    })
                    .await
                    .unwrap();
                }
                super::settlement::retired(&original.recovery_home).await;
                let final_state = native(&original.recovery_home).unwrap();
                assert_eq!(final_state.key, key);
                assert!(final_state.command.is_none());
                if other.is_some() {
                    let calls = backend.original_calls.load(Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(2200)).await;
                    assert_eq!(
                        backend.original_calls.load(Ordering::SeqCst),
                        calls,
                        "A retired original publisher continued callbacks"
                    );
                }
                assert_eq!(
                    std::fs::read(original.root.join("main.notes")).unwrap(),
                    source
                );
                assert_eq!(
                    store.editor_recovery(UserId::new(1), 1).await.unwrap(),
                    editors
                );
                assert_eq!(
                    store
                        .language_action(UserId::new(1), 1, Some(id))
                        .await
                        .unwrap()
                        .unwrap()
                        .outcome,
                    Some(expected)
                );
                assert_eq!(
                    std::fs::read_to_string(original.root.join("executions"))
                        .unwrap()
                        .lines()
                        .count(),
                    1
                );
                assert_eq!(
                    std::fs::read_to_string(original.root.join("calls"))
                        .unwrap()
                        .lines()
                        .filter(|line| *line == "workspace/executeCommand")
                        .count(),
                    1
                );
                eprintln!(
                    "retained result recovered paired={paired} uncertain={uncertain} fault={fault} original_pid={owner} successor_pid={}",
                    peer.pid
                );
                if let Some(other) = other {
                    other.stop().await;
                }
                peer.stop().await;
                drop(callbacks);
                drop(backend);
                drop(store);
                std::fs::remove_dir_all(directory).unwrap();
            }
        }
    }
}
