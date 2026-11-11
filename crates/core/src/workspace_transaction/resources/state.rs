//! Expected trees borrow reviewed bytes and retain each object's provenance
//! while workspace and private locations change during execution/recovery.
use super::*;

#[derive(Clone, Copy)]
pub(in crate::workspace_transaction) enum Origin<'a> {
    Original {
        path: &'a str,
        mutation: OriginalMutation,
    },
    Created {
        ordinal: usize,
        mutation: OriginalMutation,
    },
}
impl Origin<'_> {
    pub fn mutation(&mut self) -> &mut OriginalMutation {
        match self {
            Self::Original { mutation, .. } | Self::Created { mutation, .. } => mutation,
        }
    }
}
#[derive(Clone, Copy)]
pub(in crate::workspace_transaction) struct ResourceObject<'a> {
    pub state: &'a EntryState,
    pub origin: Option<Origin<'a>>,
}
pub(in crate::workspace_transaction) type Tree<'a> = BTreeMap<String, ResourceObject<'a>>;
pub(in crate::workspace_transaction) static EMPTY_FILE: EntryState =
    EntryState::File { bytes: Vec::new() };

pub(in crate::workspace_transaction) struct ResourceState<'a> {
    pub workspace: Tree<'a>,
    pub retained: Tree<'a>,
    roots: &'a [String],
    pub changed_parents: BTreeSet<String>,
}
impl<'a> ResourceState<'a> {
    pub fn new(plan: &'a WorkspacePlan) -> Self {
        Self {
            workspace: plan
                .before
                .iter()
                .map(|(path, state)| {
                    (
                        path.clone(),
                        ResourceObject {
                            state,
                            origin: (!matches!(state, EntryState::Missing)).then_some(
                                Origin::Original {
                                    path: path.as_str(),
                                    mutation: OriginalMutation::default(),
                                },
                            ),
                        },
                    )
                })
                .collect(),
            retained: Tree::new(),
            roots: &plan.roots,
            changed_parents: BTreeSet::new(),
        }
    }
    fn tree(&self, location: &ResourceLocation) -> (&Tree<'a>, String) {
        match location {
            ResourceLocation::Workspace(path) => (&self.workspace, path.clone()),
            ResourceLocation::Retained(slot) => (&self.retained, slot.to_string()),
        }
    }
    fn tree_mut(&mut self, location: &ResourceLocation) -> &mut Tree<'a> {
        match location {
            ResourceLocation::Workspace(_) => &mut self.workspace,
            ResourceLocation::Retained(_) => &mut self.retained,
        }
    }
    pub fn subtree(&self, location: &ResourceLocation) -> Tree<'a> {
        let (tree, root) = self.tree(location);
        tree.iter()
            .filter(|(path, value)| {
                contains_path(&root, path) && !matches!(value.state, EntryState::Missing)
            })
            .map(|(path, value)| (path[root.len()..].to_string(), *value))
            .collect()
    }
    fn change_parent(&mut self, location: &ResourceLocation) {
        if let ResourceLocation::Workspace(path) = location {
            let directory = parent(path);
            if let Some(object) = self.workspace.get_mut(directory) {
                if let Some(origin) = &mut object.origin {
                    origin.mutation().directory_changed = true;
                }
            } else {
                self.changed_parents.insert(directory.into());
            }
        }
    }
    pub fn advance(&mut self, step: &ResourceStep, ordinal: usize) -> Result<(), String> {
        match step {
            ResourceStep::Create { path, .. } | ResourceStep::Remove { path, .. } => {
                self.change_parent(&ResourceLocation::Workspace(path.clone()));
            }
            ResourceStep::Transfer { from, to, .. } => {
                self.change_parent(from);
                self.change_parent(to);
            }
        }
        match step {
            ResourceStep::Create { path, kind } => {
                if self
                    .workspace
                    .get(path)
                    .is_some_and(|value| !matches!(value.state, EntryState::Missing))
                {
                    return Err("Resource creation would replace an existing object".into());
                }
                self.workspace.insert(
                    path.clone(),
                    ResourceObject {
                        state: if kind.is_dir() {
                            &EntryState::Directory
                        } else {
                            &EMPTY_FILE
                        },
                        origin: Some(Origin::Created {
                            ordinal,
                            mutation: OriginalMutation::default(),
                        }),
                    },
                );
            }
            ResourceStep::Remove { path, kind } => {
                let expected = if kind.is_dir() {
                    &EntryState::Directory
                } else {
                    &EMPTY_FILE
                };
                if self.workspace.get(path).is_none_or(|value| {
                    value.state != expected || !matches!(value.origin, Some(Origin::Created { .. }))
                }) || self
                    .workspace
                    .keys()
                    .any(|child| child != path && contains_path(path, child))
                {
                    return Err(
                        "Only a newly created empty resource can be removed on rollback".into(),
                    );
                }
                self.workspace.remove(path);
            }
            ResourceStep::Transfer {
                from,
                to,
                kind: expected,
            } => {
                let mut moved = self.subtree(from);
                if let Some(root) = moved.get_mut("")
                    && let Some(origin) = &mut root.origin
                {
                    origin.mutation().moved = true;
                }
                if kind(
                    moved
                        .get("")
                        .map_or(&EntryState::Missing, |object| object.state),
                )? != *expected
                    || !self.subtree(to).is_empty()
                {
                    return Err("Invalid resource transfer source or occupied destination".into());
                }
                let (_, source) = self.tree(from);
                let (_, destination) = self.tree(to);
                self.tree_mut(from)
                    .retain(|path, _| !contains_path(&source, path));
                self.tree_mut(to).extend(
                    moved
                        .into_iter()
                        .map(|(suffix, value)| (format!("{destination}{suffix}"), value)),
                );
            }
        }
        for root in self.roots {
            self.workspace
                .entry(root.clone())
                .or_insert(ResourceObject {
                    state: &EntryState::Missing,
                    origin: None,
                });
        }
        Ok(())
    }
    pub fn matches_workspace(&self, snapshot: &Snapshot) -> bool {
        snapshot.len() == self.workspace.len()
            && snapshot.iter().all(|(path, value)| {
                self.workspace
                    .get(path)
                    .is_some_and(|expected| expected.state == value)
            })
    }
}
