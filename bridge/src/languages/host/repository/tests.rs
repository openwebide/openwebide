use super::*;
use std::path::Path;

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.org",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn initialize(root: &Path) {
    let format = std::env::var("OPENWEBIDE_TEST_GIT_REF_FORMAT").unwrap_or_else(|_| "files".into());
    assert!(matches!(format.as_str(), "files" | "reftable"));
    let mut args = vec!["init", "-q", "-b", "main"];
    // The default retains compatibility with the supported Git 2.39 tools.
    // A separate explicit run verifies reftable on a capable installed Git.
    if format == "reftable" {
        args.push("--ref-format=reftable");
    }
    git(root, &args);
    git(root, &["config", "core.logAllRefUpdates", "false"]);
    std::fs::write(root.join("main.notes"), "source").unwrap();
    git(root, &["add", "main.notes"]);
    git(root, &["commit", "-qm", "initial"]);
}

#[tokio::test]
async fn git_metadata_ignores_index_refreshes_and_rejects_branch_round_trips() {
    let root = tempfile::tempdir().unwrap();
    initialize(root.path());
    let repository = Repository::new(root.path()).await.unwrap();
    repository.check(root.path()).await.unwrap();
    std::fs::write(root.path().join(".git/index.lock"), "unrelated").unwrap();
    std::fs::remove_file(root.path().join(".git/index.lock")).unwrap();
    git(root.path(), &["status", "--porcelain"]);
    repository.check(root.path()).await.unwrap();
    let before = crate::exec::git::repository_revision(root.path())
        .await
        .unwrap();
    let head = root.path().join(".git/HEAD");
    let modified = std::fs::metadata(&head).unwrap().modified().unwrap();
    let original = std::fs::read(&head).unwrap();
    git(root.path(), &["checkout", "-q", "-b", "topic"]);
    git(root.path(), &["checkout", "-q", "main"]);
    std::fs::File::options()
        .write(true)
        .open(&head)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(std::fs::read(&head).unwrap(), original);
    assert_eq!(
        crate::exec::git::repository_revision(root.path())
            .await
            .unwrap(),
        before
    );
    assert!(
        repository
            .check(root.path())
            .await
            .unwrap_err()
            .contains("Git revision changed")
    );
}

#[tokio::test]
async fn git_metadata_tracks_packed_detached_and_linked_worktree_reference_round_trips() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("repository");
    std::fs::create_dir(&root).unwrap();
    initialize(&root);
    let original = git(&root, &["rev-parse", "HEAD"]);
    git(&root, &["checkout", "-q", "-b", "other"]);
    git(&root, &["commit", "--allow-empty", "-qm", "other"]);
    let other = git(&root, &["rev-parse", "HEAD"]);
    git(&root, &["checkout", "-q", "main"]);
    git(&root, &["pack-refs", "--all", "--prune"]);
    let linked = parent.path().join("linked");
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
    );
    for workspace in [&root, &linked] {
        let repository = Repository::new(workspace).await.unwrap();
        let before = crate::exec::git::repository_revision(workspace)
            .await
            .unwrap();
        git(workspace, &["update-ref", "HEAD", &other]);
        git(workspace, &["update-ref", "HEAD", &original]);
        assert_eq!(
            crate::exec::git::repository_revision(workspace)
                .await
                .unwrap(),
            before
        );
        assert!(
            repository
                .check(workspace)
                .await
                .unwrap_err()
                .contains("Git revision changed")
        );
    }
    git(&root, &["checkout", "-q", "--detach"]);
    let repository = Repository::new(&root).await.unwrap();
    git(&root, &["update-ref", "HEAD", &other]);
    git(&root, &["update-ref", "HEAD", &original]);
    assert!(
        repository
            .check(&root)
            .await
            .unwrap_err()
            .contains("Git revision changed")
    );
}

#[tokio::test]
async fn git_symbolic_reference_chain_round_trips_reject_the_original_context() {
    let root = tempfile::tempdir().unwrap();
    initialize(root.path());
    git(root.path(), &["branch", "other"]);
    git(
        root.path(),
        &["symbolic-ref", "refs/heads/alias", "refs/heads/main"],
    );
    git(root.path(), &["symbolic-ref", "HEAD", "refs/heads/alias"]);
    let repository = Repository::new(root.path()).await.unwrap();
    let before = crate::exec::git::repository_revision(root.path())
        .await
        .unwrap();
    git(
        root.path(),
        &["symbolic-ref", "refs/heads/alias", "refs/heads/other"],
    );
    git(
        root.path(),
        &["symbolic-ref", "refs/heads/alias", "refs/heads/main"],
    );
    assert_eq!(
        crate::exec::git::repository_revision(root.path())
            .await
            .unwrap(),
        before
    );
    assert!(
        repository
            .check(root.path())
            .await
            .unwrap_err()
            .contains("Git revision changed")
    );
}

#[tokio::test]
async fn ordinary_non_repository_writes_keep_the_context_and_git_creation_changes_it() {
    let root = tempfile::tempdir().unwrap();
    let repository = Repository::new(root.path()).await.unwrap();
    std::fs::write(root.path().join("ordinary"), "write").unwrap();
    repository.check(root.path()).await.unwrap();
    initialize(root.path());
    assert!(
        repository
            .check(root.path())
            .await
            .unwrap_err()
            .contains("Git revision changed")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn git_metadata_follows_external_reference_directory_aliases() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("project");
    std::fs::create_dir(&root).unwrap();
    initialize(&root);
    let references = parent.path().join("references");
    std::fs::rename(root.join(".git/refs"), &references).unwrap();
    std::os::unix::fs::symlink(&references, root.join(".git/refs")).unwrap();
    let repository = Repository::new(&root).await.unwrap();
    repository.check(&root).await.unwrap();
    git(&root, &["checkout", "-q", "-b", "topic"]);
    git(&root, &["checkout", "-q", "main"]);
    assert!(
        repository
            .check(&root)
            .await
            .unwrap_err()
            .contains("Git revision changed")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn symbolic_git_directories_preserve_index_refreshes_and_reject_retargeting() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("project");
    std::fs::create_dir(&root).unwrap();
    initialize(&root);
    let metadata = parent.path().join("metadata");
    std::fs::rename(root.join(".git"), &metadata).unwrap();
    std::os::unix::fs::symlink(&metadata, root.join(".git")).unwrap();
    let repository = Repository::new(&root).await.unwrap();
    std::fs::write(metadata.join("index.lock"), "unrelated").unwrap();
    std::fs::remove_file(metadata.join("index.lock")).unwrap();
    git(&root, &["status", "--porcelain"]);
    repository.check(&root).await.unwrap();
    // Replacing a marker with an equivalent link must not revive the context.
    std::fs::remove_file(root.join(".git")).unwrap();
    std::os::unix::fs::symlink(&metadata, root.join(".git")).unwrap();
    assert!(
        repository
            .check(&root)
            .await
            .unwrap_err()
            .contains("Git revision changed")
    );
}
