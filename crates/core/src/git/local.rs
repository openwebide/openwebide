//! In-browser local Git repository engine running over any [`Vfs`].
//!
//! Provides read-only and local working-tree Git operations (status, history,
//! commit inspection, diffs, branches, checkout, commit, revert) directly
//! against `.git` on disk or through browser File System Access handles,
//! without requiring an external bridge daemon.

use std::collections::{BTreeSet, HashMap};
use std::io::Read;

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use sha1::{Digest, Sha1};

use crate::diff::{line_lcs, LineOp};
use crate::git::{
    GitBranchInfo, GitCheckoutRequest, GitCheckoutResult, GitCommitDiff, GitCommitDiffRequest,
    GitCommitFile, GitCommitRequest, GitCommitResult, GitFileStatus, GitHistoryCommit,
    GitHistoryPage, GitHistoryRef, GitHistoryRequest, GitLineStats, GitPathAction, GitPathChanges,
    GitPathRequest, GitRepoStatus, GitStashRequest, GitStashResult, GitStatusAvailability,
    GitSyncRequest, GitSyncResult, HISTORY_PAGE_SIZE,
};
use crate::vfs::{Vfs, VfsEntryKind};

/// Object types recognized in the Git object database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitObjectType {
    Commit,
    Tree,
    Blob,
    Tag,
}

impl GitObjectType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Tree => "tree",
            Self::Blob => "blob",
            Self::Tag => "tag",
        }
    }

    pub fn from_id(id: u8) -> Option<Self> {
        match id {
            1 => Some(Self::Commit),
            2 => Some(Self::Tree),
            3 => Some(Self::Blob),
            4 => Some(Self::Tag),
            _ => None,
        }
    }
}

/// A parsed entry from a Git tree object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub mode: String,
    pub name: String,
    pub hash: String,
    pub is_dir: bool,
}

/// A parsed commit object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommit {
    pub hash: String,
    pub tree_hash: String,
    pub parents: Vec<String>,
    pub author: String,
    pub author_email: String,
    pub authored_at: String,
    pub committer: String,
    pub committer_email: String,
    pub committed_at: String,
    pub subject: String,
    pub message: String,
}

/// A Git repository engine operating over a [`Vfs`] root.
pub struct LocalGit<V> {
    vfs: V,
}

impl<V: Vfs> LocalGit<V> {
    pub fn new(vfs: V) -> Self {
        Self { vfs }
    }

    pub fn vfs(&self) -> &V {
        &self.vfs
    }

    /// Check if `.git/HEAD` exists in this workspace.
    pub async fn is_repo(&self) -> bool {
        self.vfs.read(".git/HEAD").await.is_ok()
    }

    /// Read the current `HEAD` reference or commit hash.
    /// Returns `(ref_path_or_hash, current_branch_name)`.
    pub async fn read_head(&self) -> Result<(String, Option<String>), String> {
        let content = self
            .vfs
            .read(".git/HEAD")
            .await
            .map_err(|e| format!("cannot read .git/HEAD: {e}"))?;
        let line = content.lines().next().unwrap_or("").trim();
        if let Some(branch) = line.strip_prefix("ref: refs/heads/") {
            Ok((line.to_string(), Some(branch.trim().to_string())))
        } else if let Some(stripped) = line.strip_prefix("ref: ") {
            Ok((stripped.trim().to_string(), None))
        } else if line.len() == 40 && line.chars().all(|c| c.is_ascii_hexdigit()) {
            Ok((line.to_string(), None))
        } else {
            Err(format!("invalid .git/HEAD format: {line}"))
        }
    }

    /// Read packed references from `.git/packed-refs` if present.
    pub async fn read_packed_refs(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        if let Ok(content) = self.vfs.read(".git/packed-refs").await {
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with('#') || line.starts_with('^') || line.is_empty() {
                    continue;
                }
                if let Some((hash, ref_path)) = line.split_once(' ') {
                    map.insert(ref_path.trim().to_string(), hash.trim().to_string());
                }
            }
        }
        map
    }

    /// Resolve a symbolic or branch reference to a 40-character commit hash.
    pub async fn resolve_ref(&self, target: &str) -> Result<String, String> {
        let target = target.trim();
        if target.len() == 40 && target.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(target.to_string());
        }

        if target == "HEAD" {
            let (head_target, _) = self.read_head().await?;
            if let Some(ref_path) = head_target.strip_prefix("ref: ") {
                return Box::pin(self.resolve_ref(ref_path)).await;
            }
            if head_target.len() == 40 {
                return Ok(head_target);
            }
        }

        // Try direct file under .git/
        let paths = if target.starts_with("refs/") {
            vec![format!(".git/{target}")]
        } else {
            vec![
                format!(".git/refs/heads/{target}"),
                format!(".git/refs/remotes/{target}"),
                format!(".git/refs/tags/{target}"),
            ]
        };

        for path in paths {
            if let Ok(content) = self.vfs.read(&path).await {
                let hash = content.trim();
                if hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Ok(hash.to_string());
                }
            }
        }

        // Check packed-refs
        let packed = self.read_packed_refs().await;
        if let Some(hash) = packed.get(target) {
            return Ok(hash.clone());
        }
        if !target.starts_with("refs/") {
            for prefix in &["refs/heads/", "refs/remotes/origin/", "refs/tags/"] {
                let full = format!("{prefix}{target}");
                if let Some(hash) = packed.get(&full) {
                    return Ok(hash.clone());
                }
            }
        }

        Err(format!("cannot resolve reference: {target}"))
    }

    /// Read and decompress a Git object by its 40-character hex hash.
    pub async fn read_object(&self, hash: &str) -> Result<(GitObjectType, Vec<u8>), String> {
        let hash = hash.trim().to_lowercase();
        if hash.len() != 40 {
            return Err(format!("invalid git object hash: {hash}"));
        }

        // 1. Try loose object: .git/objects/xx/xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
        let loose_path = format!(".git/objects/{}/{}", &hash[..2], &hash[2..]);
        if let Ok(bytes) = self.vfs.read_bytes(&loose_path).await {
            let mut decoder = ZlibDecoder::new(&bytes[..]);
            let mut decompressed = Vec::new();
            if decoder.read_to_end(&mut decompressed).is_ok() {
                if let Some(null_pos) = decompressed.iter().position(|&b| b == 0) {
                    let header = String::from_utf8_lossy(&decompressed[..null_pos]);
                    if let Some((kind_str, _)) = header.split_once(' ') {
                        let kind = match kind_str {
                            "commit" => GitObjectType::Commit,
                            "tree" => GitObjectType::Tree,
                            "blob" => GitObjectType::Blob,
                            "tag" => GitObjectType::Tag,
                            other => return Err(format!("unknown git object type: {other}")),
                        };
                        let data = decompressed[null_pos + 1..].to_vec();
                        return Ok((kind, data));
                    }
                }
            }
        }

        // 2. Try packfiles in .git/objects/pack/
        if let Ok(pack_entries) = self.vfs.list(".git/objects/pack").await {
            for entry in pack_entries {
                if entry.name.ends_with(".idx") {
                    let idx_path = entry.path.clone();
                    let pack_path = idx_path.trim_end_matches(".idx").to_string() + ".pack";
                    if let Ok(Some(obj)) = self.read_from_packfile(&idx_path, &pack_path, &hash).await {
                        return Ok(obj);
                    }
                }
            }
        }

        Err(format!("git object not found: {hash}"))
    }

    /// Attempt to find and extract an object from a packfile using its .idx v2.
    async fn read_from_packfile(
        &self,
        idx_path: &str,
        pack_path: &str,
        hash: &str,
    ) -> Result<Option<(GitObjectType, Vec<u8>)>, String> {
        let idx_bytes = match self.vfs.read_bytes(idx_path).await {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };
        if idx_bytes.len() < 1032 {
            return Ok(None);
        }

        // Validate index v2 magic: \xFFtOc
        if &idx_bytes[0..4] != b"\xfftOc" || &idx_bytes[4..8] != &[0, 0, 0, 2] {
            return Ok(None);
        }

        let target_bytes = match hex_to_bytes(hash) {
            Some(b) => b,
            None => return Ok(None),
        };

        let first_byte = target_bytes[0] as usize;
        let fanout_offset = 8;
        let read_u32 = |offset: usize| -> usize {
            u32::from_be_bytes(idx_bytes[offset..offset + 4].try_into().unwrap_or_default()) as usize
        };

        let start = if first_byte == 0 {
            0
        } else {
            read_u32(fanout_offset + (first_byte - 1) * 4)
        };
        let end = read_u32(fanout_offset + first_byte * 4);
        let total_objects = read_u32(fanout_offset + 255 * 4);

        if start >= end || end > total_objects {
            return Ok(None);
        }

        let sha_table_offset = 8 + 1024;
        let mut low = start;
        let mut high = end;
        let mut found_index = None;

        while low < high {
            let mid = (low + high) / 2;
            let entry_offset = sha_table_offset + mid * 20;
            if entry_offset + 20 > idx_bytes.len() {
                break;
            }
            let entry_sha = &idx_bytes[entry_offset..entry_offset + 20];
            match entry_sha.cmp(&target_bytes[..]) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => {
                    found_index = Some(mid);
                    break;
                }
            }
        }

        let index = match found_index {
            Some(i) => i,
            None => return Ok(None),
        };

        // Offset table follows SHA table (total_objects * 20) and CRC table (total_objects * 4)
        let offsets_offset = sha_table_offset + total_objects * 20 + total_objects * 4;
        let entry_offset_pos = offsets_offset + index * 4;
        if entry_offset_pos + 4 > idx_bytes.len() {
            return Ok(None);
        }

        let pack_offset_raw = read_u32(entry_offset_pos);
        let pack_offset = if pack_offset_raw & 0x8000_0000 != 0 {
            let large_offset_index = (pack_offset_raw & 0x7fff_ffff) * 8;
            let large_table_offset = offsets_offset + total_objects * 4 + large_offset_index;
            if large_table_offset + 8 > idx_bytes.len() {
                return Ok(None);
            }
            u64::from_be_bytes(
                idx_bytes[large_table_offset..large_table_offset + 8]
                    .try_into()
                    .unwrap_or_default(),
            ) as usize
        } else {
            pack_offset_raw
        };

        let pack_bytes = match self.vfs.read_bytes(pack_path).await {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };

        self.decode_pack_object(&pack_bytes, pack_offset).map(Some)
    }

    /// Decode an object from packfile bytes at a given offset, handling deltas.
    fn decode_pack_object(
        &self,
        pack: &[u8],
        offset: usize,
    ) -> Result<(GitObjectType, Vec<u8>), String> {
        if offset >= pack.len() {
            return Err(format!("pack offset {offset} out of bounds"));
        }

        let mut pos = offset;
        let first = pack[pos];
        pos += 1;
        let type_id = (first >> 4) & 0x07;
        let mut msb = (first & 0x80) != 0;
        let mut shift = 4;

        while msb && pos < pack.len() {
            let next = pack[pos];
            pos += 1;
            msb = (next & 0x80) != 0;
            shift += 7;
        }

        let _ = shift;

        match type_id {
            1..=4 => {
                let kind = GitObjectType::from_id(type_id).unwrap();
                let mut decoder = ZlibDecoder::new(&pack[pos..]);
                let mut decompressed = Vec::new();
                decoder
                    .read_to_end(&mut decompressed)
                    .map_err(|e| format!("zlib decompress error: {e}"))?;
                Ok((kind, decompressed))
            }
            6 => {
                // OFS_DELTA: variable length offset back to base object
                let mut byte = pack[pos];
                pos += 1;
                let mut base_offset = (byte & 0x7f) as usize;
                while (byte & 0x80) != 0 && pos < pack.len() {
                    byte = pack[pos];
                    pos += 1;
                    base_offset = ((base_offset + 1) << 7) | ((byte & 0x7f) as usize);
                }
                let base_target_offset = offset
                    .checked_sub(base_offset)
                    .ok_or("invalid ofs_delta base offset")?;

                let (kind, base_data) = self.decode_pack_object(pack, base_target_offset)?;
                let mut decoder = ZlibDecoder::new(&pack[pos..]);
                let mut delta_data = Vec::new();
                decoder
                    .read_to_end(&mut delta_data)
                    .map_err(|e| format!("zlib decompress delta error: {e}"))?;

                let patched = apply_git_delta(&base_data, &delta_data)?;
                Ok((kind, patched))
            }
            _ => Err(format!("unsupported pack object type {type_id}")),
        }
    }

    /// Parse a commit object from raw bytes.
    pub fn parse_commit(&self, hash: &str, data: &[u8]) -> Result<ParsedCommit, String> {
        let text = String::from_utf8_lossy(data);
        let (header, message) = text.split_once("\n\n").unwrap_or((&text, ""));

        let mut tree_hash = String::new();
        let mut parents = Vec::new();
        let mut author = "Unknown".to_string();
        let mut author_email = "unknown@example.com".to_string();
        let mut authored_at = String::new();
        let mut committer = "Unknown".to_string();
        let mut committer_email = "unknown@example.com".to_string();
        let mut committed_at = String::new();

        for line in header.lines() {
            if let Some(tree) = line.strip_prefix("tree ") {
                tree_hash = tree.trim().to_string();
            } else if let Some(parent) = line.strip_prefix("parent ") {
                parents.push(parent.trim().to_string());
            } else if let Some(auth_line) = line.strip_prefix("author ") {
                if let Some((name, rest)) = auth_line.split_once('<') {
                    author = name.trim().to_string();
                    if let Some((email, time_part)) = rest.split_once('>') {
                        author_email = email.trim().to_string();
                        if let Some((secs_str, _)) = time_part.trim().split_once(' ') {
                            if let Ok(secs) = secs_str.parse::<i64>() {
                                authored_at = crate::vfs::format_utc_timestamp(secs);
                            }
                        }
                    }
                }
            } else if let Some(com_line) = line.strip_prefix("committer ") {
                if let Some((name, rest)) = com_line.split_once('<') {
                    committer = name.trim().to_string();
                    if let Some((email, time_part)) = rest.split_once('>') {
                        committer_email = email.trim().to_string();
                        if let Some((secs_str, _)) = time_part.trim().split_once(' ') {
                            if let Ok(secs) = secs_str.parse::<i64>() {
                                committed_at = crate::vfs::format_utc_timestamp(secs);
                            }
                        }
                    }
                }
            }
        }

        let subject = message.lines().next().unwrap_or("").trim().to_string();

        Ok(ParsedCommit {
            hash: hash.to_string(),
            tree_hash,
            parents,
            author,
            author_email,
            authored_at,
            committer,
            committer_email,
            committed_at,
            subject,
            message: message.to_string(),
        })
    }

    /// Parse tree entries from a tree object's raw bytes.
    pub fn parse_tree(&self, data: &[u8]) -> Result<Vec<TreeEntry>, String> {
        let mut entries = Vec::new();
        let mut pos = 0;

        while pos < data.len() {
            let space_pos = match data[pos..].iter().position(|&b| b == b' ') {
                Some(p) => pos + p,
                None => break,
            };
            let mode = String::from_utf8_lossy(&data[pos..space_pos]).to_string();
            let is_dir = mode.starts_with("40") || mode.starts_with("04");

            let null_pos = match data[space_pos + 1..].iter().position(|&b| b == 0) {
                Some(p) => space_pos + 1 + p,
                None => break,
            };
            let name = String::from_utf8_lossy(&data[space_pos + 1..null_pos]).to_string();

            let sha_start = null_pos + 1;
            let sha_end = sha_start + 20;
            if sha_end > data.len() {
                break;
            }
            let hash = bytes_to_hex(&data[sha_start..sha_end]);

            entries.push(TreeEntry {
                mode,
                name,
                hash,
                is_dir,
            });

            pos = sha_end;
        }

        Ok(entries)
    }

    /// Recursively flatten a Git tree into path -> blob hash mappings.
    pub async fn flatten_tree(&self, tree_hash: &str) -> Result<HashMap<String, String>, String> {
        let mut map = HashMap::new();
        if tree_hash.is_empty() {
            return Ok(map);
        }
        self.flatten_tree_inner(tree_hash, "", &mut map).await?;
        Ok(map)
    }

    async fn flatten_tree_inner(
        &self,
        tree_hash: &str,
        prefix: &str,
        map: &mut HashMap<String, String>,
    ) -> Result<(), String> {
        let (kind, data) = self.read_object(tree_hash).await?;
        if kind != GitObjectType::Tree {
            return Err(format!("expected tree object for {tree_hash}"));
        }
        let entries = self.parse_tree(&data)?;
        for entry in entries {
            let path = if prefix.is_empty() {
                entry.name.clone()
            } else {
                format!("{prefix}/{}", entry.name)
            };
            if entry.is_dir {
                Box::pin(self.flatten_tree_inner(&entry.hash, &path, map)).await?;
            } else {
                map.insert(path, entry.hash);
            }
        }
        Ok(())
    }

    /// List commit history with pagination, ref tags and search.
    pub async fn history(&self, request: &GitHistoryRequest) -> Result<GitHistoryPage, String> {
        if !self.is_repo().await {
            return Err("not a git repository".into());
        }
        let start_ref = request.reference.as_deref().unwrap_or("HEAD");
        let start_hash = self.resolve_ref(start_ref).await?;

        // Collect all refs for display tags in the history graph
        let mut refs_list = Vec::new();
        let current_branch = self.read_head().await.ok().and_then(|(_, b)| b);

        if let Ok(branch_entries) = self.vfs.list(".git/refs/heads").await {
            for entry in branch_entries {
                if !entry.is_dir {
                    if let Ok(hash) = self.resolve_ref(&format!("refs/heads/{}", entry.name)).await {
                        refs_list.push(GitHistoryRef {
                            name: entry.name,
                            hash,
                            kind: "branch".into(),
                        });
                    }
                }
            }
        }
        for (ref_name, hash) in self.read_packed_refs().await {
            if let Some(branch) = ref_name.strip_prefix("refs/heads/") {
                if !refs_list.iter().any(|r| r.name == branch) {
                    refs_list.push(GitHistoryRef {
                        name: branch.to_string(),
                        hash: hash.clone(),
                        kind: "branch".into(),
                    });
                }
            } else if let Some(tag) = ref_name.strip_prefix("refs/tags/") {
                refs_list.push(GitHistoryRef {
                    name: tag.to_string(),
                    hash: hash.clone(),
                    kind: "tag".into(),
                });
            } else if let Some(remote) = ref_name.strip_prefix("refs/remotes/") {
                refs_list.push(GitHistoryRef {
                    name: remote.to_string(),
                    hash,
                    kind: "remote".into(),
                });
            }
        }

        // Map hash -> ref names
        let mut hash_to_refs = HashMap::<String, Vec<String>>::new();
        for r in &refs_list {
            hash_to_refs.entry(r.hash.clone()).or_default().push(r.name.clone());
        }
        if let Some(branch) = current_branch {
            if let Ok(head_hash) = self.resolve_ref("HEAD").await {
                let entry = hash_to_refs.entry(head_hash).or_default();
                if !entry.contains(&branch) {
                    entry.insert(0, branch);
                }
            }
        }

        // Traverse commits
        let mut visited = BTreeSet::new();
        let mut queue = std::collections::VecDeque::new();
        let mut all_commits = Vec::new();

        queue.push_back(start_hash.clone());
        visited.insert(start_hash);

        let query_search = request.search.trim().to_lowercase();
        let limit = 2000;

        while let Some(hash) = queue.pop_front() {
            if all_commits.len() >= limit {
                break;
            }
            let (kind, data) = match self.read_object(&hash).await {
                Ok(res) => res,
                Err(_) => continue,
            };
            if kind != GitObjectType::Commit {
                continue;
            }
            let parsed = match self.parse_commit(&hash, &data) {
                Ok(c) => c,
                Err(_) => continue,
            };

            for parent in &parsed.parents {
                if visited.insert(parent.clone()) {
                    queue.push_back(parent.clone());
                }
            }

            // Filter by search query if present
            if !query_search.is_empty() {
                let matches = parsed.subject.to_lowercase().contains(&query_search)
                    || parsed.author.to_lowercase().contains(&query_search)
                    || parsed.hash.starts_with(&query_search);
                if !matches {
                    continue;
                }
            }

            // Filter by path if present
            if let Some(target_path) = &request.path {
                let current_tree = self.flatten_tree(&parsed.tree_hash).await.unwrap_or_default();
                let parent_tree = if let Some(parent) = parsed.parents.first() {
                    if let Ok((GitObjectType::Commit, p_data)) = self.read_object(parent).await {
                        if let Ok(p_commit) = self.parse_commit(parent, &p_data) {
                            self.flatten_tree(&p_commit.tree_hash).await.unwrap_or_default()
                        } else {
                            HashMap::new()
                        }
                    } else {
                        HashMap::new()
                    }
                } else {
                    HashMap::new()
                };
                if current_tree.get(target_path) == parent_tree.get(target_path) {
                    continue;
                }
            }

            all_commits.push(GitHistoryCommit {
                history_path: request.path.clone(),
                hash: parsed.hash.clone(),
                parents: parsed.parents,
                author: parsed.author,
                author_email: parsed.author_email,
                authored_at: parsed.authored_at,
                committer: parsed.committer,
                committer_email: parsed.committer_email,
                committed_at: parsed.committed_at,
                subject: parsed.subject,
                message: parsed.message,
                refs: hash_to_refs.get(&parsed.hash).cloned().unwrap_or_default(),
            });
        }

        let offset = request.offset;
        let page_size = HISTORY_PAGE_SIZE;
        let has_more = all_commits.len() > offset + page_size;
        let commits = all_commits
            .into_iter()
            .skip(offset)
            .take(page_size)
            .collect();

        Ok(GitHistoryPage {
            commits,
            has_more,
            refs: refs_list,
        })
    }

    /// Produce commit inspection details and file diffs for a given commit.
    pub async fn commit_diff(&self, request: &GitCommitDiffRequest) -> Result<GitCommitDiff, String> {
        let (kind, data) = self.read_object(&request.hash).await?;
        if kind != GitObjectType::Commit {
            return Err(format!("expected commit object for {}", request.hash));
        }
        let commit = self.parse_commit(&request.hash, &data)?;
        let parent_hash = request
            .parent
            .as_deref()
            .or_else(|| commit.parents.first().map(String::as_str));

        let current_tree = self.flatten_tree(&commit.tree_hash).await?;
        let parent_tree = if let Some(parent) = parent_hash {
            if let Ok((GitObjectType::Commit, p_data)) = self.read_object(parent).await {
                let p_commit = self.parse_commit(parent, &p_data)?;
                self.flatten_tree(&p_commit.tree_hash).await?
            } else {
                HashMap::new()
            }
        } else {
            HashMap::new()
        };

        let mut files = Vec::new();
        let mut all_paths = BTreeSet::new();
        all_paths.extend(current_tree.keys().cloned());
        all_paths.extend(parent_tree.keys().cloned());

        let mut diff_builder = String::new();

        for path in all_paths {
            if let Some(target_path) = &request.path {
                if &path != target_path {
                    continue;
                }
            }
            let in_current = current_tree.get(&path);
            let in_parent = parent_tree.get(&path);

            let (status, old_content, new_content) = match (in_parent, in_current) {
                (None, Some(new_hash)) => {
                    let new_text = self.read_blob_text(new_hash).await.unwrap_or_default();
                    ("added", String::new(), new_text)
                }
                (Some(old_hash), None) => {
                    let old_text = self.read_blob_text(old_hash).await.unwrap_or_default();
                    ("deleted", old_text, String::new())
                }
                (Some(old_hash), Some(new_hash)) if old_hash != new_hash => {
                    let old_text = self.read_blob_text(old_hash).await.unwrap_or_default();
                    let new_text = self.read_blob_text(new_hash).await.unwrap_or_default();
                    ("modified", old_text, new_text)
                }
                _ => continue,
            };

            files.push(GitCommitFile {
                path: path.clone(),
                status: status.to_string(),
                previous_path: None,
            });

            diff_builder.push_str(&format!("diff --git a/{path} b/{path}\n"));
            diff_builder.push_str(&format!("--- a/{path}\n+++ b/{path}\n"));
            diff_builder.push_str(&format_unified_diff(&old_content, &new_content));
            diff_builder.push('\n');
        }

        Ok(GitCommitDiff {
            truncated: false,
            files,
            diff: diff_builder,
        })
    }

    /// Read the UTF-8 text of a blob object by hash.
    pub async fn read_blob_text(&self, hash: &str) -> Result<String, String> {
        let (kind, data) = self.read_object(hash).await?;
        if kind != GitObjectType::Blob {
            return Err(format!("expected blob object for {hash}"));
        }
        Ok(String::from_utf8_lossy(&data).into_owned())
    }

    /// Read file content at `HEAD` for editor diff comparisons.
    pub async fn file_head(&self, path: &str) -> Result<String, String> {
        let head_hash = self.resolve_ref("HEAD").await?;
        let (kind, data) = self.read_object(&head_hash).await?;
        if kind != GitObjectType::Commit {
            return Err("HEAD is not a commit".into());
        }
        let commit = self.parse_commit(&head_hash, &data)?;
        let tree = self.flatten_tree(&commit.tree_hash).await?;
        let blob_hash = tree
            .get(path)
            .ok_or_else(|| format!("path not found in HEAD: {path}"))?;
        self.read_blob_text(blob_hash).await
    }

    /// List repository branches with current/upstream metadata.
    pub async fn branches(&self) -> Result<Vec<GitBranchInfo>, String> {
        if !self.is_repo().await {
            return Err("not a git repository".into());
        }
        let (_, current_branch) = self.read_head().await?;
        let current_name = current_branch.unwrap_or_default();
        let mut branches = Vec::new();
        let mut seen = BTreeSet::new();

        if let Ok(entries) = self.vfs.list(".git/refs/heads").await {
            for entry in entries {
                if !entry.is_dir && seen.insert(entry.name.clone()) {
                    branches.push(GitBranchInfo {
                        is_current: entry.name == current_name,
                        is_remote: false,
                        name: entry.name,
                        upstream: None,
                    });
                }
            }
        }

        for ref_name in self.read_packed_refs().await.keys() {
            if let Some(branch) = ref_name.strip_prefix("refs/heads/") {
                if seen.insert(branch.to_string()) {
                    branches.push(GitBranchInfo {
                        is_current: branch == current_name,
                        is_remote: false,
                        name: branch.to_string(),
                        upstream: None,
                    });
                }
            }
        }

        Ok(branches)
    }

    /// Compute working-tree repository status by comparing HEAD tree with files on disk.
    pub async fn status(&self) -> Result<GitRepoStatus, String> {
        if !self.is_repo().await {
            return Err("not a git repository".into());
        }
        let (head_target, branch_name) = self.read_head().await.unwrap_or_default();
        let commit_hash = self.resolve_ref("HEAD").await.unwrap_or_default();

        let mut commit_message = None;
        let mut head_tree = HashMap::new();

        if !commit_hash.is_empty() {
            if let Ok((GitObjectType::Commit, data)) = self.read_object(&commit_hash).await {
                if let Ok(commit) = self.parse_commit(&commit_hash, &data) {
                    commit_message = Some(commit.subject);
                    head_tree = self.flatten_tree(&commit.tree_hash).await.unwrap_or_default();
                }
            }
        }

        // Enumerate workspace files (skipping ignored system and build folders)
        let mut worktree_files = HashMap::new();
        self.collect_worktree_files("", &mut worktree_files).await;

        let mut files = HashMap::new();
        let mut line_stats = GitLineStats::default();
        let mut file_line_stats = HashMap::new();

        // 1. Check tracked files in HEAD
        for (path, blob_hash) in &head_tree {
            match worktree_files.remove(path) {
                Some(content) => {
                    let head_text = self.read_blob_text(blob_hash).await.unwrap_or_default();
                    if content != head_text {
                        files.insert(path.clone(), GitFileStatus::Modified);
                        let stats = compute_line_stats(&head_text, &content);
                        line_stats.insertions += stats.insertions;
                        line_stats.deletions += stats.deletions;
                        file_line_stats.insert(path.clone(), stats);
                    }
                }
                None => {
                    files.insert(path.clone(), GitFileStatus::Deleted);
                    let head_text = self.read_blob_text(blob_hash).await.unwrap_or_default();
                    let stats = compute_line_stats(&head_text, "");
                    line_stats.insertions += stats.insertions;
                    line_stats.deletions += stats.deletions;
                    file_line_stats.insert(path.clone(), stats);
                }
            }
        }

        // 2. Remaining worktree files are untracked
        for (path, content) in worktree_files {
            files.insert(path.clone(), GitFileStatus::Untracked);
            let stats = compute_line_stats("", &content);
            line_stats.insertions += stats.insertions;
            file_line_stats.insert(path, stats);
        }

        let is_clean = files.is_empty();

        Ok(GitRepoStatus {
            branch: branch_name.unwrap_or_else(|| {
                if head_target.len() == 40 {
                    head_target[..8].to_string()
                } else {
                    "HEAD".to_string()
                }
            }),
            commit_hash,
            commit_message,
            upstream: None,
            ahead: 0,
            behind: 0,
            availability: GitStatusAvailability::Complete,
            is_clean,
            line_stats,
            file_line_stats,
            files,
        })
    }

    async fn collect_worktree_files(&self, dir: &str, files: &mut HashMap<String, String>) {
        if let Ok(entries) = self.vfs.list(dir).await {
            for entry in entries {
                let name = &entry.name;
                if name.starts_with('.') && name == ".git" {
                    continue;
                }
                if matches!(name.as_str(), "target" | "node_modules" | "dist" | ".spin") {
                    continue;
                }
                if entry.is_dir {
                    Box::pin(self.collect_worktree_files(&entry.path, files)).await;
                } else if let Ok(content) = self.vfs.read(&entry.path).await {
                    files.insert(entry.path, content);
                }
            }
        }
    }

    /// Compute working tree diff for a file or the whole repository.
    pub async fn diff(&self, path: Option<&str>) -> Result<String, String> {
        let status = self.status().await?;
        let mut diff_builder = String::new();

        let paths: Vec<String> = if let Some(p) = path {
            vec![p.to_string()]
        } else {
            status.files.keys().cloned().collect()
        };

        for p in paths {
            let file_status = status.files.get(&p);
            let head_text = self.file_head(&p).await.unwrap_or_default();
            let work_text = self.vfs.read(&p).await.unwrap_or_default();

            if file_status.is_some() {
                diff_builder.push_str(&format!("diff --git a/{p} b/{p}\n"));
                diff_builder.push_str(&format!("--- a/{p}\n+++ b/{p}\n"));
                diff_builder.push_str(&format_unified_diff(&head_text, &work_text));
                diff_builder.push('\n');
            }
        }

        Ok(diff_builder)
    }

    /// Return staged/index diff if available.
    pub async fn index_diff(&self) -> Result<String, String> {
        Ok(String::new())
    }

    /// Switch branches or create a new branch.
    pub async fn checkout(&self, request: &GitCheckoutRequest) -> Result<GitCheckoutResult, String> {
        let (head_target, current_branch) = self.read_head().await?;
        let previous_branch = current_branch;

        let target_ref = format!(".git/refs/heads/{}", request.branch);
        let exists = self.vfs.read(&target_ref).await.is_ok()
            || self.read_packed_refs().await.contains_key(&format!("refs/heads/{}", request.branch));

        if !exists {
            if request.create_if_missing {
                let current_hash = self.resolve_ref("HEAD").await?;
                self.vfs
                    .write(&target_ref, &format!("{current_hash}\n"))
                    .await
                    .map_err(|e| format!("cannot create branch: {e}"))?;
            } else {
                return Err(format!("branch '{}' does not exist", request.branch));
            }
        }

        // Update HEAD
        self.vfs
            .write(".git/HEAD", &format!("ref: refs/heads/{}\n", request.branch))
            .await
            .map_err(|e| format!("cannot update HEAD: {e}"))?;

        let _ = head_target;

        Ok(GitCheckoutResult {
            branch: request.branch.clone(),
            previous_branch,
            switched: true,
        })
    }

    /// Write a raw loose object into `.git/objects/`.
    pub async fn write_loose_object(&self, kind: GitObjectType, data: &[u8]) -> Result<String, String> {
        let header = format!("{} {}\0", kind.as_str(), data.len());
        let mut hasher = Sha1::new();
        hasher.update(header.as_bytes());
        hasher.update(data);
        let hash = bytes_to_hex(&hasher.finalize());

        let dir = format!(".git/objects/{}", &hash[..2]);
        let path = format!("{dir}/{}", &hash[2..]);

        let _ = self.vfs.create(&dir, VfsEntryKind::Directory).await;

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        let mut full_payload = header.into_bytes();
        full_payload.extend_from_slice(data);
        use std::io::Write;
        encoder.write_all(&full_payload).map_err(|e| e.to_string())?;
        let compressed = encoder.finish().map_err(|e| e.to_string())?;

        self.vfs
            .write_bytes(&path, &compressed)
            .await
            .map_err(|e| format!("cannot write git object: {e}"))?;

        Ok(hash)
    }

    /// Create a local commit with modified/untracked files.
    pub async fn commit(&self, request: &GitCommitRequest) -> Result<GitCommitResult, String> {
        let parent_hash = self.resolve_ref("HEAD").await.ok();

        // 1. Collect working files to commit
        let mut worktree_files = HashMap::new();
        self.collect_worktree_files("", &mut worktree_files).await;

        // 2. Write blobs for files
        let mut tree_entries = Vec::new();
        for (path, content) in &worktree_files {
            if let Some(paths) = &request.paths {
                if !paths.contains(path) {
                    continue;
                }
            }
            let blob_hash = self.write_loose_object(GitObjectType::Blob, content.as_bytes()).await?;
            tree_entries.push(TreeEntry {
                mode: "100644".into(),
                name: path.clone(),
                hash: blob_hash,
                is_dir: false,
            });
        }

        // 3. Write tree object
        let mut tree_payload = Vec::new();
        for entry in &tree_entries {
            let sha_bytes = hex_to_bytes(&entry.hash).ok_or("invalid blob hash")?;
            tree_payload.extend_from_slice(entry.mode.as_bytes());
            tree_payload.push(b' ');
            tree_payload.extend_from_slice(entry.name.as_bytes());
            tree_payload.push(0);
            tree_payload.extend_from_slice(&sha_bytes);
        }
        let tree_hash = self.write_loose_object(GitObjectType::Tree, &tree_payload).await?;

        // 4. Write commit object
        let now_secs = 1700000000i64; // Fallback or current time
        let mut commit_payload = format!("tree {tree_hash}\n");
        if let Some(parent) = parent_hash {
            commit_payload.push_str(&format!("parent {parent}\n"));
        }
        commit_payload.push_str(&format!(
            "author User <user@openwebide.local> {now_secs} +0000\ncommitter User <user@openwebide.local> {now_secs} +0000\n\n{}\n",
            request.message.trim()
        ));

        let commit_hash = self
            .write_loose_object(GitObjectType::Commit, commit_payload.as_bytes())
            .await?;

        // 5. Update branch ref or HEAD
        let (head_target, branch_name) = self.read_head().await?;
        if let Some(branch) = branch_name {
            self.vfs
                .write(&format!(".git/refs/heads/{branch}"), &format!("{commit_hash}\n"))
                .await
                .map_err(|e| format!("cannot update branch ref: {e}"))?;
        } else {
            self.vfs
                .write(".git/HEAD", &format!("{commit_hash}\n"))
                .await
                .map_err(|e| format!("cannot update HEAD: {e}"))?;
        }

        let _ = head_target;

        Ok(GitCommitResult {
            commit_hash: commit_hash.clone(),
            summary: request.message.lines().next().unwrap_or("").to_string(),
            pre_commit_output: None,
            is_signed: false,
        })
    }

    /// Report path changes (staged, unstaged, untracked).
    pub async fn path_changes(&self) -> Result<GitPathChanges, String> {
        let status = self.status().await?;
        let mut staged = BTreeSet::new();
        let mut unstaged = BTreeSet::new();
        let mut untracked = BTreeSet::new();

        for (path, file_status) in status.files {
            match file_status {
                GitFileStatus::Added => {
                    staged.insert(path);
                }
                GitFileStatus::Modified => {
                    unstaged.insert(path);
                }
                GitFileStatus::Deleted => {
                    unstaged.insert(path);
                }
                GitFileStatus::Untracked => {
                    untracked.insert(path);
                }
                _ => {}
            }
        }

        Ok(GitPathChanges {
            has_head: !status.commit_hash.is_empty(),
            staged,
            unstaged,
            untracked,
            renamed_from: HashMap::new(),
        })
    }

    /// Perform a working-tree path action (e.g. revert changes).
    pub async fn path_action(&self, request: &GitPathRequest) -> Result<GitPathChanges, String> {
        if request.action == GitPathAction::Revert {
            if let Ok(head_content) = self.file_head(&request.path).await {
                let _ = self.vfs.write(&request.path, &head_content).await;
            } else {
                let _ = self.vfs.delete(&request.path).await;
            }
        }
        self.path_changes().await
    }

    /// Stash action placeholder in local browser mode.
    pub async fn stash(&self, _request: &GitStashRequest) -> Result<GitStashResult, String> {
        Ok(GitStashResult {
            stashes: Vec::new(),
            output: "Stash is not configured in local in-browser mode.".into(),
        })
    }

    /// Remote sync informs the user that network sync requires bridge connection.
    pub async fn sync(&self, _request: &GitSyncRequest) -> Result<GitSyncResult, String> {
        Err("Remote sync requires network credentials. Connect openwebide-bridge to push/pull with your system credentials.".into())
    }
}

fn format_unified_diff(old_text: &str, new_text: &str) -> String {
    let ops = line_lcs(old_text, new_text);
    let mut out = String::new();
    let mut del_count = 0;
    let mut ins_count = 0;
    for op in &ops {
        match op {
            LineOp::Delete(_) => del_count += 1,
            LineOp::Insert(_) => ins_count += 1,
            _ => {}
        }
    }
    if del_count == 0 && ins_count == 0 {
        return out;
    }
    out.push_str(&format!("@@ -1,{} +1,{} @@\n", del_count, ins_count));
    for op in ops {
        match op {
            LineOp::Equal(l) => {
                out.push(' ');
                out.push_str(l.text);
                out.push('\n');
            }
            LineOp::Delete(l) => {
                out.push('-');
                out.push_str(l.text);
                out.push('\n');
            }
            LineOp::Insert(l) => {
                out.push('+');
                out.push_str(l.text);
                out.push('\n');
            }
        }
    }
    out
}

fn compute_line_stats(old_text: &str, new_text: &str) -> GitLineStats {
    let mut insertions = 0;
    let mut deletions = 0;
    for op in line_lcs(old_text, new_text) {
        match op {
            LineOp::Insert(_) => insertions += 1,
            LineOp::Delete(_) => deletions += 1,
            _ => {}
        }
    }
    GitLineStats {
        insertions,
        deletions,
    }
}

fn hex_to_bytes(hex: &str) -> Option<[u8; 20]> {
    if hex.len() != 40 {
        return None;
    }
    let mut bytes = [0u8; 20];
    for i in 0..20 {
        bytes[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

/// Apply a standard Git delta (copy/insert instructions) to a base object.
fn apply_git_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>, String> {
    let mut pos = 0;

    // Read base size
    let mut base_size: usize = 0;
    let mut shift = 0;
    loop {
        if pos >= delta.len() {
            return Err("unexpected EOF reading base size".into());
        }
        let b = delta[pos];
        pos += 1;
        base_size |= ((b & 0x7f) as usize) << shift;
        shift += 7;
        if b & 0x80 == 0 {
            break;
        }
    }
    if base_size != base.len() {
        return Err(format!(
            "delta base size mismatch: expected {}, got {}",
            base_size,
            base.len()
        ));
    }

    // Read target size
    let mut target_size: usize = 0;
    shift = 0;
    loop {
        if pos >= delta.len() {
            return Err("unexpected EOF reading target size".into());
        }
        let b = delta[pos];
        pos += 1;
        target_size |= ((b & 0x7f) as usize) << shift;
        shift += 7;
        if b & 0x80 == 0 {
            break;
        }
    }

    let mut output = Vec::with_capacity(target_size);

    while pos < delta.len() {
        let op = delta[pos];
        pos += 1;

        if op & 0x80 != 0 {
            // Copy instruction from base
            let mut offset = 0usize;
            let mut size = 0usize;

            if op & 0x01 != 0 {
                offset |= delta[pos] as usize;
                pos += 1;
            }
            if op & 0x02 != 0 {
                offset |= (delta[pos] as usize) << 8;
                pos += 1;
            }
            if op & 0x04 != 0 {
                offset |= (delta[pos] as usize) << 16;
                pos += 1;
            }
            if op & 0x08 != 0 {
                offset |= (delta[pos] as usize) << 24;
                pos += 1;
            }
            if op & 0x10 != 0 {
                size |= delta[pos] as usize;
                pos += 1;
            }
            if op & 0x20 != 0 {
                size |= (delta[pos] as usize) << 8;
                pos += 1;
            }
            if op & 0x40 != 0 {
                size |= (delta[pos] as usize) << 16;
                pos += 1;
            }
            if size == 0 {
                size = 0x10000;
            }

            if offset + size > base.len() {
                return Err("delta copy offset exceeds base length".into());
            }
            output.extend_from_slice(&base[offset..offset + size]);
        } else if op != 0 {
            // Insert instruction
            let size = op as usize;
            if pos + size > delta.len() {
                return Err("delta insert size exceeds delta length".into());
            }
            output.extend_from_slice(&delta[pos..pos + size]);
            pos += size;
        } else {
            return Err("invalid zero opcode in git delta".into());
        }
    }

    if output.len() != target_size {
        return Err(format!(
            "reconstructed delta size {} does not match expected {}",
            output.len(),
            target_size
        ));
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryVfs;

    #[test]
    fn local_git_initializes_and_reads_head_and_commits() {
        futures::executor::block_on(async {
            let vfs = MemoryVfs::new();
            let git = LocalGit::new(vfs.clone());

            assert!(!git.is_repo().await);

            vfs.write(".git/HEAD", "ref: refs/heads/main\n").await.unwrap();
            assert!(git.is_repo().await);

            let (head_ref, branch) = git.read_head().await.unwrap();
            assert_eq!(head_ref, "ref: refs/heads/main");
            assert_eq!(branch.as_deref(), Some("main"));

            // Write a workspace file
            vfs.write("hello.txt", "hello world\n").await.unwrap();

            // Create a commit
            let commit_res = git
                .commit(&GitCommitRequest {
                    message: "initial commit".into(),
                    paths: None,
                    include_untracked: true,
                    staged_only: false,
                })
                .await
                .unwrap();

            assert_eq!(commit_res.summary, "initial commit");
            assert_eq!(commit_res.commit_hash.len(), 40);

            // Verify resolved HEAD
            let head_hash = git.resolve_ref("HEAD").await.unwrap();
            assert_eq!(head_hash, commit_res.commit_hash);

            // Verify status after commit is clean
            let status = git.status().await.unwrap();
            assert!(status.is_clean);
            assert_eq!(status.branch, "main");
            assert_eq!(status.commit_hash, commit_res.commit_hash);

            // Verify history
            let history = git
                .history(&GitHistoryRequest {
                    offset: 0,
                    search: String::new(),
                    reference: None,
                    path: None,
                })
                .await
                .unwrap();

            assert_eq!(history.commits.len(), 1);
            assert_eq!(history.commits[0].subject, "initial commit");
            assert_eq!(history.commits[0].hash, commit_res.commit_hash);

            // Verify file_head
            let head_content = git.file_head("hello.txt").await.unwrap();
            assert_eq!(head_content, "hello world\n");

            // Verify branches
            let branches = git.branches().await.unwrap();
            assert_eq!(branches.len(), 1);
            assert_eq!(branches[0].name, "main");
            assert!(branches[0].is_current);

            // Create and switch branch
            let checkout_res = git
                .checkout(&GitCheckoutRequest {
                    branch: "feat-a".into(),
                    create_if_missing: true,
                })
                .await
                .unwrap();

            assert_eq!(checkout_res.branch, "feat-a");
            assert_eq!(checkout_res.previous_branch.as_deref(), Some("main"));

            let (_, new_branch) = git.read_head().await.unwrap();
            assert_eq!(new_branch.as_deref(), Some("feat-a"));

            // Modify file and test diff & status
            vfs.write("hello.txt", "hello world\nmore lines\n").await.unwrap();
            let status2 = git.status().await.unwrap();
            assert!(!status2.is_clean);
            assert_eq!(status2.files.get("hello.txt"), Some(&GitFileStatus::Modified));
            let diff_text = git.diff(Some("hello.txt")).await.unwrap();
            assert!(diff_text.contains("+more lines"));
        });
    }
}
