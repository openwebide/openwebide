//! Native collection primitives for the shared declared-input pattern policy.
use super::files::FileRevision;
use super::*;
use openwebide_core::{
    plugins::languages::{ProjectInputPatterns, ProjectInputs},
    workspace_transaction::{EntryState, WorkspacePlan},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::io::AsyncReadExt;

const MAX_ENTRIES: usize = 65_536;
const MAX_FILES: usize = 4096;
const MAX_PATH_BYTES: usize = 2 * 1024 * 1024;
const MAX_CONTENT_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(super) struct Snapshot(BTreeMap<String, File>);
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct File {
    digest: String,
    canonical: PathBuf,
}
struct CachedFile {
    stamp: FileRevision,
    file: File,
}
#[derive(Default)]
struct Cache {
    files: BTreeMap<String, CachedFile>,
    revision: u64,
}
pub(super) struct Observation {
    pub snapshot: Snapshot,
    pub revision: u64,
}
pub(super) struct Inputs {
    patterns: Option<ProjectInputPatterns>,
    pub(super) baseline: Snapshot,
    baseline_revision: u64,
    cache: tokio::sync::Mutex<Cache>,
}
impl Inputs {
    pub(super) async fn new(
        root: &Path,
        declaration: Option<&ProjectInputs>,
    ) -> Result<Self, String> {
        let mut value = Self {
            patterns: declaration.map(ProjectInputPatterns::new).transpose()?,
            baseline: Snapshot::default(),
            baseline_revision: 0,
            cache: tokio::sync::Mutex::default(),
        };
        let observed = value.observe(root).await?;
        value.baseline = observed.snapshot;
        value.baseline_revision = observed.revision;
        Ok(value)
    }
    pub(super) fn is_baseline(&self, observed: &Observation) -> bool {
        observed.snapshot == self.baseline && observed.revision == self.baseline_revision
    }
    pub(super) async fn revision(&self) -> u64 {
        self.cache.lock().await.revision
    }
    #[cfg(test)]
    pub(super) async fn current(&self, root: &Path) -> Result<Snapshot, String> {
        Ok(self.observe(root).await?.snapshot)
    }
    pub(super) async fn observe(&self, root: &Path) -> Result<Observation, String> {
        let Some(patterns) = &self.patterns else {
            return Ok(Observation {
                snapshot: Snapshot::default(),
                revision: 0,
            });
        };
        tokio::time::timeout(Duration::from_secs(5), self.collect(root, patterns))
            .await
            .map_err(|_| "Reading declared project inputs timed out".to_string())?
    }
    async fn collect(
        &self,
        root: &Path,
        patterns: &ProjectInputPatterns,
    ) -> Result<Observation, String> {
        let mut cache = self.cache.lock().await;
        let mut next = BTreeMap::new();
        let mut pending = vec![(root.to_path_buf(), 0_usize)];
        let mut snapshot = Snapshot::default();
        let mut entries = 0;
        let mut path_bytes = 0_usize;
        let mut content_bytes = 0_u64;
        while let Some((directory, depth)) = pending.pop() {
            let mut children = tokio::fs::read_dir(&directory)
                .await
                .map_err(|e| e.to_string())?;
            while let Some(entry) = children.next_entry().await.map_err(|e| e.to_string())? {
                entries += 1;
                if entries > MAX_ENTRIES {
                    return Err("Declared project inputs exceed the directory-entry limit; narrow their patterns/exclusions".into());
                }
                let path = entry.path();
                let relative = path
                    .strip_prefix(root)
                    .map_err(|e| e.to_string())?
                    .components()
                    .map(|component| {
                        component
                            .as_os_str()
                            .to_str()
                            .ok_or("Project input paths must be UTF-8")
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .join("/");
                let kind = entry.file_type().await.map_err(|e| e.to_string())?;
                if kind.is_dir() {
                    if patterns.includes_directory(&relative)
                        && !patterns.excludes_directory(&relative)
                    {
                        if depth >= 128 {
                            return Err(
                                "Declared project inputs exceed the directory-depth limit".into()
                            );
                        }
                        pending.push((path, depth + 1));
                    }
                } else if patterns.includes_file(&relative) {
                    let metadata = tokio::fs::metadata(&path)
                        .await
                        .map_err(|e| e.to_string())?;
                    if !metadata.is_file() {
                        return Err("A declared project input is not a regular file".into());
                    }
                    path_bytes = path_bytes.saturating_add(relative.len());
                    content_bytes = content_bytes.saturating_add(metadata.len());
                    if snapshot.0.len() >= MAX_FILES
                        || path_bytes > MAX_PATH_BYTES
                        || content_bytes > MAX_CONTENT_BYTES
                    {
                        return Err("Declared project inputs exceed their file, path or content budget; narrow their patterns/exclusions".into());
                    }
                    let canonical = tokio::fs::canonicalize(&path)
                        .await
                        .map_err(|e| e.to_string())?;
                    if openwebide_core::vfs::private_workspace_path(
                        &canonical.to_string_lossy().replace('\\', "/"),
                    ) {
                        continue;
                    }
                    let stamp = FileRevision::from(&metadata);
                    let file = if let Some(known) = cache
                        .files
                        .get(&relative)
                        .filter(|known| known.stamp == stamp && known.file.canonical == canonical)
                    {
                        known.file.clone()
                    } else {
                        let mut reader = tokio::fs::File::open(&path)
                            .await
                            .map_err(|e| e.to_string())?
                            .take(MAX_CONTENT_BYTES + 1);
                        let mut digest = Sha256::new();
                        let mut buffer = vec![0_u8; 64 * 1024];
                        let mut read = 0_u64;
                        loop {
                            let size = reader.read(&mut buffer).await.map_err(|e| e.to_string())?;
                            if size == 0 {
                                break;
                            }
                            read = read.saturating_add(size as u64);
                            if read > metadata.len() || read > MAX_CONTENT_BYTES {
                                return Err("A declared project input changed while reading".into());
                            }
                            digest.update(&buffer[..size]);
                        }
                        if read != metadata.len()
                            || FileRevision::from(
                                &tokio::fs::metadata(&path)
                                    .await
                                    .map_err(|e| e.to_string())?,
                            ) != stamp
                            || tokio::fs::canonicalize(&path)
                                .await
                                .map_err(|e| e.to_string())?
                                != canonical
                        {
                            return Err("A declared project input changed while reading".into());
                        }
                        File {
                            digest: digest
                                .finalize()
                                .iter()
                                .map(|byte| format!("{byte:02x}"))
                                .collect(),
                            canonical,
                        }
                    };
                    next.insert(
                        relative.clone(),
                        CachedFile {
                            stamp,
                            file: file.clone(),
                        },
                    );
                    snapshot.0.insert(relative, file);
                }
            }
        }
        let changed = next.len() != cache.files.len()
            || next.iter().any(|(path, file)| {
                !cache.files.get(path).is_some_and(|previous| {
                    previous.stamp == file.stamp && previous.file == file.file
                })
            });
        if changed {
            cache.revision = cache
                .revision
                .checked_add(1)
                .ok_or("Declared project input revision exhausted")?;
        }
        // Publish the complete inventory and its revision together. A cancelled
        // or failed collection must not consume metadata observations piecemeal.
        cache.files = next;
        Ok(Observation {
            snapshot,
            revision: cache.revision,
        })
    }
    pub(super) fn after_plan(&self, root: &Path, plan: &WorkspacePlan) -> Snapshot {
        let Some(patterns) = &self.patterns else {
            return Snapshot::default();
        };
        let mut snapshot = self.baseline.clone();
        snapshot.0.retain(|path, _| {
            !plan
                .roots
                .iter()
                .any(|parent| path == parent || path.starts_with(&format!("{parent}/")))
        });
        for (path, state) in &plan.after {
            if let EntryState::File { bytes } = state
                && patterns.includes_file(path)
            {
                snapshot.0.insert(
                    path.clone(),
                    File {
                        digest: openwebide_core::plugins::content_digest(bytes),
                        canonical: root.join(path),
                    },
                );
            }
        }
        snapshot
    }
}

#[cfg(test)]
mod tests;
