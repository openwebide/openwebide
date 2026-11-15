use super::*;

#[tokio::test]
async fn prepared_recovery_requires_finished_files_and_fences_the_original_producer() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for finished in [false, true] {
            for stage in [
                CommandStage::Prepared,
                CommandStage::Admitted,
                CommandStage::Dispatched,
            ] {
                let home = tempfile::tempdir().unwrap();
                let root = tempfile::tempdir().unwrap();
                let coordinator = Coordinator::default();
                coordinator.set_recovery_home(Arc::new(home.path().into()));
                let folder = Folder::capture(root.path()).unwrap();
                let namespace = Namespace::open(home.path()).unwrap();
                let mut recovery = original();
                recovery.scope.root.mode = mode;
                recovery.scope.root.path =
                    (mode == WorkspaceMode::Remote).then(|| "project".into());
                recovery.command.phase = ActionPhase::Approved;
                recovery.activity.file_recovery = true;
                let authority =
                    RecoveryAuthority::for_scope(7, 11, &recovery.scope, "A".repeat(43)).unwrap();
                let permit = namespace
                    .permit(&folder, &authority, &recovery.scope, recovery.activity)
                    .unwrap()
                    .with_origin(Some("http://127.0.0.1:3000/api"))
                    .unwrap();
                let lease = coordinator
                    .acquire_recovery(folder.clone(), permit.clone())
                    .await
                    .unwrap();
                let worker = WorkspaceTransactionWorker {
                    id: "8".repeat(32),
                    secret: "9".repeat(32),
                };
                namespace
                    .reserve_command(
                        &folder,
                        &lease,
                        &permit,
                        CommandIntent {
                            recovery: &recovery,
                            worker: &worker,
                        },
                    )
                    .unwrap();
                namespace.files_admitted(&folder, &lease, &permit).unwrap();
                recovery.activity.file_recovery = false;
                if finished {
                    namespace
                        .files_finished(&folder, &lease, &permit, recovery.activity)
                        .unwrap();
                }
                recovery.command.phase = ActionPhase::Running;
                namespace
                    .command_intent(
                        &folder,
                        &lease,
                        &permit,
                        CommandIntent {
                            recovery: &recovery,
                            worker: &worker,
                        },
                    )
                    .unwrap();
                if stage != CommandStage::Prepared {
                    namespace
                        .command_admitted(&folder, &lease, &permit, &worker)
                        .unwrap();
                }
                if stage == CommandStage::Dispatched {
                    namespace
                        .command_dispatch(&folder, &lease, &permit, &worker)
                        .unwrap();
                }
                drop(lease);
                drop(namespace);
                let namespace = Namespace::open(home.path()).unwrap();
                let pending = namespace
                    .pending_command(7, 11, mode, &recovery.command.invocation)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    pending.can_reject,
                    finished && stage == CommandStage::Prepared,
                    "{mode:?}/{finished}/{stage:?}"
                );
                assert!(
                    !pending.can_cancel
                        && !pending.dispatched_cleanup
                        && !pending.process_cleanup_confirmed
                );
                assert!(coordinator.acquire(folder.clone()).await.is_err());
                if pending.can_reject {
                    let (folder, permit) = pending.reopen().unwrap();
                    let lease = coordinator
                        .acquire_recovery(folder.clone(), permit.clone())
                        .await
                        .unwrap();
                    let outcome = CommandOutcome::Rejected {
                        message: "Original intent recovered before dispatch 文😀".into(),
                    };
                    namespace
                        .command_outcome(&folder, &lease, &permit, &worker, &outcome, true)
                        .unwrap();
                    assert!(
                        namespace
                            .command_admitted(&folder, &lease, &permit, &worker)
                            .is_err()
                    );
                    assert!(
                        namespace
                            .command_dispatch(&folder, &lease, &permit, &worker)
                            .is_err()
                    );
                    assert!(
                        namespace
                            .command_process_cleanup(&folder, &lease, &permit, &worker)
                            .is_err()
                    );
                    let (record, new) = namespace
                        .command_intent(
                            &folder,
                            &lease,
                            &permit,
                            CommandIntent {
                                recovery: &recovery,
                                worker: &worker,
                            },
                        )
                        .unwrap();
                    assert!(!new);
                    assert_eq!(record.outcome, Some(outcome));
                }
            }
        }
    }
}
