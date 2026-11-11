//! Object-preserving resource instructions from the same interpreter used for
//! review. Removed and overwritten objects are retained until the durable
//! transaction decision; rollback transfers those original objects back.
use super::*;
use crate::vfs::VfsEntryKind;

pub const MAX_RESOURCE_STEPS: usize = 1024;
const MAX_PATH_BYTES: usize = 256 * 1024;

/// Metadata changes implied by the resource interpreter, rather than arbitrary
/// changes accepted because an object still has the same identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OriginalMutation {
    pub moved: bool,
    pub directory_changed: bool,
}

mod state;
pub(super) use state::{EMPTY_FILE, Origin, ResourceObject, ResourceState, Tree};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceScope {
    pub transaction: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "location",
    content = "path",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ResourceLocation {
    Workspace(String),
    /// An opaque slot belonging to this transaction's private recovery area.
    Retained(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceStep {
    /// Exclusive creation of an empty object, with existing parent directories.
    Create { path: String, kind: VfsEntryKind },
    /// Removal of a newly created, empty object during rollback.
    Remove { path: String, kind: VfsEntryKind },
    /// A single native object move which never replaces its destination.
    Transfer {
        from: ResourceLocation,
        to: ResourceLocation,
        kind: VfsEntryKind,
    },
}
impl ResourceStep {
    pub fn inverse(&self) -> Self {
        match self {
            Self::Create { path, kind } => Self::Remove {
                path: path.clone(),
                kind: *kind,
            },
            Self::Remove { path, kind } => Self::Create {
                path: path.clone(),
                kind: *kind,
            },
            Self::Transfer { from, to, kind } => Self::Transfer {
                from: to.clone(),
                to: from.clone(),
                kind: *kind,
            },
        }
    }

    fn path_bytes(&self) -> usize {
        fn bytes(location: &ResourceLocation) -> usize {
            match location {
                ResourceLocation::Workspace(path) => path.len(),
                ResourceLocation::Retained(_) => 0,
            }
        }
        match self {
            Self::Create { path, .. } | Self::Remove { path, .. } => path.len(),
            Self::Transfer { from, to, .. } => bytes(from).saturating_add(bytes(to)),
        }
    }
}

/// Bounded instructions contain paths and kinds, never per-step content copies.
/// Compiling a script does not select a filesystem or mutate any resources.
pub struct ResourceScript {
    pub steps: Vec<ResourceStep>,
}
impl ResourceScript {
    pub fn prepare(plan: &WorkspacePlan) -> Result<Self, String> {
        plan.validate()?;
        Self::compile(plan)
    }

    pub(super) fn compile(plan: &WorkspacePlan) -> Result<Self, String> {
        let mut steps = Vec::new();
        let mut path_bytes = 0_usize;
        let after = interpret_operations(&plan.before, &plan.roots, &plan.operations, |step| {
            path_bytes = path_bytes.saturating_add(step.path_bytes());
            if steps.len() >= MAX_RESOURCE_STEPS || path_bytes > MAX_PATH_BYTES {
                return Err(
                    "Workspace resource instructions exceed their step or path budget".into(),
                );
            }
            steps.push(step);
            Ok(())
        })?;
        if after != plan.after {
            return Err("Resource instructions do not match the reviewed workspace".into());
        }
        let mut state = ResourceState::new(plan);
        let slots = steps
            .iter()
            .filter_map(|step| match step {
                ResourceStep::Transfer {
                    to: ResourceLocation::Retained(slot),
                    ..
                } => Some(*slot),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        for (ordinal, step) in steps.iter().enumerate() {
            state.advance(step, ordinal)?;
            let absent = slots
                .iter()
                .filter(|slot| !state.retained.contains_key(&slot.to_string()))
                .count();
            if state.retained.len().saturating_add(absent) > MAX_ENTRIES {
                return Err("Retained resource inventory exceeds its entry budget".into());
            }
        }
        if !state.matches_workspace(&plan.after) {
            return Err("Native resource steps do not reach the reviewed workspace".into());
        }
        Ok(Self { steps })
    }

    pub(super) fn slots(&self) -> Vec<String> {
        self.steps
            .iter()
            .filter_map(|step| match step {
                ResourceStep::Transfer {
                    to: ResourceLocation::Retained(slot),
                    ..
                } => Some(slot.to_string()),
                _ => None,
            })
            .collect()
    }

    /// Roll back only the acknowledged or reconciled prefix. Transferring an
    /// original object back also preserves its native attributes and identity.
    pub fn rollback(&self, completed: usize) -> Result<Vec<ResourceStep>, String> {
        self.steps
            .get(..completed)
            .map(|steps| steps.iter().rev().map(ResourceStep::inverse).collect())
            .ok_or_else(|| "Invalid resource rollback cursor".into())
    }
}

pub(super) fn kind(state: &EntryState) -> Result<VfsEntryKind, String> {
    match state {
        EntryState::Directory => Ok(VfsEntryKind::Directory),
        EntryState::File { .. } => Ok(VfsEntryKind::File),
        EntryState::Missing => Err("Cannot transfer a missing workspace object".into()),
    }
}

pub(super) fn retain(
    path: &str,
    state: &EntryState,
    retained: &mut u32,
    emit: &mut impl FnMut(ResourceStep) -> Result<(), String>,
) -> Result<(), String> {
    emit(ResourceStep::Transfer {
        from: ResourceLocation::Workspace(path.into()),
        to: ResourceLocation::Retained(*retained),
        kind: kind(state)?,
    })?;
    *retained += 1;
    Ok(())
}

#[cfg(test)]
mod tests;
