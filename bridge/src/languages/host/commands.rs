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
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod owned;
use outcomes::CommandScope;
pub(super) use outcomes::Outcomes;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Default, Clone)]
pub(super) struct Recoveries(Arc<Mutex<std::collections::BTreeSet<String>>>);

#[cfg(all(
    feature = "test-support",
    any(target_os = "linux", target_os = "macos")
))]
impl Host {
    /// Observe actual registered native jobs in isolated lifecycle contracts.
    /// This is not exposed by the host's wire protocol or distribution API.
    pub fn registered_workspace_workers(&self) -> usize {
        self.inner.coordinator.workers()
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) struct CallbackConnections(Arc<tokio::sync::Semaphore>);
#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Default for CallbackConnections {
    fn default() -> Self {
        Self(Arc::new(tokio::sync::Semaphore::new(2)))
    }
}

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
    preparation: Option<(
        CommandInvocation,
        openwebide_core::workspace_transaction::WorkspaceTransactionWorker,
    )>,
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
                }
                | CommandProgress::Cancelled => true,
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
        .saturating_add(protocol::actions::MAX_COMMAND_BYTES)
        // Include the future native worker/preparation and opaque invocation.
        .saturating_add(4096);
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
            preparation: None,
            progress: watch::channel(CommandProgress::Ready).0,
            synchronizing: None,
            input_revision,
            synchronized_inputs: None,
        },
    );
    Ok(review)
}

impl Host {
    /// Resolve controls from the original native session/receipt. No provider
    /// payload, replacement service or filesystem access is needed to fence it.
    fn command_control_scope(
        &self,
        owner: &str,
        project: i64,
        invocation: &CommandInvocation,
    ) -> Result<Option<Scope>, String> {
        let scope = {
            let entries = self.inner.entries.lock().unwrap();
            entries
                .iter()
                .find(|(key, entry)| {
                    key.owner == owner
                        && key.project == project
                        && key.client == invocation.client
                        && entry
                            .session
                            .get()
                            .is_some_and(|session| session.id == invocation.server)
                })
                .map(|(key, _)| {
                    Ok::<_, String>(Scope {
                        owner: key.owner.clone(),
                        project,
                        root: PathBuf::new(),
                        provider: key.provider.clone(),
                        digest: key.digest.clone(),
                        server: serde_json::from_str(&key.server)
                            .map_err(|error| error.to_string())?,
                    })
                })
                .transpose()?
        };
        let scope = match scope {
            Some(scope) => Some(scope),
            None => self
                .inner
                .outcomes
                .lock()
                .unwrap()
                .control_scope(owner, project, invocation)?,
        };
        Ok(scope)
    }
    pub async fn control_command(
        &self,
        owner: &str,
        project: i64,
        invocation: &CommandInvocation,
        operation: protocol::actions::CommandOperation,
    ) -> Result<CommandProgress, String> {
        invocation.validate()?;
        if owner.is_empty()
            || project <= 0
            || operation == protocol::actions::CommandOperation::Start
        {
            return Err("Original command controls cannot start execution".into());
        }
        if operation == protocol::actions::CommandOperation::Cancel {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            return self
                .cancel_original_control(owner, project, invocation)
                .await;
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            return Err("Original native cancellation recovery is unavailable on this host".into());
        }
        let scope = self.command_control_scope(owner, project, invocation)?;
        let Some(scope) = scope else {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            if let Some(progress) = self
                .recover_original_control(
                    owner,
                    project,
                    invocation,
                    operation == protocol::actions::CommandOperation::Stop,
                    None,
                )
                .await?
            {
                return Ok(progress);
            }
            return Ok(unavailable());
        };
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let original_scope = scope.clone();
        let progress = match operation {
            protocol::actions::CommandOperation::Status => {
                self.command_progress(scope, invocation).await
            }
            protocol::actions::CommandOperation::Stop => self.stop_command(scope, invocation).await,
            protocol::actions::CommandOperation::Start => {
                Err("Original command controls cannot start execution".into())
            }
            protocol::actions::CommandOperation::Cancel => unreachable!(),
        }?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if operation == protocol::actions::CommandOperation::Stop
            && let CommandProgress::Finished {
                outcome: outcome @ CommandOutcome::Rejected { .. },
            } = &progress
            && let Some(recovered) = self
                .recover_original_control(owner, project, invocation, true, Some(outcome.clone()))
                .await?
        {
            self.retain_original_control(original_scope, invocation, &recovered)
                .await?;
            return Ok(recovered);
        }
        Ok(progress)
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    async fn retain_original_control(
        &self,
        scope: Scope,
        invocation: &CommandInvocation,
        progress: &CommandProgress,
    ) -> Result<(), String> {
        if !matches!(
            progress,
            CommandProgress::Cancelled
                | CommandProgress::Finished {
                    outcome: CommandOutcome::Rejected { .. }
                }
        ) {
            return Ok(());
        }
        let (context, entry) = self.command_entry(scope, invocation, false).await?;
        if let Some(entry) = entry
            && let Some(session) = entry.session.get()
        {
            let mut commands = session.commands.lock().unwrap();
            if let Some(slot) = commands.slots.get_mut(&invocation.command) {
                let previous = slot.progress.borrow().clone();
                let bound = slot.invocation.as_ref()
                    == Some(&(invocation.id.clone(), invocation.approval.clone()))
                    || (slot.invocation.is_none()
                        && slot
                            .preparation
                            .as_ref()
                            .is_some_and(|prepared| prepared.0 == *invocation));
                if bound
                    && slot.data.origin == invocation.origin
                    && slot.data.review == invocation.review
                    && matches!(
                        previous,
                        CommandProgress::Ready
                            | CommandProgress::Finished {
                                outcome: CommandOutcome::Rejected { .. }
                            }
                    )
                {
                    if slot.invocation.is_none() {
                        let _ = self.inner.outcomes.lock().unwrap().record(
                            &context,
                            invocation,
                            &entry,
                            &slot.progress,
                        )?;
                        slot.invocation =
                            Some((invocation.id.clone(), invocation.approval.clone()));
                    }
                    slot.progress.send_replace(progress.clone());
                }
            }
        }
        self.inner
            .outcomes
            .lock()
            .unwrap()
            .retain_control(&context, invocation, progress)
    }
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
    pub(super) async fn native_command_worker(
        &self,
        scope: Scope,
        invocation: &CommandInvocation,
    ) -> Result<openwebide_core::workspace_transaction::WorkspaceTransactionWorker, String> {
        let (_, entry) = self.command_entry(scope, invocation, true).await?;
        let entry = entry.ok_or("The original language service is unavailable")?;
        let _guard = ServiceUse(entry.clone());
        let session = entry
            .session
            .get()
            .ok_or("The original language service was lost")?;
        let mut commands = session.commands.lock().unwrap();
        let slot = commands
            .slots
            .get_mut(&invocation.command)
            .ok_or("The original command review is unavailable")?;
        if slot.data.origin != invocation.origin
            || slot.data.review != invocation.review
            || slot.created.elapsed() >= COMMAND_LIFETIME
        {
            return Err("The original native command review changed or expired".into());
        }
        if let Some((prepared, worker)) = &slot.preparation {
            if prepared != invocation {
                return Err("The native command preparation belongs to another approval".into());
            }
            return Ok(worker.clone());
        }
        let worker = openwebide_core::workspace_transaction::WorkspaceTransactionWorker {
            id: format!("{:032x}", rand::random::<u128>()),
            secret: format!("{:032x}", rand::random::<u128>()),
        };
        slot.preparation = Some((invocation.clone(), worker.clone()));
        Ok(worker)
    }
    pub async fn prepare_command(
        &self,
        scope: Scope,
        invocation: &CommandInvocation,
    ) -> Result<
        openwebide_core::plugins::languages::intelligence::actions::CommandPreparation,
        String,
    > {
        use openwebide_core::plugins::languages::intelligence::actions::CommandPreparation;
        let context = CommandScope::new(&scope, invocation)?;
        if let Some(progress) = self
            .inner
            .outcomes
            .lock()
            .unwrap()
            .get(&context, invocation, false)?
        {
            return Ok(CommandPreparation::Existing { progress });
        }
        let worker = self.native_command_worker(scope, invocation).await?;
        Ok(CommandPreparation::Worker {
            worker: worker.delegation_key(),
        })
    }
    pub(crate) async fn start_owned_command(
        &self,
        scope: Scope,
        request: openwebide_core::plugins::languages::intelligence::actions::HostCommandRequest,
        backend: Option<Arc<crate::runs::backend_client::BackendClient>>,
    ) -> Result<CommandProgress, String> {
        let selected = request
            .command
            .selection
            .as_ref()
            .ok_or("Starting a command requires its complete approved provider")?;
        if request.command.operation != protocol::actions::CommandOperation::Start
            || selected.user != request.command.user
            || selected.project != request.command.project
            || scope.project != request.command.project
        {
            return Err("Language command start scope changed".into());
        }
        let invocation = request.command.invocation;
        let context = CommandScope::new(&scope, &invocation)?;
        if let Some(progress) =
            self.inner
                .outcomes
                .lock()
                .unwrap()
                .get(&context, &invocation, false)?
        {
            return Ok(progress);
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(progress) = self
            .recover_original_control(&scope.owner, scope.project, &invocation, false, None)
            .await?
        {
            return Ok(progress);
        }
        let worker = self
            .native_command_worker(scope.clone(), &invocation)
            .await?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let origin = request
                .journal
                .as_ref()
                .map(|j| j.api_url.as_str())
                .or_else(|| backend.as_ref().map(|b| b.recovery_origin()));
            if let Some(origin) = origin {
                self.inner
                    .coordinator
                    .recovery_namespace()?
                    .check_command_origin(
                        request.command.user,
                        scope.project,
                        if request.journal.is_some() {
                            openwebide_core::WorkspaceMode::Local
                        } else {
                            openwebide_core::WorkspaceMode::Remote
                        },
                        &invocation,
                        origin,
                    )?;
            }
            let authority = request
                .authority
                .ok_or("A fresh manual command grant is required")?;
            let user = request.command.user;
            let project = scope.project;
            if let Some(journal) = request.journal {
                if journal.delegation.user != user
                    || journal.delegation.project != project
                    || journal.delegation.scope.id != invocation.id
                {
                    return Err("The paired command journal changed identity".into());
                }
                let backend =
                    crate::workspace_transactions::command_delegation::PairedCommandJournal::new(
                        journal.api_url,
                        &journal.delegation.scope.root_identity.clone(),
                        journal.delegation,
                        worker.clone(),
                    )?;
                let journal = owned::Journal {
                    backend: Arc::new(backend),
                    user,
                    project,
                    invocation: invocation.clone(),
                    worker,
                    recoveries: self.inner.command_recoveries.clone(),
                };
                self.start_command_with(
                    scope,
                    invocation,
                    move |coordinator, entry, data, revision| {
                        journal.execute(coordinator, entry, data, revision, authority)
                    },
                )
                .await
            } else {
                let journal = owned::Journal {
                    backend: backend.ok_or("The original native backend is unavailable")?,
                    user,
                    project,
                    invocation: invocation.clone(),
                    worker,
                    recoveries: self.inner.command_recoveries.clone(),
                };
                self.start_command_with(
                    scope,
                    invocation,
                    move |coordinator, entry, data, revision| {
                        journal.execute(coordinator, entry, data, revision, authority)
                    },
                )
                .await
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (worker, backend, request.authority, request.journal, scope);
            Err("Native command recovery is unavailable on this platform".into())
        }
    }
    /// Standalone embedding primitive; its caller owns approval persistence.
    /// Ordinary folder admission cannot resume a pending original native owner.
    /// Application transports use the owned journal path. Both paths join a
    /// duplicate invocation rather than send its RPC again.
    pub async fn start_command(
        &self,
        scope: Scope,
        invocation: CommandInvocation,
    ) -> Result<CommandProgress, String> {
        self.start_command_with(
            scope,
            invocation,
            |coordinator, entry, data, revision| async move {
                coordinated_execute(&coordinator, &entry, &data, revision).await
            },
        )
        .await
    }
    async fn start_command_with<F, Fut>(
        &self,
        scope: Scope,
        invocation: CommandInvocation,
        run: F,
    ) -> Result<CommandProgress, String>
    where
        F: FnOnce(
                crate::folder_coordination::Coordinator,
                Arc<Entry>,
                Arc<CommandData>,
                u64,
            ) -> Fut
            + Send
            + 'static,
        Fut: std::future::Future<Output = CommandOutcome> + Send + 'static,
    {
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
            let outcome = run(coordinator, entry, data, input_revision).await;
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
        CommandProgress::Finished { .. } | CommandProgress::Cancelled => false,
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
        lease = coordinator.acquire(folder.clone()) => match lease {
            Ok(lease) => lease,
            Err(message) => return CommandOutcome::Rejected { message },
        },
    };
    leased_execute(
        coordinator,
        entry,
        data,
        input_revision,
        &folder,
        &lease,
        None,
    )
    .await
    .into_outcome()
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
use owned::Dispatch;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
struct Dispatch<'a>(&'a std::convert::Infallible);
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl Dispatch<'_> {
    fn outcome(&self, _: &CommandOutcome) -> Result<(), String> {
        match *self.0 {}
    }
    fn stopped(&self) -> Result<(), String> {
        match *self.0 {}
    }
    fn record(&self) -> Result<(), String> {
        match *self.0 {}
    }
}
struct ExecutionResult {
    outcome: CommandOutcome,
    cleaned: bool,
}
impl ExecutionResult {
    fn into_outcome(self) -> CommandOutcome {
        if !self.cleaned && !matches!(self.outcome, CommandOutcome::Uncertain { .. }) {
            CommandOutcome::Uncertain {
                message: "Native command cleanup is unconfirmed".into(),
            }
        } else {
            self.outcome
        }
    }
}
async fn leased_execute(
    coordinator: &crate::folder_coordination::Coordinator,
    entry: &Entry,
    data: &CommandData,
    input_revision: u64,
    folder: &crate::folder_coordination::Folder,
    lease: &crate::folder_coordination::Lease,
    dispatch: Option<&Dispatch<'_>>,
) -> ExecutionResult {
    if entry.session.get().is_none() {
        return ExecutionResult {
            outcome: CommandOutcome::Rejected {
                message: "The original service is unavailable".into(),
            },
            cleaned: true,
        };
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let watcher = {
        let session = entry
            .session
            .get()
            .expect("The original session is retained by its entry");
        let owner = { session.process.lock().unwrap().owner() };
        let retained = match owner {
            Some(owner) => match dispatch {
                Some(dispatch) => match dispatch.process_cleanup() {
                    Ok(cleanup) => lease.watch_command_process(&owner, cleanup).await,
                    Err(message) => Err(message),
                },
                None => lease.watch_process(&owner).await,
            },
            None => Err("The original process owner is unavailable".to_string()),
        };
        match retained {
            Ok(watcher) => watcher,
            Err(message) => {
                if let Err(cleanup) = entry.stop_and_wait().await {
                    lease.mark_uncertain();
                    return ExecutionResult {
                        outcome: CommandOutcome::Uncertain {
                            message: format!("{message}. Native cleanup is unconfirmed: {cleanup}"),
                        },
                        cleaned: false,
                    };
                }
                if let Some(dispatch) = dispatch
                    && let Err(cleanup) = dispatch.stopped()
                {
                    lease.mark_uncertain();
                    return ExecutionResult {
                        outcome: CommandOutcome::Uncertain {
                            message: format!(
                                "{message}. Original cleanup publication is pending: {cleanup}"
                            ),
                        },
                        cleaned: false,
                    };
                }
                return ExecutionResult {
                    outcome: CommandOutcome::Rejected { message },
                    cleaned: true,
                };
            }
        }
    };
    // Context, overlays and affected-file verification occur AFTER queued waits.
    // Panic must stop the original process before releasing its physical lease.
    use futures::FutureExt;
    let work =
        std::panic::AssertUnwindSafe(execute(entry, data, input_revision, dispatch)).catch_unwind();
    let mut outcome = tokio::select! {
        biased;
        () = coordinator.stopped() => CommandOutcome::Uncertain { message: "Execution host stopped during command preparation or execution; review effects before continuing".into() },
        result = work => result.unwrap_or_else(|_| CommandOutcome::Uncertain { message: "The native command worker failed; review effects before continuing".into() }),
    };
    if !lease.covers(folder) && matches!(outcome, CommandOutcome::Completed { .. }) {
        outcome = CommandOutcome::Uncertain {
            message: "The original command folder changed during execution; review its effects"
                .into(),
        };
    }
    if let Err(message) = outcome.validate() {
        outcome = CommandOutcome::Uncertain { message };
    }
    // The exact result precedes either watcher release or process cleanup. A
    // lost acknowledgement must not replace a completed result with uncertainty.
    let recorded = dispatch.map_or(Ok(()), |dispatch| dispatch.outcome(&outcome));
    if recorded.is_err()
        || matches!(outcome, CommandOutcome::Uncertain { .. })
        || *entry.stopped.borrow()
        || !lease.covers(folder)
    {
        let cleaned = match entry.stop_and_wait().await {
            Ok(()) => dispatch.is_none_or(|dispatch| dispatch.stopped().is_ok()),
            Err(message) => {
                lease.mark_uncertain();
                if dispatch.is_none() {
                    outcome = CommandOutcome::Uncertain {
                        message: format!(
                            "Native command cleanup could not be confirmed: {message}"
                        ),
                    };
                }
                false
            }
        };
        if !cleaned {
            lease.mark_uncertain();
        }
        return ExecutionResult { outcome, cleaned };
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Err(message) = watcher.finish().await {
        let cleaned = entry.stop_and_wait().await.is_ok()
            && dispatch.is_none_or(|dispatch| dispatch.stopped().is_ok());
        if !cleaned {
            lease.mark_uncertain();
        }
        return ExecutionResult {
            outcome: if dispatch.is_some() {
                outcome
            } else {
                CommandOutcome::Uncertain {
                    message: format!("Native folder release could not be confirmed: {message}"),
                }
            },
            cleaned,
        };
    }
    ExecutionResult {
        outcome,
        cleaned: true,
    }
}

async fn execute(
    entry: &Entry,
    data: &CommandData,
    input_revision: u64,
    dispatch: Option<&Dispatch<'_>>,
) -> CommandOutcome {
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
    send_command(entry, session, data, input_revision, dispatch).await
}

async fn verify_inputs(session: &Session, data: &CommandData, revision: u64) -> Result<(), String> {
    let current = session.inputs.observe(&session.root).await?;
    if current.snapshot != data.project_inputs || current.revision != revision {
        return Err("Declared project inputs no longer match the reviewed action".into());
    }
    Ok(())
}

async fn send_command(
    entry: &Entry,
    session: &Session,
    data: &CommandData,
    input_revision: u64,
    dispatch: Option<&Dispatch<'_>>,
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
    if let Some(dispatch) = dispatch
        && let Err(message) = dispatch.record()
    {
        // The native admission write may have committed before its local
        // acknowledgement failed. Never contradict a durable dispatch boundary.
        return CommandOutcome::Uncertain { message };
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
