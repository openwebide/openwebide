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
    retiring: Option<std::collections::hash_map::IntoIter<usize, Part<T>>>,
}

impl<T> Default for ParsedSubtrees<T> {
    fn default() -> Self {
        Self {
            tree: None,
            parts: HashMap::new(),
            retiring: None,
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
        self.retiring = None;
    }

    /// Move a validated owner instead of keeping a second cache reference alive.
    pub(super) fn take_candidate(
        &mut self,
        root: Node<'_>,
        node: Node<'_>,
        parent: Option<Node<'_>>,
    ) -> Option<Part<T>> {
        self.candidate(root, node, parent)?;
        self.parts.remove(&node.id())
    }

    /// Release leftover entries through the driver's record budget. Keep the
    /// old tree pinned until this cache owner is dropped; IDs remain protected.
    /// Iterator seeks, table deallocation and tree destruction are separate costs.
    pub(super) fn retire(&mut self, records: usize) -> bool {
        let entries = self
            .retiring
            .get_or_insert_with(|| std::mem::take(&mut self.parts).into_iter());
        for _ in 0..records.min(entries.len()) {
            drop(entries.next());
        }
        entries.len() == 0
    }

    #[cfg(test)]
    pub(super) fn retirement_pending(&self) -> bool {
        self.retiring
            .as_ref()
            .is_some_and(|entries| entries.len() > 0)
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

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use crate::highlight::Language;
    use std::{cell::Cell, rc::Rc, sync::Arc};

    struct TrackedValue(Rc<Cell<usize>>);
    impl Drop for TrackedValue {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn transferred_entries_keep_one_owner_and_leftovers_retire_within_budget() {
        let source = "fn item() { call(1); }\n".repeat(20);
        let mut document = super::super::SyntaxDocument::new(Language::Rust).unwrap();
        document.update(&source, || true);
        let tree = document.tree.as_ref().unwrap();
        let root = tree.root_node();
        let mut cursor = root.walk();
        let nodes: Vec<_> = root.children(&mut cursor).collect();
        assert_eq!(nodes.len(), 20);
        let drops = Rc::new(Cell::new(0));
        let mut old = ParsedSubtrees::default();
        old.replace(
            tree,
            nodes
                .iter()
                .map(|node| {
                    (
                        node.id(),
                        Part::new(
                            *node,
                            Some(root),
                            node.descendant_count() - 1,
                            TrackedValue(drops.clone()),
                        ),
                    )
                })
                .collect(),
        );
        assert!(old.take_candidate(root, nodes[0], None).is_none());
        assert_eq!(old.parts.len(), 20);
        let moved = old.take_candidate(root, nodes[0], Some(root)).unwrap();
        assert_eq!(Arc::strong_count(&moved.value), 1);
        assert_eq!(old.parts.len(), 19);
        assert!(old.take_candidate(root, nodes[0], Some(root)).is_none());
        let mut next = ParsedSubtrees::default();
        next.replace(tree, HashMap::from([(nodes[0].id(), moved)]));
        assert!(!old.retire(0));
        assert_eq!(drops.get(), 0);
        for retired in 1..=19 {
            assert_eq!(old.retire(1), retired == 19);
            assert_eq!(drops.get(), retired);
            assert!(old.tree.is_some(), "keep IDs pinned throughout retirement");
        }
        assert!(next.candidate(root, nodes[0], Some(root)).is_some());
        drop(old);
        assert_eq!(drops.get(), 19);
        drop(next);
        assert_eq!(drops.get(), 20);
    }
}
