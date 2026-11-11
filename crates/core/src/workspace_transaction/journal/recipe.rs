//! Versioned primitive recipes share the journal's intent, acknowledgement,
//! reconciliation, rollback and durable decision workflow.
use super::*;
use crate::workspace_transaction::resources::{Origin, ResourceObject, ResourceState, Tree};

pub(super) enum Recipe {
    Legacy(Vec<Step>),
    Resources {
        script: ResourceScript,
        selected: Vec<ResourceStep>,
    },
}
impl Recipe {
    pub fn new(plan: &WorkspacePlan, progress: &TransactionProgress) -> Result<Self, String> {
        if plan.version == 1 {
            return legacy_steps_for(plan, progress).map(Self::Legacy);
        }
        plan.validate()?;
        let script = ResourceScript::compile(plan)?;
        let selected = match progress.phase {
            TransactionPhase::Prepared
            | TransactionPhase::Applying
            | TransactionPhase::Committed => {
                if progress.rollback_from.is_some() {
                    return Err("Invalid forward resource cursor".into());
                }
                script.steps.clone()
            }
            TransactionPhase::Reverting | TransactionPhase::Restored => script.rollback(
                progress
                    .rollback_from
                    .ok_or("Missing resource rollback origin")?,
            )?,
            TransactionPhase::Finalizing => match progress.rollback_from {
                Some(completed) => script.rollback(completed)?,
                None => script.steps.clone(),
            },
        };
        if progress.completed > selected.len()
            || (progress.active && progress.completed == selected.len())
            || (progress.phase == TransactionPhase::Prepared
                && (progress.completed != 0 || progress.active))
            || (matches!(
                progress.phase,
                TransactionPhase::Finalizing
                    | TransactionPhase::Committed
                    | TransactionPhase::Restored
            ) && (progress.active || progress.completed != selected.len()))
        {
            return Err("Invalid resource recovery cursor".into());
        }
        Ok(Self::Resources { script, selected })
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Legacy(steps) => steps.len(),
            Self::Resources { selected, .. } => selected.len(),
        }
    }

    fn model<'a>(
        &self,
        plan: &'a WorkspacePlan,
        progress: &TransactionProgress,
        completed: usize,
    ) -> Result<ResourceState<'a>, String> {
        let Self::Resources { script, selected } = self else {
            return Err("Expected resource recipe".into());
        };
        let mut state = ResourceState::new(plan);
        if let Some(origin) = progress.rollback_from {
            for (ordinal, step) in script
                .steps
                .get(..origin)
                .ok_or("Invalid resource rollback origin")?
                .iter()
                .enumerate()
            {
                state.advance(step, ordinal)?;
            }
        }
        for (index, step) in selected
            .get(..completed)
            .ok_or("Invalid resource recovery cursor")?
            .iter()
            .enumerate()
        {
            let ordinal = progress
                .rollback_from
                .map_or(index, |origin| origin - 1 - index);
            state.advance(step, ordinal)?;
        }
        Ok(state)
    }

    pub fn snapshot(
        &self,
        plan: &WorkspacePlan,
        progress: &TransactionProgress,
        completed: usize,
    ) -> Result<Snapshot, String> {
        match self {
            Self::Legacy(_) => legacy_recovery_snapshot(plan, progress, completed),
            Self::Resources { .. } => Ok(self
                .model(plan, progress, completed)?
                .workspace
                .into_iter()
                .map(|(path, value)| (path, value.state.clone()))
                .collect()),
        }
    }
}

pub(super) enum Observation {
    Before,
    After,
    Changed,
}

pub(super) struct Execution<'a> {
    recipe: Recipe,
    model: Option<ResourceState<'a>>,
    scope: Option<ResourceScope>,
    completed: usize,
    rollback_from: Option<usize>,
}
impl<'a> Execution<'a> {
    pub fn new(
        id: &str,
        plan: &'a WorkspacePlan,
        progress: &TransactionProgress,
    ) -> Result<Self, String> {
        let recipe = Recipe::new(plan, progress)?;
        let (model, scope) = if matches!(recipe, Recipe::Resources { .. }) {
            (
                Some(recipe.model(plan, progress, progress.completed)?),
                Some(ResourceScope {
                    transaction: id.into(),
                    fingerprint: plan.fingerprint()?,
                }),
            )
        } else {
            (None, None)
        };
        Ok(Self {
            recipe,
            model,
            scope,
            completed: progress.completed,
            rollback_from: progress.rollback_from,
        })
    }
    pub fn remaining(&self) -> bool {
        self.completed < self.recipe.len()
    }

    pub async fn reconcile_creation(
        &self,
        files: &impl TransactionFiles,
        current: &impl Fn() -> bool,
    ) -> Result<(), String> {
        if let Recipe::Resources { selected, .. } = &self.recipe
            && let Some(ResourceStep::Create { path, kind }) = selected.get(self.completed)
        {
            if !current() {
                return Err("Workspace ownership changed before creation recovery".into());
            }
            files
                .reconcile_resource_creation(
                    self.scope.as_ref().unwrap(),
                    self.completed,
                    path,
                    *kind,
                )
                .await?;
            if !current() {
                return Err("Workspace ownership changed during creation recovery".into());
            }
        }
        Ok(())
    }

    pub fn advance(&mut self) -> Result<(), String> {
        if let Recipe::Resources { selected, .. } = &self.recipe {
            self.model.as_mut().unwrap().advance(
                &selected[self.completed],
                self.rollback_from
                    .map_or(self.completed, |origin| origin - 1 - self.completed),
            )?;
        }
        self.completed += 1;
        Ok(())
    }

    pub async fn observe(
        &self,
        files: &impl TransactionFiles,
        current: &impl Fn() -> bool,
    ) -> Result<Observation, String> {
        match &self.recipe {
            Recipe::Legacy(steps) => {
                let step = &steps[self.completed];
                let actual = read_state(files, &step.path).await?;
                Ok(if actual == step.after {
                    Observation::After
                } else if actual == step.before {
                    Observation::Before
                } else {
                    Observation::Changed
                })
            }
            Recipe::Resources { selected, .. } => {
                let step = &selected[self.completed];
                let model = self.model.as_ref().unwrap();
                let scope = self.scope.as_ref().unwrap();
                let (locations, before, after) = match step {
                    ResourceStep::Create { path, kind } => {
                        let location = ResourceLocation::Workspace(path.clone());
                        let value = ResourceObject {
                            state: if kind.is_dir() {
                                &EntryState::Directory
                            } else {
                                &resources::EMPTY_FILE
                            },
                            origin: Some(Origin::Created {
                                ordinal: self.completed,
                                mutation: OriginalMutation::default(),
                            }),
                        };
                        (
                            vec![location.clone()],
                            vec![model.subtree(&location)],
                            vec![BTreeMap::from([(String::new(), value)])],
                        )
                    }
                    ResourceStep::Remove { path, .. } => {
                        let location = ResourceLocation::Workspace(path.clone());
                        (
                            vec![location.clone()],
                            vec![model.subtree(&location)],
                            vec![BTreeMap::new()],
                        )
                    }
                    ResourceStep::Transfer { from, to, .. } => {
                        let mut source = model.subtree(from);
                        let before_source = source.clone();
                        if let Some(root) = source.get_mut("")
                            && let Some(origin) = &mut root.origin
                        {
                            origin.mutation().moved = true;
                        }
                        (
                            vec![from.clone(), to.clone()],
                            vec![before_source, model.subtree(to)],
                            vec![BTreeMap::new(), source],
                        )
                    }
                };
                let mut actual = Vec::new();
                for location in &locations {
                    actual.push(read_tree(files, scope, location, current).await?);
                }
                fn matches(actual: &[Snapshot], expected: &[Tree<'_>]) -> bool {
                    actual.iter().zip(expected).all(|(actual, expected)| {
                        actual.len() == expected.len()
                            && actual.iter().all(|(path, value)| {
                                expected
                                    .get(path)
                                    .is_some_and(|expected| expected.state == value)
                            })
                    })
                }
                Ok(if matches(&actual, &after) {
                    if let ResourceStep::Create { path, kind } = step
                        && !files
                            .created_resource_matches(scope, self.completed, path, *kind)
                            .await?
                    {
                        return Ok(Observation::Changed);
                    }
                    if let ResourceStep::Remove { path, kind } = step
                        && !files
                            .removed_resource_matches(scope, self.creation_ordinal()?, path, *kind)
                            .await?
                    {
                        return Ok(Observation::Changed);
                    }
                    if !self.parents_match(files, true).await?
                        || !originals_match(files, scope, &locations, &after).await?
                    {
                        return Ok(Observation::Changed);
                    }
                    Observation::After
                } else if matches(&actual, &before) {
                    if let ResourceStep::Remove { path, kind } = step
                        && !files
                            .created_resource_matches(scope, self.creation_ordinal()?, path, *kind)
                            .await?
                    {
                        return Ok(Observation::Changed);
                    }
                    if !self.parents_match(files, false).await?
                        || !originals_match(files, scope, &locations, &before).await?
                    {
                        return Ok(Observation::Changed);
                    }
                    Observation::Before
                } else {
                    Observation::Changed
                })
            }
        }
    }

    pub async fn validate_initial(
        &self,
        files: &impl TransactionFiles,
        current: &impl Fn() -> bool,
    ) -> Result<(), String> {
        let Some(model) = &self.model else {
            return Ok(());
        };
        let scope = self.scope.as_ref().unwrap();
        for (path, object) in &model.workspace {
            if !current() {
                return Err("Workspace ownership changed before resource admission".into());
            }
            if !matches!(object.state, EntryState::Missing)
                && !object_matches(
                    files,
                    scope,
                    &ResourceLocation::Workspace(path.clone()),
                    "",
                    *object,
                )
                .await?
            {
                return Err(
                    "Original object metadata changed; no resource mutation was started".into(),
                );
            }
        }
        let locations = model
            .workspace
            .keys()
            .cloned()
            .map(ResourceLocation::Workspace)
            .collect();
        if !self
            .parents_of(files, locations, BTreeSet::new(), true)
            .await?
            || !current()
        {
            return Err(
                "Original parent metadata changed; no resource mutation was started".into(),
            );
        }
        Ok(())
    }
    async fn parents_match(
        &self,
        files: &impl TransactionFiles,
        after: bool,
    ) -> Result<bool, String> {
        let Recipe::Resources { selected, .. } = &self.recipe else {
            return Ok(true);
        };
        let step = &selected[self.completed];
        let locations = match step {
            ResourceStep::Create { path, .. } | ResourceStep::Remove { path, .. } => {
                vec![ResourceLocation::Workspace(path.clone())]
            }
            ResourceStep::Transfer { from, to, .. } => vec![from.clone(), to.clone()],
        };
        let mutated = if after {
            locations
                .iter()
                .filter_map(|location| match location {
                    ResourceLocation::Workspace(path) => Some(parent(path).to_string()),
                    ResourceLocation::Retained(_) => None,
                })
                .collect::<BTreeSet<_>>()
        } else {
            BTreeSet::new()
        };
        self.parents_of(files, locations, mutated, false).await
    }
    async fn parents_of(
        &self,
        files: &impl TransactionFiles,
        locations: Vec<ResourceLocation>,
        mutated: BTreeSet<String>,
        uncreated: bool,
    ) -> Result<bool, String> {
        let scope = self.scope.as_ref().unwrap();
        let model = self.model.as_ref().unwrap();
        let mut seen = BTreeSet::new();
        for location in locations {
            if let ResourceLocation::Workspace(path) = location {
                let mut directory = parent(&path);
                loop {
                    if seen.insert(directory.to_string()) {
                        let mut object =
                            model
                                .workspace
                                .get(directory)
                                .copied()
                                .unwrap_or(ResourceObject {
                                    state: &EntryState::Directory,
                                    origin: Some(Origin::Original {
                                        path: directory,
                                        mutation: OriginalMutation::default(),
                                    }),
                                });
                        if uncreated && matches!(object.state, EntryState::Missing) {
                            if directory.is_empty() {
                                break;
                            }
                            directory = parent(directory);
                            continue;
                        }
                        if let Some(origin) = &mut object.origin {
                            origin.mutation().directory_changed |= directory.is_empty()
                                || model.changed_parents.contains(directory)
                                || mutated.contains(directory);
                        }
                        if !matches!(object.state, EntryState::Directory)
                            || !object_matches(
                                files,
                                scope,
                                &ResourceLocation::Workspace(directory.into()),
                                "",
                                object,
                            )
                            .await?
                        {
                            return Ok(false);
                        }
                    }
                    if directory.is_empty() {
                        break;
                    }
                    directory = parent(directory);
                }
            }
        }
        Ok(true)
    }
    pub async fn mutate(
        &self,
        files: &impl TransactionFiles,
        current: &impl Fn() -> bool,
    ) -> Result<(), String> {
        if matches!(self.recipe, Recipe::Resources { .. }) {
            let observation = self.observe(files, current).await?;
            if !current() {
                return Err("Workspace ownership changed before resource mutation".into());
            }
            match observation {
                Observation::After => return Ok(()),
                Observation::Changed => {
                    return Err(
                        "A resource object changed after its intent; it was preserved".into(),
                    );
                }
                Observation::Before => {}
            }
        }
        match &self.recipe {
            Recipe::Legacy(steps) => mutate(files, &steps[self.completed]).await,
            Recipe::Resources { selected, .. } => match &selected[self.completed] {
                ResourceStep::Create { path, kind } => {
                    files
                        .create_resource(self.scope.as_ref().unwrap(), self.completed, path, *kind)
                        .await
                }
                ResourceStep::Remove { path, kind } => {
                    files
                        .remove_created_resource(
                            self.scope.as_ref().unwrap(),
                            self.creation_ordinal()?,
                            path,
                            *kind,
                        )
                        .await
                }
                ResourceStep::Transfer { from, to, kind } => {
                    files
                        .transfer_resource(self.scope.as_ref().unwrap(), from, to, *kind)
                        .await
                }
            },
        }
    }
    fn creation_ordinal(&self) -> Result<usize, String> {
        let Recipe::Resources { selected, .. } = &self.recipe else {
            return Err("Legacy journal has no native creation".into());
        };
        let Some(ResourceStep::Remove { path, .. }) = selected.get(self.completed) else {
            return Err("Expected a created-object removal".into());
        };
        self.model
            .as_ref()
            .and_then(|model| model.workspace.get(path))
            .and_then(|object| match object.origin {
                Some(Origin::Created { ordinal, .. }) => Some(ordinal),
                _ => None,
            })
            .ok_or_else(|| "Invalid created-object rollback provenance".into())
    }
}

async fn object_matches(
    files: &impl TransactionFiles,
    scope: &ResourceScope,
    location: &ResourceLocation,
    suffix: &str,
    object: ResourceObject<'_>,
) -> Result<bool, String> {
    match object.origin {
        Some(Origin::Original {
            path: original,
            mutation,
        }) => {
            files
                .original_resource_matches(scope, location, suffix, original, mutation)
                .await
        }
        Some(Origin::Created { ordinal, mutation }) => {
            files
                .created_resource_matches_at(scope, ordinal, location, suffix, mutation)
                .await
        }
        None => Ok(true),
    }
}
async fn originals_match(
    files: &impl TransactionFiles,
    scope: &ResourceScope,
    locations: &[ResourceLocation],
    trees: &[Tree<'_>],
) -> Result<bool, String> {
    for (location, tree) in locations.iter().zip(trees) {
        for (suffix, object) in tree {
            if !object_matches(files, scope, location, suffix, *object).await? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

struct RetainedEntries<'a, F> {
    files: &'a F,
    scope: &'a ResourceScope,
}
impl<F: TransactionFiles> WorkspaceEntries for RetainedEntries<'_, F> {
    async fn canonicalize(&self, path: &str) -> Result<String, String> {
        self.files.retained_canonicalize(self.scope, path).await
    }
    async fn list(&self, path: &str) -> Result<Vec<crate::FileEntry>, String> {
        self.files.retained_list(self.scope, path).await
    }
    async fn read_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        self.files.retained_read_bytes(self.scope, path).await
    }
    async fn create(&self, _path: &str, _kind: crate::vfs::VfsEntryKind) -> Result<(), String> {
        Err("Retained objects can only be transferred by a journal".into())
    }
    async fn write_bytes(&self, _path: &str, _bytes: &[u8]) -> Result<(), String> {
        Err("Retained objects cannot be rewritten".into())
    }
    async fn delete(&self, _path: &str) -> Result<(), String> {
        Err("Retained objects require a durable cleanup decision".into())
    }
}

async fn read_tree(
    files: &impl TransactionFiles,
    scope: &ResourceScope,
    location: &ResourceLocation,
    current: &impl Fn() -> bool,
) -> Result<Snapshot, String> {
    let (root, snapshot) = match location {
        ResourceLocation::Workspace(path) => (
            path.clone(),
            capture(files, std::slice::from_ref(path), current).await?,
        ),
        ResourceLocation::Retained(slot) => {
            let root = slot.to_string();
            let entries = RetainedEntries { files, scope };
            let snapshot = capture(&entries, std::slice::from_ref(&root), current).await?;
            (root, snapshot)
        }
    };
    Ok(snapshot
        .into_iter()
        .filter(|(_, value)| !matches!(value, EntryState::Missing))
        .map(|(path, value)| (path[root.len()..].to_string(), value))
        .collect())
}

fn cleanup_namespaces(expected: &Tree<'_>, actual: &Snapshot) -> BTreeSet<String> {
    expected
        .keys()
        .filter(|path| {
            actual
                .get(*path)
                .is_none_or(|state| matches!(state, EntryState::Missing))
        })
        .map(|path| parent(path).to_string())
        .collect()
}
fn changed_directory(mut object: ResourceObject<'_>, changed: bool) -> ResourceObject<'_> {
    if let Some(Origin::Original { mutation, .. }) = &mut object.origin {
        mutation.directory_changed |= changed;
    }
    object
}
impl Execution<'_> {
    pub async fn check_retained(
        &self,
        files: &impl TransactionFiles,
        current: &impl Fn() -> bool,
        allow_missing: bool,
    ) -> Result<Snapshot, String> {
        let Recipe::Resources { script, .. } = &self.recipe else {
            return Ok(Snapshot::new());
        };
        let scope = self.scope.as_ref().unwrap();
        let entries = RetainedEntries { files, scope };
        let roots = script.slots();
        for entry in entries.list("").await? {
            if !roots.contains(&entry.path) {
                return Err("The private recovery area contains an unknown object".into());
            }
        }
        let snapshot = capture(&entries, &roots, current).await?;
        let expected = &self.model.as_ref().unwrap().retained;
        if snapshot.iter().any(|(path, value)| {
            if matches!(value, EntryState::Missing) {
                !allow_missing && expected.contains_key(path)
            } else {
                expected
                    .get(path)
                    .is_none_or(|expected| expected.state != value)
            }
        }) || (!allow_missing && expected.keys().any(|path| !snapshot.contains_key(path)))
        {
            return Err(
                "Retained originals changed; recovery preserved their current contents".into(),
            );
        }
        let changed = if allow_missing {
            cleanup_namespaces(expected, &snapshot)
        } else {
            BTreeSet::new()
        };
        for (path, actual) in &snapshot {
            if !matches!(actual, EntryState::Missing)
                && let Some(object) = expected.get(path)
            {
                let (slot, suffix) = path
                    .split_once('/')
                    .map_or((path.as_str(), String::new()), |(slot, suffix)| {
                        (slot, format!("/{suffix}"))
                    });
                let location = ResourceLocation::Retained(
                    slot.parse().map_err(|_| "Invalid original retained slot")?,
                );
                if !object_matches(
                    files,
                    scope,
                    &location,
                    &suffix,
                    changed_directory(*object, changed.contains(path)),
                )
                .await?
                {
                    return Err("A retained object changed; its current state was preserved".into());
                }
            }
        }
        Ok(snapshot)
    }

    pub async fn cleanup(
        &self,
        files: &impl TransactionFiles,
        current: &impl Fn() -> bool,
    ) -> Result<(), String> {
        let snapshot = self.check_retained(files, current, true).await?;
        let scope = self
            .scope
            .as_ref()
            .ok_or("Legacy journals have no retained objects")?;
        let entries = RetainedEntries { files, scope };
        let mut changed = cleanup_namespaces(&self.model.as_ref().unwrap().retained, &snapshot);
        let mut paths = snapshot
            .iter()
            .filter(|(_, value)| !matches!(value, EntryState::Missing))
            .collect::<Vec<_>>();
        paths.sort_by_key(|(path, _)| std::cmp::Reverse(depth(path)));
        for (path, expected) in paths {
            if !current() {
                return Err("Reconnect the original workspace before finishing cleanup".into());
            }
            let actual = capture(&entries, std::slice::from_ref(path), current).await?;
            let value = state(&actual, path);
            if matches!(value, EntryState::Missing) {
                continue;
            }
            if value != expected || (matches!(value, EntryState::Directory) && actual.len() != 1) {
                return Err(
                    "A retained object changed during cleanup; its contents were preserved".into(),
                );
            }
            if !current() {
                return Err("Workspace ownership changed before retained-object cleanup".into());
            }
            if let Some(object) = self
                .model
                .as_ref()
                .and_then(|model| model.retained.get(path))
            {
                let (slot, suffix) = path
                    .split_once('/')
                    .map_or((path.as_str(), String::new()), |(slot, suffix)| {
                        (slot, format!("/{suffix}"))
                    });
                if !object_matches(
                    files,
                    scope,
                    &ResourceLocation::Retained(
                        slot.parse().map_err(|_| "Invalid original retained slot")?,
                    ),
                    &suffix,
                    changed_directory(*object, changed.contains(path)),
                )
                .await?
                {
                    return Err(
                        "A retained object changed during cleanup; its current state was preserved"
                            .into(),
                    );
                }
            }
            files
                .remove_retained_entry(scope, path, resources::kind(expected)?)
                .await?;
            changed.insert(parent(path).to_string());
        }
        if !current() {
            return Err("Workspace ownership changed while finishing cleanup".into());
        }
        self.check_retained(files, current, true)
            .await?
            .values()
            .all(|value| matches!(value, EntryState::Missing))
            .then_some(())
            .ok_or("Retained-object cleanup is incomplete")?;
        files.finish_retained(scope).await
    }
}
