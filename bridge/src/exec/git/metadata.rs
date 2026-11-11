//! Read-only Git path discovery, honoring worktree/common-directory routing.
use super::*;
use std::{collections::BTreeSet, path::PathBuf};

pub(crate) struct RepositoryMetadata {
    pub directories: BTreeSet<PathBuf>,
    pub boundaries: BTreeSet<PathBuf>,
    pub files: BTreeSet<PathBuf>,
    pub parents: Vec<PathBuf>,
}

pub(crate) async fn repository_metadata(
    cwd: &Path,
    revision: &RepositoryRevision,
) -> Result<RepositoryMetadata, GitError> {
    timeout(Duration::from_secs(2), async {
        let (format, error, success) = exec_git(
            &["--no-optional-locks", "rev-parse", "--show-ref-format"],
            cwd,
        )
        .await?;
        let references = match (success, format.trim()) {
            (true, "reftable") => Vec::new(),
            // Older supported Git versions pass through this unknown flag.
            (true, "files" | "--show-ref-format") => symbolic_references(cwd, revision).await?,
            _ => {
                return Err(GitError::Execution(format!(
                    "Cannot discover Git reference storage: {error}"
                )));
            }
        };
        let mut args = vec![
            "--no-optional-locks",
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
            "--git-path",
            "HEAD",
            "--git-path",
            "logs/HEAD",
            "--git-path",
            "config",
            "--git-path",
            "config.worktree",
            "--git-path",
            "commondir",
            "--git-path",
            "packed-refs",
            "--git-path",
            "reftable/tables.list",
        ];
        for reference in &references {
            args.extend(["--git-path", reference]);
        }
        let output = exec_git_output(&args, cwd).await?;
        if !output.status.success() {
            return Err(GitError::Execution(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        let bytes = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
        let paths = bytes
            .split(|byte| *byte == b'\n')
            .map(path)
            .collect::<Result<Vec<_>, _>>()?;
        if paths.len() != 9 + references.len() {
            return Err(GitError::Execution(
                "Git returned invalid metadata paths".into(),
            ));
        }
        let directories = paths[..2].iter().cloned().collect::<BTreeSet<_>>();
        let mut files = paths[2..].iter().cloned().collect::<BTreeSet<_>>();
        let mut parents = vec![
            paths[8]
                .parent()
                .ok_or_else(|| GitError::Execution("Invalid Git table path".into()))?
                .to_path_buf(),
        ];
        for reference in &paths[9..] {
            parents.push(
                reference
                    .parent()
                    .ok_or_else(|| GitError::Execution("Invalid Git reference path".into()))?
                    .to_path_buf(),
            );
        }
        for ancestor in cwd.ancestors() {
            let marker = ancestor.join(".git");
            match tokio::fs::symlink_metadata(&marker).await {
                Ok(metadata) => {
                    if !metadata.is_dir() {
                        files.insert(marker);
                    }
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(GitError::Execution(error.to_string())),
            }
        }
        let mut boundaries = directories.clone();
        // Canonical paths from Git may follow intentionally external reference
        // links. Keep those inputs without walking their unrelated ancestors.
        for parent in &parents {
            if !directories.iter().any(|root| parent.starts_with(root)) {
                boundaries.insert(parent.parent().unwrap_or(parent).to_path_buf());
            }
        }
        Ok(RepositoryMetadata {
            directories,
            boundaries,
            files,
            parents,
        })
    })
    .await
    .map_err(|_| GitError::Execution("Reading Git metadata paths timed out".into()))?
}

async fn symbolic_references(
    cwd: &Path,
    revision: &RepositoryRevision,
) -> Result<Vec<String>, GitError> {
    let Some(final_reference) = &revision.reference else {
        return Ok(Vec::new());
    };
    let mut references = Vec::new();
    let mut current = "HEAD".to_string();
    for _ in 0..16 {
        let (reference, _, success) = exec_git(
            &[
                "--no-optional-locks",
                "symbolic-ref",
                "--quiet",
                "--no-recurse",
                &current,
            ],
            cwd,
        )
        .await?;
        let reference = reference.trim_end_matches(['\r', '\n']);
        if !success
            || !reference.starts_with("refs/")
            || reference.len() > 4096
            || reference.chars().any(char::is_control)
            || references.iter().any(|previous| previous == reference)
        {
            return Err(GitError::Execution(
                "Git symbolic references changed or are invalid".into(),
            ));
        }
        references.push(reference.to_string());
        if reference == final_reference {
            return Ok(references);
        }
        current = reference.to_string();
    }
    Err(GitError::Execution(
        "Git symbolic reference chain exceeds its limit".into(),
    ))
}

fn path(bytes: &[u8]) -> Result<PathBuf, GitError> {
    #[cfg(unix)]
    let path = {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    };
    #[cfg(not(unix))]
    let path = PathBuf::from(
        std::str::from_utf8(bytes)
            .map_err(|error| GitError::Execution(error.to_string()))?
            .trim_end_matches('\r'),
    );
    if !path.is_absolute() || bytes.contains(&0) {
        return Err(GitError::Execution(
            "Git returned an invalid metadata path".into(),
        ));
    }
    Ok(path)
}
