use super::*;
use openwebide_core::git::*;

async fn git(repo: &Path, args: &[&str]) -> String {
    let (out, error, ok) = exec_git(args, repo).await.unwrap();
    assert!(ok, "{args:?}: {error}");
    out
}
async fn repository() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.name", "Workspace Test"],
        vec!["config", "user.email", "workspace@example.org"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["commit", "--allow-empty", "-qm", "initial"],
    ] {
        git(repo.path(), &args).await;
    }
    repo
}
async fn write(repo: &Path, path: &str, data: &str) {
    tokio::fs::create_dir_all(repo.join(path).parent().unwrap())
        .await
        .unwrap_or_else(|error| panic!("{path}: {error}"));
    tokio::fs::write(repo.join(path), data).await.unwrap();
}
fn commit_request() -> GitCommitRequest {
    GitCommitRequest {
        message: "workspace".into(),
        paths: None,
        include_untracked: true,
        staged_only: false,
    }
}
fn private_names() -> Vec<String> {
    let id = "a".repeat(32);
    let mut names = vec![
        ".ſpin/secret".into(),
        ".spin/secret".into(),
        format!("nested/.OPENWEBIDE-PROOF-{}", "C".repeat(32)),
        format!(
            "nested/.OpenWebIde/ReCoVeRy-{}.OwNeR.PaRt",
            id.to_uppercase()
        ),
        format!(".openwebide-proof-{id}"),
        format!("nested/.openwebide-proof-{id}/secret"),
    ];
    for suffix in ["", ".init", ".owner", ".owner.part"] {
        names.push(format!(".openwebide/recovery-{id}{suffix}/secret"));
        names.push(format!("nested/.openwebide/recovery-{id}{suffix}"));
    }
    for ordinal in [0, 1, 9, 10, 99, 100, 999, 1000, 1019, 1020, 1023] {
        names.push(format!("nested/.openwebide-create-{id}-{ordinal}/secret"));
    }
    names
}

#[tokio::test]
async fn bulk_stage_and_commit_exclude_exact_runtime_names_at_every_depth() {
    let repo = repository().await;
    let root = repo.path();
    let id = "a".repeat(32);
    let private = private_names();
    let public = [
        "normal.txt".into(),
        ".openwebide/recovery-notes".into(),
        ".openwebide-proof-notes".into(),
        format!(".openwebide-create-{id}-01"),
        format!(".openwebide-create-{id}-1024"),
        format!(".openwebide-proof-{id}x"),
        format!(".openwebide/recovery-{id}.owner.notes"),
    ];
    for path in private.iter().chain(&public) {
        write(root, path, "payload\n").await;
    }
    let status = get_repo_status(root).await.unwrap();
    assert_eq!(status.files.len(), public.len(), "{:?}", status.files);
    let changes = apply_path_action(
        root,
        &GitPathRequest {
            path: String::new(),
            action: GitPathAction::StageAll,
        },
    )
    .await
    .unwrap();
    assert_eq!(changes.staged.len(), public.len());
    let indexed = git(root, &["ls-files", "-z"]).await;
    for path in &private {
        assert!(!indexed.split('\0').any(|entry| entry == path), "{path}");
    }
    for path in &public {
        assert!(indexed.split('\0').any(|entry| entry == path), "{path}");
    }
    // Add another untracked public file so the commit's own bulk staging is exercised.
    write(root, "later.txt", "later\n").await;
    commit_changes(root, &commit_request()).await.unwrap();
    let committed = git(root, &["ls-tree", "-r", "--name-only", "-z", "HEAD"]).await;
    assert_eq!(
        committed
            .split('\0')
            .filter(|path| !path.is_empty())
            .count(),
        public.len() + 1
    );
    assert!(get_repo_status(root).await.unwrap().is_clean);
    for path in private {
        assert_eq!(
            tokio::fs::read_to_string(root.join(path)).await.unwrap(),
            "payload\n"
        );
    }
    // A legacy index can also contain a case alias; positive selectors must
    // detect it as well as bulk staging's negative selectors excluding it.
    let alias = format!("nested/.OPENWEBIDE-PROOF-{}", "C".repeat(32));
    git(root, &["add", "--", &alias]).await;
    assert!(matches!(
        commit_changes(root, &commit_request()).await,
        Err(GitError::Validation(_))
    ));
}

#[tokio::test]
async fn literal_selection_does_not_stage_glob_neighbors_or_private_children() {
    let repo = repository().await;
    let root = repo.path();
    write(root, "[x]*.txt", "literal").await;
    write(root, "x-other.txt", "neighbor").await;
    write(root, ".spin/secret", "runtime").await;
    apply_path_action(
        root,
        &GitPathRequest {
            path: "[x]*.txt".into(),
            action: GitPathAction::Stage,
        },
    )
    .await
    .unwrap();
    assert_eq!(git(root, &["ls-files", "-z"]).await, "[x]*.txt\0");
    commit_changes(
        root,
        &GitCommitRequest {
            paths: Some(vec!["[x]*.txt".into()]),
            ..commit_request()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        git(root, &["ls-tree", "-r", "--name-only", "-z", "HEAD"]).await,
        "[x]*.txt\0"
    );
    write(root, "[x]*.txt", "changed").await;
    assert!(
        get_repo_diff(root, Some("[x]*.txt"))
            .await
            .unwrap()
            .contains("+changed")
    );
    assert_eq!(
        get_file_at_head(root, "[x]*.txt").await.unwrap().content,
        "literal"
    );
}

#[tokio::test]
async fn tracked_runtime_payloads_are_hidden_and_cannot_enter_a_new_commit() {
    let repo = repository().await;
    let root = repo.path();
    let path = format!(".openwebide/recovery-{}/objects/secret", "b".repeat(32));
    write(root, &path, "private-old\n").await;
    git(root, &["add", "--", &path]).await;
    git(root, &["commit", "-qm", "legacy private entry"]).await;
    let hash = git(root, &["rev-parse", "HEAD"]).await.trim().to_owned();
    write(root, &path, "private-new\n").await;
    git(root, &["add", "--", &path]).await;
    assert!(get_repo_diff(root, None).await.unwrap().is_empty());
    assert!(get_index_diff(root).await.unwrap().is_empty());
    assert!(get_repo_status(root).await.unwrap().files.is_empty());
    let detail = get_commit_diff(
        root,
        &GitCommitDiffRequest {
            hash,
            parent: None,
            path: None,
        },
    )
    .await
    .unwrap();
    assert!(detail.diff.is_empty());
    assert!(detail.files.is_empty());
    assert!(matches!(
        get_file_at_head(root, &path).await,
        Err(GitError::Validation(_))
    ));
    write(root, "public.txt", "public\n").await;
    let before = git(root, &["ls-files", "--stage", "-z"]).await;
    assert!(matches!(
        commit_changes(root, &commit_request()).await,
        Err(GitError::Validation(_))
    ));
    assert_eq!(git(root, &["ls-files", "--stage", "-z"]).await, before);
    git(root, &["rm", "--cached", "--", &path]).await;
    assert!(matches!(
        manage_stash(
            root,
            &GitStashRequest {
                action: GitStashAction::Save,
                hash: None,
                message: None
            }
        )
        .await,
        Err(GitError::Validation(_))
    ));
    assert!(git(root, &["stash", "list"]).await.is_empty());
    assert!(matches!(
        commit_changes(
            root,
            &GitCommitRequest {
                paths: Some(vec!["public.txt".into()]),
                ..commit_request()
            }
        )
        .await,
        Err(GitError::Validation(_))
    ));
    commit_changes(
        root,
        &GitCommitRequest {
            staged_only: true,
            include_untracked: false,
            ..commit_request()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(root.join(path)).await.unwrap(),
        "private-new\n"
    );
}

#[tokio::test]
async fn revert_preserves_ignored_files_below_a_replaced_tracked_file() {
    let repo = repository().await;
    let root = repo.path();
    write(root, "item", "tracked\n").await;
    git(root, &["add", "item"]).await;
    git(root, &["commit", "-qm", "tracked file"]).await;
    tokio::fs::remove_file(root.join("item")).await.unwrap();
    write(root, "item/.spin/secret", "retain me").await;
    write(root, ".git/info/exclude", "item/.spin/\n").await;
    assert!(matches!(
        apply_path_action(
            root,
            &GitPathRequest {
                path: "item".into(),
                action: GitPathAction::Revert
            }
        )
        .await,
        Err(GitError::Validation(_))
    ));
    assert_eq!(
        tokio::fs::read_to_string(root.join("item/.spin/secret"))
            .await
            .unwrap(),
        "retain me"
    );
    assert_eq!(git(root, &["show", "HEAD:item"]).await, "tracked\n");
}

#[tokio::test]
async fn project_bulk_commit_does_not_stage_siblings_or_include_their_staged_changes() {
    let repo = repository().await;
    let root = repo.path();
    write(root, "project/file.txt", "project\n").await;
    write(root, "project/.spin/secret", "private\n").await;
    write(root, "project/.ſpin/secret", "private\n").await;
    write(root, "outside.txt", "sibling\n").await;
    let project = root.join("project");
    commit_changes(&project, &commit_request()).await.unwrap();
    assert_eq!(
        git(root, &["ls-tree", "-r", "--name-only", "-z", "HEAD"]).await,
        "project/file.txt\0"
    );
    assert_eq!(
        get_file_at_head(&project, "file.txt")
            .await
            .unwrap()
            .content,
        "project\n"
    );
    git(root, &["add", "outside.txt"]).await;
    write(root, "project/file.txt", "modified\n").await;
    let before = git(root, &["ls-files", "--stage", "-z"]).await;
    assert!(matches!(
        commit_changes(&project, &commit_request()).await,
        Err(GitError::Validation(_))
    ));
    assert_eq!(git(root, &["ls-files", "--stage", "-z"]).await, before);
}

#[tokio::test]
async fn stash_save_and_apply_preserve_runtime_files_and_refuse_legacy_private_entries() {
    let repo = repository().await;
    let root = repo.path();
    write(root, "tracked", "original\n").await;
    git(root, &["add", "tracked"]).await;
    git(root, &["commit", "-qm", "tracked"]).await;
    write(root, "tracked", "edited\n").await;
    write(root, "new.txt", "new\n").await;
    for path in private_names() {
        write(root, &path, "private\n").await;
    }
    let result = manage_stash(
        root,
        &GitStashRequest {
            action: GitStashAction::Save,
            hash: None,
            message: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(result.stashes.len(), 1);
    assert_eq!(
        tokio::fs::read_to_string(root.join("tracked"))
            .await
            .unwrap(),
        "original\n"
    );
    assert!(!root.join("new.txt").exists());
    for path in private_names() {
        assert_eq!(
            tokio::fs::read_to_string(root.join(path)).await.unwrap(),
            "private\n"
        );
    }
    manage_stash(
        root,
        &GitStashRequest {
            action: GitStashAction::Apply,
            hash: Some(result.stashes[0].hash.clone()),
            message: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(root.join("tracked"))
            .await
            .unwrap(),
        "edited\n"
    );
    assert!(root.join("new.txt").exists());
    git(
        root,
        &[
            "stash",
            "push",
            "--include-untracked",
            "-m",
            "legacy with private files",
        ],
    )
    .await;
    let hash = git(root, &["rev-parse", "refs/stash"])
        .await
        .trim()
        .to_owned();
    write(root, ".spin/secret", "newer runtime\n").await;
    let before = git(root, &["ls-files", "--stage", "-z"]).await;
    assert!(matches!(
        manage_stash(
            root,
            &GitStashRequest {
                action: GitStashAction::Apply,
                hash: Some(hash),
                message: None
            }
        )
        .await,
        Err(GitError::Validation(_))
    ));
    assert_eq!(git(root, &["ls-files", "--stage", "-z"]).await, before);
    assert_eq!(
        tokio::fs::read_to_string(root.join(".spin/secret"))
            .await
            .unwrap(),
        "newer runtime\n"
    );
}

#[tokio::test]
async fn stash_save_and_branch_switch_refuse_runtime_directory_collisions() {
    let repo = repository().await;
    let root = repo.path();
    write(root, "item", "tracked\n").await;
    git(root, &["add", "item"]).await;
    git(root, &["commit", "-qm", "tracked"]).await;
    git(root, &["branch", "with-item"]).await;
    tokio::fs::remove_file(root.join("item")).await.unwrap();
    write(root, "item/.spin/secret", "retain me").await;
    write(root, ".git/info/exclude", "item/.spin/\n").await;
    assert!(matches!(
        manage_stash(
            root,
            &GitStashRequest {
                action: GitStashAction::Save,
                hash: None,
                message: None
            }
        )
        .await,
        Err(GitError::Validation(_))
    ));
    // Record the public deletion, leaving the ignored runtime directory intact.
    git(root, &["add", "-u", "--", "item"]).await;
    git(root, &["commit", "-qm", "without item"]).await;
    assert!(matches!(
        checkout_branch(
            root,
            &GitCheckoutRequest {
                branch: "with-item".into(),
                create_if_missing: false
            }
        )
        .await,
        Err(GitError::Validation(_))
    ));
    assert_eq!(
        git(root, &["branch", "--show-current"]).await.trim(),
        "main"
    );
    assert_eq!(
        tokio::fs::read_to_string(root.join("item/.spin/secret"))
            .await
            .unwrap(),
        "retain me"
    );
}

#[tokio::test]
async fn branch_switch_refuses_a_target_that_tracks_runtime_files() {
    let repo = repository().await;
    let root = repo.path();
    git(root, &["switch", "-c", "legacy"]).await;
    write(root, ".spin/secret", "legacy private\n").await;
    git(root, &["add", ".spin/secret"]).await;
    git(root, &["commit", "-qm", "legacy private"]).await;
    git(root, &["switch", "main"]).await;
    assert!(matches!(
        checkout_branch(
            root,
            &GitCheckoutRequest {
                branch: "legacy".into(),
                create_if_missing: false
            }
        )
        .await,
        Err(GitError::Validation(_))
    ));
    assert!(!root.join(".spin/secret").exists());
    assert_eq!(
        git(root, &["branch", "--show-current"]).await.trim(),
        "main"
    );
}

#[tokio::test]
async fn inherited_pathspec_flags_cannot_disable_runtime_exclusions() {
    const CHILD: &str = "OPENWEBIDE_GIT_WORKSPACE_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        for flag in [
            "GIT_LITERAL_PATHSPECS",
            "GIT_GLOB_PATHSPECS",
            "GIT_NOGLOB_PATHSPECS",
            "GIT_ICASE_PATHSPECS",
        ] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "exec::git::workspace::tests::inherited_pathspec_flags_cannot_disable_runtime_exclusions", "--nocapture"])
                .env(CHILD, "1").env(flag, "1").output().await.unwrap();
            assert!(
                output.status.success(),
                "{flag}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        }
        return;
    }
    let repo = repository().await;
    let root = repo.path();
    write(root, "[x]*.txt", "literal\n").await;
    write(root, "x-other.txt", "neighbor\n").await;
    write(root, ".spin/secret", "private\n").await;
    apply_path_action(
        root,
        &GitPathRequest {
            path: "[x]*.txt".into(),
            action: GitPathAction::Stage,
        },
    )
    .await
    .unwrap();
    assert_eq!(git(root, &["ls-files", "-z"]).await, "[x]*.txt\0");
    apply_path_action(
        root,
        &GitPathRequest {
            path: String::new(),
            action: GitPathAction::StageAll,
        },
    )
    .await
    .unwrap();
    let index = git(root, &["ls-files", "-z"]).await;
    assert_eq!(index.split('\0').filter(|path| !path.is_empty()).count(), 2);
    assert!(!index.contains("secret"));
}

async fn paired_repository() -> (tempfile::TempDir, tempfile::TempDir) {
    let remote = repository().await;
    write(remote.path(), "base.txt", "base\n").await;
    git(remote.path(), &["add", "base.txt"]).await;
    git(remote.path(), &["commit", "-qm", "base"]).await;
    let local = repository().await;
    git(
        local.path(),
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    )
    .await;
    git(local.path(), &["fetch", "origin"]).await;
    git(local.path(), &["reset", "--hard", "origin/main"]).await;
    (remote, local)
}
fn pull_request() -> GitSyncRequest {
    GitSyncRequest {
        action: "pull".into(),
        remote: Some("origin".into()),
        branch: Some("main".into()),
    }
}

#[tokio::test]
async fn pull_preserves_ignored_runtime_directories_before_any_rebase() {
    let (remote, local) = paired_repository().await;
    write(remote.path(), "item", "upstream file\n").await;
    git(remote.path(), &["add", "item"]).await;
    git(remote.path(), &["commit", "-qm", "upstream item"]).await;
    write(local.path(), "item/.spin/secret", "retain me").await;
    write(local.path(), ".git/info/exclude", "item/.spin/\n").await;
    // The operation affects the whole repository even when a subproject is open.
    tokio::fs::create_dir(local.path().join("project"))
        .await
        .unwrap();
    let before = git(local.path(), &["rev-parse", "HEAD"]).await;
    assert!(matches!(
        sync_repo(&local.path().join("project"), &pull_request()).await,
        Err(GitError::Validation(_))
    ));
    assert_eq!(git(local.path(), &["rev-parse", "HEAD"]).await, before);
    assert_eq!(
        tokio::fs::read_to_string(local.path().join("item/.spin/secret"))
            .await
            .unwrap(),
        "retain me"
    );
    assert!(!local.path().join(".git/rebase-merge").exists());
    assert!(!local.path().join(".git/rebase-apply").exists());
}

#[tokio::test]
async fn pull_replays_literal_local_paths_but_refuses_runtime_changes_in_replay_history() {
    let (remote, local) = paired_repository().await;
    write(local.path(), "local\n[x]*.txt", "local\n").await;
    git(local.path(), &["add", "--", "local\n[x]*.txt"]).await;
    git(local.path(), &["commit", "-qm", "local commit"]).await;
    write(remote.path(), "upstream.txt", "upstream\n").await;
    git(remote.path(), &["add", "upstream.txt"]).await;
    git(remote.path(), &["commit", "-qm", "upstream commit"]).await;
    sync_repo(local.path(), &pull_request()).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(local.path().join("local\n[x]*.txt"))
            .await
            .unwrap(),
        "local\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(local.path().join("upstream.txt"))
            .await
            .unwrap(),
        "upstream\n"
    );
    write(local.path(), ".spin/secret", "retain private\n").await;
    git(local.path(), &["add", ".spin/secret"]).await;
    git(local.path(), &["commit", "-qm", "legacy private entry"]).await;
    git(local.path(), &["rm", "--cached", ".spin/secret"]).await;
    git(local.path(), &["commit", "-qm", "remove private entry"]).await;
    write(remote.path(), "upstream.txt", "later upstream\n").await;
    git(remote.path(), &["commit", "-am", "later upstream"]).await;
    let before = git(local.path(), &["rev-parse", "HEAD"]).await;
    assert!(matches!(
        sync_repo(local.path(), &pull_request()).await,
        Err(GitError::Validation(_))
    ));
    assert_eq!(git(local.path(), &["rev-parse", "HEAD"]).await, before);
    assert_eq!(
        tokio::fs::read_to_string(local.path().join(".spin/secret"))
            .await
            .unwrap(),
        "retain private\n"
    );
}

#[tokio::test]
async fn pull_supports_unborn_repositories_and_rejects_private_upstream_trees() {
    let remote = repository().await;
    write(remote.path(), "normal.txt", "upstream\n").await;
    git(remote.path(), &["add", "normal.txt"]).await;
    git(remote.path(), &["commit", "-qm", "upstream"]).await;
    let local = tempfile::tempdir().unwrap();
    git(local.path(), &["init", "-q", "-b", "main"]).await;
    git(
        local.path(),
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    )
    .await;
    sync_repo(local.path(), &pull_request()).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(local.path().join("normal.txt"))
            .await
            .unwrap(),
        "upstream\n"
    );
    write(remote.path(), ".spin/secret", "historical private\n").await;
    git(remote.path(), &["add", ".spin/secret"]).await;
    git(remote.path(), &["commit", "-qm", "legacy private"]).await;
    write(local.path(), ".spin/secret", "current private\n").await;
    let before = git(local.path(), &["rev-parse", "HEAD"]).await;
    assert!(matches!(
        sync_repo(local.path(), &pull_request()).await,
        Err(GitError::Validation(_))
    ));
    assert_eq!(git(local.path(), &["rev-parse", "HEAD"]).await, before);
    assert_eq!(
        tokio::fs::read_to_string(local.path().join(".spin/secret"))
            .await
            .unwrap(),
        "current private\n"
    );
}

#[tokio::test]
async fn switching_without_submodule_recursion_preserves_runtime_data_in_gitlink_directories() {
    let repo = repository().await;
    let root = repo.path();
    let hash = git(root, &["rev-parse", "HEAD"]).await;
    git(
        root,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},submodule", hash.trim()),
        ],
    )
    .await;
    git(root, &["commit", "-qm", "gitlink"]).await;
    git(root, &["branch", "topic"]).await;
    write(root, "submodule/.spin/secret", "keep me").await;
    git(root, &["config", "submodule.recurse", "true"]).await;
    checkout_branch(
        root,
        &GitCheckoutRequest {
            branch: "topic".into(),
            create_if_missing: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(root.join("submodule/.spin/secret"))
            .await
            .unwrap(),
        "keep me"
    );
}

#[tokio::test]
async fn file_history_stops_before_a_private_rename_source() {
    let repo = repository().await;
    let root = repo.path();
    let private = format!(".openwebide/recovery-{}/objects/secret", "a".repeat(32));
    write(root, &private, "legacy payload\n").await;
    git(root, &["add", "--", &private]).await;
    git(root, &["commit", "-qm", "legacy private entry"]).await;
    git(root, &["mv", "--", &private, "public.txt"]).await;
    git(root, &["commit", "-qm", "public file"]).await;
    let history = get_history(
        root,
        &GitHistoryRequest {
            path: Some("public.txt".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(history.commits.len(), 1);
    assert_eq!(
        history.commits[0].history_path.as_deref(),
        Some("public.txt")
    );
    let detail = get_commit_diff(
        root,
        &GitCommitDiffRequest {
            hash: history.commits[0].hash.clone(),
            path: Some("public.txt".into()),
            parent: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(detail.files.len(), 1);
    assert!(detail.files[0].previous_path.is_none());
    assert!(!detail.diff.contains(".openwebide/"));
}
