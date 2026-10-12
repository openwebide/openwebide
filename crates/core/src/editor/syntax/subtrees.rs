//! Retained tree ownership and identity checks shared by syntax consumers.
use super::{MAX_FOLD_NODES, Node, SyntaxStatus, Tree};
use std::{collections::HashMap, sync::Arc};

pub(super) struct Part<T> {
    kind: u16,
    bytes: usize,
    parent: Option<u16>,
    pub visits: usize,
    pub value: Arc<T>,
}

impl<T> Clone for Part<T> {
    fn clone(&self) -> Self {
        Self {
            kind: self.kind,
            bytes: self.bytes,
            parent: self.parent,
            visits: self.visits,
            value: self.value.clone(),
        }
    }
}

impl<T> Part<T> {
    pub(super) fn new(node: Node<'_>, parent: Option<Node<'_>>, visits: usize, value: T) -> Self {
        Self {
            kind: node.kind_id(),
            bytes: node.byte_range().len(),
            parent: parent.map(|parent| parent.kind_id()),
            visits,
            value: Arc::new(value),
        }
    }
}

pub(super) struct ParsedSubtrees<T> {
    // Keep old node allocations alive: a freed node ID could identify a new node.
    tree: Option<Tree>,
    parts: HashMap<usize, Part<T>>,
}

impl<T> Default for ParsedSubtrees<T> {
    fn default() -> Self {
        Self {
            tree: None,
            parts: HashMap::new(),
        }
    }
}

impl<T> ParsedSubtrees<T> {
    pub(super) fn candidate(
        &self,
        root: Node<'_>,
        node: Node<'_>,
        parent: Option<Node<'_>>,
    ) -> Option<&Part<T>> {
        if self.tree.as_ref()?.root_node().kind_id() != root.kind_id() {
            return None;
        }
        self.parts.get(&node.id()).filter(|part| {
            part.kind == node.kind_id()
                && part.bytes == node.byte_range().len()
                && part.parent == parent.map(|parent| parent.kind_id())
        })
    }

    pub(super) fn replace(&mut self, tree: &Tree, parts: HashMap<usize, Part<T>>) {
        self.tree = Some(tree.clone());
        self.parts = parts;
    }
}

/// Extract large containers individually and retain their smaller descendants.
/// The frontier is disjoint, so records and visit credits are never duplicated.
pub(super) fn visit_parts<'tree>(
    root: Node<'tree>,
    mut visit: impl FnMut(Node<'tree>, Option<Node<'tree>>, bool) -> Result<(), SyntaxStatus>,
) -> Result<(), SyntaxStatus> {
    let mut cursor = root.walk();
    if !cursor.goto_first_child() {
        return Ok(());
    }
    let mut parents = vec![root];
    loop {
        let node = cursor.node();
        let descend = descend_part(node);
        visit(node, parents.last().copied(), !descend)?;
        if descend && cursor.goto_first_child() {
            parents.push(node);
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(());
            }
            parents.pop();
            if cursor.node().id() == root.id() {
                return Ok(());
            }
        }
    }
}

pub(super) fn descend_part(node: Node<'_>) -> bool {
    const MAX_PART_BYTES: usize = 4096;
    // Transparent wrappers may be rebuilt around retained expressions.
    let wraps_subtree = node.named_child_count() == 1
        && node
            .named_child(0)
            .is_some_and(|child| child.child_count() > 0);
    node.child_count() > 0
        && (node.byte_range().len() > MAX_PART_BYTES
            || wraps_subtree
            || (node.named_child_count() > 0
                && node.end_position().row > node.start_position().row))
}

pub(super) fn charge_visits(visited: &mut usize, visits: usize) -> Result<(), SyntaxStatus> {
    *visited = visited.saturating_add(visits);
    if *visited > MAX_FOLD_NODES {
        Err(SyntaxStatus::TooLarge)
    } else {
        Ok(())
    }
}
