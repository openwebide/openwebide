//! Source-bound private grammar paint. Drivers choose when to yield; policy is shared.
use super::*;

const PAINT_BATCH_BYTES: usize = 8 * 1024;
const PAINT_BATCH_RECORDS: usize = 256;

/// Ordered endpoint cursor without allocating a complete segment table.
#[derive(Default)]
struct Segments {
    protected: usize,
    scopes: usize,
    semantic: usize,
    start: usize,
}
impl Segments {
    fn next(
        &mut self,
        context: &crate::editor::Structure,
        semantic: &[(Range<usize>, TokenKind)],
        length: usize,
    ) -> Option<Range<usize>> {
        let endpoint = |range: &Range<usize>, index: usize| {
            if index.is_multiple_of(2) {
                range.start
            } else {
                range.end
            }
        };
        loop {
            let protected = context
                .protected
                .get(self.protected / 2)
                .map(|(r, _, _)| endpoint(r, self.protected));
            let scopes = context
                .scopes
                .get(self.scopes / 2)
                .map(|(r, _)| endpoint(r, self.scopes));
            let selected = semantic
                .get(self.semantic / 2)
                .map(|(r, _)| endpoint(r, self.semantic));
            let end = [
                protected,
                scopes,
                selected,
                (self.start < length).then_some(length),
            ]
            .into_iter()
            .flatten()
            .min()?;
            if protected == Some(end) {
                self.protected += 1;
            }
            if scopes == Some(end) {
                self.scopes += 1;
            }
            if selected == Some(end) {
                self.semantic += 1;
            }
            if end == self.start {
                continue;
            }
            return Some(std::mem::replace(&mut self.start, end)..end);
        }
    }
}

struct PiecePreparation {
    start: usize,
    end: usize,
    offset: usize,
    language: Language,
    kind: Option<TokenKind>,
    text: String,
    rows: TokenRows,
    trailing_empty: bool,
}
impl PiecePreparation {
    fn advance(&mut self, source: &str, bytes: usize, mut records: usize) -> bool {
        let limit = source.floor_char_boundary(self.end.min(self.offset.saturating_add(bytes)));
        while records > 0 {
            if self.trailing_empty {
                self.rows.push(share_token_row(Vec::new()));
                self.trailing_empty = false;
                return true;
            }
            let newline = source[self.offset..limit].find('\n');
            let end = newline.map_or(limit, |offset| self.offset + offset);
            self.text.push_str(&source[self.offset..end]);
            self.offset = end;
            if newline.is_none() && end < self.end {
                return false;
            }
            let text = std::mem::take(&mut self.text);
            self.rows.push(if text.is_empty() {
                share_token_row(Vec::new())
            } else {
                share_token_row(vec![Token {
                    kind: self.kind.unwrap_or(TokenKind::Plain),
                    text: text.into(),
                }])
            });
            records -= 1;
            if newline.is_none() {
                return true;
            }
            self.offset += 1;
            if self.offset == self.end {
                self.trailing_empty = true;
            }
            if self.offset == limit && !self.trailing_empty {
                return false;
            }
        }
        false
    }
    fn finish(self) -> (usize, Piece) {
        (
            self.start,
            Piece {
                end: self.end,
                paint: Arc::new(PaintedPiece {
                    language: self.language,
                    kind: self.kind,
                    rows: self.rows,
                }),
            },
        )
    }
}

pub(in crate::editor::syntax) struct PaintPreparation {
    source: Arc<String>,
    context: Arc<crate::editor::Structure>,
    semantic: Vec<(Range<usize>, TokenKind)>,
    segments: Segments,
    previous: SyntaxPaint,
    edit: super::super::InputEdit,
    protected: usize,
    selected: usize,
    pieces: HashMap<usize, Piece>,
    rows: HashMap<usize, PaintedRow>,
    tokens: TokenRows,
    row: usize,
    current: Option<RowPaint>,
    assembly: Option<RowAssembly>,
    preparing: Option<PiecePreparation>,
    piece: Option<(usize, Piece, usize, bool)>,
    ended: bool,
    #[cfg(test)]
    repainted_pieces: usize,
    #[cfg(test)]
    reused_source_change: bool,
}
impl PaintPreparation {
    #[cfg(test)]
    pub(in crate::editor::syntax) fn diagnostic(&self) -> String {
        format!(
            "row={}, segment={}, pieces={}, tokens={}, copying={:?}",
            self.row,
            self.segments.start,
            self.pieces.len(),
            self.tokens.len(),
            self.preparing
                .as_ref()
                .map(|piece| (piece.offset, piece.end))
        )
    }

    pub(super) fn new(
        document: &SyntaxDocument,
        context: Arc<crate::editor::Structure>,
    ) -> Option<Self> {
        let semantic = document.paint_semantics(&context)?;
        let previous = std::mem::take(&mut *document.paint.borrow_mut());
        let edit = if Arc::ptr_eq(&previous.source, &document.text) {
            super::super::input_edit("", "", None).ok()?
        } else if let Some(edit) = previous.source_change {
            edit
        } else {
            super::super::input_edit(&previous.source, &document.text, None).ok()?
        };
        let mut work = Self {
            source: document.text.clone(),
            context,
            semantic,
            segments: Segments::default(),
            #[cfg(test)]
            reused_source_change: previous.source_change.is_some(),
            previous,
            edit,
            protected: 0,
            selected: 0,
            pieces: HashMap::new(),
            rows: HashMap::new(),
            tokens: Vec::new(),
            row: 0,
            current: None,
            assembly: None,
            preparing: None,
            piece: None,
            ended: false,
            #[cfg(test)]
            repainted_pieces: 0,
        };
        work.current = Some(RowPaint::new(
            document.source_lines.first()?,
            &work.source,
            &mut work.previous.rows,
            &work.edit,
        ));
        Some(work)
    }
    fn next_piece(&mut self, range: Range<usize>) {
        let (start, end) = (range.start, range.end);
        while self
            .context
            .protected
            .get(self.protected)
            .is_some_and(|(r, _, _)| r.end <= start)
        {
            self.protected += 1;
        }
        while self
            .semantic
            .get(self.selected)
            .is_some_and(|(r, _)| r.end <= start)
        {
            self.selected += 1;
        }
        let language = self
            .context
            .language_at(self.source.floor_char_boundary(end - 1));
        let literal = self
            .context
            .protected
            .get(self.protected)
            .filter(|(r, _, _)| r.contains(&start));
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
                    ) && (self.source[start..end].starts_with('\'')
                        || self.source[start..end].starts_with("b'")) =>
                {
                    TokenKind::Char
                }
                _ => TokenKind::String,
            })
            .or_else(|| {
                self.semantic
                    .get(self.selected)
                    .filter(|(r, _)| r.contains(&start))
                    .map(|(_, kind)| *kind)
            });
        let retained = previous_range(start..end, &self.edit).and_then(|r| {
            self.previous
                .pieces
                .get(&r.start)
                .filter(|p| p.end == r.end && p.paint.language == language && p.paint.kind == kind)
                .map(|p| p.paint.clone())
        });
        if let Some(paint) = retained {
            self.piece = Some((start, Piece { end, paint }, 0, false));
        } else {
            #[cfg(test)]
            {
                self.repainted_pieces += 1;
            }
            self.preparing = Some(PiecePreparation {
                start,
                end,
                offset: start,
                language,
                kind,
                text: String::new(),
                rows: Vec::new(),
                trailing_empty: false,
            });
        }
    }
    /// Independently cap copied source bytes, constructed piece rows, copied
    /// token owners, row publications and segment/piece-row visits. Allocation/publication and initial
    /// semantic collection remain separate work.
    pub(super) fn advance(&mut self, document: &SyntaxDocument) -> Option<bool> {
        let mut bytes = PAINT_BATCH_BYTES;
        let mut records = PAINT_BATCH_RECORDS;
        if !Arc::ptr_eq(&self.source, &document.text) {
            return None;
        }
        let mut token_records = records;
        let mut source_rows = records;
        let mut row_publications = records;
        while records > 0
            && bytes > 0
            && token_records > 0
            && source_rows > 0
            && row_publications > 0
        {
            if let Some(piece) = &mut self.preparing {
                let old_offset = piece.offset;
                let old_rows = piece.rows.len();
                let done = piece.advance(&self.source, bytes, source_rows);
                bytes -= piece.offset - old_offset;
                source_rows -= piece.rows.len() - old_rows;
                if done {
                    let (start, piece) = self.preparing.take()?.finish();
                    self.piece = Some((start, piece, 0, false));
                }
                if !done {
                    return Some(false);
                }
                continue;
            }
            if let Some(row) = &mut self.assembly {
                let old_count = row.tokens.len();
                let done = row.advance(token_records);
                token_records -= row.tokens.len() - old_count;
                if !done || token_records == 0 {
                    return Some(false);
                }
                let painted = self.assembly.take()?.finish();
                self.tokens.push(painted.tokens.clone());
                self.rows
                    .insert(document.source_lines.get(self.row)?.start, painted);
                row_publications -= 1;
                if self.ended {
                    return Some(true);
                }
                self.row += 1;
                self.current = Some(RowPaint::new(
                    document.source_lines.get(self.row)?,
                    &self.source,
                    &mut self.previous.rows,
                    &self.edit,
                ));
                continue;
            }
            if let Some((_, piece, index, started)) = &mut self.piece {
                if *index > 0 && !*started {
                    self.assembly = Some(RowAssembly::new(
                        self.current.take()?,
                        document.source_lines.get(self.row)?,
                        &self.source,
                    ));
                    *started = true;
                } else {
                    if !piece.paint.rows[*index].is_empty() {
                        self.current.as_mut()?.append(&piece.paint, *index);
                    }
                    *index += 1;
                    *started = false;
                    if *index == piece.paint.rows.len() {
                        let (start, piece, _, _) = self.piece.take()?;
                        self.pieces.insert(start, piece);
                    }
                    records -= 1;
                }
                continue;
            }
            if let Some(range) =
                self.segments
                    .next(&self.context, &self.semantic, self.source.len())
            {
                self.next_piece(range);
                records -= 1;
                continue;
            }
            if self.row + 1 != document.source_lines.len() {
                return None;
            }
            self.ended = true;
            self.assembly = Some(RowAssembly::new(
                self.current.take()?,
                document.source_lines.get(self.row)?,
                &self.source,
            ));
            records -= 1;
        }
        Some(false)
    }
    pub(super) fn finish(self) -> Option<(SyntaxPaint, TokenRows)> {
        if !self.ended || self.assembly.is_some() {
            return None;
        }
        Some((
            SyntaxPaint {
                source: self.source,
                source_change: None,
                pieces: self.pieces,
                rows: self.rows,
                #[cfg(test)]
                repainted_pieces: self.repainted_pieces,
                #[cfg(test)]
                reused_source_change: self.reused_source_change,
            },
            self.tokens,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn piece_copying_respects_utf8_byte_batches_and_empty_line_shapes() {
        for ending in ["\n", "\r\n"] {
            let source = format!("{}{ending}{ending}tail{ending}", "文😀".repeat(16_384));
            for kind in [None, Some(TokenKind::String)] {
                let mut work = PiecePreparation {
                    start: 0,
                    end: source.len(),
                    offset: 0,
                    language: Language::Rust,
                    kind,
                    text: String::new(),
                    rows: Vec::new(),
                    trailing_empty: false,
                };
                let mut steps = 0;
                loop {
                    let before = work.offset;
                    let done = work.advance(&source, 31, 64);
                    assert!(work.offset - before <= 31);
                    assert!(source.is_char_boundary(work.offset));
                    steps += 1;
                    if done {
                        break;
                    }
                }
                assert!(steps > 1000);
                let (_, piece) = work.finish();
                assert_eq!(
                    piece
                        .paint
                        .rows
                        .iter()
                        .map(|row| row
                            .iter()
                            .map(|token| token.text.as_str())
                            .collect::<String>())
                        .collect::<Vec<_>>()
                        .join("\n"),
                    source
                );
                assert!(
                    piece
                        .paint
                        .rows
                        .iter()
                        .flat_map(|row| row.iter())
                        .all(|token| token.kind == kind.unwrap_or(TokenKind::Plain))
                );
                assert!(piece.paint.rows.last().unwrap().is_empty());
            }
        }
    }

    #[test]
    fn endpoint_cursor_matches_independent_segment_merge_for_embedded_contexts() {
        for (language, source) in [
            (
                Language::Rust,
                "fn f() { let s = \"文😀\"; /* x */ call(); }\n".repeat(100),
            ),
            (
                Language::Html,
                "<script>const s='文😀';</script><style>a { color:red; }</style>\n".repeat(100),
            ),
            (
                Language::Markdown,
                "# A\n```rust\nfn f() { call(); }\n```\nText **strong**.\n".repeat(100),
            ),
        ] {
            let mut document = SyntaxDocument::new(language).unwrap();
            document.update(&source, || true);
            let context = document.structure().unwrap();
            let semantic = document.paint_semantics(&context).unwrap();
            let expected = paint_segments(
                context
                    .protected
                    .iter()
                    .flat_map(|(r, _, _)| [r.start, r.end]),
                context.scopes.iter().flat_map(|(r, _)| [r.start, r.end]),
                semantic.iter().flat_map(|(r, _)| [r.start, r.end]),
                source.len(),
            )
            .collect::<Vec<_>>();
            let mut cursor = Segments::default();
            let actual = std::iter::from_fn(|| cursor.next(&context, &semantic, source.len()))
                .collect::<Vec<_>>();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn mixed_rows_clone_bounded_token_owners_without_changing_styles_or_source() {
        let tokens = (0..2048)
            .map(|index| Token {
                kind: if index % 2 == 0 {
                    TokenKind::Keyword
                } else {
                    TokenKind::String
                },
                text: Arc::new("文😀".into()),
            })
            .collect::<Vec<_>>();
        let source = tokens
            .iter()
            .map(|token| token.text.as_str())
            .collect::<String>();
        let first = Arc::new(PaintedPiece {
            language: Language::Rust,
            kind: None,
            rows: vec![share_token_row(tokens[..1024].to_vec())],
        });
        let second = Arc::new(PaintedPiece {
            language: Language::Rust,
            kind: None,
            rows: vec![share_token_row(tokens[1024..].to_vec())],
        });
        let row = crate::editor::lines::lines(&source)[0].clone();
        let mut paint = RowPaint {
            previous: None,
            parts: Vec::new(),
            matched: 0,
            changed: false,
        };
        paint.append(&first, 0);
        paint.append(&second, 0);
        let mut work = RowAssembly::new(paint, &row, &source);
        let mut steps = 0;
        loop {
            let before = work.tokens.len();
            let done = work.advance(64);
            assert!(work.tokens.len() - before <= 64);
            steps += 1;
            if done {
                break;
            }
        }
        assert_eq!(steps, 32);
        let result = work.finish();
        assert_eq!(result.tokens.as_ref(), tokens);
        assert_eq!(
            result
                .tokens
                .iter()
                .map(|token| token.text.as_str())
                .collect::<String>(),
            source
        );
        assert!(
            result
                .tokens
                .iter()
                .zip(&tokens)
                .all(|(a, b)| Arc::ptr_eq(&a.text, &b.text))
        );
    }

    #[test]
    fn yielded_paint_is_private_and_cancellation_discards_it() {
        let source = Arc::new(format!(
            "fn f() {{ let s = \"{}\"; }}",
            "文😀".repeat(16_384)
        ));
        let mut document = SyntaxDocument::new(Language::Rust).unwrap();
        let mut steps = 0;
        loop {
            let mut calls = 0;
            let result = document.prepare_cooperative(
                source.clone(),
                4,
                || true,
                || {
                    calls += 1;
                    calls > 2
                },
            );
            assert!(result.is_none());
            assert!(document.prepared.is_none());
            steps += 1;
            if document.paint_progress.as_ref().is_some_and(|work| {
                work.preparing
                    .as_ref()
                    .is_some_and(|piece| piece.text.len() >= 8192)
            }) {
                break;
            }
            assert!(steps < 10_000);
        }
        let (status, analysis) = document
            .prepare_cooperative(source, 4, || false, || false)
            .unwrap();
        assert_eq!(status, super::super::super::SyntaxStatus::Cancelled);
        assert!(analysis.is_none());
        assert!(document.paint_progress.is_none());
        let changed = "fn changed() { call(); }";
        let (_, analysis) = document.prepare(changed, 4, || true);
        let analysis = analysis.unwrap();
        assert_eq!(analysis.source_snapshot().as_str(), changed);
        assert_eq!(
            analysis.highlights().unwrap()[0]
                .iter()
                .map(|token| token.text.as_str())
                .collect::<String>(),
            changed
        );
    }
}
