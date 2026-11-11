use super::*;

async fn git(root: &Path, args: &[&str]) {
    let (_, error, success) = exec_git(args, root).await.unwrap();
    assert!(success, "{args:?}: {error}");
}
async fn commit(root: &Path) {
    git(
        root,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.org",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "revision",
        ],
    )
    .await;
}

#[tokio::test]
async fn revisions_cover_unborn_packed_detached_and_linked_worktrees() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repository");
    tokio::fs::create_dir(&root).await.unwrap();
    assert_eq!(repository_revision(&root).await.unwrap(), None);
    git(&root, &["init", "-q", "-b", "main"]).await;
    let unborn = repository_revision(&root).await.unwrap().unwrap();
    assert_eq!(unborn.commit, None);
    assert_eq!(unborn.reference.as_deref(), Some("refs/heads/main"));
    commit(&root).await;
    let first = repository_revision(&root).await.unwrap().unwrap();
    assert_eq!(first.commit.as_ref().unwrap().len(), 40);
    git(&root, &["checkout", "-q", "-b", "topic"]).await;
    let branch = repository_revision(&root).await.unwrap().unwrap();
    assert_eq!(first.commit, branch.commit);
    assert_ne!(first.reference, branch.reference);
    commit(&root).await;
    let second = repository_revision(&root).await.unwrap().unwrap();
    assert_ne!(branch.commit, second.commit);
    git(&root, &["pack-refs", "--all", "--prune"]).await;
    assert_eq!(
        repository_revision(&root).await.unwrap().as_ref(),
        Some(&second)
    );
    git(&root, &["checkout", "-q", "--detach"]).await;
    let detached = repository_revision(&root).await.unwrap().unwrap();
    assert_eq!(detached.commit, second.commit);
    assert_eq!(detached.reference, None);
    let linked = temp.path().join("linked");
    git(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ],
    )
    .await;
    let revision = repository_revision(&linked).await.unwrap().unwrap();
    assert_eq!(revision.commit, second.commit);
    assert_eq!(revision.reference.as_deref(), Some("refs/heads/linked"));
    tokio::fs::create_dir(linked.join("subdirectory"))
        .await
        .unwrap();
    assert_eq!(
        repository_revision(&linked.join("subdirectory"))
            .await
            .unwrap(),
        Some(revision)
    );
}

#[tokio::test]
async fn revision_supports_sha256_and_does_not_hide_corrupt_repository_metadata() {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &["init", "-q", "--object-format=sha256", "-b", "main"],
    )
    .await;
    commit(root.path()).await;
    let revision = repository_revision(root.path()).await.unwrap().unwrap();
    assert_eq!(revision.commit.unwrap().len(), 64);
    tokio::fs::write(root.path().join(".git/HEAD"), "not a git revision\n")
        .await
        .unwrap();
    assert!(repository_revision(root.path()).await.is_err());
}
