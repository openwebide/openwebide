//! Separate native runtimes must exclude the same physical workspace, even
//! when their account/project maps and in-memory coordinators are independent.
use super::*;
use std::{path::Path, process::Child, time::Duration};

struct Peer(Child);
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Peer {
    fn start(root: &Path, signal: &Path) -> Self {
        Self(
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
        )
    }
    async fn wait(&mut self, path: &Path) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !path.exists() {
                assert!(
                    self.0.try_wait().unwrap().is_none(),
                    "peer stopped before {path:?}"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn stop(&self) {
        let pid = i32::try_from(self.0.id()).unwrap();
        // SAFETY: pid identifies this fixture's own live child. SIGSTOP freezes
        // it without dropping its directory handles or running any destructors.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let mut status = 0;
                // SAFETY: status is valid output and WNOHANG never blocks. A
                // stopped status is observed without reaping the live child.
                let result =
                    unsafe { libc::waitpid(pid, &raw mut status, libc::WUNTRACED | libc::WNOHANG) };
                assert!(result >= 0);
                if result == pid {
                    assert!(libc::WIFSTOPPED(status));
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    fn resume(&self) {
        let pid = i32::try_from(self.0.id()).unwrap();
        // SAFETY: resume the fixture child whose stopped status we observed.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGCONT) }, 0);
    }
}

#[tokio::test]
async fn directory_lock_peer() {
    let Some(root) = std::env::var_os("OPENWEBIDE_TEST_FOLDER_ROOT") else {
        return;
    };
    let signal =
        std::path::PathBuf::from(std::env::var_os("OPENWEBIDE_TEST_FOLDER_SIGNAL").unwrap());
    let coordinator = Coordinator::default();
    let mut work = Box::pin(coordinator.acquire(Folder::capture(Path::new(&root)).unwrap()));
    let lease = if let Some(result) = work.as_mut().now_or_never() {
        std::fs::write(signal.with_extension("observed"), "held").unwrap();
        result.unwrap()
    } else {
        std::fs::write(signal.with_extension("observed"), "waiting").unwrap();
        work.await.unwrap()
    };
    std::fs::write(signal.with_extension("held"), "held").unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !signal.with_extension("release").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(lease);
}

#[tokio::test]
async fn independent_processes_wait_for_physical_parents_and_aliases_while_other_roots_continue() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::create_dir_all(root.join("project/child")).unwrap();
    std::fs::create_dir(root.join("independent")).unwrap();
    std::os::unix::fs::symlink(root.join("project"), root.join("alias")).unwrap();
    let signal = root.join("peer");
    let mut peer = Peer::start(&root.join("project"), &signal);
    peer.wait(&signal.with_extension("held")).await;
    let coordinator = Coordinator::default();
    for path in ["project", "project/child", "alias/child"] {
        let mut waiting = Box::pin(coordinator.acquire(Folder::capture(&root.join(path)).unwrap()));
        assert!(
            waiting.as_mut().now_or_never().is_none(),
            "foreign owner bypassed at {path}"
        );
        drop(waiting);
    }
    drop(
        tokio::time::timeout(
            Duration::from_secs(1),
            coordinator.acquire(Folder::capture(&root.join("independent")).unwrap()),
        )
        .await
        .unwrap()
        .unwrap(),
    );
    let mut waiting =
        Box::pin(coordinator.acquire(Folder::capture(&root.join("alias/child")).unwrap()));
    assert!(waiting.as_mut().now_or_never().is_none());
    std::fs::write(signal.with_extension("release"), "release").unwrap();
    drop(
        tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap(),
    );
}

#[tokio::test]
async fn unconfirmed_cleanup_keeps_the_physical_fence_visible_to_another_process() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let coordinator = Coordinator::default();
    coordinator
        .acquire(Folder::capture(&root).unwrap())
        .await
        .unwrap()
        .quarantine();
    let signal = directory.path().join("peer");
    let mut peer = Peer::start(&root, &signal);
    peer.wait(&signal.with_extension("observed")).await;
    assert_eq!(
        std::fs::read_to_string(signal.with_extension("observed")).unwrap(),
        "waiting"
    );
    assert!(!signal.with_extension("held").exists());
    // No accepted worker survives this coordinator in this fixture. This does
    // not model descendants surviving a crashed execution host.
    drop(coordinator);
    peer.wait(&signal.with_extension("held")).await;
    std::fs::write(signal.with_extension("release"), "release").unwrap();
}

#[tokio::test]
async fn a_frozen_foreign_waiter_holds_no_os_locks_and_cannot_delay_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    std::fs::create_dir_all(root.join("child")).unwrap();
    let signal = directory.path().join("peer");
    let mut peer = Peer::start(&root, &signal);
    peer.wait(&signal.with_extension("held")).await;
    let coordinator = Coordinator::default();
    let mut waiting = Box::pin(coordinator.acquire(Folder::capture(&root.join("child")).unwrap()));
    assert!(waiting.as_mut().now_or_never().is_none());
    tokio::time::timeout(Duration::from_secs(1), coordinator.shutdown())
        .await
        .unwrap();
    // The peer has no descendants. Its actual process exit releases its OS
    // locks; neither a timer nor a new host claiming ownership releases them.
    drop(peer);
    let fresh = Coordinator::default();
    drop(
        tokio::time::timeout(
            Duration::from_secs(1),
            fresh.acquire(Folder::capture(directory.path()).unwrap()),
        )
        .await
        .unwrap()
        .unwrap(),
    );
    assert!(waiting.await.is_err());
}

#[tokio::test]
async fn a_foreign_wait_rejects_root_replacement_and_preserves_the_new_folder() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let signal = directory.path().join("peer");
    let mut peer = Peer::start(&root, &signal);
    peer.wait(&signal.with_extension("held")).await;
    let coordinator = Coordinator::default();
    let mut waiting = Box::pin(coordinator.acquire(Folder::capture(&root).unwrap()));
    assert!(waiting.as_mut().now_or_never().is_none());
    std::fs::rename(&root, directory.path().join("original")).unwrap();
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("external"), "new folder").unwrap();
    let error = tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .unwrap()
        .err()
        .unwrap();
    assert!(error.contains("changed"), "{error}");
    assert_eq!(
        std::fs::read_to_string(root.join("external")).unwrap(),
        "new folder"
    );
    assert!(peer.0.try_wait().unwrap().is_none());
    std::fs::write(signal.with_extension("release"), "release").unwrap();
}

#[tokio::test]
async fn a_stopped_native_owner_keeps_exclusion_until_it_resumes_and_releases() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    std::fs::create_dir_all(root.join("child")).unwrap();
    let signal = directory.path().join("peer");
    let mut peer = Peer::start(&root, &signal);
    peer.wait(&signal.with_extension("held")).await;
    peer.stop().await;
    let coordinator = Coordinator::default();
    let mut waiting = Box::pin(coordinator.acquire(Folder::capture(&root.join("child")).unwrap()));
    assert!(waiting.as_mut().now_or_never().is_none());
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(waiting.as_mut().now_or_never().is_none());
    assert!(peer.0.try_wait().unwrap().is_none());
    peer.resume();
    std::fs::write(signal.with_extension("release"), "release").unwrap();
    drop(
        tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap(),
    );
}
