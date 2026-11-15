//! Real pruning and two abrupt native replacements, including a lost ACK after
//! the backend has deleted its final receipt. No test fabricates an outcome.
use super::{command_case, command_journal, progress, send, send_json};
use openwebide_core::{
    NewProject, UserId, WorkspaceMode,
    editor::{EditorRecovery, EditorRecoveryRoot},
    plugins::languages::intelligence::actions::{
        CommandOperation, CommandProgress, HostCommandRequest,
    },
    workspace_transaction::{
        BeginWorkspaceTransaction, EditorRecoveryPatch, WorkspacePlan, WorkspaceTransactionScope,
    },
};
use openwebide_storage::{Store, rusqlite_db::RusqliteDb};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::Duration,
};

const CHILD: &str = "OPENWEBIDE_PRUNED_SETTLEMENT_FIXTURE";
const TEST: &str =
    "settlement::public_pruning_and_lost_metadata_ack_survive_distinct_native_restarts";
#[derive(Serialize, Deserialize)]
struct Original {
    root: PathBuf,
    home: PathBuf,
    callback: String,
    request: HostCommandRequest,
}
struct Snapshot {
    key: String,
    commands: u32,
    acknowledgements: u32,
}
fn snapshot(home: &Path) -> Option<Snapshot> {
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
    let key = db
        .query_row("SELECT key FROM recovery_namespace WHERE id=1", [], |r| {
            r.get(0)
        })
        .ok()?;
    let commands = db
        .query_row("SELECT count(*) FROM recovery_commands", [], |r| r.get(0))
        .ok()?;
    let tables: u32 = db.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='recovery_command_settlements'", [], |r| r.get(0)).ok()?;
    let acknowledgements = if tables == 0 {
        0
    } else {
        db.query_row(
            "SELECT count(*) FROM recovery_command_settlements",
            [],
            |r| r.get(0),
        )
        .ok()?
    };
    Some(Snapshot {
        key,
        commands,
        acknowledgements,
    })
}
/// Native folder retirement precedes the final metadata callback. Observe the
/// durable outbox as well before asserting that all callbacks have stopped.
pub(super) async fn retired(home: &Path) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if snapshot(home)
                .is_some_and(|state| state.commands == 0 && state.acknowledgements == 0)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
async fn counter(counter: &std::sync::atomic::AtomicUsize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while counter.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
async fn original(directory: &Path) {
    let paired: bool =
        serde_json::from_slice(&std::fs::read(directory.join("case.json")).unwrap()).unwrap();
    let case = command_case::Case::completing(paired, directory).await;
    case.backend
        .reject_initial_settlement
        .store(true, Ordering::SeqCst);
    assert_eq!(
        progress(send(case.port, paired, case.request.clone()).await),
        CommandProgress::Running
    );
    counter(&case.backend.rejected_initial_settlements).await;
    let data = Original {
        root: case.root.clone(),
        home: case.recovery_home().into(),
        callback: case.backend_url().into(),
        request: case.request.clone(),
    };
    std::fs::write(
        directory.join("ready.tmp"),
        serde_json::to_vec(&data).unwrap(),
    )
    .unwrap();
    std::fs::rename(directory.join("ready.tmp"), directory.join("ready.json")).unwrap();
    tokio::time::sleep(Duration::from_secs(120)).await;
    panic!("Original settlement fixture owner was not killed");
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
    .unwrap();
    if reply.status != 200 {
        assert_eq!(reply.status, 400, "{}", reply.body);
        return false;
    }
    let outcome: openwebide_core::CommandOutcome = serde_json::from_str(&reply.body).unwrap();
    assert!(outcome.is_success(), "{outcome:?}");
    true
}
async fn prune(store: &Store<RusqliteDb>, paired: bool) -> WorkspaceTransactionScope {
    let user = UserId::new(1);
    let project = store
        .create_project(
            &NewProject {
                name: "Pruning trigger".into(),
                mode: if paired {
                    WorkspaceMode::Local
                } else {
                    WorkspaceMode::Remote
                },
                path: Some("other".into()),
            },
            user,
            1_000_000,
        )
        .await
        .unwrap();
    let root = EditorRecoveryRoot::for_project(&project);
    let revision = store
        .save_editor_recovery(
            user,
            project.id,
            0,
            &EditorRecovery {
                format: 1,
                root: Some(root.clone()),
                files: vec![],
                selected: None,
            },
        )
        .await
        .unwrap();
    let input = BeginWorkspaceTransaction {
        id: "e".repeat(32),
        language_review: None,
        plan: WorkspacePlan {
            version: 1,
            root: openwebide_core::plugins::content_digest(b"separate pruning project"),
            operations: vec![],
            roots: vec![],
            before: BTreeMap::new(),
            after: BTreeMap::new(),
        },
        editor: EditorRecoveryPatch {
            revision,
            root,
            before: vec![],
            after: vec![],
            order_before: vec![],
            order_after: vec![],
            selected_before: None,
            selected_after: None,
        },
    };
    store
        .begin_workspace_transaction(user, project.id, &input, 1_000_000)
        .await
        .unwrap();
    store
        .workspace_transaction_scope(user, project.id, &input.id)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_pruning_and_lost_metadata_ack_survive_distinct_native_restarts() {
    if let Some(directory) = std::env::var_os(CHILD) {
        original(&PathBuf::from(directory)).await;
        return;
    }
    for paired in [false, true] {
        let directory = tempfile::tempdir().unwrap().keep();
        eprintln!("pruned settlement fixture={directory:?} paired={paired}");
        std::fs::write(
            directory.join("case.json"),
            serde_json::to_vec(&paired).unwrap(),
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
        let original_pid = child.id().unwrap();
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
        .unwrap();
        let original: Original =
            serde_json::from_slice(&std::fs::read(directory.join("ready.json")).unwrap()).unwrap();
        child.start_kill().unwrap();
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        let backend = command_journal::Backend::persisted(&directory.join("journal.sqlite")).await;
        let store = Store::new(RusqliteDb::open(directory.join("journal.sqlite")).unwrap());
        backend.now.store(103, Ordering::SeqCst);
        backend.original_unavailable.store(true, Ordering::SeqCst);
        backend.drop_settlement_ack.store(true, Ordering::SeqCst);
        let id = &original.request.command.invocation.id;
        let expected = store
            .language_action(UserId::new(1), 1, Some(id))
            .await
            .unwrap()
            .unwrap()
            .outcome
            .unwrap();
        let editor = store.editor_recovery(UserId::new(1), 1).await.unwrap();
        let source = std::fs::read(original.root.join("main.notes")).unwrap();
        let key = snapshot(&original.home).unwrap().key;
        let url = url::Url::parse(&original.callback).unwrap();
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", url.port().unwrap()))
            .await
            .unwrap();
        let callbacks = command_journal::serve_on(backend.clone(), listener);
        let peer = command_case::spawn_native_peer(
            original.root.parent().unwrap(),
            &original.home,
            &callbacks.url,
        )
        .await;
        assert_ne!(peer.pid, original_pid);
        assert!(!writer(peer.port, &original.root).await);
        backend.original_unavailable.store(false, Ordering::SeqCst);
        counter(&backend.lost_settlement_acks).await;
        assert!(
            !store
                .workspace_command_recovery(UserId::new(1), 1, id)
                .await
                .unwrap()
                .unwrap()
                .command
                .native_settlement_pending
        );
        let unrelated = prune(&store, paired).await;
        backend.now.store(1_000_000, Ordering::SeqCst);
        assert!(
            store
                .language_action(UserId::new(1), 1, Some(id))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .workspace_transaction(UserId::new(1), 1, Some(id))
                .await
                .unwrap()
                .is_none()
        );
        assert!(!writer(peer.port, &original.root).await);
        let mut control = original.request.clone();
        control.root.clear();
        control.command.operation = CommandOperation::Status;
        control.command.selection = None;
        control.authority = None;
        control.journal = None;
        assert_eq!(
            progress(send(peer.port, paired, control).await),
            CommandProgress::Finished {
                outcome: expected.clone()
            }
        );
        backend
            .drop_settlement_metadata_ack
            .store(true, Ordering::SeqCst);
        backend.original_unavailable.store(false, Ordering::SeqCst);
        counter(&backend.lost_settlement_metadata_acks).await;
        let state = snapshot(&original.home).unwrap();
        assert_eq!(state.key, key);
        assert_eq!(state.commands, 0);
        assert_eq!(state.acknowledgements, 1);
        assert!(writer(peer.port, &original.root).await);
        let first_successor = peer.pid;
        peer.kill().await;
        // Only the durable metadata outbox remains. Move the genuine issuer
        // after public pruning and receipt deletion; startup must use the new
        // candidate without a public row, active native owner, or folder grant.
        let old_callback = callbacks.url.clone();
        drop(callbacks);
        let callbacks = command_journal::serve(backend.clone()).await;
        assert_ne!(callbacks.url, old_callback);
        backend.original_unavailable.store(false, Ordering::SeqCst);
        let successor = command_case::spawn_native_peer(
            original.root.parent().unwrap(),
            &original.home,
            &callbacks.url,
        )
        .await;
        assert_ne!(successor.pid, first_successor);
        tokio::time::timeout(Duration::from_secs(10), async {
            while snapshot(&original.home).is_none_or(|s| s.acknowledgements != 0) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(snapshot(&original.home).unwrap().key, key);
        assert!(writer(successor.port, &original.root).await);
        assert_eq!(
            store
                .workspace_transaction_scope(UserId::new(1), 2, &unrelated.id)
                .await
                .unwrap(),
            Some(unrelated)
        );
        assert_eq!(
            store.editor_recovery(UserId::new(1), 1).await.unwrap(),
            editor
        );
        assert_eq!(
            std::fs::read(original.root.join("main.notes")).unwrap(),
            source
        );
        assert_eq!(
            std::fs::read_to_string(original.root.join("executions"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        let calls = backend.original_calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(2200)).await;
        assert_eq!(backend.original_calls.load(Ordering::SeqCst), calls);
        eprintln!(
            "pruned settlement recovered paired={paired} original_pid={original_pid} first_successor={first_successor} last_successor={}",
            successor.pid
        );
        successor.stop().await;
        drop(callbacks);
        drop(backend);
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
