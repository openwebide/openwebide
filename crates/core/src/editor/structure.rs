//! Lexical structure for editing commands. Strings and comments are opaque;
//! unsupported languages retain plain-text editing rather than guessing syntax.
use std::ops::Range;
use std::sync::Arc;
#[cfg(feature = "editor-parser")]
pub(super) mod fallback;
#[cfg(feature = "editor-parser")]
pub(super) mod ordering;
#[cfg(feature = "editor-parser")]
mod publication;
#[cfg(feature = "editor-parser")]
mod reception;
#[cfg(feature = "editor-parser")]
mod reconciliation;
#[cfg(feature = "editor-parser")]
pub(super) use reception::StructureReception;
#[cfg(feature = "editor-parser")]
mod framing;
pub(super) mod scanning;
#[cfg(feature = "editor-parser")]
mod serialization;
#[cfg(feature = "editor-parser")]
pub(super) use framing::{StructureFrame, StructureFrameDecoding, StructureFrames};
#[cfg(feature = "editor-parser")]
pub(super) use publication::StructurePublicationPreparation;
#[cfg(feature = "editor-parser")]
pub(super) use serialization::StructureSerialization;

use crate::highlight::Language;

/// Bound structural commands and source snapshots independently of transport.
pub const MAX_STRUCTURE_BYTES: usize = 2 * 1024 * 1024;
const MAX_BRACKETS: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) enum RegionKind {
    String,
    Template,
    #[cfg(feature = "editor-parser")]
    Text,
    LineComment,
    BlockComment,
    Regex,
}

impl RegionKind {
    fn is_literal(self) -> bool {
        match self {
            Self::String | Self::Template | Self::Regex => true,
            #[cfg(feature = "editor-parser")]
            Self::Text => true,
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Structure {
    source: Arc<String>,
    language: Language,
    pub(super) scopes: Vec<(Range<usize>, Language)>,
    selection_ranges: Vec<Range<usize>>,
    pub(super) opaque_starts: Vec<usize>,
    available: bool,
    pub(super) protected: Vec<(Range<usize>, bool, RegionKind)>,
    pub brackets: Vec<(usize, char, Option<usize>)>,
}

#[derive(Default)]
pub(super) struct LexicalStructure {
    pub opaque_starts: Vec<usize>,
    pub protected: Vec<(Range<usize>, bool, RegionKind)>,
    pub brackets: Vec<(usize, char, Option<usize>)>,
}

/// Source-independent region queries shared by retained and borrowed analysis.
#[derive(Clone, Copy)]
pub(super) struct Regions<'a> {
    protected: &'a [(Range<usize>, bool, RegionKind)],
    opaque_starts: &'a [usize],
}
impl<'a> Regions<'a> {
    pub(super) fn literals(self) -> impl Iterator<Item = &'a Range<usize>> {
        self.protected.iter().filter_map(|(range, _, kind)| {
            matches!(
                kind,
                RegionKind::String | RegionKind::Template | RegionKind::BlockComment
            )
            .then_some(range)
        })
    }

    pub(super) fn is_literal(self, position: usize) -> bool {
        self.region_at(position)
            .is_some_and(|(range, _, kind)| kind.is_literal() && range.contains(&position))
    }

    pub(super) fn is_opaque_body(self, position: usize) -> bool {
        self.region_at(position).is_some_and(|(range, _, _)| {
            (range.start < position || self.opaque_starts.binary_search(&position).is_ok())
                && range.contains(&position)
        })
    }

    pub(super) fn is_line_comment(self, position: usize) -> bool {
        self.region_at(position).is_some_and(|(range, _, kind)| {
            *kind == RegionKind::LineComment && range.contains(&position)
        })
    }

    pub(super) fn is_comment(self, position: usize) -> bool {
        self.region_at(position).is_some_and(|(range, _, kind)| {
            matches!(kind, RegionKind::LineComment | RegionKind::BlockComment)
                && range.contains(&position)
        })
    }

    fn region_at(self, position: usize) -> Option<&'a (Range<usize>, bool, RegionKind)> {
        let end = self
            .protected
            .partition_point(|(range, _, _)| range.start <= position);
        end.checked_sub(1)
            .and_then(|index| self.protected.get(index))
    }
}

impl LexicalStructure {
    pub fn regions(&self) -> Regions<'_> {
        Regions {
            protected: &self.protected,
            opaque_starts: &self.opaque_starts,
        }
    }
}

impl Structure {
    pub fn new(text: &str, language: Language) -> Self {
        let Some(lexical) = Self::scan(text, language) else {
            return Self::unavailable();
        };
        Self {
            source: Arc::new(text.to_owned()),
            language,
            scopes: Vec::new(),
            selection_ranges: Vec::new(),
            opaque_starts: lexical.opaque_starts,
            available: true,
            protected: lexical.protected,
            brackets: lexical.brackets,
        }
    }

    /// Scan borrowed source; callers that only need metadata need no source snapshot.
    pub(super) fn scan(text: &str, language: Language) -> Option<LexicalStructure> {
        let mut scan = scanning::LexicalScan::new(language);
        scan.advance(text, usize::MAX)?;
        scan.finish()
    }

    fn unavailable() -> Self {
        Self {
            source: Arc::new(String::new()),
            language: Language::Plain,
            scopes: Vec::new(),
            selection_ranges: Vec::new(),
            opaque_starts: Vec::new(),
            available: false,
            protected: Vec::new(),
            brackets: Vec::new(),
        }
    }

    pub fn matches_source(&self, text: &str) -> bool {
        self.source.as_ref() == text
    }

    pub fn opens_interpolation(&self, position: usize, ch: char) -> bool {
        if ch != '{'
            || !matches!(
                self.language_at(position),
                Language::JavaScript | Language::TypeScript | Language::Jsx | Language::Tsx
            )
        {
            return false;
        }
        let Some(prefix) = self
            .source
            .get(..position)
            .and_then(|prefix| prefix.strip_suffix('$'))
        else {
            return false;
        };
        prefix.chars().rev().take_while(|ch| *ch == '\\').count() % 2 == 0
            && self
                .region_at(position)
                .is_some_and(|(range, closed, kind)| {
                    *kind == RegionKind::Template
                        && range.start < position
                        && (range.contains(&position) || !closed && range.end == position)
                })
    }

    /// Next character within the current language body.
    pub fn next_character(&self, position: usize) -> Option<char> {
        if self.scopes.iter().any(|(range, _)| range.end == position) {
            return None;
        }
        self.source.get(position..)?.chars().next()
    }

    /// Parsed scopes are ordered and nonoverlapping. Inclusive insertion queries
    /// select the first touching scope; bracket ancestry uses exclusive ends.
    fn language_scope_at(&self, position: usize, inclusive_end: bool) -> Option<usize> {
        let index = self.scopes.partition_point(|(range, _)| {
            if inclusive_end {
                range.end < position
            } else {
                range.end <= position
            }
        });
        self.scopes
            .get(index)
            .filter(|(range, _)| range.start <= position)
            .map(|_| index)
    }

    /// The language of an insertion, including the end of an embedded body.
    pub fn language_at(&self, position: usize) -> Language {
        self.language_scope_at(position, true)
            .map_or(self.language, |index| self.scopes[index].1)
    }

    /// Separate embedded bodies cannot share indentation or bracket ancestry,
    /// even when both use the same language.
    pub fn same_language_body(&self, first: usize, second: usize) -> bool {
        self.language_scope_at(first, false) == self.language_scope_at(second, false)
    }

    /// The bounds of the embedded language containing a caret. Root-language
    /// commands retain the document bounds.
    pub fn language_body(&self, position: usize) -> Range<usize> {
        self.language_scope_at(position, true)
            .map_or(0..self.source.len(), |index| self.scopes[index].0.clone())
    }

    #[cfg(feature = "editor-parser")]
    pub(super) fn prepare_parsed(
        text: Arc<String>,
        language: Language,
        protected: Vec<(Range<usize>, bool, RegionKind)>,
        scopes: Vec<(Range<usize>, Language)>,
        opaque_starts: Vec<usize>,
        selection_ranges: Vec<Range<usize>>,
    ) -> StructurePreparation {
        let result = Self {
            source: text,
            language,
            scopes,
            selection_ranges,
            opaque_starts,
            available: true,
            protected,
            brackets: Vec::new(),
        };
        StructurePreparation {
            structure: result,
            fallback: None,
            reconciliation: None,
            metadata: MetadataPreparation::default(),
            linking: BracketLinking::default(),
        }
    }

    #[cfg(all(feature = "editor-parser", test))]
    pub(super) fn parsed(
        text: Arc<String>,
        language: Language,
        protected: Vec<(Range<usize>, bool, RegionKind)>,
        scopes: Vec<(Range<usize>, Language)>,
        opaque_starts: Vec<usize>,
        selection_ranges: Vec<Range<usize>>,
    ) -> Option<Self> {
        let mut work = Self::prepare_parsed(
            text,
            language,
            protected,
            scopes,
            opaque_starts,
            selection_ranges,
        );
        while !work.advance(256)? {}
        work.finish()
    }

    #[cfg(all(feature = "editor-parser", test))]
    fn link_brackets(&mut self, mut examined: impl FnMut()) -> Option<()> {
        let mut linking = BracketLinking::default();
        while !linking.advance(self, 256, &mut examined)? {}
        Some(())
    }

    pub(super) fn selection_ranges(&self) -> impl Iterator<Item = &Range<usize>> {
        self.selection_ranges.iter()
    }

    pub fn available(&self) -> bool {
        self.available
    }

    fn regions(&self) -> Regions<'_> {
        Regions {
            protected: &self.protected,
            opaque_starts: &self.opaque_starts,
        }
    }

    pub(super) fn literals(&self) -> impl Iterator<Item = &Range<usize>> {
        self.regions().literals()
    }
    fn region_at(&self, position: usize) -> Option<&(Range<usize>, bool, RegionKind)> {
        self.regions().region_at(position)
    }

    pub fn is_code(&self, position: usize) -> bool {
        self.available
            && !self.region_at(position).is_some_and(|(range, closed, _)| {
                (position > range.start || self.opaque_starts.binary_search(&position).is_ok())
                    && (position < range.end || (!closed && position == range.end))
            })
    }

    pub fn allows_newline_indent(&self, position: usize) -> bool {
        self.is_code(position)
            || self.protected.iter().any(|(range, _, kind)| {
                *kind == RegionKind::LineComment && position > range.start && position <= range.end
            })
    }

    pub fn quote_closes_at(&self, position: usize) -> bool {
        self.protected.iter().any(|(range, closed, kind)| {
            matches!(kind, RegionKind::String | RegionKind::Template)
                && *closed
                && range.end == position + 1
        })
    }
    pub fn empty_quote_pair(&self, position: usize) -> bool {
        self.protected.iter().any(|(range, closed, _)| {
            *closed && range.start + 1 == position && range.end == position + 1
        })
    }

    pub fn last_code(&self, text: &str, range: Range<usize>) -> Option<(usize, char)> {
        if !self.available {
            return None;
        }
        text[range.clone()]
            .char_indices()
            .rev()
            .find_map(|(offset, ch)| {
                let position = range.start + offset;
                (!ch.is_whitespace()
                    && !self
                        .region_at(position)
                        .is_some_and(|(range, _, _)| range.contains(&position)))
                .then_some((position, ch))
            })
    }
}

#[cfg(feature = "editor-parser")]
#[derive(Default)]
struct MetadataPreparation {
    phase: u8,
    index: usize,
    end: usize,
    write: usize,
    ordering: Option<ordering::MetadataOrder>,
    failed: bool,
}

#[cfg(feature = "editor-parser")]
impl MetadataPreparation {
    fn complete(&self) -> bool {
        self.phase == 8 && !self.failed
    }

    fn next(&mut self) {
        self.phase += 1;
        self.index = 0;
        self.end = 0;
        self.write = 0;
        self.ordering = None;
    }

    fn advance(&mut self, structure: &mut Structure, budget: usize) -> Option<bool> {
        if self.failed {
            return None;
        }
        for _ in 0..budget {
            let valid = match self.phase {
                0 => {
                    if let Some((range, _)) = structure.scopes.get(self.index) {
                        let valid = range.start >= self.end
                            && range.start <= range.end
                            && structure.source.is_char_boundary(range.start)
                            && structure.source.is_char_boundary(range.end);
                        self.end = range.end;
                        self.index += 1;
                        valid
                    } else {
                        self.next();
                        true
                    }
                }
                1 => {
                    let order = self
                        .ordering
                        .get_or_insert_with(ordering::MetadataOrder::new);
                    if order.step(&mut structure.protected, |(range, _, _)| {
                        (range.start, range.end)
                    }) {
                        self.next();
                    }
                    true
                }
                2 => {
                    if let Some((range, _, _)) = structure.protected.get(self.index) {
                        let valid = range.start >= self.end
                            && range.start <= range.end
                            && structure.source.is_char_boundary(range.start)
                            && structure.source.is_char_boundary(range.end);
                        self.end = range.end;
                        self.index += 1;
                        valid
                    } else {
                        self.next();
                        true
                    }
                }
                3 => {
                    if let Some(range) = structure.selection_ranges.get(self.index) {
                        let valid = range.start < range.end
                            && structure.source.is_char_boundary(range.start)
                            && structure.source.is_char_boundary(range.end);
                        self.index += 1;
                        valid
                    } else {
                        self.next();
                        true
                    }
                }
                4 => {
                    let order = self
                        .ordering
                        .get_or_insert_with(ordering::MetadataOrder::new);
                    if order.step(&mut structure.selection_ranges, |range| {
                        (range.start, range.end)
                    }) {
                        self.next();
                    }
                    true
                }
                5 => {
                    if self.index < structure.selection_ranges.len() {
                        if self.write == 0
                            || structure.selection_ranges[self.index]
                                != structure.selection_ranges[self.write - 1]
                        {
                            structure.selection_ranges.swap(self.write, self.index);
                            self.write += 1;
                        }
                        self.index += 1;
                    } else {
                        structure.selection_ranges.truncate(self.write);
                        self.next();
                    }
                    true
                }
                6 => {
                    let order = self
                        .ordering
                        .get_or_insert_with(ordering::MetadataOrder::new);
                    if order.step(&mut structure.opaque_starts, |position| (*position, 0)) {
                        self.next();
                    }
                    true
                }
                7 => {
                    if self.index < structure.opaque_starts.len() {
                        if self.write == 0
                            || structure.opaque_starts[self.index]
                                != structure.opaque_starts[self.write - 1]
                        {
                            structure.opaque_starts.swap(self.write, self.index);
                            self.write += 1;
                        }
                        self.index += 1;
                    } else {
                        structure.opaque_starts.truncate(self.write);
                        self.next();
                    }
                    true
                }
                _ => return Some(true),
            };
            if !valid {
                self.failed = true;
                return None;
            }
        }
        Some(self.complete())
    }
}

#[cfg(feature = "editor-parser")]
pub(super) struct StructurePreparation {
    structure: Structure,
    fallback: Option<fallback::FallbackCollection>,
    reconciliation: Option<reconciliation::ContextReconciliation>,
    metadata: MetadataPreparation,
    linking: BracketLinking,
}
#[cfg(feature = "editor-parser")]
impl StructurePreparation {
    pub fn collect_fallbacks(
        &mut self,
        outer: Arc<fallback::FallbackContexts>,
        embedded: Vec<(usize, Arc<fallback::FallbackContexts>)>,
    ) {
        self.fallback = Some(fallback::FallbackCollection::new(outer, embedded));
    }

    pub fn reconcile(
        &mut self,
        parsed: Vec<(Range<usize>, bool, RegionKind)>,
        holes: Vec<(Range<usize>, Range<usize>)>,
    ) {
        self.reconciliation = Some(reconciliation::ContextReconciliation::new(parsed, holes));
    }

    pub fn advance(&mut self, budget: usize) -> Option<bool> {
        if let Some(fallback) = &mut self.fallback {
            if fallback.advance(&mut self.structure, budget) {
                self.fallback = None;
            }
            return Some(false);
        }
        if let Some(reconciliation) = &mut self.reconciliation {
            if reconciliation.advance(&mut self.structure, budget) {
                self.reconciliation = None;
            }
            return Some(false);
        }
        if !self.metadata.complete() {
            self.metadata.advance(&mut self.structure, budget)?;
            return Some(false);
        }
        self.linking
            .advance(&mut self.structure, budget, &mut || {})
    }
    pub fn finish(self) -> Option<Structure> {
        (self.fallback.is_none()
            && self.reconciliation.is_none()
            && self.metadata.complete()
            && !self.linking.failed
            && self.linking.position == self.structure.source.len())
        .then_some(self.structure)
    }
}
#[cfg(feature = "editor-parser")]
#[derive(Default)]
struct BracketLinking {
    stack: Vec<usize>,
    last_scope: Option<usize>,
    scope_index: usize,
    region_index: usize,
    position: usize,
    failed: bool,
}
#[cfg(feature = "editor-parser")]
impl BracketLinking {
    fn advance(
        &mut self,
        structure: &mut Structure,
        budget: usize,
        examined: &mut impl FnMut(),
    ) -> Option<bool> {
        if self.failed {
            return None;
        }
        let mut steps = 0;
        while self.position < structure.source.len() && steps < budget {
            while structure
                .scopes
                .get(self.scope_index)
                .is_some_and(|(range, _)| range.end <= self.position)
            {
                if steps == budget {
                    return Some(false);
                }
                self.scope_index += 1;
                steps += 1;
            }
            if steps == budget {
                return Some(false);
            }
            let scope = structure
                .scopes
                .get(self.scope_index)
                .filter(|(range, _)| range.contains(&self.position))
                .map(|_| self.scope_index);
            let language = scope.map_or(structure.language, |index| structure.scopes[index].1);
            if scope != self.last_scope {
                self.stack.clear();
                self.last_scope = scope;
            }
            // A skipped region may cross language bodies. Visit every scope
            // boundary so unrelated bodies never share bracket ancestry.
            let boundary = structure.scopes.get(self.scope_index).map_or(
                structure.source.len(),
                |(range, _)| {
                    if scope.is_some() {
                        range.end
                    } else {
                        range.start
                    }
                },
            );
            if !supports_brackets(language) {
                steps += 1;
                self.position = boundary;
                continue;
            }
            while structure
                .protected
                .get(self.region_index)
                .is_some_and(|(range, _, _)| range.end <= self.position)
            {
                if steps == budget {
                    return Some(false);
                }
                self.region_index += 1;
                steps += 1;
            }
            if steps == budget {
                return Some(false);
            }
            steps += 1;
            if let Some((range, _, _)) = structure.protected.get(self.region_index)
                && range.contains(&self.position)
            {
                self.position = range.end.min(boundary);
                continue;
            }
            examined();
            let ch = structure.source[self.position..].chars().next()?;
            if closing(ch).is_some() {
                if structure.brackets.len() == MAX_BRACKETS {
                    self.failed = true;
                    return None;
                }
                self.stack.push(structure.brackets.len());
                structure.brackets.push((self.position, ch, None));
            } else if matches!(ch, ')' | ']' | '}') {
                if structure.brackets.len() == MAX_BRACKETS {
                    self.failed = true;
                    return None;
                }
                let index = structure.brackets.len();
                structure.brackets.push((self.position, ch, None));
                if let Some(open) = self.stack.last().copied()
                    && closing(structure.brackets[open].1) == Some(ch)
                {
                    self.stack.pop();
                    structure.brackets[open].2 = Some(self.position);
                    structure.brackets[index].2 = Some(structure.brackets[open].0);
                }
            }
            self.position += ch.len_utf8();
        }
        Some(self.position == structure.source.len())
    }
}

fn regex_position(before: &str) -> bool {
    let before = before.trim_end();
    before.is_empty()
        || before.chars().last().is_some_and(|ch| {
            matches!(
                ch,
                '=' | '(' | '[' | '{' | ',' | ':' | ';' | '!' | '?' | '&' | '|'
            )
        })
        || ["return", "throw", "case", "yield", "await"]
            .iter()
            .any(|word| {
                before.strip_suffix(word).is_some_and(|prefix| {
                    prefix
                        .chars()
                        .next_back()
                        .is_none_or(|ch| !ch.is_alphanumeric() && ch != '_')
                })
            })
}

pub fn supports_quote(language: Language, ch: char) -> bool {
    match language {
        Language::Plain | Language::Ini | Language::Markdown | Language::MarkdownInline => false,
        Language::Json => ch == '"',
        Language::JavaScript
        | Language::TypeScript
        | Language::Jsx
        | Language::Tsx
        | Language::Go
        | Language::Shell => {
            matches!(ch, '\'' | '"' | '`')
        }
        _ => matches!(ch, '\'' | '"'),
    }
}

pub fn supports_brackets(language: Language) -> bool {
    matches!(
        language,
        Language::Rust
            | Language::Python
            | Language::JavaScript
            | Language::TypeScript
            | Language::Jsx
            | Language::Tsx
            | Language::Java
            | Language::CSharp
            | Language::Php
            | Language::Json
            | Language::C
            | Language::Cpp
            | Language::Go
            | Language::Css
    )
}

pub fn closing(ch: char) -> Option<char> {
    match ch {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indexed_language_queries_match_linear_touching_and_empty_scope_boundaries() {
        for count in [0, 1, 32, 10_000] {
            let mut structure = Structure::new(&"x".repeat(count * 8 + 4), Language::Rust);
            for index in 0..count {
                let start = index * 8;
                structure.scopes.extend([
                    (start..start + 3, Language::JavaScript),
                    (start + 3..start + 3, Language::Css),
                    (start + 3..start + 8, Language::Markdown),
                ]);
            }
            let positions: Vec<_> = if count < 100 {
                (0..=count * 8 + 4).chain([usize::MAX]).collect()
            } else {
                (0..512)
                    .flat_map(|sample| {
                        let start = (sample * 619 % count) * 8;
                        [start, start + 1, start + 3, start + 4, start + 7, start + 8]
                    })
                    .chain([usize::MAX, count * 8 + 4])
                    .collect()
            };
            for position in positions {
                let old = structure
                    .scopes
                    .iter()
                    .find(|(range, _)| range.start <= position && position <= range.end);
                assert_eq!(
                    structure.language_at(position),
                    old.map_or(structure.language, |(_, language)| *language)
                );
                assert_eq!(
                    structure.language_body(position),
                    old.map_or(0..structure.source.len(), |(range, _)| range.clone())
                );
                let other = position.wrapping_mul(17) % structure.source.len();
                let old_body = |point| {
                    structure
                        .scopes
                        .iter()
                        .position(|(range, _)| range.contains(&point))
                };
                assert_eq!(
                    structure.same_language_body(position, other),
                    old_body(position) == old_body(other)
                );
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    #[ignore = "native release measurement, not a timing assertion"]
    fn measure_indexed_language_queries() {
        let mut structure = Structure::new(&"x".repeat(80_000), Language::Rust);
        for index in 0..10_000 {
            let start = index * 8;
            structure.scopes.extend([
                (start..start + 3, Language::JavaScript),
                (start + 3..start + 3, Language::Css),
                (start + 3..start + 8, Language::Markdown),
            ]);
        }
        let positions: Vec<usize> = (0..1024).map(|sample| sample * 619 % 80_000).collect();
        let queries = |indexed| {
            positions
                .iter()
                .map(|&position| {
                    let other = position.wrapping_mul(17) % structure.source.len();
                    if indexed {
                        (
                            structure.language_at(position),
                            structure.language_body(position),
                            structure.same_language_body(position, other),
                        )
                    } else {
                        let old = structure
                            .scopes
                            .iter()
                            .find(|(range, _)| range.start <= position && position <= range.end);
                        let body = |point| {
                            structure
                                .scopes
                                .iter()
                                .position(|(range, _)| range.contains(&point))
                        };
                        (
                            old.map_or(structure.language, |(_, language)| *language),
                            old.map_or(0..structure.source.len(), |(range, _)| range.clone()),
                            body(position) == body(other),
                        )
                    }
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(queries(false), queries(true));
        let mut old = Vec::new();
        let mut indexed = Vec::new();
        for iteration in 0..20 {
            for new in if iteration % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let start = std::time::Instant::now();
                std::hint::black_box(queries(std::hint::black_box(new)));
                let micros = start.elapsed().as_secs_f64() * 1_000_000.0;
                if new {
                    indexed.push(micros);
                } else {
                    old.push(micros);
                }
            }
        }
        println!(
            "{}",
            serde_json::json!({"scope":"Native language/body queries only; excludes parsing, transfer, browser input and paint", "scopes":structure.scopes.len(), "queriesPerSample":positions.len(), "interleavedSamples":20, "linearMicroseconds":old, "indexedMicroseconds":indexed})
        );
    }

    #[cfg(feature = "editor-parser")]
    #[test]
    fn metadata_batches_match_independent_sort_dedup_and_lexical_bracket_oracles() {
        for ending in ["\n", "\r\n"] {
            let source = Arc::new(format!("α() /* x */ β[]{ending}").repeat(2048));
            let lexical = Structure::scan(&source, Language::Rust).unwrap();
            let mut protected = lexical.protected.clone();
            protected.reverse();
            let mut selections: Vec<_> = source
                .char_indices()
                .map(|(start, ch)| start..start + ch.len_utf8())
                .collect();
            selections.reverse();
            selections.extend(selections.clone());
            let mut expected_selections = selections.clone();
            expected_selections.sort_by_key(|range| (range.start, range.end));
            expected_selections.dedup();
            let mut opaque: Vec<_> = lexical
                .protected
                .iter()
                .map(|(range, _, _)| range.start)
                .collect();
            opaque.reverse();
            opaque.extend(opaque.clone());
            let mut expected_opaque = opaque.clone();
            expected_opaque.sort_unstable();
            expected_opaque.dedup();
            for budget in [1, 7, 256] {
                let mut work = Structure::prepare_parsed(
                    source.clone(),
                    Language::Rust,
                    protected.clone(),
                    vec![],
                    opaque.clone(),
                    selections.clone(),
                );
                assert_eq!(work.advance(0), Some(false));
                while !work.advance(budget).unwrap() {
                    if !work.metadata.complete() {
                        assert_eq!(work.linking.position, 0);
                        assert!(work.structure.brackets.is_empty());
                    }
                }
                let result = work.finish().unwrap();
                assert!(Arc::ptr_eq(&result.source, &source));
                assert_eq!(result.protected, lexical.protected);
                assert_eq!(result.selection_ranges, expected_selections);
                assert_eq!(result.opaque_starts, expected_opaque);
                assert_eq!(result.brackets, lexical.brackets);
            }
        }
    }

    #[cfg(feature = "editor-parser")]
    #[test]
    fn invalid_metadata_remains_rejected_after_yielding_and_releases_its_source() {
        for (protected, scopes, selections) in [
            (
                vec![
                    (0..2, true, RegionKind::String),
                    (1..3, true, RegionKind::String),
                ],
                vec![],
                vec![],
            ),
            (vec![(1..2, true, RegionKind::String)], vec![], vec![]),
            (
                vec![],
                vec![(0..2, Language::Rust), (1..3, Language::Rust)],
                vec![],
            ),
            (vec![], vec![], std::iter::once(2..2).collect()),
            (vec![], vec![], std::iter::once(0..9).collect()),
        ] {
            let source = Arc::new("α()".to_owned());
            let weak = Arc::downgrade(&source);
            let mut work = Structure::prepare_parsed(
                source,
                Language::Rust,
                protected,
                scopes,
                vec![],
                selections,
            );
            while work.advance(1) == Some(false) {}
            assert_eq!(work.advance(256), None);
            assert!(work.finish().is_none());
            assert!(weak.upgrade().is_none());
        }
    }
    #[cfg(feature = "editor-parser")]
    #[test]
    fn bracket_batches_bound_region_and_scope_cursors_and_preserve_pairing() {
        for budget in [1, 7, 256] {
            let source = Arc::new("()".to_owned());
            let mut work = Structure::prepare_parsed(
                source,
                Language::Rust,
                vec![(0..0, true, RegionKind::String); 1000],
                vec![(0..0, Language::JavaScript); 1000],
                vec![],
                vec![],
            );
            assert_eq!(work.advance(0), Some(false));
            assert_eq!(work.linking.position, 0);
            while !work.metadata.advance(&mut work.structure, 256).unwrap() {}
            let mut batches = 0;
            loop {
                let scopes = work.linking.scope_index;
                let regions = work.linking.region_index;
                let mut examined = 0;
                let complete = work
                    .linking
                    .advance(&mut work.structure, budget, &mut || examined += 1)
                    .unwrap();
                assert!(
                    examined + work.linking.scope_index - scopes + work.linking.region_index
                        - regions
                        <= budget
                );
                batches += 1;
                assert!(batches < 3000);
                if complete {
                    break;
                }
            }
            assert!(batches > 1);
            assert_eq!(
                work.finish().unwrap().brackets,
                vec![(0, '(', Some(1)), (1, ')', Some(0))]
            );
        }
    }

    #[cfg(feature = "editor-parser")]
    #[test]
    fn yielded_bracket_limits_and_owned_source_release_match_synchronous_rejection() {
        for count in [MAX_BRACKETS, MAX_BRACKETS + 1] {
            let source = Arc::new("(".repeat(count));
            let weak = Arc::downgrade(&source);
            let mut work = Structure::prepare_parsed(
                source.clone(),
                Language::Rust,
                vec![],
                vec![],
                vec![],
                vec![],
            );
            drop(source);
            let complete = loop {
                match work.advance(256) {
                    Some(false) => {}
                    result => break result,
                }
            };
            if count == MAX_BRACKETS {
                assert_eq!(complete, Some(true));
                assert_eq!(work.finish().unwrap().brackets.len(), count);
            } else {
                assert!(complete.is_none());
                assert!(work.advance(256).is_none());
                assert!(work.finish().is_none());
            }
            assert!(weak.upgrade().is_none());
        }
        let source = Arc::new("文😀 {}".repeat(10_000));
        let weak = Arc::downgrade(&source);
        let mut work = Structure::prepare_parsed(
            source.clone(),
            Language::Rust,
            vec![],
            vec![],
            vec![],
            vec![],
        );
        assert_eq!(work.advance(256), Some(false));
        drop(source);
        assert!(weak.upgrade().is_some());
        assert!(work.finish().is_none());
        assert!(weak.upgrade().is_none());
    }

    #[cfg(feature = "editor-parser")]
    #[test]
    fn parsed_brackets_match_the_original_lexical_oracle() {
        #[derive(serde::Deserialize)]
        struct Case {
            language: Language,
            source: String,
            metadata: serde_json::Value,
        }
        let cases: Vec<Case> =
            serde_json::from_str(include_str!("structure/scanning_cases.json")).unwrap();
        for case in cases {
            if case.metadata.is_null() {
                continue;
            }
            let expected: Vec<(usize, char, Option<usize>)> =
                serde_json::from_value(case.metadata["brackets"].clone()).unwrap();
            let protected = serde_json::from_value(case.metadata["protected"].clone()).unwrap();
            let opaque = serde_json::from_value(case.metadata["opaque_starts"].clone()).unwrap();
            for budget in [1, 7, 256] {
                let mut work = Structure::prepare_parsed(
                    Arc::new(case.source.clone()),
                    case.language,
                    serde_json::from_value(case.metadata["protected"].clone()).unwrap(),
                    vec![],
                    serde_json::from_value(case.metadata["opaque_starts"].clone()).unwrap(),
                    vec![],
                );
                while !work.advance(budget).unwrap() {}
                assert_eq!(
                    work.finish().unwrap().brackets,
                    expected,
                    "{:?}: {:?}",
                    case.language,
                    case.source
                );
            }
            let parsed = Structure::parsed(
                Arc::new(case.source.clone()),
                case.language,
                protected,
                vec![],
                opaque,
                vec![],
            )
            .unwrap();
            assert_eq!(
                parsed.brackets, expected,
                "{:?}: {:?}",
                case.language, case.source
            );
        }
    }

    #[cfg(feature = "editor-parser")]
    #[test]
    fn parsed_brackets_skip_large_opaque_bodies_but_preserve_scope_boundaries() {
        let source = Arc::new(format!("({})", "文😀".repeat(149_000)));
        let end = source.len() - 1;
        let scope_start = source.find('😀').unwrap();
        let scope_end = scope_start + 4;
        let mut parsed = Structure::parsed(
            source.clone(),
            Language::Rust,
            vec![(1..end, true, RegionKind::String)],
            vec![(scope_start..scope_end, Language::JavaScript)],
            vec![],
            vec![],
        )
        .unwrap();
        assert!(Arc::ptr_eq(&parsed.source, &source));
        assert_eq!(parsed.brackets, vec![(0, '(', None), (end, ')', None)]);
        parsed.brackets.clear();
        let mut examined = 0;
        parsed.link_brackets(|| examined += 1).unwrap();
        assert_eq!(
            examined, 2,
            "literal bytes are never decoded by the final bracket pass"
        );
        assert_eq!(parsed.brackets, vec![(0, '(', None), (end, ')', None)]);

        let source = Arc::new(format!("{}()", "文😀".repeat(149_000)));
        let start = source.len() - 2;
        let mut parsed = Structure::parsed(
            source,
            Language::Markdown,
            vec![],
            vec![(start..start + 2, Language::Rust)],
            vec![],
            vec![],
        )
        .unwrap();
        parsed.brackets.clear();
        examined = 0;
        parsed.link_brackets(|| examined += 1).unwrap();
        assert_eq!(
            examined, 2,
            "unbracketed prose is never decoded by the bracket pass"
        );
        assert_eq!(
            parsed.brackets,
            vec![(start, '(', Some(start + 1)), (start + 1, ')', Some(start))]
        );
    }

    #[cfg(feature = "editor-parser")]
    #[test]
    fn parsed_bracket_limits_and_scope_validation_match_reconstruction() {
        let parsed = |source: &str, scopes| {
            Structure::parsed(
                Arc::new(source.to_owned()),
                Language::Rust,
                vec![],
                scopes,
                vec![],
                vec![],
            )
        };
        assert!(parsed(&"(".repeat(MAX_BRACKETS + 1), vec![]).is_none());
        assert_eq!(
            parsed(&"(".repeat(MAX_BRACKETS), vec![])
                .unwrap()
                .brackets
                .len(),
            MAX_BRACKETS
        );
        assert!(parsed("😀", vec![(2..4, Language::Rust)]).is_none());
        assert!(parsed("abcd", vec![(0..3, Language::Rust), (2..4, Language::Rust)]).is_none());
        assert!(parsed("abcd", vec![(0..5, Language::Rust)]).is_none());
        assert_eq!(
            parsed("()", vec![(0..1, Language::Rust), (1..2, Language::Rust)])
                .unwrap()
                .brackets,
            vec![(0, '(', None), (1, ')', None)]
        );
        assert_eq!(
            parsed("{[]}", vec![(1..3, Language::Yaml)])
                .unwrap()
                .brackets,
            vec![(0, '{', None), (3, '}', None)]
        );
        assert_eq!(
            parsed("()", vec![(0..0, Language::Rust)]).unwrap().brackets,
            vec![(0, '(', Some(1)), (1, ')', Some(0))]
        );
    }

    #[test]
    fn lexical_structure_ignores_multiline_comments_raw_strings_and_lifetimes() {
        let text = "fn f<'a>() { /* { /* } */ } */ let x = r##\"}\"##; '\\''; }";
        let syntax = Structure::new(text, Language::Rust);
        let braces: Vec<_> = syntax
            .brackets
            .iter()
            .filter(|(_, ch, _)| matches!(ch, '{' | '}'))
            .collect();
        assert_eq!(braces.len(), 2);
        assert_eq!(braces[0].2, Some(braces[1].0));
        assert!(!Structure::new("\"unterminated", Language::Rust).is_code(13));
        assert!(Structure::new("\"closed\"", Language::Rust).is_code(8));
    }
    #[test]
    fn regex_literals_and_go_escapes_are_opaque_and_work_is_bounded() {
        let syntax = Structure::new(
            "const r = /[{}\\/]/; const x = 1 / 2; {}",
            Language::JavaScript,
        );
        assert_eq!(syntax.brackets.len(), 2);
        let syntax = Structure::new("s := \"escaped \\\" } \"; {}", Language::Go);
        assert_eq!(syntax.brackets.len(), 2);
        assert!(!Structure::new(&"x".repeat(MAX_STRUCTURE_BYTES + 1), Language::Rust).available());
        assert!(!Structure::new(&"{".repeat(MAX_BRACKETS + 1), Language::Rust).available());
    }

    #[test]
    fn python_triple_strings_and_unsupported_text_do_not_supply_brackets() {
        let syntax = Structure::new("'''\n{ }\n'''\nx = [1] # }", Language::Python);
        assert_eq!(syntax.brackets.len(), 2);
        assert!(
            Structure::new("{text}", Language::Plain)
                .brackets
                .is_empty()
        );
    }
}

/// Coordinate-only representation, accepted only through validated reconstruction.
#[cfg(feature = "editor-parser")]
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StructureData {
    language: Language,
    scopes: Vec<(Range<usize>, Language)>,
    selections: Vec<Range<usize>>,
    opaque_starts: Vec<usize>,
    protected: Vec<(Range<usize>, bool, RegionKind)>,
    brackets: Vec<(usize, char, Option<usize>)>,
}

/// Structural list patches use record indices, independently of source byte offsets.
#[cfg(feature = "editor-parser")]
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub(super) enum StructurePublication {
    Full(StructureData),
    Changes { changes: StructureChanges },
}

#[cfg(feature = "editor-parser")]
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StructureChanges {
    language: Language,
    scopes: StructuralRecords<(Range<usize>, Language)>,
    selections: StructuralRecords<Range<usize>>,
    opaque_starts: StructuralRecords<usize>,
    protected: StructuralRecords<(Range<usize>, bool, RegionKind)>,
    brackets: StructuralRecords<(usize, char, Option<usize>)>,
}

#[cfg(feature = "editor-parser")]
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum StructuralRecords<T> {
    Full(Vec<T>),
    Replace {
        start: usize,
        end: usize,
        items: Vec<T>,
    },
}

#[cfg(feature = "editor-parser")]
impl<T: Clone + PartialEq> StructuralRecords<T> {
    #[cfg(test)]
    fn publication(current: &[T], previous: &[T]) -> Self {
        let prefix = current
            .iter()
            .zip(previous)
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = current[prefix..]
            .iter()
            .rev()
            .zip(previous[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        // Small lists remain standalone rather than paying patch-envelope overhead.
        if prefix + suffix > 4 {
            Self::Replace {
                start: prefix,
                end: previous.len() - suffix,
                items: current[prefix..current.len() - suffix].to_vec(),
            }
        } else {
            Self::Full(current.to_vec())
        }
    }
    fn is_patch(&self) -> bool {
        matches!(self, Self::Replace { .. })
    }
    fn transmitted_records(&self) -> usize {
        match self {
            Self::Full(items) | Self::Replace { items, .. } => items.len(),
        }
    }
    fn resolved_len(&self, previous: &[T]) -> Option<usize> {
        match self {
            Self::Full(items) => Some(items.len()),
            Self::Replace { start, end, items } => {
                if start > end || *end > previous.len() {
                    return None;
                }
                previous
                    .len()
                    .checked_sub(end - start)?
                    .checked_add(items.len())
            }
        }
    }
    #[cfg(test)]
    fn resolve(self, previous: &[T]) -> Vec<T> {
        match self {
            Self::Full(items) => items,
            Self::Replace { start, end, items } => {
                let mut result = Vec::with_capacity(previous.len() - (end - start) + items.len());
                result.extend_from_slice(&previous[..start]);
                result.extend(items);
                result.extend_from_slice(&previous[end..]);
                result
            }
        }
    }
}

#[cfg(feature = "editor-parser")]
impl StructurePublication {
    #[cfg(test)]
    pub(super) fn publication(current: &Structure, previous: Option<&Structure>) -> Self {
        if let Some(previous) = previous.filter(|old| old.language == current.language) {
            let changes = StructureChanges {
                language: current.language,
                scopes: StructuralRecords::publication(&current.scopes, &previous.scopes),
                selections: StructuralRecords::publication(
                    &current.selection_ranges,
                    &previous.selection_ranges,
                ),
                opaque_starts: StructuralRecords::publication(
                    &current.opaque_starts,
                    &previous.opaque_starts,
                ),
                protected: StructuralRecords::publication(&current.protected, &previous.protected),
                brackets: StructuralRecords::publication(&current.brackets, &previous.brackets),
            };
            if changes.scopes.is_patch()
                || changes.selections.is_patch()
                || changes.opaque_starts.is_patch()
                || changes.protected.is_patch()
                || changes.brackets.is_patch()
            {
                return Self::Changes { changes };
            }
        }
        Self::Full(current.transfer_data())
    }
    pub(super) fn needs_base(&self) -> bool {
        matches!(self, Self::Changes { .. })
    }
    pub(super) fn record_count(&self) -> usize {
        match self {
            Self::Full(data) => data.record_count(),
            Self::Changes { changes } => changes
                .scopes
                .transmitted_records()
                .saturating_add(changes.selections.transmitted_records())
                .saturating_add(changes.opaque_starts.transmitted_records())
                .saturating_add(changes.protected.transmitted_records())
                .saturating_add(changes.brackets.transmitted_records()),
        }
    }
    #[cfg(test)]
    pub(super) fn validate(
        self,
        source: Arc<String>,
        previous: Option<&Structure>,
        budget: usize,
    ) -> Option<Structure> {
        let mut work = StructureReception::new(self, source, previous, budget)?;
        while !work.advance()? {}
        work.finish()
    }

    #[cfg(test)]
    fn validate_reference(
        self,
        source: Arc<String>,
        previous: Option<&Structure>,
        budget: usize,
    ) -> Option<Structure> {
        let data = match self {
            Self::Full(data) => {
                if data.record_count() > budget {
                    return None;
                }
                data
            }
            Self::Changes { changes } => {
                let old = previous.filter(|old| old.language == changes.language)?;
                // Validate every range and the expanded budget before allocating any list.
                let count = changes
                    .scopes
                    .resolved_len(&old.scopes)?
                    .checked_add(changes.selections.resolved_len(&old.selection_ranges)?)?
                    .checked_add(changes.opaque_starts.resolved_len(&old.opaque_starts)?)?
                    .checked_add(changes.protected.resolved_len(&old.protected)?)?
                    .checked_add(changes.brackets.resolved_len(&old.brackets)?)?;
                if count > budget {
                    return None;
                }
                StructureData {
                    language: changes.language,
                    scopes: changes.scopes.resolve(&old.scopes),
                    selections: changes.selections.resolve(&old.selection_ranges),
                    opaque_starts: changes.opaque_starts.resolve(&old.opaque_starts),
                    protected: changes.protected.resolve(&old.protected),
                    brackets: changes.brackets.resolve(&old.brackets),
                }
            }
        };
        data.validate_reference(source)
    }
}

#[cfg(feature = "editor-parser")]
impl Structure {
    pub(super) fn record_count(&self) -> usize {
        self.scopes
            .len()
            .saturating_add(self.selection_ranges.len())
            .saturating_add(self.opaque_starts.len())
            .saturating_add(self.protected.len())
            .saturating_add(self.brackets.len())
    }
    #[cfg(test)]
    pub(super) fn transfer_data(&self) -> StructureData {
        StructureData {
            language: self.language,
            scopes: self.scopes.clone(),
            selections: self.selection_ranges.clone(),
            opaque_starts: self.opaque_starts.clone(),
            protected: self.protected.clone(),
            brackets: self.brackets.clone(),
        }
    }
}

#[cfg(feature = "editor-parser")]
impl StructureData {
    pub(super) fn record_count(&self) -> usize {
        self.scopes
            .len()
            .saturating_add(self.selections.len())
            .saturating_add(self.opaque_starts.len())
            .saturating_add(self.protected.len())
            .saturating_add(self.brackets.len())
    }
    #[cfg(test)]
    fn validate_reference(self, source: Arc<String>) -> Option<Structure> {
        let valid_range = |range: &Range<usize>| {
            range.start < range.end
                && source.is_char_boundary(range.start)
                && source.is_char_boundary(range.end)
        };
        if self.brackets.len() > MAX_BRACKETS
            || self.scopes.iter().any(|(range, _)| {
                range.start > range.end
                    || !source.is_char_boundary(range.start)
                    || !source.is_char_boundary(range.end)
            })
            || self
                .scopes
                .windows(2)
                .any(|pair| pair[0].0.end > pair[1].0.start)
            || self.selections.iter().any(|range| !valid_range(range))
            || self
                .selections
                .windows(2)
                .any(|pair| (pair[0].start, pair[0].end) >= (pair[1].start, pair[1].end))
            || self
                .protected
                .iter()
                .any(|(range, _, _)| !valid_range(range))
            || self
                .protected
                .windows(2)
                .any(|pair| pair[0].0.end > pair[1].0.start)
            || self
                .opaque_starts
                .iter()
                .any(|&offset| !source.is_char_boundary(offset))
            || self.opaque_starts.windows(2).any(|pair| pair[0] >= pair[1])
            || self.brackets.windows(2).any(|pair| pair[0].0 >= pair[1].0)
        {
            return None;
        }
        let result = Structure {
            source,
            language: self.language,
            scopes: self.scopes,
            selection_ranges: self.selections,
            opaque_starts: self.opaque_starts,
            available: true,
            protected: self.protected,
            brackets: self.brackets,
        };
        for &(position, ch, partner) in &result.brackets {
            if !matches!(ch, '(' | ')' | '[' | ']' | '{' | '}')
                || !result
                    .source
                    .get(position..)
                    .is_some_and(|text| text.starts_with(ch))
                || result
                    .region_at(position)
                    .is_some_and(|(range, _, _)| range.contains(&position))
                || !supports_brackets(result.language_at(position))
            {
                return None;
            }
            if let Some(partner) = partner {
                let index = result
                    .brackets
                    .binary_search_by_key(&partner, |bracket| bracket.0)
                    .ok()?;
                let (_, other, back) = result.brackets[index];
                let ordered = if position < partner {
                    closing(ch) == Some(other)
                } else {
                    closing(other) == Some(ch)
                };
                if back != Some(position)
                    || !ordered
                    || !result.same_language_body(position, partner)
                {
                    return None;
                }
            }
        }
        Some(result)
    }
}
