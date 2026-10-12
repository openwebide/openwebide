//! Retain descendant color classifications while refreshing external parent roles.
use super::subtrees::{ParsedSubtrees, Part, charge_visits, descend_part};
use super::{Node, SyntaxProvider, SyntaxStatus, Tree};
use crate::{editor::SyntaxHighlightScope, highlight::TokenKind};
use std::{collections::HashMap, ops::Range};

type Spans = Vec<(Range<usize>, TokenKind)>;

#[derive(Default)]
pub(super) struct ParsedHighlights {
    subtrees: ParsedSubtrees<Spans>,
    #[cfg(test)]
    pub(super) reused_nodes: usize,
}

impl ParsedHighlights {
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    #[cfg(test)]
    pub(super) fn collect(
        &mut self,
        tree: &Tree,
        provider: SyntaxProvider,
        text: &str,
        visited: &mut usize,
        spans: &mut Spans,
    ) -> Result<(), SyntaxStatus> {
        let mut work = HighlightCollection::new(tree, provider, std::mem::take(self));
        while !work.advance(text, visited, spans, 256)? {}
        *self = work.finish();
        Ok(())
    }
}

/// Own immutable trees, not borrowed nodes/cursors, across driver yields.
/// Output remains private to the source-bound paint job until complete.
pub(super) struct HighlightCollection {
    tree: Tree,
    provider: SyntaxProvider,
    previous: ParsedHighlights,
    next: HashMap<usize, Part<Spans>>,
    node: usize,
    ancestors: Vec<(usize, usize)>,
    pending: Option<PendingPart>,
    #[cfg(test)]
    reused_nodes: usize,
}
struct PendingPart {
    node: usize,
    id: usize,
    template: Part<Spans>,
    end: usize,
    base: usize,
    byte_end: usize,
    output_start: usize,
    offset: usize,
    relative: Spans,
    retained: Option<Part<Spans>>,
}
impl HighlightCollection {
    pub(super) fn new(tree: &Tree, provider: SyntaxProvider, previous: ParsedHighlights) -> Self {
        Self {
            tree: tree.clone(),
            provider,
            previous,
            next: HashMap::new(),
            node: 0,
            ancestors: Vec::new(),
            pending: None,
            #[cfg(test)]
            reused_nodes: 0,
        }
    }
    /// Independently cap node classification, cached-span mapping, ancestry
    /// unwinding and subtree publication; none can monopolize a driver turn.
    pub(super) fn advance(
        &mut self,
        text: &str,
        visited: &mut usize,
        spans: &mut Spans,
        records: usize,
    ) -> Result<bool, SyntaxStatus> {
        if self.provider.highlight.is_none() {
            return Ok(true);
        }
        let root = self.tree.root_node();
        let document = self.provider.highlight_scope == SyntaxHighlightScope::Document;
        let mut cursor = self.tree.walk();
        let mut parent_cursor = self.tree.walk();
        let mut nodes = records;
        let mut spans_left = records;
        let mut ancestors_left = records;
        let mut parts_left = records;
        loop {
            // Numeric ancestry survives yields without storing borrowed Nodes.
            // Unwind one entry per step, including deeply nested trees.
            if self
                .ancestors
                .last()
                .is_some_and(|(_, end)| *end <= self.node)
            {
                if !consume(&mut ancestors_left) {
                    return Ok(false);
                }
                self.ancestors.pop();
                continue;
            }
            if let Some(part) = &mut self.pending {
                if let Some(retained) = &part.retained {
                    if let Some((range, kind)) = retained.value.get(part.offset) {
                        if !consume(&mut spans_left) {
                            return Ok(false);
                        }
                        let absolute = part
                            .base
                            .checked_add(range.start)
                            .zip(part.base.checked_add(range.end))
                            .filter(|(start, end)| {
                                *start < *end
                                    && *end <= part.byte_end
                                    && text.is_char_boundary(*start)
                                    && text.is_char_boundary(*end)
                            });
                        if let Some((start, end)) = absolute {
                            spans.push((start..end, *kind));
                            part.offset += 1;
                        } else {
                            // No retained visit credits were charged before validation.
                            spans.truncate(part.output_start);
                            part.retained = None;
                            self.node = part.node + 1;
                        }
                        continue;
                    }
                    if !consume(&mut parts_left) {
                        return Ok(false);
                    }
                    charge_visits(visited, retained.visits)?;
                    #[cfg(test)]
                    {
                        self.reused_nodes += retained.visits;
                    }
                    self.next.insert(part.id, retained.clone());
                    // Within a batch, skip the retained subtree through the
                    // existing cursor instead of reseeking it from the root.
                    if (part.node..part.end).contains(&cursor.descendant_index()) {
                        while cursor.descendant_index() > part.node && cursor.goto_parent() {}
                        next_sibling(&mut cursor);
                    }
                    self.node = part.end;
                    self.pending = None;
                    continue;
                }
                if self.node == part.end {
                    if !consume(&mut parts_left) {
                        return Ok(false);
                    }
                    let mut part = self.pending.take().unwrap();
                    if part.template.visits > 0 {
                        part.template.value = std::sync::Arc::new(part.relative);
                        self.next.insert(part.id, part.template);
                    }
                    continue;
                }
                if !consume(&mut nodes) {
                    return Ok(false);
                }
                if cursor.descendant_index() != self.node {
                    cursor.goto_descendant(self.node);
                }
                let node = cursor.node();
                charge_visits(visited, 1)?;
                let before = spans.len();
                classify(
                    node,
                    parent_node(
                        &mut parent_cursor,
                        self.ancestors.last().map(|(index, _)| *index),
                    ),
                    self.provider,
                    text,
                    spans,
                );
                if node.child_count() > 0 {
                    self.ancestors
                        .push((self.node, self.node + node.descendant_count()));
                }
                if let Some((range, kind)) = spans.get(before) {
                    part.relative
                        .push((range.start - part.base..range.end - part.base, *kind));
                }
                self.node += 1;
                next_node(&mut cursor);
                continue;
            }
            if self.node == root.descendant_count() {
                return Ok(true);
            }
            if !consume(&mut nodes) {
                return Ok(false);
            }
            if cursor.descendant_index() != self.node {
                cursor.goto_descendant(self.node);
            }
            let node = cursor.node();
            charge_visits(visited, 1)?;
            let parent_index = self.ancestors.last().map(|(index, _)| *index);
            let parent = parent_node(&mut parent_cursor, parent_index);
            classify(node, parent, self.provider, text, spans);
            let index = self.node;
            if node.child_count() > 0 {
                self.ancestors
                    .push((index, index + node.descendant_count()));
            }
            self.node += 1;
            next_node(&mut cursor);
            if document || index == 0 || descend_part(node) || node.descendant_count() == 1 {
                continue;
            }
            let retained = self
                .previous
                .subtrees
                .candidate(root, node, parent)
                .cloned();
            self.pending = Some(PendingPart {
                node: index,
                id: node.id(),
                template: retained.clone().unwrap_or_else(|| {
                    Part::new(node, parent, node.descendant_count() - 1, Vec::new())
                }),
                end: index + node.descendant_count(),
                base: node.start_byte(),
                byte_end: node.end_byte(),
                output_start: spans.len(),
                offset: 0,
                relative: Vec::new(),
                retained,
            });
        }
    }
    pub(super) fn finish(self) -> ParsedHighlights {
        let mut result = ParsedHighlights::default();
        if self.provider.highlight.is_some()
            && self.provider.highlight_scope != SyntaxHighlightScope::Document
        {
            result.subtrees.replace(&self.tree, self.next);
        }
        #[cfg(test)]
        {
            result.reused_nodes = self.reused_nodes;
        }
        result
    }
}

fn consume(remaining: &mut usize) -> bool {
    if *remaining == 0 {
        return false;
    }
    *remaining -= 1;
    true
}

fn parent_node<'tree>(
    cursor: &mut tree_sitter::TreeCursor<'tree>,
    index: Option<usize>,
) -> Option<Node<'tree>> {
    index.map(|index| {
        if cursor.descendant_index() != index {
            cursor.goto_descendant(index);
        }
        cursor.node()
    })
}

fn next_node(cursor: &mut tree_sitter::TreeCursor<'_>) {
    if cursor.goto_first_child() {
        return;
    }
    next_sibling(cursor);
}
fn next_sibling(cursor: &mut tree_sitter::TreeCursor<'_>) {
    while !cursor.goto_next_sibling() {
        if !cursor.goto_parent() {
            return;
        }
    }
}

fn classify(
    node: Node<'_>,
    parent: Option<Node<'_>>,
    provider: SyntaxProvider,
    text: &str,
    spans: &mut Spans,
) {
    if let Some(kind) = provider.highlight.and_then(|select| select(node, parent)) {
        let range = node.byte_range();
        if !range.is_empty()
            && text.is_char_boundary(range.start)
            && text.is_char_boundary(range.end)
        {
            spans.push((range, kind));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::Language;

    fn oracle(tree: &Tree, provider: SyntaxProvider, source: &str) -> (usize, Spans) {
        let mut visited = 0;
        let mut spans = Vec::new();
        super::super::visit_node_with_parent(
            tree.root_node(),
            None,
            &mut visited,
            |node, parent| {
                classify(node, parent, provider, source, &mut spans);
                Ok(())
            },
        )
        .unwrap();
        (visited, spans)
    }

    #[test]
    fn bounded_collection_matches_full_traversal_and_warm_unicode_rebasing() {
        for (language, source) in [
            (
                Language::Rust,
                "fn main() { let value = 42; println!(\"文😀\"); }\r\n".repeat(60),
            ),
            (
                Language::Python,
                "def greet():\r\n    value = 42\r\n    return '文😀'\r\n".repeat(60),
            ),
            (Language::Json, "{\"name\": \"文😀\", \"value\": 42}".into()),
        ] {
            let mut document = super::super::SyntaxDocument::new(language).unwrap();
            document.prepare(&source, 4, || true).1.unwrap();
            let changed = format!(" \r\n{source}");
            document.update(&changed, || true);
            let tree = document.tree.as_ref().unwrap();
            let provider = document.provider.unwrap();
            let expected = oracle(tree, provider, &changed);
            let old = std::mem::take(&mut *document.highlights.borrow_mut());
            let mut work = HighlightCollection::new(tree, provider, old);
            let mut visited = 0;
            let mut spans = Vec::new();
            let mut steps = 0;
            loop {
                let before = spans.len();
                let done = work.advance(&changed, &mut visited, &mut spans, 3).unwrap();
                assert!(spans.len().saturating_sub(before) <= 6);
                steps += 1;
                if done {
                    break;
                }
                assert!(steps < 100_000);
            }
            assert!(steps > 1);
            assert_eq!((visited, spans), expected);
            let result = work.finish();
            if language != Language::Json {
                assert!(result.reused_nodes > 0, "{language:?}");
            }
            // A changed frontier node may be rebuilt (the small JSON object).
            // Repeating the same immutable tree must reuse its descendants.
            let mut repeated = HighlightCollection::new(tree, provider, result);
            let mut spans = Vec::new();
            let mut visited = 0;
            while !repeated
                .advance(&changed, &mut visited, &mut spans, 3)
                .unwrap()
            {}
            assert_eq!((visited, spans), oracle(tree, provider, &changed));
            assert!(repeated.finish().reused_nodes > 0, "{language:?}");
        }
    }

    #[test]
    fn invalid_cached_ranges_fall_back_without_double_charging() {
        let source = "fn main() { let value = 42; }\r\n".repeat(20);
        let mut document = super::super::SyntaxDocument::new(Language::Rust).unwrap();
        document.prepare(&source, 4, || true).1.unwrap();
        let tree = document.tree.as_ref().unwrap();
        let provider = document.provider.unwrap();
        let mut parts = HashMap::new();
        super::super::subtrees::visit_parts(tree.root_node(), |node, parent, complete| {
            if complete {
                parts.insert(
                    node.id(),
                    Part::new(
                        node,
                        parent,
                        node.descendant_count() - 1,
                        vec![(0..usize::MAX, TokenKind::Plain)],
                    ),
                );
            }
            Ok(())
        })
        .unwrap();
        let mut old = ParsedHighlights::default();
        old.subtrees.replace(tree, parts);
        let mut work = HighlightCollection::new(tree, provider, old);
        let mut visited = 0;
        let mut spans = Vec::new();
        while !work.advance(&source, &mut visited, &mut spans, 1).unwrap() {}
        assert_eq!((visited, spans), oracle(tree, provider, &source));
        assert_eq!(work.finish().reused_nodes, 0);
    }
}
