//! Bounded command receipts independent of a process/document cache lifetime.
//! Receivers retain results, never process handles or workspace source buffers.
use super::*;
use std::sync::Weak;

const MAX_OUTCOMES: usize = 64;
const MAX_OUTCOME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(super) struct CommandScope {
    owner: String,
    project: i64,
    provider: String,
    digest: String,
    server: String,
    client: String,
}
impl CommandScope {
    pub(super) fn new(scope: &Scope, invocation: &CommandInvocation) -> Result<Self, String> {
        invocation.validate()?;
        if scope.owner.is_empty() || scope.project <= 0 {
            return Err("Language command requires an owned project".into());
        }
        let mut server = scope.server.clone();
        server.language_id.take();
        Ok(Self {
            owner: scope.owner.clone(),
            project: scope.project,
            provider: scope.provider.clone(),
            digest: scope.digest.clone(),
            server: serde_json::to_string(&server).map_err(|e| e.to_string())?,
            client: invocation.client.clone(),
        })
    }
    pub(super) fn matches(&self, key: &Key) -> bool {
        key.owner == self.owner
            && key.project == self.project
            && key.provider == self.provider
            && key.digest == self.digest
            && key.server == self.server
            && key.client == self.client
    }
}
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct OutcomeKey {
    scope: CommandScope,
    server: String,
    command: String,
}
impl OutcomeKey {
    fn new(scope: CommandScope, invocation: &CommandInvocation) -> Self {
        Self {
            scope,
            server: invocation.server.clone(),
            command: invocation.command.clone(),
        }
    }
}
struct Receipt {
    invocation: CommandInvocation,
    progress: watch::Receiver<CommandProgress>,
    entry: Weak<Entry>,
    termination: Option<crate::exec::proc::Cleanup>,
    created: Instant,
    reserved: usize,
}
impl Receipt {
    fn progress(&self) -> CommandProgress {
        let value = self.progress.borrow().clone();
        if matches!(value, CommandProgress::Ready | CommandProgress::Running)
            && self.progress.has_changed().is_err()
        {
            return unavailable();
        }
        value
    }
}
#[derive(Default)]
pub(in crate::languages::host) struct Outcomes {
    records: BTreeMap<OutcomeKey, Receipt>,
}
impl Outcomes {
    pub(in crate::languages::host) fn prune(&mut self) {
        self.records.retain(|_, receipt| {
            receipt.created.elapsed() < COMMAND_LIFETIME
                || matches!(
                    receipt.progress(),
                    CommandProgress::Ready | CommandProgress::Running
                )
        });
    }
    /// Reserve the largest valid outcome before dispatch. Capacity exhaustion
    /// rejects new execution, rather than evicting another pending receipt.
    pub(super) fn record(
        &mut self,
        scope: &CommandScope,
        invocation: &CommandInvocation,
        entry: &Arc<Entry>,
        progress: &watch::Sender<CommandProgress>,
    ) -> Result<Admission, String> {
        self.prune();
        let key = OutcomeKey::new(scope.clone(), invocation);
        if let Some(existing) = self.records.get(&key) {
            if existing.invocation != *invocation {
                return Err("Language command receipt belongs to another approval".into());
            }
            return Ok(Admission::Existing(existing.progress()));
        }
        // Include duplicated lookup strings, map/node overhead and the complete
        // bounded future outcome, not just its initial Running value.
        let reserved = serde_json::to_vec(&(scope, invocation))
            .map_err(|e| e.to_string())?
            .len()
            .saturating_mul(2)
            .saturating_add(4096)
            .saturating_add(protocol::actions::MAX_COMMAND_BYTES);
        if self.records.len() >= MAX_OUTCOMES
            || self
                .records
                .values()
                .map(|receipt| receipt.reserved)
                .sum::<usize>()
                .saturating_add(reserved)
                > MAX_OUTCOME_BYTES
        {
            return Ok(Admission::Full);
        }
        self.records.insert(
            key,
            Receipt {
                invocation: invocation.clone(),
                progress: progress.subscribe(),
                entry: Arc::downgrade(entry),
                termination: entry
                    .session
                    .get()
                    .map(|session| session.process.lock().unwrap().termination()),
                created: Instant::now(),
                reserved,
            },
        );
        Ok(Admission::New)
    }
    pub(super) fn get(
        &mut self,
        scope: &CommandScope,
        invocation: &CommandInvocation,
        stop: bool,
    ) -> Result<Option<CommandProgress>, String> {
        self.prune();
        let Some(receipt) = self
            .records
            .get(&OutcomeKey::new(scope.clone(), invocation))
        else {
            return Ok(None);
        };
        if receipt.invocation != *invocation {
            return Err("Language command receipt belongs to another approval".into());
        }
        let progress = receipt.progress();
        if stop
            && matches!(
                &progress,
                CommandProgress::Ready
                    | CommandProgress::Running
                    | CommandProgress::Finished {
                        outcome: CommandOutcome::Uncertain { .. }
                    }
            )
            && let Some(entry) = receipt.entry.upgrade()
        {
            entry.stop();
        }
        Ok(Some(progress))
    }
    pub(super) fn fence(
        &mut self,
        scope: &CommandScope,
        invocation: &CommandInvocation,
    ) -> Result<(Option<CommandProgress>, Option<crate::exec::proc::Cleanup>), String> {
        let progress = self.get(scope, invocation, true)?;
        let completion = progress
            .as_ref()
            .filter(|value| needs_fence(value))
            .and_then(|_| {
                self.records
                    .get(&OutcomeKey::new(scope.clone(), invocation))
                    .and_then(|receipt| receipt.termination.clone())
            });
        Ok((progress, completion))
    }
}
pub(super) enum Admission {
    New,
    Existing(CommandProgress),
    Full,
}

/// Created before spawning, so cancellation before the first poll, task panic
/// and ordinary completion all leave a terminal, bounded recovery result.
pub(super) struct Completion(pub(super) watch::Sender<CommandProgress>);
impl Completion {
    pub(super) fn finish(self, outcome: CommandOutcome) {
        let outcome = match outcome.validate() {
            Ok(()) => outcome,
            Err(message) if matches!(outcome, CommandOutcome::Rejected { .. }) => {
                CommandOutcome::Rejected { message }
            }
            Err(message) => CommandOutcome::Uncertain { message },
        };
        self.0.send_replace(CommandProgress::Finished { outcome });
    }
}
impl Drop for Completion {
    fn drop(&mut self) {
        let pending = matches!(
            *self.0.borrow(),
            CommandProgress::Ready | CommandProgress::Running
        );
        if pending {
            self.0.send_replace(CommandProgress::Finished { outcome: CommandOutcome::Uncertain {
                message: "The original command worker ended without reporting its outcome; review effects before continuing".into(),
            }});
        }
    }
}

#[cfg(test)]
mod tests;
