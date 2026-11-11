//! Measurements shared by the real-server probes. Never follow server-created
//! links when measuring derived data, or count them as project output.
use anyhow::{Result, ensure};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::SystemTime,
};

pub type Snapshot = BTreeMap<PathBuf, (u64, SystemTime)>;

pub fn namespaces(root: &Path) -> Result<std::collections::BTreeSet<PathBuf>> {
    let mut result = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(root.join("v1"))? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            result.insert(entry.path());
        }
    }
    Ok(result)
}

pub fn snapshot(root: &Path) -> Result<Snapshot> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file()
                && entry
                    .path()
                    .components()
                    .any(|part| part.as_os_str() == "data")
            {
                let metadata = entry.metadata()?;
                files.insert(entry.path(), (metadata.len(), metadata.modified()?));
            }
        }
    }
    Ok(files)
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
