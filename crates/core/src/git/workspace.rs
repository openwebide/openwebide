//! One workspace selection contract for all native Git transports.
use crate::vfs::{private_workspace_globs, private_workspace_path};

/// Explicit literal user paths followed by exact runtime exclusions. An empty
/// selection preserves Git's default scope; `.` selects only the opened project.
pub fn workspace_pathspecs(paths: &[String]) -> Result<Vec<String>, String> {
    let mut specs = paths
        .iter()
        .map(|path| {
            let path = if path == "." {
                path.clone()
            } else {
                crate::workspace_entries::entry_path(path)?
            };
            Ok(format!(":(literal){path}"))
        })
        .collect::<Result<Vec<_>, String>>()?;
    specs.extend(
        private_workspace_globs()
            .into_iter()
            .map(|glob| format!(":(top,exclude,glob,icase){glob}")),
    );
    Ok(specs)
}

/// Trusted positive selectors for checking an existing index before committing.
pub fn private_pathspecs() -> Vec<String> {
    private_workspace_globs()
        .into_iter()
        .map(|glob| format!(":(top,glob,icase){glob}"))
        .collect()
}

/// Keep public sides of cross-boundary renames visible without exposing or
/// subsequently restoring private names. Copies never delete their source.
pub(super) fn public_change<'a>(
    record: &'a str,
    source: Option<&'a str>,
) -> Option<(u8, u8, &'a str, Option<&'a str>)> {
    let bytes = record.as_bytes();
    let path = record.get(3..)?;
    let (mut index, mut work) = (*bytes.first()?, *bytes.get(1)?);
    if private_workspace_path(path) {
        let source = source.filter(|path| !private_workspace_path(path))?;
        if ![index, work].contains(&b'R') {
            return None;
        }
        return Some((
            if index == b'R' { b'D' } else { b' ' },
            if work == b'R' { b'D' } else { b' ' },
            source,
            None,
        ));
    }
    let source = source.filter(|path| !private_workspace_path(path));
    if source.is_none() {
        if matches!(index, b'R' | b'C') {
            index = b'A';
        }
        if matches!(work, b'R' | b'C') {
            work = b'A';
        }
    }
    Some((index, work, path, source))
}

/// No-follow entry kinds needed to prevent a restore from replacing a folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreEntry {
    Missing,
    File,
    Directory,
    Link,
}

pub trait RestoreFiles {
    fn restore_entry(
        &self,
        path: &str,
    ) -> impl std::future::Future<Output = Result<RestoreEntry, String>>;
}

/// A broad Git restore may delete ignored descendants when a tracked file was
/// replaced by a directory. Validate every changed tracked leaf and its parents;
/// adapters supply no-follow observations, never a filtered child inventory.
pub async fn validate_restore(
    files: &impl RestoreFiles,
    request: &super::GitPathRequest,
    changes: &super::GitPathChanges,
) -> Result<(), String> {
    let selection = if request.path.is_empty() {
        vec![String::new()]
    } else {
        changes.action_paths(&request.path)?
    };
    let paths = changes
        .staged
        .iter()
        .chain(&changes.unstaged)
        .chain(changes.renamed_from.values())
        .filter(|path| {
            selection.iter().any(|parent| {
                parent.is_empty() || crate::workspace_entries::contains_path(parent, path)
            })
        })
        .collect::<std::collections::BTreeSet<_>>();
    for path in paths {
        let path = crate::workspace_entries::entry_path(path)?;
        let mut current = String::new();
        let count = path.split('/').count();
        for (index, part) in path.split('/').enumerate() {
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(part);
            let entry = files.restore_entry(&current).await?;
            if entry == RestoreEntry::Missing {
                break;
            }
            if (index + 1 < count && entry != RestoreEntry::Directory)
                || (index + 1 == count && entry == RestoreEntry::Directory)
            {
                return Err("A restored file would replace a directory or an obstructing parent. Move its contents before reverting.".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn private_records_and_cross_boundary_renames_keep_only_public_changes() {
        let private = format!(".openwebide/recovery-{}", "a".repeat(32));
        let status = format!(
            "?? {private}/objects/secret\0R  {private}/incoming\0public-old\0R  public-new\0{private}/outgoing\0C  {private}/copy\0copy-source\0?? .openwebide/recovery-notes\0"
        );
        let changes = parse_path_changes(&status);
        assert_eq!(
            changes.staged.into_iter().collect::<Vec<_>>(),
            ["public-new", "public-old"]
        );
        assert!(changes.renamed_from.is_empty());
        let (files, ..) = parse_porcelain_v1(&status);
        assert_eq!(files.len(), 3);
        assert_eq!(files["public-old"], GitFileStatus::Deleted);
        assert_eq!(files["public-new"], GitFileStatus::Added);
        assert_eq!(
            files[".openwebide/recovery-notes"],
            GitFileStatus::Untracked
        );
        assert!(workspace_pathspecs(&[format!("{private}/objects/secret")]).is_err());
        assert_eq!(
            workspace_pathspecs(&["[x]*".into()]).unwrap()[0],
            ":(literal)[x]*"
        );
    }
}
