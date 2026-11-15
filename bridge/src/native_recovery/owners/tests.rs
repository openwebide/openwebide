use super::*;
use crate::folder_coordination::Coordinator;
use openwebide_core::{WorkspaceMode, editor::EditorRecoveryRoot};

fn scope() -> WorkspaceTransactionScope {
    WorkspaceTransactionScope {
        id: "1".repeat(32),
        generation: "2".repeat(32),
        revision: 1,
        root_identity: "3".repeat(64),
        root: EditorRecoveryRoot {
            mode: WorkspaceMode::Remote,
            path: Some("project".into()),
        },
        worker: None,
        accepted_language_review_digest: None,
    }
}
#[tokio::test]
async fn pending_original_owner_excludes_new_coordinators_aliases_accounts_and_replacement_paths() {
    let home = tempfile::Builder::new()
        .prefix("openwebide-owner-home-")
        .tempdir()
        .unwrap()
        .keep();
    let parent = tempfile::Builder::new()
        .prefix("openwebide-owner-folders-")
        .tempdir()
        .unwrap()
        .keep();
    let path = parent.join("project");
    std::fs::create_dir(&path).unwrap();
    std::fs::create_dir(path.join("child")).unwrap();
    std::fs::create_dir(parent.join("independent")).unwrap();
    let folder = Folder::capture(&path).unwrap();
    let namespace = Namespace::open(&home).unwrap();
    let scope = scope();
    let authority = RecoveryAuthority::for_scope(7, 11, &scope, "A".repeat(43)).unwrap();
    let activity = WorkspaceActivity {
        file_recovery: true,
        language_command: false,
    };
    let permit = namespace
        .permit(&folder, &authority, &scope, activity)
        .unwrap();
    let coordinator = Coordinator::default();
    coordinator.set_recovery_home(std::sync::Arc::new(home.clone()));
    let lease = coordinator
        .acquire_recovery(folder.clone(), permit.clone())
        .await
        .unwrap();
    namespace.enroll(&folder, &lease, &permit).unwrap();
    drop(lease);
    drop(coordinator);
    drop(namespace);
    let next = Coordinator::default();
    next.set_recovery_home(std::sync::Arc::new(home.clone()));
    std::os::unix::fs::symlink(&path, parent.join("alias")).unwrap();
    for selected in [&path, &path.join("child"), &parent, &parent.join("alias")] {
        assert!(
            next.acquire(Folder::capture(selected).unwrap())
                .await
                .is_err()
        );
        assert!(
            next.acquire_ordinary(Folder::capture(selected).unwrap())
                .await
                .is_err()
        );
    }
    drop(
        next.acquire(Folder::capture(&parent.join("independent")).unwrap())
            .await
            .unwrap(),
    );
    let namespace = Namespace::open(&home).unwrap();
    let mut changed_scope = scope.clone();
    changed_scope.generation = "9".repeat(32);
    for (user, project, original, secret) in [
        (7, 11, &scope, "Q".repeat(43)),
        (8, 11, &scope, "A".repeat(43)),
        (7, 12, &scope, "A".repeat(43)),
        (7, 11, &changed_scope, "A".repeat(43)),
    ] {
        let other = RecoveryAuthority::for_scope(user, project, original, secret).unwrap();
        let wrong = namespace
            .permit(&folder, &other, original, activity)
            .unwrap();
        assert!(next.acquire_recovery(folder.clone(), wrong).await.is_err());
    }
    let mut progressed = scope.clone();
    progressed.revision = 99;
    progressed.worker = Some(
        openwebide_core::workspace_transaction::WorkspaceTransactionWorkerState {
            id: "8".repeat(32),
            active: false,
        },
    );
    let still_original = namespace
        .permit(&folder, &authority, &progressed, activity)
        .unwrap();
    drop(
        next.acquire_recovery(folder.clone(), still_original)
            .await
            .unwrap(),
    );
    std::fs::rename(&path, parent.join("original")).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(next.acquire(Folder::capture(&path).unwrap()).await.is_err());
    assert!(
        next.acquire(Folder::capture(&parent.join("original")).unwrap())
            .await
            .is_err()
    );
    std::fs::rename(&path, parent.join("retained-replacement")).unwrap();
    std::fs::rename(parent.join("original"), &path).unwrap();
    let lease = next
        .acquire_recovery(folder.clone(), permit.clone())
        .await
        .unwrap();
    namespace
        .finish_files(&folder, &lease, &permit, WorkspaceActivity::default())
        .unwrap();
    drop(lease);
    drop(next.acquire(folder).await.unwrap());
    println!(
        "retained_owner_fixture={} folders={}",
        home.display(),
        parent.display()
    );
}
#[tokio::test]
async fn file_completion_does_not_release_a_compound_command_owner() {
    let home = tempfile::Builder::new()
        .prefix("openwebide-command-owner-")
        .tempdir()
        .unwrap()
        .keep();
    let root = tempfile::Builder::new()
        .prefix("openwebide-command-folder-")
        .tempdir()
        .unwrap()
        .keep();
    let folder = Folder::capture(&root).unwrap();
    let ns = Namespace::open(&home).unwrap();
    let scope = scope();
    let authority = RecoveryAuthority::for_scope(7, 11, &scope, "A".repeat(43)).unwrap();
    let permit = ns
        .permit(
            &folder,
            &authority,
            &scope,
            WorkspaceActivity {
                file_recovery: true,
                language_command: true,
            },
        )
        .unwrap();
    let coordinator = Coordinator::default();
    coordinator.set_recovery_home(std::sync::Arc::new(home.clone()));
    let lease = coordinator
        .acquire_recovery(folder.clone(), permit.clone())
        .await
        .unwrap();
    ns.enroll(&folder, &lease, &permit).unwrap();
    ns.finish_files(&folder, &lease, &permit, WorkspaceActivity::default())
        .unwrap();
    drop(lease);
    assert!(coordinator.acquire(folder).await.is_err());
    println!("retained_compound_owner_fixture={}", home.display());
}

#[tokio::test]
async fn a_new_process_cannot_write_through_an_ordinary_vfs_after_the_owner_exits() {
    use openwebide_core::Vfs;
    if let Some(home) = std::env::var_os("OPENWEBIDE_TEST_DURABLE_OWNER_HOME") {
        let root = PathBuf::from(std::env::var_os("OPENWEBIDE_TEST_DURABLE_OWNER_ROOT").unwrap());
        let home = PathBuf::from(home);
        Namespace::open(&home).unwrap().ensure_current().unwrap();
        let coordinator = Coordinator::default();
        coordinator.set_recovery_home(std::sync::Arc::new(home));
        let files =
            crate::runs::native_vfs::workspace::WorkspaceFiles::new(root.clone(), coordinator);
        let error = files
            .write("unwanted.txt", "must not be written")
            .await
            .unwrap_err();
        assert!(error.to_string().contains(PENDING));
        assert!(!root.join("unwanted.txt").exists());
        return;
    }
    let home = tempfile::Builder::new()
        .prefix("openwebide-owner-process-state-")
        .tempdir()
        .unwrap()
        .keep();
    let root = tempfile::Builder::new()
        .prefix("openwebide-owner-process-folder-")
        .tempdir()
        .unwrap()
        .keep();
    let ns = Namespace::open(&home).unwrap();
    let folder = Folder::capture(&root).unwrap();
    let scope = scope();
    let authority = RecoveryAuthority::for_scope(7, 11, &scope, "A".repeat(43)).unwrap();
    let permit = ns
        .permit(
            &folder,
            &authority,
            &scope,
            WorkspaceActivity {
                file_recovery: true,
                language_command: false,
            },
        )
        .unwrap();
    let coordinator = Coordinator::default();
    coordinator.set_recovery_home(std::sync::Arc::new(home.clone()));
    let lease = coordinator
        .acquire_recovery(folder.clone(), permit)
        .await
        .unwrap();
    ns.enroll(&folder, &lease, lease.recovery().unwrap())
        .unwrap();
    drop(lease);
    drop(coordinator);
    drop(folder);
    drop(ns);
    let physical = File::open(&root).unwrap();
    physical.try_lock().unwrap();
    physical.unlock().unwrap();
    drop(physical);
    let output=tokio::time::timeout(std::time::Duration::from_secs(10),tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact","native_recovery::owners::tests::a_new_process_cannot_write_through_an_ordinary_vfs_after_the_owner_exits","--nocapture"])
        .env("OPENWEBIDE_TEST_DURABLE_OWNER_HOME",&home).env("OPENWEBIDE_TEST_DURABLE_OWNER_ROOT",&root).kill_on_drop(true).output()).await.unwrap().unwrap();
    std::fs::write(home.join("child.stdout"), &output.stdout).unwrap();
    std::fs::write(home.join("child.stderr"), &output.stderr).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!root.join("unwanted.txt").exists());
    println!(
        "retained_process_owner_fixture={} folder={}",
        home.display(),
        root.display()
    );
}

#[tokio::test]
async fn proving_a_pending_folder_never_grants_mutation_or_process_access() {
    let home = tempfile::Builder::new()
        .prefix("openwebide-owner-proof-state-")
        .tempdir()
        .unwrap()
        .keep();
    let root = tempfile::Builder::new()
        .prefix("openwebide-owner-proof-folder-")
        .tempdir()
        .unwrap()
        .keep();
    let namespace = Namespace::open(&home).unwrap();
    let folder = Folder::capture(&root).unwrap();
    let scope = scope();
    let authority = RecoveryAuthority::for_scope(7, 11, &scope, "A".repeat(43)).unwrap();
    let permit = namespace
        .permit(
            &folder,
            &authority,
            &scope,
            WorkspaceActivity {
                file_recovery: true,
                language_command: false,
            },
        )
        .unwrap();
    let coordinator = Coordinator::default();
    coordinator.set_recovery_home(std::sync::Arc::new(home.clone()));
    let lease = coordinator
        .acquire_recovery(folder.clone(), permit.clone())
        .await
        .unwrap();
    namespace.enroll(&folder, &lease, &permit).unwrap();
    drop(lease);
    let proof = std::sync::Arc::new(coordinator.acquire_proof(folder.clone()).await.unwrap());
    assert!(proof.proof_covers(&folder));
    assert!(!proof.covers(&folder));
    assert!(proof.command_descriptors(&root).is_err());
    assert!(crate::runs::native_vfs::workspace::WorkspaceFiles::with_lease(folder, proof).is_err());
    println!("retained_proof_owner_fixture={}", home.display());
}

#[cfg(target_os = "linux")]
#[test]
fn packaged_owner_survives_an_outer_runtime_recreation() {
    let Ok(stage) = std::env::var("OPENWEBIDE_TEST_PACKAGED_OWNER_STAGE") else {
        return;
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        use openwebide_core::Vfs;
        let home = home().unwrap();
        assert_eq!(home, Path::new("/app/.spin/native-principal"));
        let path = Path::new("/workspace/project");
        if stage == "initialize" {
            std::fs::create_dir(path).unwrap();
        }
        let folder = Folder::capture(path).unwrap();
        let namespace = Namespace::open(&home).unwrap();
        let scope = scope();
        let authority = RecoveryAuthority::for_scope(7, 11, &scope, "A".repeat(43)).unwrap();
        let permit = namespace
            .permit(
                &folder,
                &authority,
                &scope,
                WorkspaceActivity {
                    file_recovery: true,
                    language_command: false,
                },
            )
            .unwrap();
        let coordinator = Coordinator::default();
        coordinator.set_recovery_home(std::sync::Arc::new(home));
        match stage.as_str() {
            "initialize" | "recover" => {
                let lease = coordinator
                    .acquire_recovery(folder.clone(), permit.clone())
                    .await
                    .unwrap();
                namespace.enroll(&folder, &lease, &permit).unwrap();
                drop(lease);
                assert!(
                    namespace
                        .admit(&folder, None)
                        .unwrap_err()
                        .contains(PENDING)
                );
            }
            "ordinary" => {
                let files = crate::runs::native_vfs::workspace::WorkspaceFiles::new(
                    path.into(),
                    coordinator,
                );
                let error = files.write("unwanted.txt", "no").await.unwrap_err();
                assert!(error.to_string().contains(PENDING));
            }
            "resolve" => {
                let lease = coordinator
                    .acquire_recovery(folder.clone(), permit.clone())
                    .await
                    .unwrap();
                namespace
                    .finish_files(&folder, &lease, &permit, WorkspaceActivity::default())
                    .unwrap();
                drop(lease);
                let files = crate::runs::native_vfs::workspace::WorkspaceFiles::new(
                    path.into(),
                    coordinator,
                );
                files
                    .write("resolved.txt", "original owner resolved")
                    .await
                    .unwrap();
                assert_eq!(
                    std::fs::read(path.join("resolved.txt")).unwrap(),
                    b"original owner resolved"
                );
            }
            _ => panic!("Unknown packaged owner fixture stage"),
        }
        assert!(!path.join("unwanted.txt").exists());
        println!("packaged_owner_stage={stage}");
        println!(
            "packaged_owner_folder={}",
            serde_json::to_string(&OriginalFolder::capture(&folder, true).unwrap()).unwrap()
        );
    });
}
