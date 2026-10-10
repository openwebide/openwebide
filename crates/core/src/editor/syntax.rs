//! Incremental syntax analysis shared by browser and native editor adapters.
mod cache;
pub use cache::{MAX_SYNTAX_DOCUMENTS, MAX_SYNTAX_SOURCE_BYTES, SyntaxPreparations};
mod contexts;
mod folds;
mod highlighting;
mod highlights;
mod preparation;
mod service;
mod subtrees;
pub use service::{
    MAX_SYNTAX_REQUEST_BYTES, SYNTAX_BATCH_MS, SYNTAX_PROTOCOL_VERSION, SyntaxReply, SyntaxRequest,
    SyntaxWorker,
};
mod transfer;
use super::structure::fallback::FallbackContexts;
#[cfg(test)]
use super::structure::fallback::{embedded_scope_after, overlaps};
use super::{FoldRange, MAX_STRUCTURE_BYTES, SyntaxProvider, normalize_folds, syntax_provider};
use super::{Structure, SyntaxContextKind, structure::RegionKind};
use crate::highlight::Language;
use std::sync::Arc;
pub use transfer::{MAX_ANALYSIS_MESSAGE_BYTES, SyntaxAnalysisData, SyntaxSource};
use tree_sitter::{InputEdit, Node, Parser, Point, Range, Tree};

const MAX_PROGRESS_CHECKS: usize = 4_096;
const MAX_FOLD_NODES: usize = 100_000;
const MAX_INJECTIONS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SyntaxStatus {
    Ready {
        incremental: bool,
    },
    TooLarge,
    Cancelled,
    /// Worker control response: retry once with a complete source snapshot.
    NeedsSource,
}

/// Immutable source-bound preparation shared by rendering and editing callers.
#[derive(Clone, Debug)]
pub struct SyntaxAnalysis {
    source: Arc<String>,
    folds: Vec<FoldRange>,
    structure: Option<Arc<Structure>>,
    highlights: Option<Arc<crate::highlight::TokenRows>>,
}
impl SyntaxAnalysis {
    pub fn matches_source(&self, source: &str) -> bool {
        std::ptr::eq(self.source.as_str(), source) || self.source.as_str() == source
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn source_snapshot(&self) -> &Arc<String> {
        &self.source
    }
    pub fn folds(&self) -> &[FoldRange] {
        &self.folds
    }
    pub fn structure(&self) -> Option<&Arc<Structure>> {
        self.structure.as_ref()
    }
    pub fn highlights(&self) -> Option<&Arc<crate::highlight::TokenRows>> {
        self.highlights.as_ref()
    }
}

mod admission;
pub use admission::{SyntaxAdmission, SyntaxAdmissionStatus, preparation_exceeds_limits};

struct EmbeddedSyntax {
    provider: SyntaxProvider,
    tree: Option<Tree>,
    range: Range,
    fallback: std::cell::RefCell<Option<Arc<FallbackContexts>>>,
    folds: std::cell::RefCell<folds::ParsedFolds>,
    contexts: std::cell::RefCell<contexts::ParsedContexts>,
    highlights: std::cell::RefCell<highlights::ParsedHighlights>,
}

/// One outer parser plus independent embedded bodies. All positions are source coordinates.
pub struct SyntaxDocument {
    parser: Option<Parser>,
    provider: Option<SyntaxProvider>,
    language: Language,
    ready: bool,
    tree: Option<Tree>,
    embedded: Vec<EmbeddedSyntax>,
    embedded_parsers: Vec<(Language, Parser)>,
    pending: Option<preparation::SyntaxWork>,
    source_comparison: Option<preparation::SourceComparison>,
    admission: Option<SyntaxAdmission>,
    row_preparation: Option<preparation::RowPreparation>,
    outer_fallback: Option<Arc<FallbackContexts>>,
    #[cfg(test)]
    embedded_parses: usize,
    text: Arc<String>,
    source_lines: Vec<super::lines::Line>,
    prepared: Option<(usize, Arc<SyntaxAnalysis>)>,
    input_structure: Option<Arc<Structure>>,
    structure_progress: Option<(SyntaxStatus, super::structure::StructurePreparation)>,
    lexical: Option<Arc<crate::highlight::LexicalSnapshot>>,
    lexical_pending: Option<(SyntaxStatus, crate::highlight::LexicalPreparation)>,
    publication: Option<(u32, Arc<SyntaxAnalysis>)>,
    folds: std::cell::RefCell<folds::ParsedFolds>,
    contexts: std::cell::RefCell<contexts::ParsedContexts>,
    highlights: std::cell::RefCell<highlights::ParsedHighlights>,
    paint: std::cell::RefCell<highlighting::SyntaxPaint>,
}

impl SyntaxDocument {
    pub fn new(language: Language) -> Option<Self> {
        Self::with_provider(language, syntax_provider(language))
    }

    pub fn with_provider(language: Language, provider: Option<SyntaxProvider>) -> Option<Self> {
        if provider.is_some_and(|provider| provider.language != language) {
            return None;
        }
        let parser = provider.map(new_parser).transpose().ok()?;
        Some(Self {
            parser,
            provider,
            language,
            ready: false,
            tree: None,
            embedded: Vec::new(),
            embedded_parsers: Vec::new(),
            pending: None,
            source_comparison: None,
            admission: None,
            row_preparation: None,
            outer_fallback: None,
            #[cfg(test)]
            embedded_parses: 0,
            text: Arc::new(String::new()),
            source_lines: provider.map_or_else(Vec::new, |_| super::lines::lines("")),
            prepared: None,
            input_structure: None,
            structure_progress: None,
            lexical: None,
            lexical_pending: None,
            publication: None,
            folds: std::cell::RefCell::default(),
            contexts: std::cell::RefCell::default(),
            highlights: std::cell::RefCell::default(),
            paint: std::cell::RefCell::default(),
        })
    }

    pub fn update(&mut self, text: &str, should_continue: impl FnMut() -> bool) -> SyntaxStatus {
        self.update_source(text, should_continue, || Arc::new(text.to_owned()), None)
    }

    fn update_source(
        &mut self,
        text: &str,
        mut should_continue: impl FnMut() -> bool,
        source: impl FnOnce() -> Arc<String>,
        resolved_change: Option<(&Arc<String>, &super::TextChange)>,
    ) -> SyntaxStatus {
        if let Some(status) =
            self.begin_update(text, &mut should_continue, source, resolved_change, None)
        {
            return status;
        }
        self.advance_update(&mut should_continue, &mut || false)
            .expect("synchronous syntax does not yield")
    }

    /// Prepare all consumers once per source/tab width. Cancellation clears the
    /// cache; previously published snapshots remain immutable and source-bound.
    pub fn prepare(
        &mut self,
        text: &str,
        tab_width: usize,
        should_continue: impl FnMut() -> bool,
    ) -> (SyntaxStatus, Option<Arc<SyntaxAnalysis>>) {
        self.prepare_source(
            text,
            tab_width,
            should_continue,
            || Arc::new(text.to_owned()),
            None,
        )
    }

    /// Retain a host's immutable source through parsing and all prepared consumers.
    pub fn prepare_shared(
        &mut self,
        text: Arc<String>,
        tab_width: usize,
        should_continue: impl FnMut() -> bool,
    ) -> (SyntaxStatus, Option<Arc<SyntaxAnalysis>>) {
        self.prepare_source(&text, tab_width, should_continue, || text.clone(), None)
    }

    fn prepare_source(
        &mut self,
        text: &str,
        tab_width: usize,
        should_continue: impl FnMut() -> bool,
        source: impl FnOnce() -> Arc<String>,
        resolved_change: Option<(&Arc<String>, &super::TextChange)>,
    ) -> (SyntaxStatus, Option<Arc<SyntaxAnalysis>>) {
        if preparation_exceeds_limits(text) {
            self.clear();
            return (SyntaxStatus::TooLarge, None);
        }
        let mut should_continue = should_continue;
        let status = self.update_source(text, &mut should_continue, source, resolved_change);
        self.finish_preparation(status, tab_width, &mut should_continue)
    }

    fn finish_preparation(
        &mut self,
        status: SyntaxStatus,
        tab_width: usize,
        should_continue: &mut impl FnMut() -> bool,
    ) -> (SyntaxStatus, Option<Arc<SyntaxAnalysis>>) {
        let status = self
            .advance_lexical(status, should_continue, &mut || false, None)
            .expect("synchronous lexical preparation does not yield");
        self.finish_preparation_cooperative(status, tab_width, should_continue, &mut || false)
            .expect("synchronous structure preparation does not yield")
    }

    fn finish_preparation_cooperative(
        &mut self,
        status: SyntaxStatus,
        tab_width: usize,
        should_continue: &mut impl FnMut() -> bool,
        should_yield: &mut impl FnMut() -> bool,
    ) -> Option<(SyntaxStatus, Option<Arc<SyntaxAnalysis>>)> {
        if !matches!(status, SyntaxStatus::Ready { .. }) {
            return Some((status, None));
        }
        if let Some((width, analysis)) = &self.prepared
            && *width == tab_width
        {
            return Some((status, Some(analysis.clone())));
        }
        let (status, structure) =
            self.finish_structure_cooperative(status, should_continue, should_yield)?;
        if !matches!(status, SyntaxStatus::Ready { .. }) {
            return Some((status, None));
        }
        Some(self.assemble_preparation(status, tab_width, structure))
    }

    /// Prepare only editing context. Highlights and folds are background outputs,
    /// and must not consume the ordinary-input continuation budget.
    pub fn prepare_input_shared(
        &mut self,
        source: Arc<String>,
        mut should_continue: impl FnMut() -> bool,
    ) -> (SyntaxStatus, Option<Arc<SyntaxAnalysis>>) {
        self.prepare_output_cooperative(
            source,
            preparation::PreparationOutput::Input,
            &mut should_continue,
            &mut || false,
            None,
        )
        .expect("synchronous input preparation does not yield")
    }

    fn finish_input_cooperative(
        &mut self,
        status: SyntaxStatus,
        should_continue: &mut impl FnMut() -> bool,
        should_yield: &mut impl FnMut() -> bool,
    ) -> Option<(SyntaxStatus, Option<Arc<SyntaxAnalysis>>)> {
        let (status, structure) =
            self.finish_structure_cooperative(status, should_continue, should_yield)?;
        let analysis = matches!(status, SyntaxStatus::Ready { .. }).then(|| {
            Arc::new(SyntaxAnalysis {
                source: self.text.clone(),
                structure,
                highlights: None,
                folds: Vec::new(),
            })
        });
        if analysis
            .as_ref()
            .is_some_and(|analysis| analysis.record_count() > transfer::MAX_ANALYSIS_RECORDS)
        {
            self.clear();
            return Some((SyntaxStatus::TooLarge, None));
        }
        Some((status, analysis))
    }

    fn finish_structure_cooperative(
        &mut self,
        status: SyntaxStatus,
        should_continue: &mut impl FnMut() -> bool,
        should_yield: &mut impl FnMut() -> bool,
    ) -> Option<(SyntaxStatus, Option<Arc<Structure>>)> {
        if !matches!(status, SyntaxStatus::Ready { .. }) {
            return Some((status, None));
        }
        if let Some(structure) = &self.input_structure {
            return Some((status, Some(structure.clone())));
        }
        if self.structure_progress.is_none() && self.provider.is_some() {
            self.structure_progress = self.structure_preparation().map(|work| (status, work));
        }
        let mut structure = None;
        let mut status = status;
        if self.structure_progress.is_some() {
            loop {
                if !should_continue() {
                    self.clear();
                    return Some((SyntaxStatus::Cancelled, None));
                }
                if should_yield() {
                    return None;
                }
                match self.structure_progress.as_mut().unwrap().1.advance(256) {
                    Some(false) => {}
                    Some(true) => {
                        let (original_status, work) = self.structure_progress.take().unwrap();
                        status = original_status;
                        structure = work.finish().map(Arc::new);
                        break;
                    }
                    None => {
                        status = self.structure_progress.take().unwrap().0;
                        break;
                    }
                }
            }
        }
        self.input_structure = structure.clone();
        Some((status, structure))
    }

    fn assemble_preparation(
        &mut self,
        status: SyntaxStatus,
        tab_width: usize,
        structure: Option<Arc<Structure>>,
    ) -> (SyntaxStatus, Option<Arc<SyntaxAnalysis>>) {
        // update() clears preparation on every source change. Tab width only
        // affects indentation-derived folds; contexts and tokens stay source-bound.
        let (structure, highlights) = if let Some((_, previous)) = &self.prepared {
            (previous.structure.clone(), previous.highlights.clone())
        } else {
            let highlights = if self.provider.is_none() && self.language != Language::Plain {
                Some(
                    self.lexical
                        .as_ref()
                        .expect("prepared lexical source")
                        .tokens()
                        .clone(),
                )
            } else {
                structure
                    .as_ref()
                    .and_then(|structure| self.highlight_with_structure(structure))
                    .map(Arc::new)
            };
            (structure, highlights)
        };
        let analysis = Arc::new(SyntaxAnalysis {
            source: self.text.clone(),
            folds: self.folds_with_tab_width(tab_width),
            structure,
            highlights,
        });
        if analysis.record_count() > transfer::MAX_ANALYSIS_RECORDS {
            self.clear();
            return (SyntaxStatus::TooLarge, None);
        }
        self.prepared = Some((tab_width, analysis.clone()));
        (status, Some(analysis))
    }

    /// Insertions at the end of an embedded body still belong to that body.
    pub fn language_at(&self, position: usize) -> Language {
        if self.ready
            && self.structure_progress.is_none()
            && self.admission.is_none()
            && self.row_preparation.is_none()
            && self.source_comparison.is_none()
            && self.text.is_char_boundary(position)
        {
            for embedded in &self.embedded {
                if embedded.range.start_byte <= position && position <= embedded.range.end_byte {
                    return embedded.provider.language;
                }
            }
        }
        self.language
    }

    /// Immutable editing contexts from the current tree; failed/stale parses publish nothing.
    pub fn structure(&self) -> Option<Structure> {
        if self.structure_progress.is_some() {
            return None;
        }
        let mut work = self.structure_preparation()?;
        while !work.advance(256)? {}
        work.finish()
    }

    fn structure_preparation(&self) -> Option<super::structure::StructurePreparation> {
        if !self.ready
            || self.admission.is_some()
            || self.row_preparation.is_some()
            || self.source_comparison.is_some()
            || self.provider.is_none()
        {
            return None;
        }
        let fallback = self
            .outer_fallback
            .clone()
            .unwrap_or_else(|| Arc::new(FallbackContexts::scan(&self.text, self.language)));
        let scopes: Vec<_> = self
            .embedded
            .iter()
            .map(|body| {
                (
                    body.range.start_byte..body.range.end_byte,
                    body.provider.language,
                )
            })
            .collect();
        let mut embedded_fallbacks = Vec::with_capacity(self.embedded.len());
        for body in &self.embedded {
            let start = body.range.start_byte;
            let end = body.range.end_byte;
            let fallback = body
                .fallback
                .borrow_mut()
                .get_or_insert_with(|| {
                    Arc::new(FallbackContexts::scan(
                        &self.text[start..end],
                        body.provider.language,
                    ))
                })
                .clone();
            embedded_fallbacks.push((start, fallback));
        }
        let mut visited = 0;
        let mut contexts = contexts::SyntaxContexts::default();
        if let (Some(tree), Some(provider)) = (&self.tree, self.provider) {
            self.contexts
                .borrow_mut()
                .collect(tree, provider, &self.text, &mut visited, &mut contexts)
                .ok()?;
        }
        for body in &self.embedded {
            if let Some(tree) = &body.tree {
                body.contexts
                    .borrow_mut()
                    .collect(tree, body.provider, &self.text, &mut visited, &mut contexts)
                    .ok()?;
            }
        }
        let mut work = Structure::prepare_parsed(
            self.text.clone(),
            self.language,
            Vec::new(),
            scopes,
            Vec::new(),
            contexts.selections,
        );
        work.collect_fallbacks(fallback, embedded_fallbacks);
        work.reconcile(contexts.protected, contexts.holes);
        Some(work)
    }

    fn clear(&mut self) {
        if let Some(parser) = self.parser.as_mut() {
            parser.reset();
        }
        self.embedded_parsers.clear();
        self.pending = None;
        self.source_comparison = None;
        self.admission = None;
        self.row_preparation = None;
        self.outer_fallback = None;
        self.ready = false;
        self.tree = None;
        self.embedded.clear();
        self.text = Arc::new(String::new());
        self.source_lines = self
            .provider
            .map_or_else(Vec::new, |_| super::lines::lines(""));
        self.prepared = None;
        self.input_structure = None;
        self.structure_progress = None;
        self.lexical = None;
        self.lexical_pending = None;
        self.publication = None;
        self.folds.borrow_mut().clear();
        self.contexts.borrow_mut().clear();
        self.highlights.borrow_mut().clear();
        *self.paint.borrow_mut() = highlighting::SyntaxPaint::default();
    }

    pub fn folds(&self) -> Vec<FoldRange> {
        self.folds_with_tab_width(4)
    }
    pub fn folds_with_tab_width(&self, tab_width: usize) -> Vec<FoldRange> {
        if self.structure_progress.is_some()
            || !self.ready
            || self.admission.is_some()
            || self.row_preparation.is_some()
            || self.source_comparison.is_some()
        {
            return Vec::new();
        }
        let parsed = self.tree.as_ref().map(|_| self.parser_folds());
        super::fold_providers::fold_ranges_with_rows(
            &self.text,
            self.language,
            parsed.as_deref(),
            tab_width,
            self.tree.as_ref().map(|_| self.source_lines.as_slice()),
        )
    }

    fn parser_folds(&self) -> Vec<FoldRange> {
        let Some(tree) = &self.tree else {
            return Vec::new();
        };
        let Some(provider) = self.provider else {
            return Vec::new();
        };
        let lines = super::lines::SyntaxLines::new(&self.text, &self.source_lines);
        let mut ranges = Vec::new();
        let mut visited = 0;
        if self
            .folds
            .borrow_mut()
            .collect(tree, provider, lines, &mut ranges, &mut visited)
            .is_err()
        {
            return Vec::new();
        }
        for embedded in &self.embedded {
            if let Some(tree) = &embedded.tree
                && embedded
                    .folds
                    .borrow_mut()
                    .collect(tree, embedded.provider, lines, &mut ranges, &mut visited)
                    .is_err()
            {
                return Vec::new();
            }
        }
        normalize_folds(ranges, lines.len())
    }
}

fn new_parser(provider: SyntaxProvider) -> Result<Parser, SyntaxStatus> {
    let mut parser = Parser::new();
    parser
        .set_language(&(provider.grammar)())
        .map_err(|_| SyntaxStatus::Cancelled)?;
    Ok(parser)
}

#[cfg(test)]
fn visit_tree<'tree>(
    tree: &'tree Tree,
    visited: &mut usize,
    visitor: impl FnMut(Node<'tree>) -> Result<(), SyntaxStatus>,
) -> Result<(), SyntaxStatus> {
    visit_node(tree.root_node(), visited, visitor)
}

fn visit_node<'tree>(
    node: Node<'tree>,
    visited: &mut usize,
    mut visitor: impl FnMut(Node<'tree>) -> Result<(), SyntaxStatus>,
) -> Result<(), SyntaxStatus> {
    visit_node_with_parent(node, None, visited, |node, _| visitor(node))
}

fn visit_node_with_parent<'tree>(
    node: Node<'tree>,
    parent: Option<Node<'tree>>,
    visited: &mut usize,
    mut visitor: impl FnMut(Node<'tree>, Option<Node<'tree>>) -> Result<(), SyntaxStatus>,
) -> Result<(), SyntaxStatus> {
    let mut cursor = node.walk();
    let mut parents = Vec::new();
    loop {
        subtrees::charge_visits(visited, 1)?;
        let current = cursor.node();
        visitor(current, parents.last().copied().or(parent))?;
        if cursor.goto_first_child() {
            parents.push(current);
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
        }
    }
}

#[derive(Default)]
struct InjectionSelection {
    next_node: usize,
    visited: usize,
    code_bodies: usize,
    selected: Vec<(Language, Range)>,
    complete: bool,
}
impl InjectionSelection {
    /// Resume at the next descendant of the same immutable tree. No borrowed
    /// cursor or node survives a batch, and every visited node is charged once.
    fn advance(
        &mut self,
        tree: &Tree,
        provider: Option<SyntaxProvider>,
        text: &str,
        rows: &[super::lines::Line],
        should_continue: &mut impl FnMut() -> bool,
        should_yield: &mut impl FnMut() -> bool,
    ) -> Result<bool, SyntaxStatus> {
        if !should_continue() {
            return Err(SyntaxStatus::Cancelled);
        }
        if self.complete {
            return Ok(true);
        }
        let Some(select) = provider.and_then(|provider| provider.injection) else {
            self.complete = true;
            return Ok(true);
        };
        let source_point = |offset| indexed_point(rows, offset);
        let mut cursor = tree.walk();
        cursor.goto_descendant(self.next_node);
        let mut until_check = 0;
        loop {
            if until_check == 0 {
                if !should_continue() {
                    return Err(SyntaxStatus::Cancelled);
                }
                if should_yield() {
                    return Ok(false);
                }
                until_check = 256;
            }
            until_check -= 1;
            subtrees::charge_visits(&mut self.visited, 1)?;
            let node = cursor.node();
            if let Some((language, range)) = select(node, text) {
                // Inline Markdown is ordinary prose syntax, not a separately configured
                // code body. Its count remains bounded by the shared node/record budgets.
                if language != Language::MarkdownInline {
                    if self.code_bodies == MAX_INJECTIONS {
                        return Err(SyntaxStatus::TooLarge);
                    }
                    self.code_bodies += 1;
                }
                if range.start_byte > range.end_byte
                    || !text.is_char_boundary(range.start_byte)
                    || !text.is_char_boundary(range.end_byte)
                    || range.start_point != source_point(range.start_byte)
                    || range.end_point != source_point(range.end_byte)
                    || self
                        .selected
                        .last()
                        .is_some_and(|(_, previous): &(Language, Range)| {
                            previous.end_byte > range.start_byte
                        })
                {
                    return Err(SyntaxStatus::Cancelled);
                }
                self.selected.push((language, range));
            }

            if !cursor.goto_first_child() {
                loop {
                    if cursor.goto_next_sibling() {
                        break;
                    }
                    if !cursor.goto_parent() {
                        self.complete = true;
                        return Ok(true);
                    }
                }
            }
            self.next_node = cursor.descendant_index();
        }
    }
}

fn indexed_point(rows: &[super::lines::Line], offset: usize) -> Point {
    let row = super::lines::row_at(rows, offset);
    Point {
        row,
        column: offset - rows[row].start,
    }
}

fn point(text: &str, offset: usize) -> Point {
    let before = &text[..offset];
    Point {
        row: before.bytes().filter(|byte| *byte == b'\n').count(),
        column: before
            .rfind('\n')
            .map_or(offset, |newline| offset - newline - 1),
    }
}

fn input_edit(
    old: &str,
    new: &str,
    source_lines: Option<&mut Vec<super::lines::Line>>,
) -> Result<InputEdit, SyntaxStatus> {
    let change = super::text_change(old, new).unwrap_or(super::TextChange {
        range: 0..0,
        new_end: 0,
    });
    input_edit_change(old, new, source_lines, change)
}

fn input_edit_change(
    old: &str,
    new: &str,
    source_lines: Option<&mut Vec<super::lines::Line>>,
    change: super::TextChange,
) -> Result<InputEdit, SyntaxStatus> {
    let start = change.range.start;
    let old_end = change.range.end;
    let new_end = change.new_end;
    let (start_position, old_end_position, new_end_position) = if let Some(rows) = source_lines {
        let start_position = indexed_point(rows, start);
        let old_end_position = indexed_point(rows, old_end);
        let edit = super::lines::LineEdit::new(rows, old.len(), new.len(), change.range, new_end);
        let retained = rows.len() - edit.rows.len();
        let limit = super::MAX_EDITOR_LINES
            .checked_sub(retained)
            .ok_or(SyntaxStatus::TooLarge)?;
        let replacement = edit.replacement(new, limit).ok_or(SyntaxStatus::TooLarge)?;
        edit.apply(rows, replacement);
        (
            start_position,
            old_end_position,
            indexed_point(rows, new_end),
        )
    } else {
        (point(old, start), point(old, old_end), point(new, new_end))
    };
    Ok(InputEdit {
        start_byte: start,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position,
        old_end_position,
        new_end_position,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_parent_context_matches_complete_tree_parent_and_colors() {
        let mut checked = Vec::new();
        for (path, source) in [
            (
                "sample.rs",
                "fn main() { let value: Option<String> = Some(call()); } // note\n",
            ),
            (
                "sample.ts",
                "function run(value: string): string { return value; }",
            ),
            ("sample.js", "function run(value) { return call(value); }"),
            (
                "sample.tsx",
                "function App(): string { return <span id='x'>{value}</span>; }",
            ),
            (
                "sample.py",
                "class App:\n    def run(self):\n        return call(value)\n",
            ),
            (
                "sample.jsx",
                "const App = () => <span title='x'>{value}</span>;",
            ),
            (
                "sample.java",
                "class App { String run() { return call(); } }",
            ),
            ("sample.cs", "class App { string Run() => Call(); }"),
            ("sample.cpp", "struct App { int run() { return call(); } };"),
            (
                "sample.php",
                "<?php class App { function run() { return call(); } }",
            ),
            ("sample.sh", "run() { echo \"value $HOME\"; }"),
            ("sample.c", "int run(void) { return call(); }"),
            (
                "sample.go",
                "package main\nfunc run() string { return call() }",
            ),
            ("sample.html", "<div class='x'><span>value</span></div>"),
            ("sample.css", "body { color: red; margin: 1px; }"),
            ("sample.json", "{\"value\": [true, 1, \"text\"]}"),
            ("sample.yaml", "value: [true, 1, text]\nnext: |\n  text\n"),
            ("sample.toml", "[section]\nvalue = [true, 1, \"text\"]\n"),
            ("sample.ini", "[section]\nvalue = text\n"),
            (
                "sample.xml",
                "<root value='text'><child>text</child></root>",
            ),
            (
                "sample.md",
                "# Heading\n\n**text** and `code`\n\n```rust\nfn run() {}\n```\n",
            ),
        ] {
            let language = crate::highlight::language_from_path(path);
            let mut document = SyntaxDocument::new(language).unwrap();
            assert_eq!(
                document.update(source, || true),
                SyntaxStatus::Ready { incremental: false }
            );
            let mut check_tree = |tree: &Tree, provider: SyntaxProvider| {
                checked.push(provider.language);
                let root = tree.root_node();
                let check = |node: Node<'_>, parent: Option<Node<'_>>| {
                    assert_eq!(
                        parent.map(|parent| parent.id()),
                        node.parent().map(|parent| parent.id()),
                        "{path}: {}",
                        node.kind()
                    );
                    if let Some(select) = provider.highlight {
                        assert_eq!(
                            select(node, parent),
                            select(node, node.parent()),
                            "{path}: {}",
                            node.kind()
                        );
                    }
                };
                let mut visited = 0;
                visit_node_with_parent(root, None, &mut visited, |node, parent| {
                    check(node, parent);
                    Ok(())
                })
                .unwrap();
                assert!(visited > 0);
                subtrees::visit_parts(root, |node, parent, _| {
                    check(node, parent);
                    Ok(())
                })
                .unwrap();
            };
            check_tree(document.tree.as_ref().unwrap(), document.provider.unwrap());
            for body in &document.embedded {
                if let Some(tree) = &body.tree {
                    check_tree(tree, body.provider);
                }
            }
        }
        for provider in crate::editor::SYNTAX_PROVIDERS {
            assert!(
                checked.contains(&provider.language),
                "missing {:?}",
                provider.language
            );
        }
    }

    #[test]
    fn unchanged_embedded_fallbacks_survive_edits_and_coordinate_shifts() {
        for ending in ["\n", "\r\n"] {
            let source = (0..1_000)
                .map(|index| {
                    format!("Paragraph {index}: **文😀** and `code` \"literal\".{ending}{ending}")
                })
                .collect::<String>();
            let changed = source.replacen("Paragraph 500:", "Changed paragraph 500:", 1);
            let inserted = format!("New **paragraph** and `text`.{ending}{ending}{changed}");
            let mut document = SyntaxDocument::new(Language::Markdown).unwrap();
            assert!(matches!(
                document.prepare(&source, 4, || true).0,
                SyntaxStatus::Ready { .. }
            ));
            for (source, expected_new) in [(&changed, 1), (&inserted, 1), (&changed, 0)] {
                let retained: Vec<_> = document
                    .embedded
                    .iter()
                    .map(|body| body.fallback.borrow().as_ref().unwrap().clone())
                    .collect();
                let (status, actual) = document.prepare(source, 4, || true);
                assert_eq!(status, SyntaxStatus::Ready { incremental: true });
                let new_contexts = document
                    .embedded
                    .iter()
                    .filter(|body| {
                        let fallback = body.fallback.borrow();
                        !retained
                            .iter()
                            .any(|previous| Arc::ptr_eq(previous, fallback.as_ref().unwrap()))
                    })
                    .count();
                assert_eq!(
                    new_contexts, expected_new,
                    "only changed/new bodies scan fallback source"
                );
                let mut fresh = SyntaxDocument::new(Language::Markdown).unwrap();
                let (_, expected) = fresh.prepare(source, 4, || true);
                assert_eq!(
                    serde_json::to_value(actual.unwrap().transfer_data()).unwrap(),
                    serde_json::to_value(expected.unwrap().transfer_data()).unwrap()
                );
            }
        }
    }

    #[test]
    fn ordered_scope_lookup_matches_full_scan_at_every_boundary() {
        for bodies in [
            vec![],
            std::iter::once(0..0).collect(),
            vec![0..0, 0..3, 3..3, 3..5, 5..5, 8..12, 12..12],
            vec![2..4, 4..4, 4..4, 6..6, 9..13],
            (0..1_000)
                .map(|index| (index * 3)..(index * 3 + 2))
                .collect(),
        ] {
            let scopes: Vec<_> = bodies
                .into_iter()
                .map(|body| (body, Language::MarkdownInline))
                .collect();
            let limit = scopes.last().map_or(16, |(body, _)| body.end + 2);
            for start in 0..=limit {
                let body = embedded_scope_after(&scopes, start);
                assert_eq!(
                    body.is_some_and(|body| body.contains(&start)),
                    scopes.iter().any(|(body, _)| body.contains(&start))
                );
                // Include empty, touching, contained and enclosing ranges.
                for end in [start, start.saturating_add(1).min(limit), limit] {
                    let range = start..end;
                    assert_eq!(
                        body.is_some_and(|body| overlaps(&range, body)),
                        scopes.iter().any(|(body, _)| overlaps(&range, body)),
                        "{range:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn large_markdown_prose_reuses_parsers_without_joining_paragraphs() {
        for ending in ["\n", "\r\n"] {
            let source = (0..1_000)
                .map(|index| {
                    format!("Paragraph {index}: **文😀 strong** and `code`.{ending}{ending}")
                })
                .collect::<String>();
            let source = format!(
                "Unclosed `marker{ending}{ending}{source}Closing marker`{ending}{ending}```rust{ending}fn main() {{}}{ending}```{ending}"
            );
            let mut warm = SyntaxDocument::new(Language::Markdown).unwrap();
            for (revision, text) in [
                source.clone(),
                source.replacen("Paragraph 500", "Changed 500", 1),
            ]
            .into_iter()
            .enumerate()
            {
                let (status, actual) = warm.prepare(&text, 4, || true);
                assert!(matches!(status, SyntaxStatus::Ready { .. }), "{status:?}");
                assert!(warm.embedded.len() > 1_000);
                assert_eq!(warm.embedded_parsers.len(), 2);
                assert_eq!(
                    warm.embedded_parses,
                    if revision == 0 {
                        warm.embedded.len()
                    } else {
                        1
                    }
                );
                let mut fresh = SyntaxDocument::new(Language::Markdown).unwrap();
                let (_, expected) = fresh.prepare(&text, 4, || true);
                let actual = actual.unwrap();
                let expected = expected.unwrap();
                assert_eq!(actual.folds(), expected.folds());
                assert_eq!(
                    serde_json::to_value(actual.transfer_data()).unwrap(),
                    serde_json::to_value(expected.transfer_data()).unwrap()
                );
                assert_eq!(actual.highlights(), expected.highlights());
                assert_eq!(
                    actual
                        .highlights()
                        .unwrap()
                        .iter()
                        .filter(|row| row
                            .iter()
                            .any(|token| token.kind == crate::highlight::TokenKind::String
                                && token.text.contains("code")))
                        .count(),
                    1_000,
                    "each paragraph receives inline grammar colors"
                );
                let first = text.find("Unclosed").unwrap();
                let last = text.find("Closing").unwrap();
                assert_eq!(warm.language_at(first), Language::MarkdownInline);
                assert_eq!(warm.language_at(last), Language::MarkdownInline);
                assert!(warm.embedded[0].range.end_byte < last);
            }
            let (status, result) = warm.prepare(&source, 4, || false);
            assert_eq!(status, SyntaxStatus::Cancelled);
            assert!(result.is_none());
            assert!(warm.embedded_parsers.is_empty());
            assert!(matches!(
                warm.prepare(&source, 4, || true).0,
                SyntaxStatus::Ready { incremental: false }
            ));
        }
    }

    #[test]
    fn embedded_reuse_tracks_shifted_ranges_and_provider_changes() {
        for ending in ["\n", "\r\n"] {
            let base = format!(
                "first **文😀**{ending}{ending}second `code`{ending}{ending}```rust{ending}fn main() {{}}{ending}```{ending}"
            );
            let inserted = format!("new paragraph 文😀{ending}{ending}{base}");
            let after_first = base.replacen(
                "first **文😀**",
                &format!("first **文😀**{ending}{ending}new paragraph"),
                1,
            );
            let changed = base.replace("```rust", "```go");
            let mut warm = SyntaxDocument::new(Language::Markdown).unwrap();
            for (source, parses) in [
                (&base, 3),
                (&inserted, 1),
                (&base, 0),
                (&after_first, 1),
                (&base, 0),
                (&changed, 1),
            ] {
                let (status, actual) = warm.prepare(source, 4, || true);
                assert!(matches!(status, SyntaxStatus::Ready { .. }));
                assert_eq!(warm.embedded_parses, parses);
                let mut fresh = SyntaxDocument::new(Language::Markdown).unwrap();
                let (_, expected) = fresh.prepare(source, 4, || true);
                assert_eq!(
                    serde_json::to_value(actual.unwrap().transfer_data()).unwrap(),
                    serde_json::to_value(expected.unwrap().transfer_data()).unwrap()
                );
            }
        }
    }

    #[test]
    fn embedded_range_points_reuse_updated_unicode_crlf_source_rows() {
        for ending in ["\n", "\r\n"] {
            let base = format!(
                "<section>文😀</section>{ending}<script>const value = 1;{ending}console.log(value);</script>{ending}<style>body {{color:red;}}</style>{ending}"
            );
            let mut syntax = SyntaxDocument::new(Language::Html).unwrap();
            for prefix in [
                format!("外 文😀{ending}").repeat(3000),
                format!("short{ending}"),
                "文😀".into(),
                String::new(),
            ] {
                let source = format!("{prefix}{base}");
                assert!(matches!(
                    syntax.update(&source, || true),
                    SyntaxStatus::Ready { .. }
                ));
                assert_eq!(syntax.source_lines, super::super::lines::lines(&source));
                assert_eq!(syntax.embedded.len(), 2);
                for embedded in &syntax.embedded {
                    assert_eq!(
                        embedded.range.start_point,
                        point(&source, embedded.range.start_byte)
                    );
                    assert_eq!(
                        embedded.range.end_point,
                        point(&source, embedded.range.end_byte)
                    );
                }
                assert_eq!(
                    indexed_point(&syntax.source_lines, source.len()),
                    point(&source, source.len())
                );
            }
        }
    }

    #[test]
    fn incremental_parser_points_match_complete_unicode_line_scans() {
        let mut source = "文😀\r\nsecond\nlast\r\n".to_owned();
        let mut rows = super::super::lines::lines(&source);
        let payloads = ["", "文😀", "\n", "\r", "\r\n", "x\n\ny"];
        for step in 0..240 {
            let boundaries = source
                .char_indices()
                .map(|(at, _)| at)
                .chain(std::iter::once(source.len()))
                .collect::<Vec<_>>();
            let first = (step * 17 + 3) % boundaries.len();
            let last = (first + step % 5).min(boundaries.len() - 1);
            let mut changed = source.clone();
            changed.replace_range(
                boundaries[first]..boundaries[last],
                payloads[step % payloads.len()],
            );
            let incremental = input_edit(&source, &changed, Some(&mut rows)).unwrap();
            let complete = input_edit(&source, &changed, None).unwrap();
            assert_eq!(
                (
                    incremental.start_byte,
                    incremental.old_end_byte,
                    incremental.new_end_byte,
                    incremental.start_position,
                    incremental.old_end_position,
                    incremental.new_end_position
                ),
                (
                    complete.start_byte,
                    complete.old_end_byte,
                    complete.new_end_byte,
                    complete.start_position,
                    complete.old_end_position,
                    complete.new_end_position
                )
            );
            assert_eq!(rows, super::super::lines::lines(&changed));
            let indexed = super::super::lines::SyntaxLines::new(&changed, &rows);
            let complete = changed.split('\n').collect::<Vec<_>>();
            assert_eq!(indexed.len(), complete.len());
            for (index, text) in complete.into_iter().enumerate() {
                assert_eq!(indexed.get(index), Some(text));
            }
            assert!(indexed.get(indexed.len()).is_none());
            source = changed;
        }
        let changed = "";
        let incremental = input_edit(&source, changed, Some(&mut rows)).unwrap();
        assert_eq!(incremental.new_end_position, Point { row: 0, column: 0 });
        assert_eq!(rows, super::super::lines::lines(changed));
    }

    #[test]
    fn parser_rows_bound_changed_scans_and_direct_update_admission() {
        let source = "fn call() {}\r\n".repeat(2_000);
        let mut changed = source.clone();
        let last = source.rfind("call").unwrap();
        changed.replace_range(last..last + 4, "renamed");
        let rows = super::super::lines::lines(&source);
        let change = super::super::text_change(&source, &changed).unwrap();
        let edit = super::super::lines::LineEdit::new(
            &rows,
            source.len(),
            changed.len(),
            change.range,
            change.new_end,
        );
        assert!(edit.bytes.len() < 100);
        let mut document = SyntaxDocument::new(Language::Rust).unwrap();
        document.prepare(&source, 4, || true).1.unwrap();
        document.prepare(&changed, 4, || true).1.unwrap();
        assert_eq!(document.source_lines, super::super::lines::lines(&changed));
        assert_eq!(
            document.update(&"\n".repeat(super::super::MAX_EDITOR_LINES), || true),
            SyntaxStatus::TooLarge
        );
        assert_eq!(document.source_lines, super::super::lines::lines(""));
        let restored = document.prepare(&source, 4, || true).1.unwrap();
        let mut fresh = SyntaxDocument::new(Language::Rust).unwrap();
        assert_eq!(
            restored.highlights(),
            fresh.prepare(&source, 4, || true).1.unwrap().highlights()
        );
        assert_eq!(
            document.update("cancelled", || false),
            SyntaxStatus::Cancelled
        );
        assert_eq!(document.source_lines, super::super::lines::lines(""));
    }

    #[test]
    fn lexical_languages_prepare_cache_and_transfer_the_same_lossless_paint() {
        for (language, source) in [
            (
                Language::Sql,
                "/* first\r\n still comment */\r\nSELECT '文😀';\r\n",
            ),
            (Language::Sql, "SELECT '文😀';\r\n"),
        ] {
            let mut document = SyntaxDocument::new(language).unwrap();
            let (_, first) = document.prepare(source, 4, || true);
            let first = first.unwrap();
            assert!(first.structure().is_none());
            let expected = crate::highlight::share_token_rows(crate::highlight::highlight_lines(
                source, language,
            ));
            assert_eq!(first.highlights().unwrap().as_ref(), &expected);
            let (_, cached) = document.prepare(source, 4, || true);
            assert!(Arc::ptr_eq(&first, &cached.unwrap()));
            let transferred = first.transfer_data().unwrap().validate(source).unwrap();
            assert_eq!(transferred.highlights().unwrap().as_ref(), &expected);
            assert!(transferred.structure().is_none());
            let revised = source.replace("文😀", "😀 changed");
            let (_, next) = document.prepare(&revised, 4, || true);
            assert_eq!(
                next.unwrap().highlights().unwrap().as_ref(),
                &crate::highlight::share_token_rows(crate::highlight::highlight_lines(
                    &revised, language
                ))
            );
            assert!(first.matches_source(source));
            assert_eq!(document.lexical.as_ref().unwrap().retokenized_rows(), 1);
        }
    }

    #[test]
    fn lexical_preparation_cancellation_never_publishes_partial_rows() {
        let mut document = SyntaxDocument::new(Language::Sql).unwrap();
        let source = "/* first\r\nstill comment */\r\nSELECT '文😀';";
        let mut checks = 0;
        let (status, result) = document.prepare(source, 4, || {
            checks += 1;
            checks < 3
        });
        assert_eq!(status, SyntaxStatus::Cancelled);
        assert!(result.is_none() && document.prepared.is_none() && document.lexical.is_none());
        let (status, recovered) = document.prepare(source, 4, || true);
        assert_eq!(status, SyntaxStatus::Ready { incremental: false });
        assert_eq!(
            recovered.unwrap().highlights().unwrap().as_ref(),
            &crate::highlight::share_token_rows(crate::highlight::highlight_lines(
                source,
                Language::Sql
            ))
        );
    }

    #[test]
    fn preparation_reuses_one_snapshot_and_retains_immutable_old_sources() {
        let source = "fn main() {\r\n    call(\"文😀\");\r\n}\r\n";
        let mut syntax = SyntaxDocument::new(Language::Rust).unwrap();
        let (status, first) = syntax.prepare(source, 4, || true);
        assert_eq!(status, SyntaxStatus::Ready { incremental: false });
        let first = first.unwrap();
        let (_, again) = syntax.prepare(source, 4, || true);
        assert!(Arc::ptr_eq(&first, &again.unwrap()));
        let (_, width) = syntax.prepare(source, 8, || true);
        assert!(!Arc::ptr_eq(&first, &width.unwrap()));
        let revised = source.replace("文😀", "😀 changed");
        let (status, next) = syntax.prepare(&revised, 4, || true);
        assert_eq!(status, SyntaxStatus::Ready { incremental: true });
        let next = next.unwrap();
        assert!(!Arc::ptr_eq(&first, &next));
        assert!(first.matches_source(source));
        assert!(!first.matches_source(&revised));
        assert!(first.structure().unwrap().matches_source(source));
        assert!(next.structure().unwrap().matches_source(&revised));
        assert_eq!(first.source(), source);
        assert_eq!(next.source(), revised);
    }

    #[test]
    fn reused_contexts_match_fresh_structures_across_provider_edits() {
        let sources = super::super::syntax_contracts::LANGUAGE_CASES
            .iter()
            .map(|&(path, source, _)| (crate::highlight::language_from_path(path), source))
            .chain([
                (
                    Language::JavaScript,
                    "function f() { return `text ${call(\"inner\")} tail`; }\r\n",
                ),
                (
                    Language::CSharp,
                    "class C { string s = $\"text {call(\"inner\")} tail\"; }\r\n",
                ),
                (
                    Language::Php,
                    "<?php function f() { return \"text {$items[\"inner\"]} tail\"; } ?>\r\n",
                ),
                (
                    Language::Shell,
                    "f() { echo \"text $(call 'inner') tail\"; }\r\n",
                ),
            ]);
        for (language, source) in sources {
            let original = source.repeat(3);
            let mut syntax = SyntaxDocument::new(language).unwrap();
            for text in [
                original.clone(),
                format!("\r\n{original}"),
                original.replacen("文😀", "😀 changed", 1),
                original.replacen("inner", "other", 1),
                original.replacen("inner", "unterminated\"", 1),
                original.replace('}', ""),
                format!("文😀 {original}"),
                original,
            ] {
                syntax.update(&text, || true);
                let context = syntax.structure().unwrap();
                let mut fresh = SyntaxDocument::new(language).unwrap();
                fresh.update(&text, || true);
                assert_eq!(
                    serde_json::to_value(context.transfer_data()).unwrap(),
                    serde_json::to_value(fresh.structure().unwrap().transfer_data()).unwrap(),
                    "{language:?}: {text}"
                );
                assert!(context.matches_source(&text));
            }
        }
    }

    #[test]
    fn contexts_skip_unchanged_subtrees_and_preserve_work_limits() {
        let source = (0..200)
            .map(|index| format!("fn f{index}() {{ call(\"文😀\"); }}\r\n"))
            .collect::<String>();
        let mut syntax = SyntaxDocument::new(Language::Rust).unwrap();
        syntax.update(&source, || true);
        let original = syntax.structure().unwrap();
        let changed = format!("// before\r\n{source}");
        syntax.update(&changed, || true);
        let next = syntax.structure().unwrap();
        let mut nodes = 0;
        visit_tree(syntax.tree.as_ref().unwrap(), &mut nodes, |_| Ok(())).unwrap();
        assert!(syntax.contexts.borrow().reused_nodes > nodes / 2);
        assert!(original.matches_source(&source));
        assert!(next.matches_source(&changed));
        let mut visited = MAX_FOLD_NODES - 1;
        let mut contexts = contexts::SyntaxContexts::default();
        assert_eq!(
            syntax.contexts.borrow_mut().collect(
                syntax.tree.as_ref().unwrap(),
                syntax.provider.unwrap(),
                &changed,
                &mut visited,
                &mut contexts
            ),
            Err(SyntaxStatus::TooLarge)
        );
        syntax.update(&changed, || false);
        assert_eq!(syntax.contexts.borrow().reused_nodes, 0);
        assert!(syntax.structure().is_none());
    }

    #[test]
    fn stable_interpolation_ancestors_preserve_subtree_reuse() {
        let source = (0..200)
            .map(|index| {
                format!("function f{index}() {{ return `text ${{call(\"inner\")}} tail`; }}\r\n")
            })
            .collect::<String>();
        let mut syntax = SyntaxDocument::new(Language::JavaScript).unwrap();
        syntax.update(&source, || true);
        let original = syntax.structure().unwrap();
        let changed = format!("// before\r\n{source}");
        syntax.update(&changed, || true);
        let next = syntax.structure().unwrap();
        let mut nodes = 0;
        visit_tree(syntax.tree.as_ref().unwrap(), &mut nodes, |_| Ok(())).unwrap();
        assert!(syntax.contexts.borrow().reused_nodes > nodes / 2);
        let mut fresh = SyntaxDocument::new(Language::JavaScript).unwrap();
        fresh.update(&changed, || true);
        assert_eq!(
            serde_json::to_value(next.transfer_data()).unwrap(),
            serde_json::to_value(fresh.structure().unwrap().transfer_data()).unwrap()
        );
        assert!(original.matches_source(&source));
    }

    #[test]
    fn changed_ancestor_classification_cannot_reuse_interpolation_contexts() {
        fn context(node: Node<'_>) -> Option<SyntaxContextKind> {
            match node.kind() {
                "source_file" if node.has_error() => Some(SyntaxContextKind::String),
                "identifier" => Some(SyntaxContextKind::Interpolation),
                _ => None,
            }
        }
        let provider = SyntaxProvider {
            context: Some(context),
            context_scope: super::super::SyntaxContextScope::Node,
            ..syntax_provider(Language::Rust).unwrap()
        };
        let source = "fn first() { call(); }\r\nfn second() { call(); }\r\n";
        let mut syntax = SyntaxDocument::with_provider(Language::Rust, Some(provider)).unwrap();
        syntax.update(source, || true);
        syntax.structure().unwrap();
        let changed = format!("{source}fn broken(");
        syntax.update(&changed, || true);
        let next = syntax.structure().unwrap();
        let mut fresh = SyntaxDocument::with_provider(Language::Rust, Some(provider)).unwrap();
        fresh.update(&changed, || true);
        assert_eq!(
            serde_json::to_value(next.transfer_data()).unwrap(),
            serde_json::to_value(fresh.structure().unwrap().transfer_data()).unwrap()
        );
    }

    #[test]
    fn interpolation_owners_outside_a_subtree_are_recomputed() {
        fn context(node: Node<'_>) -> Option<SyntaxContextKind> {
            match node.kind() {
                "source_file" => Some(SyntaxContextKind::String),
                "identifier" => Some(SyntaxContextKind::Interpolation),
                _ => None,
            }
        }
        let provider = SyntaxProvider {
            context: Some(context),
            context_scope: super::super::SyntaxContextScope::Node,
            ..syntax_provider(Language::Rust).unwrap()
        };
        // The root owner's byte range initially coincides with its only child.
        let source = "fn first() { call(); }";
        let mut syntax = SyntaxDocument::with_provider(Language::Rust, Some(provider)).unwrap();
        syntax.update(source, || true);
        syntax.structure().unwrap();
        let changed = format!("fn before() {{}}\r\n{source}");
        syntax.update(&changed, || true);
        let next = syntax.structure().unwrap();
        let mut fresh = SyntaxDocument::with_provider(Language::Rust, Some(provider)).unwrap();
        fresh.update(&changed, || true);
        assert_eq!(
            serde_json::to_value(next.transfer_data()).unwrap(),
            serde_json::to_value(fresh.structure().unwrap().transfer_data()).unwrap()
        );
        assert_eq!(syntax.contexts.borrow().reused_nodes, 0);
    }

    #[test]
    fn document_dependent_classifiers_do_not_reuse_contexts() {
        fn context(node: Node<'_>) -> Option<SyntaxContextKind> {
            if node.kind() != "identifier" {
                return None;
            }
            let mut root = node;
            while let Some(parent) = root.parent() {
                root = parent;
            }
            root.has_error().then_some(SyntaxContextKind::String)
        }
        let provider = SyntaxProvider {
            context: Some(context),
            context_scope: super::super::SyntaxContextScope::Document,
            ..syntax_provider(Language::Rust).unwrap()
        };
        let source = "fn first() { call(); }\r\nfn second() { call(); }\r\n";
        let mut syntax = SyntaxDocument::with_provider(Language::Rust, Some(provider)).unwrap();
        syntax.update(source, || true);
        let original = syntax.structure().unwrap();
        let changed = format!("{source}fn broken(");
        syntax.update(&changed, || true);
        let next = syntax.structure().unwrap();
        let mut fresh = SyntaxDocument::with_provider(Language::Rust, Some(provider)).unwrap();
        fresh.update(&changed, || true);
        assert_eq!(
            serde_json::to_value(next.transfer_data()).unwrap(),
            serde_json::to_value(fresh.structure().unwrap().transfer_data()).unwrap()
        );
        assert_ne!(original.protected, next.protected);
        assert_eq!(syntax.contexts.borrow().reused_nodes, 0);
    }

    #[test]
    fn reused_parser_folds_match_fresh_parses_across_provider_edits() {
        for &(path, source, _) in super::super::syntax_contracts::LANGUAGE_CASES {
            let language = crate::highlight::language_from_path(path);
            let original = source.repeat(3);
            let changed = original.replace("文😀", "😀 changed");
            let mut syntax = SyntaxDocument::new(language).unwrap();
            for text in [
                original.clone(),
                format!("\r\n{original}"),
                changed.clone(),
                changed.replace("😀 changed", "文"),
                format!("{source}\r\n{changed}"),
                format!("文😀 {original}"),
                original.replace('}', ""),
                original,
            ] {
                syntax.update(&text, || true);
                let mut fresh = SyntaxDocument::new(language).unwrap();
                fresh.update(&text, || true);
                assert_eq!(
                    syntax.parser_folds(),
                    fresh.parser_folds(),
                    "{path}: {text}"
                );
                assert_eq!(
                    syntax.folds_with_tab_width(3),
                    fresh.folds_with_tab_width(3)
                );
            }
        }
    }

    #[test]
    fn nested_container_edits_reuse_sibling_folds_and_contexts() {
        for (language, open, member, close) in [
            (
                Language::Rust,
                "impl Example {\r\n",
                "fn fINDEX() {\r\n call(\"文😀\");\r\n}\r\n",
                "}\r\n",
            ),
            (
                Language::CSharp,
                "class Example {\r\n",
                "void F_INDEX() {\r\n Call(\"文😀\");\r\n}\r\n",
                "}\r\n",
            ),
            (
                Language::Java,
                "class Example {\r\n",
                "void fINDEX() {\r\n call(\"文😀\");\r\n}\r\n",
                "}\r\n",
            ),
            (
                Language::JavaScript,
                "class Example {\r\n",
                "fINDEX() {\r\n call(`文😀 ${inner(\"value\")}`);\r\n}\r\n",
                "}\r\n",
            ),
            (
                Language::Python,
                "class Example:\r\n",
                "    def fINDEX(self):\r\n        call(\"文😀\")\r\n",
                "",
            ),
            (
                Language::Html,
                "<div>\r\n",
                "<section id=\"INDEX\">\r\n<p>文😀</p>\r\n</section>\r\n",
                "</div>\r\n",
            ),
        ] {
            let body = (0..200)
                .map(|index| member.replace("INDEX", &index.to_string()))
                .collect::<String>();
            let source = format!("{open}{body}{close}");
            let mut syntax = SyntaxDocument::new(language).unwrap();
            syntax.update(&source, || true);
            syntax.structure().unwrap();
            syntax.parser_folds();
            let changed = source.replacen("文😀", "😀 changed文", 1);
            syntax.update(&changed, || true);
            let context = syntax.structure().unwrap();
            let folds = syntax.parser_folds();
            let mut nodes = 0;
            visit_tree(syntax.tree.as_ref().unwrap(), &mut nodes, |_| Ok(())).unwrap();
            assert!(
                syntax.contexts.borrow().reused_nodes > nodes / 3,
                "{language:?}: contexts reused {} of {nodes}",
                syntax.contexts.borrow().reused_nodes
            );
            assert!(
                syntax.folds.borrow().reused_nodes > nodes / 3,
                "{language:?}: folds"
            );
            let mut visited = MAX_FOLD_NODES - nodes;
            let mut ranges = Vec::new();
            syntax
                .folds
                .borrow_mut()
                .collect(
                    syntax.tree.as_ref().unwrap(),
                    syntax.provider.unwrap(),
                    super::super::lines::SyntaxLines::new(&changed, &syntax.source_lines),
                    &mut ranges,
                    &mut visited,
                )
                .unwrap();
            assert_eq!(
                visited, MAX_FOLD_NODES,
                "{language:?}: disjoint visit credits"
            );
            let mut fresh = SyntaxDocument::new(language).unwrap();
            fresh.update(&changed, || true);
            assert_eq!(folds, fresh.parser_folds(), "{language:?}");
            assert_eq!(
                serde_json::to_value(context.transfer_data()).unwrap(),
                serde_json::to_value(fresh.structure().unwrap().transfer_data()).unwrap(),
                "{language:?}"
            );
            // Shift, error recovery and undo cross the changed outer container.
            for text in [format!("\r\n{changed}"), changed.replace('}', ""), source] {
                syntax.update(&text, || true);
                let mut fresh = SyntaxDocument::new(language).unwrap();
                fresh.update(&text, || true);
                assert_eq!(
                    syntax.parser_folds(),
                    fresh.parser_folds(),
                    "{language:?}: {text}"
                );
                assert_eq!(
                    serde_json::to_value(syntax.structure().unwrap().transfer_data()).unwrap(),
                    serde_json::to_value(fresh.structure().unwrap().transfer_data()).unwrap(),
                    "{language:?}: {text}"
                );
            }
        }
    }

    #[test]
    fn parser_folds_skip_unchanged_subtrees_without_bypassing_limits() {
        let source = (0..200)
            .map(|index| format!("fn f{index}() {{\r\n call(\"文😀\");\r\n}}\r\n"))
            .collect::<String>();
        let mut syntax = SyntaxDocument::new(Language::Rust).unwrap();
        syntax.update(&source, || true);
        syntax.parser_folds();
        let changed = format!("// shifted\r\n{source}");
        syntax.update(&changed, || true);
        let folds = syntax.parser_folds();
        let mut nodes = 0;
        visit_tree(syntax.tree.as_ref().unwrap(), &mut nodes, |_| Ok(())).unwrap();
        assert!(syntax.folds.borrow().reused_nodes > nodes / 2);
        let mut fresh = SyntaxDocument::new(Language::Rust).unwrap();
        fresh.update(&changed, || true);
        assert_eq!(folds, fresh.parser_folds());
        let mut visited = MAX_FOLD_NODES - 1;
        let mut ranges = Vec::new();
        assert_eq!(
            syntax.folds.borrow_mut().collect(
                syntax.tree.as_ref().unwrap(),
                syntax.provider.unwrap(),
                super::super::lines::SyntaxLines::new(&changed, &syntax.source_lines),
                &mut ranges,
                &mut visited,
            ),
            Err(SyntaxStatus::TooLarge)
        );
        syntax.update(&changed, || false);
        assert_eq!(syntax.folds.borrow().reused_nodes, 0);
        assert!(syntax.parser_folds().is_empty());
    }

    #[test]
    fn parent_owned_fold_headers_are_not_rebased_from_an_external_owner() {
        let provider = SyntaxProvider {
            fold_nodes: &["function_item"],
            parent_headers: &["function_item"],
            ..syntax_provider(Language::Rust).unwrap()
        };
        let source = "fn main() {\n call();\n}\n";
        let mut syntax = SyntaxDocument::with_provider(Language::Rust, Some(provider)).unwrap();
        syntax.update(source, || true);
        syntax.parser_folds();
        let changed = format!("// before\n{source}");
        syntax.update(&changed, || true);
        let mut fresh = SyntaxDocument::with_provider(Language::Rust, Some(provider)).unwrap();
        fresh.update(&changed, || true);
        let folds = syntax.parser_folds();
        assert_eq!(folds, fresh.parser_folds());
        assert_eq!(folds[0].start_line, 0);
        // The external header is fresh while its unchanged descendants reuse data.
        assert!(syntax.folds.borrow().reused_nodes > 0);
    }

    #[test]
    fn tab_width_changes_share_source_metadata_and_recompute_folds() {
        let providers = super::super::syntax_contracts::LANGUAGE_CASES
            .iter()
            .map(|&(path, source, _)| (crate::highlight::language_from_path(path), source));
        let lexical = [
            (Language::Json, "{\r\n\t\"name\": \"文😀\"\r\n}"),
            (
                Language::Sql,
                "/* first\r\n still comment */\r\nSELECT '文😀';",
            ),
            (Language::Plain, "root\r\n\tbranch\r\n    文😀\r\nend"),
        ];
        for (language, source) in providers.chain(lexical) {
            let mut syntax = SyntaxDocument::new(language).unwrap();
            let (_, first) = syntax.prepare(source, 3, || true);
            let first = first.unwrap();
            for width in [8, 2, 3] {
                let (_, next) = syntax.prepare(source, width, || true);
                let next = next.unwrap();
                assert!(!Arc::ptr_eq(&first, &next));
                assert!(Arc::ptr_eq(first.source_snapshot(), next.source_snapshot()));
                match (first.structure(), next.structure()) {
                    (Some(first), Some(next)) => assert!(Arc::ptr_eq(first, next)),
                    (None, None) => (),
                    _ => panic!("structure changed with tab width for {language:?}"),
                }
                match (first.highlights(), next.highlights()) {
                    (Some(first), Some(next)) => assert!(Arc::ptr_eq(first, next)),
                    (None, None) => (),
                    _ => panic!("tokens changed with tab width for {language:?}"),
                }
                let mut fresh = SyntaxDocument::new(language).unwrap();
                let (_, expected) = fresh.prepare(source, width, || true);
                let expected = expected.unwrap();
                assert_eq!(
                    next.folds(),
                    expected.folds(),
                    "{language:?}, width={width}"
                );
                if language == Language::Plain && width == 8 {
                    assert_ne!(
                        first.folds(),
                        next.folds(),
                        "tab stops must change nested folds"
                    );
                }
                assert_eq!(next.highlights(), expected.highlights());
            }
            let changed = source.replace("文😀", "😀 changed");
            let (_, next) = syntax.prepare(&changed, 8, || true);
            let next = next.unwrap();
            assert!(next.matches_source(&changed));
            assert!(!Arc::ptr_eq(
                first.source_snapshot(),
                next.source_snapshot()
            ));
            if let (Some(first), Some(next)) = (first.structure(), next.structure()) {
                assert!(!Arc::ptr_eq(first, next));
            }
            if let (Some(first), Some(next)) = (first.highlights(), next.highlights()) {
                assert!(!Arc::ptr_eq(first, next));
            }
            let (status, cancelled) = syntax.prepare(&changed, 4, || false);
            assert_eq!(status, SyntaxStatus::Cancelled);
            assert!(cancelled.is_none());
            let (_, recovered) = syntax.prepare(&changed, 4, || true);
            let recovered = recovered.unwrap();
            assert!(!Arc::ptr_eq(
                next.source_snapshot(),
                recovered.source_snapshot()
            ));
            assert!(first.matches_source(source));
        }
    }

    #[test]
    fn input_preparation_checks_budget_during_admission_and_reuses_cooperative_rows() {
        let source = Arc::new(format!("// {}\nfn main() {{}}\n", "x".repeat(128 * 1024)));
        let mut syntax = SyntaxDocument::new(Language::Rust).unwrap();
        let mut checks = 0;
        let (status, rejected) = syntax.prepare_input_shared(source.clone(), || {
            checks += 1;
            checks < 3
        });
        assert_eq!(status, SyntaxStatus::Cancelled);
        assert!(rejected.is_none());
        assert!(!syntax.ready);
        assert!(syntax.source_lines.len() <= 1);
        assert!(syntax.input_structure.is_none());

        let mut yields = 0;
        let input = loop {
            let mut batches = 0;
            let result = syntax.prepare_output_cooperative(
                source.clone(),
                preparation::PreparationOutput::Input,
                &mut || true,
                &mut || {
                    batches += 1;
                    batches > 1
                },
                None,
            );
            if let Some((status, result)) = result {
                assert!(matches!(status, SyntaxStatus::Ready { .. }));
                break result.unwrap();
            }
            yields += 1;
            assert!(yields < 10_000, "input preparation must finish");
        };
        assert!(yields > 1);
        assert!(Arc::ptr_eq(input.source_snapshot(), &source));
        assert!(input.highlights().is_none());
        assert!(input.folds().is_empty());
        assert_eq!(syntax.source_lines, super::super::lines::lines(&source));
        let changed = Arc::new(source.replace("main", "changed"));
        let (_, current) = syntax.prepare_input_shared(changed.clone(), || true);
        assert!(current.unwrap().matches_source(&changed));
        assert!(input.matches_source(&source));
        assert_eq!(syntax.source_lines, super::super::lines::lines(&changed));
        let (status, result) = syntax.prepare_input_shared(Arc::new("\n".repeat(50_000)), || true);
        assert_eq!(status, SyntaxStatus::TooLarge);
        assert!(result.is_none());
        assert!(syntax.input_structure.is_none());
    }

    #[test]
    fn input_preparation_shares_structure_without_preparing_decorations() {
        for &(path, source, _) in super::super::syntax_contracts::LANGUAGE_CASES {
            let language = crate::highlight::language_from_path(path);
            let source = Arc::new(source.to_owned());
            let mut syntax = SyntaxDocument::new(language).unwrap();
            let (status, input) = syntax.prepare_input_shared(source.clone(), || true);
            assert!(matches!(status, SyntaxStatus::Ready { .. }), "{path}");
            let input = input.unwrap();
            assert!(Arc::ptr_eq(input.source_snapshot(), &source));
            assert!(input.highlights().is_none());
            assert!(input.folds().is_empty());
            assert!(
                syntax.prepared.is_none(),
                "input must not prepare decorations"
            );
            let (_, full) = syntax.prepare_shared(source.clone(), 4, || true);
            let full = full.unwrap();
            match (input.structure(), full.structure()) {
                (Some(input), Some(full)) => assert!(Arc::ptr_eq(input, full), "{path}"),
                (None, None) => (),
                _ => panic!("input/full context mismatch: {path}"),
            }
            let mut fresh = SyntaxDocument::new(language).unwrap();
            let (_, expected) = fresh.prepare_shared(source, 4, || true);
            let expected = expected.unwrap();
            assert_eq!(full.highlights(), expected.highlights(), "{path}");
            assert_eq!(full.folds(), expected.folds(), "{path}");
        }
    }

    #[test]
    fn input_preparation_rejects_cancelled_and_replaced_sources() {
        for (language, source) in [
            (Language::Html, "<script>function run() {}</script>"),
            (Language::Markdown, "```javascript\nfunction run() {}\n```"),
        ] {
            let source = Arc::new(source.to_owned());
            let mut syntax = SyntaxDocument::new(language).unwrap();
            let (_, old) = syntax.prepare_input_shared(source.clone(), || true);
            let old = old.unwrap();
            let changed = Arc::new(source.replace("{}", "{ // changed\n}"));
            let (status, rejected) = syntax.prepare_input_shared(changed.clone(), || false);
            assert_eq!(status, SyntaxStatus::Cancelled);
            assert!(rejected.is_none());
            assert!(syntax.input_structure.is_none());
            let (_, current) = syntax.prepare_input_shared(changed.clone(), || true);
            let current = current.unwrap();
            assert!(!current.matches_source(&source));
            assert!(current.matches_source(&changed));
            assert!(!Arc::ptr_eq(
                old.structure().unwrap(),
                current.structure().unwrap()
            ));
            assert!(old.matches_source(&source));
            let too_large = Arc::new("x".repeat(MAX_STRUCTURE_BYTES + 1));
            let (status, rejected) = syntax.prepare_input_shared(too_large, || true);
            assert_eq!(status, SyntaxStatus::TooLarge);
            assert!(rejected.is_none());
            assert!(syntax.input_structure.is_none());
        }
    }

    #[test]
    fn prepared_consumers_match_independent_analysis_for_all_providers() {
        for &(path, source, _) in super::super::syntax_contracts::LANGUAGE_CASES {
            let language = crate::highlight::language_from_path(path);
            let mut syntax = SyntaxDocument::new(language).unwrap();
            let (_, analysis) = syntax.prepare(source, 3, || true);
            let analysis = analysis.unwrap();
            assert_eq!(analysis.folds(), syntax.folds_with_tab_width(3));
            assert_eq!(
                analysis.highlights().unwrap().as_ref(),
                &crate::highlight::share_token_rows(syntax.highlight_lines().unwrap())
            );
            let context = syntax.structure().unwrap();
            assert_eq!(analysis.structure().unwrap().protected, context.protected);
            assert_eq!(analysis.structure().unwrap().brackets, context.brackets);
            assert!(
                analysis
                    .structure()
                    .unwrap()
                    .matches_source(analysis.source())
            );
        }
    }

    #[test]
    fn shared_preparation_retains_host_source_across_consumers_and_edits() {
        for language in [
            Language::Rust,
            Language::Python,
            Language::Json,
            Language::Plain,
        ] {
            let source = Arc::new("fn main() { 文😀(); }\r\n".to_owned());
            let mut syntax = SyntaxDocument::new(language).unwrap();
            let (_, first) = syntax.prepare_shared(source.clone(), 4, || true);
            let first = first.unwrap();
            assert!(Arc::ptr_eq(&source, first.source_snapshot()));
            assert!(Arc::ptr_eq(&source, &syntax.text));
            if let Some(structure) = first.structure() {
                assert!(structure.matches_source(&source));
            }
            let (_, width) = syntax.prepare_shared(source.clone(), 8, || true);
            assert!(Arc::ptr_eq(&source, width.unwrap().source_snapshot()));
            let changed = Arc::new(format!("{source}\nnext"));
            let (_, next) = syntax.prepare_shared(changed.clone(), 4, || true);
            assert!(Arc::ptr_eq(&changed, next.unwrap().source_snapshot()));
            assert_eq!(first.source(), source.as_str());
            assert_eq!(
                syntax.prepare_shared(changed.clone(), 4, || false).0,
                SyntaxStatus::Cancelled
            );
            let (_, restored) = syntax.prepare_shared(source.clone(), 4, || true);
            assert!(Arc::ptr_eq(&source, restored.unwrap().source_snapshot()));
        }
    }

    #[test]
    fn resolved_parser_spans_require_the_exact_cached_base() {
        let original = Arc::new("fn main() { let value = \"文😀\"; }\r\n".to_owned());
        let revised = Arc::new(original.replace("value", "updated_value"));
        let broad = crate::editor::TextChange {
            range: 0..original.len(),
            new_end: revised.len(),
        };
        let minimal = crate::editor::text_change(&original, &revised).unwrap();
        for exact_base in [true, false] {
            let mut syntax = SyntaxDocument::new(Language::Rust).unwrap();
            let (_, retained) = syntax.prepare_shared(original.clone(), 4, || true);
            let retained = retained.unwrap();
            let base = if exact_base {
                original.clone()
            } else {
                Arc::new(original.as_ref().clone())
            };
            assert_eq!(
                syntax.update_source(&revised, || true, || revised.clone(), Some((&base, &broad))),
                SyntaxStatus::Ready { incremental: true }
            );
            let edit = syntax.paint.borrow().source_change.unwrap();
            let expected = if exact_base { &broad } else { &minimal };
            assert_eq!(edit.start_byte, expected.range.start);
            assert_eq!(edit.old_end_byte, expected.range.end);
            assert_eq!(edit.new_end_byte, expected.new_end);
            let (_, updated) = syntax.prepare_shared(revised.clone(), 4, || true);
            let updated = updated.unwrap();
            let mut fresh = SyntaxDocument::new(Language::Rust).unwrap();
            let (_, complete) = fresh.prepare_shared(revised.clone(), 4, || true);
            let complete = complete.unwrap();
            assert_eq!(updated.folds(), complete.folds());
            assert_eq!(updated.highlights(), complete.highlights());
            assert!(Arc::ptr_eq(updated.source_snapshot(), &revised));
            assert!(Arc::ptr_eq(retained.source_snapshot(), &original));
            assert_eq!(retained.source(), original.as_str());
        }
    }

    #[test]
    fn cancellation_direct_updates_and_size_limits_invalidate_preparation() {
        let source = "fn main() {}";
        let mut syntax = SyntaxDocument::new(Language::Rust).unwrap();
        let (_, first) = syntax.prepare(source, 4, || true);
        let first = first.unwrap();
        let (status, analysis) = syntax.prepare(source, 4, || false);
        assert_eq!(status, SyntaxStatus::Cancelled);
        assert!(analysis.is_none());
        let (status, recovered) = syntax.prepare(source, 4, || true);
        assert_eq!(status, SyntaxStatus::Ready { incremental: false });
        assert!(!Arc::ptr_eq(&first, &recovered.unwrap()));
        syntax.update("fn changed() {}", || true);
        assert!(syntax.prepared.is_none());
        let (status, oversized) = syntax.prepare(&"x".repeat(MAX_STRUCTURE_BYTES + 1), 4, || true);
        assert_eq!(status, SyntaxStatus::TooLarge);
        assert!(oversized.is_none());
        assert!(first.matches_source(source));
        let mut plain = SyntaxDocument::new(Language::Plain).unwrap();
        let (_, plain) = plain.prepare("hello", 4, || true);
        let plain = plain.unwrap();
        assert_eq!(plain.source(), "hello");
        assert!(plain.structure().is_none() && plain.highlights().is_none());
    }

    #[test]
    fn html_embedded_bodies_keep_global_coordinates_incremental_trees_and_languages() {
        let original = "<header>文😀</header>\n<script type=module>\nfunction first() {\n  return { label: \"{\" };\n}\n</script>\n<style>\n.main {\n  content: \"}\";\n}\n</style>\n<script type=application/json>\n{\n  \"not\": \"code\"\n}\n</script>\n<script TYPE=\"text&sol;javascript\">\nfunction second() {\n  if (true) {\n    console.log(\"文😀\");\n  }\n}\n</script>\n";
        let mut syntax = SyntaxDocument::new(Language::Html).unwrap();
        let revised = original
            .replace(
                "<header>文😀</header>",
                "<header>😀文 changed</header>\n<p>prefix</p>",
            )
            .replace("return { label", "return { renamed")
            .replace('\n', "\r\n");
        let reordered = revised
            .replace("type=module", "type=application/json")
            .replace("TYPE=\"text&sol;javascript\"", "type=module");
        for (index, source) in [original.to_string(), revised, reordered]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                syntax.update(&source, || true),
                SyntaxStatus::Ready {
                    incremental: index != 0
                }
            );
            let mut fresh = SyntaxDocument::new(Language::Html).unwrap();
            fresh.update(&source, || true);
            assert_eq!(syntax.folds(), fresh.folds());
            assert_eq!(
                positions(syntax.tree.as_ref().unwrap()),
                positions(fresh.tree.as_ref().unwrap())
            );
            assert_eq!(syntax.embedded.len(), fresh.embedded.len());
            for (embedded, fresh) in syntax.embedded.iter().zip(&fresh.embedded) {
                assert_eq!(embedded.range, fresh.range);
                assert_eq!(
                    positions(embedded.tree.as_ref().unwrap()),
                    positions(fresh.tree.as_ref().unwrap())
                );
                assert!(!embedded.tree.as_ref().unwrap().root_node().has_error());
                assert_eq!(
                    embedded.range.start_point,
                    point(&source, embedded.range.start_byte)
                );
            }
            assert_eq!(
                syntax.language_at(source.find("content:").unwrap()),
                Language::Css
            );
            assert_eq!(
                syntax.language_at(source.find("not").unwrap()),
                Language::Html
            );
            assert_eq!(
                syntax.language_at(source.find("console.log").unwrap()),
                Language::JavaScript
            );
        }
        syntax.update(original, || true);
        assert_eq!(syntax.embedded.len(), 3);
        for expected in [
            FoldRange {
                start_line: 2,
                end_line: 4,
            },
            FoldRange {
                start_line: 7,
                end_line: 9,
            },
            FoldRange {
                start_line: 17,
                end_line: 21,
            },
        ] {
            assert!(syntax.folds().contains(&expected), "{:?}", syntax.folds());
        }
        assert_eq!(syntax.update(original, || false), SyntaxStatus::Cancelled);
        assert!(syntax.embedded.is_empty());
        assert_eq!(
            syntax.language_at(original.find("content:").unwrap()),
            Language::Html
        );
    }

    #[test]
    fn parsed_contexts_keep_interpolation_code_nested_literals_and_embedded_scopes() {
        let source = "const s = `hello ${call({x: \"}\"})} tail`;";
        let mut syntax = SyntaxDocument::new(Language::JavaScript).unwrap();
        syntax.update(source, || true);
        let structure = syntax.structure().unwrap();
        assert!(!structure.is_code(source.find("hello").unwrap()));
        assert!(structure.is_code(source.find("call").unwrap()));
        assert!(!structure.is_code(source.find("\"}\"").unwrap() + 1));
        assert!(!structure.is_code(source.find(" tail").unwrap()));
        assert!(!structure.is_code(source.find("tail").unwrap()));
        assert_eq!(structure.brackets.len(), 6);
        assert!(structure.brackets.iter().all(|(_, _, pair)| pair.is_some()));
        let html = format!(
            "<script>{source}</script><style>a {{ content: \"}}\"; }}</style><script>({{x:1}})</script>"
        );
        syntax = SyntaxDocument::new(Language::Html).unwrap();
        syntax.update(&html, || true);
        let structure = syntax.structure().unwrap();
        assert_eq!(
            structure.language_at(html.find("call").unwrap()),
            Language::JavaScript
        );
        assert_eq!(
            structure.language_at(html.find("content").unwrap()),
            Language::Css
        );
        assert_eq!(structure.language_at(0), Language::Html);
        assert!(!structure.is_code(html.find("\"}\"").unwrap() + 1));
        assert_eq!(structure.brackets.len(), 12);
        assert!(structure.brackets.iter().all(|(_, _, pair)| pair.is_some()));
        syntax.update(&html, || false);
        assert!(syntax.structure().is_none());
    }

    #[test]
    fn prepared_commands_use_each_cursor_language_and_reject_stale_contexts_atomically() {
        use super::super::{Document, EditError, Indentation, Selection};
        let source = "<script>const s = `text ${call()} tail`;</script><style>a { color: ; }</style><p>plain</p>";
        let mut syntax = SyntaxDocument::new(Language::Html).unwrap();
        syntax.update(source, || true);
        let context = syntax.structure().unwrap();
        let mut document = Document::new(source);
        document
            .set_selections(vec![
                Selection::caret(source.find("call()").unwrap() + "call(".len()),
                Selection::caret(source.find("; }").unwrap()),
                Selection::caret(source.find("plain").unwrap()),
            ])
            .unwrap();
        let before = document.clone();
        document.type_character_with_context('(', &context).unwrap();
        assert_eq!(
            document.text(),
            "<script>const s = `text ${call(())} tail`;</script><style>a { color: (); }</style><p>(plain</p>"
        );
        let changed = document.clone();
        assert_eq!(
            document.newline_with_context(Indentation::default(), None, &context),
            Err(EditError::StaleContext)
        );
        assert_eq!(document, changed);
        assert_eq!(
            document.delete_pairs_with_context(&context),
            Err(EditError::StaleContext)
        );
        assert_eq!(document, changed);
        assert!(document.undo());
        assert_eq!(document.text(), before.text());
        assert_eq!(document.selections(), before.selections());
        for source in ["`text $`", "`text $"] {
            let mut document = Document::new(source);
            document
                .set_selections(vec![Selection::caret(source.find('$').unwrap() + 1)])
                .unwrap();
            document.type_character('{', Language::JavaScript).unwrap();
            assert_eq!(document.text(), source.replace('$', "${}"));
        }
        let transition = "<script>const s = `text $`;</script>";
        syntax.update(transition, || true);
        let context = syntax.structure().unwrap();
        let mut document = Document::new(transition);
        document
            .set_selections(vec![Selection::caret(transition.find('$').unwrap() + 1)])
            .unwrap();
        document.type_character_with_context('{', &context).unwrap();
        assert_eq!(document.text(), transition.replace('$', "${}"));
        let escaped = "`text \\ $`".replace("\\ ", "\\");
        let mut document = Document::new(&escaped);
        document
            .set_selections(vec![Selection::caret(escaped.find('$').unwrap() + 1)])
            .unwrap();
        document.type_character('{', Language::JavaScript).unwrap();
        assert_eq!(document.text(), escaped.replace('$', "${"));
        let comment = "<script>// note</script>";
        syntax.update(comment, || true);
        let context = syntax.structure().unwrap();
        let mut comment_document = Document::new(comment);
        comment_document
            .set_selections(vec![Selection::caret(comment.find("</script>").unwrap())])
            .unwrap();
        comment_document
            .type_character_with_context('(', &context)
            .unwrap();
        assert_eq!(comment_document.text(), "<script>// note(</script>");
        for source in ["// note", "// note\nnext"] {
            let mut document = Document::new(source);
            document
                .set_selections(vec![Selection::caret("// note".len())])
                .unwrap();
            document.type_character('(', Language::JavaScript).unwrap();
            assert_eq!(document.text(), source.replace("// note", "// note("));
        }
        let empty = "<script></script><style></style>";
        syntax.update(empty, || true);
        let context = syntax.structure().unwrap();
        let mut empty_document = Document::new(empty);
        empty_document
            .set_selections(vec![
                Selection::caret(empty.find("</script>").unwrap()),
                Selection::caret(empty.find("</style>").unwrap()),
            ])
            .unwrap();
        empty_document
            .type_character_with_context('(', &context)
            .unwrap();
        assert_eq!(
            empty_document.text(),
            "<script>()</script><style>()</style>"
        );
        let blocks = "<script>\nfunction run() {}\n</script>\n<style>\na {}\n</style>";
        syntax.update(blocks, || true);
        let context = syntax.structure().unwrap();
        let mut document = Document::new(blocks);
        document
            .set_selections(vec![
                Selection::caret(blocks.find("{}").unwrap() + 1),
                Selection::caret(blocks.rfind("{}").unwrap() + 1),
            ])
            .unwrap();
        document
            .newline_with_context(Indentation::default(), None, &context)
            .unwrap();
        assert_eq!(
            document.text(),
            "<script>\nfunction run() {\n    \n}\n</script>\n<style>\na {\n    \n}\n</style>"
        );
        let nested = "`a ${`b ${call({key: '} '})} c`} d`";
        let fallback = Structure::new(nested, Language::JavaScript);
        assert!(fallback.is_code(nested.find("call").unwrap()));
        assert!(!fallback.is_code(nested.find(" c").unwrap()));
        assert!(!fallback.is_code(nested.find(" d").unwrap()));
        assert!(fallback.brackets.iter().all(|(_, _, pair)| pair.is_some()));
        let escaped = "`a \\${notCode} tail`";
        assert!(
            !Structure::new(escaped, Language::JavaScript)
                .is_code(escaped.find("notCode").unwrap())
        );
    }

    #[test]
    fn reindent_uses_embedded_bodies_without_leaking_unclosed_blocks() {
        use super::super::{Document, EditError, Indentation, Selection};
        let source = "<script>\r\nfunction first() {\r\nx();\r\n</script>\r\n<style>\r\na {\r\ncolor: red;\r\n}\r\n</style>\r\n<script>\r\nfunction second() {\r\ny();\r\n}\r\n</script>";
        let mut parser = SyntaxDocument::new(Language::Html).unwrap();
        parser.update(source, || true);
        let context = parser.structure().unwrap();
        let mut document = Document::new(source);
        document
            .set_selections(vec![Selection {
                anchor: source.len(),
                head: 0,
            }])
            .unwrap();
        document
            .reindent_with_context(Indentation::default(), &context)
            .unwrap();
        assert_eq!(
            document.text(),
            "<script>\r\nfunction first() {\r\n    x();\r\n</script>\r\n<style>\r\na {\r\n    color: red;\r\n}\r\n</style>\r\n<script>\r\nfunction second() {\r\n    y();\r\n}\r\n</script>"
        );
        let edited = document.clone();
        assert_eq!(
            document.reindent_with_context(Indentation::default(), &context),
            Err(EditError::StaleContext)
        );
        assert_eq!(document, edited);
        assert!(document.undo());
        assert_eq!(document.text(), source);
    }

    #[test]
    fn reindent_preserves_parser_classified_multiline_literals() {
        use super::super::{Document, Indentation, Selection};
        let source = "class C {\nvoid F() {\nvar text = @\"first\n  unchanged literal\nlast\";\nCall();\n}\n}";
        let mut parser = SyntaxDocument::new(Language::CSharp).unwrap();
        parser.update(source, || true);
        let context = parser.structure().unwrap();
        let mut document = Document::new(source);
        document
            .set_selections(vec![Selection {
                anchor: 0,
                head: source.len(),
            }])
            .unwrap();
        document
            .reindent_with_context(Indentation::default(), &context)
            .unwrap();
        assert_eq!(
            document.text(),
            "class C {\n    void F() {\n        var text = @\"first\n  unchanged literal\nlast\";\n        Call();\n    }\n}"
        );
    }

    #[test]
    fn reindent_does_not_change_literal_whitespace_before_interpolation_code() {
        use super::super::{Document, Indentation, Selection};
        let source = "class C {\nvoid F() {\nvar text = $@\"first\n  {Call()}\nlast\";\n}\n}";
        let mut parser = SyntaxDocument::new(Language::CSharp).unwrap();
        parser.update(source, || true);
        let context = parser.structure().unwrap();
        let mut document = Document::new(source);
        document
            .set_selections(vec![Selection {
                anchor: 0,
                head: source.len(),
            }])
            .unwrap();
        document
            .reindent_with_context(Indentation::default(), &context)
            .unwrap();
        assert_eq!(
            document.text(),
            "class C {\n    void F() {\n        var text = $@\"first\n  {Call()}\nlast\";\n    }\n}"
        );
    }

    #[test]
    fn reindent_preserves_python_fstring_whitespace_around_code_holes() {
        use super::super::{Document, Indentation, Selection};
        let source = "values = [\nf\"\"\"first\n  {call()}\nlast\"\"\"\n]";
        let mut parser = SyntaxDocument::new(Language::Python).unwrap();
        parser.update(source, || true);
        let context = parser.structure().unwrap();
        let mut document = Document::new(source);
        document
            .set_selections(vec![Selection {
                anchor: 0,
                head: source.len(),
            }])
            .unwrap();
        document
            .reindent_with_context(Indentation::default(), &context)
            .unwrap();
        assert_eq!(
            document.text(),
            "values = [\n    f\"\"\"first\n  {call()}\nlast\"\"\"\n]"
        );
    }

    #[test]
    fn block_comments_use_each_embedded_language_and_keep_markup_outside_caret_edits() {
        use super::super::{Document, EditError, Selection};
        let source = "<script>call();</script><style>a { color: red; }</style><p>文</p>";
        let mut parser = SyntaxDocument::new(Language::Html).unwrap();
        parser.update(source, || true);
        let context = parser.structure().unwrap();
        let mut document = Document::new(source);
        document
            .set_selections(vec![
                Selection::caret(source.find("call").unwrap()),
                Selection {
                    anchor: source.find("red").unwrap(),
                    head: source.find("red").unwrap() + 3,
                },
            ])
            .unwrap();
        document
            .toggle_block_comments_with_context(&context)
            .unwrap();
        assert_eq!(
            document.text(),
            "<script>/* call(); */</script><style>a { color: /* red */; }</style><p>文</p>"
        );
        let edited = document.clone();
        assert_eq!(
            document.toggle_block_comments_with_context(&context),
            Err(EditError::StaleContext)
        );
        assert_eq!(document, edited);
        parser.update(document.text(), || true);
        document
            .toggle_block_comments_with_context(&parser.structure().unwrap())
            .unwrap();
        assert_eq!(document.text(), source);
        assert!(document.undo());
        assert_eq!(document.text(), edited.text());
    }

    #[test]
    fn embedded_block_comment_selection_cannot_escape_its_language_body() {
        use super::super::{Document, Selection};
        let source = "<script>call();</script><style>a {}</style>";
        let mut parser = SyntaxDocument::new(Language::Html).unwrap();
        parser.update(source, || true);
        let context = parser.structure().unwrap();
        let mut document = Document::new(source);
        document
            .set_selections(vec![Selection {
                anchor: source.find("call").unwrap(),
                head: source.len(),
            }])
            .unwrap();
        let before = document.clone();
        assert!(
            !document
                .toggle_block_comments_with_context(&context)
                .unwrap()
        );
        assert_eq!(document, before);
    }

    #[test]
    fn line_comments_mix_embedded_syntax_in_one_transaction_and_reject_stale_contexts() {
        use super::super::{Document, EditError, Selection};
        let source = "<script>call();</script><style>a { color: red; }</style>";
        let mut parser = SyntaxDocument::new(Language::Html).unwrap();
        parser.update(source, || true);
        let mut document = Document::new(source);
        document
            .set_selections(vec![
                Selection::caret(source.find("call").unwrap()),
                Selection {
                    anchor: source.find("red").unwrap() + 3,
                    head: source.find("red").unwrap(),
                },
            ])
            .unwrap();
        let context = parser.structure().unwrap();
        document
            .toggle_line_comments_with_context(&context)
            .unwrap();
        let expected = "<script>// call();</script><style>a { color: /* red */; }</style>";
        assert_eq!(document.text(), expected);
        let css = document.selections()[1];
        assert!(css.anchor > css.head);
        assert_eq!(&document.text()[css.range()], "red");
        let changed = document.clone();
        assert_eq!(
            document.toggle_line_comments_with_context(&context),
            Err(EditError::StaleContext)
        );
        assert_eq!(document, changed);
        parser.update(document.text(), || true);
        document
            .toggle_line_comments_with_context(&parser.structure().unwrap())
            .unwrap();
        assert_eq!(document.text(), source);
        assert!(document.undo());
        assert_eq!(document.text(), expected);
        assert!(document.undo());
        assert_eq!(document.text(), source);
    }

    #[test]
    fn line_comments_deduplicate_same_body_rows_and_block_carets() {
        use super::super::{Document, Selection};
        let source = "<script>call();</script><style>a { color: red; }</style>";
        let mut parser = SyntaxDocument::new(Language::Html).unwrap();
        parser.update(source, || true);
        let mut document = Document::new(source);
        document
            .set_selections(vec![
                Selection::caret(source.find("call").unwrap()),
                Selection::caret(source.find("call").unwrap() + 3),
                Selection::caret(source.find("color").unwrap()),
                Selection::caret(source.find("red").unwrap()),
            ])
            .unwrap();
        document
            .toggle_line_comments_with_context(&parser.structure().unwrap())
            .unwrap();
        assert_eq!(
            document.text(),
            "<script>// call();</script><style>/* a { color: red; } */</style>"
        );
        assert!(document.undo());
        assert_eq!(document.text(), source);
        assert_eq!(document.selections().len(), 4);
    }

    #[test]
    fn line_comments_reject_escaping_selections_without_applying_other_cursors() {
        use super::super::{Document, Selection};
        let source = "<script>call();</script><style>a { color: red; }</style>";
        let mut parser = SyntaxDocument::new(Language::Html).unwrap();
        parser.update(source, || true);
        let mut document = Document::new(source);
        document
            .set_selections(vec![
                Selection::caret(source.find("call").unwrap()),
                Selection {
                    anchor: source.find("color").unwrap(),
                    head: source.len(),
                },
            ])
            .unwrap();
        let before = document.clone();
        assert!(
            !document
                .toggle_line_comments_with_context(&parser.structure().unwrap())
                .unwrap()
        );
        assert_eq!(document, before);
    }

    #[test]
    fn parsed_navigation_matches_interpolation_code_and_keeps_embedded_bodies_separate() {
        use super::super::matching_bracket_with_context;
        let source = "<script>const t = `literal { ${call(foo)} tail }`;</script><style>a { color: red; }</style>";
        let mut parser = SyntaxDocument::new(Language::Html).unwrap();
        parser.update(source, || true);
        let context = parser.structure().unwrap();
        let open = source.find("(foo)").unwrap();
        assert_eq!(
            matching_bracket_with_context(source, &context, open),
            Some((open, open + 4))
        );
        assert_eq!(
            matching_bracket_with_context(source, &context, open + 1),
            Some((open, open + 4))
        );
        assert!(
            matching_bracket_with_context(source, &context, source.find("literal {").unwrap() + 8)
                .is_none()
        );
        let css = source.find("{ color").unwrap();
        assert_eq!(
            matching_bracket_with_context(source, &context, css),
            Some((css, source.rfind('}').unwrap()))
        );
        assert!(matching_bracket_with_context(&format!("{source}x"), &context, open).is_none());
        for offset in [source.len() + 1, usize::MAX] {
            assert!(matching_bracket_with_context(source, &context, offset).is_none());
        }
        let incomplete = "<script>{</script><style>}</style><script>}</script>";
        parser.update(incomplete, || true);
        let context = parser.structure().unwrap();
        assert!(
            matching_bracket_with_context(incomplete, &context, incomplete.find('{').unwrap())
                .is_none()
        );
    }

    #[test]
    fn parsed_selection_expands_into_python_blocks_and_shrinks_without_changing_text() {
        use super::super::{Document, EditError, Indentation, Selection, SelectionCommand};
        let source = "def f():\r\n    value = call(foo)\r\n    return value\r\noutside()";
        let mut parser = SyntaxDocument::new(Language::Python).unwrap();
        parser.update(source, || true);
        let context = parser.structure().unwrap();
        let mut document = Document::new(source);
        let start = source.find("foo").unwrap();
        document
            .set_selections(vec![Selection {
                anchor: start + 2,
                head: start + 1,
            }])
            .unwrap();
        for expected in [
            "foo",
            "(foo)",
            "call(foo)",
            "value = call(foo)",
            "value = call(foo)\r\n    return value",
            "def f():\r\n    value = call(foo)\r\n    return value",
        ] {
            document
                .selection_command_with_context(
                    SelectionCommand::Expand,
                    Indentation::default(),
                    &context,
                )
                .unwrap();
            assert_eq!(&document.text()[document.selections()[0].range()], expected);
            assert!(document.selections()[0].anchor > document.selections()[0].head);
        }
        document
            .selection_command_with_context(
                SelectionCommand::Shrink,
                Indentation::default(),
                &context,
            )
            .unwrap();
        assert_eq!(
            &document.text()[document.selections()[0].range()],
            "value = call(foo)\r\n    return value"
        );
        let other = Structure::new("changed", Language::Python);
        let before = document.clone();
        assert!(matches!(
            document.selection_command_with_context(
                SelectionCommand::Shrink,
                Indentation::default(),
                &other
            ),
            Err(super::super::SelectionError::Edit(EditError::StaleContext))
        ));
        assert_eq!(document, before);
        assert_eq!(document.text(), source);
    }

    #[test]
    fn builtin_providers_publish_validated_syntax_selection_ranges() {
        for &(path, source, _) in super::super::syntax_contracts::LANGUAGE_CASES {
            let language = crate::highlight::language_from_path(path);
            let mut parser = SyntaxDocument::new(language).unwrap();
            parser.update(source, || true);
            let context = parser.structure().unwrap();
            assert!(context.selection_ranges().next().is_some(), "{path}");
            assert!(
                context.selection_ranges().all(|range| {
                    range.start < range.end
                        && source.is_char_boundary(range.start)
                        && source.is_char_boundary(range.end)
                }),
                "{path}"
            );
        }
    }

    #[test]
    fn literal_contexts_protect_text_and_expose_executable_interpolation() {
        for &(path, source, literal, code) in super::super::syntax_contracts::LITERAL_CASES {
            let mut syntax =
                SyntaxDocument::new(crate::highlight::language_from_path(path)).unwrap();
            syntax.update(source, || true);
            let structure = syntax.structure().unwrap();
            assert!(
                !structure.is_code(source.find(literal).unwrap()),
                "{path} {source}: {}",
                syntax.tree.as_ref().unwrap().root_node().to_sexp()
            );
            if let Some(code) = code {
                assert!(
                    structure.is_code(source.find(code).unwrap()),
                    "{path} {source}: {}",
                    syntax.tree.as_ref().unwrap().root_node().to_sexp()
                );
            }
            if let Some(inner) = source.find("inner") {
                assert!(!structure.is_code(inner), "nested literal: {path} {source}");
            }
            if path.ends_with("php") && code.is_some() {
                let open = source.find("{$").unwrap();
                let close = source[open..].find('}').unwrap() + open;
                assert_eq!(
                    super::super::matching_bracket_with_context(source, &structure, open),
                    Some((open, close)),
                    "PHP interpolation delimiters: {source}"
                );
            }
            let revised = source.replace("文😀", "😀文 changed").replace('\n', "\r\n");
            assert_eq!(
                syntax.update(&revised, || true),
                SyntaxStatus::Ready { incremental: true }
            );
            let mut fresh =
                SyntaxDocument::new(crate::highlight::language_from_path(path)).unwrap();
            fresh.update(&revised, || true);
            assert_eq!(
                syntax.structure().unwrap().protected,
                fresh.structure().unwrap().protected
            );
            let painted = syntax.highlight_lines().unwrap();
            assert_eq!(
                painted
                    .iter()
                    .map(|line| line
                        .iter()
                        .map(|token| token.text.as_str())
                        .collect::<String>())
                    .collect::<Vec<_>>(),
                revised.split('\n').collect::<Vec<_>>()
            );
            assert!(
                painted
                    .iter()
                    .flatten()
                    .any(|token| token.kind == crate::highlight::TokenKind::String
                        && token.text.contains(literal)),
                "literal paint: {path} {revised}"
            );
        }
    }

    #[test]
    fn parsed_contexts_cover_builtin_literals_and_incomplete_interpolation() {
        for &(path, source, _) in super::super::syntax_contracts::LANGUAGE_CASES {
            let language = crate::highlight::language_from_path(path);
            let mut syntax = SyntaxDocument::new(language).unwrap();
            syntax.update(source, || true);
            let structure = syntax.structure().unwrap();
            if language != Language::Html {
                assert!(
                    !structure.is_code(source.find("文😀").unwrap() + "文".len()),
                    "{path}"
                );
            }
        }
        for (language, source, code) in [
            (Language::JavaScript, "`text ${call(", "call"),
            (Language::Python, "f'hello {call(\"x\")} end'", "call"),
            (
                Language::CSharp,
                "class C { string s = $\"text {Call(1)} tail\"; }",
                "Call",
            ),
        ] {
            let mut syntax = SyntaxDocument::new(language).unwrap();
            syntax.update(source, || true);
            let structure = syntax.structure().unwrap();
            assert!(
                structure.is_code(source.find(code).unwrap()),
                "{language:?}: {structure:?} {}",
                syntax.tree.as_ref().unwrap().root_node().to_sexp()
            );
            assert!(
                !structure.is_code(
                    source
                        .find("text")
                        .or_else(|| source.find("hello"))
                        .unwrap()
                        + 1
                ),
                "{language:?}"
            );
        }
    }

    #[test]
    fn html_mime_defaults_empty_bodies_and_independent_broken_scripts() {
        for (opening, expected) in [
            ("<script>", Some(Language::JavaScript)),
            (
                "<script type=\"TEXT/JAVASCRIPT1.5\">",
                Some(Language::JavaScript),
            ),
            (
                "<script TYPE=\"text&#47;javascript\">",
                Some(Language::JavaScript),
            ),
            (
                "<script type=module type=application/json>",
                Some(Language::JavaScript),
            ),
            ("<script type=application/json type=module>", None),
            ("<script type=\"text/javascript; charset=utf-8\">", None),
            ("<script type=importmap>", None),
            (
                "<script type=' text/javascript &#10;'>",
                Some(Language::JavaScript),
            ),
            ("<script type='&#160;text/javascript'>", None),
            ("<script type=' module '>", None),
            (
                "<script language=JavaScript1.5>",
                Some(Language::JavaScript),
            ),
            ("<script language=python>", None),
            ("<script language=' javascript '>", None),
            (
                "<script type='' language=python>",
                Some(Language::JavaScript),
            ),
            ("<style type='text/css'>", Some(Language::Css)),
            ("<style type=text/less>", None),
        ] {
            let closing = if opening.starts_with("<style") {
                "</style>"
            } else {
                "</script>"
            };
            let source = format!("{opening}{closing}");
            let mut syntax = SyntaxDocument::new(Language::Html).unwrap();
            assert_eq!(
                syntax.update(&source, || true),
                SyntaxStatus::Ready { incremental: false }
            );
            assert_eq!(
                syntax.embedded.first().map(|body| body.provider.language),
                expected,
                "{opening}"
            );
            assert_eq!(
                syntax.language_at(opening.len()),
                expected.unwrap_or(Language::Html)
            );
        }
        let source = "<script>\nfunction broken() {\n</script>\n<script>\nfunction valid() {\n  return 1;\n}\n</script>";
        let mut syntax = SyntaxDocument::new(Language::Html).unwrap();
        syntax.update(source, || true);
        assert_eq!(syntax.embedded.len(), 2);
        assert!(
            syntax.embedded[0]
                .tree
                .as_ref()
                .unwrap()
                .root_node()
                .has_error()
        );
        assert!(
            !syntax.embedded[1]
                .tree
                .as_ref()
                .unwrap()
                .root_node()
                .has_error()
        );
        assert!(syntax.folds().contains(&FoldRange {
            start_line: 4,
            end_line: 6
        }));
    }

    #[test]
    fn injection_limits_invalid_ranges_and_mid_parse_cancellation_discard_all_trees() {
        let small = "<script>let value = 1;</script>";
        let mut syntax = SyntaxDocument::new(Language::Html).unwrap();
        syntax.update(small, || true);
        let oversized = small.repeat(MAX_INJECTIONS + 1);
        assert_eq!(syntax.update(&oversized, || true), SyntaxStatus::TooLarge);
        assert!(syntax.tree.is_none());
        assert!(syntax.embedded.is_empty());
        let mut checks = 0;
        assert_eq!(
            syntax.update(small, || {
                checks += 1;
                checks < 5
            }),
            SyntaxStatus::Cancelled
        );
        assert!(syntax.tree.is_none());
        assert!(syntax.embedded.is_empty());
        assert_eq!(
            syntax.update(small, || true),
            SyntaxStatus::Ready { incremental: false }
        );
        let mut invalid = syntax_provider(Language::Html).unwrap();
        invalid.injection = Some(|_, _| {
            Some((
                Language::JavaScript,
                Range {
                    start_byte: 0,
                    end_byte: usize::MAX,
                    start_point: Point::new(0, 0),
                    end_point: Point::new(0, 0),
                },
            ))
        });
        let mut syntax = SyntaxDocument::with_provider(Language::Html, Some(invalid)).unwrap();
        assert_eq!(syntax.update(small, || true), SyntaxStatus::Cancelled);
        assert!(syntax.tree.is_none());
    }

    #[test]
    fn provider_fold_nodes_exist_in_the_released_grammars() {
        for provider in super::super::SYNTAX_PROVIDERS {
            let grammar = (provider.grammar)();
            for kind in provider.fold_nodes.iter().chain(provider.parent_headers) {
                assert_ne!(
                    grammar.id_for_node_kind(kind, true),
                    0,
                    "{:?}: {kind}",
                    provider.language
                );
            }
        }
    }

    fn positions(tree: &Tree) -> Vec<String> {
        let mut cursor = tree.walk();
        let mut positions = Vec::new();
        loop {
            let node = cursor.node();
            positions.push(format!(
                "{}:{}..{}:{:?}..{:?}:{}:{}",
                node.kind(),
                node.start_byte(),
                node.end_byte(),
                node.start_position(),
                node.end_position(),
                node.is_missing(),
                node.is_error()
            ));
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return positions;
                }
            }
        }
    }

    #[test]
    fn built_in_grammars_incremental_folds_and_recovery_share_one_contract() {
        let oversized = "x".repeat(MAX_STRUCTURE_BYTES + 1);
        for &(path, source, expected) in super::super::syntax_contracts::LANGUAGE_CASES {
            let language = crate::highlight::language_from_path(path);
            let mut syntax = SyntaxDocument::new(language).unwrap();
            for (index, source) in [
                source.to_string(),
                source.replace("文😀", "😀文 changed").replace('\n', "\r\n"),
            ]
            .into_iter()
            .enumerate()
            {
                assert_eq!(
                    syntax.update(&source, || true),
                    SyntaxStatus::Ready {
                        incremental: index != 0
                    },
                    "{path}"
                );
                let tree = syntax.tree.as_ref().unwrap();
                assert!(
                    !tree.root_node().has_error(),
                    "{path}: {}",
                    tree.root_node().to_sexp()
                );
                assert!(
                    syntax.folds().contains(&expected),
                    "{path}: {:?}",
                    syntax.folds()
                );
                let mut fresh = SyntaxDocument::new(language).unwrap();
                fresh.update(&source, || true);
                assert_eq!(
                    positions(tree),
                    positions(fresh.tree.as_ref().unwrap()),
                    "{path}"
                );
                assert_eq!(syntax.folds(), fresh.folds(), "{path}");
            }
            assert_eq!(syntax.update(source, || false), SyntaxStatus::Cancelled);
            assert!(syntax.tree.is_none());
            assert!(syntax.folds().is_empty());
            assert_eq!(syntax.update(&oversized, || true), SyntaxStatus::TooLarge);
            assert!(syntax.tree.is_none());
            assert_eq!(
                syntax.update(source, || true),
                SyntaxStatus::Ready { incremental: false }
            );
        }
    }

    #[test]
    fn custom_provider_uses_shared_parse_and_fold_policy() {
        let provider = SyntaxProvider {
            language: Language::Rust,
            context: None,
            context_scope: super::super::syntax_providers::SyntaxContextScope::Document,
            highlight: None,
            highlight_scope: super::super::SyntaxHighlightScope::Document,
            injection: None,
            grammar: || tree_sitter_rust::LANGUAGE.into(),
            fold_nodes: &["arguments"],
            parent_headers: &[],
        };
        assert!(SyntaxDocument::with_provider(Language::Python, Some(provider)).is_none());
        let mut syntax = SyntaxDocument::with_provider(Language::Rust, Some(provider)).unwrap();
        syntax.update(
            "fn main() {\n    call(\n        1,\n        2\n    );\n}\n",
            || true,
        );
        assert_eq!(
            syntax.folds(),
            vec![FoldRange {
                start_line: 1,
                end_line: 3
            }]
        );
    }

    #[test]
    fn incremental_unicode_crlf_edits_match_fresh_trees_and_fold_ranges() {
        let mut syntax = SyntaxDocument::new(Language::Rust).unwrap();
        let sources = [
            "// 😀\r\nfn main() {\r\n    let x = r###\" } /* \"###;\r\n    /* outer\r\n       /* nested */\r\n    */\r\n}\r\n",
            "// 😀\r\nfn main() {\r\n    if true {\r\n        let x = r###\" } /* \"###;\r\n    }\r\n}\r\n",
            "fn main() {\n    let text = \"文😀\";\n}\n",
            "",
        ];
        for (index, source) in sources.into_iter().enumerate() {
            assert_eq!(
                syntax.update(source, || true),
                SyntaxStatus::Ready {
                    incremental: index != 0
                }
            );
            let mut fresh = SyntaxDocument::new(Language::Rust).unwrap();
            fresh.update(source, || true);
            assert_eq!(
                syntax.tree.as_ref().unwrap().root_node().to_sexp(),
                fresh.tree.as_ref().unwrap().root_node().to_sexp()
            );
            assert_eq!(
                syntax.tree.as_ref().unwrap().root_node().end_byte(),
                source.len()
            );
            assert_eq!(
                positions(syntax.tree.as_ref().unwrap()),
                positions(fresh.tree.as_ref().unwrap())
            );
            assert_eq!(syntax.folds(), fresh.folds());
            assert!(!syntax.tree.as_ref().unwrap().root_node().has_error());
        }
    }

    #[test]
    fn syntax_folds_ignore_braces_in_literals_and_include_nested_comments() {
        let source = "fn main() {\n    let text = r###\" { } \"###;\n    /* outer\n       /* nested */\n    */\n}\n";
        let mut syntax = SyntaxDocument::new(Language::Rust).unwrap();
        syntax.update(source, || true);
        assert_eq!(
            syntax.folds(),
            vec![
                FoldRange {
                    start_line: 0,
                    end_line: 5
                },
                FoldRange {
                    start_line: 2,
                    end_line: 4
                }
            ]
        );
        assert!(SyntaxDocument::new(Language::Python).is_some());
    }

    #[test]
    fn shared_closing_and_opening_rows_keep_both_fold_headers_visible() {
        let mut syntax = SyntaxDocument::new(Language::Rust).unwrap();
        syntax.update(
            "fn main() {\n    if true {\n        a();\n    } else {\n        b();\n    }\n}\n",
            || true,
        );
        assert_eq!(
            syntax.folds(),
            vec![
                FoldRange {
                    start_line: 0,
                    end_line: 6
                },
                FoldRange {
                    start_line: 1,
                    end_line: 2
                },
                FoldRange {
                    start_line: 3,
                    end_line: 5
                },
            ]
        );
    }

    #[test]
    fn cancelled_and_oversize_parses_drop_stale_trees_and_can_recover() {
        let mut syntax = SyntaxDocument::new(Language::Rust).unwrap();
        syntax.update("fn main() {\n}\n", || true);
        assert!(!syntax.folds().is_empty());
        assert_eq!(
            syntax.update("fn other() {\n}\n", || false),
            SyntaxStatus::Cancelled
        );
        assert!(syntax.folds().is_empty());
        assert_eq!(
            syntax.update(&"x".repeat(MAX_STRUCTURE_BYTES + 1), || true),
            SyntaxStatus::TooLarge
        );
        assert_eq!(
            syntax.update("fn main() {\n}\n", || true),
            SyntaxStatus::Ready { incremental: false }
        );
        // Cancel within parser progress, not just before entering the runtime.
        let mut calls = 0;
        assert_eq!(
            syntax.update(&"fn main() {}\n".repeat(10_000), || {
                calls += 1;
                calls < 2
            }),
            SyntaxStatus::Cancelled
        );
        assert!(calls >= 2);
        assert!(syntax.folds().is_empty());
        assert_eq!(
            syntax.update("fn recovered() {}", || true),
            SyntaxStatus::Ready { incremental: false }
        );
    }
}
