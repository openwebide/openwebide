use super::*;
use std::{sync::Arc, time::Duration};
use tokio::sync::Notify;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod root;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod runtime;

#[cfg(unix)]
#[tokio::test]
async fn backend_root_confinement_checks_the_captured_object_after_path_replacement() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let workspace = workspace.path().canonicalize().unwrap();
    let selected = workspace.join("selected");
    std::fs::create_dir(&selected).unwrap();
    let earlier_path = selected.canonicalize().unwrap();
    std::fs::rename(&selected, workspace.join("original")).unwrap();
    std::os::unix::fs::symlink(outside.path(), &selected).unwrap();
    assert!(matches!(
        WorkspaceFiles::in_workspace(earlier_path, &workspace, Coordinator::default()),
        Err(VfsError::PathEscape(_))
    ));
    std::fs::remove_file(&selected).unwrap();
    std::os::unix::fs::symlink(workspace.join("original"), &selected).unwrap();
    let files = WorkspaceFiles::in_workspace(selected, &workspace, Coordinator::default()).unwrap();
    files.write("allowed", "original folder").await.unwrap();
    assert_eq!(
        std::fs::read(workspace.join("original/allowed")).unwrap(),
        b"original folder"
    );
    assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
}

async fn queued(coordinator: &Coordinator) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while coordinator.waiting() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("File writer did not queue for native access");
}

#[tokio::test]
async fn every_file_mutator_waits_for_the_same_gate_while_reads_remain_available() {
    for operation in 0..8 {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("source"), "before").unwrap();
        let coordinator = Coordinator::default();
        let files = WorkspaceFiles::new(directory.path().into(), coordinator.clone());
        let held = coordinator
            .acquire(Folder::capture(directory.path()).unwrap())
            .await
            .unwrap();
        let writer = files.clone();
        let task = tokio::spawn(async move {
            match operation {
                0 => writer.write("source", "after").await,
                1 => writer.write_bytes("source", b"after").await,
                2 => writer.replace_atomic("source", b"after").await,
                3 => writer.create("created", VfsEntryKind::File).await,
                4 => writer.delete("source").await,
                5 => writer.remove_entry("source", VfsEntryKind::File).await,
                6 => writer.copy("source", "copied").await,
                7 => {
                    writer
                        .move_atomic("source", "moved", VfsEntryKind::File)
                        .await
                }
                _ => unreachable!(),
            }
        });
        queued(&coordinator).await;
        assert!(!task.is_finished());
        assert_eq!(files.read("source").await.unwrap(), "before");
        for name in ["created", "copied", "moved"] {
            assert!(!directory.path().join(name).exists());
        }
        drop(held);
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        if operation == 7 && !cfg!(any(target_os = "linux", target_os = "macos")) {
            assert_eq!(
                result,
                Err(VfsError::Io(
                    "Atomic object moves are unavailable on this platform".into()
                ))
            );
            assert_eq!(files.read("source").await.unwrap(), "before");
            assert!(!directory.path().join("moved").exists());
            continue;
        }
        result.unwrap();
        match operation {
            0..=2 => assert_eq!(files.read("source").await.unwrap(), "after"),
            3 => assert!(directory.path().join("created").is_file()),
            4 | 5 => assert!(!directory.path().join("source").exists()),
            6 => assert_eq!(files.read("copied").await.unwrap(), "before"),
            7 => {
                assert_eq!(files.read("moved").await.unwrap(), "before");
                assert!(!directory.path().join("source").exists());
            }
            _ => unreachable!(),
        }
    }
}

#[tokio::test]
async fn cancelled_queued_file_write_never_starts_after_access_becomes_available() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = Coordinator::default();
    let files = WorkspaceFiles::new(directory.path().into(), coordinator.clone());
    let held = coordinator
        .acquire(Folder::capture(directory.path()).unwrap())
        .await
        .unwrap();
    let writer = files.clone();
    let task = tokio::spawn(async move { writer.write("late", "never").await });
    queued(&coordinator).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(coordinator.waiting(), 0);
    drop(held);
    files.write("acknowledged", "yes").await.unwrap();
    assert!(!directory.path().join("late").exists());
}

#[tokio::test]
async fn admitted_file_write_retains_exclusion_after_its_observer_disappears() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("source"), "before").unwrap();
    let coordinator = Coordinator::default();
    let files = WorkspaceFiles::new(directory.path().into(), coordinator.clone());
    let entered = Arc::new(Notify::new());
    let (release, proceed) = std::sync::mpsc::channel();
    let writer = files.clone();
    let started = entered.clone();
    let observer = tokio::spawn(async move {
        writer
            .mutate(move |raw| {
                move || {
                    started.notify_one();
                    proceed
                        .recv()
                        .map_err(|error| VfsError::Io(error.to_string()))?;
                    raw.write_bytes_sync("source", b"after")
                }
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    let competing = coordinator.clone();
    let folder = Folder::capture(directory.path()).unwrap();
    let next = tokio::spawn(async move { competing.acquire(folder).await });
    queued(&coordinator).await;
    assert!(!next.is_finished());
    assert_eq!(files.read("source").await.unwrap(), "before");
    release.send(()).unwrap();
    let lease = tokio::time::timeout(Duration::from_secs(2), next)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(files.read("source").await.unwrap(), "after");
    drop(lease);
}

#[tokio::test]
async fn queued_file_writer_rejects_a_replacement_root_without_touching_either_folder() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("source"), "original").unwrap();
    let coordinator = Coordinator::default();
    let files = WorkspaceFiles::new(root.clone(), coordinator.clone());
    let held = coordinator
        .acquire(Folder::capture(&root).unwrap())
        .await
        .unwrap();
    let writer = files.clone();
    let task = tokio::spawn(async move { writer.write("source", "must not appear").await });
    queued(&coordinator).await;
    let old = directory.path().join("original");
    std::fs::rename(&root, &old).unwrap();
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("source"), "replacement").unwrap();
    drop(held);
    assert!(
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(old.join("source")).unwrap(),
        "original"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("source")).unwrap(),
        "replacement"
    );
    assert!(files.read("source").await.is_err());
}

#[tokio::test]
async fn known_file_errors_release_access_for_a_followup_write() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("source"), "before").unwrap();
    let files = WorkspaceFiles::new(directory.path().into(), Coordinator::default());
    assert!(matches!(
        files.create("source", VfsEntryKind::File).await,
        Err(VfsError::AlreadyExists(_))
    ));
    files.write("source", "after").await.unwrap();
    assert_eq!(files.read("source").await.unwrap(), "after");
}

#[tokio::test]
async fn file_worker_panic_quarantines_its_physical_claim() {
    fn fail() -> impl FnOnce() -> Result<(), VfsError> {
        || -> Result<(), VfsError> { panic!("injected accepted file worker panic") }
    }
    let directory = tempfile::tempdir().unwrap();
    let coordinator = Coordinator::default();
    let files = WorkspaceFiles::new(directory.path().into(), coordinator.clone());
    assert!(files.mutate(|_| fail()).await.is_err());
    assert!(
        coordinator
            .acquire(Folder::capture(directory.path()).unwrap())
            .await
            .is_err()
    );
    assert!(!directory.path().join("unexpected").exists());
}

#[tokio::test]
async fn shutdown_cancels_queued_writes_and_waits_for_the_admitted_file_worker() {
    use futures::FutureExt;
    let directory = tempfile::tempdir().unwrap();
    let coordinator = Coordinator::default();
    let files = WorkspaceFiles::new(directory.path().into(), coordinator.clone());
    let entered = Arc::new(Notify::new());
    let (release, proceed) = std::sync::mpsc::channel();
    let writer = files.clone();
    let started = entered.clone();
    let active = tokio::spawn(async move {
        writer
            .mutate(move |raw| {
                move || {
                    started.notify_one();
                    proceed
                        .recv()
                        .map_err(|error| VfsError::Io(error.to_string()))?;
                    raw.write_bytes_sync("accepted", b"done")
                }
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    // Ordinary work may coexist. A previously queued review still fences later
    // writes, giving shutdown a real pending operation to cancel.
    let mut review = Box::pin(coordinator.acquire(Folder::capture(directory.path()).unwrap()));
    assert!(review.as_mut().now_or_never().is_none());
    let writer = files.clone();
    let pending = tokio::spawn(async move { writer.write("queued", "never").await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while coordinator.waiting() < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let closing = coordinator.clone();
    let shutdown = tokio::spawn(async move { closing.shutdown().await });
    tokio::time::timeout(Duration::from_secs(2), coordinator.stopped())
        .await
        .unwrap();
    assert!(!shutdown.is_finished());
    assert!(review.await.is_err());
    assert!(
        tokio::time::timeout(Duration::from_secs(2), pending)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), active)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), shutdown)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(files.read("accepted").await.unwrap(), "done");
    assert!(!directory.path().join("queued").exists());
    assert!(files.write("late", "never").await.is_err());
}

#[tokio::test]
async fn an_existing_outer_lease_is_reused_without_recursive_admission() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = Coordinator::default();
    let folder = Folder::capture(directory.path()).unwrap();
    let lease = Arc::new(coordinator.acquire(folder.clone()).await.unwrap());
    let files = WorkspaceFiles::with_lease(folder.clone(), lease.clone()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), files.write("source", "owned"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(files.read("source").await.unwrap(), "owned");
    drop(files);
    let mut competing = Box::pin(coordinator.acquire(folder));
    assert!(futures::poll!(&mut competing).is_pending());
    drop(lease);
    drop(
        tokio::time::timeout(Duration::from_secs(2), competing)
            .await
            .unwrap()
            .unwrap(),
    );
}

#[tokio::test]
async fn an_unrelated_or_quarantined_outer_lease_cannot_authorize_file_io() {
    let directory = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let coordinator = Coordinator::default();
    let folder = Folder::capture(directory.path()).unwrap();
    let lease = Arc::new(coordinator.acquire(folder.clone()).await.unwrap());
    assert!(
        WorkspaceFiles::with_lease(Folder::capture(other.path()).unwrap(), lease.clone()).is_err()
    );
    let files = WorkspaceFiles::with_lease(folder, lease.clone()).unwrap();
    lease.mark_uncertain();
    assert!(files.write("rejected", "never").await.is_err());
    assert!(!directory.path().join("rejected").exists());
    assert!(!other.path().join("rejected").exists());
}
