//! Shared grammar-aware paint; source bytes and line boundaries are preserved.
use super::SyntaxDocument;
use crate::editor::structure::RegionKind;
use crate::highlight::{
    Language, Token, TokenKind, TokenRow, TokenRows, highlight_lines, share_token_row,
    share_token_rows,
};
use std::{collections::HashMap, ops::Range, sync::Arc};

/// Merge validated ordered span endpoints into disjoint paint segments.
/// Protected regions, embedded scopes and semantic spans each have disjoint ranges.
fn paint_segments(
    protected: impl Iterator<Item = usize>,
    scopes: impl Iterator<Item = usize>,
    semantic: impl Iterator<Item = usize>,
    length: usize,
) -> impl Iterator<Item = Range<usize>> {
    let mut protected = protected.peekable();
    let mut scopes = scopes.peekable();
    let mut semantic = semantic.peekable();
    let mut source = [0, length].into_iter().peekable();
    let boundaries = std::iter::from_fn(move || {
        let end = [
            protected.peek().copied(),
            scopes.peek().copied(),
            semantic.peek().copied(),
            source.peek().copied(),
        ]
        .into_iter()
        .flatten()
        .min()?;
        while protected.peek() == Some(&end) {
            protected.next();
        }
        while scopes.peek() == Some(&end) {
            scopes.next();
        }
        while semantic.peek() == Some(&end) {
            semantic.next();
        }
        while source.peek() == Some(&end) {
            source.next();
        }
        Some(end)
    });
    let mut previous = None;
    boundaries.filter_map(move |end| previous.replace(end).map(|start| start..end))
}

struct PaintedPiece {
    language: Language,
    kind: Option<TokenKind>,
    rows: TokenRows,
}

struct Piece {
    end: usize,
    paint: Arc<PaintedPiece>,
}

struct PaintedRow {
    end: usize,
    pieces: Vec<(Arc<PaintedPiece>, usize)>,
    tokens: TokenRow,
}

/// Match retained row parts without allocating another list for unchanged rows.
struct RowPaint {
    previous: Option<PaintedRow>,
    parts: Vec<(Arc<PaintedPiece>, usize)>,
    matched: usize,
    changed: bool,
}

impl RowPaint {
    fn new(
        row: &crate::editor::lines::Line,
        source: &str,
        previous: &mut HashMap<usize, PaintedRow>,
        edit: &super::InputEdit,
    ) -> Self {
        let end = row.end - usize::from(source[row.start..row.end].ends_with('\n'));
        let retained = previous_range(row.start..end, edit).and_then(|range| {
            previous
                .remove(&range.start)
                .filter(|row| row.end == range.end)
        });
        Self {
            previous: retained,
            parts: Vec::new(),
            matched: 0,
            changed: false,
        }
    }

    fn append(&mut self, piece: &Arc<PaintedPiece>, index: usize) {
        if !self.changed {
            if self
                .previous
                .as_ref()
                .and_then(|row| row.pieces.get(self.matched))
                .is_some_and(|(old, old_row)| *old_row == index && Arc::ptr_eq(old, piece))
            {
                self.matched += 1;
                return;
            }
            if let Some(row) = &mut self.previous {
                row.pieces.truncate(self.matched);
                self.parts = std::mem::take(&mut row.pieces);
            }
            self.changed = true;
        }
        self.parts.push((piece.clone(), index));
    }

    fn finish(mut self, row: &crate::editor::lines::Line, text: &str) -> PaintedRow {
        let raw = &text[row.start..row.end];
        let source = raw.strip_suffix('\n').unwrap_or(raw);
        let end = row.start + source.len();
        if !self.changed
            && let Some(mut row) = self.previous.take()
        {
            if self.matched == row.pieces.len() {
                row.end = end;
                return row;
            }
            row.pieces.truncate(self.matched);
            self.parts = row.pieces;
        }
        // Grammar analysis already owns source/work admission. Long prepared
        // rows retain their styles; the renderer bounds their paint probes.
        // Unavailable grammar still uses the independently limited lexical path.
        let tokens = if let [(piece, row)] = self.parts.as_slice() {
            piece.rows[*row].clone()
        } else {
            share_token_row(
                self.parts
                    .iter()
                    .flat_map(|(piece, row)| piece.rows[*row].iter().cloned())
                    .collect::<Vec<_>>(),
            )
        };
        PaintedRow {
            end,
            pieces: self.parts,
            tokens,
        }
    }
}

#[derive(Default)]
pub(super) struct SyntaxPaint {
    pub(super) source: Arc<String>,
    pub(super) source_change: Option<super::InputEdit>,
    pieces: HashMap<usize, Piece>,
    rows: HashMap<usize, PaintedRow>,
    #[cfg(test)]
    repainted_pieces: usize,
    #[cfg(test)]
    reused_source_change: bool,
}

fn previous_range(range: Range<usize>, edit: &super::InputEdit) -> Option<Range<usize>> {
    if range.end <= edit.start_byte {
        Some(range)
    } else if range.start >= edit.new_end_byte {
        Some(
            range
                .start
                .checked_sub(edit.new_end_byte)?
                .checked_add(edit.old_end_byte)?
                ..range
                    .end
                    .checked_sub(edit.new_end_byte)?
                    .checked_add(edit.old_end_byte)?,
        )
    } else {
        None
    }
}

impl SyntaxDocument {
    /// None requests the ordinary bounded lexical fallback after failed analysis.
    pub fn highlight_lines(&self) -> Option<Vec<Vec<Token>>> {
        let context = self.structure()?;
        self.highlight_with_structure(&context)
            .map(|rows| rows.iter().map(|row| row.to_vec()).collect())
    }

    pub(super) fn highlight_with_structure(
        &self,
        context: &crate::editor::Structure,
    ) -> Option<TokenRows> {
        let mut semantic = Vec::new();
        let mut visited = 0;
        for (index, (tree, provider, cache)) in self
            .tree
            .iter()
            .zip(self.provider)
            .map(|(tree, provider)| (tree, provider, &self.highlights))
            .chain(self.embedded.iter().filter_map(|body| {
                body.tree
                    .as_ref()
                    .map(|tree| (tree, body.provider, &body.highlights))
            }))
            .enumerate()
        {
            cache
                .borrow_mut()
                .collect(tree, provider, &self.text, &mut visited, &mut semantic)
                .ok()?;
            if index == 0 {
                // An injected grammar owns its source range. The outer grammar
                // may also expose punctuation there (Markdown inline blocks).
                semantic.retain(|(range, _)| {
                    !self.embedded.iter().any(|body| {
                        body.tree.is_some()
                            && range.start < body.range.end_byte
                            && range.end > body.range.start_byte
                    })
                });
            }
        }
        semantic.sort_by_key(|(range, _)| (range.start, range.end));
        // Opaque grammar contexts own their complete strings/comments. Tokens
        // inside them cannot split their paint or override that classification.
        semantic.retain(|(range, _)| {
            context
                .protected
                .partition_point(|(protected, _, _)| protected.start <= range.start)
                .checked_sub(1)
                .and_then(|index| context.protected.get(index))
                .is_none_or(|(protected, _, _)| range.end > protected.end)
        });
        // A selector must classify leaves or disjoint spans. Overlap is a typed
        // analysis fallback, rather than a renderer-dependent priority rule.
        if semantic
            .windows(2)
            .any(|pair| pair[0].0.end > pair[1].0.start)
        {
            return None;
        }
        let segments = paint_segments(
            context
                .protected
                .iter()
                .flat_map(|(range, _, _)| [range.start, range.end]),
            context
                .scopes
                .iter()
                .flat_map(|(range, _)| [range.start, range.end]),
            semantic
                .iter()
                .flat_map(|(range, _)| [range.start, range.end]),
            self.text.len(),
        );
        let mut previous = self.paint.borrow_mut();
        // Only a source-identity match permits the parser's exact change span
        // to validate retained paint. Updates without paint can leave an older
        // base; compute that wider envelope instead of trusting the last edit.
        let source_change = previous.source_change;
        #[cfg(test)]
        let reused_source_change = source_change.is_some();
        let edit = if Arc::ptr_eq(&previous.source, &self.text) {
            super::input_edit("", "", None).ok()?
        } else if let Some(edit) = source_change {
            edit
        } else {
            super::input_edit(&previous.source, &self.text, None).ok()?
        };
        let mut pieces = HashMap::new();
        #[cfg(test)]
        let mut repainted_pieces = 0;
        let mut rows = HashMap::new();
        let mut tokens = Vec::with_capacity(self.source_lines.len());
        let mut current_row = 0;
        let mut current = Some(RowPaint::new(
            self.source_lines.first()?,
            &self.text,
            &mut previous.rows,
            &edit,
        ));
        let mut finish_row = |paint: RowPaint, row: &crate::editor::lines::Line| {
            let painted = paint.finish(row, &self.text);
            tokens.push(painted.tokens.clone());
            rows.insert(row.start, painted);
        };
        let mut protected = 0;
        let mut selected = 0;
        for range in segments {
            let (start, end) = (range.start, range.end);
            while context
                .protected
                .get(protected)
                .is_some_and(|(range, _, _)| range.end <= start)
            {
                protected += 1;
            }
            while semantic
                .get(selected)
                .is_some_and(|(range, _)| range.end <= start)
            {
                selected += 1;
            }
            let language = context.language_at(self.text.floor_char_boundary(end - 1));
            let literal = context
                .protected
                .get(protected)
                .filter(|(range, _, _)| range.contains(&start));
            let kind = literal
                .map(|(_, _, kind)| match kind {
                    RegionKind::LineComment | RegionKind::BlockComment => TokenKind::Comment,
                    RegionKind::Text => TokenKind::Plain,
                    RegionKind::String
                        if matches!(
                            language,
                            Language::Rust
                                | Language::Java
                                | Language::CSharp
                                | Language::C
                                | Language::Cpp
                                | Language::Go
                        ) && (self.text[start..end].starts_with('\'')
                            || self.text[start..end].starts_with("b'")) =>
                    {
                        TokenKind::Char
                    }
                    _ => TokenKind::String,
                })
                .or_else(|| {
                    semantic
                        .get(selected)
                        .filter(|(range, _)| range.contains(&start))
                        .map(|(_, kind)| *kind)
                });
            let piece = &self.text[start..end];
            let retained = previous_range(start..end, &edit).and_then(|range| {
                let part = previous.pieces.get(&range.start)?;
                (part.end == range.end
                    && part.paint.language == language
                    && part.paint.kind == kind)
                    .then(|| part.paint.clone())
            });
            let paint = retained.unwrap_or_else(|| {
                #[cfg(test)]
                {
                    repainted_pieces += 1;
                }
                let painted = if let Some(kind) = kind {
                    piece
                        .split('\n')
                        .map(|text| {
                            vec![Token {
                                kind,
                                text: Arc::new(text.into()),
                            }]
                        })
                        .collect()
                } else {
                    highlight_lines(piece, language)
                };
                Arc::new(PaintedPiece {
                    language,
                    kind,
                    rows: share_token_rows(
                        painted
                            .into_iter()
                            .map(|tokens| {
                                tokens
                                    .into_iter()
                                    .filter(|token| !token.text.is_empty())
                                    .collect()
                            })
                            .collect(),
                    ),
                })
            });
            for index in 0..paint.rows.len() {
                if index > 0 {
                    finish_row(current.take()?, self.source_lines.get(current_row)?);
                    current_row += 1;
                    current = Some(RowPaint::new(
                        self.source_lines.get(current_row)?,
                        &self.text,
                        &mut previous.rows,
                        &edit,
                    ));
                }
                if !paint.rows[index].is_empty() {
                    current.as_mut()?.append(&paint, index);
                }
            }
            pieces.insert(start, Piece { end, paint });
        }
        if current_row + 1 != self.source_lines.len() {
            return None;
        }
        finish_row(current.take()?, self.source_lines.get(current_row)?);
        *previous = SyntaxPaint {
            source: self.text.clone(),
            source_change: None,
            pieces,
            rows,
            #[cfg(test)]
            repainted_pieces,
            #[cfg(test)]
            reused_source_change,
        };
        Some(tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streamed_paint_segments_match_unique_boundaries_across_ordered_span_lists() {
        use std::collections::BTreeSet;
        let cases = [
            vec![],
            vec![0],
            vec![0, 0],
            vec![1, 2],
            vec![0, 2, 2, 4],
            vec![1, 3, 5, 8],
            vec![0, 8],
        ];
        for protected in &cases {
            for scopes in &cases {
                for semantic in &cases {
                    let expected = protected
                        .iter()
                        .chain(scopes)
                        .chain(semantic)
                        .copied()
                        .chain([0, 8])
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>();
                    let expected = expected
                        .windows(2)
                        .map(|pair| pair[0]..pair[1])
                        .collect::<Vec<_>>();
                    assert_eq!(
                        paint_segments(
                            protected.iter().copied(),
                            scopes.iter().copied(),
                            semantic.iter().copied(),
                            8
                        )
                        .collect::<Vec<_>>(),
                        expected
                    );
                }
            }
        }
        assert!(
            paint_segments(
                std::iter::empty(),
                std::iter::empty(),
                std::iter::empty(),
                0
            )
            .next()
            .is_none()
        );
    }

    #[test]
    fn retained_row_matching_rebuilds_changed_truncated_and_extended_parts() {
        let piece = |text: &str| {
            Arc::new(PaintedPiece {
                language: Language::Rust,
                kind: Some(TokenKind::Keyword),
                rows: share_token_rows(vec![vec![Token {
                    kind: TokenKind::Keyword,
                    text: std::sync::Arc::new(text.into()),
                }]]),
            })
        };
        let pieces = [piece("a"), piece("b"), piece("c")];
        for indices in [
            vec![0, 1],
            vec![0, 2],
            vec![2, 1],
            vec![0],
            vec![0, 1, 2],
            vec![],
        ] {
            let previous = PaintedRow {
                end: 2,
                pieces: vec![(pieces[0].clone(), 0), (pieces[1].clone(), 0)],
                tokens: Arc::from(vec![
                    pieces[0].rows[0][0].clone(),
                    pieces[1].rows[0][0].clone(),
                ]),
            };
            let original_parts = previous.pieces.as_ptr();
            let original_tokens = previous.tokens.clone();
            let mut paint = RowPaint {
                previous: Some(previous),
                parts: Vec::new(),
                matched: 0,
                changed: false,
            };
            let source = indices
                .iter()
                .map(|index| pieces[*index].rows[0][0].text.as_str())
                .collect::<String>();
            for index in &indices {
                paint.append(&pieces[*index], 0);
            }
            let row = paint.finish(&crate::editor::lines::lines(&source)[0], &source);
            assert_eq!(row.end, source.len());
            assert_eq!(row.pieces.len(), indices.len());
            for (token, index) in row.tokens.iter().zip(&indices) {
                assert!(Arc::ptr_eq(&token.text, &pieces[*index].rows[0][0].text));
            }
            assert_eq!(
                row.tokens
                    .iter()
                    .map(|token| token.text.as_str())
                    .collect::<String>(),
                source
            );
            if indices == [0, 1] {
                assert_eq!(row.pieces.as_ptr(), original_parts);
                assert!(Arc::ptr_eq(&row.tokens, &original_tokens));
            } else {
                assert!(!Arc::ptr_eq(&row.tokens, &original_tokens));
            }
        }
    }

    #[test]
    fn color_classification_reuses_descendants_without_losing_work_limits() {
        let source = (0..200)
            .map(|index| format!("fn f{index}() {{ call(\"文😀\"); }}\r\n"))
            .collect::<String>();
        let mut document = SyntaxDocument::new(Language::Rust).unwrap();
        document.prepare(&source, 4, || true).1.unwrap();
        let changed = source.replacen("文😀", "😀 changed", 1);
        let next = document.prepare(&changed, 4, || true).1.unwrap();
        let mut nodes = 0;
        super::super::visit_tree(document.tree.as_ref().unwrap(), &mut nodes, |_| Ok(())).unwrap();
        assert!(document.highlights.borrow().reused_nodes > nodes / 2);
        let mut fresh = SyntaxDocument::new(Language::Rust).unwrap();
        assert_eq!(
            next.highlights,
            fresh.prepare(&changed, 4, || true).1.unwrap().highlights
        );
        let mut visited = super::super::MAX_FOLD_NODES - nodes;
        let mut spans = Vec::new();
        document
            .highlights
            .borrow_mut()
            .collect(
                document.tree.as_ref().unwrap(),
                document.provider.unwrap(),
                &changed,
                &mut visited,
                &mut spans,
            )
            .unwrap();
        assert_eq!(visited, super::super::MAX_FOLD_NODES);
        assert_eq!(
            document.highlights.borrow_mut().collect(
                document.tree.as_ref().unwrap(),
                document.provider.unwrap(),
                &changed,
                &mut visited,
                &mut spans
            ),
            Err(super::super::SyntaxStatus::TooLarge)
        );
        document.update(&changed, || false);
        assert_eq!(document.highlights.borrow().reused_nodes, 0);
    }

    #[test]
    fn document_dependent_color_classification_remains_fresh() {
        fn classify(
            node: super::super::Node<'_>,
            _parent: Option<super::super::Node<'_>>,
        ) -> Option<TokenKind> {
            if node.kind() != "identifier" {
                return None;
            }
            let mut root = node;
            while let Some(parent) = root.parent() {
                root = parent;
            }
            root.has_error().then_some(TokenKind::Type)
        }
        let provider = crate::editor::SyntaxProvider {
            highlight: Some(classify),
            highlight_scope: crate::editor::SyntaxHighlightScope::Document,
            ..crate::editor::syntax_provider(Language::Rust).unwrap()
        };
        let source = "fn first() { call(); }\r\nfn second() { call(); }\r\n";
        let mut document = SyntaxDocument::with_provider(Language::Rust, Some(provider)).unwrap();
        let before = document.prepare(source, 4, || true).1.unwrap();
        let changed = format!("{source}fn broken(");
        let next = document.prepare(&changed, 4, || true).1.unwrap();
        assert_ne!(
            before.highlights.as_ref().unwrap()[0],
            next.highlights.as_ref().unwrap()[0]
        );
        let mut fresh = SyntaxDocument::with_provider(Language::Rust, Some(provider)).unwrap();
        assert_eq!(
            next.highlights,
            fresh.prepare(&changed, 4, || true).1.unwrap().highlights
        );
        assert_eq!(document.highlights.borrow().reused_nodes, 0);
    }

    #[test]
    fn grammar_paint_retains_unchanged_piece_and_row_allocations() {
        let source = (0..200)
            .map(|index| format!("fn f{index}() {{\r\n call(\"文😀\");\r\n}}\r\n"))
            .collect::<String>();
        let mut document = SyntaxDocument::new(Language::Rust).unwrap();
        let original = document.prepare(&source, 4, || true).1.unwrap();
        let pieces = document.paint.borrow().pieces.len();
        let row_allocations = |document: &SyntaxDocument| {
            document
                .source_lines
                .iter()
                .map(|row| {
                    let paint = document.paint.borrow();
                    let parts = &paint.rows[&row.start].pieces;
                    (!parts.is_empty()).then_some(parts.as_ptr() as usize)
                })
                .collect::<Vec<_>>()
        };
        let original_parts = row_allocations(&document);
        let changed = source.replacen("文😀", "😀 changed", 1);
        let next = document.prepare(&changed, 4, || true).1.unwrap();
        let next_parts = row_allocations(&document);
        assert!(
            original_parts
                .iter()
                .zip(&next_parts)
                .filter(|(old, next)| old.is_some() && old == next)
                .count()
                > next_parts.len() / 2
        );
        let old_rows = original.highlights.as_ref().unwrap();
        let rows = next.highlights.as_ref().unwrap();
        assert!(
            old_rows
                .iter()
                .zip(rows.iter())
                .filter(|(old, next)| Arc::ptr_eq(old, next))
                .count()
                > rows.len() / 2
        );
        assert!(document.paint.borrow().repainted_pieces < pieces / 10);
        assert!(document.paint.borrow().reused_source_change);
        assert!(original.matches_source(&source));
        let shifted = document
            .prepare(&format!("\r\n{changed}"), 4, || true)
            .1
            .unwrap();
        let shifted_parts = row_allocations(&document);
        assert!(
            next_parts
                .iter()
                .zip(shifted_parts.iter().skip(1))
                .filter(|(old, next)| old.is_some() && old == next)
                .count()
                > next_parts.len() / 2
        );
        let shifted_rows = shifted.highlights.as_ref().unwrap();
        assert!(
            rows.iter()
                .zip(shifted_rows.iter().skip(1))
                .filter(|(old, next)| Arc::ptr_eq(old, next))
                .count()
                > rows.len() / 2
        );
        document.update("cancelled", || false);
        assert!(document.paint.borrow().pieces.is_empty());
        assert!(document.paint.borrow().rows.is_empty());
        assert!(next.matches_source(&changed));
    }

    #[test]
    fn skipped_paint_versions_use_the_retained_source_base() {
        let source = "fn first() { call(\"文😀\"); }\r\nfn second() { call(); }\r\n";
        let mut document = SyntaxDocument::new(Language::Rust).unwrap();
        let first = document.prepare(source, 4, || true).1.unwrap();
        let middle = source.replacen("文😀", "middle", 1);
        let changed = middle.replace("second", "renamed");
        document.update(&middle, || true);
        document.update(&changed, || true);
        let warm = document.prepare(&changed, 4, || true).1.unwrap();
        assert!(!document.paint.borrow().reused_source_change);
        let mut fresh = SyntaxDocument::new(Language::Rust).unwrap();
        assert_eq!(
            warm.highlights,
            fresh.prepare(&changed, 4, || true).1.unwrap().highlights
        );
        assert!(first.matches_source(source));
        document.update("cancelled", || false);
        assert!(document.paint.borrow().source_change.is_none());
        let restored = document.prepare(source, 4, || true).1.unwrap();
        let mut fresh = SyntaxDocument::new(Language::Rust).unwrap();
        assert_eq!(
            restored.highlights,
            fresh.prepare(source, 4, || true).1.unwrap().highlights
        );
    }

    #[test]
    fn retained_grammar_paint_matches_cold_colors_after_context_and_scope_edits() {
        for &(path, source, _) in crate::editor::syntax_contracts::LANGUAGE_CASES {
            let language = crate::highlight::language_from_path(path);
            let source = source.repeat(3);
            let mut document = SyntaxDocument::new(language).unwrap();
            for text in [
                source.clone(),
                format!("\r\n{source}"),
                source.replacen("文😀", "😀 changed", 1),
                source.replace('"', ""),
                format!("/*\r\n{source}"),
                source.replace('}', ""),
                source,
            ] {
                let warm = document.prepare(&text, 4, || true).1.unwrap();
                let mut fresh = SyntaxDocument::new(language).unwrap();
                let cold = fresh.prepare(&text, 4, || true).1.unwrap();
                assert_eq!(warm.highlights, cold.highlights, "{path}: {text}");
                assert!(warm.matches_source(&text));
            }
        }
    }

    #[test]
    fn primitive_type_terminals_do_not_overlap_paint_or_become_strings() {
        for (language, source, name) in [
            (
                Language::TypeScript,
                "function main(): number { return 1; }",
                "number",
            ),
            (
                Language::Tsx,
                "function main(): number { return 1; }",
                "number",
            ),
            (
                Language::TypeScript,
                "const name: string = \"value\";",
                "string",
            ),
            (Language::CSharp, "class A { int value = 1; }", "int"),
            (
                Language::CSharp,
                "class A { string name = \"value\"; }",
                "string",
            ),
            (Language::Java, "class A { int value = 1; }", "int"),
            (Language::Cpp, "int main() { return 1; }", "int"),
            (Language::C, "int main() { return 1; }", "int"),
            (
                Language::Go,
                "package main\nfunc main() { var value int = 1 }",
                "int",
            ),
        ] {
            let mut document = SyntaxDocument::new(language).unwrap();
            let analysis = document.prepare(source, 4, || true).1.unwrap();
            let rows = analysis
                .highlights()
                .unwrap_or_else(|| panic!("{language:?}: missing paint"));
            let tokens = rows.iter().flat_map(|row| row.iter()).collect::<Vec<_>>();
            assert!(
                tokens
                    .iter()
                    .any(|token| token.kind == TokenKind::Type && token.text.as_str() == name),
                "{language:?}: {tokens:?}"
            );
            if source.contains("return 1") || source.contains("= 1") {
                assert!(
                    tokens
                        .iter()
                        .any(|token| token.kind == TokenKind::Number && token.text.as_str() == "1"),
                    "{language:?}: {tokens:?}"
                );
            }
            let at = source.find(name).unwrap();
            assert!(
                !analysis
                    .structure()
                    .unwrap()
                    .protected
                    .iter()
                    .any(|(range, _, _)| range.contains(&at)),
                "{language:?}: type is not a string literal"
            );
            let changed = format!("\n{source}");
            let warm = document.prepare(&changed, 4, || true).1.unwrap();
            let mut fresh = SyntaxDocument::new(language).unwrap();
            assert_eq!(
                warm.highlights(),
                fresh.prepare(&changed, 4, || true).1.unwrap().highlights()
            );
        }
    }

    #[test]
    fn paint_preserves_all_provider_sources_and_line_endings() {
        for &(path, source, _) in crate::editor::syntax_contracts::LANGUAGE_CASES {
            let language = crate::highlight::language_from_path(path);
            let mut document = SyntaxDocument::new(language).unwrap();
            document.update(source, || true);
            let lines = document.highlight_lines().unwrap();
            assert_eq!(
                lines
                    .iter()
                    .map(|line| line
                        .iter()
                        .map(|token| token.text.as_str())
                        .collect::<String>())
                    .collect::<Vec<_>>(),
                source.split('\n').collect::<Vec<_>>(),
                "{path}"
            );
        }
    }
    #[test]
    fn embedded_code_and_interpolation_receive_their_own_colors() {
        let source =
            "<script>const t = `文 ${call(42)} tail`;</script><style>a { color: red; }</style>";
        let mut document = SyntaxDocument::new(Language::Html).unwrap();
        document.update(source, || true);
        let lines = document.highlight_lines().unwrap();
        let tokens = &lines[0];
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == TokenKind::Keyword && token.text.as_str() == "const")
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == TokenKind::Function && token.text.as_str() == "call")
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == TokenKind::Number && token.text.as_str() == "42")
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == TokenKind::Attribute && token.text.as_str() == "color")
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == TokenKind::String && token.text.contains("tail"))
        );
    }

    #[test]
    fn prepared_long_unicode_tokens_keep_styles_in_every_builtin_code_language() {
        let body = "word 文😀e\u{301} ".repeat(1500);
        for (language, before, after) in [
            (Language::Rust, "fn main() { let value = \"", "\"; }"),
            (Language::Python, "value = \"", "\""),
            (Language::JavaScript, "const value = \"", "\";"),
            (Language::Jsx, "const value = <div title=\"", "\" />;"),
            (Language::TypeScript, "const value: string = \"", "\";"),
            (Language::Tsx, "const value = <div title=\"", "\" />;"),
            (Language::Java, "class A { String value = \"", "\"; }"),
            (Language::CSharp, "class A { string value = \"", "\"; }"),
            (Language::Cpp, "const char* value = \"", "\";"),
            (Language::Php, "<?php $value = \"", "\";"),
            (Language::Shell, "value=\"", "\""),
            (Language::C, "const char* value = \"", "\";"),
            (Language::Go, "package main\nvar value = \"", "\""),
            (Language::Html, "<div title=\"", "\"></div>"),
            (Language::Css, ".value { content: \"", "\"; }"),
        ] {
            let source = format!("{before}{body}{after}");
            let mut document = SyntaxDocument::new(language).unwrap();
            document.update(&source, || true);
            let rows = document.highlight_lines().unwrap();
            assert!(
                rows.iter()
                    .flat_map(|row| row.iter())
                    .any(|token| { token.kind == TokenKind::String && token.text.contains(&body) }),
                "{language:?}"
            );
            assert_eq!(
                rows.iter()
                    .map(|row| row
                        .iter()
                        .map(|token| token.text.as_str())
                        .collect::<String>())
                    .collect::<Vec<_>>()
                    .join("\n"),
                source,
                "{language:?}"
            );
        }
    }
    #[test]
    fn mixed_long_rows_share_text_across_pieces_and_warm_publication() {
        for ending in ["\n", "\r\n"] {
            let mut document = SyntaxDocument::new(Language::Rust).unwrap();
            let source = format!(
                "fn first() {{ let value = \"{}\"; }}{ending}fn last() {{}}",
                "文😀".repeat(16_384)
            );
            document.prepare(&source, 4, || true).1.unwrap();
            let painted = document.highlight_lines().unwrap();
            let original = painted[0]
                .iter()
                .find(|token| token.kind == TokenKind::String)
                .unwrap()
                .text
                .clone();
            {
                let paint = document.paint.borrow();
                let row = paint.rows.get(&0).unwrap();
                assert!(row.pieces.len() > 1);
                assert!(row.pieces.iter().any(|(piece, index)| {
                    piece.rows[*index]
                        .iter()
                        .any(|token| Arc::ptr_eq(&token.text, &original))
                }));
            }
            let changed = source.replace("fn last()", "fn changed()");
            document.prepare(&changed, 4, || true).1.unwrap();
            let next = document.highlight_lines().unwrap();
            let retained = next[0]
                .iter()
                .find(|token| token.kind == TokenKind::String)
                .unwrap();
            assert!(Arc::ptr_eq(&retained.text, &original));
            assert_eq!(
                next.iter()
                    .map(|row| row
                        .iter()
                        .map(|token| token.text.as_str())
                        .collect::<String>())
                    .collect::<Vec<_>>()
                    .join("\n"),
                changed
            );
        }
    }

    #[test]
    fn prepared_long_rows_keep_styles_and_failed_analysis_retains_fallback_limits() {
        let mut document = SyntaxDocument::new(Language::Rust).unwrap();
        let source = format!(
            "let s = \"{}\";\nfn next() {{}}",
            "x".repeat(crate::highlight::MAX_HIGHLIGHT_LINE_BYTES)
        );
        document.update(&source, || true);
        let painted = document.highlight_lines().unwrap();
        assert!(
            painted[0]
                .iter()
                .any(|token| token.kind == TokenKind::String
                    && token.text.len() > crate::highlight::MAX_HIGHLIGHT_LINE_BYTES)
        );
        assert_eq!(
            painted[0]
                .iter()
                .map(|token| token.text.as_str())
                .collect::<String>(),
            source.split('\n').next().unwrap()
        );
        let fallback = highlight_lines(&source, Language::Rust);
        assert_eq!(fallback[0].len(), 1);
        assert_eq!(fallback[0][0].kind, TokenKind::Plain);
        assert!(
            painted[1]
                .iter()
                .any(|token| token.kind == TokenKind::Function && token.text.as_str() == "next")
        );
        document.update("changed", || false);
        assert!(document.highlight_lines().is_none());
        document.update(&"x".repeat(crate::editor::MAX_STRUCTURE_BYTES + 1), || true);
        assert!(document.highlight_lines().is_none());
    }
}

#[cfg(test)]
mod parser_color_contracts {
    use super::*;
    fn highlight_lines(source: &str, language: Language) -> Vec<Vec<Token>> {
        let mut document = SyntaxDocument::new(language).expect("registered grammar");
        document.update(source, || true);
        document.highlight_lines().expect("prepared grammar colors")
    }
    fn rejoins(source: &str, language: Language) {
        assert_eq!(
            highlight_lines(source, language)
                .iter()
                .map(|row| row
                    .iter()
                    .map(|token| token.text.as_str())
                    .collect::<String>())
                .collect::<Vec<_>>()
                .join("\n"),
            source
        );
    }
    fn kinds(source: &str, language: Language) -> Vec<TokenKind> {
        highlight_lines(source, language)
            .into_iter()
            .flatten()
            .map(|token| token.kind)
            .collect()
    }
    #[test]
    fn rust_keywords_and_types() {
        let toks = highlight_lines("fn main() { let x = 5; }", Language::Rust);
        let flat: Vec<TokenKind> = toks.into_iter().flatten().map(|t| t.kind).collect();
        assert!(flat.contains(&TokenKind::Keyword)); // fn, let
        assert!(flat.contains(&TokenKind::Function)); // main
        assert!(flat.contains(&TokenKind::Number)); // 5
    }

    #[test]
    fn rust_string_and_escape() {
        let toks = highlight_lines(r#"let s = "a \"b\" c";"#, Language::Rust);
        let flat: Vec<(TokenKind, String)> = toks
            .into_iter()
            .flatten()
            .map(|t| (t.kind, t.text.as_str().to_owned()))
            .collect();
        assert!(flat.contains(&(TokenKind::String, r#""a \"b\" c""#.to_string())));
    }

    #[test]
    fn rust_line_comment_to_eol() {
        let toks = highlight_lines("let a = 1; // note", Language::Rust);
        let line = &toks[0];
        assert_eq!(line.last().unwrap().kind, TokenKind::Comment);
        assert_eq!(line.last().unwrap().text.as_str(), "// note");
    }

    #[test]
    fn rust_block_comment_spans_lines() {
        let source = "let a = 1; /* start\nstill comment */ let b = 2;";
        let toks = highlight_lines(source, Language::Rust);
        // Line 1 ends inside the block comment; line 2 finishes it.
        assert_eq!(toks[0].last().unwrap().kind, TokenKind::Comment);
        assert_eq!(toks[0].last().unwrap().text.as_str(), "/* start");
        assert_eq!(toks[1].first().unwrap().kind, TokenKind::Comment);
        assert_eq!(toks[1].first().unwrap().text.as_str(), "still comment */");
        rejoins(source, Language::Rust);
    }

    #[test]
    fn rust_char_and_lifetime() {
        let toks = highlight_lines("fn f<'a>(c: char) -> &'a str { 'x' }", Language::Rust);
        let flat: Vec<(TokenKind, String)> = toks
            .into_iter()
            .flatten()
            .map(|t| (t.kind, t.text.as_str().to_owned()))
            .collect();
        assert!(flat.contains(&(TokenKind::Lifetime, "'a".to_string())));
        assert!(flat.contains(&(TokenKind::Char, "'x'".to_string())));
    }

    #[test]
    fn rust_macro_and_attribute() {
        let toks = highlight_lines("#[derive(Debug)]\nprintln!(\"hi\");", Language::Rust);
        let flat: Vec<(TokenKind, String)> = toks
            .into_iter()
            .flatten()
            .map(|t| (t.kind, t.text.as_str().to_owned()))
            .collect();
        assert!(flat.contains(&(TokenKind::Attribute, "#".to_string())));
        assert!(flat.contains(&(TokenKind::Macro, "println".to_string())));
    }

    #[test]
    fn rust_numbers() {
        let toks = highlight_lines("let a = 0x1F; let b = 3.14; let c = 1e3;", Language::Rust);
        let nums: Vec<String> = toks
            .into_iter()
            .flatten()
            .filter(|t| t.kind == TokenKind::Number)
            .map(|t| t.text.as_str().to_owned())
            .collect();
        assert_eq!(nums, vec!["0x1F", "3.14", "1e3"]);
    }

    #[test]
    fn rust_range_is_not_a_float() {
        let toks = highlight_lines("for i in 1..5 {}", Language::Rust);
        let flat: Vec<(TokenKind, String)> = toks
            .into_iter()
            .flatten()
            .map(|t| (t.kind, t.text.as_str().to_owned()))
            .collect();
        assert!(flat.contains(&(TokenKind::Number, "1".to_string())));
        assert!(flat.contains(&(TokenKind::Number, "5".to_string())));
        // The `..` must not be swallowed into a number.
        assert!(
            !flat
                .iter()
                .any(|(k, t)| *k == TokenKind::Number && t.contains('.'))
        );
    }

    #[test]
    fn python_comment_and_keywords() {
        let k = kinds("def f():\n    return None  # done", Language::Python);
        assert!(k.contains(&TokenKind::Keyword));
        assert!(k.contains(&TokenKind::Boolean));
        assert!(k.contains(&TokenKind::Comment));
    }

    #[test]
    fn json_strings_numbers_booleans() {
        let k = kinds(r#"{"a": 1, "b": true, "c": null}"#, Language::Json);
        assert!(k.contains(&TokenKind::String));
        assert!(k.contains(&TokenKind::Number));
        assert!(k.contains(&TokenKind::Boolean));
    }
}

#[cfg(test)]
mod config_contracts {
    use super::*;

    #[test]
    fn configuration_grammars_preserve_source_folds_transfer_and_incremental_colors() {
        for &(path, fixture) in crate::editor::syntax_contracts::CONFIG_CASES {
            let language = crate::highlight::language_from_path(path);
            assert!(crate::editor::syntax_provider(language).is_some(), "{path}");
            for source in [fixture.to_owned(), fixture.replace('\n', "\r\n")] {
                let mut document = SyntaxDocument::new(language).unwrap();
                let (_, first) = document.prepare(&source, 4, || true);
                let first = first.expect(path);
                let colors = first.highlights().expect(path);
                assert!(
                    colors
                        .iter()
                        .flat_map(|row| row.iter())
                        .any(|token| token.kind != TokenKind::Plain),
                    "{path}"
                );
                assert_eq!(
                    colors
                        .iter()
                        .map(|row| row
                            .iter()
                            .map(|token| token.text.as_str())
                            .collect::<String>())
                        .collect::<Vec<_>>()
                        .join("\n"),
                    source,
                    "{path}"
                );
                assert!(
                    !first.folds().is_empty(),
                    "{path}: expected structural folding"
                );
                let transferred = first.transfer_data().unwrap().validate(&source).unwrap();
                assert_eq!(transferred.highlights(), first.highlights(), "{path}");
                assert_eq!(transferred.folds(), first.folds(), "{path}");
                for changed in [
                    format!("\n{source}"),
                    source.replace("文😀", "😀 changed"),
                    source.replace('"', ""),
                ] {
                    let (_, warm) = document.prepare(&changed, 4, || true);
                    let warm = warm.expect(path);
                    let mut fresh = SyntaxDocument::new(language).unwrap();
                    let cold = fresh.prepare(&changed, 4, || true).1.expect(path);
                    assert_eq!(warm.highlights(), cold.highlights(), "{path}");
                    assert_eq!(warm.folds(), cold.folds(), "{path}");
                    assert!(first.matches_source(&source));
                }
            }
        }
    }

    #[test]
    fn unknown_fence_languages_do_not_cancel_markdown_preparation() {
        for language in ["sql", "unknown", "markdown"] {
            let source = format!("# Heading\n\n```{language}\nSELECT 'prose'; // text\n```\n");
            let mut document = SyntaxDocument::new(Language::Markdown).unwrap();
            let prepared = document.prepare(&source, 4, || true).1.unwrap();
            let colors = prepared.highlights().unwrap();
            assert!(colors[3].iter().all(|token| token.kind == TokenKind::Plain));
            assert_eq!(
                colors
                    .iter()
                    .map(|row| row
                        .iter()
                        .map(|token| token.text.as_str())
                        .collect::<String>())
                    .collect::<Vec<_>>()
                    .join("\n"),
                source
            );
        }
    }

    #[test]
    fn markdown_prose_stays_plain_and_fenced_code_uses_its_declared_grammar() {
        let source = "# Heading\n\nCopyright (c) 2026 AS IS.\n\n```rust\nfn main() { let value = 42; }\n```\n";
        let mut document = SyntaxDocument::new(Language::Markdown).unwrap();
        let (status, prepared) = document.prepare(source, 4, || true);
        let prepared = prepared.unwrap_or_else(|| panic!("markdown analysis: {status:?}"));
        let colors = prepared
            .highlights()
            .unwrap_or_else(|| panic!("markdown colors: {status:?}"));
        assert!(colors[2].iter().all(|token| token.kind == TokenKind::Plain));
        assert!(
            colors[5]
                .iter()
                .any(|token| token.text.as_str() == "fn" && token.kind == TokenKind::Keyword)
        );
        assert!(
            colors[5]
                .iter()
                .any(|token| token.text.as_str() == "main" && token.kind == TokenKind::Function)
        );
        assert!(
            colors[5]
                .iter()
                .any(|token| token.text.as_str() == "42" && token.kind == TokenKind::Number)
        );
    }
}
