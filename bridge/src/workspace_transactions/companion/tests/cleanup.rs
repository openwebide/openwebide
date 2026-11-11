//! A proof reaper is a managed writer, including when its session is retired
//! while another physical folder has the current request's lease.
use super::*;
use crate::folder_coordination::Folder;
use std::path::Path;

async fn expire(case: &Case, id: &str) {
    let mut state = case.host.state.lock().await;
    state.sessions.get_mut(id).unwrap().touched = Instant::now() - PROOF_TTL;
    state.sweep();
    assert!(!state.sessions.contains_key(id));
}
async fn idle(host: &Host) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while host.coordinator.workers() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn queued(host: &Host) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while host.coordinator.waiting() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn swept_proofs_wait_for_the_original_physical_gate_before_cleanup() {
    let case = Case::new();
    let (id, name, _) = case.open().await;
    let path = case.root().join(name);
    let original = std::fs::read(&path).unwrap();
    let lease = case
        .host
        .coordinator
        .acquire(Folder::capture(&case.root()).unwrap())
        .await
        .unwrap();
    expire(&case, &id).await;
    assert_eq!(std::fs::read(&path).unwrap(), original);
    queued(&case.host).await;
    assert_eq!(std::fs::read(&path).unwrap(), original);
    drop(lease);
    idle(&case.host).await;
    assert!(!path.exists());
}

#[tokio::test]
async fn sweeping_an_unrelated_folder_never_borrows_its_gate_for_old_proof_cleanup() {
    let case = Case::new();
    std::fs::create_dir(case.root().join("other")).unwrap();
    std::fs::create_dir(case.root().join("project")).unwrap();
    let FolderReply::Opened {
        session, challenge, ..
    } = case
        .request(FolderAction::Open {
            path: "project".into(),
            expected: None,
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    let path = case.root().join("project").join(challenge);
    let held = case
        .host
        .coordinator
        .acquire(Folder::capture(&case.root().join("project")).unwrap())
        .await
        .unwrap();
    case.host
        .state
        .lock()
        .await
        .sessions
        .get_mut(&session)
        .unwrap()
        .touched = Instant::now() - PROOF_TTL;
    let unrelated = Transactions::capture_root(&case.root(), "other", 1, 1).unwrap();
    let fence = case.host.exclude(&unrelated).await.unwrap();
    assert!(path.exists());
    queued(&case.host).await;
    assert!(path.exists());
    drop(fence);
    drop(held);
    idle(&case.host).await;
    assert!(!path.exists());
}

#[tokio::test]
async fn dropping_the_host_queues_proof_cleanup_behind_existing_native_work() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let host = Host::default();
    let coordinator = host.coordinator.clone();
    let FolderReply::Opened { challenge, .. } = host
        .request(
            root.clone(),
            "paired/user:1".into(),
            FolderRequest {
                user: 1,
                project: 1,
                action: FolderAction::Open {
                    path: "".into(),
                    expected: None,
                },
            },
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    let path = root.join(challenge);
    let lease = coordinator
        .acquire(Folder::capture(&root).unwrap())
        .await
        .unwrap();
    drop(host);
    assert!(path.exists());
    tokio::time::timeout(Duration::from_secs(3), async {
        while coordinator.waiting() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(path.exists());
    drop(lease);
    tokio::time::timeout(Duration::from_secs(3), async {
        while coordinator.workers() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!path.exists());
}

#[tokio::test]
async fn queued_proof_cleanup_preserves_replacements_and_changed_contents() {
    for replacement in [false, true] {
        let case = Case::new();
        let (id, name, _) = case.open().await;
        let path = case.root().join(name);
        let held = case
            .host
            .coordinator
            .acquire(Folder::capture(&case.root()).unwrap())
            .await
            .unwrap();
        expire(&case, &id).await;
        assert!(path.exists());
        queued(&case.host).await;
        if replacement {
            std::fs::rename(&path, case.root().join("original-proof")).unwrap();
        }
        std::fs::write(&path, "later user data").unwrap();
        drop(held);
        idle(&case.host).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "later user data");
    }
}

#[tokio::test]
async fn changed_folder_paths_preserve_original_and_replacement_proofs() {
    let case = Case::new();
    std::fs::create_dir(case.root().join("project")).unwrap();
    let FolderReply::Opened {
        session, challenge, ..
    } = case
        .request(FolderAction::Open {
            path: "project".into(),
            expected: None,
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    let path = case.root().join("project").join(&challenge);
    let original = std::fs::read(&path).unwrap();
    let held = case
        .host
        .coordinator
        .acquire(Folder::capture(&case.root().join("project")).unwrap())
        .await
        .unwrap();
    expire(&case, &session).await;
    queued(&case.host).await;
    std::fs::rename(
        case.root().join("project"),
        case.root().join("original-project"),
    )
    .unwrap();
    std::fs::create_dir(case.root().join("project")).unwrap();
    std::fs::write(&path, "replacement-folder data").unwrap();
    drop(held);
    idle(&case.host).await;
    assert_eq!(
        std::fs::read(case.root().join("original-project").join(challenge)).unwrap(),
        original
    );
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "replacement-folder data"
    );
}

#[tokio::test]
async fn shutdown_and_full_cleanup_capacity_preserve_proofs_without_mutation() {
    for closing in [false, true] {
        let case = Case::new();
        let (id, name, _) = case.open().await;
        let path = case.root().join(name);
        let original = std::fs::read(&path).unwrap();
        let registrations = if closing {
            case.host.coordinator.shutdown().await;
            Vec::new()
        } else {
            (0..256)
                .map(|_| case.host.coordinator.worker().unwrap())
                .collect::<Vec<_>>()
        };
        expire(&case, &id).await;
        assert_eq!(std::fs::read(path).unwrap(), original);
        assert_eq!(case.host.coordinator.waiting(), 0);
        assert_eq!(case.host.coordinator.workers(), registrations.len());
        drop(registrations);
    }
}

#[test]
fn dropping_a_host_after_its_runtime_has_stopped_preserves_the_proof() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (host, path) = runtime.block_on(async {
        let host = Host::default();
        let FolderReply::Opened { challenge, .. } = host
            .request(
                root.clone(),
                "paired/user:1".into(),
                FolderRequest {
                    user: 1,
                    project: 1,
                    action: FolderAction::Open {
                        path: "".into(),
                        expected: None,
                    },
                },
            )
            .await
            .unwrap()
        else {
            panic!()
        };
        (host, root.join(challenge))
    });
    let original = std::fs::read(&path).unwrap();
    drop(runtime);
    drop(host);
    assert_eq!(std::fs::read(path).unwrap(), original);
}

struct Peer(std::process::Child);
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Peer {
    async fn start(root: &Path, signal: &Path) -> Self {
        let mut peer = Self(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "folder_coordination::tests::processes::directory_lock_peer",
                    "--nocapture",
                ])
                .env("OPENWEBIDE_TEST_FOLDER_ROOT", root)
                .env("OPENWEBIDE_TEST_FOLDER_SIGNAL", signal)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while !signal.with_extension("held").exists() {
                assert!(peer.0.try_wait().unwrap().is_none());
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        peer
    }
}
#[tokio::test]
async fn expired_proof_cleanup_respects_a_separate_process_holding_an_ancestor_alias() {
    let case = Case::new();
    let signals = tempfile::tempdir().unwrap();
    std::fs::create_dir(case.root().join("child")).unwrap();
    let alias = signals.path().join("alias");
    std::os::unix::fs::symlink(case.root(), &alias).unwrap();
    let signal = signals.path().join("peer");
    let FolderReply::Opened {
        session, challenge, ..
    } = case
        .request(FolderAction::Open {
            path: "child".into(),
            expected: None,
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    let path = case.root().join("child").join(challenge);
    let original = std::fs::read(&path).unwrap();
    let mut peer = Peer::start(&alias, &signal).await;
    expire(&case, &session).await;
    queued(&case.host).await;
    assert_eq!(std::fs::read(&path).unwrap(), original);
    std::fs::write(signal.with_extension("release"), "release").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while peer.0.try_wait().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    idle(&case.host).await;
    assert!(!path.exists());
}
