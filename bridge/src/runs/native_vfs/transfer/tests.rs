use crate::runs::native_vfs::NativeFsVfs;
use openwebide_core::{
    Vfs, VfsError,
    vfs::VfsEntryKind,
    workspace_transaction::{
        self as transaction, FileOperation, ResourceLocation, ResourceScript, ResourceStep,
        VfsTransactionFiles,
    },
};
use std::{path::Path, sync::Arc};

mod attributes;

#[tokio::test]
async fn object_move_and_existing_transaction_primitives_forward_through_vfs_wrappers() {
    let directory = tempfile::tempdir().unwrap();
    let vfs = Arc::new(NativeFsVfs {
        root: directory.path().canonicalize().unwrap(),
    });
    openwebide_core::testing::workspace_resource_transfer_contract(vfs.as_ref()).await;
    openwebide_core::testing::workspace_resource_transfer_contract(&vfs).await;
    let reference = vfs.as_ref();
    openwebide_core::testing::workspace_resource_transfer_contract(&reference).await;
    openwebide_core::testing::workspace_transaction_primitives_contract(&vfs).await;
    openwebide_core::testing::workspace_transaction_primitives_contract(&reference).await;
}

#[tokio::test]
async fn source_and_destination_links_are_never_followed_or_overwritten() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let vfs = NativeFsVfs {
        root: directory.path().canonicalize().unwrap(),
    };
    vfs.write("source", "keep source").await.unwrap();
    vfs.write("real/file", "keep target").await.unwrap();
    std::fs::write(outside.path().join("file"), "outside").unwrap();
    symlink("source", directory.path().join("source-link")).unwrap();
    symlink("real", directory.path().join("parent-link")).unwrap();
    symlink(outside.path(), directory.path().join("outside-link")).unwrap();
    symlink("absent", directory.path().join("dangling-link")).unwrap();
    for (from, to) in [
        ("source-link", "moved"),
        ("parent-link/file", "moved"),
        ("outside-link/file", "moved"),
        ("source", "parent-link/new"),
        ("source", "outside-link/new"),
        ("source", "dangling-link"),
    ] {
        assert!(
            vfs.move_atomic(from, to, VfsEntryKind::File).await.is_err(),
            "{from} -> {to}"
        );
    }
    assert_eq!(vfs.read("source").await.unwrap(), "keep source");
    assert_eq!(vfs.read("real/file").await.unwrap(), "keep target");
    assert_eq!(
        std::fs::read_to_string(outside.path().join("file")).unwrap(),
        "outside"
    );
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
    assert!(!directory.path().join("moved").exists());
    assert!(!directory.path().join("real/new").exists());
    assert!(
        std::fs::symlink_metadata(directory.path().join("dangling-link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(matches!(
        vfs.move_atomic("source", "dangling-link", VfsEntryKind::File)
            .await,
        Err(VfsError::AlreadyExists(_))
    ));
    std::fs::hard_link(
        directory.path().join("source"),
        directory.path().join("same-object"),
    )
    .unwrap();
    let handle = original(&directory.path().join("source"));
    assert!(matches!(
        vfs.move_atomic("source", "same-object", VfsEntryKind::File)
            .await,
        Err(VfsError::AlreadyExists(_))
    ));
    assert_eq!(original(&directory.path().join("source")), handle);
    assert_eq!(original(&directory.path().join("same-object")), handle);
}

fn original(path: &Path) -> same_file::Handle {
    same_file::Handle::from_path(path).unwrap()
}
fn location(location: &ResourceLocation) -> String {
    match location {
        ResourceLocation::Workspace(path) => path.clone(),
        ResourceLocation::Retained(slot) => format!("held/{slot}"),
    }
}
async fn step(vfs: &NativeFsVfs, step: &ResourceStep) {
    match step {
        ResourceStep::Create { path, kind } => vfs.create(path, *kind).await.unwrap(),
        ResourceStep::Remove { path, kind } => vfs.remove_entry(path, *kind).await.unwrap(),
        ResourceStep::Transfer { from, to, kind } => vfs
            .move_atomic(&location(from), &location(to), *kind)
            .await
            .unwrap(),
    }
}

#[tokio::test]
async fn every_resource_script_prefix_restores_the_original_files_and_directories() {
    let operations = vec![
        FileOperation::Rename {
            from: "source".into(),
            to: "target".into(),
            overwrite: true,
            ignore_if_exists: false,
        },
        FileOperation::Create {
            path: "target/new/empty".into(),
            overwrite: false,
            ignore_if_exists: false,
        },
        FileOperation::Delete {
            path: "old".into(),
            recursive: false,
            ignore_if_missing: false,
        },
        FileOperation::Create {
            path: "target/file".into(),
            overwrite: true,
            ignore_if_exists: false,
        },
    ];
    // Seven primitives: hold target, move source, create directory/file, hold old,
    // then hold/recreate target/file. Every interruption boundary must invert.
    for completed in 0..=7 {
        let directory = tempfile::tempdir().unwrap();
        let vfs = NativeFsVfs {
            root: directory.path().canonicalize().unwrap(),
        };
        vfs.write_bytes("source/file", &[0, 255, 1]).await.unwrap();
        vfs.write("target/prior", "overwritten directory")
            .await
            .unwrap();
        vfs.write("old", "deleted file").await.unwrap();
        vfs.create("held", VfsEntryKind::Directory).await.unwrap();
        let paths = ["source", "source/file", "target", "target/prior", "old"];
        let originals = paths.map(|path| original(&directory.path().join(path)));
        let files = VfsTransactionFiles {
            vfs: &vfs,
            identity: "owned-root",
        };
        let plan = transaction::prepare(&files, files.identity.into(), operations.clone(), || true)
            .await
            .unwrap();
        let script = ResourceScript::prepare(&plan).unwrap();
        assert_eq!(script.steps.len(), 7);
        for operation in &script.steps[..completed] {
            step(&vfs, operation).await;
        }
        if completed == script.steps.len() {
            assert_eq!(
                transaction::capture(&files, &plan.roots, || true)
                    .await
                    .unwrap(),
                plan.after
            );
            assert_eq!(original(&directory.path().join("target")), originals[0]);
        }
        for operation in script.rollback(completed).unwrap() {
            step(&vfs, &operation).await;
        }
        assert_eq!(
            transaction::capture(&files, &plan.roots, || true)
                .await
                .unwrap(),
            plan.before
        );
        for (path, handle) in paths.into_iter().zip(originals) {
            assert_eq!(
                original(&directory.path().join(path)),
                handle,
                "{completed}: {path}"
            );
        }
        assert!(vfs.list("held").await.unwrap().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_destination_creation_never_replaces_the_winning_object() {
    for directory in [false, true] {
        for _ in 0..8 {
            let root = tempfile::tempdir().unwrap();
            let vfs = Arc::new(NativeFsVfs {
                root: root.path().canonicalize().unwrap(),
            });
            let kind = if directory {
                VfsEntryKind::Directory
            } else {
                VfsEntryKind::File
            };
            for name in ["a", "b"] {
                if directory {
                    vfs.write(&format!("{name}/file"), name).await.unwrap();
                } else {
                    vfs.write(name, name).await.unwrap();
                }
            }
            let handles = [
                original(&root.path().join("a")),
                original(&root.path().join("b")),
            ];
            let barrier = Arc::new(tokio::sync::Barrier::new(3));
            let mut tasks = Vec::new();
            for name in ["a", "b"] {
                let (vfs, barrier) = (vfs.clone(), barrier.clone());
                tasks.push(tokio::spawn(async move {
                    barrier.wait().await;
                    vfs.move_atomic(name, "destination", kind).await
                }));
            }
            barrier.wait().await;
            let results = [
                tasks.remove(0).await.unwrap(),
                tasks.remove(0).await.unwrap(),
            ];
            assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
            let winner = usize::from(results[1].is_ok());
            assert!(matches!(
                results[1 - winner],
                Err(VfsError::AlreadyExists(_))
            ));
            assert_eq!(original(&root.path().join("destination")), handles[winner]);
            let loser = ["a", "b"][1 - winner];
            assert_eq!(original(&root.path().join(loser)), handles[1 - winner]);
            let leaf = if directory {
                format!("{loser}/file")
            } else {
                loser.into()
            };
            assert_eq!(vfs.read(&leaf).await.unwrap(), loser);
        }
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cross_filesystem_moves_preserve_the_source_and_never_copy() {
    use std::os::unix::fs::MetadataExt;
    let source = tempfile::tempdir_in("/tmp").unwrap();
    let target = tempfile::tempdir_in("/dev/shm").unwrap();
    assert_ne!(
        std::fs::metadata(source.path()).unwrap().dev(),
        std::fs::metadata(target.path()).unwrap().dev()
    );
    let from = source.path().join("file");
    let to = target.path().join("moved");
    std::fs::write(&from, [0, 255, 1]).unwrap();
    let handle = original(&from);
    let vfs = NativeFsVfs { root: "/".into() };
    let error = vfs
        .move_atomic(
            from.to_str().unwrap(),
            to.to_str().unwrap(),
            VfsEntryKind::File,
        )
        .await
        .unwrap_err();
    assert_eq!(
        error,
        VfsError::Io(std::io::Error::from_raw_os_error(libc::EXDEV).to_string())
    );
    assert_eq!(original(&from), handle);
    assert_eq!(std::fs::read(&from).unwrap(), [0, 255, 1]);
    assert!(!to.exists());
}
