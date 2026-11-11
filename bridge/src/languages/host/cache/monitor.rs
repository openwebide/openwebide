//! Sample running servers' derived data without deleting live namespaces. This
//! bounds sustained growth, not writes between samples or an OS disk quota.
use super::*;
use std::collections::BTreeSet;

const SAMPLE_INTERVAL: Duration = Duration::from_secs(10);

pub(in crate::languages::host) async fn poll(inner: &Arc<Inner>) {
    {
        let mut sampled = inner.cache.sampled.lock().unwrap();
        if sampled.is_some_and(|last| last.elapsed() < SAMPLE_INTERVAL) {
            return;
        }
        *sampled = Some(Instant::now());
    }
    enforce(inner).await;
}

fn path(entry: &Entry) -> Result<Option<PathBuf>, String> {
    fn data(cache: Option<&Use>) -> Result<Option<PathBuf>, String> {
        let Some(cache) = cache else {
            return Ok(None);
        };
        cache.check()?;
        Ok(cache
            .lease
            .lock()
            .unwrap()
            .as_ref()
            .map(|lease| lease.data.clone()))
    }
    entry.session.get().map_or_else(
        || {
            data(
                entry
                    .handoff
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(reuse::Warm::cache),
            )
        },
        |session| data(session.cache.as_ref()),
    )
}

struct Sample {
    entries: Vec<Existing>,
    bytes: u64,
    over: bool,
}
fn sample(root: &Path, live: &BTreeSet<PathBuf>, limit: u64) -> Result<Sample, String> {
    directory(root, true)?;
    let _lock = lock(root)?;
    let mut entries = inventory(root)?;
    if live
        .iter()
        .any(|data| !entries.iter().any(|entry| entry.path.join("data") == *data))
    {
        return Err("Active native language cache ownership could not be verified".into());
    }
    let over = !prune_idle(&mut entries, limit, MAX_NAMESPACES)?;
    let bytes = entries
        .iter()
        .fold(0_u64, |total, entry| total.saturating_add(entry.bytes));
    Ok(Sample {
        entries,
        bytes,
        over,
    })
}

async fn enforce(inner: &Arc<Inner>) {
    let mut roots = BTreeMap::<PathBuf, BTreeSet<PathBuf>>::new();
    {
        let entries = inner.entries.lock().unwrap();
        for entry in entries.values().filter(|entry| !*entry.stopped.borrow()) {
            match path(entry) {
                Ok(Some(data)) => {
                    if let Some(root) = data.parent().and_then(Path::parent) {
                        roots.entry(root.to_path_buf()).or_default().insert(data);
                    }
                }
                Ok(None) => (),
                Err(error) => stop(inner, entry, &error),
            }
        }
    }
    if roots.is_empty() {
        return;
    }
    let mut measured = 0_u64;
    let mut complete = true;
    for (root, live) in roots {
        let scan_root = root.clone();
        let limit = inner.cache.retained_bytes.clone();
        let sampled = tokio::task::spawn_blocking(move || sample(&scan_root, &live, limit?))
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result);
        let reason = match sampled {
            Ok(sample) if !sample.over => {
                measured = measured.saturating_add(sample.bytes);
                continue;
            }
            Ok(sample) => {
                measured = measured.saturating_add(sample.bytes);
                Some(sample)
            }
            Err(error) => {
                complete = false;
                let mut usage = inner.resources.lock().unwrap();
                usage.cache_sample_errors = usage.cache_sample_errors.saturating_add(1);
                tracing::warn!(%error, "Native language cache growth could not be checked");
                None
            }
        };
        // Resolve current ownership after the scan: an idle process can transfer
        // to another editing client while the filesystem inventory is running.
        let entries = inner.entries.lock().unwrap();
        let mut groups = BTreeMap::<PathBuf, Vec<&Arc<Entry>>>::new();
        for entry in entries.values().filter(|entry| !*entry.stopped.borrow()) {
            if let Ok(Some(data)) = path(entry)
                && data.parent().and_then(Path::parent) == Some(root.as_path())
            {
                groups.entry(data).or_default().push(entry);
            }
        }
        let mut victims = groups.into_iter().collect::<Vec<_>>();
        let bytes = |data: &Path| {
            reason
                .as_ref()
                .and_then(|sample| {
                    sample
                        .entries
                        .iter()
                        .find(|entry| entry.path.join("data") == data)
                })
                .map_or(0, |entry| entry.bytes)
        };
        victims.sort_by_key(|(data, owners)| {
            (
                owners.iter().any(|entry| entry.busy()),
                std::cmp::Reverse(bytes(data)),
            )
        });
        let mut excess = reason.as_ref().map_or(u64::MAX, |sample| {
            sample
                .bytes
                .saturating_sub(inner.cache.retained_bytes.clone().unwrap_or(0))
        });
        let mut count = reason.as_ref().map_or(usize::MAX, |sample| {
            sample.entries.len().saturating_sub(MAX_NAMESPACES)
        });
        for (data, owners) in victims {
            if excess == 0 && count == 0 {
                break;
            }
            let message = if reason.is_some() {
                "Language service exceeded the native derived-cache budget"
            } else {
                "Language service stopped because native derived-cache growth could not be checked"
            };
            for entry in owners {
                stop(inner, entry, message);
            }
            excess = excess.saturating_sub(bytes(&data));
            count = count.saturating_sub(1);
        }
    }
    let mut usage = inner.resources.lock().unwrap();
    usage.cache_bytes = measured;
    usage.cache_sample_complete = complete;
    usage.peak_cache_bytes = usage.peak_cache_bytes.max(measured);
}

fn stop(inner: &Inner, entry: &Entry, message: &str) {
    {
        let mut failure = entry.failure.lock().unwrap();
        failure.until = Some(Instant::now() + Duration::from_secs(5));
        failure.reason = message.into();
    }
    entry.stop();
    let mut usage = inner.resources.lock().unwrap();
    usage.cache_evictions = usage.cache_evictions.saturating_add(1);
}

#[cfg(test)]
mod tests;
