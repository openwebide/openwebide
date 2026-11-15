use super::*;
use futures::FutureExt;
use std::time::Duration;

fn project() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("child/descendant")).unwrap();
    root
}
async fn admitted(work: impl std::future::Future<Output = Result<Lease, String>>) -> Lease {
    tokio::time::timeout(Duration::from_secs(2), work)
        .await
        .expect("Compatible ordinary work did not enter")
        .unwrap()
}

#[tokio::test]
async fn ordinary_parent_child_and_alias_users_coexist_without_review_authority() {
    let root = project();
    let first = Coordinator::default();
    let second = Coordinator::default();
    let parent = Folder::capture(root.path()).unwrap();
    let child = Folder::capture(&root.path().join("child")).unwrap();
    let parent_use = admitted(first.acquire_ordinary(parent.clone())).await;
    let child_use = admitted(second.acquire_ordinary(child.clone())).await;
    assert!(parent_use.permits_files(&child));
    assert!(!parent_use.covers(&parent));
    assert!(
        crate::runs::native_vfs::workspace::WorkspaceFiles::with_lease(
            child.clone(),
            Arc::new(child_use),
        )
        .is_err()
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.path().join("child"), root.path().join("alias")).unwrap();
        let alias = Folder::capture(&root.path().join("alias")).unwrap();
        let _alias = admitted(second.acquire_ordinary(alias)).await;
    }
}

#[tokio::test]
async fn reviewed_ancestors_and_descendants_exclude_ordinary_work_across_coordinators() {
    let root = project();
    for ordinary_parent in [true, false] {
        for reviewed_first in [true, false] {
            let first = Coordinator::default();
            let second = Coordinator::default();
            let parent = Folder::capture(root.path()).unwrap();
            let child = Folder::capture(&root.path().join("child")).unwrap();
            let (ordinary, reviewed) = if ordinary_parent {
                (parent, child)
            } else {
                (child, parent)
            };
            if reviewed_first {
                let held = admitted(first.acquire(reviewed)).await;
                let mut waiting = Box::pin(second.acquire_ordinary(ordinary));
                assert!(waiting.as_mut().now_or_never().is_none());
                drop(held);
                drop(admitted(waiting).await);
            } else {
                let held = admitted(first.acquire_ordinary(ordinary)).await;
                let mut waiting = Box::pin(second.acquire(reviewed));
                assert!(waiting.as_mut().now_or_never().is_none());
                drop(held);
                drop(admitted(waiting).await);
            }
        }
    }
}

#[tokio::test]
async fn a_waiting_review_prevents_later_ordinary_work_from_barging() {
    let root = project();
    let coordinator = Coordinator::default();
    let parent = Folder::capture(root.path()).unwrap();
    let held = admitted(coordinator.acquire_ordinary(parent.clone())).await;
    let mut review = Box::pin(coordinator.acquire(parent.clone()));
    assert!(review.as_mut().now_or_never().is_none());
    let mut ordinary = Box::pin(coordinator.acquire_ordinary(parent));
    assert!(ordinary.as_mut().now_or_never().is_none());
    drop(held);
    let review = admitted(review).await;
    assert!(ordinary.as_mut().now_or_never().is_none());
    drop(review);
    drop(admitted(ordinary).await);
    let state = coordinator.0.state.lock().unwrap();
    assert!(state.uses.is_empty() && state.waiting.is_empty() && state.active.is_empty());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn quarantined_ordinary_work_excludes_other_coordinators_without_blocking_siblings() {
    let root = project();
    std::fs::create_dir(root.path().join("sibling")).unwrap();
    for selected in [root.path().to_path_buf(), root.path().join("child")] {
        for caller in [
            root.path().to_path_buf(),
            root.path().join("child"),
            root.path().join("child/descendant"),
        ] {
            let first = Coordinator::default();
            let second = Coordinator::default();
            let folder = Folder::capture(&selected).unwrap();
            let lease = admitted(first.acquire_ordinary(folder.clone())).await;
            lease.mark_uncertain();
            drop(lease);
            assert!(first.acquire_ordinary(folder).await.is_err());
            let mut pending = Box::pin(second.acquire_ordinary(Folder::capture(&caller).unwrap()));
            let admitted_while_quarantined = matches!(pending.as_mut().now_or_never(), Some(Ok(_)));
            if selected.ends_with("child") {
                drop(
                    admitted(
                        second.acquire_ordinary(
                            Folder::capture(&root.path().join("sibling")).unwrap(),
                        ),
                    )
                    .await,
                );
            }
            // The old contract retains a live host's quarantine. Releasing that
            // host makes the same ordinary request admissible again.
            drop(first);
            assert!(
                !admitted_while_quarantined,
                "An ordinary writer entered after a different coordinator quarantined this physical folder"
            );
            drop(admitted(pending).await);
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn transferred_command_descriptions_retain_quarantine_after_the_original_lease_drops() {
    let root = project();
    let first = Coordinator::default();
    let second = Coordinator::default();
    let child = root.path().join("child");
    let lease = admitted(first.acquire_ordinary(Folder::capture(&child).unwrap())).await;
    let descriptions = lease.command_descriptors(&child).unwrap();
    quarantine_command(&descriptions).unwrap();
    drop(lease);
    let mut pending = Box::pin(second.acquire_ordinary(Folder::capture(root.path()).unwrap()));
    assert!(pending.as_mut().now_or_never().is_none());
    drop(descriptions);
    drop(admitted(pending).await);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn ordinary_quarantine_peer() {
    let Some(root) = std::env::var_os("OPENWEBIDE_TEST_ORDINARY_QUARANTINE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let coordinator = Coordinator::default();
    let lease =
        admitted(coordinator.acquire_ordinary(Folder::capture(&root.join("child")).unwrap())).await;
    lease.mark_uncertain();
    drop(lease);
    std::fs::write(root.join("ready"), "quarantined").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.join("release").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn an_independent_process_quarantines_ordinary_ancestors_but_not_siblings() {
    struct Peer(std::process::Child);
    impl Drop for Peer {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let root = project();
    std::fs::create_dir(root.path().join("sibling")).unwrap();
    let mut peer = Peer(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "folder_coordination::ordinary_tests::ordinary_quarantine_peer",
                "--nocapture",
            ])
            .env("OPENWEBIDE_TEST_ORDINARY_QUARANTINE_ROOT", root.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.path().join("ready").exists() {
            assert!(
                peer.0.try_wait().unwrap().is_none(),
                "Quarantine peer exited before admission"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let coordinator = Coordinator::default();
    let mut pending = Box::pin(coordinator.acquire_ordinary(Folder::capture(root.path()).unwrap()));
    assert!(pending.as_mut().now_or_never().is_none());
    drop(
        admitted(
            coordinator.acquire_ordinary(Folder::capture(&root.path().join("sibling")).unwrap()),
        )
        .await,
    );
    std::fs::write(root.path().join("release"), "").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(status) = peer.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(admitted(pending).await);
}
