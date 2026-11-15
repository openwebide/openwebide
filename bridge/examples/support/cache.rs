//! Measurements shared by the real-server probes. Never follow server-created
//! links when measuring derived data, or count them as project output.
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::SystemTime,
};

pub type Snapshot = BTreeMap<PathBuf, (u64, SystemTime)>;

pub fn namespaces(root: &Path) -> Result<std::collections::BTreeSet<PathBuf>> {
    let mut result = std::collections::BTreeSet::new();
    let path = root.join("v1");
    for entry in std::fs::read_dir(&path).with_context(|| {
        format!(
            "Listing native derived-cache namespaces in {}",
            path.display()
        )
    })? {
        let entry = entry.with_context(|| format!("Reading a namespace in {}", path.display()))?;
        if entry
            .file_type()
            .with_context(|| format!("Reading namespace type for {}", entry.path().display()))?
            .is_dir()
        {
            result.insert(entry.path());
        }
    }
    Ok(result)
}

pub fn snapshot(root: &Path) -> Result<Snapshot> {
    snapshot_with(root, std::fs::DirEntry::metadata)
}

fn derived_data(root: &Path, path: &Path) -> bool {
    let Ok(path) = path.strip_prefix(root) else {
        return false;
    };
    let mut parts = path.components();
    parts.next().is_some_and(|part| part.as_os_str() == "v1")
        && parts.next().is_some()
        && parts.next().is_some_and(|part| part.as_os_str() == "data")
}

fn cache_entry<T>(
    root: &Path,
    path: &Path,
    result: std::io::Result<T>,
) -> std::io::Result<Option<T>> {
    match result {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && derived_data(root, path) => {
            Ok(None)
        }
        result => result.map(Some),
    }
}

fn snapshot_with(
    root: &Path,
    mut metadata: impl FnMut(&std::fs::DirEntry) -> std::io::Result<std::fs::Metadata>,
) -> Result<Snapshot> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        // Servers may replace temporary derived files while being sampled.
        // Control directories and every other I/O error remain required.
        let Some(entries) = cache_entry(root, &path, std::fs::read_dir(&path))
            .with_context(|| format!("Listing derived-cache files in {}", path.display()))?
        else {
            continue;
        };
        for entry in entries {
            let Some(entry) = cache_entry(root, &path, entry)
                .with_context(|| format!("Reading an entry in {}", path.display()))?
            else {
                continue;
            };
            let entry_path = entry.path();
            let Some(kind) =
                cache_entry(root, &entry_path, entry.file_type()).with_context(|| {
                    format!(
                        "Reading derived-cache entry type for {}",
                        entry_path.display()
                    )
                })?
            else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                pending.push(entry_path);
            } else if kind.is_file() && derived_data(root, &entry_path) {
                let Some(metadata) = cache_entry(root, &entry_path, metadata(&entry))
                    .with_context(|| {
                        format!(
                            "Reading derived-cache metadata for {}",
                            entry_path.display()
                        )
                    })?
                else {
                    continue;
                };
                files.insert(entry_path, (metadata.len(), metadata.modified()?));
            }
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_keeps_existing_data_when_another_cache_file_disappears() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("v1/namespace/data");
        std::fs::create_dir_all(&data).unwrap();
        let retained = data.join("retained");
        let removed = data.join("removed");
        std::fs::write(&retained, "retained derived data").unwrap();
        std::fs::write(&removed, "temporary derived data").unwrap();
        let snapshot = snapshot_with(root.path(), |entry| {
            if entry.path() == removed {
                std::fs::remove_file(&removed)?;
            }
            entry.metadata()
        })
        .unwrap();
        assert_eq!(snapshot.keys().collect::<Vec<_>>(), vec![&retained]);
    }

    #[test]
    fn snapshot_requires_its_control_root_even_below_a_data_directory() {
        let root = tempfile::tempdir().unwrap();
        let error = snapshot(&root.path().join("data/missing-cache")).unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::NotFound
        );
    }
}

pub async fn wait_for(root: &Path, matches: impl Fn(&Path) -> bool) -> Result<Snapshot> {
    tokio::time::timeout(std::time::Duration::from_secs(45), async {
        loop {
            let files = snapshot(root)?;
            if files.keys().any(|path| matches(path)) {
                return Ok(files);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await?
}

pub fn report(
    server: &str,
    before: &Snapshot,
    after: &Snapshot,
    cold: std::time::Duration,
    restart: std::time::Duration,
) -> Result<()> {
    let retained = before
        .iter()
        .filter(|(path, stamp)| after.get(*path) == Some(*stamp))
        .count();
    ensure!(
        retained > 0,
        "{server} retained no derived files across process restart"
    );
    println!(
        "{}",
        serde_json::json!({"cache_server":server,"derived_bytes":after.values().map(|(bytes, _)| *bytes).sum::<u64>(),"retained_files":retained,"cold_hover_ms":cold.as_secs_f64()*1000.0,"restart_hover_ms":restart.as_secs_f64()*1000.0})
    );
    Ok(())
}
