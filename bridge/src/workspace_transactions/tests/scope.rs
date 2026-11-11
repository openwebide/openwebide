use super::*;

#[tokio::test]
async fn busy_physical_folders_wait_without_snapshots_and_recheck_recovery_after_the_wait() {
    for changed in [0, 1, 2] {
        let fixture = fixture().await;
        let original = fixture
            .backend
            .store
            .workspace_transaction(
                fixture.user,
                fixture.project,
                Some(&fixture.command.request.id),
            )
            .await
            .unwrap()
            .unwrap();
        let host = Arc::new(Host::default());
        let held = host
            .coordinator
            .acquire(Folder::capture(&fixture.directory.path().join("project")).unwrap())
            .await
            .unwrap();
        let pending_host = host.clone();
        let backend = fixture.backend.clone();
        let workspace = fixture.directory.path().to_owned();
        let command = fixture.command.clone();
        let pending =
            tokio::spawn(async move { pending_host.execute(workspace, command, backend).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while host.coordinator.waiting() == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(fixture.backend.scope_reads.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.backend.snapshot_reads.load(Ordering::SeqCst), 0);
        // The independent-root capacity is still available while this worker
        // waits for a physical folder already held by another native writer.
        drop(host.slots.clone().try_acquire_owned().unwrap());
        if changed > 0 {
            fixture
                .backend
                .store
                .advance_workspace_transaction(
                    fixture.user,
                    fixture.project,
                    &AdvanceWorkspaceTransaction {
                        id: fixture.command.request.id.clone(),
                        generation: fixture.command.request.generation.clone(),
                        revision: 1,
                        progress: TransactionProgress::prepared_cancellation(),
                    },
                    101,
                )
                .await
                .unwrap();
        }
        if changed == 2 {
            // Expire the terminal record through normal retention, then reuse
            // its public ID and revision with precisely the same reviewed data.
            // Only the server-owned generation distinguishes this replacement.
            let intermediate = "b".repeat(32);
            for (id, now) in [
                (intermediate.clone(), 1000),
                (fixture.command.request.id.clone(), 1002),
            ] {
                let prepared = fixture
                    .backend
                    .store
                    .begin_workspace_transaction(
                        fixture.user,
                        fixture.project,
                        &BeginWorkspaceTransaction {
                            language_review: None,
                            id: id.clone(),
                            plan: original.journal.plan.clone(),
                            editor: original.editor.clone(),
                        },
                        now,
                    )
                    .await
                    .unwrap();
                if id == intermediate {
                    fixture
                        .backend
                        .store
                        .advance_workspace_transaction(
                            fixture.user,
                            fixture.project,
                            &AdvanceWorkspaceTransaction {
                                id,
                                generation: prepared.journal.generation,
                                revision: 1,
                                progress: TransactionProgress::prepared_cancellation(),
                            },
                            now + 1,
                        )
                        .await
                        .unwrap();
                }
            }
        }
        drop(held);
        let outcome = pending.await.unwrap();
        if changed > 0 {
            assert!(outcome.unwrap_err().contains("changed"));
            assert!(fixture.directory.path().join("project/before.rs").exists());
            assert!(!fixture.directory.path().join("project/after.rs").exists());
        } else {
            let outcome = outcome.unwrap();
            assert!(outcome.error.is_none(), "{:?}", outcome.error);
            assert_eq!(outcome.phase, Some(TransactionPhase::Committed));
            assert!(!fixture.directory.path().join("project/before.rs").exists());
            assert!(fixture.directory.path().join("project/after.rs").exists());
        }
        assert!(fixture.backend.snapshot_reads.load(Ordering::SeqCst) >= 1);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn replacement_after_loading_rejects_the_original_phase_before_any_workspace_mutation() {
    let fixture = fixture_version(2).await;
    let root = fixture.directory.path().join("project");
    let original_source = same_file::Handle::from_path(root.join("before.rs")).unwrap();
    let original_destination = same_file::Handle::from_path(root.join("after.rs")).unwrap();
    fixture.backend.pause.store(true, Ordering::SeqCst);
    let host = Arc::new(Host::default());
    let executing = host.clone();
    let backend = fixture.backend.clone();
    let workspace = fixture.directory.path().to_owned();
    let command = fixture.command.clone();
    let pending = tokio::spawn(async move { executing.execute(workspace, command, backend).await });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fixture.backend.entered.notified(),
    )
    .await
    .unwrap();
    let original = fixture
        .backend
        .store
        .workspace_transaction(
            fixture.user,
            fixture.project,
            Some(&fixture.command.request.id),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(original.journal.revision, 2);
    assert!(
        fixture
            .backend
            .store
            .advance_workspace_transaction(
                fixture.user,
                fixture.project,
                &AdvanceWorkspaceTransaction {
                    id: original.journal.id.clone(),
                    generation: original.journal.generation.clone(),
                    revision: original.journal.revision,
                    progress: original.journal.progress.clone(),
                },
                101,
            )
            .await
            .is_err()
    );
    // Ordinary callbacks cannot retire the active worker. This privileged fault
    // fixture impersonates its private credential to preserve the independent
    // same-ID/revision replacement regression, including original-object checks.
    let worker = fixture.backend.last_worker.lock().unwrap().clone().unwrap();
    for progress in [
        TransactionProgress {
            phase: TransactionPhase::Reverting,
            completed: 0,
            active: false,
            rollback_from: Some(0),
        },
        TransactionProgress {
            phase: TransactionPhase::Finalizing,
            completed: 0,
            active: false,
            rollback_from: Some(0),
        },
        TransactionProgress::prepared_cancellation(),
    ] {
        let state = fixture
            .backend
            .store
            .workspace_transaction(
                fixture.user,
                fixture.project,
                Some(&fixture.command.request.id),
            )
            .await
            .unwrap()
            .unwrap();
        fixture
            .backend
            .store
            .advance_workspace_transaction_for_worker(
                fixture.user,
                fixture.project,
                &AdvanceWorkspaceTransaction {
                    id: state.journal.id,
                    generation: state.journal.generation,
                    revision: state.journal.revision,
                    progress,
                },
                Some(&worker),
                101,
            )
            .await
            .unwrap();
    }
    for (id, now) in [
        ("b".repeat(32), 1000),
        (fixture.command.request.id.clone(), 1002),
    ] {
        let prepared = fixture
            .backend
            .store
            .begin_workspace_transaction(
                fixture.user,
                fixture.project,
                &BeginWorkspaceTransaction {
                    language_review: None,
                    id: id.clone(),
                    plan: original.journal.plan.clone(),
                    editor: original.editor.clone(),
                },
                now,
            )
            .await
            .unwrap();
        let progress = if id == fixture.command.request.id {
            TransactionProgress {
                phase: TransactionPhase::Applying,
                ..TransactionProgress::default()
            }
        } else {
            TransactionProgress::prepared_cancellation()
        };
        fixture
            .backend
            .store
            .advance_workspace_transaction(
                fixture.user,
                fixture.project,
                &AdvanceWorkspaceTransaction {
                    id,
                    generation: prepared.journal.generation,
                    revision: 1,
                    progress,
                },
                now + 1,
            )
            .await
            .unwrap();
    }
    let replacement = fixture
        .backend
        .store
        .workspace_transaction_scope(fixture.user, fixture.project, &fixture.command.request.id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(original.journal.generation, replacement.generation);
    assert_eq!(original.journal.id, replacement.id);
    assert_eq!(original.journal.revision, replacement.revision);
    fixture.backend.release.notify_one();
    let outcome = pending.await.unwrap().unwrap();
    assert!(outcome.error.is_some());
    assert_eq!(outcome.generation, fixture.command.request.generation);
    assert_eq!(
        same_file::Handle::from_path(root.join("before.rs")).unwrap(),
        original_source
    );
    assert_eq!(
        same_file::Handle::from_path(root.join("after.rs")).unwrap(),
        original_destination
    );
    assert_eq!(
        std::fs::read_to_string(root.join("before.rs")).unwrap(),
        "disk 文😀\r\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("after.rs")).unwrap(),
        "overwritten original"
    );
    assert_eq!(
        fixture
            .backend
            .store
            .workspace_transaction_scope(fixture.user, fixture.project, &fixture.command.request.id)
            .await
            .unwrap(),
        Some(replacement)
    );
}
