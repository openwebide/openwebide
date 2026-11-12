//! One speculative request per execution host, yielding to interactive work.
//! Admission never starts a cold process or evicts a service to make room.
use super::*;

pub(super) const TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const SUSPENDED: &str =
    "Language prefetch is suspended for this process after an interrupted request";

/// Count discovery as well as dispatched RPCs. Cancellation signals the owner;
/// it never steals the slot before that owner's future has actually unwound.
#[derive(Default)]
pub(super) struct Queue(Mutex<Option<watch::Sender<bool>>>);
pub(super) struct Permit<'a> {
    queue: &'a Queue,
    pub cancelled: watch::Receiver<bool>,
}
impl Queue {
    pub fn acquire(&self) -> Result<Permit<'_>, String> {
        let mut active = self.0.lock().unwrap();
        if active.is_some() {
            return Err(
                "Language prefetch deferred while another speculative request is active".into(),
            );
        }
        let (cancel, cancelled) = watch::channel(false);
        *active = Some(cancel);
        Ok(Permit {
            queue: self,
            cancelled,
        })
    }
    fn interrupt(&self) {
        if let Some(cancel) = self.0.lock().unwrap().as_ref() {
            cancel.send_replace(true);
        }
    }
}
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.queue.0.lock().unwrap().take();
    }
}

pub(super) fn admit(
    inner: &Inner,
    entries: &BTreeMap<Key, Arc<Entry>>,
    key: &Key,
    input: &Request,
) -> Result<(), String> {
    if input.query.is_prefetch() {
        if entries.values().any(|entry| entry.busy()) {
            return Err("Language prefetch deferred while execution-host services are busy".into());
        }
        if entries.get(key).is_none_or(|entry| *entry.stopped.borrow()) {
            return Err("Language prefetch requires an existing editing-client service".into());
        }
        if entries
            .get(key)
            .and_then(|entry| entry.session.get())
            .is_some_and(|session| {
                !session
                    .prefetch_enabled
                    .load(std::sync::atomic::Ordering::Relaxed)
            })
        {
            return Err(SUSPENDED.into());
        }
        // A parked client retains its sources, not a warm-process claim.
        // Optional work must not take the process back from another window.
        if !entries.get(key).is_some_and(|entry| reuse::eligible(entry)) {
            return Err("Language prefetch requires the editing client's own warm service".into());
        }
        return Ok(());
    }
    inner.prefetch.interrupt();
    for entry in entries.values() {
        for request in entry.activity.lock().unwrap().active.values() {
            if request.prefetch {
                request.cancel.send_replace(true);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
