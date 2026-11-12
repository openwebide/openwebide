//! Retained server commands and single-use native execution. Both transports
//! must obtain a durable UI approval/claim before calling this trusted adapter.
use super::*;
use openwebide_core::{
    Vfs,
    plugins::languages::{
        intelligence::{
            actions::{
                ActionReview, CommandInvocation, CommandOutcome, CommandProgress, ServerCommand,
                origin_fingerprint,
            },
            results::{WorkspaceChange, WorkspaceEdit},
            workspace_edit::WorkspaceDraft,
        },
        source_digest,
    },
    workspace_transaction::{EntryState, VfsTransactionFiles},
};
use serde::Serialize;
use std::sync::atomic::Ordering;
mod outcomes;
use outcomes::CommandScope;
pub(super) use outcomes::Outcomes;

pub(super) const COMMAND_LIFETIME: Duration = Duration::from_secs(300);
const MAX_COMMANDS: usize = 128;
const MAX_CACHE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Default)]
pub(super) struct Commands {
    slots: BTreeMap<String, Slot>,
}
struct Slot {
    data: Arc<CommandData>,
    bytes: usize,
    created: Instant,
    invocation: Option<(String, String)>,
    progress: watch::Sender<CommandProgress>,
    synchronizing: Option<Instant>,
    input_revision: u64,
    synchronized_inputs: Option<u64>,
}
impl Commands {
    pub(super) fn reusable(&self) -> bool {
        self.slots
            .values()
            .all(|slot| match &*slot.progress.borrow() {
                CommandProgress::Finished {
                    outcome: CommandOutcome::Completed { .. } | CommandOutcome::Rejected { .. },
                } => true,
                CommandProgress::Ready => slot.created.elapsed() >= COMMAND_LIFETIME,
                CommandProgress::Running
                | CommandProgress::Finished {
                    outcome: CommandOutcome::Uncertain { .. },
                } => false,
            })
    }
}
#[derive(Serialize)]
struct CommandData {
    origin: String,
    review: String,
    command: ServerCommand,
    expected: BTreeMap<String, ExpectedEntry>,
    project_inputs: inputs::Snapshot,
}
#[derive(Serialize)]
enum ExpectedEntry {
    Missing,
    Directory,
    File(String),
}

pub(super) async fn prepare(
    documents: &documents::Documents,
    session: &Session,
    input: &Request,
    raw: Value,
    edit: WorkspaceEdit,
    created: Instant,
) -> Result<ActionReview, String> {
    let Query::ReviewAction { id } = &input.query else {
        return Err("A command can only originate in a reviewed code action".into());
    };
    let raw_command = match raw.get("command") {
        Some(Value::String(_)) => Some(&raw),
        Some(value @ Value::Object(_)) => Some(value),
        None | Some(Value::Null) => None,
        _ => return Err("Invalid server command in code action".into()),
    };
    let command = if let Some(value) = raw_command {
        let name = value["command"]
            .as_str()
            .ok_or("Server command name is missing")?;
        if !session
            .capabilities
            .pointer("/executeCommandProvider/commands")
            .and_then(Value::as_array)
            .is_some_and(|commands| commands.iter().any(|value| value.as_str() == Some(name)))
        {
            return Err(
                "The server did not advertise this command; no part of the action was applied"
                    .into(),
            );
        }
        let arguments = match value.get("arguments") {
            None | Some(Value::Null) => vec![],
            Some(Value::Array(arguments)) => arguments.clone(),
            _ => return Err("Server command arguments must be an array".into()),
        };
        Some(ServerCommand {
            id: id.clone(),
            server: session.id.clone(),
            name: name.into(),
            arguments,
        })
    } else {
        None
    };
    let review = ActionReview {
        title: raw["title"]
            .as_str()
            .ok_or("Code action title is missing")?
            .into(),
        edit,
        command,
    };
    review.validate()?;
    let Some(command) = review.command.clone() else {
        return Ok(review);
    };
    let vfs = crate::runs::native_vfs::NativeFsVfs {
        root: session.root.clone(),
    };
    let files = VfsTransactionFiles {
        vfs: &vfs,
        identity: "language-command-review",
    };
    let changes = review.edit.clone().ordered()?;
    // Moving the server's working directory would invalidate its process and
    // URI state. Reject the complete action while it is still only a preview.
    if changes.iter().any(|change| match change {
        WorkspaceChange::File { operation } => operation
            .paths()
            .iter()
            .any(|path| session.workspace.starts_with(session.root.join(path))),
        WorkspaceChange::Text { .. } => false,
    }) {
        return Err(
            "This command would replace its language workspace; no part of the action was applied"
                .into(),
        );
    }
    let operations = changes
        .iter()
        .filter_map(|change| match change {
            WorkspaceChange::File { operation } => Some(operation.clone()),
            WorkspaceChange::Text { .. } => None,
        })
        .collect();
    let retained = documents.workspace_documents(&session.root);
    let order = retained.keys().cloned().collect();
    let mut draft = WorkspaceDraft::prepare(
        &files,
        files.identity.into(),
        operations,
        retained,
        order,
        None,
        || true,
    )
    .await?;
    for change in changes {
        match change {
            WorkspaceChange::Text { file } => {
                draft.document(&files, &file.path, || true).await?;
                draft.text(file)?;
            }
            WorkspaceChange::File { operation } => draft.resource(operation)?,
        }
    }
    draft.finish()?;
    let paths = draft
        .plan
        .changed_paths()
        .into_iter()
        .map(str::to_owned)
        .chain(draft.documents.keys().cloned())
        .collect::<std::collections::BTreeSet<_>>();
    let mut expected = BTreeMap::new();
    for path in paths {
        let entry = if let Some(document) = draft.documents.get(&path) {
            ExpectedEntry::File(source_digest(document.document.text()))
        } else {
            match draft.plan.after.get(&path) {
                Some(EntryState::Directory) => ExpectedEntry::Directory,
                Some(EntryState::File { bytes }) => {
                    ExpectedEntry::File(openwebide_core::plugins::content_digest(bytes))
                }
                _ => ExpectedEntry::Missing,
            }
        };
        expected.insert(path, entry);
    }
    let data = CommandData {
        origin: origin_fingerprint(input)?,
        review: review.fingerprint()?,
        command,
        expected,
        project_inputs: session.inputs.after_plan(&session.root, &draft.plan),
    };
    // Reserve the bounded outcome before accepting a review, so lost-response
    // recovery cannot grow the cache beyond its declared payload budget.
    let bytes = serde_json::to_vec(&data)
        .map_err(|e| e.to_string())?
        .len()
        .saturating_add(protocol::actions::MAX_COMMAND_BYTES);
    let input_revision = session.inputs.revision().await;
    let mut cache = session.commands.lock().unwrap();
    cache.slots.retain(|_, slot| {
        slot.created.elapsed() < COMMAND_LIFETIME
            || *slot.progress.borrow() == CommandProgress::Running
    });
    if let Some(existing) = cache.slots.get(id) {
        if existing.data.origin == data.origin && existing.data.review == data.review {
            return Ok(review);
        }
        return Err("A retained language action changed; request code actions again".into());
    }
    if cache.slots.len() >= MAX_COMMANDS
        || cache
            .slots
            .values()
            .map(|slot| slot.bytes)
            .sum::<usize>()
            .saturating_add(bytes)
            > MAX_CACHE_BYTES
    {
        return Err(
            "Language command review capacity is full; wait for older reviews to expire".into(),
        );
    }
    cache.slots.insert(
        id.clone(),
        Slot {
            data: Arc::new(data),
            bytes,
            created,
            invocation: None,
            progress: watch::channel(CommandProgress::Ready).0,
            synchronizing: None,
            input_revision,
            synchronized_inputs: None,
        },
    );
    Ok(review)
}

impl Host {
    async fn command_entry(
        &self,
        mut scope: Scope,
        invocation: &CommandInvocation,
        require_root: bool,
    ) -> Result<(CommandScope, Option<Arc<Entry>>), String> {
        let context = CommandScope::new(&scope, invocation)?;
        if require_root {
            scope.root = tokio::fs::canonicalize(&scope.root)
                .await
                .map_err(|e| e.to_string())?;
        }
        let entries = self.inner.entries.lock().unwrap();
        let entry = entries
            .iter()
            .find(|(key, entry)| {
                context.matches(key)
                    && (!require_root || key.root == scope.root)
                    && entry
                        .session
                        .get()
                        .is_some_and(|session| session.id == invocation.server)
            })
            .map(|(_, entry)| entry.clone());
        // Register while holding the entries lock, so capacity eviction cannot
        // race acquisition. Memory-pressure eviction may still stop a command.
        if let Some(entry) = &entry {
            entry.active_uses.fetch_add(1, Ordering::Relaxed);
        }
        Ok((context, entry))
    }
    /// Trusted adapters call this only after a durable claim. No raw executable,
    /// command name or arguments are accepted. Duplicate dispatch joins the same
    /// native outcome, including while its original HTTP response is lost.
    pub async fn start_command(
        &self,
        scope: Scope,
        invocation: CommandInvocation,
    ) -> Result<CommandProgress, String> {
        let (context, entry) = self.command_entry(scope, &invocation, true).await?;
        let guard = entry.as_ref().map(|entry| ServiceUse(entry.clone()));
        if let Some(progress) =
            self.inner
                .outcomes
                .lock()
                .unwrap()
                .get(&context, &invocation, false)?
        {
            return Ok(progress);
        }
        let entry = entry
            .ok_or("The original language service is unavailable; the command was not restarted")?;
        let guard = guard.expect("Found command entry has an active guard");
        let session = entry
            .session
            .get()
            .ok_or("The original language service was lost")?;
        let (data, input_revision, progress, registration) = {
            let mut commands = session.commands.lock().unwrap();
            let Some(slot) = commands.slots.get_mut(&invocation.command) else {
                return self
                    .inner
                    .outcomes
                    .lock()
                    .unwrap()
                    .get(&context, &invocation, false)?
                    .ok_or_else(|| "The reviewed language command is missing or expired".into());
            };
            if slot.data.origin != invocation.origin || slot.data.review != invocation.review {
                return Err("Language command does not match its reviewed source and edit".into());
            }
            if let Some((id, approval)) = &slot.invocation {
                if id == &invocation.id && approval == &invocation.approval {
                    return Ok(slot.progress.borrow().clone());
                }
                return Err(
                    "This reviewed command was already used; it will not be replayed".into(),
                );
            }
            if slot.created.elapsed() >= COMMAND_LIFETIME {
                return Err("Language command review expired; request the action again".into());
            }
            let retained = self.inner.outcomes.lock().unwrap().record(
                &context,
                &invocation,
                &entry,
                &slot.progress,
            )?;
            if let outcomes::Admission::Existing(progress) = retained {
                return Ok(progress);
            }
            slot.invocation = Some((invocation.id, invocation.approval));
            if matches!(retained, outcomes::Admission::Full) {
                let progress = CommandProgress::Finished { outcome: CommandOutcome::Rejected {
                    message: "Native command recovery capacity is full; the command was not dispatched".into(),
                }};
                slot.progress.send_replace(progress.clone());
                return Ok(progress);
            }
            let registration = match self.inner.coordinator.worker() {
                Ok(registration) => registration,
                Err(message) => {
                    let progress = CommandProgress::Finished {
                        outcome: CommandOutcome::Rejected { message },
                    };
                    slot.progress.send_replace(progress.clone());
                    return Ok(progress);
                }
            };
            slot.progress.send_replace(CommandProgress::Running);
            (
                slot.data.clone(),
                slot.synchronized_inputs.unwrap_or(slot.input_revision),
                slot.progress.clone(),
                registration,
            )
        };
        let completion = outcomes::Completion(progress);
        let coordinator = self.inner.coordinator.clone();
        tokio::spawn(async move {
            let _registration = registration;
            let outcome = coordinated_execute(&coordinator, &entry, &data, input_revision).await;
            completion.finish(outcome);
            drop(guard);
        });
        Ok(CommandProgress::Running)
    }
    pub async fn command_progress(
        &self,
        scope: Scope,
        invocation: &CommandInvocation,
    ) -> Result<CommandProgress, String> {
        let (context, entry) = self.command_entry(scope, invocation, false).await?;
        let Some(entry) = entry else {
            return self
                .inner
                .outcomes
                .lock()
                .unwrap()
                .get(&context, invocation, false)
                .map(|progress| progress.unwrap_or_else(unavailable));
        };
        let _guard = ServiceUse(entry.clone());
        let session = entry
            .session
            .get()
            .ok_or("The original language service was lost")?;
        let commands = session.commands.lock().unwrap();
        let Some(slot) = commands.slots.get(&invocation.command) else {
            return self
                .inner
                .outcomes
                .lock()
                .unwrap()
                .get(&context, invocation, false)
                .map(|progress| progress.unwrap_or_else(unavailable));
        };
        if slot
            .invocation
            .as_ref()
            .is_some_and(|claim| claim != &(invocation.id.clone(), invocation.approval.clone()))
            || slot.data.origin != invocation.origin
            || slot.data.review != invocation.review
        {
            return Err("Language command outcome belongs to another approval".into());
        }
        Ok(slot.progress.borrow().clone())
    }

    /// Fence a claimed command before its uncertainty is acknowledged. A ready
    /// slot receives a tombstone; an in-flight RPC stops its original service.
    /// Neither path creates or restarts a process, including after disconnect.
    pub async fn stop_command(
        &self,
        scope: Scope,
        invocation: &CommandInvocation,
    ) -> Result<CommandProgress, String> {
        let (context, entry) = self.command_entry(scope.clone(), invocation, false).await?;
        let Some(entry) = entry else {
            return self.stop_retained(&context, invocation).await;
        };
        let _guard = ServiceUse(entry.clone());
        let progress = self.stop_command_now(&entry, &context, invocation)?;
        if needs_fence(&progress) || *entry.stopped.borrow() {
            entry.stop_and_wait().await?;
            self.command_progress(scope, invocation).await
        } else {
            Ok(progress)
        }
    }
    async fn stop_retained(
        &self,
        context: &CommandScope,
        invocation: &CommandInvocation,
    ) -> Result<CommandProgress, String> {
        let (progress, completion) = self
            .inner
            .outcomes
            .lock()
            .unwrap()
            .fence(context, invocation)?;
        if let Some(completion) = completion {
            completion.wait().await?;
        }
        Ok(progress.unwrap_or_else(unavailable))
    }
    fn stop_command_now(
        &self,
        entry: &Arc<Entry>,
        context: &CommandScope,
        invocation: &CommandInvocation,
    ) -> Result<CommandProgress, String> {
        let session = entry
            .session
            .get()
            .ok_or("The original language service was lost")?;
        let progress = {
            let mut commands = session.commands.lock().unwrap();
            let Some(slot) = commands.slots.get_mut(&invocation.command) else {
                entry.stop();
                return self
                    .inner
                    .outcomes
                    .lock()
                    .unwrap()
                    .get(context, invocation, true)
                    .map(|progress| progress.unwrap_or_else(unavailable));
            };
            if slot.data.origin != invocation.origin
                || slot.data.review != invocation.review
                || slot.invocation.as_ref().is_some_and(|claim| {
                    claim != &(invocation.id.clone(), invocation.approval.clone())
                })
            {
                return Err("Language command belongs to another approval".into());
            }
            if slot.invocation.is_none() {
                // A stopped-before-start tombstone must outlive its service too.
                // At capacity the local tombstone still fences execution; its
                // response can be persisted by the durable action journal.
                if let outcomes::Admission::Existing(progress) = self
                    .inner
                    .outcomes
                    .lock()
                    .unwrap()
                    .record(context, invocation, entry, &slot.progress)?
                {
                    return Ok(progress);
                }
                slot.invocation = Some((invocation.id.clone(), invocation.approval.clone()));
                slot.progress.send_replace(CommandProgress::Finished {
                    outcome: CommandOutcome::Rejected {
                        message: "The approved command was stopped before dispatch".into(),
                    },
                });
            }
            slot.progress.clone()
        };
        if matches!(
            *progress.borrow(),
            CommandProgress::Running
                | CommandProgress::Finished {
                    outcome: CommandOutcome::Uncertain { .. }
                }
        ) {
            entry.stop();
        }
        Ok(progress.borrow().clone())
    }
}

fn unavailable() -> CommandProgress {
    CommandProgress::Finished { outcome: CommandOutcome::Uncertain { message: "The original language service or command outcome is unavailable. The command will not be restarted; review any effects before continuing.".into() } }
}

fn pending(slot: &Slot) -> bool {
    match *slot.progress.borrow() {
        CommandProgress::Running => true,
        CommandProgress::Ready => {
            slot.created.elapsed() < COMMAND_LIFETIME
                && slot.invocation.is_none()
                && slot
                    .synchronizing
                    .is_some_and(|since| since.elapsed() < COMMAND_LIFETIME)
        }
        CommandProgress::Finished { .. } => false,
    }
}
pub(super) fn has_pending_action(session: &Session) -> bool {
    session.commands.lock().unwrap().slots.values().any(pending)
}
/// Only the complete original prepared command may cross native admission.
/// Ordinary queries keep their existing fence and never call this exception.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn validate_workspace_review(
    session: &Session,
    input: &Request,
    result: &Value,
    input_revision: u64,
) -> Result<(), String> {
    let review = if matches!(input.query, Query::ReviewAction { .. }) {
        Some(
            serde_json::from_value::<ActionReview>(result.clone())
                .map_err(|_| "Invalid original action review")?,
        )
    } else {
        None
    };
    let expected = review.as_ref().and_then(|review| review.command.as_ref());
    let origin = origin_fingerprint(input)?;
    let fingerprint = review.as_ref().map(ActionReview::fingerprint).transpose()?;
    let commands = session.commands.lock().unwrap();
    let mut matched = false;
    for slot in commands.slots.values().filter(|slot| pending(slot)) {
        if expected.is_some_and(|command| command == &slot.data.command)
            && *slot.progress.borrow() == CommandProgress::Ready
            && slot.invocation.is_none()
            && slot.synchronized_inputs.is_none()
            && slot.input_revision == input_revision
            && slot.data.origin == origin
            && Some(&slot.data.review) == fingerprint.as_ref()
        {
            matched = true;
        } else {
            return Err(
                "Another or already claimed command prevents native language review admission"
                    .into(),
            );
        }
    }
    if expected.is_some() && !matched {
        return Err(
            "Prepare the complete original language command before native admission".into(),
        );
    }
    Ok(())
}
/// Cancelled or acknowledged durable actions release their transitional input
/// pin. A claimed command keeps its independent execution/outcome ownership.
pub(super) fn release_synchronization(session: &Session, input: &Request) -> Result<(), String> {
    let Query::ReleaseAction { action } = &input.query else {
        return Err("Expected reviewed action release".into());
    };
    if action.server != session.id {
        return Err("The reviewed action belongs to another language service".into());
    }
    let mut commands = session.commands.lock().unwrap();
    if let Some(slot) = commands.slots.get(&action.command) {
        if slot.data.origin != action.origin || slot.data.review != action.review {
            return Err("The released action does not match its reviewed source and edit".into());
        }
        if slot.invocation.is_some() {
            return Ok(());
        }
        commands.slots.remove(&action.command);
    }
    Ok(())
}

pub(super) fn prepare_synchronization(
    session: &Session,
    input: &Request,
    current: &inputs::Snapshot,
    documents: &documents::Documents,
    path: &std::path::Path,
) -> Result<(), String> {
    let Query::PrepareAction { action } = &input.query else {
        return Err("Expected reviewed action preparation".into());
    };
    if action.server != session.id
        || *current != session.inputs.baseline
        || !documents.has_source(input, path)
    {
        return Err(
            "The reviewed action's project inputs or original source changed before application"
                .into(),
        );
    }
    let mut commands = session.commands.lock().unwrap();
    let slot = commands
        .slots
        .get_mut(&action.command)
        .filter(|slot| slot.created.elapsed() < COMMAND_LIFETIME)
        .ok_or("The reviewed action is missing or expired")?;
    if slot.invocation.is_some()
        || *slot.progress.borrow() != CommandProgress::Ready
        || slot.data.origin != action.origin
        || slot.data.review != action.review
        || origin_fingerprint(input)? != action.origin
    {
        return Err("The reviewed action preparation changed or was already claimed".into());
    }
    slot.synchronizing = Some(Instant::now());
    Ok(())
}
pub(super) fn verify_synchronization(
    session: &Session,
    input: &Request,
    current: &inputs::Observation,
) -> Result<(), String> {
    let Query::SynchronizeAction { action, close } = &input.query else {
        return Err("Expected reviewed action synchronization".into());
    };
    if action.server != session.id {
        return Err("The reviewed action belongs to another language service".into());
    }
    let mut commands = session.commands.lock().unwrap();
    let slot = commands
        .slots
        .get_mut(&action.command)
        .filter(|slot| slot.created.elapsed() < COMMAND_LIFETIME)
        .ok_or("The reviewed action is missing or expired")?;
    if slot.invocation.is_some()
        || *slot.progress.borrow() != CommandProgress::Ready
        || slot.data.origin != action.origin
        || slot.data.review != action.review
    {
        return Err("The reviewed action synchronization was changed or already claimed".into());
    }
    if slot.data.project_inputs != current.snapshot {
        return Err(
            "Declared project inputs do not match the reviewed action's complete file plan".into(),
        );
    }
    if *close {
        if !slot.data.expected.contains_key(&input.path) {
            return Err("The reviewed action does not own this closing source".into());
        }
    } else if !matches!(slot.data.expected.get(&input.path),Some(ExpectedEntry::File(digest)) if *digest==input.source_digest)
    {
        return Err(
            "The source does not match the reviewed action's complete post-edit buffers".into(),
        );
    }
    if slot
        .synchronized_inputs
        .is_some_and(|revision| revision != current.revision)
    {
        return Err(
            "Declared project inputs changed during reviewed action synchronization".into(),
        );
    }
    // The first post-edit synchronization admits the reviewed configuration
    // moves. Later synchronization and dispatch retain this exact observation.
    slot.synchronized_inputs = Some(current.revision);
    Ok(())
}

async fn verify_sources(
    session: &Session,
    documents: &documents::Documents,
    data: &CommandData,
    input_revision: u64,
) -> Result<(), String> {
    verify_inputs(session, data, input_revision).await?;
    let vfs = crate::runs::native_vfs::NativeFsVfs {
        root: session.root.clone(),
    };
    for (path, expected) in &data.expected {
        if vfs.canonicalize(path).await.map_err(|e| e.to_string())? != *path {
            return Err("An affected command path changed through a symbolic link".into());
        }
        let absolute = session.root.join(path);
        let metadata = match tokio::fs::symlink_metadata(&absolute).await {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        let matches = match expected {
            ExpectedEntry::Missing => {
                metadata.is_none() && documents.overlay_source(&absolute).is_none()
            }
            ExpectedEntry::Directory => {
                metadata.is_some_and(|value| value.is_dir())
                    && documents.overlay_source(&absolute).is_none()
            }
            ExpectedEntry::File(digest) => {
                if !metadata.is_some_and(|value| {
                    value.is_file()
                        && value.len()
                            <= u64::try_from(
                                openwebide_core::workspace_transaction::MAX_SNAPSHOT_BYTES,
                            )
                            .unwrap()
                }) {
                    false
                } else if let Some(source) = documents.overlay_source(&absolute) {
                    source_digest(source) == *digest
                } else {
                    openwebide_core::plugins::content_digest(
                        &vfs.read_bytes(path).await.map_err(|e| e.to_string())?,
                    ) == *digest
                }
            }
        };
        if !matches {
            return Err(format!(
                "{path} no longer matches the approved action's complete edit"
            ));
        }
    }
    Ok(())
}
fn needs_fence(progress: &CommandProgress) -> bool {
    matches!(
        progress,
        CommandProgress::Ready
            | CommandProgress::Running
            | CommandProgress::Finished {
                outcome: CommandOutcome::Uncertain { .. }
            }
    )
}
async fn coordinated_execute(
    coordinator: &crate::folder_coordination::Coordinator,
    entry: &Entry,
    data: &CommandData,
    input_revision: u64,
) -> CommandOutcome {
    let Some(session) = entry.session.get() else {
        return CommandOutcome::Rejected {
            message: "The original service is unavailable".into(),
        };
    };
    let mut stopped = entry.stopped.subscribe();
    if *stopped.borrow() {
        return CommandOutcome::Rejected {
            message: "The original service stopped before folder access".into(),
        };
    }
    let folder = match crate::folder_coordination::Folder::capture(&session.root) {
        Ok(folder) => folder,
        Err(message) => return CommandOutcome::Rejected { message },
    };
    let lease = tokio::select! {
        biased;
        _ = stopped.changed() => return CommandOutcome::Rejected { message: "The original service stopped before folder access".into() },
        lease = coordinator.acquire(folder) => match lease {
            Ok(lease) => lease,
            Err(message) => return CommandOutcome::Rejected { message },
        },
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let watcher = {
        let owner = { session.process.lock().unwrap().owner() };
        let retained = match owner {
            Some(owner) => lease.watch_process(&owner).await,
            None => Err("The original process owner is unavailable".to_string()),
        };
        match retained {
            Ok(watcher) => watcher,
            Err(message) => {
                if let Err(cleanup) = entry.stop_and_wait().await {
                    lease.quarantine();
                    return CommandOutcome::Uncertain {
                        message: format!("{message}. Native cleanup is unconfirmed: {cleanup}"),
                    };
                }
                return CommandOutcome::Rejected { message };
            }
        }
    };
    // Context, overlays and affected-file verification occur AFTER queued waits.
    // Panic must stop the original process before releasing its physical lease.
    use futures::FutureExt;
    let work = std::panic::AssertUnwindSafe(execute(entry, data, input_revision)).catch_unwind();
    let mut outcome = tokio::select! {
        biased;
        () = coordinator.stopped() => CommandOutcome::Uncertain { message: "Execution host stopped during command preparation or execution; review effects before continuing".into() },
        result = work => result.unwrap_or_else(|_| CommandOutcome::Uncertain { message: "The native command worker failed; review effects before continuing".into() }),
    };
    if matches!(outcome, CommandOutcome::Uncertain { .. }) || *entry.stopped.borrow() {
        if let Err(message) = entry.stop_and_wait().await {
            lease.quarantine();
            outcome = CommandOutcome::Uncertain {
                message: format!("Native command cleanup could not be confirmed: {message}"),
            };
        }
        return outcome;
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Err(message) = watcher.finish().await {
        if entry.stop_and_wait().await.is_err() {
            lease.quarantine();
        }
        return CommandOutcome::Uncertain {
            message: format!("Native folder release could not be confirmed: {message}"),
        };
    }
    outcome
}

async fn execute(entry: &Entry, data: &CommandData, input_revision: u64) -> CommandOutcome {
    let Some(session) = entry.session.get() else {
        return CommandOutcome::Rejected {
            message: "The original service is unavailable".into(),
        };
    };
    let documents = session.documents.lock().await;
    if let Err(message) = entry.check_context(session).await {
        return CommandOutcome::Rejected { message };
    }
    if let Err(message) = verify_sources(session, &documents, data, input_revision).await {
        return CommandOutcome::Rejected { message };
    }
    dispatch(entry, session, data, input_revision).await
}

async fn verify_inputs(session: &Session, data: &CommandData, revision: u64) -> Result<(), String> {
    let current = session.inputs.observe(&session.root).await?;
    if current.snapshot != data.project_inputs || current.revision != revision {
        return Err("Declared project inputs no longer match the reviewed action".into());
    }
    Ok(())
}

async fn dispatch(
    entry: &Entry,
    session: &Session,
    data: &CommandData,
    input_revision: u64,
) -> CommandOutcome {
    // Verifying a bounded workspace can still yield for many filesystem reads.
    // Recheck native context after those reads, immediately before the mutating
    // RPC, so a changed executable, folder or Git revision cannot slip through
    // an earlier successful context check.
    if let Err(message) = entry.check_context(session).await {
        return CommandOutcome::Rejected { message };
    }
    if let Err(message) = verify_inputs(session, data, input_revision).await {
        return CommandOutcome::Rejected { message };
    }
    let mut stopped = entry.stopped.subscribe();
    if *stopped.borrow() || session.client.is_closed() {
        return CommandOutcome::Rejected {
            message: "The original service stopped before the command was sent".into(),
        };
    }
    // Mutations are NEVER sent through request_current's automatic read retry.
    let result = tokio::select! {
        _ = stopped.changed() => Err("Language service stopped during command execution; its effects need review".into()),
        result = session.client.request("workspace/executeCommand", json!({"command":data.command.name,"arguments":data.command.arguments}), REQUEST_TIMEOUT) => result.map_err(|e|e.to_string()),
    };
    match result {
        Ok(result) => CommandOutcome::Completed { result },
        Err(message) => CommandOutcome::Uncertain { message },
    }
}

#[cfg(test)]
mod tests;
