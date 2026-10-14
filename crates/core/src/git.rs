//! Git domain models, repository status, commit payloads, and porcelain parsing.

pub mod local;
pub use local::LocalGit;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Git working tree status for a specific file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GitFileStatus {
    Added,
    Modified,
    Deleted,
    Untracked,
    Renamed,
    Conflict,
}

impl GitFileStatus {
    pub const fn description(self) -> &'static str {
        match self {
            Self::Added => "Added",
            Self::Modified => "Modified",
            Self::Deleted => "Deleted",
            Self::Untracked => "Untracked",
            Self::Renamed => "Renamed",
            Self::Conflict => "Conflict",
        }
    }

    /// Return a single-character status badge for the file explorer tree.
    pub fn badge(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Modified => "M",
            Self::Deleted => "D",
            Self::Untracked => "U",
            Self::Renamed => "R",
            Self::Conflict => "!",
        }
    }

    /// Return a CSS class modifier for styling file tree badges.
    pub fn css_class(self) -> &'static str {
        match self {
            Self::Added => "git-badge-added",
            Self::Modified => "git-badge-modified",
            Self::Deleted => "git-badge-deleted",
            Self::Untracked => "git-badge-untracked",
            Self::Renamed => "git-badge-renamed",
            Self::Conflict => "git-badge-conflict",
        }
    }
}

/// Aggregated diff insertion and deletion line statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct GitLineStats {
    pub insertions: usize,
    pub deletions: usize,
}

impl GitLineStats {
    pub fn format_diff(&self) -> String {
        format!("+{} -{}", self.insertions, self.deletions)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitStatusAvailability {
    #[default]
    Complete,
    Passive,
    Unavailable,
}

/// Comprehensive Git repository status returned to the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitRepoStatus {
    pub branch: String,
    pub commit_hash: String,
    pub commit_message: Option<String>,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    #[serde(default)]
    pub availability: GitStatusAvailability,
    pub is_clean: bool,
    pub line_stats: GitLineStats,
    #[serde(default)]
    pub file_line_stats: HashMap<String, GitLineStats>,
    /// Map of workspace-relative paths to file status.
    pub files: HashMap<String, GitFileStatus>,
}

impl Default for GitRepoStatus {
    fn default() -> Self {
        Self {
            branch: "main".into(),
            commit_hash: String::new(),
            commit_message: None,
            upstream: None,
            ahead: 0,
            behind: 0,
            availability: GitStatusAvailability::Complete,
            is_clean: true,
            line_stats: GitLineStats::default(),
            file_line_stats: HashMap::new(),
            files: HashMap::new(),
        }
    }
}

/// Request payload to create a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitRequest {
    pub message: String,
    /// Specific paths to stage and commit, or None to commit all tracked modified/deleted files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
    /// Auto-stage untracked files before committing.
    #[serde(default)]
    pub include_untracked: bool,
    /// Commit the index exactly as staged, without staging worktree edits.
    #[serde(default)]
    pub staged_only: bool,
}

/// Result returned from a commit operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitResult {
    pub commit_hash: String,
    pub summary: String,
    pub pre_commit_output: Option<String>,
    pub is_signed: bool,
}

/// Information about a Git branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitBranchInfo {
    pub name: String,
    pub is_current: bool,
    pub is_remote: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
}

/// Request payload to switch or create a branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCheckoutRequest {
    pub branch: String,
    #[serde(default)]
    pub create_if_missing: bool,
}

/// Result returned from a checkout operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCheckoutResult {
    pub branch: String,
    pub previous_branch: Option<String>,
    pub switched: bool,
}

/// Request payload to synchronize with remote upstream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitSyncRequest {
    /// Action to perform: "fetch", "pull", "push", or "sync" (pull --rebase then push).
    #[serde(default = "default_sync_action")]
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

fn default_sync_action() -> String {
    "sync".into()
}

/// Result returned from remote sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitSyncResult {
    pub remote: String,
    pub branch: String,
    pub pulled_commits: usize,
    pub pushed_commits: usize,
    pub output: String,
}

/// Decode NUL records once, consuming rename/copy source records together.
fn porcelain_records(output: &str) -> impl Iterator<Item = (&str, Option<&str>)> {
    let mut records = output.split('\0');
    std::iter::from_fn(move || {
        let record = records.next()?;
        let bytes = record.as_bytes();
        let source = if bytes.len() >= 4
            && bytes[2] == b' '
            && bytes[..2]
                .iter()
                .any(|status| matches!(status, b'R' | b'C'))
        {
            records.next()
        } else {
            None
        };
        Some((record, source))
    })
}

/// Parse output of `git status --porcelain=v1 -b -z`.
///
/// Records are NUL-separated and paths are unquoted. A rename/copy record is
/// followed by a second record holding the source path. Returns:
/// `(files_map, branch_name, upstream_branch, ahead_count, behind_count)`
pub fn parse_porcelain_v1(
    output: &str,
) -> (
    HashMap<String, GitFileStatus>,
    String,
    Option<String>,
    usize,
    usize,
) {
    let mut files = HashMap::new();
    let mut branch = String::from("HEAD");
    let mut upstream = None;
    let mut ahead = 0;
    let mut behind = 0;

    for (rec, _) in porcelain_records(output) {
        if rec.starts_with("## ") {
            // Branch header: e.g.
            // ## main...origin/main [ahead 1, behind 2]
            // ## feat/test
            // ## No commits yet on main
            // ## HEAD (no branch)
            let header = rec.trim_start_matches('#').trim();
            if let Some(name) = header
                .strip_prefix("No commits yet on ")
                .or_else(|| header.strip_prefix("Initial commit on "))
            {
                branch = name.trim().to_string();
            } else if header == "HEAD (no branch)" {
                branch = "HEAD".to_string();
            } else if let Some((b_part, tracker)) = header.split_once(" [") {
                parse_branch_part(b_part, &mut branch, &mut upstream);
                let track_clean = tracker.trim_end_matches(']');
                parse_ahead_behind(track_clean, &mut ahead, &mut behind);
            } else {
                parse_branch_part(header, &mut branch, &mut upstream);
            }
            continue;
        }

        let bytes = rec.as_bytes();
        if rec.len() < 4 || bytes[2] != b' ' {
            continue;
        }
        let index_status = bytes[0];
        let work_status = bytes[1];
        let path = rec.get(3..).unwrap_or("");

        let status = if matches!(
            (index_status, work_status),
            (b'D' | b'U', b'D') | (b'A' | b'D' | b'U', b'U') | (b'U' | b'A', b'A')
        ) {
            GitFileStatus::Conflict
        } else if index_status == b'?' || work_status == b'?' {
            GitFileStatus::Untracked
        } else if index_status == b'A' {
            GitFileStatus::Added
        } else if index_status == b'D' || work_status == b'D' {
            GitFileStatus::Deleted
        } else if index_status == b'R' {
            GitFileStatus::Renamed
        } else {
            GitFileStatus::Modified
        };

        files.insert(path.to_string(), status);
    }

    (files, branch, upstream, ahead, behind)
}

fn parse_branch_part(b_part: &str, branch: &mut String, upstream: &mut Option<String>) {
    if let Some((b, u)) = b_part.split_once("...") {
        *branch = b.trim().to_string();
        *upstream = Some(u.trim().to_string());
    } else {
        *branch = b_part.trim().to_string();
    }
}

fn parse_ahead_behind(tracker: &str, ahead: &mut usize, behind: &mut usize) {
    for part in tracker.split(',') {
        let p = part.trim();
        if let Some(n) = p.strip_prefix("ahead ") {
            *ahead = n.trim().parse().unwrap_or(0);
        } else if let Some(n) = p.strip_prefix("behind ") {
            *behind = n.trim().parse().unwrap_or(0);
        }
    }
}

/// Parse net line changes from unified diff or diffstat.
pub fn parse_diff_stat(output: &str) -> GitLineStats {
    let mut insertions = 0;
    let mut deletions = 0;

    for line in output.lines() {
        // e.g. " 3 files changed, 24 insertions(+), 5 deletions(-)"
        // or short stat from diff
        if line.contains("changed") {
            for part in line.split(',') {
                let p = part.trim();
                if p.contains("insertion") {
                    if let Some(num_str) = p.split_whitespace().next() {
                        insertions = num_str.parse().unwrap_or(0);
                    }
                } else if p.contains("deletion")
                    && let Some(num_str) = p.split_whitespace().next()
                {
                    deletions = num_str.parse().unwrap_or(0);
                }
            }
        }
    }

    GitLineStats {
        insertions,
        deletions,
    }
}

/// Diff payload shared by bridge HTTP and in-process Git adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitDiff {
    pub diff: String,
}

/// A file's contents at HEAD. Missing encoding preserves legacy UTF-8 responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFileContent {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
}

impl GitFileContent {
    /// Text previews share binary and unknown-encoding behavior across transports.
    pub fn into_text(self) -> Result<String, String> {
        match self.encoding.as_deref().unwrap_or("utf8") {
            "utf8" => Ok(self.content),
            "base64" => Err("binary file".into()),
            encoding => Err(format!("unknown blob encoding: {encoding}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_text_preview_contract() {
        for encoding in [None, Some("utf8")] {
            let value = GitFileContent {
                content: "世界\n".into(),
                encoding: encoding.map(str::to_string),
            };
            assert_eq!(value.into_text().unwrap(), "世界\n");
        }
        let binary = GitFileContent {
            content: "/w==".into(),
            encoding: Some("base64".into()),
        };
        assert_eq!(binary.into_text().unwrap_err(), "binary file");
        let unknown = GitFileContent {
            content: "text".into(),
            encoding: Some("unknown".into()),
        };
        assert!(unknown.into_text().is_err());
    }

    #[test]
    fn test_parse_porcelain_v1_full() {
        // -z output: NUL-separated records, unquoted paths; a rename record
        // is followed by a record holding the source path.
        let output = "## main...origin/main [ahead 2, behind 1]\0 M crates/core/src/lib.rs\0?? new file with space.txt\0A  файл.txt\0 D old_file.rs\0A  staged.rs\0R  new.txt\0old.txt\0DD both_deleted.rs\0Mbad\0";
        let (files, branch, upstream, ahead, behind) = parse_porcelain_v1(output);

        assert_eq!(branch, "main");
        assert_eq!(upstream, Some("origin/main".to_string()));
        assert_eq!(ahead, 2);
        assert_eq!(behind, 1);

        assert_eq!(
            files.get("crates/core/src/lib.rs"),
            Some(&GitFileStatus::Modified)
        );
        assert_eq!(
            files.get("new file with space.txt"),
            Some(&GitFileStatus::Untracked)
        );
        assert_eq!(files.get("файл.txt"), Some(&GitFileStatus::Added));
        assert_eq!(files.get("old_file.rs"), Some(&GitFileStatus::Deleted));
        assert_eq!(files.get("staged.rs"), Some(&GitFileStatus::Added));
        assert_eq!(files.get("new.txt"), Some(&GitFileStatus::Renamed));
        assert!(
            !files.contains_key("old.txt"),
            "rename source must be consumed"
        );
        assert_eq!(files.get("both_deleted.rs"), Some(&GitFileStatus::Conflict));
        assert!(
            !files.contains_key("Mbad"),
            "malformed record must be skipped"
        );
    }

    #[test]
    fn test_parse_porcelain_v1_clean() {
        let output = "## feat/awesome\0";
        let (files, branch, upstream, ahead, behind) = parse_porcelain_v1(output);

        assert_eq!(branch, "feat/awesome");
        assert_eq!(upstream, None);
        assert_eq!(ahead, 0);
        assert_eq!(behind, 0);
        assert!(files.is_empty());

        // Unborn-branch headers carry the branch name after a prefix.
        let (files, branch, ..) = parse_porcelain_v1("## No commits yet on main\0");
        assert_eq!(branch, "main");
        assert!(files.is_empty());

        let (files, branch, ..) = parse_porcelain_v1("## Initial commit on main\0");
        assert_eq!(branch, "main");
        assert!(files.is_empty());

        // Detached HEAD reports the literal branch "HEAD".
        let (files, branch, ..) = parse_porcelain_v1("## HEAD (no branch)\0");
        assert_eq!(branch, "HEAD");
        assert!(files.is_empty());
    }

    #[test]
    fn test_parse_diff_stat() {
        let output = " 2 files changed, 42 insertions(+), 12 deletions(-)\n";
        let stats = parse_diff_stat(output);
        assert_eq!(stats.insertions, 42);
        assert_eq!(stats.deletions, 12);
        assert_eq!(stats.format_diff(), "+42 -12");
    }
}

/// File-tree Git mutations; each request names one literal workspace path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitPathAction {
    Stage,
    Unstage,
    Revert,
    StageAll,
    UnstageAll,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPathRequest {
    pub path: String,
    pub action: GitPathAction,
}

/// Index and working-tree changes remain separate so menus can offer both
/// Stage and Unstage when a staged file has been edited again.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GitPathChanges {
    #[serde(default)]
    pub has_head: bool,
    pub staged: std::collections::BTreeSet<String>,
    pub unstaged: std::collections::BTreeSet<String>,
    pub untracked: std::collections::BTreeSet<String>,
    pub renamed_from: HashMap<String, String>,
}

impl GitPathChanges {
    pub fn has_staged(&self, path: &str) -> bool {
        self.staged
            .iter()
            .any(|entry| crate::workspace_entries::contains_path(path, entry))
    }
    pub fn has_unstaged(&self, path: &str) -> bool {
        self.unstaged
            .iter()
            .chain(&self.untracked)
            .any(|entry| crate::workspace_entries::contains_path(path, entry))
    }
    pub fn has_tracked_changes(&self, path: &str) -> bool {
        self.staged
            .iter()
            .chain(&self.unstaged)
            .any(|entry| crate::workspace_entries::contains_path(path, entry))
    }
    /// Include a renamed entry's original name to unstage/revert the whole
    /// rename rather than leaving its staged deletion behind.
    pub fn action_paths(&self, path: &str) -> Result<Vec<String>, String> {
        let path = crate::workspace_entries::entry_path(path)?;
        let mut paths = vec![path.clone()];
        for (destination, source) in &self.renamed_from {
            if crate::workspace_entries::contains_path(&path, destination) {
                paths.push(crate::workspace_entries::entry_path(source)?);
            }
        }
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

/// Parse the same NUL-delimited porcelain records as repository status,
/// retaining the two status columns and rename source names.
pub fn parse_path_changes(output: &str) -> GitPathChanges {
    let mut changes = GitPathChanges::default();
    for (record, source) in porcelain_records(output) {
        if record.starts_with("## ") {
            continue;
        }
        let bytes = record.as_bytes();
        if bytes.len() < 4 || bytes[2] != b' ' {
            continue;
        }
        let path = &record[3..];
        if bytes[0] == b'?' && bytes[1] == b'?' {
            changes.untracked.insert(path.into());
        } else {
            if bytes[0] != b' ' {
                changes.staged.insert(path.into());
            }
            if bytes[1] != b' ' {
                changes.unstaged.insert(path.into());
            }
        }
        // Copies do not delete their source and must not revert it.
        if bytes[..2].contains(&b'R')
            && let Some(source) = source
        {
            changes.renamed_from.insert(path.into(), source.into());
        }
    }
    changes
}

/// Domain validation and command choice shared by every Git transport.
/// The execution adapter must set GIT_LITERAL_PATHSPECS=1.
pub fn path_action_args(
    request: &GitPathRequest,
    changes: &GitPathChanges,
    has_head: bool,
) -> Result<Vec<String>, String> {
    if matches!(
        request.action,
        GitPathAction::StageAll | GitPathAction::UnstageAll
    ) {
        if !request.path.is_empty() {
            return Err("Bulk Git actions must use the project root".into());
        }
        let args = match request.action {
            GitPathAction::StageAll
                if !changes.unstaged.is_empty() || !changes.untracked.is_empty() =>
            {
                vec!["add", "-A", "--", "."]
            }
            GitPathAction::UnstageAll if !changes.staged.is_empty() && has_head => {
                vec!["reset", "HEAD", "--", "."]
            }
            GitPathAction::UnstageAll if !changes.staged.is_empty() => {
                vec!["rm", "-r", "-f", "--cached", "--ignore-unmatch", "--", "."]
            }
            _ => return Err("There are no changes for this Git action".into()),
        };
        return Ok(args.into_iter().map(str::to_owned).collect());
    }
    let path = crate::workspace_entries::entry_path(&request.path)?;
    let paths = changes.action_paths(&path)?;
    if request.action == GitPathAction::Revert
        && changes.untracked.iter().any(|untracked| {
            changes
                .staged
                .iter()
                .chain(&changes.unstaged)
                .chain(changes.renamed_from.values())
                .filter(|tracked| {
                    paths
                        .iter()
                        .any(|path| crate::workspace_entries::contains_path(path, tracked))
                })
                .any(|tracked| {
                    crate::workspace_entries::contains_path(tracked, untracked)
                        || crate::workspace_entries::contains_path(untracked, tracked)
                })
        })
    {
        return Err("Move untracked files out of the paths being restored before reverting".into());
    }
    let args = match request.action {
        GitPathAction::Stage if changes.has_unstaged(&path) => vec!["add", "-A", "--"],
        GitPathAction::Unstage if changes.has_staged(&path) => {
            if has_head {
                vec!["reset", "HEAD", "--"]
            } else {
                vec!["rm", "-r", "--cached", "--ignore-unmatch", "--"]
            }
        }
        GitPathAction::Revert if has_head && changes.has_tracked_changes(&path) => {
            vec!["restore", "--source=HEAD", "--staged", "--worktree", "--"]
        }
        _ => return Err("This Git action is not available for the selected path".into()),
    };
    let mut args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
    args.extend(paths);
    Ok(args)
}

#[cfg(test)]
mod path_tests {
    use super::*;
    #[test]
    fn distinguishes_index_and_worktree_and_preserves_literal_names() {
        let changes = parse_path_changes(
            "## main\0MM src/a b\0?? src/[x]*\0R  new\0old\0C  copied\0source\0",
        );
        assert!(changes.has_staged("src"));
        assert!(changes.has_unstaged("src"));
        assert!(!changes.has_unstaged("sr"));
        assert_eq!(changes.action_paths("new").unwrap(), ["new", "old"]);
        assert_eq!(changes.action_paths("copied").unwrap(), ["copied"]);
        let args = path_action_args(
            &GitPathRequest {
                path: "src/[x]*".into(),
                action: GitPathAction::Stage,
            },
            &changes,
            true,
        )
        .unwrap();
        assert_eq!(args, ["add", "-A", "--", "src/[x]*"]);
    }
    #[test]
    fn unborn_unstage_keeps_worktree_and_revert_never_cleans_untracked() {
        let changes = parse_path_changes("A  new\0?? scratch\0");
        let request = GitPathRequest {
            path: "new".into(),
            action: GitPathAction::Unstage,
        };
        assert_eq!(
            path_action_args(&request, &changes, false).unwrap(),
            ["rm", "-r", "--cached", "--ignore-unmatch", "--", "new"]
        );
        for (path, has_head) in [("new", false), ("scratch", true)] {
            assert!(
                path_action_args(
                    &GitPathRequest {
                        path: path.into(),
                        action: GitPathAction::Revert
                    },
                    &changes,
                    has_head
                )
                .is_err()
            );
        }
        assert!(changes.action_paths("../outside").is_err());
    }
}

/// Parse NUL-delimited Git numstat output, including renamed and unusual paths.
pub fn parse_numstat(output: &str) -> HashMap<String, GitLineStats> {
    let mut stats = HashMap::new();
    let mut records = output.split('\0');
    while let Some(record) = records.next() {
        let mut parts = record.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let path = if path.is_empty() {
            let _old = records.next();
            let Some(new) = records.next() else {
                break;
            };
            new
        } else {
            path
        };
        let (Ok(insertions), Ok(deletions)) = (added.parse(), removed.parse()) else {
            continue;
        };
        stats.insert(
            path.to_string(),
            GitLineStats {
                insertions,
                deletions,
            },
        );
    }
    stats
}

#[cfg(test)]
mod numstat_tests {
    #[test]
    fn preserves_tabs_newlines_renames_and_binary_files() {
        let stats = super::parse_numstat("2\t1\tfile\tname\n.rs\0");
        assert_eq!(stats["file\tname\n.rs"].insertions, 2);
        let stats = super::parse_numstat("3\t4\t\0old\0new\0-\t-\timage.png\0");
        assert_eq!(stats["new"].deletions, 4);
        assert!(!stats.contains_key("old"));
        assert!(!stats.contains_key("image.png"));
    }
}

/// A bounded history query. Revisions are literal refs or full object IDs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHistoryRequest {
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub search: String,
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

pub const HISTORY_PAGE_SIZE: usize = 100;
pub const HISTORY_LIMIT: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHistoryCommit {
    /// Path at this revision, following renames for a file history query.
    #[serde(default)]
    pub history_path: Option<String>,
    pub hash: String,
    pub parents: Vec<String>,
    pub author: String,
    pub author_email: String,
    pub authored_at: String,
    pub committer: String,
    pub committer_email: String,
    pub committed_at: String,
    pub subject: String,
    pub message: String,
    pub refs: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHistoryPage {
    pub commits: Vec<GitHistoryCommit>,
    pub has_more: bool,
    pub refs: Vec<GitHistoryRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHistoryRef {
    pub name: String,
    pub hash: String,
    pub kind: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitDiffRequest {
    pub hash: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitFile {
    pub path: String,
    pub status: String,
    pub previous_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitDiff {
    #[serde(default)]
    pub truncated: bool,
    pub files: Vec<GitCommitFile>,
    pub diff: String,
}

/// Topology segments carry a stable color through merges and lane compaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitGraphEdge {
    pub from: usize,
    pub to: usize,
    pub color: usize,
    pub through_commit: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitGraphRow {
    pub lane: usize,
    pub color: usize,
    pub incoming: bool,
    pub edges: Vec<GitGraphEdge>,
    pub width: usize,
}

fn next_graph_color(lanes: &[(String, usize)], next_color: &mut usize) -> usize {
    let color = (0..6)
        .map(|offset| (*next_color + offset) % 6)
        .find(|candidate| lanes.iter().all(|(_, color)| color != candidate))
        .unwrap_or(*next_color);
    *next_color = (color + 1) % 6;
    color
}

pub fn history_graph(commits: &[GitHistoryCommit]) -> Vec<GitGraphRow> {
    let mut lanes: Vec<(String, usize)> = Vec::new();
    let mut next_color = 0;
    commits
        .iter()
        .map(|commit| {
            let existing = lanes.iter().position(|(hash, _)| hash == &commit.hash);
            let lane = existing.unwrap_or_else(|| {
                let color = next_graph_color(&lanes, &mut next_color);
                lanes.push((commit.hash.clone(), color));
                lanes.len() - 1
            });
            let color = lanes[lane].1;
            let before = lanes.clone();
            lanes.remove(lane);
            for (index, parent) in commit.parents.iter().enumerate() {
                if !lanes.iter().any(|(hash, _)| hash == parent) {
                    let parent_color = if index == 0 {
                        color
                    } else {
                        next_graph_color(&lanes, &mut next_color)
                    };
                    lanes.insert(
                        (lane + index).min(lanes.len()),
                        (parent.clone(), parent_color),
                    );
                }
            }
            let mut edges = Vec::new();
            for (from, (hash, color)) in before.iter().enumerate() {
                if from != lane
                    && let Some(to) = lanes.iter().position(|(next, _)| next == hash)
                {
                    edges.push(GitGraphEdge {
                        from,
                        to,
                        color: *color,
                        through_commit: false,
                    });
                }
            }
            for parent in &commit.parents {
                if let Some(to) = lanes.iter().position(|(next, _)| next == parent) {
                    edges.push(GitGraphEdge {
                        from: lane,
                        to,
                        color: lanes[to].1,
                        through_commit: true,
                    });
                }
            }
            GitGraphRow {
                lane,
                color,
                incoming: existing.is_some(),
                edges,
                width: before.len().max(lanes.len()),
            }
        })
        .collect()
}

/// Repository stashes are addressed by commit hash so stale lists cannot select another stash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitStashAction {
    List,
    Save,
    Apply,
    Drop,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStashRequest {
    pub action: GitStashAction,
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStash {
    pub hash: String,
    pub reference: String,
    pub subject: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GitStashResult {
    pub stashes: Vec<GitStash>,
    pub output: String,
}

#[cfg(test)]
mod history_graph_tests {
    use super::*;
    fn commit(hash: &str, parents: &[&str]) -> GitHistoryCommit {
        GitHistoryCommit {
            history_path: None,
            hash: hash.into(),
            parents: parents.iter().map(|parent| (*parent).into()).collect(),
            author: String::new(),
            author_email: String::new(),
            authored_at: String::new(),
            committer: String::new(),
            committer_email: String::new(),
            committed_at: String::new(),
            subject: String::new(),
            message: String::new(),
            refs: Vec::new(),
        }
    }
    #[test]
    fn merge_lanes_keep_unrelated_tracks_and_colors_through_compaction() {
        let commits = [
            commit("merge", &["main", "topic"]),
            commit("main", &["root"]),
            commit("topic", &["root"]),
            commit("root", &[]),
        ];
        let graph = history_graph(&commits);
        assert_eq!(graph[0].width, 2);
        assert_ne!(graph[0].color, graph[2].color);
        let topic = graph[1]
            .edges
            .iter()
            .find(|edge| !edge.through_commit)
            .unwrap();
        assert_eq!((topic.from, topic.to), (1, 1));
        assert_eq!(topic.color, graph[2].color);
        assert!(graph[3].incoming);
        assert!(graph[3].edges.is_empty());
        assert_eq!(history_graph(&commits[..2]), graph[..2]);
    }
}

#[cfg(test)]
mod complex_history_graph_tests {
    use super::*;

    fn verify_history(nodes: &[(&str, &[&str])]) {
        let commits = nodes
            .iter()
            .map(|(hash, parents)| GitHistoryCommit {
                history_path: None,
                hash: (*hash).into(),
                parents: parents.iter().map(|hash| (*hash).into()).collect(),
                author: String::new(),
                author_email: String::new(),
                authored_at: String::new(),
                committer: String::new(),
                committer_email: String::new(),
                committed_at: String::new(),
                subject: String::new(),
                message: String::new(),
                refs: Vec::new(),
            })
            .collect::<Vec<_>>();
        let graph = history_graph(&commits);
        for prefix in 1..=commits.len() {
            assert_eq!(
                history_graph(&commits[..prefix]),
                graph[..prefix],
                "paging must not move existing tracks"
            );
        }
        for row in &graph {
            let active = row
                .edges
                .iter()
                .map(|edge| (edge.to, edge.color))
                .collect::<std::collections::BTreeMap<_, _>>();
            if active.len() <= 6 {
                let colors = active.values().collect::<std::collections::BTreeSet<_>>();
                assert_eq!(
                    colors.len(),
                    active.len(),
                    "active tracks must use distinct available colors"
                );
            }
        }
        for (index, commit) in commits.iter().enumerate() {
            let row = &graph[index];
            assert!(row.lane < row.width);
            assert_eq!(
                row.edges.iter().filter(|edge| edge.through_commit).count(),
                commit.parents.len()
            );
            for (parent, edge) in commit
                .parents
                .iter()
                .zip(row.edges.iter().filter(|edge| edge.through_commit))
            {
                assert_eq!(edge.from, row.lane);
                let mut lane = edge.to;
                let color = edge.color;
                let parent_index = commits
                    .iter()
                    .position(|commit| &commit.hash == parent)
                    .unwrap();
                assert!(parent_index > index);
                for intermediate in &graph[index + 1..parent_index] {
                    assert_ne!(
                        intermediate.lane, lane,
                        "an unrelated commit must not consume this track"
                    );
                    let bypass = intermediate
                        .edges
                        .iter()
                        .find(|edge| !edge.through_commit && edge.from == lane)
                        .expect("parent track must continue through every intervening row");
                    assert_eq!(
                        bypass.color, color,
                        "compaction must preserve a track's color"
                    );
                    lane = bypass.to;
                }
                assert_eq!(
                    graph[parent_index].lane, lane,
                    "track must end at its actual parent"
                );
                assert_eq!(graph[parent_index].color, color);
                assert!(graph[parent_index].incoming);
            }
        }
    }

    #[test]
    fn octopus_merge_preserves_parallel_tracks_until_their_shared_ancestor() {
        verify_history(&[
            ("octopus", &["main", "topic-a", "topic-b", "topic-c"]),
            ("main", &["root"]),
            ("topic-a", &["topic-a-base"]),
            ("topic-b", &["root"]),
            ("topic-c", &["root"]),
            ("topic-a-base", &["root"]),
            ("root", &[]),
        ]);
    }

    #[test]
    fn crisscross_merges_and_unrelated_roots_keep_parent_connections() {
        verify_history(&[
            ("tip", &["merge-a", "merge-b"]),
            ("unrelated", &[]),
            ("merge-a", &["a", "b"]),
            ("merge-b", &["b", "a"]),
            ("a", &["root"]),
            ("b", &["root"]),
            ("root", &[]),
        ]);
    }

    #[test]
    fn colors_avoid_long_running_tracks_after_closed_branches() {
        verify_history(&[
            ("old-tip", &["root"]),
            ("side-a", &[]),
            ("side-b", &[]),
            ("side-c", &[]),
            ("side-d", &[]),
            ("side-e", &[]),
            ("octopus", &["a", "b", "c", "d"]),
            ("a", &["base"]),
            ("b", &["base"]),
            ("c", &["base"]),
            ("d", &["base"]),
            ("base", &["root"]),
            ("root", &[]),
        ]);
    }

    #[test]
    fn squash_and_rebase_show_only_the_committed_ancestry() {
        verify_history(&[
            ("squash", &["main"]),
            ("original-topic", &["original-base"]),
            ("main", &["root"]),
            ("original-base", &["root"]),
            ("root", &[]),
        ]);
        verify_history(&[
            ("rebased-tip", &["rebased-base"]),
            ("rebased-base", &["updated-main"]),
            ("old-tip", &["old-base"]),
            ("updated-main", &["root"]),
            ("old-base", &["root"]),
            ("root", &[]),
        ]);
    }
}
