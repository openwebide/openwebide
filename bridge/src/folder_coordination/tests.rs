use super::*;
use futures::FutureExt;

#[cfg(unix)]
mod processes;

#[tokio::test]
async fn unconfirmed_cleanup_rejects_overlapping_recovery_without_blocking_independent_work_or_shutdown()
 {
    let directory = tempfile::tempdir().unwrap();
    for name in ["project/child", "independent"] {
        std::fs::create_dir_all(directory.path().join(name)).unwrap();
    }
    let coordinator = Coordinator::default();
    let folder = |name| Folder::capture(&directory.path().join(name)).unwrap();
    let active = coordinator.acquire(folder("project")).await.unwrap();
    let mut queued = Box::pin(coordinator.acquire(folder("project/child")));
    assert!(queued.as_mut().now_or_never().is_none());
    active.quarantine();
    assert!(queued.await.is_err());
    for name in ["project", "project/child", ""] {
        assert!(coordinator.acquire(folder(name)).await.is_err());
    }
    let independent = coordinator.acquire(folder("independent")).await.unwrap();
    drop(independent);
    coordinator.shutdown().await;
}

#[tokio::test]
async fn shutdown_closes_admission_and_drains_workers_without_waiting_for_frozen_waiters() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = Coordinator::default();
    let worker = coordinator.worker().unwrap();
    let active = coordinator
        .acquire(Folder::capture(directory.path()).unwrap())
        .await
        .unwrap();
    let mut queued = Box::pin(coordinator.acquire(Folder::capture(directory.path()).unwrap()));
    assert!(queued.as_mut().now_or_never().is_none());
    let mut shutdown = Box::pin(coordinator.shutdown());
    assert!(shutdown.as_mut().now_or_never().is_none());
    assert!(coordinator.worker().is_err());
    assert!(
        coordinator
            .acquire(Folder::capture(directory.path()).unwrap())
            .await
            .is_err()
    );
    drop(active);
    assert!(shutdown.as_mut().now_or_never().is_none());
    // The queued future is intentionally not polled until shutdown is done.
    drop(worker);
    shutdown.await;
    assert!(queued.await.is_err());
}

#[tokio::test]
async fn conflicting_folders_wait_fairly_while_independent_folders_continue() {
    let directory = tempfile::tempdir().unwrap();
    for name in ["project/one", "project/two", "independent"] {
        std::fs::create_dir_all(directory.path().join(name)).unwrap();
    }
    let coordinator = Coordinator::default();
    let folder = |name| Folder::capture(&directory.path().join(name)).unwrap();
    let first = coordinator.acquire(folder("project/one")).await.unwrap();
    let mut parent = Box::pin(coordinator.acquire(folder("project")));
    assert!(parent.as_mut().now_or_never().is_none());
    let mut sibling = Box::pin(coordinator.acquire(folder("project/two")));
    assert!(sibling.as_mut().now_or_never().is_none());
    let independent = coordinator.acquire(folder("independent")).await.unwrap();
    drop(first);
    let parent = parent.await.unwrap();
    assert!(sibling.as_mut().now_or_never().is_none());
    drop(parent);
    let _sibling = sibling.await.unwrap();
    drop(independent);
}

#[tokio::test]
async fn cancelled_waiters_do_not_reserve_folders_or_block_later_work() {
    let directory = tempfile::tempdir().unwrap();
    for name in ["one", "two"] {
        std::fs::create_dir(directory.path().join(name)).unwrap();
    }
    let coordinator = Coordinator::default();
    let first = coordinator
        .acquire(Folder::capture(&directory.path().join("one")).unwrap())
        .await
        .unwrap();
    let mut parent = Box::pin(coordinator.acquire(Folder::capture(directory.path()).unwrap()));
    assert!(parent.as_mut().now_or_never().is_none());
    let mut sibling =
        Box::pin(coordinator.acquire(Folder::capture(&directory.path().join("two")).unwrap()));
    assert!(sibling.as_mut().now_or_never().is_none());
    drop(parent);
    let _sibling = sibling.await.unwrap();
    drop(first);
}

#[tokio::test]
async fn replaced_queued_folders_fail_before_execution_without_leaking_their_lease() {
    let directory = tempfile::tempdir().unwrap();
    let selected = directory.path().join("selected");
    std::fs::create_dir(&selected).unwrap();
    let coordinator = Coordinator::default();
    let parent = coordinator
        .acquire(Folder::capture(directory.path()).unwrap())
        .await
        .unwrap();
    let mut queued = Box::pin(coordinator.acquire(Folder::capture(&selected).unwrap()));
    assert!(queued.as_mut().now_or_never().is_none());
    std::fs::rename(&selected, directory.path().join("original")).unwrap();
    std::fs::create_dir(&selected).unwrap();
    drop(parent);
    assert!(queued.await.is_err());
    let _new = coordinator
        .acquire(Folder::capture(&selected).unwrap())
        .await
        .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn physical_ancestors_cover_symlink_aliases_and_renamed_roots() {
    let directory = tempfile::tempdir().unwrap();
    let original = directory.path().join("original");
    std::fs::create_dir_all(original.join("child")).unwrap();
    let alias = directory.path().join("alias");
    std::os::unix::fs::symlink(&original, &alias).unwrap();
    let coordinator = Coordinator::default();
    let first = coordinator
        .acquire(Folder::capture(&original).unwrap())
        .await
        .unwrap();
    let mut aliased = Box::pin(coordinator.acquire(Folder::capture(&alias.join("child")).unwrap()));
    assert!(aliased.as_mut().now_or_never().is_none());
    drop(aliased);
    let moved = directory.path().join("moved");
    std::fs::rename(&original, &moved).unwrap();
    let mut child = Box::pin(coordinator.acquire(Folder::capture(&moved.join("child")).unwrap()));
    assert!(child.as_mut().now_or_never().is_none());
    drop(first);
    let _child = child.await.unwrap();
}

#[cfg(unix)]
#[test]
fn captured_folder_rejects_a_reparented_root_reached_through_a_new_alias() {
    let directory = tempfile::tempdir().unwrap();
    let selected = directory.path().join("before/project");
    let destination = directory.path().join("after");
    std::fs::create_dir_all(&selected).unwrap();
    std::fs::create_dir(&destination).unwrap();
    let original = Folder::capture(&selected).unwrap();
    std::fs::rename(&selected, destination.join("project")).unwrap();
    std::os::unix::fs::symlink(destination.join("project"), &selected).unwrap();
    assert_eq!(
        same_file::Handle::from_path(&selected).unwrap(),
        *original.handle
    );
    assert!(
        !original.current(),
        "The captured ancestor gate no longer covers this namespace"
    );
    let fresh = Folder::capture(&selected).unwrap();
    assert!(fresh.current());
    assert!(fresh.overlaps(&Folder::capture(&destination).unwrap()));
}

#[test]
fn captured_folder_rejects_replaced_ancestors_even_when_the_leaf_and_absolute_path_match() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("parent");
    let selected = parent.join("project");
    let original_parent = directory.path().join("original-parent");
    std::fs::create_dir_all(&selected).unwrap();
    let original = Folder::capture(&selected).unwrap();
    std::fs::rename(&parent, &original_parent).unwrap();
    std::fs::create_dir(&parent).unwrap();
    std::fs::rename(original_parent.join("project"), &selected).unwrap();
    assert_eq!(selected.canonicalize().unwrap(), original.path);
    assert_eq!(
        same_file::Handle::from_path(&selected).unwrap(),
        *original.handle
    );
    assert!(
        !original.current(),
        "A matching leaf cannot substitute a new ancestor for its physical fence"
    );
    let fresh = Folder::capture(&selected).unwrap();
    assert!(fresh.current());
    assert!(fresh.overlaps(&Folder::capture(&parent).unwrap()));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn shared_command_uncertainty_keeps_the_original_gate_after_every_observer_drops() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = Coordinator::default();
    let folder = Folder::capture(directory.path()).unwrap();
    let active = Arc::new(coordinator.acquire(folder.clone()).await.unwrap());
    let observer = active.clone();
    active.mark_uncertain();
    assert!(!active.covers(&folder));
    drop(active);
    let physical = std::fs::File::open(directory.path()).unwrap();
    assert!(matches!(
        physical.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    let mut queued = Box::pin(coordinator.acquire(folder.clone()));
    assert!(queued.as_mut().now_or_never().is_none());
    drop(observer);
    assert!(queued.await.is_err());
    assert!(coordinator.acquire(folder).await.is_err());
    assert!(matches!(
        physical.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    coordinator.shutdown().await;
}
