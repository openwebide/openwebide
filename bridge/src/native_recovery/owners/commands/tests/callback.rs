use super::*;
#[tokio::test]
async fn verified_callback_changes_preserve_native_ownership_and_reject_stale_or_forged_proofs() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let coordinator = Coordinator::default();
        coordinator.set_recovery_home(Arc::new(home.path().into()));
        let namespace = Namespace::open(home.path()).unwrap();
        let folder = Folder::capture(root.path()).unwrap();
        let mut current = original();
        current.scope.root.mode = mode;
        current.scope.root.path = (mode == WorkspaceMode::Remote).then(|| "project".into());
        current.command.phase = ActionPhase::Approved;
        current.activity.file_recovery = true;
        let authority =
            RecoveryAuthority::for_scope(7, 11, &current.scope, "A".repeat(43)).unwrap();
        let permit = namespace
            .permit(&folder, &authority, &current.scope, current.activity)
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
                    recovery: &current,
                    worker: &worker,
                },
            )
            .unwrap();
        drop(lease);
        let pending = namespace.pending_commands().unwrap().remove(0);
        let database = home.path().join(DIRECTORY).join(DATABASE);
        let before = std::fs::read(&database).unwrap();
        for field in ["authority", "worker", "invocation", "origin"] {
            let mut forged = namespace.pending_commands().unwrap().remove(0);
            match field {
                "authority" => forged.authority.secret = "w".repeat(43),
                "worker" => forged.worker.secret = "f".repeat(32),
                "invocation" => forged.invocation.client = "different window".into(),
                _ => forged.origin = "http://127.0.0.1:9000/api".into(),
            }
            assert!(
                namespace
                    .reconnect_command_callback(&forged, &current, "http://127.0.0.1:3001/api")
                    .is_err(),
                "{field}"
            );
            assert_eq!(std::fs::read(&database).unwrap(), before);
        }
        let wrong = tempfile::tempdir().unwrap();
        let other = Namespace::open(wrong.path()).unwrap();
        assert!(
            other
                .reconnect_command_callback(&pending, &current, "http://127.0.0.1:3001/api")
                .is_err()
        );
        let mut changed = current.clone();
        changed.scope.generation = "f".repeat(32);
        assert!(
            namespace
                .reconnect_command_callback(&pending, &changed, "http://127.0.0.1:3001/api")
                .is_err()
        );
        let parked = root.path().with_extension("parked");
        std::fs::rename(root.path(), &parked).unwrap();
        namespace
            .reconnect_command_callback(&pending, &current, "http://127.0.0.1:3001/api")
            .unwrap();
        drop(namespace);
        let namespace = Namespace::open(home.path()).unwrap();
        assert_eq!(
            namespace
                .command_callback(&authority, &worker)
                .unwrap()
                .as_deref(),
            Some("http://127.0.0.1:3001/api")
        );
        let next = namespace.pending_commands().unwrap().remove(0);
        assert_eq!(next.origin, "http://127.0.0.1:3001/api");
        assert_eq!(next.invocation, pending.invocation);
        assert_eq!(next.outcome, pending.outcome);
        assert_eq!(next.cleaned, pending.cleaned);
        assert_eq!(next.can_cancel, pending.can_cancel);
        assert_eq!(next.can_reject, pending.can_reject);
        assert!(next.reopen().is_err());
        assert!(
            namespace
                .reconnect_command_callback(&pending, &current, "http://127.0.0.1:3002/api")
                .is_err()
        );
        std::fs::rename(&parked, root.path()).unwrap();
        let lease = coordinator
            .acquire_recovery(folder.clone(), permit.clone())
            .await
            .unwrap();
        namespace
            .enroll(&folder, &lease, &permit)
            .expect("Reconnecting command recovery must preserve the original file journal");
        assert_eq!(
            namespace
                .command_callback(&authority, &worker)
                .unwrap()
                .as_deref(),
            Some("http://127.0.0.1:3001/api"),
            "File recovery changed the verified command callback"
        );
        drop(lease);
        assert!(coordinator.acquire(folder).await.is_err());
    }
}
