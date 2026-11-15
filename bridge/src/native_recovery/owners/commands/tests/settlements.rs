use super::*;

pub(super) async fn prepared(
    namespace: &Namespace,
    coordinator: &Coordinator,
    root: &Path,
    mode: WorkspaceMode,
    tag: u32,
) -> (
    Folder,
    Lease,
    Permit,
    WorkspaceTransactionWorker,
    WorkspaceCommandRecovery,
) {
    let folder = Folder::capture(root).unwrap();
    let mut recovery = original();
    recovery.scope.id = format!("{tag:032x}");
    recovery
        .command
        .invocation
        .id
        .clone_from(&recovery.scope.id);
    recovery.scope.root.mode = mode;
    recovery.scope.root.path = (mode == WorkspaceMode::Remote).then(|| "project".into());
    recovery.command.phase = ActionPhase::Approved;
    recovery.activity.file_recovery = true;
    let authority = RecoveryAuthority::for_scope(7, 11, &recovery.scope, "A".repeat(43)).unwrap();
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
    namespace
        .files_finished(&folder, &lease, &permit, recovery.activity)
        .unwrap();
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
    (folder, lease, permit, worker, recovery)
}

#[tokio::test]
async fn final_metadata_ack_is_durable_non_fencing_and_bound_to_its_original_namespace() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let coordinator = Coordinator::default();
        coordinator.set_recovery_home(Arc::new(home.path().into()));
        let namespace = Namespace::open(home.path()).unwrap();
        let (folder, lease, permit, worker, mut recovery) =
            prepared(&namespace, &coordinator, root.path(), mode, 1).await;
        let outcome = CommandOutcome::Rejected {
            message: "Original command never dispatched 文😀".into(),
        };
        namespace
            .command_outcome(&folder, &lease, &permit, &worker, &outcome, true)
            .unwrap();
        recovery.command.phase = ActionPhase::Acknowledged;
        recovery.command.outcome_digest = Some(openwebide_core::plugins::content_digest(
            &serde_json::to_vec(&outcome).unwrap(),
        ));
        let mut inactive = worker.state();
        inactive.active = false;
        recovery.command.worker = Some(inactive);
        recovery.command.command_cleanup_confirmed = true;
        recovery.command.native_settlement_pending = false;
        recovery.activity.language_command = false;
        recovery.validate_settlement().unwrap();
        namespace
            .command_settled(&folder, &lease, &permit, &recovery)
            .unwrap();
        drop(lease);
        drop(namespace);
        let namespace = Namespace::open(home.path()).unwrap();
        assert!(namespace.pending_commands().unwrap().is_empty());
        let pending = namespace.pending_command_settlements().unwrap().remove(0);
        assert_eq!(pending.recovery, recovery);
        assert!(namespace.has_command_settlement(&pending).unwrap());
        drop(coordinator.acquire(folder).await.unwrap());
        // A later command's process receipt must not downgrade the namespace
        // and erase or make the first command's pending ACK unreadable.
        let other_root = tempfile::tempdir().unwrap();
        let (other_folder, other_lease, other_permit, other_worker, other_recovery) =
            prepared(&namespace, &coordinator, other_root.path(), mode, 10).await;
        namespace
            .command_admitted(&other_folder, &other_lease, &other_permit, &other_worker)
            .unwrap();
        namespace
            .command_process_cleanup(&other_folder, &other_lease, &other_permit, &other_worker)
            .unwrap();
        assert_eq!(namespace.pending_command_settlements().unwrap().len(), 1);
        let before = std::fs::read(home.path().join(DIRECTORY).join(DATABASE)).unwrap();
        let mut forged = pending.clone();
        forged.worker.secret = "f".repeat(32);
        assert!(namespace.acknowledged_command_settlement(&forged).is_err());
        assert_eq!(
            std::fs::read(home.path().join(DIRECTORY).join(DATABASE)).unwrap(),
            before
        );
        let wrong_home = tempfile::tempdir().unwrap();
        let wrong_namespace = Namespace::open(wrong_home.path()).unwrap();
        assert!(
            wrong_namespace
                .acknowledged_command_settlement(&pending)
                .is_err()
        );
        // Metadata disposal needs no access to the retired workspace path.
        let parked = root.path().with_extension("parked");
        std::fs::rename(root.path(), &parked).unwrap();
        namespace.acknowledged_command_settlement(&pending).unwrap();
        namespace.acknowledged_command_settlement(&pending).unwrap();
        std::fs::rename(&parked, root.path()).unwrap();
        assert!(namespace.pending_command_settlements().unwrap().is_empty());
        assert!(
            namespace
                .pending_command(7, 11, mode, &other_recovery.command.invocation)
                .unwrap()
                .is_some()
        );
        drop(other_lease);
        assert!(coordinator.acquire(other_folder).await.is_err());
    }
}

#[tokio::test]
async fn pending_metadata_reserves_capacity_before_new_native_command_ownership() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let home = tempfile::tempdir().unwrap();
        let coordinator = Coordinator::default();
        coordinator.set_recovery_home(Arc::new(home.path().into()));
        let namespace = Namespace::open(home.path()).unwrap();
        for index in 0..32 {
            let root = tempfile::tempdir().unwrap();
            let (folder, lease, permit, worker, mut recovery) =
                prepared(&namespace, &coordinator, root.path(), mode, index).await;
            let outcome = CommandOutcome::Rejected {
                message: "No dispatch occurred".into(),
            };
            namespace
                .command_outcome(&folder, &lease, &permit, &worker, &outcome, true)
                .unwrap();
            recovery.command.phase = ActionPhase::Acknowledged;
            recovery.command.outcome_digest = Some(openwebide_core::plugins::content_digest(
                &serde_json::to_vec(&outcome).unwrap(),
            ));
            let mut inactive = worker.state();
            inactive.active = false;
            recovery.command.worker = Some(inactive);
            recovery.command.command_cleanup_confirmed = true;
            recovery.command.native_settlement_pending = false;
            recovery.activity.language_command = false;
            namespace
                .command_settled(&folder, &lease, &permit, &recovery)
                .unwrap();
        }
        assert!(namespace.pending_commands().unwrap().is_empty());
        assert_eq!(namespace.pending_command_settlements().unwrap().len(), 32);
        let root = tempfile::tempdir().unwrap();
        let folder = Folder::capture(root.path()).unwrap();
        let mut recovery = original();
        recovery.scope.id = "f".repeat(32);
        recovery
            .command
            .invocation
            .id
            .clone_from(&recovery.scope.id);
        recovery.scope.root.mode = mode;
        recovery.scope.root.path = (mode == WorkspaceMode::Remote).then(|| "project".into());
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
        assert!(
            namespace
                .reserve_command(
                    &folder,
                    &lease,
                    &permit,
                    CommandIntent {
                        recovery: &recovery,
                        worker: &worker
                    }
                )
                .is_err()
        );
        assert!(namespace.pending_commands().unwrap().is_empty());
        drop(lease);
        // Failed reservation must leave no owner or folder fence behind.
        drop(coordinator.acquire(folder.clone()).await.unwrap());
        let pending = namespace.pending_command_settlements().unwrap().remove(0);
        namespace.acknowledged_command_settlement(&pending).unwrap();
        let lease = coordinator
            .acquire_recovery(folder.clone(), permit.clone())
            .await
            .unwrap();
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
        assert_eq!(namespace.pending_commands().unwrap().len(), 1);
        assert_eq!(namespace.pending_command_settlements().unwrap().len(), 31);
    }
}
