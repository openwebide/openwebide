//! Native Git execution against the mounted repository.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use base64::Engine;
use openwebide_core::{
    GitBranchInfo, GitCheckoutRequest, GitCheckoutResult, GitCommitRequest, GitCommitResult,
    GitRepoStatus, GitSyncRequest, GitSyncResult, parse_porcelain_v1,
};
use tokio::process::Command;
use tokio::time::timeout;

pub use crate::BridgeError as GitError;
mod metadata;
#[cfg(test)]
mod revision_tests;
pub(crate) use metadata::repository_metadata;
mod workspace;
use workspace::{
    validate_commit_index, validate_restore, workspace_git, workspace_history_command,
};

/// Ask Git itself, honoring linked worktrees and inherited Git environment.
/// This is a read-only discovery primitive for the shared execution facade.
pub(crate) async fn execution_root(cwd: &Path) -> Result<std::path::PathBuf, GitError> {
    let output = exec_git_output(&["rev-parse", "--show-toplevel"], cwd).await?;
    let bytes = if output.status.success() {
        output.stdout
    } else {
        let bare = exec_git_output(&["rev-parse", "--is-bare-repository"], cwd).await?;
        if !bare.status.success() || String::from_utf8_lossy(&bare.stdout).trim() != "true" {
            return Err(GitError::Execution(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        let directory = exec_git_output(&["rev-parse", "--absolute-git-dir"], cwd).await?;
        if !directory.status.success() {
            return Err(GitError::Execution(
                String::from_utf8_lossy(&directory.stderr).into_owned(),
            ));
        }
        directory.stdout
    };
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    #[cfg(unix)]
    let path = {
        use std::os::unix::ffi::OsStrExt;
        std::path::PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    };
    #[cfg(not(unix))]
    let path = std::path::PathBuf::from(
        std::str::from_utf8(bytes)
            .map_err(|error| GitError::Execution(error.to_string()))?
            .trim_end_matches('\r'),
    );
    if !path.is_absolute() {
        return Err(GitError::Execution(
            "Git returned an invalid execution root".into(),
        ));
    }
    Ok(path)
}

/// Repository context for consumers that retain project analysis. An unborn
/// branch has no commit; a detached HEAD has no symbolic reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryRevision {
    pub commit: Option<String>,
    pub reference: Option<String>,
}

/// Read only revision metadata, without walking source files or refreshing the
/// index. Git resolves worktree/common-dir pointers and packed refs itself.
pub async fn repository_revision(cwd: &Path) -> Result<Option<RepositoryRevision>, GitError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut found = false;
        for ancestor in cwd.ancestors() {
            match tokio::fs::symlink_metadata(ancestor.join(".git")).await {
                Ok(_) => {
                    found = true;
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(GitError::Execution(error.to_string())),
            }
        }
        if !found {
            return Ok(None);
        }
        let (output, error, success) = exec_git(
            &[
                "--no-optional-locks",
                "rev-parse",
                "HEAD",
                "--symbolic-full-name",
                "HEAD",
            ],
            cwd,
        )
        .await?;
        if success {
            let mut lines = output.lines();
            let commit = lines.next().unwrap_or_default();
            let reference = lines.next().unwrap_or_default();
            if !matches!(commit.len(), 40 | 64)
                || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
                || (reference != "HEAD" && !reference.starts_with("refs/"))
                || reference.chars().any(char::is_control)
                || lines.next().is_some()
            {
                return Err(GitError::Execution(
                    "Git returned an invalid repository revision".into(),
                ));
            }
            return Ok(Some(RepositoryRevision {
                commit: Some(commit.into()),
                reference: (reference != "HEAD").then(|| reference.into()),
            }));
        }
        // A new repository's HEAD is a symbolic branch with no ref yet. Do not
        // turn other rev-parse failures (corruption, permissions) into no repo.
        let (reference, _, symbolic) = exec_git(&["symbolic-ref", "--quiet", "HEAD"], cwd).await?;
        let reference = reference.trim_end_matches('\n');
        if symbolic && reference.starts_with("refs/") && !reference.chars().any(char::is_control) {
            let probe =
                exec_git_output(&["show-ref", "--verify", "--quiet", reference], cwd).await?;
            if probe.status.code() == Some(1) {
                return Ok(Some(RepositoryRevision {
                    commit: None,
                    reference: Some(reference.into()),
                }));
            }
        }
        Err(GitError::Execution(format!(
            "Cannot read repository revision: {error}"
        )))
    })
    .await
    .map_err(|_| {
        GitError::Execution("Reading repository revision timed out after two seconds".into())
    })?
}

/// Helper to execute git command with 30s timeout and return untrimmed stdout/stderr.
async fn exec_git(args: &[&str], cwd: &Path) -> Result<(String, String, bool), GitError> {
    let (stdout, stderr, success) = exec_git_bytes(args, cwd).await?;
    Ok((
        String::from_utf8_lossy(&stdout).into_owned(),
        stderr,
        success,
    ))
}

async fn exec_git_bytes(args: &[&str], cwd: &Path) -> Result<(Vec<u8>, String, bool), GitError> {
    let output = exec_git_output(args, cwd).await?;
    Ok((
        output.stdout,
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    ))
}

async fn exec_git_output(args: &[&str], cwd: &Path) -> Result<std::process::Output, GitError> {
    let mut cmd = Command::new("git");
    cmd.args(args);
    cmd.current_dir(cwd);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    // Explicit literal and trusted exclusion pathspecs must not be reinterpreted
    // by the process that launched the bridge. All other host environment stays.
    for name in [
        "GIT_LITERAL_PATHSPECS",
        "GIT_GLOB_PATHSPECS",
        "GIT_NOGLOB_PATHSPECS",
        "GIT_ICASE_PATHSPECS",
    ] {
        cmd.env_remove(name);
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    if std::env::var_os("GIT_SSH_COMMAND").is_none() && std::env::var_os("GIT_SSH").is_none() {
        cmd.env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o StrictHostKeyChecking=yes",
        );
    }

    let child = cmd
        .spawn()
        .map_err(|e| GitError::Execution(format!("failed to spawn git {}: {e}", args.join(" "))))?;
    #[cfg(unix)]
    let pid = child.id().and_then(|id| i32::try_from(id).ok());
    #[cfg(unix)]
    let group = pid.map(crate::exec::proc::GroupGuard::new);

    let result = match timeout(Duration::from_secs(30), child.wait_with_output()).await {
        Ok(Ok(output)) => {
            if output.stdout.len() > 16 * 1024 * 1024 {
                Err(GitError::Execution("git output exceeds 16 MiB".into()))
            } else {
                Ok(output)
            }
        }
        Ok(Err(e)) => {
            #[cfg(unix)]
            if let Some(pgid) = pid {
                crate::exec::proc::terminate_group(pgid, Duration::from_secs(2)).await;
            }
            Err(GitError::Execution(format!("git execution error: {e}")))
        }
        Err(_) => {
            #[cfg(unix)]
            if let Some(pgid) = pid {
                crate::exec::proc::terminate_group(pgid, Duration::from_secs(2)).await;
            }
            Err(GitError::Execution(
                "git command timed out after 30s".into(),
            ))
        }
    };
    #[cfg(unix)]
    if let Some(group) = group {
        group.disarm();
    }
    result
}

/// Workspace-relative index/worktree state for file-tree actions.
pub async fn get_path_changes(
    repo_dir: &Path,
) -> Result<openwebide_core::git::GitPathChanges, GitError> {
    let (prefix, stderr, success) = exec_git(&["rev-parse", "--show-prefix"], repo_dir).await?;
    if !success {
        return Err(GitError::Execution(stderr));
    }
    let prefix = prefix.strip_suffix('\n').unwrap_or(&prefix);
    let (output, stderr, success) = workspace_git(
        repo_dir,
        &["status", "--porcelain=v1", "--untracked-files=all", "-z"],
        &[".".into()],
    )
    .await?;
    if !success {
        return Err(GitError::Execution(stderr));
    }
    let raw = openwebide_core::git::parse_path_changes(&output);
    let relative = |path: String| -> Result<String, GitError> {
        path.strip_prefix(prefix).map(str::to_owned).ok_or_else(|| {
            GitError::Validation(
                "Rename crosses the project root; manage it from the repository root".into(),
            )
        })
    };
    let (_, _, has_head) = exec_git(&["rev-parse", "--verify", "HEAD"], repo_dir).await?;
    Ok(openwebide_core::git::GitPathChanges {
        has_head,
        staged: raw
            .staged
            .into_iter()
            .map(&relative)
            .collect::<Result<_, _>>()?,
        unstaged: raw
            .unstaged
            .into_iter()
            .map(&relative)
            .collect::<Result<_, _>>()?,
        untracked: raw
            .untracked
            .into_iter()
            .map(&relative)
            .collect::<Result<_, _>>()?,
        renamed_from: raw
            .renamed_from
            .into_iter()
            .map(|(to, from)| Ok((relative(to)?, relative(from)?)))
            .collect::<Result<_, GitError>>()?,
    })
}

pub async fn apply_path_action(
    repo_dir: &Path,
    request: &openwebide_core::git::GitPathRequest,
) -> Result<openwebide_core::git::GitPathChanges, GitError> {
    let changes = get_path_changes(repo_dir).await?;
    let args = openwebide_core::git::path_action_args(request, &changes, changes.has_head)
        .map_err(GitError::Validation)?;
    if request.action == openwebide_core::git::GitPathAction::Revert {
        validate_restore(repo_dir, request, &changes).await?;
    }
    let (_, stderr, success) = exec_git(
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
        repo_dir,
    )
    .await?;
    if !success {
        return Err(GitError::Execution(stderr));
    }
    get_path_changes(repo_dir).await
}

/// Validate branch name: reject empty, leading '-', or invalid format according to git check-ref-format.
pub async fn validate_branch(name: &str) -> Result<(), GitError> {
    if name.is_empty() {
        return Err(GitError::Validation("branch name cannot be empty".into()));
    }
    if name.starts_with('-') {
        return Err(GitError::Validation(format!(
            "invalid branch name '{name}': leading '-' not allowed"
        )));
    }

    let mut cmd = Command::new("git");
    cmd.args(["check-ref-format", "--branch", name]);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    cmd.env("GIT_TERMINAL_PROMPT", "0");

    let output = match timeout(Duration::from_secs(5), cmd.output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return Err(GitError::Execution(format!(
                "failed to run git check-ref-format: {e}"
            )));
        }
        Err(_) => return Err(GitError::Execution("git check-ref-format timed out".into())),
    };

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let msg = err.trim();
        return Err(GitError::Validation(if msg.is_empty() {
            format!("invalid branch name '{name}'")
        } else {
            format!("invalid branch name '{name}': {msg}")
        }));
    }

    Ok(())
}

/// Validate remote name: reject empty, leading '-', or non-members of `git remote`.
pub async fn validate_remote(repo: &Path, name: &str) -> Result<(), GitError> {
    if name.is_empty() {
        return Err(GitError::Validation("remote name cannot be empty".into()));
    }
    if name.starts_with('-') {
        return Err(GitError::Validation(format!(
            "invalid remote name '{name}': leading '-' not allowed"
        )));
    }

    let (out, err, ok) = exec_git(&["remote"], repo).await?;
    if !ok {
        return Err(GitError::Execution(format!("git remote failed: {err}")));
    }

    let is_member = out.lines().any(|l| l.trim() == name);
    if !is_member {
        return Err(GitError::Validation(format!("unknown remote '{name}'")));
    }

    Ok(())
}

/// Retrieve full repository status (branch, upstream, ahead/behind, line stats, uncommitted files).
pub async fn get_repo_status(repo_dir: &Path) -> Result<GitRepoStatus, GitError> {
    // 1. Run git status --porcelain=v1 -b -z (NUL-separated, unquoted paths)
    let (status_out, err, ok) = workspace_git(
        repo_dir,
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "-b",
            "-z",
        ],
        &[],
    )
    .await?;
    if !ok {
        return Err(GitError::Execution(format!("git status failed: {err}")));
    }

    let (files, branch, upstream, ahead, behind) = parse_porcelain_v1(&status_out);
    let is_clean = files.is_empty();

    // 2. Fetch last commit info if available
    let (commit_hash, commit_message) =
        match exec_git(&["log", "-1", "--format=%H%x00%s"], repo_dir).await {
            Ok((log_out, _, true)) => {
                let trimmed = log_out.trim_end_matches('\n');
                if !trimmed.is_empty() {
                    if let Some((h, s)) = trimmed.split_once('\0') {
                        (h.trim().to_string(), Some(s.to_string()))
                    } else {
                        (trimmed.trim().to_string(), None)
                    }
                } else {
                    (String::new(), None)
                }
            }
            _ => (String::new(), None),
        };

    // Both browser-local and remote status transports use this one native primitive.
    let file_line_stats =
        match workspace_git(repo_dir, &["diff", "--numstat", "-z", "HEAD"], &[]).await {
            Ok((output, _, true)) => openwebide_core::git::parse_numstat(&output),
            _ => {
                let mut stats = std::collections::HashMap::new();
                for args in [
                    vec!["diff", "--numstat", "-z"],
                    vec!["diff", "--cached", "--numstat", "-z"],
                ] {
                    if let Ok((output, _, true)) = workspace_git(repo_dir, &args, &[]).await {
                        for (path, changes) in openwebide_core::git::parse_numstat(&output) {
                            let entry = stats
                                .entry(path)
                                .or_insert(openwebide_core::GitLineStats::default());
                            entry.insertions += changes.insertions;
                            entry.deletions += changes.deletions;
                        }
                    }
                }
                stats
            }
        };
    let line_stats = openwebide_core::GitLineStats {
        insertions: file_line_stats.values().map(|stats| stats.insertions).sum(),
        deletions: file_line_stats.values().map(|stats| stats.deletions).sum(),
    };

    Ok(GitRepoStatus {
        availability: openwebide_core::git::GitStatusAvailability::Complete,
        branch,
        commit_hash,
        commit_message,
        upstream,
        ahead,
        behind,
        is_clean,
        line_stats,
        file_line_stats,
        files,
    })
}

/// Retrieve repository or file diff against HEAD or working tree.
pub async fn get_repo_diff(repo_dir: &Path, file_path: Option<&str>) -> Result<String, GitError> {
    let mut args = vec!["diff"];
    let has_head = exec_git(&["rev-parse", "--verify", "HEAD"], repo_dir)
        .await
        .map(|(_, _, ok)| ok)
        .unwrap_or(false);

    if has_head {
        args.push("HEAD");
    }

    args.extend(["--no-ext-diff", "--no-textconv"]);
    let paths = file_path
        .map(|path| vec![path.to_owned()])
        .unwrap_or_default();
    let (diff_out, err, ok) = workspace_git(repo_dir, &args, &paths).await?;
    if !ok {
        return Err(GitError::Execution(format!("git diff failed: {err}")));
    }
    Ok(diff_out)
}

/// Retrieve the raw contents of a file at Git HEAD.
pub async fn get_file_at_head(
    repo_dir: &Path,
    file_path: &str,
) -> Result<openwebide_core::GitFileContent, GitError> {
    let clean_path =
        openwebide_core::workspace_entries::entry_path(file_path).map_err(GitError::Validation)?;
    let prefix = history_command(repo_dir, &["rev-parse".into(), "--show-prefix".into()]).await?;
    let clean_path = format!(
        "{}{clean_path}",
        prefix.strip_suffix('\n').unwrap_or(&prefix)
    );
    let (out, err, ok) = exec_git_bytes(&["show", &format!("HEAD:{clean_path}")], repo_dir).await?;
    if !ok {
        return Err(GitError::Execution(format!(
            "git show HEAD:{clean_path} failed: {err}"
        )));
    }
    Ok(match String::from_utf8(out) {
        Ok(content) => openwebide_core::GitFileContent {
            content,
            encoding: Some("utf8".into()),
        },
        Err(error) => openwebide_core::GitFileContent {
            content: base64::engine::general_purpose::STANDARD.encode(error.into_bytes()),
            encoding: Some("base64".into()),
        },
    })
}

/// List local and remote branches.
pub async fn get_repo_branches(repo_dir: &Path) -> Result<Vec<GitBranchInfo>, GitError> {
    let (out, err, ok) = exec_git(
        &[
            "branch",
            "-a",
            "--format=%(refname)%00%(HEAD)%00%(upstream:short)%00%(symref)",
        ],
        repo_dir,
    )
    .await?;

    if !ok {
        return Err(GitError::Execution(format!("git branch failed: {err}")));
    }

    let mut branches = Vec::new();
    for line in out.lines() {
        let parts: Vec<&str> = line.split('\0').collect();
        if parts.is_empty() || parts[0].trim().is_empty() {
            continue;
        }

        let symref = parts.get(3).map(|s| s.trim()).unwrap_or("");
        if !symref.is_empty() {
            continue;
        }

        let refname = parts[0].trim();
        let is_remote = refname.starts_with("refs/remotes/");
        let name = if let Some(stripped) = refname.strip_prefix("refs/heads/") {
            stripped.to_string()
        } else if let Some(stripped) = refname.strip_prefix("refs/remotes/") {
            stripped.to_string()
        } else {
            refname.to_string()
        };

        let is_current = parts.get(1).map(|&s| s.trim() == "*").unwrap_or(false);
        let upstream = parts
            .get(2)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        branches.push(GitBranchInfo {
            name,
            is_current,
            is_remote,
            upstream,
        });
    }

    Ok(branches)
}

/// Create a Git commit on the host.
pub async fn commit_changes(
    repo_dir: &Path,
    req: &GitCommitRequest,
) -> Result<GitCommitResult, GitError> {
    if req.message.trim().is_empty() {
        return Err(GitError::Validation(
            "commit message cannot be empty".into(),
        ));
    }

    if req.staged_only && (req.paths.is_some() || req.include_untracked) {
        return Err(GitError::Validation(
            "A staged commit cannot also stage paths or untracked files".into(),
        ));
    }
    let has_paths = req.paths.as_ref().is_some_and(|paths| !paths.is_empty());
    let paths = req
        .paths
        .clone()
        .filter(|paths| !paths.is_empty())
        .unwrap_or_else(|| vec![".".into()]);
    openwebide_core::git::workspace_pathspecs(&paths).map_err(GitError::Validation)?;
    validate_commit_index(repo_dir, has_paths).await?;
    if !has_paths {
        let prefix =
            history_command(repo_dir, &["rev-parse".into(), "--show-prefix".into()]).await?;
        let prefix = prefix.strip_suffix('\n').unwrap_or(&prefix);
        if !prefix.is_empty() {
            let paths = history_command(
                repo_dir,
                &[
                    "diff".into(),
                    "--cached".into(),
                    "--name-only".into(),
                    "--no-renames".into(),
                    "-z".into(),
                    "--".into(),
                ],
            )
            .await?;
            if paths
                .split('\0')
                .filter(|path| !path.is_empty())
                .any(|path| !path.starts_with(prefix))
            {
                return Err(GitError::Validation("Staged changes outside this project would be included. Open the repository root or unstage them first.".into()));
            }
        }
    }
    if !req.staged_only {
        let mode = if has_paths || req.include_untracked {
            "-A"
        } else {
            "-u"
        };
        let (_, err, ok) = workspace_git(repo_dir, &["add", mode], &paths).await?;
        if !ok {
            return Err(GitError::Execution(format!("git add failed: {err}")));
        }
    }
    // Check again after staging, before invoking hooks or writing a commit.
    validate_commit_index(repo_dir, has_paths).await?;
    let (stdout, stderr, ok) = if has_paths {
        workspace_git(repo_dir, &["commit", "-m", &req.message], &paths).await?
    } else {
        exec_git(&["commit", "-m", &req.message], repo_dir).await?
    };

    if !ok {
        let err_msg = if !stderr.is_empty() {
            stderr
        } else {
            stdout.clone()
        };
        return Err(GitError::Execution(format!("git commit failed: {err_msg}")));
    }

    let (log_out, _, _) = exec_git(&["log", "-1", "--format=%H%x00%s%x00%G?"], repo_dir).await?;
    let mut parts = log_out.split('\0');
    let commit_hash = parts.next().unwrap_or_default().trim().to_string();
    let summary = parts.next().unwrap_or(&req.message).to_string();
    let gpg_flag = parts.next().unwrap_or_default().trim();
    let is_signed = gpg_flag == "G" || gpg_flag == "U";

    Ok(GitCommitResult {
        commit_hash,
        summary,
        pre_commit_output: if stdout.contains("hook") {
            Some(stdout)
        } else {
            None
        },
        is_signed,
    })
}

/// Switch or create a Git branch.
pub async fn checkout_branch(
    repo_dir: &Path,
    req: &GitCheckoutRequest,
) -> Result<GitCheckoutResult, GitError> {
    let target = req.branch.trim();
    validate_branch(target).await?;
    workspace::validate_checkout_tree(repo_dir, target).await?;

    let (prev_branch_raw, _, _) =
        exec_git(&["rev-parse", "--abbrev-ref", "HEAD"], repo_dir).await?;
    let prev_branch = prev_branch_raw.trim();
    let previous_branch = if prev_branch.is_empty() || prev_branch == "HEAD" {
        None
    } else {
        Some(prev_branch.to_string())
    };

    if req.create_if_missing {
        let (_stdout, stderr, ok) = exec_git(
            &[
                "switch",
                "--no-recurse-submodules",
                "--no-overwrite-ignore",
                "-c",
                target,
            ],
            repo_dir,
        )
        .await?;
        if !ok {
            let (_, _, branch_exists) = exec_git(
                &[
                    "show-ref",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{target}"),
                ],
                repo_dir,
            )
            .await?;

            if branch_exists {
                let (_sw_stdout, sw_stderr, sw_ok) = exec_git(
                    &[
                        "switch",
                        "--no-recurse-submodules",
                        "--no-overwrite-ignore",
                        "--no-guess",
                        target,
                    ],
                    repo_dir,
                )
                .await?;
                if !sw_ok {
                    return Err(GitError::Execution(format!(
                        "git switch failed: {}",
                        sw_stderr.trim()
                    )));
                }
            } else {
                return Err(GitError::Execution(format!(
                    "git switch failed: {}",
                    stderr.trim()
                )));
            }
        }
    } else {
        let (_, _, local_exists) = exec_git(
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{target}"),
            ],
            repo_dir,
        )
        .await?;
        let remote_ref = format!("refs/remotes/{target}");
        let (_, _, remote_exists) =
            exec_git(&["show-ref", "--verify", "--quiet", &remote_ref], repo_dir).await?;
        let (_, stderr, ok) = if remote_exists && !local_exists {
            let name = target
                .split_once('/')
                .map(|(_, name)| name)
                .ok_or_else(|| GitError::Validation("Choose a remote branch".into()))?;
            exec_git(
                &[
                    "switch",
                    "--no-recurse-submodules",
                    "--no-overwrite-ignore",
                    "--track",
                    "-c",
                    name,
                    &remote_ref,
                ],
                repo_dir,
            )
            .await?
        } else {
            exec_git(
                &[
                    "switch",
                    "--no-recurse-submodules",
                    "--no-overwrite-ignore",
                    "--no-guess",
                    target,
                ],
                repo_dir,
            )
            .await?
        };
        if !ok {
            return Err(GitError::Execution(format!(
                "git switch failed: {}",
                stderr.trim()
            )));
        }
    }
    let (branch, stderr, ok) = exec_git(&["symbolic-ref", "--short", "HEAD"], repo_dir).await?;
    if !ok {
        return Err(GitError::Execution(stderr));
    }

    Ok(GitCheckoutResult {
        branch: branch.trim().to_string(),
        previous_branch,
        switched: true,
    })
}

/// Keep Git's diagnostic and provide setup guidance without classifying stderr.
fn sync_failure_detail(stderr: &str) -> String {
    format!(
        "{}\nSSH authentication: load the key with ssh-add on the bridge host and forward \
         its agent socket to Docker. Host key verification: verify the fingerprint, update \
         known_hosts and recreate the container with refreshed public configuration. \
         HTTPS authentication: configure a credential helper/token on the bridge.",
        stderr.trim()
    )
}

/// Synchronize with remote (pull, push, or sync).
pub async fn sync_repo(repo_dir: &Path, req: &GitSyncRequest) -> Result<GitSyncResult, GitError> {
    if req.action != "pull" && req.action != "push" && req.action != "sync" && req.action != "fetch"
    {
        return Err(GitError::Validation(format!(
            "invalid sync action '{}': must be 'fetch', 'pull', 'push', or 'sync'",
            req.action
        )));
    }

    let (current_branch_raw, _, _) =
        exec_git(&["rev-parse", "--abbrev-ref", "HEAD"], repo_dir).await?;
    let current_branch = current_branch_raw.trim();
    let (configured_remote, _, _) = exec_git(
        &[
            "config",
            "--get",
            &format!("branch.{current_branch}.remote"),
        ],
        repo_dir,
    )
    .await?;
    let (configured_merge, _, _) = exec_git(
        &["config", "--get", &format!("branch.{current_branch}.merge")],
        repo_dir,
    )
    .await?;
    let configured_remote = configured_remote.trim();
    let remote = req
        .remote
        .as_deref()
        .unwrap_or(if configured_remote.is_empty() {
            "origin"
        } else {
            configured_remote
        });
    let branch = req.branch.as_deref().unwrap_or_else(|| {
        configured_merge
            .trim()
            .strip_prefix("refs/heads/")
            .unwrap_or(current_branch)
    });

    validate_remote(repo_dir, remote).await?;
    if req.action != "fetch" {
        validate_branch(branch).await?;
    }

    let mut output = String::new();
    let mut pulled_commits = 0;
    let mut pushed_commits = 0;

    let append_output = |target: &mut String, out: &str, err: &str| {
        let combined = match (out.trim().is_empty(), err.trim().is_empty()) {
            (false, false) => format!("{}\n{}", out.trim_end(), err.trim_end()),
            (false, true) => out.trim_end().to_string(),
            (true, false) => err.trim_end().to_string(),
            (true, true) => String::new(),
        };
        if !combined.is_empty() {
            if !target.is_empty() {
                target.push('\n');
            }
            target.push_str(&combined);
        }
    };

    if req.action == "fetch" {
        let (out, err, ok) = exec_git(&["fetch", "--prune", "--", remote], repo_dir).await?;
        append_output(&mut output, &out, &err);
        if !ok {
            return Err(GitError::Execution(format!(
                "git fetch failed: {}",
                sync_failure_detail(&err)
            )));
        }
    }
    if req.action == "pull" || req.action == "sync" {
        let (old_head_raw, _, has_head) =
            exec_git(&["rev-parse", "--verify", "HEAD"], repo_dir).await?;
        let old_head = if has_head {
            old_head_raw.trim().to_owned()
        } else {
            String::new()
        };
        let (fetch_out, fetch_err, ok) =
            exec_git(&["fetch", "--", remote, branch], repo_dir).await?;
        append_output(&mut output, &fetch_out, &fetch_err);
        if !ok {
            return Err(GitError::Execution(format!(
                "git pull failed: {}",
                sync_failure_detail(&fetch_err)
            )));
        }
        let upstream = history_command(
            repo_dir,
            &[
                "rev-parse".into(),
                "--verify".into(),
                "FETCH_HEAD^{commit}".into(),
            ],
        )
        .await?;
        let upstream = upstream.trim();
        workspace::validate_pull(repo_dir, upstream, &old_head).await?;
        let (pull_out, pull_err, ok) = if has_head {
            exec_git(
                &[
                    "-c",
                    "submodule.recurse=false",
                    "rebase",
                    "--no-autostash",
                    "--",
                    upstream,
                ],
                repo_dir,
            )
            .await?
        } else {
            exec_git(
                &[
                    "-c",
                    "submodule.recurse=false",
                    "merge",
                    "--ff-only",
                    "--no-autostash",
                    "--no-overwrite-ignore",
                    "--",
                    upstream,
                ],
                repo_dir,
            )
            .await?
        };
        append_output(&mut output, &pull_out, &pull_err);
        if !ok {
            return Err(GitError::Execution(format!(
                "git pull failed: {}",
                sync_failure_detail(&pull_err)
            )));
        }

        let (new_head_raw, _, _) = exec_git(&["rev-parse", "HEAD"], repo_dir).await?;
        let new_head = new_head_raw.trim().to_string();

        if !old_head.is_empty() && !new_head.is_empty() && old_head != new_head {
            let (count_out, _, count_ok) = exec_git(
                &["rev-list", "--count", &format!("{old_head}..{new_head}")],
                repo_dir,
            )
            .await?;
            if count_ok {
                pulled_commits = count_out.trim().parse::<usize>().unwrap_or(0);
            }
        } else if old_head.is_empty() && !new_head.is_empty() {
            let (count_out, _, count_ok) =
                exec_git(&["rev-list", "--count", &new_head], repo_dir).await?;
            if count_ok {
                pulled_commits = count_out.trim().parse::<usize>().unwrap_or(0);
            }
        }
    }

    if req.action == "push" || req.action == "sync" {
        let remote_ref = format!("{remote}/{branch}");
        let sha_before = match exec_git(&["rev-parse", "--verify", &remote_ref], repo_dir).await {
            Ok((sha_out, _, true)) if !sha_out.trim().is_empty() => {
                Some(sha_out.trim().to_string())
            }
            _ => None,
        };

        let refspec = if current_branch == branch {
            branch.to_owned()
        } else {
            format!("HEAD:refs/heads/{branch}")
        };
        let (push_out, push_err, ok) = exec_git(
            &["push", "--set-upstream", "--", remote, &refspec],
            repo_dir,
        )
        .await?;
        append_output(&mut output, &push_out, &push_err);
        if !ok {
            return Err(GitError::Execution(format!(
                "git push failed: {}",
                sync_failure_detail(&push_err)
            )));
        }

        let rev_list_arg = match &sha_before {
            Some(sha) => format!("{sha}..HEAD"),
            None => "HEAD".to_string(),
        };
        let (count_out, _, count_ok) =
            exec_git(&["rev-list", "--count", &rev_list_arg], repo_dir).await?;
        if count_ok {
            pushed_commits = count_out.trim().parse::<usize>().unwrap_or(0);
        }
    }

    Ok(GitSyncResult {
        remote: remote.to_string(),
        branch: branch.to_string(),
        pulled_commits,
        pushed_commits,
        output,
    })
}

async fn history_command(repo: &Path, args: &[String]) -> Result<String, GitError> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let (out, err, ok) = exec_git(&args, repo).await?;
    if ok {
        Ok(out)
    } else {
        Err(GitError::Execution(err))
    }
}

fn validate_history_revision(revision: &str) -> Result<(), GitError> {
    if revision.is_empty()
        || revision.starts_with('-')
        || revision.len() > 256
        || revision.chars().any(char::is_control)
        || revision.contains("..")
        || revision.contains([':', '~', '^', '{', '}', '\\'])
    {
        return Err(GitError::Validation(
            "Select a branch, tag or commit hash".into(),
        ));
    }
    Ok(())
}

async fn resolve_history_commit(repo: &Path, revision: &str) -> Result<String, GitError> {
    validate_history_revision(revision)?;
    Ok(history_command(
        repo,
        &[
            "rev-parse".into(),
            "--verify".into(),
            "--end-of-options".into(),
            format!("{revision}^{{commit}}"),
        ],
    )
    .await?
    .trim()
    .into())
}

pub async fn get_history(
    repo: &Path,
    request: &openwebide_core::git::GitHistoryRequest,
) -> Result<openwebide_core::git::GitHistoryPage, GitError> {
    use openwebide_core::git::*;
    if request.offset >= HISTORY_LIMIT || request.search.len() > 256 {
        return Err(GitError::Validation(
            "History limit reached; narrow the search or select a branch".into(),
        ));
    }
    let refs_output = history_command(
        repo,
        &[
            "for-each-ref".into(),
            "--format=%(refname)%00%(objectname)%00%(*objectname)".into(),
            "refs/heads".into(),
            "refs/remotes".into(),
            "refs/tags".into(),
        ],
    )
    .await?;
    let refs = refs_output
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split('\0').collect();
            if fields.len() != 3 || fields[0].ends_with("/HEAD") {
                return None;
            }
            let (kind, name) = if let Some(name) = fields[0].strip_prefix("refs/heads/") {
                ("branch", name)
            } else if let Some(name) = fields[0].strip_prefix("refs/remotes/") {
                ("remote", name)
            } else {
                ("tag", fields[0].strip_prefix("refs/tags/")?)
            };
            Some(GitHistoryRef {
                name: name.into(),
                hash: if fields[2].is_empty() {
                    fields[1]
                } else {
                    fields[2]
                }
                .into(),
                kind: kind.into(),
            })
        })
        .collect::<Vec<_>>();
    // Unborn repositories have no commits; distinguish that from an invalid project.
    let (_, _, has_head) = exec_git(&["rev-parse", "--verify", "HEAD"], repo).await?;
    if !has_head && refs.is_empty() {
        return Ok(GitHistoryPage::default());
    }
    let file_path = request
        .path
        .as_deref()
        .map(|path| {
            openwebide_core::workspace_entries::entry_path(path).map_err(GitError::Validation)
        })
        .transpose()?;
    let mut args = vec![
        "--literal-pathspecs".into(),
        "log".into(),
        "--topo-order".into(),
        "--date=iso-strict".into(),
        "--format=%x00%H%x00%P%x00%an%x00%ae%x00%aI%x00%cn%x00%ce%x00%cI%x00%s%x00%B%x00".into(),
        format!(
            "--max-count={}",
            if file_path.is_some() {
                HISTORY_LIMIT + 1
            } else {
                HISTORY_PAGE_SIZE + 1
            }
        ),
    ];
    if file_path.is_some() {
        args.extend(["--follow".into(), "--name-status".into(), "-z".into()]);
    } else {
        args.push(format!("--skip={}", request.offset));
        if !request.search.is_empty() {
            args.extend([
                "--regexp-ignore-case".into(),
                "--fixed-strings".into(),
                format!("--grep={}", request.search),
            ]);
        }
    }
    if let Some(reference) = &request.reference {
        args.push(resolve_history_commit(repo, reference).await?);
    } else if file_path.is_some() {
        args.push("HEAD".into());
    } else {
        args.extend(["--all".into(), "HEAD".into()]);
    }
    args.push("--".into());
    if let Some(path) = &file_path {
        args.push(path.clone());
    }
    let out = history_command(repo, &args).await?;
    let fields: Vec<_> = out.split('\0').collect();
    let mut cursor = 0;
    let mut commits = Vec::new();
    let prefix = if file_path.is_some() {
        history_command(repo, &["rev-parse".into(), "--show-prefix".into()])
            .await?
            .trim_end_matches('\n')
            .to_owned()
    } else {
        String::new()
    };
    let mut historical_path = file_path.map(|path| format!("{prefix}{path}"));
    while cursor < fields.len() {
        while cursor < fields.len() && fields[cursor].trim().is_empty() {
            cursor += 1;
        }
        if cursor == fields.len() {
            break;
        }
        let hash = fields[cursor].trim();
        let Some(values) = fields.get(cursor + 1..cursor + 10) else {
            return Err(GitError::Execution(
                "Incomplete Git history response".into(),
            ));
        };
        // --follow may cross from a public file into a former runtime name.
        // Stop at that boundary instead of returning private history paths.
        if historical_path
            .as_deref()
            .is_some_and(openwebide_core::vfs::private_workspace_path)
        {
            break;
        }
        let history_path = historical_path
            .as_ref()
            .and_then(|path| path.strip_prefix(&prefix))
            .map(str::to_owned);
        if historical_path.is_some() && history_path.is_none() {
            break;
        }
        cursor += 10;
        if historical_path.is_some() {
            // Git separates each fixed-width commit record from its NUL-delimited name-status rows.
            if fields.get(cursor).is_some_and(|value| value.is_empty()) {
                cursor += 1;
            }
            while cursor < fields.len() && !fields[cursor].is_empty() {
                let status = fields[cursor].trim();
                cursor += 1;
                let Some(first) = fields.get(cursor) else {
                    return Err(GitError::Execution(
                        "Incomplete file history response".into(),
                    ));
                };
                cursor += 1;
                if status.starts_with('R') || status.starts_with('C') {
                    let Some(second) = fields.get(cursor) else {
                        return Err(GitError::Execution(
                            "Incomplete rename history response".into(),
                        ));
                    };
                    cursor += 1;
                    if status.starts_with('R') && historical_path.as_deref() == Some(*second) {
                        historical_path = Some((*first).into());
                    }
                }
            }
        }
        commits.push(GitHistoryCommit {
            history_path,
            hash: hash.into(),
            parents: values[0].split_whitespace().map(str::to_owned).collect(),
            author: values[1].into(),
            author_email: values[2].into(),
            authored_at: values[3].into(),
            committer: values[4].into(),
            committer_email: values[5].into(),
            committed_at: values[6].into(),
            subject: values[7].into(),
            message: values[8].trim_end().into(),
            refs: refs
                .iter()
                .filter(|reference| reference.hash == hash)
                .map(|reference| format!("{}: {}", reference.kind, reference.name))
                .collect(),
        });
    }
    if request.path.is_some() {
        if !request.search.is_empty() {
            let query = request.search.to_lowercase();
            commits.retain(|commit| commit.message.to_lowercase().contains(&query));
        }
        commits = commits.into_iter().skip(request.offset).collect();
    }
    let has_more =
        commits.len() > HISTORY_PAGE_SIZE && request.offset + HISTORY_PAGE_SIZE < HISTORY_LIMIT;
    commits.truncate(HISTORY_PAGE_SIZE);
    Ok(GitHistoryPage {
        commits,
        has_more,
        refs,
    })
}

pub async fn get_commit_diff(
    repo: &Path,
    request: &openwebide_core::git::GitCommitDiffRequest,
) -> Result<openwebide_core::git::GitCommitDiff, GitError> {
    use openwebide_core::git::*;
    let hash = resolve_history_commit(repo, &request.hash).await?;
    let parents = history_command(
        repo,
        &[
            "show".into(),
            "-s".into(),
            "--format=%P".into(),
            hash.clone(),
            "--".into(),
        ],
    )
    .await?;
    let parent = match &request.parent {
        Some(parent) => {
            let parent = resolve_history_commit(repo, parent).await?;
            if !parents
                .split_whitespace()
                .any(|candidate| candidate == parent)
            {
                return Err(GitError::Validation(
                    "Selected commit is not a parent".into(),
                ));
            }
            Some(parent)
        }
        None => parents.split_whitespace().next().map(str::to_owned),
    };
    let mut base = vec![
        "diff-tree".into(),
        "--root".into(),
        "--no-commit-id".into(),
        "-r".into(),
        "-M".into(),
    ];
    if let Some(parent) = parent {
        base.push(parent);
    }
    base.push(hash);
    let mut names = base.clone();
    names.extend(["--name-status".into(), "-z".into()]);
    let output = workspace_history_command(repo, &names, &[".".into()]).await?;
    let prefix = history_command(repo, &["rev-parse".into(), "--show-prefix".into()]).await?;
    let prefix = prefix.strip_suffix('\n').unwrap_or(&prefix);
    let mut records = output.split('\0').filter(|value| !value.is_empty());
    let mut files = Vec::new();
    while let Some(status) = records.next() {
        let Some(first) = records.next() else {
            break;
        };
        let (previous, path) = if status.starts_with(['R', 'C']) {
            (Some(first), records.next().unwrap_or(first))
        } else {
            (None, first)
        };
        if let Some(path) = path.strip_prefix(prefix) {
            files.push(GitCommitFile {
                path: path.into(),
                status: status.into(),
                previous_path: previous
                    .and_then(|path| path.strip_prefix(prefix))
                    .map(str::to_owned),
            });
        }
    }
    base.extend([
        "--no-ext-diff".into(),
        "--no-textconv".into(),
        "--no-color".into(),
        "--patch".into(),
    ]);
    let mut paths = Vec::new();
    if let Some(path) = &request.path {
        let path =
            openwebide_core::workspace_entries::entry_path(path).map_err(GitError::Validation)?;
        if let Some(previous) = files
            .iter()
            .find(|file| file.path == path)
            .and_then(|file| file.previous_path.as_ref())
        {
            paths.push(previous.clone());
        }
        paths.push(path);
    } else {
        paths.push(".".into());
    }
    let mut diff = workspace_history_command(repo, &base, &paths).await?;
    let truncated = diff.len() > 512 * 1024 || diff.lines().count() > 5000;
    if truncated {
        let line_end = diff
            .match_indices('\n')
            .nth(4999)
            .map_or(diff.len(), |(index, _)| index + 1);
        let mut end = line_end.min(512 * 1024);
        while !diff.is_char_boundary(end) {
            end -= 1;
        }
        diff.truncate(end);
    }
    Ok(GitCommitDiff {
        files,
        diff,
        truncated,
    })
}

/// Read only staged content, scoped to the opened project (including unborn branches).
pub async fn get_index_diff(repo: &Path) -> Result<String, GitError> {
    workspace_git(
        repo,
        &["diff", "--cached", "--no-ext-diff", "--no-textconv"],
        &[".".into()],
    )
    .await
    .and_then(|(out, err, ok)| {
        if ok {
            Ok(out)
        } else {
            Err(GitError::Execution(err))
        }
    })
}

pub async fn manage_stash(
    repo: &Path,
    request: &openwebide_core::git::GitStashRequest,
) -> Result<openwebide_core::git::GitStashResult, GitError> {
    use openwebide_core::git::{GitStash, GitStashAction, GitStashResult};
    let list = |output: String| -> Vec<GitStash> {
        output
            .split('\0')
            .collect::<Vec<_>>()
            .as_chunks::<3>()
            .0
            .iter()
            .map(|fields| GitStash {
                hash: fields[0].trim_start_matches('\n').into(),
                reference: fields[1].into(),
                subject: fields[2].into(),
            })
            .collect()
    };
    let args = vec![
        "stash".into(),
        "list".into(),
        "--format=%H%x00%gd%x00%gs%x00".into(),
    ];
    let mut stashes = list(history_command(repo, &args).await?);
    let mut output = String::new();
    if request.action != GitStashAction::List {
        let prefix = history_command(repo, &["rev-parse".into(), "--show-prefix".into()]).await?;
        if !prefix.trim().is_empty() {
            return Err(GitError::Validation(
                "Open the repository root to manage stashes".into(),
            ));
        }
        let command = match request.action {
            GitStashAction::Save => {
                validate_commit_index(repo, true).await?;
                let changes = get_path_changes(repo).await?;
                validate_restore(
                    repo,
                    &openwebide_core::git::GitPathRequest {
                        path: String::new(),
                        action: openwebide_core::git::GitPathAction::Revert,
                    },
                    &changes,
                )
                .await?;
                let message = request.message.as_deref().unwrap_or("Open WebIDE changes");
                if message.len() > 4096 || message.contains('\0') {
                    return Err(GitError::Validation(
                        "Stash message is too long or invalid".into(),
                    ));
                }
                vec![
                    "stash".into(),
                    "push".into(),
                    "--include-untracked".into(),
                    "-m".into(),
                    message.into(),
                ]
            }
            GitStashAction::Apply | GitStashAction::Drop => {
                let hash = request
                    .hash
                    .as_deref()
                    .ok_or_else(|| GitError::Validation("Choose a stash first".into()))?;
                let stash = stashes
                    .iter()
                    .find(|stash| stash.hash == hash)
                    .ok_or_else(|| {
                        GitError::Validation("This stash no longer exists; refresh the list".into())
                    })?;
                if request.action == GitStashAction::Apply {
                    workspace::validate_stash_apply(repo, &stash.hash).await?;
                    vec![
                        "stash".into(),
                        "apply".into(),
                        "--index".into(),
                        stash.hash.clone(),
                    ]
                } else {
                    vec!["stash".into(), "drop".into(), stash.reference.clone()]
                }
            }
            GitStashAction::List => unreachable!(),
        };
        output = if request.action == GitStashAction::Save {
            workspace_history_command(repo, &command, &[".".into()]).await?
        } else {
            history_command(repo, &command).await?
        };
        stashes = list(history_command(repo, &args).await?);
    }
    Ok(GitStashResult { stashes, output })
}

#[cfg(test)]
mod history_tests {
    use super::*;
    use openwebide_core::git::*;
    #[tokio::test]
    async fn history_merge_details_and_literal_paths() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "History Author"],
            vec!["config", "user.email", "history@example.com"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            assert!(exec_git(&args, repo).await.unwrap().2);
        }
        assert!(
            get_history(repo, &GitHistoryRequest::default())
                .await
                .unwrap()
                .commits
                .is_empty()
        );
        std::fs::write(repo.join("[literal]*.txt"), "first\n").unwrap();
        assert!(exec_git(&["add", "."], repo).await.unwrap().2);
        assert!(
            exec_git(&["commit", "-m", "Root\n\nFull message"], repo)
                .await
                .unwrap()
                .2
        );
        assert!(
            exec_git(&["checkout", "-b", "topic"], repo)
                .await
                .unwrap()
                .2
        );
        std::fs::write(repo.join("topic"), "topic\n").unwrap();
        assert!(exec_git(&["add", "."], repo).await.unwrap().2);
        assert!(exec_git(&["commit", "-m", "Topic"], repo).await.unwrap().2);
        assert!(exec_git(&["checkout", "main"], repo).await.unwrap().2);
        std::fs::write(repo.join("main"), "main\n").unwrap();
        assert!(exec_git(&["add", "."], repo).await.unwrap().2);
        assert!(exec_git(&["commit", "-m", "Main"], repo).await.unwrap().2);
        assert!(
            exec_git(&["merge", "--no-ff", "topic", "-m", "Merge topic"], repo)
                .await
                .unwrap()
                .2
        );
        assert!(exec_git(&["tag", "v1"], repo).await.unwrap().2);
        let page = get_history(repo, &GitHistoryRequest::default())
            .await
            .unwrap();
        assert_eq!(page.commits.len(), 4);
        let merge = &page.commits[0];
        assert_eq!(merge.parents.len(), 2);
        assert!(page.refs.iter().any(|reference| reference.name == "v1"));
        assert!(history_graph(&page.commits)[0].width >= 2);
        for parent in &merge.parents {
            let diff = get_commit_diff(
                repo,
                &GitCommitDiffRequest {
                    hash: merge.hash.clone(),
                    parent: Some(parent.clone()),
                    path: None,
                },
            )
            .await
            .unwrap();
            assert_eq!(diff.files.len(), 1);
            assert!(diff.diff.contains("+"));
        }
        let root = page.commits.last().unwrap();
        assert!(root.message.contains("Full message"));
        let diff = get_commit_diff(
            repo,
            &GitCommitDiffRequest {
                hash: root.hash.clone(),
                parent: None,
                path: Some("[literal]*.txt".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(diff.files[0].path, "[literal]*.txt");
        assert!(diff.diff.contains("+first"));
        let found = get_history(
            repo,
            &GitHistoryRequest {
                search: "full MESSAGE".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(found.commits.len(), 1);
        assert!(
            get_history(
                repo,
                &GitHistoryRequest {
                    reference: Some("--all".into()),
                    ..Default::default()
                }
            )
            .await
            .is_err()
        );
        assert!(
            get_commit_diff(
                repo,
                &GitCommitDiffRequest {
                    hash: merge.hash.clone(),
                    parent: Some(root.hash.clone()),
                    path: None
                }
            )
            .await
            .is_err()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new() -> Self {
            let count = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
            let unique = format!(
                "openwebide-git-test-{}-{}-{}",
                std::process::id(),
                count,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            let path = std::env::temp_dir().join(unique);
            std::fs::create_dir_all(&path).expect("failed to create test temp dir");
            Self { path }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    async fn create_test_repo() -> TestDir {
        let td = TestDir::new();
        let mut cmd = Command::new("git");
        cmd.args(["init", "-b", "main"]);
        cmd.current_dir(&td.path);
        assert!(cmd.status().await.unwrap().success());

        let mut cmd = Command::new("git");
        cmd.args(["config", "user.name", "Test User"]);
        cmd.current_dir(&td.path);
        assert!(cmd.status().await.unwrap().success());

        let mut cmd = Command::new("git");
        cmd.args(["config", "user.email", "test@example.com"]);
        cmd.current_dir(&td.path);
        assert!(cmd.status().await.unwrap().success());

        td
    }

    #[tokio::test]
    async fn status_line_counts_share_unusual_paths_and_unborn_fallback() {
        let td = create_test_repo().await;
        let path = "file\twith\nspace.txt";
        std::fs::write(td.path.join(path), "first\nsecond\n").unwrap();
        let (_, error, ok) = exec_git(&["add", "--", path], &td.path).await.unwrap();
        assert!(ok, "{error}");
        let unborn = get_repo_status(&td.path).await.unwrap();
        assert_eq!(unborn.file_line_stats[path].insertions, 2);
        let (_, error, ok) = exec_git(&["commit", "-m", "Initial"], &td.path)
            .await
            .unwrap();
        assert!(ok, "{error}");
        std::fs::write(td.path.join(path), "first\nreplacement\nthird\n").unwrap();
        let status = get_repo_status(&td.path).await.unwrap();
        assert_eq!(
            status.file_line_stats[path],
            openwebide_core::GitLineStats {
                insertions: 2,
                deletions: 1
            }
        );
        assert_eq!(status.line_stats, status.file_line_stats[path]);
    }

    #[tokio::test]
    async fn path_actions_preserve_literal_names_and_unborn_worktree() {
        use openwebide_core::git::{GitPathAction, GitPathRequest};
        let td = create_test_repo().await;
        std::fs::write(td.path.join("[one]*.txt"), "Original").unwrap();
        std::fs::write(td.path.join("other.txt"), "Other").unwrap();
        let request = |action| GitPathRequest {
            path: "[one]*.txt".into(),
            action,
        };
        let changes = apply_path_action(&td.path, &request(GitPathAction::Stage))
            .await
            .unwrap();
        assert!(changes.staged.contains("[one]*.txt"));
        assert!(changes.untracked.contains("other.txt"));
        assert!(!changes.staged.contains("other.txt"));
        apply_path_action(&td.path, &request(GitPathAction::Unstage))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(td.path.join("[one]*.txt")).unwrap(),
            "Original"
        );
        apply_path_action(&td.path, &request(GitPathAction::Stage))
            .await
            .unwrap();
        assert!(
            exec_git(
                &["-c", "commit.gpgsign=false", "commit", "-m", "Baseline"],
                &td.path
            )
            .await
            .unwrap()
            .2
        );
        std::fs::write(td.path.join("[one]*.txt"), "Changed").unwrap();
        apply_path_action(&td.path, &request(GitPathAction::Stage))
            .await
            .unwrap();
        std::fs::write(td.path.join("[one]*.txt"), "Changed again").unwrap();
        let changes = get_path_changes(&td.path).await.unwrap();
        assert!(changes.has_staged("[one]*.txt"));
        assert!(changes.has_unstaged("[one]*.txt"));
        apply_path_action(&td.path, &request(GitPathAction::Revert))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(td.path.join("[one]*.txt")).unwrap(),
            "Original"
        );
        assert_eq!(
            std::fs::read_to_string(td.path.join("other.txt")).unwrap(),
            "Other"
        );
        assert!(
            apply_path_action(
                &td.path,
                &GitPathRequest {
                    path: "../outside".into(),
                    action: GitPathAction::Stage
                }
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn path_actions_restore_complete_renames_and_scope_subdirectories() {
        use openwebide_core::git::{GitPathAction, GitPathRequest};
        let td = create_test_repo().await;
        std::fs::create_dir(td.path.join("src")).unwrap();
        std::fs::write(td.path.join("src/old"), "Original").unwrap();
        assert!(exec_git(&["add", "--", "src"], &td.path).await.unwrap().2);
        assert!(
            exec_git(
                &["-c", "commit.gpgsign=false", "commit", "-m", "Baseline"],
                &td.path
            )
            .await
            .unwrap()
            .2
        );
        assert!(
            exec_git(&["mv", "--", "src/old", "src/new"], &td.path)
                .await
                .unwrap()
                .2
        );
        let cwd = td.path.join("src");
        let changes = get_path_changes(&cwd).await.unwrap();
        assert_eq!(changes.renamed_from.get("new").unwrap(), "old");
        std::fs::write(cwd.join("old"), "Untracked replacement").unwrap();
        assert!(
            apply_path_action(
                &cwd,
                &GitPathRequest {
                    path: "new".into(),
                    action: GitPathAction::Revert,
                }
            )
            .await
            .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(cwd.join("old")).unwrap(),
            "Untracked replacement"
        );
        assert_eq!(
            std::fs::read_to_string(cwd.join("new")).unwrap(),
            "Original"
        );
        std::fs::remove_file(cwd.join("old")).unwrap();
        apply_path_action(
            &cwd,
            &GitPathRequest {
                path: "new".into(),
                action: GitPathAction::Revert,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(cwd.join("old")).unwrap(),
            "Original"
        );
        assert!(!cwd.join("new").exists());
    }

    #[tokio::test]
    async fn show_preserves_leading_and_trailing_whitespace() {
        let td = create_test_repo().await;
        let file_path = td.path.join("spaced.txt");
        let exact_content = "\n\n  indented\n\n";
        std::fs::write(&file_path, exact_content).unwrap();

        let mut cmd = Command::new("git");
        cmd.args(["add", "spaced.txt"]).current_dir(&td.path);
        assert!(cmd.status().await.unwrap().success());

        let mut cmd = Command::new("git");
        cmd.args(["commit", "-m", "spaced"]).current_dir(&td.path);
        assert!(cmd.status().await.unwrap().success());

        let content = get_file_at_head(&td.path, "spaced.txt").await.unwrap();
        assert_eq!(content.content, exact_content);
        assert_eq!(content.encoding.as_deref(), Some("utf8"));
    }

    #[tokio::test]
    async fn show_text_file_utf8() {
        let td = create_test_repo().await;
        let text = "Hello, 世界!\n";
        std::fs::write(td.path.join("text.txt"), text).unwrap();
        assert!(exec_git(&["add", "text.txt"], &td.path).await.unwrap().2);
        assert!(
            exec_git(&["commit", "-m", "text"], &td.path)
                .await
                .unwrap()
                .2
        );
        let shown = get_file_at_head(&td.path, "text.txt").await.unwrap();
        let response = serde_json::to_value(shown).unwrap();
        assert_eq!(response["encoding"], "utf8");
        assert_eq!(response["content"], text);
    }

    #[tokio::test]
    async fn show_binary_file_base64() {
        let td = create_test_repo().await;
        let bytes = b"\x89PNG\r\n\x1a\n";
        std::fs::write(td.path.join("image.png"), bytes).unwrap();
        assert!(exec_git(&["add", "image.png"], &td.path).await.unwrap().2);
        assert!(
            exec_git(&["commit", "-m", "image"], &td.path)
                .await
                .unwrap()
                .2
        );
        let shown = get_file_at_head(&td.path, "image.png").await.unwrap();
        let response = serde_json::to_value(shown).unwrap();
        assert_eq!(response["encoding"], "base64");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(response["content"].as_str().unwrap())
                .unwrap(),
            bytes
        );
    }

    #[tokio::test]
    async fn checkout_dot_rejected_and_worktree_untouched() {
        let td = create_test_repo().await;
        let work_path = td.path.join("work.txt");
        std::fs::write(&work_path, "important uncommitted work").unwrap();

        let req = GitCheckoutRequest {
            branch: ".".into(),
            create_if_missing: false,
        };
        let res = checkout_branch(&td.path, &req).await;
        assert!(matches!(res, Err(GitError::Validation(_))));

        assert_eq!(
            std::fs::read_to_string(&work_path).unwrap(),
            "important uncommitted work"
        );
    }

    #[tokio::test]
    async fn checkout_existing_branch_with_create_if_missing_switches() {
        let td = create_test_repo().await;
        let file_path = td.path.join("init.txt");
        std::fs::write(&file_path, "init").unwrap();
        let mut cmd = Command::new("git");
        cmd.args(["add", "init.txt"]).current_dir(&td.path);
        assert!(cmd.status().await.unwrap().success());
        let mut cmd = Command::new("git");
        cmd.args(["commit", "-m", "init"]).current_dir(&td.path);
        assert!(cmd.status().await.unwrap().success());

        let req_feat = GitCheckoutRequest {
            branch: "feat".into(),
            create_if_missing: true,
        };
        let res_feat = checkout_branch(&td.path, &req_feat).await.unwrap();
        assert_eq!(res_feat.branch, "feat");

        let req_main = GitCheckoutRequest {
            branch: "main".into(),
            create_if_missing: true,
        };
        let res_main = checkout_branch(&td.path, &req_main).await.unwrap();
        assert_eq!(res_main.branch, "main");
        assert_eq!(res_main.previous_branch, Some("feat".to_string()));
        assert!(res_main.switched);
    }

    #[tokio::test]
    async fn checkout_failure_surfaces_stderr() {
        let td = create_test_repo().await;
        let file_path = td.path.join("init.txt");
        std::fs::write(&file_path, "init").unwrap();
        let mut cmd = Command::new("git");
        cmd.args(["add", "init.txt"]).current_dir(&td.path);
        assert!(cmd.status().await.unwrap().success());
        let mut cmd = Command::new("git");
        cmd.args(["commit", "-m", "init"]).current_dir(&td.path);
        assert!(cmd.status().await.unwrap().success());

        let req = GitCheckoutRequest {
            branch: "nonexistent_branch".into(),
            create_if_missing: false,
        };
        let res = checkout_branch(&td.path, &req).await;
        match res {
            Err(GitError::Execution(stderr)) => {
                assert!(
                    stderr.starts_with("git switch failed:"),
                    "expected 'git switch failed:', got: {stderr}"
                );
                assert!(
                    stderr.contains("nonexistent_branch") || stderr.contains("fatal"),
                    "expected git stderr content in: {stderr}"
                );
            }
            other => panic!("expected Err(GitError::Execution), got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn sync_rejects_option_like_remote() {
        let td = create_test_repo().await;
        let marker = td.path.join("marker_pwned");
        let evil_remote = format!("--upload-pack=touch {};git-upload-pack", marker.display());

        let req = GitSyncRequest {
            action: "pull".into(),
            remote: Some(evil_remote),
            branch: Some("main".into()),
        };
        let res = sync_repo(&td.path, &req).await;
        assert!(matches!(res, Err(GitError::Validation(_))));
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn unknown_remote_rejected() {
        let td = create_test_repo().await;
        let req = GitSyncRequest {
            action: "pull".into(),
            remote: Some("unknown_remote".into()),
            branch: Some("main".into()),
        };
        let res = sync_repo(&td.path, &req).await;
        match res {
            Err(GitError::Validation(msg)) => {
                assert!(msg.contains("unknown remote 'unknown_remote'"));
            }
            other => panic!("expected Err(GitError::Validation), got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dash_branch_rejected() {
        let td = create_test_repo().await;
        let req = GitCheckoutRequest {
            branch: "-b".into(),
            create_if_missing: false,
        };
        let res = checkout_branch(&td.path, &req).await;
        assert!(matches!(res, Err(GitError::Validation(_))));

        let res_val = validate_branch("-main").await;
        assert!(matches!(res_val, Err(GitError::Validation(_))));
    }

    #[tokio::test]
    async fn commit_with_paths_leaves_other_staged_files() {
        let td = create_test_repo().await;
        let f1 = td.path.join("file1.txt");
        let f2 = td.path.join("file2.txt");
        std::fs::write(&f1, "f1 initial").unwrap();
        std::fs::write(&f2, "f2 initial").unwrap();

        let mut cmd = Command::new("git");
        cmd.args(["add", "file1.txt", "file2.txt"])
            .current_dir(&td.path);
        assert!(cmd.status().await.unwrap().success());
        let mut cmd = Command::new("git");
        cmd.args(["commit", "-m", "init"]).current_dir(&td.path);
        assert!(cmd.status().await.unwrap().success());

        std::fs::write(&f1, "f1 modified").unwrap();
        std::fs::write(&f2, "f2 modified").unwrap();

        let mut cmd = Command::new("git");
        cmd.args(["add", "file2.txt"]).current_dir(&td.path);
        assert!(cmd.status().await.unwrap().success());

        let req = GitCommitRequest {
            message: "commit f1 only".into(),
            paths: Some(vec!["file1.txt".into()]),
            include_untracked: false,
            staged_only: false,
        };
        commit_changes(&td.path, &req).await.unwrap();

        let status = get_repo_status(&td.path).await.unwrap();
        assert!(!status.is_clean);
        assert!(status.files.contains_key("file2.txt"));
        assert!(!status.files.contains_key("file1.txt"));
    }

    #[tokio::test]
    async fn commit_without_paths_commits_all_tracked() {
        let td = create_test_repo().await;
        let f1 = td.path.join("file1.txt");
        let f2 = td.path.join("file2.txt");
        std::fs::write(&f1, "f1 initial").unwrap();
        std::fs::write(&f2, "f2 initial").unwrap();

        let mut cmd = Command::new("git");
        cmd.args(["add", "file1.txt", "file2.txt"])
            .current_dir(&td.path);
        assert!(cmd.status().await.unwrap().success());
        let mut cmd = Command::new("git");
        cmd.args(["commit", "-m", "init"]).current_dir(&td.path);
        assert!(cmd.status().await.unwrap().success());

        std::fs::write(&f1, "f1 modified").unwrap();
        std::fs::write(&f2, "f2 modified").unwrap();

        let req = GitCommitRequest {
            message: "commit all tracked".into(),
            paths: None,
            include_untracked: false,
            staged_only: false,
        };
        commit_changes(&td.path, &req).await.unwrap();

        let status = get_repo_status(&td.path).await.unwrap();
        assert!(status.is_clean);
    }

    #[tokio::test]
    async fn push_reports_count() {
        let remote_td = TestDir::new();
        let mut cmd = Command::new("git");
        cmd.args(["init", "-b", "main", "--bare"])
            .current_dir(&remote_td.path);
        assert!(cmd.status().await.unwrap().success());

        let client_td = create_test_repo().await;
        let remote_url = format!("file://{}", remote_td.path.display());
        let mut cmd = Command::new("git");
        cmd.args(["remote", "add", "origin", &remote_url])
            .current_dir(&client_td.path);
        assert!(cmd.status().await.unwrap().success());

        let f = client_td.path.join("test.txt");
        std::fs::write(&f, "c1").unwrap();
        let mut cmd = Command::new("git");
        cmd.args(["add", "test.txt"]).current_dir(&client_td.path);
        assert!(cmd.status().await.unwrap().success());
        let mut cmd = Command::new("git");
        cmd.args(["commit", "-m", "c1"])
            .current_dir(&client_td.path);
        assert!(cmd.status().await.unwrap().success());

        std::fs::write(&f, "c2").unwrap();
        let mut cmd = Command::new("git");
        cmd.args(["commit", "-a", "-m", "c2"])
            .current_dir(&client_td.path);
        assert!(cmd.status().await.unwrap().success());

        let req = GitSyncRequest {
            action: "push".into(),
            remote: Some("origin".into()),
            branch: Some("main".into()),
        };
        let res = sync_repo(&client_td.path, &req).await.unwrap();
        assert_eq!(res.pushed_commits, 2);
        assert!(
            res.output.contains("main -> main"),
            "expected output to contain 'main -> main', got: {}",
            res.output
        );
    }

    #[tokio::test]
    async fn branches_skip_origin_head_symref() {
        let remote_td = TestDir::new();
        let mut cmd = Command::new("git");
        cmd.args(["init", "-b", "main", "--bare"])
            .current_dir(&remote_td.path);
        assert!(cmd.status().await.unwrap().success());

        let client_td = create_test_repo().await;
        let remote_url = format!("file://{}", remote_td.path.display());
        let mut cmd = Command::new("git");
        cmd.args(["remote", "add", "origin", &remote_url])
            .current_dir(&client_td.path);
        assert!(cmd.status().await.unwrap().success());

        let f = client_td.path.join("test.txt");
        std::fs::write(&f, "init").unwrap();
        let mut cmd = Command::new("git");
        cmd.args(["add", "test.txt"]).current_dir(&client_td.path);
        assert!(cmd.status().await.unwrap().success());
        let mut cmd = Command::new("git");
        cmd.args(["commit", "-m", "init"])
            .current_dir(&client_td.path);
        assert!(cmd.status().await.unwrap().success());

        let mut cmd = Command::new("git");
        cmd.args(["push", "-u", "origin", "main"])
            .current_dir(&client_td.path);
        assert!(cmd.status().await.unwrap().success());

        let mut cmd = Command::new("git");
        cmd.args(["remote", "set-head", "origin", "main"])
            .current_dir(&client_td.path);
        assert!(cmd.status().await.unwrap().success());

        let branches = get_repo_branches(&client_td.path).await.unwrap();

        assert!(
            branches.iter().any(|b| b.name == "main" && !b.is_remote),
            "local main branch should be present"
        );
        assert!(
            branches
                .iter()
                .any(|b| b.name == "origin/main" && b.is_remote),
            "remote origin/main branch should be present"
        );
        assert!(
            !branches.iter().any(|b| b.name.contains("HEAD")),
            "origin/HEAD symref should be skipped"
        );
        assert!(
            !branches.iter().any(|b| b.name == "origin"),
            "phantom origin branch should not exist"
        );
    }
    #[tokio::test]
    async fn sync_respects_upstream_and_remote_checkout_preserves_dirty_work() {
        let remote = TestDir::new();
        assert!(
            exec_git(&["init", "--bare", "-b", "trunk"], &remote.path)
                .await
                .unwrap()
                .2
        );
        let repo = create_test_repo().await;
        std::fs::write(repo.path.join("shared.txt"), "baseline\n").unwrap();
        assert!(exec_git(&["add", "."], &repo.path).await.unwrap().2);
        assert!(
            exec_git(
                &["-c", "commit.gpgsign=false", "commit", "-m", "Baseline"],
                &repo.path
            )
            .await
            .unwrap()
            .2
        );
        assert!(
            exec_git(
                &["remote", "add", "upstream", remote.path.to_str().unwrap()],
                &repo.path
            )
            .await
            .unwrap()
            .2
        );
        sync_repo(
            &repo.path,
            &GitSyncRequest {
                action: "push".into(),
                remote: Some("upstream".into()),
                branch: Some("trunk".into()),
            },
        )
        .await
        .unwrap();
        std::fs::write(repo.path.join("shared.txt"), "new main\n").unwrap();
        assert!(
            exec_git(
                &["-c", "commit.gpgsign=false", "commit", "-am", "Update main"],
                &repo.path
            )
            .await
            .unwrap()
            .2
        );
        let pushed = sync_repo(
            &repo.path,
            &GitSyncRequest {
                action: "push".into(),
                remote: None,
                branch: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            (
                pushed.remote.as_str(),
                pushed.branch.as_str(),
                pushed.pushed_commits
            ),
            ("upstream", "trunk", 1)
        );
        assert!(
            exec_git(&["branch", "topic", "trunk"], &remote.path)
                .await
                .unwrap()
                .2
        );
        sync_repo(
            &repo.path,
            &GitSyncRequest {
                action: "fetch".into(),
                remote: None,
                branch: None,
            },
        )
        .await
        .unwrap();
        let checked = checkout_branch(
            &repo.path,
            &GitCheckoutRequest {
                branch: "upstream/topic".into(),
                create_if_missing: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(checked.branch, "topic");
        let branches = get_repo_branches(&repo.path).await.unwrap();
        assert!(branches.iter().any(
            |branch| branch.is_current && branch.upstream.as_deref() == Some("upstream/topic")
        ));
        std::fs::write(repo.path.join("shared.txt"), "topic change\n").unwrap();
        assert!(
            exec_git(
                &[
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "-am",
                    "Topic change"
                ],
                &repo.path
            )
            .await
            .unwrap()
            .2
        );
        checkout_branch(
            &repo.path,
            &GitCheckoutRequest {
                branch: "main".into(),
                create_if_missing: false,
            },
        )
        .await
        .unwrap();
        std::fs::write(repo.path.join("shared.txt"), "precious dirty work\n").unwrap();
        assert!(
            checkout_branch(
                &repo.path,
                &GitCheckoutRequest {
                    branch: "topic".into(),
                    create_if_missing: false
                }
            )
            .await
            .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(repo.path.join("shared.txt")).unwrap(),
            "precious dirty work\n"
        );
    }
    #[tokio::test]
    async fn history_pages_are_bounded_and_large_diffs_are_marked() {
        use openwebide_core::git::*;
        let repo = create_test_repo().await;
        std::fs::write(repo.path.join("large.txt"), "世界\n".repeat(6000)).unwrap();
        assert!(exec_git(&["add", "."], &repo.path).await.unwrap().2);
        assert!(
            exec_git(
                &["-c", "commit.gpgsign=false", "commit", "-m", "Root"],
                &repo.path
            )
            .await
            .unwrap()
            .2
        );
        for index in 0..101 {
            assert!(
                exec_git(
                    &[
                        "-c",
                        "commit.gpgsign=false",
                        "commit",
                        "--allow-empty",
                        "-m",
                        &format!("Revision {index}")
                    ],
                    &repo.path
                )
                .await
                .unwrap()
                .2
            );
        }
        let first = get_history(&repo.path, &GitHistoryRequest::default())
            .await
            .unwrap();
        assert_eq!(first.commits.len(), HISTORY_PAGE_SIZE);
        assert!(first.has_more);
        let second = get_history(
            &repo.path,
            &GitHistoryRequest {
                offset: HISTORY_PAGE_SIZE,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(second.commits.len(), 2);
        assert!(!second.has_more);
        assert!(
            second
                .commits
                .iter()
                .all(|commit| !first.commits.iter().any(|other| other.hash == commit.hash))
        );
        let root = second.commits.last().unwrap();
        assert_eq!(root.subject, "Root");
        let diff = get_commit_diff(
            &repo.path,
            &GitCommitDiffRequest {
                hash: root.hash.clone(),
                parent: None,
                path: None,
            },
        )
        .await
        .unwrap();
        assert!(diff.truncated);
        assert!(diff.diff.lines().count() <= 5000);
        assert!(diff.diff.contains("世界"));
        assert!(
            get_history(
                &repo.path,
                &GitHistoryRequest {
                    offset: HISTORY_LIMIT,
                    ..Default::default()
                }
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn bulk_staging_and_commits_respect_nested_projects_on_unborn_branches() {
        use openwebide_core::git::*;
        let repo = create_test_repo().await;
        let project = repo.path.join("src");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("old.rs"), "fn first() {}\n").unwrap();
        std::fs::write(repo.path.join("outside.txt"), "outside\n").unwrap();
        let staged = apply_path_action(
            &project,
            &GitPathRequest {
                path: "".into(),
                action: GitPathAction::StageAll,
            },
        )
        .await
        .unwrap();
        assert_eq!(staged.staged.len(), 1);
        assert!(!staged.has_head);
        let unstaged = apply_path_action(
            &project,
            &GitPathRequest {
                path: "".into(),
                action: GitPathAction::UnstageAll,
            },
        )
        .await
        .unwrap();
        assert!(unstaged.staged.is_empty());
        assert!(project.join("old.rs").exists());
        apply_path_action(
            &project,
            &GitPathRequest {
                path: "".into(),
                action: GitPathAction::StageAll,
            },
        )
        .await
        .unwrap();
        assert!(
            exec_git(&["add", "outside.txt"], &repo.path)
                .await
                .unwrap()
                .2
        );
        let request = GitCommitRequest {
            message: "only project".into(),
            paths: None,
            include_untracked: false,
            staged_only: true,
        };
        assert!(matches!(
            commit_changes(&project, &request).await,
            Err(GitError::Validation(_))
        ));
        assert!(
            exec_git(&["rm", "--cached", "outside.txt"], &repo.path)
                .await
                .unwrap()
                .2
        );
        commit_changes(&project, &request).await.unwrap();
        assert!(
            exec_git(&["mv", "old.rs", "new.rs"], &project)
                .await
                .unwrap()
                .2
        );
        commit_changes(&project, &request).await.unwrap();
        let history = get_history(
            &project,
            &GitHistoryRequest {
                path: Some("new.rs".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(history.commits.len(), 2);
        assert_eq!(history.commits[0].history_path.as_deref(), Some("new.rs"));
        assert_eq!(history.commits[1].history_path.as_deref(), Some("old.rs"));
        let old = &history.commits[1];
        assert!(
            get_commit_diff(
                &project,
                &GitCommitDiffRequest {
                    hash: old.hash.clone(),
                    path: old.history_path.clone(),
                    parent: None
                }
            )
            .await
            .unwrap()
            .diff
            .contains("+fn first")
        );
    }

    #[tokio::test]
    async fn staged_commit_bulk_and_stash_preserve_index_and_worktree() {
        use openwebide_core::git::*;
        let repo = create_test_repo().await;
        std::fs::write(repo.path.join("tracked.txt"), "base\n").unwrap();
        assert!(exec_git(&["add", "."], &repo.path).await.unwrap().2);
        assert!(
            exec_git(&["commit", "-m", "base"], &repo.path)
                .await
                .unwrap()
                .2
        );
        std::fs::write(repo.path.join("tracked.txt"), "staged\n").unwrap();
        apply_path_action(
            &repo.path,
            &GitPathRequest {
                path: "".into(),
                action: GitPathAction::StageAll,
            },
        )
        .await
        .unwrap();
        std::fs::write(repo.path.join("tracked.txt"), "unstaged\n").unwrap();
        std::fs::write(repo.path.join("[new]*.txt"), "new\n").unwrap();
        let changes = get_path_changes(&repo.path).await.unwrap();
        assert!(changes.staged.contains("tracked.txt") && changes.unstaged.contains("tracked.txt"));
        assert!(
            get_index_diff(&repo.path)
                .await
                .unwrap()
                .contains("+staged")
        );
        commit_changes(
            &repo.path,
            &GitCommitRequest {
                message: "staged only".into(),
                paths: None,
                include_untracked: false,
                staged_only: true,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            exec_git(&["show", "HEAD:tracked.txt"], &repo.path)
                .await
                .unwrap()
                .0,
            "staged\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.path.join("tracked.txt")).unwrap(),
            "unstaged\n"
        );
        let all = apply_path_action(
            &repo.path,
            &GitPathRequest {
                path: "".into(),
                action: GitPathAction::StageAll,
            },
        )
        .await
        .unwrap();
        assert_eq!(all.staged.len(), 2);
        let none = apply_path_action(
            &repo.path,
            &GitPathRequest {
                path: "".into(),
                action: GitPathAction::UnstageAll,
            },
        )
        .await
        .unwrap();
        assert!(none.staged.is_empty());
        apply_path_action(
            &repo.path,
            &GitPathRequest {
                path: "tracked.txt".into(),
                action: GitPathAction::Stage,
            },
        )
        .await
        .unwrap();
        let saved = manage_stash(
            &repo.path,
            &GitStashRequest {
                action: GitStashAction::Save,
                hash: None,
                message: Some("remember".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(saved.stashes.len(), 1);
        assert!(
            get_path_changes(&repo.path)
                .await
                .unwrap()
                .staged
                .is_empty()
        );
        assert!(!repo.path.join("[new]*.txt").exists());
        let hash = saved.stashes[0].hash.clone();
        let applied = manage_stash(
            &repo.path,
            &GitStashRequest {
                action: GitStashAction::Apply,
                hash: Some(hash.clone()),
                message: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(applied.stashes.len(), 1);
        assert!(
            get_path_changes(&repo.path)
                .await
                .unwrap()
                .staged
                .contains("tracked.txt")
        );
        assert!(repo.path.join("[new]*.txt").exists());
        assert!(
            manage_stash(
                &repo.path,
                &GitStashRequest {
                    action: GitStashAction::Drop,
                    hash: Some("missing".into()),
                    message: None
                }
            )
            .await
            .is_err()
        );
        assert!(
            manage_stash(
                &repo.path,
                &GitStashRequest {
                    action: GitStashAction::Drop,
                    hash: Some(hash),
                    message: None
                }
            )
            .await
            .unwrap()
            .stashes
            .is_empty()
        );
    }

    #[tokio::test]
    async fn file_history_tracks_renames_and_selected_branch() {
        use openwebide_core::git::*;
        let repo = create_test_repo().await;
        std::fs::write(repo.path.join("old.txt"), "first\n").unwrap();
        assert!(exec_git(&["add", "."], &repo.path).await.unwrap().2);
        assert!(
            exec_git(&["commit", "-m", "first"], &repo.path)
                .await
                .unwrap()
                .2
        );
        assert!(exec_git(&["branch", "before"], &repo.path).await.unwrap().2);
        assert!(
            exec_git(&["mv", "old.txt", "[new]*.txt"], &repo.path)
                .await
                .unwrap()
                .2
        );
        assert!(
            exec_git(&["commit", "-m", "rename"], &repo.path)
                .await
                .unwrap()
                .2
        );
        std::fs::write(repo.path.join("[new]*.txt"), "second\n").unwrap();
        assert!(
            exec_git(&["commit", "-am", "second"], &repo.path)
                .await
                .unwrap()
                .2
        );
        let history = get_history(
            &repo.path,
            &GitHistoryRequest {
                path: Some("[new]*.txt".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(history.commits.len(), 3);
        assert_eq!(
            history.commits[0].history_path.as_deref(),
            Some("[new]*.txt")
        );
        assert_eq!(history.commits[2].history_path.as_deref(), Some("old.txt"));
        let renamed = &history.commits[1];
        let rename_diff = get_commit_diff(
            &repo.path,
            &GitCommitDiffRequest {
                hash: renamed.hash.clone(),
                parent: None,
                path: renamed.history_path.clone(),
            },
        )
        .await
        .unwrap();
        assert!(rename_diff.diff.contains("rename from old.txt"));
        assert!(!rename_diff.diff.contains("+first"));
        let root = &history.commits[2];
        assert!(
            get_commit_diff(
                &repo.path,
                &GitCommitDiffRequest {
                    hash: root.hash.clone(),
                    parent: None,
                    path: root.history_path.clone()
                }
            )
            .await
            .unwrap()
            .diff
            .contains("+first")
        );
        let filtered = get_history(
            &repo.path,
            &GitHistoryRequest {
                path: Some("[new]*.txt".into()),
                search: "first".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(filtered.commits.len(), 1);
        assert_eq!(filtered.commits[0].history_path.as_deref(), Some("old.txt"));
        let other = get_history(
            &repo.path,
            &GitHistoryRequest {
                path: Some("old.txt".into()),
                reference: Some("refs/heads/before".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(other.commits.len(), 1);
        assert!(
            get_history(
                &repo.path,
                &GitHistoryRequest {
                    path: Some("../secret".into()),
                    ..Default::default()
                }
            )
            .await
            .is_err()
        );
    }
}
