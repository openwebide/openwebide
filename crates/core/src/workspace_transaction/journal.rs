//! Durable intent precedes every mutation. Recovery reconciles ambiguous RPC
//! completion, then restores completed steps through the same filesystem adapter.
use super::*;
use std::future::Future;

mod recipe;
use recipe::{Execution, Observation, Recipe};

/// Filesystem primitives only. Replacement publishes an entire old/new file,
/// including when an error leaves acknowledgement uncertain. Removal must be
/// nonrecursive, and must reject an entry of a different kind.
pub trait TransactionFiles: WorkspaceEntries {
    /// Stable authenticated account/project/folder identity, derived by the
    /// adapter from its captured workspace authority rather than operation data.
    fn root_identity(&self) -> &str;
    fn replace_atomic(&self, path: &str, bytes: &[u8]) -> impl Future<Output = Result<(), String>>;
    fn remove_entry(
        &self,
        path: &str,
        kind: crate::vfs::VfsEntryKind,
    ) -> impl Future<Output = Result<(), String>>;

    fn supports_resource_moves(&self) -> bool {
        false
    }
    /// Match the reviewed original object at its current script location.
    /// Legacy records have no native witness and retain their earlier checks.
    fn original_resource_matches(
        &self,
        _scope: &ResourceScope,
        _location: &ResourceLocation,
        _suffix: &str,
        _original: &str,
        _mutation: OriginalMutation,
    ) -> impl Future<Output = Result<bool, String>> {
        async { Ok(true) }
    }
    fn created_resource_matches_at(
        &self,
        _scope: &ResourceScope,
        _ordinal: usize,
        _location: &ResourceLocation,
        _suffix: &str,
        _mutation: OriginalMutation,
    ) -> impl Future<Output = Result<bool, String>> {
        async { Ok(true) }
    }
    fn create_resource(
        &self,
        _scope: &ResourceScope,
        _ordinal: usize,
        path: &str,
        kind: crate::vfs::VfsEntryKind,
    ) -> impl Future<Output = Result<(), String>> {
        self.create(path, kind)
    }
    /// Reconcile only unpublished, journal-owned creation artifacts before the
    /// workspace snapshot is observed. Never publish or remove the destination.
    fn reconcile_resource_creation(
        &self,
        _scope: &ResourceScope,
        _ordinal: usize,
        _path: &str,
        _kind: crate::vfs::VfsEntryKind,
    ) -> impl Future<Output = Result<(), String>> {
        async { Ok(()) }
    }
    /// An ambiguous creation may be acknowledged only when its published object
    /// belongs to this exact intent, rather than an external same-content file.
    fn created_resource_matches(
        &self,
        _scope: &ResourceScope,
        _ordinal: usize,
        _path: &str,
        _kind: crate::vfs::VfsEntryKind,
    ) -> impl Future<Output = Result<bool, String>> {
        async { Ok(true) }
    }
    fn remove_created_resource(
        &self,
        _scope: &ResourceScope,
        _ordinal: usize,
        path: &str,
        kind: crate::vfs::VfsEntryKind,
    ) -> impl Future<Output = Result<(), String>> {
        self.remove_entry(path, kind)
    }
    fn removed_resource_matches(
        &self,
        _scope: &ResourceScope,
        _ordinal: usize,
        _path: &str,
        _kind: crate::vfs::VfsEntryKind,
    ) -> impl Future<Output = Result<bool, String>> {
        async { Ok(true) }
    }
    fn retained_canonicalize(
        &self,
        _scope: &ResourceScope,
        _path: &str,
    ) -> impl Future<Output = Result<String, String>> {
        async { Err("Retained workspace objects are unavailable".into()) }
    }
    fn retained_list(
        &self,
        _scope: &ResourceScope,
        _path: &str,
    ) -> impl Future<Output = Result<Vec<crate::FileEntry>, String>> {
        async { Err("Retained workspace objects are unavailable".into()) }
    }
    fn retained_read_bytes(
        &self,
        _scope: &ResourceScope,
        _path: &str,
    ) -> impl Future<Output = Result<Vec<u8>, String>> {
        async { Err("Retained workspace objects are unavailable".into()) }
    }
    fn transfer_resource(
        &self,
        _scope: &ResourceScope,
        _from: &ResourceLocation,
        _to: &ResourceLocation,
        _kind: crate::vfs::VfsEntryKind,
    ) -> impl Future<Output = Result<(), String>> {
        async { Err("Object-preserving workspace moves are unavailable".into()) }
    }
    fn remove_retained_entry(
        &self,
        _scope: &ResourceScope,
        _path: &str,
        _kind: crate::vfs::VfsEntryKind,
    ) -> impl Future<Output = Result<(), String>> {
        async { Err("Retained workspace cleanup is unavailable".into()) }
    }
    /// Idempotently remove this transaction's empty private recovery area.
    /// Never remove an unknown object, marker or another transaction's data.
    fn finish_retained(&self, _scope: &ResourceScope) -> impl Future<Output = Result<(), String>> {
        async { Err("Retained workspace cleanup is unavailable".into()) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionPhase {
    Prepared,
    Applying,
    Reverting,
    /// The durable decision is irreversible. Recovery finishes cleanup; it can
    /// no longer choose the opposite filesystem/editor outcome.
    Finalizing,
    Committed,
    Restored,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionProgress {
    pub phase: TransactionPhase,
    pub completed: usize,
    pub active: bool,
    pub rollback_from: Option<usize>,
}
impl TransactionProgress {
    /// Native resource calls require a durable execution phase. Individual
    /// intent and ordering remain the shared interpreter's responsibility.
    pub fn permits_resource_mutation(&self) -> bool {
        matches!(
            self.phase,
            TransactionPhase::Applying | TransactionPhase::Reverting | TransactionPhase::Finalizing
        )
    }
    pub fn commits_editor(&self) -> bool {
        self.phase == TransactionPhase::Committed
            || (self.phase == TransactionPhase::Finalizing && self.rollback_from.is_none())
    }
    /// A durable cancellation while no filesystem intent has ever started.
    pub fn prepared_cancellation() -> Self {
        Self {
            phase: TransactionPhase::Restored,
            completed: 0,
            active: false,
            rollback_from: Some(0),
        }
    }
}
impl Default for TransactionProgress {
    fn default() -> Self {
        Self {
            phase: TransactionPhase::Prepared,
            completed: 0,
            active: false,
            rollback_from: None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalRecord {
    pub id: String,
    /// Immutable record identity, preserved across every cursor and retry.
    pub generation: String,
    pub revision: u64,
    pub plan: WorkspacePlan,
    pub progress: TransactionProgress,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedJournal {
    pub id: String,
    pub generation: String,
    pub revision: u64,
    pub fingerprint: String,
}

/// Adapters bind a journal to its authenticated account/project/root and use
/// compare-and-swap revisions within the exact generation. One pending journal
/// excludes another transaction. The generation is an identity, not a write lease.
/// They must not acknowledge a prepare/advance before its durable commit.
pub trait WorkspaceJournal {
    fn prepare(
        &self,
        plan: &WorkspacePlan,
    ) -> impl Future<Output = Result<PreparedJournal, String>>;
    fn advance(
        &self,
        id: &str,
        generation: &str,
        revision: u64,
        progress: &TransactionProgress,
    ) -> impl Future<Output = Result<u64, String>>;
    fn load(&self, id: Option<&str>)
    -> impl Future<Output = Result<Option<JournalRecord>, String>>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryState {
    Unchanged,
    Restored,
    Pending,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionError {
    pub message: String,
    pub recovery: RecoveryState,
}
impl std::fmt::Display for TransactionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for TransactionError {}
impl TransactionError {
    fn unchanged(message: String) -> Self {
        Self {
            message,
            recovery: RecoveryState::Unchanged,
        }
    }
    fn pending(message: String) -> Self {
        Self {
            message,
            recovery: RecoveryState::Pending,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceMutation {
    pub path: String,
    pub before: EntryState,
    pub after: EntryState,
}
type Step = WorkspaceMutation;
impl WorkspaceMutation {
    /// Execute one durably recorded intent. Repeating a lost acknowledgement is
    /// harmless; unrelated external edits are never overwritten.
    pub async fn execute(&self, files: &impl TransactionFiles) -> Result<(), String> {
        entry_path(&self.path)?;
        let actual = read_state(files, &self.path).await?;
        if actual == self.after {
            return Ok(());
        }
        if actual != self.before {
            return Err(format!("Workspace entry changed: {}", self.path));
        }
        mutate(files, self).await?;
        if read_state(files, &self.path).await? != self.after {
            return Err(format!(
                "Workspace mutation was not confirmed: {}",
                self.path
            ));
        }
        Ok(())
    }
}
fn depth(path: &str) -> usize {
    path.split('/').count()
}
fn steps(before: &Snapshot, after: &Snapshot) -> Vec<Step> {
    let paths = before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let blocking_directories = paths
        .iter()
        .filter(|path| {
            matches!(state(before, path), EntryState::Directory)
                && matches!(state(after, path), EntryState::File { .. })
        })
        .cloned()
        .collect::<Vec<_>>();
    let blocks = |path: &str| {
        blocking_directories
            .iter()
            .any(|root| contains_path(root, path))
    };
    let mut removed = BTreeSet::new();
    let mut result = Vec::new();
    // Remove only entries which block a change of kind. Ordinary moves create
    // their destinations before removing their originals.
    let mut blocking = paths
        .iter()
        .filter(|path| {
            blocks(path)
                || (matches!(state(before, path), EntryState::File { .. })
                    && matches!(state(after, path), EntryState::Directory))
        })
        .cloned()
        .collect::<Vec<_>>();
    blocking.sort_by_key(|path| std::cmp::Reverse(depth(path)));
    for path in blocking {
        let old = state(before, &path);
        if !matches!(old, EntryState::Missing) {
            result.push(Step {
                path: path.clone(),
                before: old.clone(),
                after: EntryState::Missing,
            });
            removed.insert(path);
        }
    }
    let mut directories = paths
        .iter()
        .filter(|path| {
            matches!(state(after, path), EntryState::Directory)
                && state(before, path) != state(after, path)
        })
        .cloned()
        .collect::<Vec<_>>();
    directories.sort_by_key(|path| depth(path));
    for path in directories {
        result.push(Step {
            path,
            before: EntryState::Missing,
            after: EntryState::Directory,
        });
    }
    for path in &paths {
        if let EntryState::File { bytes } = state(after, path) {
            if state(before, path) == state(after, path) {
                continue;
            }
            let old = if removed.contains(path) {
                EntryState::Missing
            } else {
                state(before, path).clone()
            };
            let old = if matches!(old, EntryState::File { .. }) {
                old
            } else {
                result.push(Step {
                    path: path.clone(),
                    before: EntryState::Missing,
                    after: EntryState::file(Vec::new()),
                });
                EntryState::file(Vec::new())
            };
            if old != *state(after, path) {
                result.push(Step {
                    path: path.clone(),
                    before: old,
                    after: EntryState::file(bytes.clone()),
                });
            }
        }
    }
    for path in &paths {
        if !removed.contains(path)
            && matches!(state(before, path), EntryState::File { .. })
            && matches!(state(after, path), EntryState::Missing)
        {
            result.push(Step {
                path: path.clone(),
                before: state(before, path).clone(),
                after: EntryState::Missing,
            });
        }
    }
    let mut directories = paths
        .iter()
        .filter(|path| {
            !removed.contains(*path)
                && matches!(state(before, path), EntryState::Directory)
                && matches!(state(after, path), EntryState::Missing)
        })
        .cloned()
        .collect::<Vec<_>>();
    directories.sort_by_key(|path| std::cmp::Reverse(depth(path)));
    for path in directories {
        result.push(Step {
            path,
            before: EntryState::Directory,
            after: EntryState::Missing,
        });
    }
    result
}
fn intermediate(plan: &WorkspacePlan, completed: usize) -> Result<Snapshot, String> {
    let forward = steps(&plan.before, &plan.after);
    if completed > forward.len() {
        return Err("Invalid workspace recovery cursor".into());
    }
    let mut result = plan.before.clone();
    for step in &forward[..completed] {
        if matches!(step.after, EntryState::Missing) {
            result.remove(&step.path);
        } else {
            result.insert(step.path.clone(), step.after.clone());
        }
    }
    for root in &plan.roots {
        result.entry(root.clone()).or_insert(EntryState::Missing);
    }
    Ok(result)
}
fn legacy_steps_for(
    plan: &WorkspacePlan,
    progress: &TransactionProgress,
) -> Result<Vec<Step>, String> {
    plan.validate()?;
    let result = match progress.phase {
        TransactionPhase::Prepared | TransactionPhase::Applying | TransactionPhase::Committed => {
            if progress.rollback_from.is_some() {
                return Err("Invalid forward workspace recovery state".into());
            }
            steps(&plan.before, &plan.after)
        }
        TransactionPhase::Reverting | TransactionPhase::Restored => {
            let partial = intermediate(
                plan,
                progress
                    .rollback_from
                    .ok_or("Missing workspace rollback origin")?,
            )?;
            steps(&partial, &plan.before)
        }
        TransactionPhase::Finalizing => {
            return Err("Legacy workspace journals cannot finalize retained objects".into());
        }
    };
    if progress.completed > result.len()
        || (progress.active && progress.completed == result.len())
        || (progress.phase == TransactionPhase::Prepared
            && (progress.completed != 0 || progress.active))
        || (matches!(
            progress.phase,
            TransactionPhase::Committed | TransactionPhase::Restored
        ) && (progress.completed != result.len() || progress.active))
    {
        return Err("Invalid workspace recovery cursor".into());
    }
    Ok(result)
}
fn selected_steps(record: &JournalRecord) -> Result<Vec<Step>, String> {
    if record.plan.version != 1 {
        return Err("This journal requires object-preserving resource execution".into());
    }
    legacy_steps_for(&record.plan, &record.progress)
}

fn legacy_recovery_snapshot(
    plan: &WorkspacePlan,
    progress: &TransactionProgress,
    completed: usize,
) -> Result<Snapshot, String> {
    if progress.phase != TransactionPhase::Reverting {
        return intermediate(plan, completed);
    }
    let mut snapshot = intermediate(
        plan,
        progress
            .rollback_from
            .ok_or("Missing workspace rollback origin")?,
    )?;
    let selected = legacy_steps_for(plan, progress)?;
    if completed > selected.len() {
        return Err("Invalid workspace recovery cursor".into());
    }
    for step in &selected[..completed] {
        if matches!(step.after, EntryState::Missing) {
            snapshot.remove(&step.path);
        } else {
            snapshot.insert(step.path.clone(), step.after.clone());
        }
    }
    for root in &plan.roots {
        snapshot.entry(root.clone()).or_insert(EntryState::Missing);
    }
    Ok(snapshot)
}

/// Read-only preflight for an interrupted operation. A fresh folder grant or
/// rollback may proceed only when the complete affected tree matches a durable
/// cursor, including either outcome of an unacknowledged atomic primitive.
/// This validates contents; the adapter/user must separately grant the folder.
pub async fn preview_recovery(
    files: &impl WorkspaceEntries,
    record: &JournalRecord,
    current: impl Fn() -> bool,
) -> Result<Snapshot, String> {
    record.validate()?;
    if !matches!(
        record.progress.phase,
        TransactionPhase::Applying | TransactionPhase::Reverting
    ) {
        return Err("Only an interrupted filesystem operation needs a recovery folder".into());
    }
    let snapshot = capture(files, &record.plan.roots, &current).await?;
    if !current() {
        return Err("Workspace ownership changed during recovery review".into());
    }
    let recipe = Recipe::new(&record.plan, &record.progress)?;
    let before = recipe.snapshot(&record.plan, &record.progress, record.progress.completed)?;
    if snapshot == before
        || (record.progress.active
            && snapshot
                == recipe.snapshot(
                    &record.plan,
                    &record.progress,
                    record.progress.completed + 1,
                )?)
    {
        Ok(snapshot)
    } else {
        Err("The folder's affected files do not match this interrupted operation. Recovery preserved the current files.".into())
    }
}
impl JournalRecord {
    pub fn prepared_for(&self, plan: &WorkspacePlan) -> Result<PreparedJournal, String> {
        self.validate()?;
        if self.plan != *plan || self.progress != TransactionProgress::default() {
            return Err("Workspace recovery no longer matches the approved operation".into());
        }
        Ok(PreparedJournal {
            id: self.id.clone(),
            generation: self.generation.clone(),
            revision: self.revision,
            fingerprint: plan.fingerprint()?,
        })
    }
    pub fn active_mutation(&self) -> Result<WorkspaceMutation, String> {
        self.validate()?;
        if !self.progress.active
            || !matches!(
                self.progress.phase,
                TransactionPhase::Applying | TransactionPhase::Reverting
            )
        {
            return Err("Workspace transaction has no active mutation".into());
        }
        selected_steps(self)?
            .get(self.progress.completed)
            .cloned()
            .ok_or_else(|| "Invalid workspace mutation cursor".into())
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty()
            || self.id.len() > 128
            || self.id.chars().any(char::is_control)
            || self.revision == 0
            || !valid_id(&self.generation)
        {
            return Err("Invalid workspace journal identity or revision".into());
        }
        Recipe::new(&self.plan, &self.progress).map(|_| ())
    }
    pub fn validate_transition(&self, next: &TransactionProgress) -> Result<(), String> {
        self.validate()?;
        Recipe::new(&self.plan, next)?;
        let old = &self.progress;
        if old == next {
            return Ok(());
        }
        let valid = match (old.phase, next.phase) {
            (TransactionPhase::Prepared, TransactionPhase::Restored) => {
                next.completed == 0 && !next.active && next.rollback_from == Some(0)
            }
            (TransactionPhase::Prepared, TransactionPhase::Applying) => {
                next.completed == 0 && !next.active && next.rollback_from.is_none()
            }
            (
                TransactionPhase::Prepared | TransactionPhase::Applying,
                TransactionPhase::Reverting,
            ) => {
                !old.active
                    && next.completed == 0
                    && !next.active
                    && next.rollback_from == Some(old.completed)
            }
            (TransactionPhase::Applying, TransactionPhase::Applying)
            | (TransactionPhase::Reverting, TransactionPhase::Reverting) => {
                old.rollback_from == next.rollback_from
                    && if old.active {
                        !next.active
                            && (next.completed == old.completed
                                || next.completed == old.completed + 1)
                    } else {
                        next.active && next.completed == old.completed
                    }
            }
            (TransactionPhase::Applying, TransactionPhase::Committed)
            | (TransactionPhase::Reverting, TransactionPhase::Restored) => {
                self.plan.version == 1
                    && !old.active
                    && !next.active
                    && next.completed == old.completed
                    && next.rollback_from == old.rollback_from
            }
            (
                TransactionPhase::Applying | TransactionPhase::Reverting,
                TransactionPhase::Finalizing,
            ) => {
                self.plan.version == 2
                    && !old.active
                    && !next.active
                    && next.completed == old.completed
                    && next.rollback_from == old.rollback_from
            }
            (
                TransactionPhase::Finalizing,
                TransactionPhase::Committed | TransactionPhase::Restored,
            ) => {
                !old.active
                    && !next.active
                    && old.completed == next.completed
                    && old.rollback_from == next.rollback_from
                    && (next.phase == TransactionPhase::Committed) == old.rollback_from.is_none()
            }
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err("Invalid workspace journal transition".into())
        }
    }
}

async fn save(
    journal: &impl WorkspaceJournal,
    record: &mut JournalRecord,
    progress: TransactionProgress,
) -> Result<(), String> {
    record.revision = journal
        .advance(&record.id, &record.generation, record.revision, &progress)
        .await?;
    record.progress = progress;
    Ok(())
}
async fn read_state(files: &impl TransactionFiles, path: &str) -> Result<EntryState, String> {
    validate_path(files, path).await?;
    match files
        .list(parent(path))
        .await?
        .into_iter()
        .find(|entry| entry.path == path)
    {
        None => Ok(EntryState::Missing),
        Some(entry) if entry.is_dir => Ok(EntryState::Directory),
        Some(_) => files.read_bytes(path).await.map(EntryState::file),
    }
}
async fn mutate(files: &impl TransactionFiles, step: &Step) -> Result<(), String> {
    use crate::vfs::VfsEntryKind;
    match &step.after {
        EntryState::Missing => {
            files
                .remove_entry(
                    &step.path,
                    if matches!(step.before, EntryState::Directory) {
                        VfsEntryKind::Directory
                    } else {
                        VfsEntryKind::File
                    },
                )
                .await
        }
        EntryState::Directory => files.create(&step.path, VfsEntryKind::Directory).await,
        EntryState::File { bytes } if matches!(step.before, EntryState::Missing) => {
            if !bytes.is_empty() {
                return Err("Workspace file creation must precede its content write".into());
            }
            files.create(&step.path, VfsEntryKind::File).await
        }
        EntryState::File { bytes } => files.replace_atomic(&step.path, bytes).await,
    }
}
struct Cursor<'a> {
    id: &'a str,
    generation: &'a str,
    revision: &'a mut u64,
    progress: &'a mut TransactionProgress,
}
impl Cursor<'_> {
    async fn save(
        &mut self,
        journal: &impl WorkspaceJournal,
        next: TransactionProgress,
    ) -> Result<(), String> {
        *self.revision = journal
            .advance(self.id, self.generation, *self.revision, &next)
            .await?;
        *self.progress = next;
        Ok(())
    }
}
async fn reconcile(
    files: &impl TransactionFiles,
    journal: &impl WorkspaceJournal,
    cursor: &mut Cursor<'_>,
    execution: &mut Execution<'_>,
    current: &impl Fn() -> bool,
) -> Result<(), String> {
    if !cursor.progress.active {
        return Ok(());
    }
    let actual = execution.observe(files, current).await?;
    if !current() {
        return Err("Workspace ownership changed during recovery".into());
    }
    let mut progress = cursor.progress.clone();
    match actual {
        Observation::After => {execution.advance()?;progress.completed+=1;},
        Observation::Before=>{},
        Observation::Changed=>return Err("Workspace objects changed during the operation; recovery preserved their current contents".into()),
    }
    progress.active = false;
    cursor.save(journal, progress).await
}
async fn run(
    files: &impl TransactionFiles,
    journal: &impl WorkspaceJournal,
    record: &mut JournalRecord,
    current: &impl Fn() -> bool,
) -> Result<(), String> {
    let mut execution = Execution::new(&record.id, &record.plan, &record.progress)?;
    let mut cursor = Cursor {
        id: &record.id,
        generation: &record.generation,
        revision: &mut record.revision,
        progress: &mut record.progress,
    };
    if !current() {
        return Err("Return to the original account and folder to recover this operation".into());
    }
    reconcile(files, journal, &mut cursor, &mut execution, current).await?;
    execution.check_retained(files, current, false).await?;
    while execution.remaining() {
        if !current() {
            return Err(
                "Return to the original account and folder to recover this operation".into(),
            );
        }
        if !matches!(
            execution.observe(files, current).await?,
            Observation::Before
        ) {
            return Err(
                "Workspace objects changed; the operation stopped before overwriting them".into(),
            );
        }
        if !current() {
            return Err("Workspace ownership changed before the file operation".into());
        }
        let mut progress = cursor.progress.clone();
        progress.active = true;
        cursor.save(journal, progress).await?;
        if !current() {
            return Err("Workspace ownership changed before the file operation".into());
        }
        execution.mutate(files, current).await?;
        if !current() {
            return Err("Workspace ownership changed during the file operation".into());
        }
        if !matches!(execution.observe(files, current).await?, Observation::After) {
            return Err("Workspace objects did not reach their reviewed state".into());
        }
        if !current() {
            return Err("Workspace ownership changed while verifying the file operation".into());
        }
        execution.advance()?;
        let mut progress = cursor.progress.clone();
        progress.completed += 1;
        progress.active = false;
        cursor.save(journal, progress).await?;
    }
    execution.check_retained(files, current, false).await?;
    Ok(())
}

async fn reconcile_record(
    files: &impl TransactionFiles,
    journal: &impl WorkspaceJournal,
    record: &mut JournalRecord,
    current: &impl Fn() -> bool,
) -> Result<(), String> {
    let mut execution = Execution::new(&record.id, &record.plan, &record.progress)?;
    let mut cursor = Cursor {
        id: &record.id,
        generation: &record.generation,
        revision: &mut record.revision,
        progress: &mut record.progress,
    };
    reconcile(files, journal, &mut cursor, &mut execution, current).await?;
    execution.check_retained(files, current, false).await?;
    Ok(())
}

async fn finish_finalization(
    files: &impl TransactionFiles,
    journal: &impl WorkspaceJournal,
    record: &mut JournalRecord,
    current: &impl Fn() -> bool,
) -> Result<(), String> {
    if record.plan.version != 2 || record.progress.phase != TransactionPhase::Finalizing {
        return Err("Expected a durable resource cleanup decision".into());
    }
    if !current() || !files.supports_resource_moves() {
        return Err(
            "Reconnect the original object-preserving workspace adapter to finish cleanup".into(),
        );
    }
    Execution::new(&record.id, &record.plan, &record.progress)?
        .cleanup(files, current)
        .await?;
    if !current() {
        return Err("Workspace ownership changed before cleanup acknowledgement".into());
    }
    save(
        journal,
        record,
        TransactionProgress {
            phase: if record.progress.rollback_from.is_none() {
                TransactionPhase::Committed
            } else {
                TransactionPhase::Restored
            },
            ..record.progress.clone()
        },
    )
    .await
}

async fn decide(
    files: &impl TransactionFiles,
    journal: &impl WorkspaceJournal,
    record: &mut JournalRecord,
    current: &impl Fn() -> bool,
) -> Result<(), String> {
    let phase = if record.plan.version == 2 {
        TransactionPhase::Finalizing
    } else if record.progress.rollback_from.is_none() {
        TransactionPhase::Committed
    } else {
        TransactionPhase::Restored
    };
    save(
        journal,
        record,
        TransactionProgress {
            phase,
            ..record.progress.clone()
        },
    )
    .await?;
    if phase == TransactionPhase::Finalizing {
        finish_finalization(files, journal, record, current).await?;
    }
    Ok(())
}

/// Recover a nonterminal operation by rolling back exactly its acknowledged or
/// reconciled steps. Never overwrite unrelated later data to force recovery.
pub async fn recover(
    files: &impl TransactionFiles,
    journal: &impl WorkspaceJournal,
    mut record: JournalRecord,
    current: impl Fn() -> bool,
) -> Result<JournalRecord, TransactionError> {
    if files.root_identity() != record.plan.root {
        return Err(TransactionError::pending(
            "Reconnect the original workspace folder before recovery".into(),
        ));
    }
    record.validate().map_err(TransactionError::pending)?;
    if record.progress.phase == TransactionPhase::Restored {
        return Ok(record);
    }
    if record.progress.phase == TransactionPhase::Committed {
        return Err(TransactionError::pending("This operation committed; request an explicit reviewed undo instead of interrupted-operation recovery".into()));
    }
    if !current() {
        return Err(TransactionError::pending(
            "Grant access to the original account and folder before recovery".into(),
        ));
    }
    if record.progress.phase == TransactionPhase::Finalizing {
        finish_finalization(files, journal, &mut record, &current)
            .await
            .map_err(TransactionError::pending)?;
        return Ok(record);
    }
    if record.progress.phase == TransactionPhase::Prepared {
        // No active intent was ever issued. Abort without comparing or restoring
        // filesystem contents that another writer may have changed since review.
        save(
            journal,
            &mut record,
            TransactionProgress::prepared_cancellation(),
        )
        .await
        .map_err(TransactionError::pending)?;
        return Ok(record);
    }
    if record.progress.active {
        Execution::new(&record.id, &record.plan, &record.progress)
            .map_err(TransactionError::pending)?
            .reconcile_creation(files, &current)
            .await
            .map_err(TransactionError::pending)?;
    }
    preview_recovery(files, &record, &current)
        .await
        .map_err(TransactionError::pending)?;
    reconcile_record(files, journal, &mut record, &current)
        .await
        .map_err(TransactionError::pending)?;
    if record.progress.phase != TransactionPhase::Reverting {
        let progress = TransactionProgress {
            phase: TransactionPhase::Reverting,
            completed: 0,
            active: false,
            rollback_from: Some(record.progress.completed),
        };
        save(journal, &mut record, progress)
            .await
            .map_err(TransactionError::pending)?;
    }
    run(files, journal, &mut record, &current)
        .await
        .map_err(TransactionError::pending)?;
    if capture(files, &record.plan.roots, &current)
        .await
        .map_err(TransactionError::pending)?
        != record.plan.before
    {
        return Err(TransactionError::pending(
            "Other files changed during recovery; the recovery record is retained".into(),
        ));
    }
    if !current() {
        return Err(TransactionError::pending(
            "Workspace ownership changed after recovery".into(),
        ));
    }
    decide(files, journal, &mut record, &current)
        .await
        .map_err(TransactionError::pending)?;
    Ok(record)
}

/// Apply a concrete reviewed plan. Success means every filesystem entry reached
/// its expected state and the durable journal committed; partial work is never
/// reported as success. Unavailable rollback retains a recoverable journal.
pub async fn apply(
    files: &impl TransactionFiles,
    journal: &impl WorkspaceJournal,
    plan: WorkspacePlan,
    current: impl Fn() -> bool,
) -> Result<JournalRecord, TransactionError> {
    apply_bound(files, journal, plan, current, None).await
}

async fn apply_bound(
    files: &impl TransactionFiles,
    journal: &impl WorkspaceJournal,
    plan: WorkspacePlan,
    current: impl Fn() -> bool,
    identity: Option<(&str, &str)>,
) -> Result<JournalRecord, TransactionError> {
    plan.validate().map_err(TransactionError::unchanged)?;
    if plan.version == 2 && !files.supports_resource_moves() {
        return Err(TransactionError::unchanged(
            "This execution host cannot preserve workspace objects; no files were changed".into(),
        ));
    }
    if files.root_identity() != plan.root {
        return Err(TransactionError::unchanged(
            "The reviewed workspace folder is no longer selected".into(),
        ));
    }

    if capture(files, &plan.roots, &current)
        .await
        .map_err(TransactionError::unchanged)?
        != plan.before
    {
        return Err(TransactionError::unchanged(
            "Files changed since review; request the operation again".into(),
        ));
    }
    if !current() {
        return Err(TransactionError::unchanged(
            "Workspace ownership changed before the operation".into(),
        ));
    }
    let fingerprint = plan.fingerprint().map_err(TransactionError::unchanged)?;
    let prepared = journal
        .prepare(&plan)
        .await
        .map_err(TransactionError::unchanged)?;
    if prepared.fingerprint != fingerprint
        || prepared.id.is_empty()
        || prepared.revision == 0
        || !valid_id(&prepared.generation)
        || identity
            .is_some_and(|(id, generation)| prepared.id != id || prepared.generation != generation)
    {
        return Err(TransactionError::pending(
            "The prepared workspace journal does not match the reviewed plan".into(),
        ));
    }
    let mut record = JournalRecord {
        id: prepared.id,
        generation: prepared.generation,
        revision: prepared.revision,
        plan,
        progress: TransactionProgress::default(),
    };
    if !current() {
        return Err(TransactionError::pending(
            "Workspace ownership changed after preparing recovery".into(),
        ));
    }
    Execution::new(&record.id, &record.plan, &record.progress)
        .map_err(TransactionError::unchanged)?
        .validate_initial(files, &current)
        .await
        .map_err(TransactionError::unchanged)?;
    let result = async {
        save(
            journal,
            &mut record,
            TransactionProgress {
                phase: TransactionPhase::Applying,
                ..TransactionProgress::default()
            },
        )
        .await?;
        run(files, journal, &mut record, &current).await?;
        if capture(files, &record.plan.roots, &current).await? != record.plan.after {
            return Err("Files changed during the operation".into());
        }
        if !current() {
            return Err("Workspace ownership changed before committing the operation".into());
        }
        decide(files, journal, &mut record, &current).await
    }
    .await;
    if let Err(error) = result {
        if !current() {
            return Err(TransactionError::pending(error));
        }
        // Re-read the durable revision: a lost acknowledgement may have
        // committed even though the caller observed a transport error.
        let authoritative = journal
            .load(Some(&record.id))
            .await
            .map_err(|failure| {
                TransactionError::pending(format!(
                    "{error}. Recovery journal unavailable: {failure}"
                ))
            })?
            .filter(|saved| {
                saved.id == record.id
                    && saved.generation == record.generation
                    && saved.plan == record.plan
            })
            .ok_or_else(|| {
                TransactionError::pending(format!("{error}. The recovery journal changed"))
            })?;
        if authoritative.progress.phase == TransactionPhase::Committed {
            authoritative
                .validate()
                .map_err(TransactionError::pending)?;
            return Ok(authoritative);
        }
        if authoritative.progress.phase == TransactionPhase::Finalizing {
            return Err(TransactionError::pending(format!(
                "{error}. The operation has a durable decision; finish its retained-object cleanup instead of rolling it back"
            )));
        }
        return match recover(files, journal, authoritative, current).await {
            Ok(_) => Err(TransactionError {
                message: format!("{error}. The original files were restored"),
                recovery: RecoveryState::Restored,
            }),
            Err(failure) => Err(TransactionError::pending(format!(
                "{error}. Recovery is pending: {}",
                failure.message
            ))),
        };
    }
    Ok(record)
}

/// The same retry/recovery dispatch runs in the browser and native worker.
/// Callers hold their workspace execution lock before loading this record.
pub async fn execute(
    files: &impl TransactionFiles,
    journal: &impl WorkspaceJournal,
    record: JournalRecord,
    action: WorkspaceTransactionAction,
    current: impl Fn() -> bool,
) -> Result<JournalRecord, TransactionError> {
    if files.root_identity() != record.plan.root || !current() {
        return Err(TransactionError::pending(
            "Reconnect the original account and workspace before continuing".into(),
        ));
    }
    record.validate().map_err(TransactionError::pending)?;
    match (action, record.progress.phase) {
        (WorkspaceTransactionAction::Apply, TransactionPhase::Prepared) => {
            let id = record.id.clone();
            let generation = record.generation.clone();
            let result = apply_bound(
                files,
                journal,
                record.plan,
                &current,
                Some((&id, &generation)),
            )
            .await;
            if result
                .as_ref()
                .is_err_and(|error| error.recovery == RecoveryState::Unchanged)
                && current()
            {
                let latest = journal
                    .load(Some(&id))
                    .await
                    .map_err(TransactionError::pending)?;
                if latest
                    .as_ref()
                    .is_some_and(|record| record.id != id || record.generation != generation)
                {
                    return Err(TransactionError::pending(
                        "Workspace recovery identity changed; reload before continuing".into(),
                    ));
                }
                if let Some(record) = latest
                    && record.progress.phase == TransactionPhase::Prepared
                {
                    recover(files, journal, record, &current).await?;
                }
            }
            result
        }
        (WorkspaceTransactionAction::Apply, TransactionPhase::Committed) => Ok(record),
        (WorkspaceTransactionAction::Apply, TransactionPhase::Restored) => {
            Err(TransactionError::unchanged(
                "This operation was restored; request a new review before applying it again".into(),
            ))
        }
        (WorkspaceTransactionAction::Apply, _) => Err(TransactionError::pending(
            "Recover the interrupted operation before applying another review".into(),
        )),
        (WorkspaceTransactionAction::Recover, _) => recover(files, journal, record, current).await,
    }
}
