//! Exclusive process handoff between matching, idle editing clients. Retain each
//! client's source inventory separately; never share a server's live overlays.
use super::*;

pub(super) struct Warm {
    session: Session,
    closes: Vec<String>,
}
impl Warm {
    pub(super) fn cache(&self) -> Option<&cache::Use> {
        self.session.cache.as_ref()
    }
    pub fn identity(&self) -> Option<ProcessIdentity> {
        ProcessIdentity::capture(&self.session.process.lock().unwrap())
    }
    async fn adopt(mut self) -> Result<Session, String> {
        self.session.check_context().await?;
        if !self
            .session
            .inputs
            .is_baseline(&self.session.inputs.observe(&self.session.root).await?)
        {
            return Err("Declared project inputs changed before service handoff".into());
        }
        self.session.diagnostics.lock().unwrap().clear();
        for uri in &self.closes {
            self.session
                .client
                .notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}))
                .await
                .map_err(|error| error.to_string())?;
        }
        // Old action/resource IDs cannot become capabilities in another client.
        self.session.id = format!("{:032x}", rand::random::<u128>());
        self.session.analysis = analysis::Analysis::default();
        *self.session.commands.lock().unwrap() = commands::Commands::default();
        Ok(self.session)
    }
}
pub(super) fn compatible(left: &Key, right: &Key) -> bool {
    left.owner == right.owner
        && left.project == right.project
        && left.root == right.root
        && left.workspace == right.workspace
        && left.provider == right.provider
        && left.digest == right.digest
        && left.server == right.server
}
pub(super) fn eligible(entry: &Entry) -> bool {
    let stopped = *entry.stopped.borrow();
    !entry.busy()
        && !stopped
        && entry.session.get().is_some_and(|session| {
            !session.client.is_closed() && session.commands.lock().unwrap().reusable()
        })
}
fn detach(entry: &mut Entry) -> Option<Warm> {
    let mut session = entry.session.take()?;
    let (documents, closes) = Arc::get_mut(&mut session.documents)?.get_mut().detach();
    *entry.retained.get_mut().unwrap() = Some(documents);
    *entry.starting.get_mut().unwrap() = None;
    Some(Warm { session, closes })
}
/// Acquisition and physical ownership transfer share the eviction lock. A busy
/// entry, including a not-yet-polled request, is never an eligible donor.
pub(super) fn offer(entries: &mut BTreeMap<Key, Arc<Entry>>, key: &Key, target: &Arc<Entry>) {
    if eligible(target) {
        release_idle(entries, key);
        return;
    }
    if target.busy() || target.session.get().is_some() || target.handoff.lock().unwrap().is_some() {
        return;
    }
    let candidate = entries
        .iter()
        .filter(|(candidate, entry)| {
            *candidate != key
                && compatible(candidate, key)
                && Arc::strong_count(entry) == 1
                && eligible(entry)
        })
        .max_by_key(|(_, entry)| *entry.used.lock().unwrap())
        .map(|(key, _)| key.clone());
    let Some(candidate) = candidate else { return };
    let Some(entry) = entries.remove(&candidate) else {
        return;
    };
    match Arc::try_unwrap(entry) {
        Ok(mut donor) => {
            if let Some(warm) = detach(&mut donor) {
                *target.starting.lock().unwrap() = warm.identity();
                *target.handoff.lock().unwrap() = Some(warm);
            }
            entries.insert(candidate, Arc::new(donor));
        }
        Err(entry) => {
            // An outcome observer acquired the donor at the lookup boundary.
            entries.insert(candidate, entry);
        }
    }
}

/// Overlapping requests may need independent processes. Once both are reusable
/// and idle, retain one process and keep the other client's complete source bank.
/// The same acquisition lock protects selection, activity registration and detach.
fn release_idle(entries: &mut BTreeMap<Key, Arc<Entry>>, keep: &Key) {
    if !entries.get(keep).is_some_and(|entry| eligible(entry)) {
        return;
    }
    let candidates = entries
        .iter()
        .filter(|(key, entry)| {
            *key != keep
                && compatible(key, keep)
                && Arc::strong_count(entry) == 1
                && eligible(entry)
        })
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    for key in candidates {
        let Some(entry) = entries.remove(&key) else {
            continue;
        };
        match Arc::try_unwrap(entry) {
            Ok(mut entry) => {
                // Dropping Session stops its original process tree. Its cache
                // lease remains protected until that cleanup is confirmed.
                drop(detach(&mut entry));
                entries.insert(key, Arc::new(entry));
            }
            Err(entry) => {
                entries.insert(key, entry);
            }
        }
    }
}

/// Converge after a burst even if no further foreground query arrives. Active
/// requests and pinned command reviews keep their exact services, as before.
pub(super) fn consolidate_idle(entries: &mut BTreeMap<Key, Arc<Entry>>) {
    let mut candidates = entries
        .iter()
        .filter(|(_, entry)| eligible(entry))
        .map(|(key, entry)| (key.clone(), *entry.used.lock().unwrap()))
        .collect::<Vec<_>>();
    candidates.sort_unstable_by_key(|(_, used)| std::cmp::Reverse(*used));
    for (key, _) in candidates {
        release_idle(entries, &key);
    }
}
/// Capacity eviction can reuse a compatible process too. Its logical source
/// inventory expires under the same bounded LRU policy as before.
pub(super) fn retire(entry: Arc<Entry>, old: &Key, new: &Key) -> Option<Warm> {
    if compatible(old, new) && eligible(&entry) {
        match Arc::try_unwrap(entry) {
            Ok(mut entry) => return detach(&mut entry),
            Err(entry) => entry.stop(),
        }
    } else {
        entry.stop();
    }
    None
}
pub(super) fn snapshot(entry: &Entry) -> Option<documents::Documents> {
    if let Some(documents) = entry.retained.lock().unwrap().as_ref() {
        return Some(documents.snapshot());
    }
    entry
        .session
        .get()?
        .documents
        .try_lock()
        .ok()
        .map(|documents| documents.snapshot())
}
/// Initialization cancellation must put the source bank back. The server may
/// have received only some didOpen messages; its owning Session is then dropped.
struct Restore<'a> {
    entry: &'a Entry,
    documents: Option<documents::Documents>,
}
impl Drop for Restore<'_> {
    fn drop(&mut self) {
        if let Some(documents) = self.documents.take() {
            *self.entry.retained.lock().unwrap() = Some(documents);
        }
    }
}
pub(super) async fn initialize(
    scope: &Scope,
    workspace: &std::path::Path,
    entry: &Entry,
    inner: &Inner,
) -> Result<Session, String> {
    let warm = entry.handoff.lock().unwrap().take();
    let _starting = StartingProcess(&entry.starting);
    let mut session = if let Some(warm) = warm {
        warm.adopt().await?
    } else {
        Session::start(scope, workspace, entry, inner).await?
    };
    *entry.starting.lock().unwrap() = ProcessIdentity::capture(&session.process.lock().unwrap());
    let mut restore = Restore {
        entry,
        documents: entry.retained.lock().unwrap().take(),
    };
    if let Some(documents) = restore.documents.as_mut() {
        documents.restore(&session).await?;
        *Arc::get_mut(&mut session.documents)
            .ok_or("Language sources were retained during initialization")?
            .get_mut() = restore.documents.take().unwrap();
    }
    Ok(session)
}

#[cfg(test)]
pub(super) mod tests;
