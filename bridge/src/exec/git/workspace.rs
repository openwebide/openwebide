//! Native primitives for the shared workspace path-selection contract.
use super::*;
use openwebide_core::git::{
    GitPathChanges, GitPathRequest, private_pathspecs, workspace_pathspecs,
};

pub(super) async fn workspace_git(
    repo: &Path,
    args: &[&str],
    paths: &[String],
) -> Result<(String, String, bool), GitError> {
    let mut args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    args.push("--".into());
    args.extend(workspace_pathspecs(paths).map_err(GitError::Validation)?);
    exec_git(&args.iter().map(String::as_str).collect::<Vec<_>>(), repo).await
}
pub(super) async fn workspace_history_command(
    repo: &Path,
    args: &[String],
    paths: &[String],
) -> Result<String, GitError> {
    let (out, err, ok) = workspace_git(
        repo,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
        paths,
    )
    .await?;
    if ok {
        Ok(out)
    } else {
        Err(GitError::Execution(err))
    }
}

pub(super) async fn validate_commit_index(
    repo: &Path,
    preserve_head: bool,
) -> Result<(), GitError> {
    let mut args = vec![
        "ls-files".into(),
        "--cached".into(),
        "-z".into(),
        "--".into(),
    ];
    args.extend(private_pathspecs());
    if !history_command(repo, &args).await?.is_empty() {
        return Err(GitError::Validation("Runtime files are already tracked in the index. Remove them from the index before committing; keep the files on disk.".into()));
    }
    // A path-limited commit builds from HEAD; a stash retains HEAD as its base.
    // Both would retain private HEAD entries removed by a pending cleanup commit.
    if preserve_head {
        let mut args = vec![
            "diff".into(),
            "--cached".into(),
            "--name-only".into(),
            "--no-renames".into(),
            "--diff-filter=D".into(),
            "-z".into(),
            "--".into(),
        ];
        args.extend(private_pathspecs());
        if !history_command(repo, &args).await?.is_empty() {
            return Err(GitError::Validation(
                "Commit the staged removal of runtime files before making a path-limited commit or saving a stash."
                    .into(),
            ));
        }
    }
    Ok(())
}

/// Inspect the actual stash trees before applying them. The working/index
/// deltas and optional untracked tree can all carry historical runtime names.
pub(super) async fn validate_stash_apply(repo: &Path, hash: &str) -> Result<(), GitError> {
    let parents = history_command(
        repo,
        &[
            "show".into(),
            "-s".into(),
            "--format=%P".into(),
            hash.into(),
            "--".into(),
        ],
    )
    .await?;
    let parents = parents.split_whitespace().collect::<Vec<_>>();
    if !(2..=3).contains(&parents.len()) {
        return Err(GitError::Validation(
            "The selected object is not a supported stash".into(),
        ));
    }
    let mut paths = std::collections::BTreeSet::new();
    for target in [hash, parents[1]] {
        let output = history_command(
            repo,
            &[
                "diff-tree".into(),
                "--no-commit-id".into(),
                "--no-renames".into(),
                "-r".into(),
                "--name-only".into(),
                "-z".into(),
                parents[0].into(),
                target.into(),
                "--".into(),
            ],
        )
        .await?;
        paths.extend(
            output
                .split('\0')
                .filter(|path| !path.is_empty())
                .map(str::to_owned),
        );
    }
    if let Some(untracked) = parents.get(2) {
        let output = history_command(
            repo,
            &[
                "ls-tree".into(),
                "-r".into(),
                "--name-only".into(),
                "-z".into(),
                (*untracked).into(),
            ],
        )
        .await?;
        paths.extend(
            output
                .split('\0')
                .filter(|path| !path.is_empty())
                .map(str::to_owned),
        );
    }
    if paths
        .iter()
        .any(|path| openwebide_core::vfs::private_workspace_path(path))
    {
        return Err(GitError::Validation("This stash contains runtime files. Remove those entries outside the app before applying it.".into()));
    }
    let changes = GitPathChanges {
        unstaged: paths,
        ..Default::default()
    };
    validate_restore(
        repo,
        &GitPathRequest {
            path: String::new(),
            action: openwebide_core::git::GitPathAction::Revert,
        },
        &changes,
    )
    .await
}

pub(super) async fn validate_checkout_tree(repo: &Path, target: &str) -> Result<(), GitError> {
    validate_commit_index(repo, false).await?;
    for reference in [
        format!("refs/heads/{target}"),
        format!("refs/remotes/{target}"),
    ] {
        let (_, _, exists) =
            exec_git(&["show-ref", "--verify", "--quiet", &reference], repo).await?;
        if exists {
            validate_tree_files(repo, &reference).await?;
            break;
        }
    }
    Ok(())
}

async fn validate_tree_files(repo: &Path, revision: &str) -> Result<(), GitError> {
    let output = history_command(
        repo,
        &[
            "ls-tree".into(),
            "--full-tree".into(),
            "-r".into(),
            "-z".into(),
            revision.into(),
        ],
    )
    .await?;
    let mut paths = std::collections::BTreeSet::new();
    for record in output.split('\0').filter(|record| !record.is_empty()) {
        let (header, path) = record
            .split_once('\t')
            .ok_or_else(|| GitError::Execution("Git returned an invalid tree entry".into()))?;
        if openwebide_core::vfs::private_workspace_path(path) {
            return Err(GitError::Validation("The target revision tracks runtime files. Remove those entries outside the app before updating the working tree.".into()));
        }
        // Gitlinks name submodule directories. Commands explicitly disable
        // recursion, so these directories are not replaced or traversed.
        if !header.starts_with("160000 ") {
            paths.insert(path.to_owned());
        }
    }
    validate_repository_paths(repo, paths).await
}

async fn validate_repository_paths(
    repo: &Path,
    paths: std::collections::BTreeSet<String>,
) -> Result<(), GitError> {
    let root = history_command(repo, &["rev-parse".into(), "--show-toplevel".into()]).await?;
    let root = Path::new(root.strip_suffix('\n').unwrap_or(&root));
    validate_restore(
        root,
        &GitPathRequest {
            path: String::new(),
            action: openwebide_core::git::GitPathAction::Revert,
        },
        &GitPathChanges {
            unstaged: paths,
            ..Default::default()
        },
    )
    .await
}

/// Fetch is read-only with respect to project files. Inspect the exact fetched
/// tree and every local replay delta before allowing a rebase to change them.
pub(super) async fn validate_pull(
    repo: &Path,
    upstream: &str,
    old_head: &str,
) -> Result<(), GitError> {
    validate_commit_index(repo, false).await?;
    validate_tree_files(repo, upstream).await?;
    if old_head.is_empty() {
        return Ok(());
    }
    let output = history_command(
        repo,
        &[
            "log".into(),
            "--raw".into(),
            "--format=".into(),
            "--no-renames".into(),
            "--no-abbrev".into(),
            "--diff-merges=separate".into(),
            "-z".into(),
            format!("{upstream}..{old_head}"),
            "--".into(),
        ],
    )
    .await?;
    let mut records = output.split('\0');
    let mut paths = std::collections::BTreeSet::new();
    while let Some(header) = records.next() {
        let header = header.trim_start_matches('\n');
        if header.is_empty() {
            continue;
        }
        let fields = header.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 5 || !fields[0].starts_with(':') {
            return Err(GitError::Execution(
                "Git returned an invalid replay entry".into(),
            ));
        }
        let path = records
            .next()
            .ok_or_else(|| GitError::Execution("Git omitted a replay path".into()))?;
        if openwebide_core::vfs::private_workspace_path(path) {
            return Err(GitError::Validation("Local commits contain runtime-file changes. Reconcile that history outside the app before rebasing.".into()));
        }
        if !matches!(fields[0], ":160000" | ":000000") || !matches!(fields[1], "160000" | "000000")
        {
            paths.insert(path.to_owned());
        }
    }
    validate_repository_paths(repo, paths).await
}

struct NativeRestoreFiles<'a>(&'a Path);
impl openwebide_core::git::RestoreFiles for NativeRestoreFiles<'_> {
    async fn restore_entry(
        &self,
        path: &str,
    ) -> Result<openwebide_core::git::RestoreEntry, String> {
        use openwebide_core::git::RestoreEntry;
        match tokio::fs::symlink_metadata(self.0.join(path)).await {
            Ok(metadata) if metadata.is_symlink() => Ok(RestoreEntry::Link),
            Ok(metadata) if metadata.is_dir() => Ok(RestoreEntry::Directory),
            Ok(_) => Ok(RestoreEntry::File),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(RestoreEntry::Missing),
            Err(error) => Err(error.to_string()),
        }
    }
}
pub(super) async fn validate_restore(
    repo: &Path,
    request: &GitPathRequest,
    changes: &GitPathChanges,
) -> Result<(), GitError> {
    openwebide_core::git::validate_restore(&NativeRestoreFiles(repo), request, changes)
        .await
        .map_err(GitError::Validation)
}

#[cfg(test)]
mod tests;
