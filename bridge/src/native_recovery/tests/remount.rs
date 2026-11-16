//! Run in separate processes on either side of an actual disposable APFS remount.
//! This checks native ownership and resource primitives, not browser permissions.
use super::*;
use crate::{
    folder_coordination::Coordinator, runs::native_vfs::resources::ResourceFiles,
    workspace_transactions::Host,
};
use openwebide_core::{
    WorkspaceMode,
    editor::EditorRecoveryRoot,
    vfs::VfsEntryKind,
    workspace_transaction::{
        RecoveryAuthority, ResourceLocation, ResourceScope, TransactionFiles, WorkspaceActivity,
        WorkspaceTransactionScope,
    },
};
use std::sync::Arc;

#[derive(Serialize, Deserialize)]
struct Fixture {
    scope: WorkspaceTransactionScope,
    authority: RecoveryAuthority,
    resource: ResourceScope,
    original: Identity,
    device: u64,
}

#[tokio::test]
#[ignore = "requires an owned APFS image remounted between prepare and recover processes"]
async fn pending_native_resources_survive_an_actual_volume_remount() {
    let base = PathBuf::from(std::env::var_os("OPENWEBIDE_REMOUNT_FIXTURE").unwrap());
    let token = std::env::var("OPENWEBIDE_REMOUNT_TOKEN").unwrap();
    assert!(base.is_absolute());
    assert!(token.len() >= 32);
    assert_eq!(
        std::fs::read_to_string(base.join(".openwebide-remount-fixture")).unwrap(),
        token
    );
    let phase = std::env::var("OPENWEBIDE_REMOUNT_PHASE").unwrap();
    assert!(matches!(phase.as_str(), "prepare" | "recover"));
    for (name, mode) in [
        ("remote", WorkspaceMode::Remote),
        ("local", WorkspaceMode::Local),
    ] {
        let case = base.join(name);
        let home = case.join("native-home");
        let path = case.join("project");
        let state = case.join("fixture.json");
        if phase == "prepare" {
            std::fs::create_dir(&case).unwrap();
            std::fs::create_dir(&home).unwrap();
            std::fs::create_dir(&path).unwrap();
            std::fs::create_dir(path.join("child")).unwrap();
            std::fs::write(path.join("original.txt"), "original 文😀\n").unwrap();
        }
        let folder = Folder::capture(&path).unwrap();
        let namespace = Namespace::open(&home).unwrap();
        namespace.ensure_current().unwrap();
        let coordinator = Coordinator::default();
        coordinator.set_recovery_home(Arc::new(home.clone()));
        let identity = Host::root_identity(&case, "project", 1, 1).unwrap();
        let fixture = if phase == "prepare" {
            let scope = WorkspaceTransactionScope {
                id: "1".repeat(32),
                generation: "2".repeat(32),
                revision: 1,
                root_identity: identity.clone(),
                root: EditorRecoveryRoot {
                    mode,
                    path: (mode == WorkspaceMode::Remote).then(|| "project".into()),
                },
                worker: None,
                accepted_language_review_digest: None,
            };
            Fixture {
                authority: RecoveryAuthority::for_scope(1, 1, &scope, "A".repeat(43)).unwrap(),
                scope,
                resource: ResourceScope {
                    transaction: "1".repeat(32),
                    fingerprint: "3".repeat(64),
                },
                original: Identity::of(&File::open(path.join("original.txt")).unwrap()).unwrap(),
                device: std::fs::metadata(&path).unwrap().dev(),
            }
        } else {
            let fixture: Fixture = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
            assert_ne!(fixture.device, std::fs::metadata(&path).unwrap().dev());
            assert_eq!(fixture.scope.root_identity, identity);
            assert!(coordinator.acquire_ordinary(folder.clone()).await.is_err());
            assert!(
                coordinator
                    .acquire(Folder::capture(&path.join("child")).unwrap())
                    .await
                    .is_err()
            );
            let alias = case.join("alias");
            std::os::unix::fs::symlink(&path, &alias).unwrap();
            assert!(
                coordinator
                    .acquire(Folder::capture(&alias).unwrap())
                    .await
                    .is_err()
            );
            let foreign =
                RecoveryAuthority::for_scope(2, 1, &fixture.scope, "Q".repeat(43)).unwrap();
            let permit = namespace
                .permit(
                    &folder,
                    &foreign,
                    &fixture.scope,
                    WorkspaceActivity {
                        file_recovery: true,
                        language_command: false,
                    },
                )
                .unwrap();
            assert!(
                coordinator
                    .acquire_recovery(folder.clone(), permit)
                    .await
                    .is_err()
            );
            fixture
        };
        let permit = namespace
            .permit(
                &folder,
                &fixture.authority,
                &fixture.scope,
                WorkspaceActivity {
                    file_recovery: true,
                    language_command: false,
                },
            )
            .unwrap();
        let lease = coordinator
            .acquire_recovery(folder.clone(), permit.clone())
            .await
            .unwrap();
        namespace.enroll(&folder, &lease, &permit).unwrap();
        namespace.files_admitted(&folder, &lease, &permit).unwrap();
        let files =
            ResourceFiles::new(folder.handle.as_file(), &identity, fixture.resource.clone())
                .unwrap();
        let source = ResourceLocation::Workspace("original.txt".into());
        let retained = ResourceLocation::Retained(0);
        if phase == "prepare" {
            files
                .transfer_resource(&fixture.resource, &source, &retained, VfsEntryKind::File)
                .await
                .unwrap();
            files
                .create_resource(&fixture.resource, 1, "created.txt", VfsEntryKind::File)
                .await
                .unwrap();
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&state)
                .unwrap();
            output
                .write_all(&serde_json::to_vec(&fixture).unwrap())
                .unwrap();
            output.sync_all().unwrap();
            assert!(!path.join("original.txt").exists());
        } else {
            assert_eq!(
                files
                    .retained_read_bytes(&fixture.resource, "0")
                    .await
                    .unwrap(),
                "original 文😀\n".as_bytes()
            );
            assert!(
                files
                    .created_resource_matches(
                        &fixture.resource,
                        1,
                        "created.txt",
                        VfsEntryKind::File
                    )
                    .await
                    .unwrap()
            );
            files
                .remove_created_resource(&fixture.resource, 1, "created.txt", VfsEntryKind::File)
                .await
                .unwrap();
            files
                .transfer_resource(&fixture.resource, &retained, &source, VfsEntryKind::File)
                .await
                .unwrap();
            assert!(
                fixture
                    .original
                    .matches(&File::open(path.join("original.txt")).unwrap())
                    .unwrap()
            );
            files.finish_retained(&fixture.resource).await.unwrap();
            namespace
                .finish_files(&folder, &lease, &permit, WorkspaceActivity::default())
                .unwrap();
            drop(lease);
            drop(coordinator.acquire_ordinary(folder).await.unwrap());
            assert!(!path.join("created.txt").exists());
        }
        println!("native_remount_case={name} phase={phase}");
    }
}
