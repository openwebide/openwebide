use super::*;
use crate::{MemoryVfs, vfs::VfsEntryKind};

#[test]
fn fallback_hides_runtime_objects_and_preserves_them_when_reverting_a_folder() {
    futures::executor::block_on(async {
        let vfs = MemoryVfs::new();
        vfs.write(".git/HEAD", "ref: refs/heads/main\n")
            .await
            .unwrap();
        vfs.write("item", "original\n").await.unwrap();
        let git = LocalGit::new(vfs.clone());
        let request = GitCommitRequest {
            message: "initial".into(),
            paths: None,
            include_untracked: true,
            staged_only: false,
        };
        git.commit(&request).await.unwrap();
        vfs.delete("item").await.unwrap();
        vfs.create("item", VfsEntryKind::Directory).await.unwrap();
        let private = format!(
            "item/.openwebide/recovery-{}/objects/secret",
            "a".repeat(32)
        );
        vfs.write(&private, "private payload").await.unwrap();
        vfs.write(".openwebide-proof-notes", "ordinary notes")
            .await
            .unwrap();
        let status = git.status().await.unwrap();
        assert_eq!(status.files.len(), 2);
        assert_eq!(status.files["item"], GitFileStatus::Deleted);
        assert!(
            git.path_action(&GitPathRequest {
                path: "item".into(),
                action: GitPathAction::Revert
            })
            .await
            .is_err()
        );
        assert_eq!(vfs.read(&private).await.unwrap(), "private payload");
        assert!(git.file_head(&private).await.is_err());
        assert!(git.diff(Some(&private)).await.is_err());
        assert!(
            git.commit(&GitCommitRequest {
                paths: Some(vec![private.clone()]),
                ..request.clone()
            })
            .await
            .is_err()
        );
        git.commit(&request).await.unwrap();
        assert_eq!(git.status().await.unwrap().files.len(), 0);
        let hash = git.resolve_ref("HEAD").await.unwrap();
        let (_, data) = git.read_object(&hash).await.unwrap();
        let commit = git.parse_commit(&hash, &data).unwrap();
        assert_eq!(
            git.flatten_tree(&commit.tree_hash)
                .await
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            [".openwebide-proof-notes"]
        );
    });
}

#[test]
fn fallback_folder_revert_restores_changed_leaves_and_leaves_untracked_files() {
    futures::executor::block_on(async {
        let vfs = MemoryVfs::new();
        vfs.write(".git/HEAD", "ref: refs/heads/main\n")
            .await
            .unwrap();
        vfs.write("src/a.txt", "original a").await.unwrap();
        vfs.write("src/b.txt", "original b").await.unwrap();
        let git = LocalGit::new(vfs.clone());
        git.commit(&GitCommitRequest {
            message: "initial".into(),
            paths: None,
            include_untracked: true,
            staged_only: false,
        })
        .await
        .unwrap();
        vfs.write("src/a.txt", "edited").await.unwrap();
        vfs.delete("src/b.txt").await.unwrap();
        vfs.write("src/scratch", "keep me").await.unwrap();
        git.path_action(&GitPathRequest {
            path: "src".into(),
            action: GitPathAction::Revert,
        })
        .await
        .unwrap();
        assert_eq!(vfs.read("src/a.txt").await.unwrap(), "original a");
        assert_eq!(vfs.read("src/b.txt").await.unwrap(), "original b");
        assert_eq!(vfs.read("src/scratch").await.unwrap(), "keep me");
        assert!(
            git.path_action(&GitPathRequest {
                path: String::new(),
                action: GitPathAction::StageAll
            })
            .await
            .is_err()
        );
    });
}
