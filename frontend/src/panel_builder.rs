//! Visual authoring transactions over the production panel schema.
//! Paths address blocks in a single document revision; callers reject stale gestures.
use crate::panel_playground::Playground;
use openwebide_core::plugins::panels::*;
use serde_json::Value;
use std::collections::{BTreeSet, VecDeque};

pub type BlockPath = Vec<usize>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Text,
    Heading,
    Status,
    Group,
    Actions,
    Form,
    List,
    Table,
    Code,
    Diff,
    Link,
    Divider,
}
impl BlockKind {
    pub const ALL: [Self; 12] = [
        Self::Text,
        Self::Heading,
        Self::Status,
        Self::Group,
        Self::Actions,
        Self::Form,
        Self::List,
        Self::Table,
        Self::Code,
        Self::Diff,
        Self::Link,
        Self::Divider,
    ];
    pub const fn label(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Heading => "Heading",
            Self::Status => "Status",
            Self::Group => "Group",
            Self::Actions => "Actions",
            Self::Form => "Form",
            Self::List => "List",
            Self::Table => "Table",
            Self::Code => "Code",
            Self::Diff => "Diff",
            Self::Link => "Link",
            Self::Divider => "Divider",
        }
    }
    pub const fn of(block: &Block) -> Self {
        match block {
            Block::Text { .. } => Self::Text,
            Block::Heading { .. } => Self::Heading,
            Block::Status { .. } => Self::Status,
            Block::Group { .. } => Self::Group,
            Block::Actions { .. } => Self::Actions,
            Block::Form { .. } => Self::Form,
            Block::List { .. } => Self::List,
            Block::Table { .. } => Self::Table,
            Block::Code { .. } => Self::Code,
            Block::Diff { .. } => Self::Diff,
            Block::Link { .. } => Self::Link,
            Block::Divider => Self::Divider,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    Insert {
        kind: BlockKind,
        parent: BlockPath,
        index: usize,
    },
    Move {
        path: BlockPath,
        parent: BlockPath,
        index: usize,
    },
    Duplicate {
        path: BlockPath,
    },
    Delete {
        path: BlockPath,
    },
    Replace {
        path: BlockPath,
        block: Block,
    },
    Definition(PanelDefinition),
}
#[derive(Clone, Debug)]
pub struct EditedPanel {
    pub panel: Playground,
    pub selected: Option<BlockPath>,
}

pub fn children<'a>(blocks: &'a [Block], parent: &[usize]) -> Result<&'a [Block], String> {
    let Some((&index, rest)) = parent.split_first() else {
        return Ok(blocks);
    };
    match blocks.get(index) {
        Some(Block::Group { blocks, .. }) => children(blocks, rest),
        _ => Err("The destination group no longer exists".into()),
    }
}
fn children_mut<'a>(
    blocks: &'a mut Vec<Block>,
    parent: &[usize],
) -> Result<&'a mut Vec<Block>, String> {
    let Some((&index, rest)) = parent.split_first() else {
        return Ok(blocks);
    };
    match blocks.get_mut(index) {
        Some(Block::Group { blocks, .. }) => children_mut(blocks, rest),
        _ => Err("The destination group no longer exists".into()),
    }
}
pub fn block<'a>(blocks: &'a [Block], path: &[usize]) -> Result<&'a Block, String> {
    let (&index, parent) = path.split_last().ok_or("Select a block first")?;
    children(blocks, parent)?
        .get(index)
        .ok_or_else(|| "The block no longer exists".into())
}

/// All structural edits are atomic and pass the same validator as JSON and exports.
pub fn edit(original: &Playground, operation: Edit) -> Result<EditedPanel, String> {
    original.validate()?;
    let mut panel = original.clone();
    let selected = match operation {
        Edit::Insert {
            kind,
            parent,
            index,
        } => {
            let value = template(&mut panel, kind);
            insert(&mut panel.document.blocks, &parent, index, value)?;
            Some(at(&parent, index))
        }
        Edit::Move {
            path,
            mut parent,
            mut index,
        } => {
            let (&source_index, source_parent) = path.split_last().ok_or("Select a block first")?;
            if parent.starts_with(&path) {
                return Err("A group cannot be moved into itself or its children".into());
            }
            if index > children(&panel.document.blocks, &parent)?.len() {
                return Err("The drop position no longer exists".into());
            }
            let siblings = children_mut(&mut panel.document.blocks, source_parent)?;
            if source_index >= siblings.len() {
                return Err("The block no longer exists".into());
            }
            let value = siblings.remove(source_index);
            if parent == source_parent && index > source_index {
                index -= 1;
            }
            // Removing an earlier sibling can shift an ancestor of the target group.
            if parent.starts_with(source_parent)
                && parent.len() > source_parent.len()
                && parent[source_parent.len()] > source_index
            {
                parent[source_parent.len()] -= 1;
            }
            insert(&mut panel.document.blocks, &parent, index, value)?;
            Some(at(&parent, index))
        }
        Edit::Duplicate { path } => {
            let mut value = block(&panel.document.blocks, &path)?.clone();
            let mut ids = identifiers(&panel.document.blocks);
            freshen(&mut value, &mut ids);
            let (&index, parent) = path.split_last().ok_or("Select a block first")?;
            insert(&mut panel.document.blocks, parent, index + 1, value)?;
            Some(at(parent, index + 1))
        }
        Edit::Delete { path } => {
            let (&index, parent) = path.split_last().ok_or("Select a block first")?;
            let siblings = children_mut(&mut panel.document.blocks, parent)?;
            if index >= siblings.len() {
                return Err("The block no longer exists".into());
            }
            siblings.remove(index);
            None
        }
        Edit::Replace { path, block } => {
            let (&index, parent) = path.split_last().ok_or("Select a block first")?;
            *children_mut(&mut panel.document.blocks, parent)?
                .get_mut(index)
                .ok_or("The block no longer exists")? = block;
            Some(path)
        }
        Edit::Definition(definition) => {
            // Preserve button bindings when existing action slots are renamed.
            // Removing an action that is still referenced remains a validation error.
            if panel.panel.actions.len() == definition.actions.len() {
                let previous = panel
                    .panel
                    .actions
                    .iter()
                    .map(|action| action.id.as_str())
                    .collect::<BTreeSet<_>>();
                let current = definition
                    .actions
                    .iter()
                    .map(|action| action.id.as_str())
                    .collect::<BTreeSet<_>>();
                let renamed = panel
                    .panel
                    .actions
                    .iter()
                    .zip(&definition.actions)
                    .filter(|(old, new)| {
                        !current.contains(old.id.as_str()) && !previous.contains(new.id.as_str())
                    })
                    .map(|(old, new)| (old.id.clone(), new.id.clone()))
                    .collect::<std::collections::BTreeMap<_, _>>();
                fn update(
                    blocks: &mut [Block],
                    renamed: &std::collections::BTreeMap<String, String>,
                ) {
                    let button = |button: &mut PanelButton| {
                        if let Some(id) = renamed.get(&button.action) {
                            button.action = id.clone();
                        }
                    };
                    for block in blocks {
                        match block {
                            Block::Group { blocks, .. } => update(blocks, renamed),
                            Block::Actions { buttons } => {
                                for item in buttons {
                                    button(item);
                                }
                            }
                            Block::Form { submit, .. } => button(submit),
                            _ => {}
                        }
                    }
                }
                update(&mut panel.document.blocks, &renamed);
            }
            panel.panel = definition;
            None
        }
    };
    panel.validate()?;
    Ok(EditedPanel { panel, selected })
}
fn at(parent: &[usize], index: usize) -> BlockPath {
    let mut path = parent.to_vec();
    path.push(index);
    path
}
fn insert(
    blocks: &mut Vec<Block>,
    parent: &[usize],
    index: usize,
    value: Block,
) -> Result<(), String> {
    let siblings = children_mut(blocks, parent)?;
    if index > siblings.len() {
        return Err("The drop position no longer exists".into());
    }
    siblings.insert(index, value);
    Ok(())
}
fn identifiers(blocks: &[Block]) -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    fn visit(blocks: &[Block], result: &mut BTreeSet<String>) {
        for block in blocks {
            match block {
                Block::Group { blocks, .. } => visit(blocks, result),
                Block::Form { id, fields, submit } => {
                    result.insert(id.clone());
                    result.extend(fields.iter().map(|field| field.id.clone()));
                    result.extend(submit.values.keys().cloned());
                }
                Block::Actions { buttons } => result.extend(
                    buttons
                        .iter()
                        .flat_map(|button| button.values.keys().cloned()),
                ),
                _ => {}
            }
        }
    }
    visit(blocks, &mut result);
    result
}
fn fresh(ids: &mut BTreeSet<String>, prefix: &str) -> String {
    for index in 1usize.. {
        let id = format!("{prefix}_{index}");
        if ids.insert(id.clone()) {
            return id;
        }
    }
    unreachable!("a bounded document cannot exhaust usize identifiers")
}
fn freshen(block: &mut Block, ids: &mut BTreeSet<String>) {
    match block {
        Block::Group { blocks, .. } => {
            for child in blocks {
                freshen(child, ids);
            }
        }
        Block::Form { id, fields, .. } => {
            *id = fresh(ids, "form");
            for field in fields {
                field.id = fresh(ids, "field");
            }
        }
        _ => {}
    }
}
fn template(panel: &mut Playground, kind: BlockKind) -> Block {
    let mut ids = identifiers(&panel.document.blocks);
    let button = |panel: &mut Playground| {
        if panel.panel.actions.is_empty() {
            panel.panel.actions.push(PanelAction {
                id: "submit".into(),
                handler: "submit".into(),
                title: "Submit".into(),
                requires_approval: true,
            });
        }
        PanelButton {
            action: panel.panel.actions[0].id.clone(),
            label: "Submit".into(),
            values: Default::default(),
            disabled: false,
        }
    };
    match kind {
        BlockKind::Text => Block::Text {
            text: "Text".into(),
        },
        BlockKind::Heading => Block::Heading {
            text: "Heading".into(),
        },
        BlockKind::Status => Block::Status {
            text: "Status".into(),
            tone: Tone::Neutral,
        },
        BlockKind::Group => Block::Group {
            title: Some("Group".into()),
            blocks: vec![],
        },
        BlockKind::Actions => Block::Actions {
            buttons: vec![button(panel)],
        },
        BlockKind::Form => Block::Form {
            id: fresh(&mut ids, "form"),
            fields: vec![PanelField {
                id: fresh(&mut ids, "field"),
                label: "Field".into(),
                kind: FieldKind::Text {
                    multiline: false,
                    max_length: 256,
                },
                value: Value::String(String::new()),
                required: false,
            }],
            submit: button(panel),
        },
        BlockKind::List => Block::List {
            items: vec!["Item".into()],
        },
        BlockKind::Table => Block::Table {
            columns: vec!["Column".into()],
            rows: vec![vec!["Cell".into()]],
        },
        BlockKind::Code => Block::Code {
            text: "// Code".into(),
            language: Some("rust".into()),
        },
        BlockKind::Diff => Block::Diff {
            before: "Before\n".into(),
            after: "After\n".into(),
            path: Some("file.txt".into()),
        },
        BlockKind::Link => Block::Link {
            text: "Link".into(),
            url: "https://openwebide.com/".into(),
        },
        BlockKind::Divider => Block::Divider,
    }
}

/// Bounded transient undo across visual edits, JSON edits and simulated responses.
#[derive(Clone, Debug)]
pub struct History {
    present: String,
    past: VecDeque<String>,
    future: VecDeque<String>,
}
impl History {
    pub fn new(source: String) -> Self {
        Self {
            present: source,
            past: VecDeque::new(),
            future: VecDeque::new(),
        }
    }
    pub fn record(&mut self, source: String) {
        if source == self.present {
            return;
        }
        self.past
            .push_back(std::mem::replace(&mut self.present, source));
        self.future.clear();
        self.trim();
    }
    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }
    pub fn undo(&mut self) -> Option<String> {
        let source = self.past.pop_back()?;
        self.future
            .push_back(std::mem::replace(&mut self.present, source));
        self.trim();
        Some(self.present.clone())
    }
    pub fn redo(&mut self) -> Option<String> {
        let source = self.future.pop_back()?;
        self.past
            .push_back(std::mem::replace(&mut self.present, source));
        self.trim();
        Some(self.present.clone())
    }
    fn trim(&mut self) {
        while self.past.len() + self.future.len() > 32
            || self
                .past
                .iter()
                .chain(&self.future)
                .map(String::len)
                .sum::<usize>()
                > 4 * 1024 * 1024
        {
            if !self.past.is_empty() {
                self.past.pop_front();
            } else {
                self.future.pop_front();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn empty() -> Playground {
        Playground::blank()
    }
    #[test]
    fn every_palette_block_exports_through_the_production_schema() {
        let mut panel = empty();
        for kind in BlockKind::ALL {
            let end = panel.document.blocks.len();
            panel = edit(
                &panel,
                Edit::Insert {
                    kind,
                    parent: vec![],
                    index: end,
                },
            )
            .unwrap()
            .panel;
            assert_eq!(Playground::parse(&panel.json()).unwrap(), panel);
            panel.rust().unwrap();
        }
        assert_eq!(panel.document.blocks.len(), 12);
        assert!(panel.panel.actions[0].requires_approval);
    }
    #[test]
    fn moving_across_ancestors_and_siblings_preserves_the_intended_destination() {
        let mut panel = empty();
        panel.document.blocks = vec![
            Block::Text { text: "A".into() },
            Block::Group {
                title: None,
                blocks: vec![
                    Block::Text { text: "B".into() },
                    Block::Text { text: "C".into() },
                ],
            },
        ];
        let result = edit(
            &panel,
            Edit::Move {
                path: vec![0],
                parent: vec![1],
                index: 1,
            },
        )
        .unwrap();
        assert_eq!(result.selected, Some(vec![0, 1]));
        assert!(
            matches!(block(&result.panel.document.blocks, &[0,1]).unwrap(), Block::Text { text } if text=="A")
        );
        let result = edit(
            &result.panel,
            Edit::Move {
                path: vec![0, 0],
                parent: vec![0],
                index: 3,
            },
        )
        .unwrap();
        assert!(
            matches!(block(&result.panel.document.blocks, &[0,2]).unwrap(), Block::Text { text } if text=="B")
        );
        let result = edit(
            &result.panel,
            Edit::Move {
                path: vec![0, 1],
                parent: vec![],
                index: 0,
            },
        )
        .unwrap();
        assert!(
            matches!(block(&result.panel.document.blocks, &[0]).unwrap(), Block::Text { text } if text=="C")
        );
        assert!(
            edit(
                &panel,
                Edit::Move {
                    path: vec![1],
                    parent: vec![1],
                    index: 0
                }
            )
            .is_err()
        );
        assert!(
            edit(
                &panel,
                Edit::Move {
                    path: vec![0],
                    parent: vec![1],
                    index: 99
                }
            )
            .is_err()
        );
        assert_eq!(panel.document.blocks.len(), 2);
    }
    #[test]
    fn duplicating_nested_forms_assigns_unique_fields_and_invalid_properties_are_atomic() {
        let mut panel = empty();
        panel = edit(
            &panel,
            Edit::Insert {
                kind: BlockKind::Group,
                parent: vec![],
                index: 0,
            },
        )
        .unwrap()
        .panel;
        panel = edit(
            &panel,
            Edit::Insert {
                kind: BlockKind::Form,
                parent: vec![0],
                index: 0,
            },
        )
        .unwrap()
        .panel;
        let duplicated = edit(&panel, Edit::Duplicate { path: vec![0] })
            .unwrap()
            .panel;
        duplicated.validate().unwrap();
        let Block::Form { fields: first, .. } =
            block(&duplicated.document.blocks, &[0, 0]).unwrap()
        else {
            panic!("form")
        };
        let Block::Form { fields: second, .. } =
            block(&duplicated.document.blocks, &[1, 0]).unwrap()
        else {
            panic!("form")
        };
        assert_ne!(first[0].id, second[0].id);
        assert!(
            edit(
                &panel,
                Edit::Replace {
                    path: vec![0],
                    block: Block::Link {
                        text: "Unsafe".into(),
                        url: "javascript:alert(1)".into()
                    }
                }
            )
            .is_err()
        );
        panel.validate().unwrap();
    }
    #[test]
    fn history_is_bounded_and_new_edits_invalidate_redo() {
        let mut history = History::new("initial".into());
        history.record("second".into());
        history.record("third".into());
        assert_eq!(history.undo().as_deref(), Some("second"));
        assert_eq!(history.redo().as_deref(), Some("third"));
        history.undo();
        history.record("different".into());
        assert!(!history.can_redo());
        for index in 0..100 {
            history.record(index.to_string());
        }
        assert_eq!(history.past.len(), 32);
    }

    #[test]
    fn production_depth_and_block_limits_reject_edits_without_losing_the_original() {
        let mut panel = empty();
        for depth in 0..6 {
            panel = edit(
                &panel,
                Edit::Insert {
                    kind: BlockKind::Group,
                    parent: vec![0; depth],
                    index: 0,
                },
            )
            .unwrap()
            .panel;
        }
        assert!(
            edit(
                &panel,
                Edit::Insert {
                    kind: BlockKind::Group,
                    parent: vec![0; 6],
                    index: 0
                }
            )
            .is_err()
        );
        panel.validate().unwrap();
        panel.document.blocks = vec![Block::Divider; MAX_BLOCKS];
        assert!(edit(&panel, Edit::Duplicate { path: vec![0] }).is_err());
        assert_eq!(panel.document.blocks.len(), MAX_BLOCKS);
    }

    #[test]
    fn action_renames_preserve_bindings_and_referenced_actions_cannot_be_removed() {
        let mut panel = empty();
        panel = edit(
            &panel,
            Edit::Insert {
                kind: BlockKind::Actions,
                parent: vec![],
                index: 0,
            },
        )
        .unwrap()
        .panel;
        panel = edit(
            &panel,
            Edit::Insert {
                kind: BlockKind::Group,
                parent: vec![],
                index: 1,
            },
        )
        .unwrap()
        .panel;
        panel = edit(
            &panel,
            Edit::Insert {
                kind: BlockKind::Form,
                parent: vec![1],
                index: 0,
            },
        )
        .unwrap()
        .panel;
        let mut definition = panel.panel.clone();
        definition.actions[0].id = "publish".into();
        let renamed = edit(&panel, Edit::Definition(definition)).unwrap().panel;
        let Block::Actions { buttons } = &renamed.document.blocks[0] else {
            panic!("actions")
        };
        assert_eq!(buttons[0].action, "publish");
        let Block::Form { submit, .. } = block(&renamed.document.blocks, &[1, 0]).unwrap() else {
            panic!("form")
        };
        assert_eq!(submit.action, "publish");
        let mut definition = renamed.panel.clone();
        definition.actions.clear();
        assert!(edit(&renamed, Edit::Definition(definition)).is_err());
        renamed.validate().unwrap();
    }

    #[test]
    fn large_valid_nested_documents_round_trip_through_the_shared_authoring_source() {
        let mut panel = empty();
        let mut blocks = vec![
            Block::Table {
                columns: vec!["Column".into(); 8],
                rows: vec![vec!["x".repeat(290); 8]; 100],
            },
            Block::Text {
                text: "x".repeat(20000),
            },
        ];
        for _ in 0..6 {
            blocks = vec![Block::Group {
                title: None,
                blocks,
            }];
        }
        panel.document.blocks = blocks;
        panel.validate().unwrap();
        assert_eq!(Playground::parse(&panel.json()).unwrap(), panel);
        let index = panel.document.blocks.len();
        panel = edit(
            &panel,
            Edit::Insert {
                kind: BlockKind::Actions,
                parent: vec![],
                index,
            },
        )
        .unwrap()
        .panel;
        let input = panel.input("submit".into(), Default::default()).unwrap();
        let replacement = panel.simulate(&input, &panel.response(), true).unwrap();
        assert_eq!(replacement.document.revision, 2);
    }
}
