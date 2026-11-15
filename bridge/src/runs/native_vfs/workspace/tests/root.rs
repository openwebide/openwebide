use super::*;

#[tokio::test]
async fn directory_bound_mutations_preserve_alias_and_entry_kind_contracts() {
    use std::os::unix::fs::symlink;
    for captured in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("ordinary")).unwrap();
        symlink(
            directory.path().join("ordinary"),
            directory.path().join("absolute"),
        )
        .unwrap();
        symlink("ordinary", directory.path().join("relative")).unwrap();
        symlink(outside.path(), directory.path().join("outside")).unwrap();
        let files: Box<dyn Vfs> = if captured {
            Box::new(WorkspaceFiles::new(
                directory.path().into(),
                Coordinator::default(),
            ))
        } else {
            Box::new(NativeFsVfs {
                root: directory.path().into(),
            })
        };
        files.write("absolute/new/source", "before").await.unwrap();
        files
            .copy("relative/new/source", "absolute/new/copied")
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(directory.path().join("ordinary/new/copied")).unwrap(),
            b"before"
        );
        files
            .create("absolute/new/empty", VfsEntryKind::Directory)
            .await
            .unwrap();
        files
            .remove_entry("relative/new/empty", VfsEntryKind::Directory)
            .await
            .unwrap();
        symlink("source", directory.path().join("ordinary/new/leaf")).unwrap();
        assert!(matches!(
            files.create("absolute/new/leaf", VfsEntryKind::File).await,
            Err(VfsError::AlreadyExists(_))
        ));
        assert!(
            files
                .remove_entry("relative/new/leaf", VfsEntryKind::File)
                .await
                .is_err()
        );
        assert!(
            files
                .replace_atomic("absolute/new/leaf", b"unwanted")
                .await
                .is_err()
        );
        files.delete("absolute/new/leaf").await.unwrap();
        assert_eq!(
            std::fs::read(directory.path().join("ordinary/new/source")).unwrap(),
            b"before"
        );
        assert!(!directory.path().join("ordinary/new/leaf").exists());
        for operation in 0..3 {
            let result = match operation {
                0 => files.write("outside/escape", "unwanted").await,
                1 => files.create("outside/escape", VfsEntryKind::File).await,
                2 => files.copy("relative/new/source", "outside/escape").await,
                _ => unreachable!(),
            };
            assert!(result.is_err());
            assert!(!outside.path().join("escape").exists());
        }
        assert!(
            files
                .move_atomic("relative/new/source", "moved", VfsEntryKind::File)
                .await
                .is_err()
        );
        files.delete("absolute/new").await.unwrap();
        assert!(!directory.path().join("ordinary/new").exists());
        assert!(
            std::fs::symlink_metadata(directory.path().join("absolute"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}

#[tokio::test]
async fn admitted_mutations_use_the_original_directory_after_its_path_is_replaced() {
    for operation in 0..8 {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("project");
        let original = directory.path().join("original");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("source"), "before").unwrap();
        let files = WorkspaceFiles::new(root.clone(), Coordinator::default());
        let entered = Arc::new(Notify::new());
        let started = entered.clone();
        let (release, proceed) = std::sync::mpsc::channel();
        let task = tokio::spawn(async move {
            files
                .mutate(move |raw| {
                    move || {
                        started.notify_one();
                        proceed
                            .recv()
                            .map_err(|error| VfsError::Io(error.to_string()))?;
                        match operation {
                            0 | 1 => raw.write_bytes_sync("source", b"after"),
                            2 => raw.replace_atomic_sync("source", b"after"),
                            3 => raw.create_sync("created", VfsEntryKind::File),
                            4 => raw.delete_sync("source"),
                            5 => raw.remove_entry_sync("source", VfsEntryKind::File),
                            6 => raw.copy_sync("source", "copied"),
                            7 => raw.move_atomic_sync("source", "moved", VfsEntryKind::File),
                            _ => unreachable!(),
                        }
                    }
                })
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        std::fs::rename(&root, &original).unwrap();
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("source"), "replacement").unwrap();
        release.send(()).unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        // Effects on the captured object require reconciliation: a changed
        // pathname must never be acknowledged as a successful current write.
        assert!(outcome.is_err(), "operation {operation}");
        assert_eq!(
            std::fs::read(root.join("source")).unwrap(),
            b"replacement",
            "operation {operation} wrote to the replacement folder"
        );
        for name in ["created", "copied", "moved"] {
            assert!(!root.join(name).exists(), "operation {operation}: {name}");
        }
        match operation {
            0..=2 => assert_eq!(std::fs::read(original.join("source")).unwrap(), b"after"),
            3 => assert!(original.join("created").is_file()),
            4 | 5 => assert!(!original.join("source").exists()),
            6 => assert_eq!(std::fs::read(original.join("copied")).unwrap(), b"before"),
            7 => {
                assert!(!original.join("source").exists());
                assert_eq!(std::fs::read(original.join("moved")).unwrap(), b"before");
            }
            _ => unreachable!(),
        }
        println!(
            "operation={operation}: original object mutated, replacement untouched, changed-root acknowledgement rejected"
        );
    }
}
