use super::*;

#[tokio::test]
async fn native_cancellation_requires_positive_original_file_and_dispatch_proof_without_an_outcome()
{
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for boundary in [
            "unadmitted",
            "finished",
            "admitted",
            "unknown",
            "dispatched",
        ] {
            let home = tempfile::tempdir().unwrap();
            let root = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join("unchanged"), "original").unwrap();
            let coordinator = Coordinator::default();
            coordinator.set_recovery_home(Arc::new(home.path().into()));
            let namespace = Namespace::open(home.path()).unwrap();
            let folder = Folder::capture(root.path()).unwrap();
            let mut recovery = original();
            recovery.scope.root.mode = mode;
            recovery.scope.root.path = (mode == WorkspaceMode::Remote).then(|| "project".into());
            recovery.command.phase = ActionPhase::Approved;
            recovery.activity.file_recovery = true;
            let authority =
                RecoveryAuthority::for_scope(7, 11, &recovery.scope, "A".repeat(43)).unwrap();
            let permit = namespace
                .permit(&folder, &authority, &recovery.scope, recovery.activity)
                .unwrap()
                .with_origin(Some("http://localhost/api"))
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
            recovery.activity.file_recovery = false;
            if boundary != "unadmitted" {
                recovery.command.worker = Some(worker.state());
                recovery.command.native_settlement_pending = true;
            }
            if matches!(boundary, "finished" | "admitted") {
                namespace.files_admitted(&folder, &lease, &permit).unwrap();
                if boundary == "finished" {
                    namespace
                        .files_finished(&folder, &lease, &permit, recovery.activity)
                        .unwrap();
                }
            }
            if boundary == "unknown" {
                namespace
                    .access(true, |parent, anchor, db| {
                        let mut owner = super::super::super::read(db, version(db)?)?.remove(0);
                        owner.files = FileStage::Unknown;
                        super::super::super::context::save_owner(db, &owner)?;
                        namespace.save(parent, anchor, db)
                    })
                    .unwrap();
            }
            if boundary == "dispatched" {
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
                namespace
                    .command_admitted(&folder, &lease, &permit, &worker)
                    .unwrap();
                namespace
                    .command_dispatch(&folder, &lease, &permit, &worker)
                    .unwrap();
            }
            recovery.command.phase = ActionPhase::Cancelled;
            recovery.activity.language_command = recovery.command.native_settlement_pending;
            recovery.validate().unwrap();
            let accepted = matches!(boundary, "unadmitted" | "finished");
            assert!(
                namespace
                    .command_settled(&folder, &lease, &permit, &recovery)
                    .is_err()
            );
            assert_eq!(
                namespace
                    .command_cancelled(&folder, &lease, &permit, &worker, &recovery)
                    .is_ok(),
                accepted,
                "{mode:?}/{boundary}"
            );
            if accepted {
                drop(namespace);
                let namespace = Namespace::open(home.path()).unwrap();
                let pending = namespace
                    .pending_command(7, 11, mode, &recovery.command.invocation)
                    .unwrap()
                    .unwrap();
                assert!(pending.cancelled && pending.cleaned && pending.outcome.is_none());
                assert!(pending.present(&namespace).unwrap());
                namespace
                    .access(false, |_, _, db| {
                        assert!(read(db, 6).is_err());
                        Ok(())
                    })
                    .unwrap();
                namespace
                    .command_cancelled(&folder, &lease, &permit, &worker, &recovery)
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
                for outcome in [
                    CommandOutcome::Rejected {
                        message: "no result".into(),
                    },
                    CommandOutcome::Uncertain {
                        message: "no result".into(),
                    },
                    CommandOutcome::Completed {
                        result: serde_json::Value::Null,
                    },
                ] {
                    assert!(
                        namespace
                            .command_outcome(&folder, &lease, &permit, &worker, &outcome, true)
                            .is_err()
                    );
                }
                recovery.command.worker = Some(
                    openwebide_core::workspace_transaction::WorkspaceTransactionWorkerState {
                        id: worker.id.clone(),
                        active: false,
                    },
                );
                recovery.command.native_settlement_pending = false;
                recovery.command.command_cleanup_confirmed = true;
                recovery.activity.language_command = false;
                namespace
                    .command_settled(&folder, &lease, &permit, &recovery)
                    .unwrap();
                assert!(!pending.present(&namespace).unwrap());
                drop(lease);
                assert!(coordinator.acquire(folder).await.is_ok());
            } else {
                drop(lease);
                assert!(coordinator.acquire(folder).await.is_err());
            }
            assert_eq!(
                std::fs::read_to_string(root.path().join("unchanged")).unwrap(),
                "original"
            );
        }
    }
}
