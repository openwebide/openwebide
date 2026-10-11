//! A folded edit view is a projection of the source, never a second document.
use std::{ops::Range, sync::Arc};

use super::{FoldState, Selection};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibleLine {
    /// Zero-based logical source line, independent of folded view rows.
    pub source_line: usize,
    pub source: Range<usize>,
    pub visible_start: usize,
    /// UTF-16 offset in the native textarea, whose CRLF endings normalize to LF.
    pub textarea_start: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HiddenText {
    source: Range<usize>,
    visible_offset: usize,
    header_end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionError {
    InvalidOffset,
    /// Expand the fold before replaying an edit that crosses its hidden gap.
    HiddenText,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoldProjection {
    text: Arc<String>,
    identity: Arc<()>,
    textarea_text: Arc<String>,
    source_len: usize,
    source_range: Range<usize>,
    textarea_origin: usize,
    lines: Arc<Vec<VisibleLine>>,
    hidden: Arc<[HiddenText]>,
    uniform_rows: bool,
    coordinates: Arc<Vec<super::coordinates::LineCoordinates>>,
}

// Bound native shaping and coordinate copies independently of byte density.
// Window mapping may retain one empty endpoint row at its exclusive boundary.
const INPUT_CONTEXT_ROWS: usize = 128;

// Folded/windowed tables own only visible rows; do not retain spare growth
// capacity when publishing their immutable coordinate storage.
fn shared_coordinates(
    mut coordinates: Vec<super::coordinates::LineCoordinates>,
) -> Arc<Vec<super::coordinates::LineCoordinates>> {
    coordinates.shrink_to_fit();
    Arc::new(coordinates)
}

fn shared_visible_rows(mut rows: Vec<VisibleLine>) -> Arc<Vec<VisibleLine>> {
    rows.shrink_to_fit();
    Arc::new(rows)
}

impl FoldProjection {
    pub fn new(source: &str, folds: &FoldState) -> Self {
        Self::indexed(source, folds, &super::index::LineIndex::new(source))
    }

    pub(super) fn indexed(
        source: &str,
        folds: &FoldState,
        index: &super::index::LineIndex,
    ) -> Self {
        Self::build(source, folds, index, None, Arc::new(()))
    }

    pub(super) fn for_document(
        source: &Arc<String>,
        identity: &Arc<()>,
        folds: &FoldState,
        index: &super::index::LineIndex,
    ) -> Self {
        let shared = folds
            .ranges()
            .iter()
            .all(|range| folds.collapsed_at(range.start_line).is_none())
            .then(|| source.clone());
        Self::build(source, folds, index, shared, identity.clone())
    }

    fn build(
        source: &str,
        folds: &FoldState,
        index: &super::index::LineIndex,
        shared: Option<Arc<String>>,
        identity: Arc<()>,
    ) -> Self {
        let shared_lines = shared.as_ref().map(|_| index.visible_rows());
        let logical = &index.rows;
        let mut endings = index.line_endings(
            0..if shared_lines.is_some() {
                logical.len()
            } else {
                0
            },
        );
        let mut visible_len = 0;
        let mut visible = Vec::new();
        let mut coordinates = shared.is_none().then(Vec::new);
        let mut hidden: Vec<HiddenText> = Vec::new();
        let mut row = 0;
        let mut textarea_start = 0;
        while shared_lines.is_none() && row < logical.len() {
            let line = &logical[row];
            let row_endings = index.line_endings(row..row + 1);
            endings.carriage_returns |= row_endings.carriage_returns;
            endings.uniform_rows &= row_endings.uniform_rows;
            visible.push(VisibleLine {
                source_line: row,
                source: line.start..line.end,
                visible_start: visible_len,
                textarea_start,
            });
            if let Some(coordinates) = &mut coordinates {
                coordinates.push(index.coordinates[row].clone());
            }
            visible_len += line.end - line.start;
            textarea_start += index.native_line_len(row);
            if let Some(range) = folds.collapsed_at(row) {
                let end = range.end_line.min(logical.len() - 1);
                if end > row {
                    let omitted = logical[row + 1].start..logical[end].end;
                    if !omitted.is_empty() {
                        hidden.push(HiddenText {
                            source: omitted,
                            visible_offset: visible_len,
                            header_end: line.body_end,
                        });
                    }
                    row = end;
                }
            }
            row += 1;
        }
        let text = shared.unwrap_or_else(|| {
            let mut text = String::with_capacity(visible_len);
            for line in &visible {
                text.push_str(&source[line.source.clone()]);
            }
            Arc::new(text)
        });
        // A trailing newline always creates a logical empty input row. Keep its
        // source identity even when a provider included it in a fold's range.
        if shared_lines.is_none()
            && text.ends_with('\n')
            && visible
                .last()
                .is_some_and(|line| line.source.start != source.len())
        {
            if let Some(coordinates) = &mut coordinates {
                coordinates.push(index.coordinates[logical.len() - 1].clone());
            }
            visible.push(VisibleLine {
                source_line: logical.len() - 1,
                source: source.len()..source.len(),
                visible_start: text.len(),
                textarea_start,
            });
        }
        // Indexed display breaks include lone CR; hidden rows do not affect
        // the visible view's measured-height or normalization requirements.
        let uniform_rows = endings.uniform_rows;
        let textarea_text = if endings.carriage_returns {
            Arc::new(super::native::textarea_text(&text))
        } else {
            text.clone()
        };
        Self {
            text,
            identity,
            textarea_text,
            uniform_rows,
            coordinates: coordinates.map_or_else(|| index.coordinates.clone(), shared_coordinates),
            source_len: source.len(),
            source_range: 0..source.len(),
            textarea_origin: 0,
            lines: shared_lines.unwrap_or_else(|| shared_visible_rows(visible)),
            hidden: hidden.into(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }
    /// Exact immutable text and document-version provenance, including empty text.
    pub fn shares_text_version(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.text, &other.text) && Arc::ptr_eq(&self.identity, &other.identity)
    }
    pub fn textarea_text(&self) -> &str {
        &self.textarea_text
    }
    /// The native offsets in a context are local; this is its offset in the
    /// complete projected input. Source offsets always remain document offsets.
    pub fn textarea_origin(&self) -> usize {
        self.textarea_origin
    }

    pub fn source_range(&self) -> Range<usize> {
        self.source_range.clone()
    }

    pub fn is_windowed(&self) -> bool {
        self.source_range != (0..self.source_len)
    }

    /// Retain only this visible byte interval, including partial logical rows.
    /// A context is not a new document: folds and source row identities survive,
    /// while byte and native coordinates start at zero within the context.
    pub fn window(&self, range: Range<usize>) -> Result<Self, ProjectionError> {
        let between_crlf = |at: usize| {
            at > 0
                && self.text.as_bytes().get(at - 1) == Some(&b'\r')
                && self.text.as_bytes().get(at) == Some(&b'\n')
        };
        if range.start > range.end
            || range.end > self.text.len()
            || !self.text.is_char_boundary(range.start)
            || !self.text.is_char_boundary(range.end)
            || between_crlf(range.start)
            || between_crlf(range.end)
        {
            return Err(ProjectionError::InvalidOffset);
        }
        if range == (0..self.text.len()) {
            return Ok(self.clone());
        }
        let source_range = self.source_offset(range.start)?..self.source_offset(range.end)?;
        let textarea_origin = self.textarea_origin
            + self
                .byte_to_textarea(range.start)
                .map_err(|_| ProjectionError::InvalidOffset)?;
        let first = self
            .lines
            .partition_point(|line| line.visible_start <= range.start)
            .saturating_sub(1);
        let mut lines = Vec::new();
        let mut coordinates = Vec::new();
        let mut native = 0;
        for row in first..self.lines.len() {
            let line = &self.lines[row];
            if line.visible_start > range.end {
                break;
            }
            let end = self
                .lines
                .get(row + 1)
                .map_or(self.text.len(), |next| next.visible_start);
            let start = line.visible_start.max(range.start);
            let end = end.min(range.end);
            let slice = &self.text[start..end];
            let source_start = line.source.start + start - line.visible_start;
            lines.push(VisibleLine {
                source_line: line.source_line,
                source: source_start..source_start + slice.len(),
                visible_start: start - range.start,
                textarea_start: native,
            });
            coordinates.push(
                if start == line.visible_start && end - start == line.source.len() {
                    self.coordinates[row].clone()
                } else {
                    super::coordinates::LineCoordinates::new(slice)
                },
            );
            native += super::byte_to_textarea(slice, slice.len())
                .map_err(|_| ProjectionError::InvalidOffset)?;
        }
        let text = Arc::new(self.text[range.clone()].to_string());
        let textarea_text = if text.contains('\r') {
            Arc::new(super::native::textarea_text(&text))
        } else {
            text.clone()
        };
        let uniform_rows = !text.as_bytes().iter().enumerate().any(|(offset, byte)| {
            *byte == b'\r' && text.as_bytes().get(offset + 1) != Some(&b'\n')
        });
        let first_gap = self
            .hidden
            .partition_point(|gap| gap.visible_offset <= range.start);
        let last_gap = self
            .hidden
            .partition_point(|gap| gap.visible_offset <= range.end);
        let hidden = self.hidden[first_gap..last_gap]
            .iter()
            .map(|gap| HiddenText {
                source: gap.source.clone(),
                visible_offset: gap.visible_offset - range.start,
                header_end: gap.header_end,
            })
            .collect::<Vec<_>>();
        Ok(Self {
            text,
            identity: self.identity.clone(),
            textarea_text,
            source_len: self.source_len,
            source_range,
            textarea_origin,
            lines: shared_visible_rows(lines),
            hidden: hidden.into(),
            uniform_rows,
            coordinates: shared_coordinates(coordinates),
        })
    }

    /// Bound the browser's surrounding text around the primary selection head.
    /// Large selections remain in the document and are clipped only for native
    /// input; callers must retain the original selection for editing/clipboard.
    pub fn input_context(
        &self,
        selection: Selection,
        max_bytes: usize,
    ) -> Result<Self, ProjectionError> {
        if max_bytes < 4 {
            return Err(ProjectionError::InvalidOffset);
        }
        let head = self.visible_selection(selection)?.head;
        let head = if head > 0
            && self.text.as_bytes().get(head - 1) == Some(&b'\r')
            && self.text.as_bytes().get(head) == Some(&b'\n')
        {
            head - 1
        } else {
            head
        };
        let mut start = head.saturating_sub(max_bytes / 2);
        let mut end = start.saturating_add(max_bytes).min(self.text.len());
        start = end.saturating_sub(max_bytes);
        let head_row = self
            .lines
            .partition_point(|line| line.visible_start <= head)
            .saturating_sub(1);
        let last_row = head_row
            .saturating_sub(INPUT_CONTEXT_ROWS / 2)
            .saturating_add(INPUT_CONTEXT_ROWS)
            .min(self.lines.len());
        let first_row = last_row.saturating_sub(INPUT_CONTEXT_ROWS);
        start = start.max(
            self.lines
                .get(first_row)
                .ok_or(ProjectionError::InvalidOffset)?
                .visible_start,
        );
        end = end.min(
            self.lines
                .get(last_row)
                .map_or(self.text.len(), |line| line.visible_start),
        );
        while !self.text.is_char_boundary(start) {
            start += 1;
        }
        while !self.text.is_char_boundary(end) {
            end -= 1;
        }
        if start > 0
            && self.text.as_bytes().get(start - 1) == Some(&b'\r')
            && self.text.as_bytes().get(start) == Some(&b'\n')
        {
            start += 1;
        }
        if end > 0
            && self.text.as_bytes().get(end - 1) == Some(&b'\r')
            && self.text.as_bytes().get(end) == Some(&b'\n')
        {
            end -= 1;
        }
        self.window(start..end)
    }

    /// Whether surrounding input text can be retained after an ordinary edit.
    /// A window may include one empty row at its trailing endpoint.
    pub fn fits_input_context(&self, max_bytes: usize) -> bool {
        self.text.len() <= max_bytes && self.lines.len() <= INPUT_CONTEXT_ROWS + 1
    }

    /// Selection for the native context, without changing document selections.
    pub fn input_selection(&self, selection: Selection) -> Result<Selection, ProjectionError> {
        if selection.anchor > self.source_len || selection.head > self.source_len {
            return Err(ProjectionError::InvalidOffset);
        }
        self.visible_selection(Selection {
            anchor: selection
                .anchor
                .clamp(self.source_range.start, self.source_range.end),
            head: selection
                .head
                .clamp(self.source_range.start, self.source_range.end),
        })
    }
    pub fn visual_line_index(&self, row: usize) -> Option<super::VisualLineIndex> {
        self.coordinates.get(row)?.visual()
    }
    pub fn textarea_to_byte(&self, offset: usize) -> usize {
        let row = self
            .lines
            .partition_point(|line| line.textarea_start <= offset)
            .saturating_sub(1);
        let line = &self.lines[row];
        let end = self
            .lines
            .get(row + 1)
            .map_or(self.text.len(), |next| next.visible_start);
        line.visible_start
            + self.coordinates[row].textarea_to_byte(
                &self.text[line.visible_start..end],
                offset.saturating_sub(line.textarea_start),
            )
    }
    pub fn byte_to_textarea(&self, offset: usize) -> Result<usize, super::EditError> {
        if offset > self.text.len() || !self.text.is_char_boundary(offset) {
            return Err(super::EditError::InvalidSelection);
        }
        let row = self
            .lines
            .partition_point(|line| line.visible_start <= offset)
            .saturating_sub(1);
        let line = &self.lines[row];
        let end = self
            .lines
            .get(row + 1)
            .map_or(self.text.len(), |next| next.visible_start);
        Ok(line.textarea_start
            + self.coordinates[row].byte_to_textarea(
                &self.text[line.visible_start..end],
                offset - line.visible_start,
            )?)
    }
    pub fn source_native_selection(
        &self,
        selection: Selection,
    ) -> Result<Selection, ProjectionError> {
        self.source_selection(Selection {
            anchor: self.textarea_to_byte(selection.anchor),
            head: self.textarea_to_byte(selection.head),
        })
    }
    pub fn lines(&self) -> &[VisibleLine] {
        &self.lines
    }
    /// Borrow a visible logical row body without allocating normalized source.
    pub fn line_body(&self, row: usize) -> Option<&str> {
        let line = self.lines.get(row)?;
        let text = self
            .text
            .get(line.visible_start..line.visible_start + line.source.len())?;
        Some(
            text.strip_suffix("\r\n")
                .or_else(|| text.strip_suffix('\n'))
                .unwrap_or(text),
        )
    }

    pub fn has_uniform_rows(&self) -> bool {
        self.uniform_rows
    }

    pub fn is_folded(&self) -> bool {
        !self.hidden.is_empty()
    }

    fn hidden_at(&self, source: usize) -> Option<&HiddenText> {
        let index = self
            .hidden
            .partition_point(|gap| gap.source.start <= source)
            .checked_sub(1)?;
        self.hidden
            .get(index)
            .filter(|gap| gap.source.contains(&source))
    }

    /// At a hidden gap, a caret belongs to the next visible source row.
    pub fn source_offset(&self, visible: usize) -> Result<usize, ProjectionError> {
        if visible > self.text.len() || !self.text.is_char_boundary(visible) {
            return Err(ProjectionError::InvalidOffset);
        }
        if visible == self.text.len() {
            return Ok(self.source_range.end);
        }
        let line = self
            .lines
            .partition_point(|line| line.visible_start <= visible)
            .saturating_sub(1);
        let line = &self.lines[line];
        Ok(line.source.start + visible - line.visible_start)
    }

    pub fn visible_offset(&self, source: usize) -> Result<usize, ProjectionError> {
        if source < self.source_range.start || source > self.source_range.end {
            return Err(ProjectionError::InvalidOffset);
        }
        if source == self.source_range.end {
            return Ok(self.text.len());
        }
        if self.hidden_at(source).is_some() {
            return Err(ProjectionError::HiddenText);
        }
        let line = self
            .lines
            .partition_point(|line| line.source.start <= source)
            .saturating_sub(1);
        let line = &self.lines[line];
        let visible = line.visible_start + source - line.source.start;
        if visible > self.text.len() || !self.text.is_char_boundary(visible) {
            return Err(ProjectionError::InvalidOffset);
        }
        Ok(visible)
    }

    pub fn source_selection(&self, visible: Selection) -> Result<Selection, ProjectionError> {
        Ok(Selection {
            anchor: self.source_offset(visible.anchor)?,
            head: self.source_offset(visible.head)?,
        })
    }

    /// Collapsing a selection endpoint moves it to its containing visible header.
    pub fn visible_selection(&self, source: Selection) -> Result<Selection, ProjectionError> {
        let endpoint = |position| match self.visible_offset(position) {
            Err(ProjectionError::HiddenText) => {
                let gap = self.hidden_at(position).unwrap();
                self.visible_offset(
                    gap.header_end
                        .clamp(self.source_range.start, self.source_range.end),
                )
            }
            result => result,
        };
        Ok(Selection {
            anchor: endpoint(source.anchor)?,
            head: endpoint(source.head)?,
        })
    }

    /// Map a projected native value to one source replacement. Unchanged source
    /// stays borrowed; only the inserted text is allocated. Selection offsets
    /// address the resulting source, preserving its CRLF endings.
    pub fn replay_edit(
        &self,
        source: &str,
        value: &str,
        selection: Selection,
        input_type: &str,
        before_selection: Selection,
    ) -> Result<(Option<super::Edit>, Selection), ProjectionError> {
        if source.len() != self.source_len
            || before_selection.range().end > source.len()
            || !source.is_char_boundary(before_selection.anchor)
            || !source.is_char_boundary(before_selection.head)
        {
            return Err(ProjectionError::InvalidOffset);
        }
        let before = self.textarea_text();
        let map = |offset| {
            let utf16 =
                super::byte_to_utf16(before, offset).map_err(|_| ProjectionError::InvalidOffset)?;
            let source_offset = self.source_offset(self.textarea_to_byte(utf16))?;
            if !source.is_char_boundary(source_offset) {
                return Err(ProjectionError::InvalidOffset);
            }
            Ok(source_offset)
        };
        let selected_context = self.selected_context_change(value, input_type, before_selection)?;
        let change = selected_context
            .as_ref()
            .map(|(change, _)| change.clone())
            .or_else(|| super::text_change(before, value));
        let Some(change) = change else {
            return Ok((
                None,
                Selection {
                    anchor: map(selection.anchor)?,
                    head: map(selection.head)?,
                },
            ));
        };
        let raw_start = self.textarea_to_byte(
            super::byte_to_utf16(before, change.range.start)
                .map_err(|_| ProjectionError::InvalidOffset)?,
        );
        let raw_end = self.textarea_to_byte(
            super::byte_to_utf16(before, change.range.end)
                .map_err(|_| ProjectionError::InvalidOffset)?,
        );
        let crosses =
            self.source_edit_range(raw_start..raw_end) == Err(ProjectionError::HiddenText);
        let mut range = selected_context.map_or_else(
            || Ok(map(change.range.start)?..map(change.range.end)?),
            |(_, range)| Ok(range),
        )?;
        let inserted = &value[change.range.start..change.new_end];
        // A caret deletion at a fold boundary consumes one logical newline,
        // including both CRLF bytes, rather than the omitted block.
        if crosses && inserted.is_empty() && before_selection.range().is_empty() {
            if input_type == "deleteContentBackward" || input_type == "deleteWordBackward" {
                let count = if input_type == "deleteWordBackward" {
                    word_delete_len(source[..range.end].chars().rev())
                } else if source[..range.end].ends_with("\r\n") {
                    2
                } else {
                    source[..range.end]
                        .chars()
                        .next_back()
                        .map_or(0, char::len_utf8)
                };
                range.start = range.end - count;
            } else if input_type == "deleteContentForward" || input_type == "deleteWordForward" {
                let count = if source[range.start..].starts_with("\r\n") {
                    2
                } else {
                    source[range.start..]
                        .chars()
                        .next()
                        .map_or(0, char::len_utf8)
                };
                range.end = range.start + count;
            }
        }
        let text = super::native::native_inserted_text(source, inserted);
        let endpoint = |position: usize| -> Result<usize, ProjectionError> {
            if position > value.len() || !value.is_char_boundary(position) {
                return Err(ProjectionError::InvalidOffset);
            }
            if change.range.start <= position && position <= change.new_end {
                let offset = super::byte_to_utf16(inserted, position - change.range.start)
                    .map_err(|_| ProjectionError::InvalidOffset)?;
                return Ok(range.start + super::textarea_to_byte(&text, offset));
            }
            let old = if position < change.range.start {
                position
            } else {
                position - change.new_end + change.range.end
            };
            let original = map(old)?;
            Ok(if original >= range.end {
                original - range.end + range.start + text.len()
            } else {
                original.min(range.start)
            })
        };
        let mapped = Selection {
            anchor: endpoint(selection.anchor)?,
            head: endpoint(selection.head)?,
        };
        Ok((Some(super::Edit { range, text }), mapped))
    }

    /// A native selection can be clipped to the input window while the source
    /// selection still spans omitted text. Reconstruct the declared replacement
    /// from unchanged native surroundings, including locally identical values.
    fn selected_context_change(
        &self,
        value: &str,
        input_type: &str,
        selection: Selection,
    ) -> Result<Option<(super::TextChange, Range<usize>)>, ProjectionError> {
        let source = selection.range();
        if !self.is_windowed()
            || source.is_empty()
            || (source.start >= self.source_range.start && source.end <= self.source_range.end)
            || !(input_type.starts_with("insert") || input_type.starts_with("delete"))
        {
            return Ok(None);
        }
        let visible = self.input_selection(selection)?.range();
        let before = self.textarea_text();
        let start = super::utf16_to_byte(
            before,
            self.byte_to_textarea(visible.start)
                .map_err(|_| ProjectionError::InvalidOffset)?,
        );
        let end = super::utf16_to_byte(
            before,
            self.byte_to_textarea(visible.end)
                .map_err(|_| ProjectionError::InvalidOffset)?,
        );
        let new_end = value
            .len()
            .checked_sub(before.len() - end)
            .filter(|end| *end >= start)
            .ok_or(ProjectionError::InvalidOffset)?;
        if !value.is_char_boundary(start)
            || !value.is_char_boundary(new_end)
            || value[..start] != before[..start]
            || value[new_end..] != before[end..]
        {
            return Err(ProjectionError::InvalidOffset);
        }
        Ok(Some((
            super::TextChange {
                range: start..end,
                new_end,
            },
            source,
        )))
    }

    /// Compatibility replay for callers requiring a complete normalized value.
    pub fn replay_input(
        &self,
        source: &str,
        value: &str,
        selection: Selection,
        input_type: &str,
        before_selection: Selection,
    ) -> Result<(String, Selection), ProjectionError> {
        let (edit, selection) =
            self.replay_edit(source, value, selection, input_type, before_selection)?;
        let mut result = source.to_string();
        if let Some(edit) = edit {
            result.replace_range(edit.range, &edit.text);
        }
        let map = |offset| {
            super::byte_to_textarea(&result, offset).map_err(|_| ProjectionError::InvalidOffset)
        };
        let anchor = map(selection.anchor)?;
        let head = map(selection.head)?;
        let normalized = result.replace("\r\n", "\n").replace('\r', "\n");
        let selection = Selection {
            anchor: super::utf16_to_byte(&normalized, anchor),
            head: super::utf16_to_byte(&normalized, head),
        };
        Ok((normalized, selection))
    }

    /// A native replacement must not silently consume bytes omitted from the view.
    pub fn source_edit_range(
        &self,
        visible: Range<usize>,
    ) -> Result<Range<usize>, ProjectionError> {
        if visible.start > visible.end {
            return Err(ProjectionError::InvalidOffset);
        }
        let start = self.source_offset(visible.start)?;
        let end = self.source_offset(visible.end)?;
        let gap = self
            .hidden
            .partition_point(|gap| gap.visible_offset <= visible.start);
        if self
            .hidden
            .get(gap)
            .is_some_and(|gap| gap.visible_offset <= visible.end)
        {
            return Err(ProjectionError::HiddenText);
        }
        Ok(start..end)
    }
}

fn word_delete_len(chars: impl Iterator<Item = char>) -> usize {
    let mut chars = chars.peekable();
    let mut count = 0;
    while chars.peek().is_some_and(|ch| ch.is_whitespace()) {
        count += chars.next().unwrap().len_utf8();
    }
    let Some(first) = chars.next() else {
        return count;
    };
    count += first.len_utf8();
    let word = first.is_alphanumeric() || first == '_';
    while chars
        .peek()
        .is_some_and(|ch| !ch.is_whitespace() && (ch.is_alphanumeric() || *ch == '_') == word)
    {
        count += chars.next().unwrap().len_utf8();
    }
    count
}

#[cfg(test)]
mod tests {

    #[test]
    fn visible_row_bodies_borrow_unicode_source_across_folds_and_line_endings() {
        let source = "head\r\n文🦀\r\n\r\nlast\rstandalone\n";
        let mut document = super::super::Document::new(source);
        let projection = document.projection();
        assert_eq!(
            (0..projection.lines().len())
                .map(|row| projection.line_body(row).unwrap())
                .collect::<Vec<_>>(),
            ["head", "文🦀", "", "last\rstandalone", ""]
        );
        assert_eq!(projection.line_body(usize::MAX), None);
        document.set_fold_ranges(vec![super::super::FoldRange {
            start_line: 0,
            end_line: 2,
        }]);
        document.fold_command(super::super::FoldCommand::Toggle(0));
        let folded = document.projection();
        assert_eq!(
            (0..folded.lines().len())
                .map(|row| folded.line_body(row).unwrap())
                .collect::<Vec<_>>(),
            ["head", "last\rstandalone", ""]
        );
        let body = folded.line_body(1).unwrap();
        assert_eq!(
            body.as_ptr(),
            folded.text()[folded.lines()[1].visible_start..].as_ptr()
        );
    }

    #[test]
    fn immutable_projection_bodies_remain_valid_through_edits_folds_and_windows() {
        use crate::editor::{Document, Edit, FoldCommand, FoldRange, Selection};
        fn check(projection: &super::FoldProjection) {
            let mut expected = projection
                .text()
                .split_inclusive('\n')
                .map(|row| {
                    row.strip_suffix("\r\n")
                        .or_else(|| row.strip_suffix('\n'))
                        .unwrap_or(row)
                })
                .collect::<Vec<_>>();
            if projection.text().is_empty() || projection.text().ends_with('\n') {
                expected.push("");
            }
            let actual = (0..projection.lines().len())
                .map(|row| projection.line_body(row).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(actual, expected);
            assert!(projection.line_body(projection.lines().len()).is_none());
        }
        for ending in ["\n", "\r\n"] {
            let source = format!("文🦀\tend{ending}").repeat(1024);
            let mut document = Document::new(source.clone());
            let retained = document.projection();
            document.set_fold_ranges(vec![FoldRange {
                start_line: 40,
                end_line: 60,
            }]);
            document.fold_command(FoldCommand::Toggle(40));
            check(&document.projection());
            document
                .apply(
                    vec![Edit::replace(0.."文".len(), "prefix 文\n")],
                    vec![Selection::caret(0)],
                    None,
                )
                .unwrap();
            let edited = document.projection();
            check(&retained);
            assert_eq!(retained.text(), source);
            check(&edited);
            for head in [
                0,
                document.text().find("end").unwrap(),
                document.text().len(),
            ] {
                check(&edited.input_context(Selection::caret(head), 4096).unwrap());
            }
        }
    }

    #[test]
    fn windowed_replacements_keep_complete_source_selections_and_line_endings() {
        let source = format!("before\r\n{}after\r\nlast", "row α🦀 kept\r\n".repeat(20));
        let begin = source.find("α").unwrap();
        let end = source.rfind("kept").unwrap() + 4;
        for (begin, end) in [(begin, end), (0, source.len())] {
            for backward in [false, true] {
                let selection = if backward {
                    Selection {
                        anchor: end,
                        head: begin,
                    }
                } else {
                    Selection {
                        anchor: begin,
                        head: end,
                    }
                };
                let full = FoldProjection::new(&source, &super::super::FoldState::default());
                let context = full.input_context(selection, 64).unwrap();
                assert!(context.is_windowed());
                assert!(context.text().len() <= 64);
                let visible = context.input_selection(selection).unwrap().range();
                let native_start = super::super::utf16_to_byte(
                    context.textarea_text(),
                    context.byte_to_textarea(visible.start).unwrap(),
                );
                let native_end = super::super::utf16_to_byte(
                    context.textarea_text(),
                    context.byte_to_textarea(visible.end).unwrap(),
                );
                for inserted in [
                    "日\n🦀",
                    "",
                    &context.textarea_text()[native_start..native_end],
                ] {
                    let mut value = context.textarea_text().to_string();
                    value.replace_range(native_start..native_end, inserted);
                    let (edit, after) = context
                        .replay_edit(
                            &source,
                            &value,
                            Selection::caret(native_start + inserted.len()),
                            "insertReplacementText",
                            selection,
                        )
                        .unwrap();
                    let edit = edit.unwrap();
                    assert_eq!(edit.range, begin..end);
                    assert_eq!(edit.text, inserted.replace('\n', "\r\n"));
                    assert_eq!(after, Selection::caret(begin + edit.text.len()));
                    let mut actual = source.clone();
                    actual.replace_range(edit.range, &edit.text);
                    let expected = format!(
                        "{}{}{}",
                        &source[..begin],
                        inserted.replace('\n', "\r\n"),
                        &source[end..]
                    );
                    assert_eq!(actual, expected);
                }
            }
        }
    }

    #[test]
    fn clipped_selection_replay_rejects_changed_surroundings_and_invalid_source_boundaries() {
        let source = format!("prefix {} suffix", "α🦀".repeat(30));
        let selection = Selection {
            anchor: 7,
            head: source.len() - 7,
        };
        let full = FoldProjection::new(&source, &super::super::FoldState::default());
        let context = full.input_context(selection, 32).unwrap();
        let visible = context.input_selection(selection).unwrap().range();
        let start = super::super::utf16_to_byte(
            context.textarea_text(),
            context.byte_to_textarea(visible.start).unwrap(),
        );
        let end = super::super::utf16_to_byte(
            context.textarea_text(),
            context.byte_to_textarea(visible.end).unwrap(),
        );
        let mut value = context.textarea_text().to_string();
        value.replace_range(start..end, "x");
        value.push('!');
        assert_eq!(
            context.replay_edit(
                &source,
                &value,
                Selection::caret(start + 1),
                "insertText",
                selection
            ),
            Err(ProjectionError::InvalidOffset)
        );
        assert_eq!(
            context.replay_edit(
                &source,
                context.textarea_text(),
                Selection::caret(0),
                "insertText",
                Selection {
                    anchor: 8,
                    head: selection.head
                }
            ),
            Err(ProjectionError::InvalidOffset)
        );
    }
    use super::*;
    use crate::editor::FoldRange;

    #[test]
    fn bounded_context_keeps_global_source_and_native_offsets_with_partial_rows() {
        for source in [
            "😀文e\u{301}\t\r\na\rbreak\nlast 😀\r\n",
            "header\r\nhidden 文\r\nend\r\nnext 😀\r\n",
        ] {
            let mut folds = FoldState::default();
            folds.set_ranges(
                vec![FoldRange {
                    start_line: 0,
                    end_line: 2,
                }],
                5,
            );
            for folded in [false, true] {
                if folded {
                    folds.collapse_all();
                }
                let full = FoldProjection::new(source, &folds);
                let boundaries: Vec<_> = (0..=full.text().len())
                    .filter(|&at| {
                        full.text().is_char_boundary(at)
                            && !(at > 0
                                && full.text().as_bytes().get(at - 1) == Some(&b'\r')
                                && full.text().as_bytes().get(at) == Some(&b'\n'))
                    })
                    .collect();
                for &start in &boundaries {
                    for &end in boundaries.iter().filter(|&&end| end >= start) {
                        let context = full.window(start..end).unwrap();
                        assert_eq!(context.text(), &full.text()[start..end]);
                        assert_eq!(
                            context.textarea_origin(),
                            full.byte_to_textarea(start).unwrap()
                        );
                        assert_eq!(
                            context.source_range(),
                            full.source_offset(start).unwrap()..full.source_offset(end).unwrap()
                        );
                        for &byte in boundaries
                            .iter()
                            .filter(|&&byte| start <= byte && byte <= end)
                        {
                            let local = byte - start;
                            let document = full.source_offset(byte).unwrap();
                            assert_eq!(context.source_offset(local), Ok(document));
                            assert_eq!(context.visible_offset(document), Ok(local));
                            assert_eq!(
                                context.textarea_origin()
                                    + context.byte_to_textarea(local).unwrap(),
                                full.byte_to_textarea(byte).unwrap()
                            );
                            let row = context
                                .lines()
                                .partition_point(|line| line.visible_start <= local)
                                .saturating_sub(1);
                            let full_row = full
                                .lines()
                                .partition_point(|line| line.visible_start <= byte)
                                .saturating_sub(1);
                            assert_eq!(
                                context.lines()[row].source_line,
                                full.lines()[full_row].source_line
                            );
                        }
                        for native in 0..=context.textarea_text().encode_utf16().count() {
                            let local = context.textarea_to_byte(native);
                            assert_eq!(
                                context.source_offset(local),
                                full.source_offset(
                                    full.textarea_to_byte(context.textarea_origin() + native)
                                )
                            );
                        }
                        if start > 0 {
                            assert_eq!(
                                context.visible_offset(0),
                                Err(ProjectionError::InvalidOffset)
                            );
                        }
                        if end < full.text().len() {
                            assert_eq!(
                                context.visible_offset(source.len()),
                                Err(ProjectionError::InvalidOffset)
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn contexts_clip_large_backward_selections_without_changing_document_selections() {
        let source = "文😀abc\r\n".repeat(100_000);
        let full = FoldProjection::new(&source, &FoldState::default());
        let head = source.find("abc").unwrap() + 50_000 * "文😀abc\r\n".len();
        let selection = Selection {
            anchor: source.len(),
            head,
        };
        let context = full.input_context(selection, 4096).unwrap();
        assert!(context.is_windowed());
        assert!(context.text().len() <= 4096);
        let local = context.input_selection(selection).unwrap();
        assert_eq!(local.anchor, context.text().len());
        assert_eq!(context.source_offset(local.head), Ok(head));
        assert_eq!(
            context.source_offset(context.text().len()),
            Ok(context.source_range().end)
        );
        assert_ne!(
            context.source_offset(context.text().len()),
            Ok(source.len())
        );
        let nested = context.window(0..local.head).unwrap();
        assert_eq!(nested.textarea_origin(), context.textarea_origin());
        assert_eq!(nested.source_range().end, head);
        assert_eq!(full.visible_selection(selection).unwrap(), selection);
        assert_eq!(full.text(), source);
    }

    #[test]
    fn bounded_context_replay_edits_the_full_source_and_preserves_folded_text() {
        let source = "before\r\nheader\r\nhidden 文\r\nend\r\nnext 😀\r\nafter";
        let mut folds = FoldState::default();
        folds.set_ranges(
            vec![FoldRange {
                start_line: 1,
                end_line: 3,
            }],
            6,
        );
        folds.collapse_all();
        let full = FoldProjection::new(source, &folds);
        let at = source.find("next").unwrap();
        let context = full.input_context(Selection::caret(at), 18).unwrap();
        let local = context.visible_offset(at).unwrap();
        let local_native = context.byte_to_textarea(local).unwrap();
        let mut value = context.textarea_text().to_string();
        let local = super::super::utf16_to_byte(&value, local_native);
        value.insert(local, '🦀');
        let (result, selection) = context
            .replay_input(
                source,
                &value,
                Selection::caret(local + '🦀'.len_utf8()),
                "insertText",
                Selection::caret(at),
            )
            .unwrap();
        let mut expected = source.replace("\r\n", "\n");
        let at = expected.find("next").unwrap();
        expected.insert(at, '🦀');
        assert_eq!(result, expected);
        assert_eq!(selection, Selection::caret(at + '🦀'.len_utf8()));
        assert!(result.contains("hidden 文\nend\n"));
    }

    #[test]
    fn bounded_replay_returns_only_inserted_source_and_preserves_native_selection() {
        let source = format!(
            "{}\r\nnext 😀\r\n{}",
            "before".repeat(1000),
            "after".repeat(1000)
        );
        let full = FoldProjection::new(&source, &FoldState::default());
        let at = source.find("next").unwrap();
        let context = full.input_context(Selection::caret(at), 24).unwrap();
        let local = context
            .byte_to_textarea(context.visible_offset(at).unwrap())
            .unwrap();
        let mut value = context.textarea_text().to_string();
        let local = super::super::utf16_to_byte(&value, local);
        value.insert_str(local, "🦀\n");
        let (edit, selection) = context
            .replay_edit(
                &source,
                &value,
                Selection::caret(local + "🦀\n".len()),
                "insertText",
                Selection::caret(at),
            )
            .unwrap();
        let edit = edit.unwrap();
        assert_eq!(edit.range, at..at);
        assert_eq!(edit.text, "🦀\r\n");
        assert_eq!(selection, Selection::caret(at + "🦀\r\n".len()));
        let mut result = source.clone();
        result.replace_range(edit.range, &edit.text);
        assert!(result.ends_with(&"after".repeat(1000)));
        assert_eq!(
            context
                .replay_edit(
                    &source,
                    context.textarea_text(),
                    Selection::caret(local),
                    "insertText",
                    Selection::caret(at)
                )
                .unwrap(),
            (None, Selection::caret(at))
        );
    }

    #[test]
    fn projected_source_replay_rejects_invalid_native_selection() {
        let source = "😀\r\nnext";
        let projection = FoldProjection::new(source, &FoldState::default());
        for selection in [Selection::caret(1), Selection::caret(100)] {
            assert_eq!(
                projection.replay_edit(
                    source,
                    "😀X\nnext",
                    selection,
                    "insertText",
                    Selection::caret(4)
                ),
                Err(ProjectionError::InvalidOffset)
            );
        }
    }

    #[test]
    fn dense_native_contexts_bound_rows_and_preserve_folded_source_coordinates() {
        for ending in ["\n", "\r\n"] {
            let source = format!("文🦀{ending}").repeat(1024);
            for folded in [false, true] {
                let mut folds = FoldState::default();
                if folded {
                    folds.set_ranges(
                        vec![super::super::FoldRange {
                            start_line: 400,
                            end_line: 700,
                        }],
                        1025,
                    );
                    assert!(folds.toggle(400));
                }
                let full = FoldProjection::new(&source, &folds);
                for head in [0, 800 * (7 + ending.len()), source.len()] {
                    let selection = Selection::caret(head);
                    let context = full.input_context(selection, 12 * 1024).unwrap();
                    assert!(context.lines().len() <= INPUT_CONTEXT_ROWS + 1);
                    assert!(context.text().len() <= 12 * 1024);
                    assert!(context.fits_input_context(12 * 1024));
                    let local = context.input_selection(selection).unwrap();
                    assert_eq!(context.source_selection(local).unwrap(), selection);
                    assert_eq!(
                        context.textarea_text(),
                        context.text().replace("\r\n", "\n")
                    );
                    assert!(
                        context
                            .lines()
                            .iter()
                            .any(|line| line.source.contains(&head) || line.source.end == head)
                    );
                }
            }
        }
    }

    #[test]
    fn row_bound_leaves_long_single_line_contexts_byte_bounded() {
        let source = "文🦀".repeat(20_000);
        let full = FoldProjection::new(&source, &FoldState::default());
        let head = source.len() / 2;
        let context = full
            .input_context(Selection::caret(head), 12 * 1024)
            .unwrap();
        assert!(context.text().len() >= 12 * 1024 - 6);
        assert!(context.text().len() <= 12 * 1024);
        assert_eq!(
            context
                .source_selection(context.input_selection(Selection::caret(head)).unwrap())
                .unwrap(),
            Selection::caret(head)
        );
    }

    #[test]
    fn retained_input_contexts_enforce_both_bounds() {
        let projection = |source: &str| FoldProjection::new(source, &FoldState::default());
        assert!(projection(&"\n".repeat(INPUT_CONTEXT_ROWS)).fits_input_context(1024));
        assert!(!projection(&"\n".repeat(INPUT_CONTEXT_ROWS + 1)).fits_input_context(1024));
        assert!(!projection("文🦀").fits_input_context(6));
        assert!(projection("文🦀").fits_input_context(7));
    }

    #[test]
    fn bounded_context_rejects_invalid_bounds_and_never_splits_unicode_or_crlf() {
        let full = FoldProjection::new("😀\r\n文\rnext", &FoldState::default());
        let reversed = Range { start: 9, end: 8 };
        for range in [1..4, 0..5, 5..6, reversed, 0..100] {
            assert_eq!(full.window(range), Err(ProjectionError::InvalidOffset));
        }
        for head in (0..=full.text().len()).filter(|&at| full.text().is_char_boundary(at)) {
            for budget in 4..=full.text().len() + 4 {
                let context = full.input_context(Selection::caret(head), budget).unwrap();
                assert!(context.text().len() <= budget);
                assert!(context.input_selection(Selection::caret(head)).is_ok());
                assert_eq!(
                    context.textarea_text(),
                    context.text().replace("\r\n", "\n").replace('\r', "\n")
                );
            }
        }
        assert_eq!(
            full.input_context(Selection::caret(0), 3),
            Err(ProjectionError::InvalidOffset)
        );
        assert_eq!(
            full.input_context(Selection::caret(100), 16),
            Err(ProjectionError::InvalidOffset)
        );
    }

    #[test]
    fn document_projection_reuses_immutable_storage_and_invalidates_on_edits_and_folds() {
        use crate::editor::{Document, Edit, FoldCommand};
        let mut document = Document::new("head 😀\r\nbody 文\r\nend\r\nnext");
        let before = document.clone();
        let first = document.projection();
        assert!(Arc::ptr_eq(&document.text, &first.text));
        assert_eq!(
            document, before,
            "preparing a derived view does not mutate document identity"
        );
        let again = document.projection();
        assert!(Arc::ptr_eq(&first.text, &again.text));
        assert!(Arc::ptr_eq(&first.lines, &again.lines));
        assert!(Arc::ptr_eq(&first.textarea_text, &again.textarea_text));
        for offset in 0..=first.textarea_text.encode_utf16().count() + 2 {
            assert_eq!(
                first.textarea_to_byte(offset),
                crate::editor::textarea_to_byte(first.text(), offset)
            );
        }
        document
            .apply(
                vec![Edit::replace(0..4, "header")],
                vec![Selection::caret(0)],
                None,
            )
            .unwrap();
        let changed = document.projection();
        assert!(!Arc::ptr_eq(&first.text, &changed.text));
        assert!(first.text().starts_with("head 😀"));
        document.fold_state_mut().set_ranges(
            vec![FoldRange {
                start_line: 0,
                end_line: 2,
            }],
            4,
        );
        document.fold_command(FoldCommand::CollapseAll);
        let folded = document.projection();
        assert!(!Arc::ptr_eq(&document.text, &folded.text));
        assert_eq!(folded.text(), "header 😀\r\nnext");
        assert_eq!(folded.textarea_text(), "header 😀\nnext");
        for offset in 0..=folded.textarea_text.encode_utf16().count() + 2 {
            assert_eq!(
                folded.textarea_to_byte(offset),
                crate::editor::textarea_to_byte(folded.text(), offset)
            );
        }
        assert_eq!(
            folded
                .source_native_selection(Selection::caret(folded.lines()[1].textarea_start))
                .unwrap()
                .head,
            document.text().find("next").unwrap()
        );
        assert_eq!(document.projection(), folded);
        document.fold_command(FoldCommand::ExpandAll);
        let expanded = document.projection();
        assert_eq!(expanded.text(), changed.text());
        assert!(Arc::ptr_eq(&document.text, &expanded.text));
    }

    #[test]
    fn unused_projection_releases_source_before_edit_and_retained_views_stay_immutable() {
        use crate::editor::{Document, Edit};
        for source in [
            "head 文😀\nbody\n",
            "head 文😀\r\nbody\r\n",
            "head 文😀\rbody\n",
        ] {
            let mut document = Document::new(source);
            let first = document.projection();
            assert!(Arc::ptr_eq(&document.text, &first.text));
            assert_eq!(
                first.textarea_text(),
                source.replace("\r\n", "\n").replace('\r', "\n")
            );
            if !source.contains('\r') {
                assert!(Arc::ptr_eq(&document.text, &first.textarea_text));
            }
            let address = document.text().as_ptr();
            drop(first);
            document
                .apply(
                    vec![Edit::replace(0..4, "HEAD")],
                    vec![Selection::caret(4)],
                    None,
                )
                .unwrap();
            assert_eq!(
                document.text().as_ptr(),
                address,
                "an unused projection must not force a source copy"
            );
            let retained = document.projection();
            document
                .apply(
                    vec![Edit::replace(0..4, "tail")],
                    vec![Selection::caret(4)],
                    None,
                )
                .unwrap();
            assert_ne!(document.text().as_ptr(), retained.text().as_ptr());
            assert!(retained.text().starts_with("HEAD"));
            assert!(document.undo());
            assert_eq!(document.text(), retained.text());
            assert!(document.redo());
            assert!(document.text().starts_with("tail"));
            assert!(retained.text().starts_with("HEAD"));
        }
    }

    #[test]
    fn folded_storage_reserves_visible_bytes_without_retaining_hidden_capacity() {
        use crate::editor::{Document, FoldCommand};
        let source = format!("head\n{}last\n", "    body 文😀\r\n".repeat(10_000));
        let mut document = Document::new(source);
        document.fold_state_mut().set_ranges(
            vec![FoldRange {
                start_line: 0,
                end_line: 10_000,
            }],
            10_003,
        );
        document.fold_command(FoldCommand::CollapseAll);
        let folded = document.projection();
        assert_eq!(folded.text(), "head\nlast\n");
        assert_eq!(folded.coordinates.capacity(), folded.coordinates.len());
        assert!(
            folded.text.capacity() < 100,
            "a small visible projection must not reserve hidden source bytes"
        );
        assert!(Arc::ptr_eq(&folded.text, &folded.textarea_text));
        let window = folded.window(0..5).unwrap();
        assert_eq!(window.text(), "head\n");
        assert_eq!(window.coordinates.capacity(), window.coordinates.len());
        assert!(window.text.capacity() < 100);
        assert!(Arc::ptr_eq(&window.text, &window.textarea_text));
    }

    #[test]
    fn unfolded_coordinates_share_source_and_detach_only_for_retained_views() {
        use crate::editor::{Document, Edit, FoldCommand};
        for ending in ["\n", "\r\n", "\r"] {
            let source = format!(
                "head{ending}{}{ending}tail{ending}",
                "文😀אב\t".repeat(1000)
            );
            let mut document = Document::new(source);
            let first = document.projection();
            assert!(Arc::ptr_eq(
                &document.line_index.coordinates,
                &first.coordinates
            ));
            let address = Arc::as_ptr(&first.coordinates);
            let rows_address = Arc::as_ptr(&first.lines);
            assert!(Arc::ptr_eq(
                &document.line_index.visible_rows(),
                &first.lines
            ));
            drop(first);
            document
                .apply(
                    vec![Edit::replace(0..4, "HEAD")],
                    vec![Selection::caret(4)],
                    None,
                )
                .unwrap();
            assert_eq!(Arc::as_ptr(&document.line_index.coordinates), address);
            assert_eq!(
                Arc::as_ptr(&document.line_index.visible_rows()),
                rows_address
            );
            let retained = document.projection();
            let original = retained.clone();
            let snapshot = document.clone();
            assert!(Arc::ptr_eq(
                &snapshot.line_index.coordinates,
                &retained.coordinates
            ));
            document
                .apply(
                    vec![Edit::replace(4..4, "\nnew 😀\n")],
                    vec![Selection::caret(4)],
                    None,
                )
                .unwrap();
            assert!(!Arc::ptr_eq(
                &document.line_index.coordinates,
                &retained.coordinates
            ));
            assert!(!Arc::ptr_eq(
                &document.line_index.visible_rows(),
                &retained.lines
            ));
            for projection in [
                document.projection(),
                retained.clone(),
                snapshot.projection(),
            ] {
                let fresh = FoldProjection::new(projection.text(), &FoldState::default());
                assert_eq!(projection, fresh);
                for offset in 0..=projection.textarea_text().encode_utf16().count() {
                    assert_eq!(
                        projection.textarea_to_byte(offset),
                        fresh.textarea_to_byte(offset)
                    );
                }
            }
            assert_eq!(retained, original);
            assert!(document.undo());
            assert_eq!(document.projection(), original);
            assert!(document.redo());
            assert_eq!(retained, original);
            if ending != "\r" {
                let rows = document.text().split('\n').count();
                document.fold_state_mut().set_ranges(
                    vec![FoldRange {
                        start_line: 0,
                        end_line: 2,
                    }],
                    rows,
                );
                document.fold_command(FoldCommand::CollapseAll);
                let folded = document.projection();
                assert!(!Arc::ptr_eq(
                    &document.line_index.coordinates,
                    &folded.coordinates
                ));
                assert!(!Arc::ptr_eq(
                    &document.line_index.visible_rows(),
                    &folded.lines
                ));
                assert_eq!(
                    folded,
                    FoldProjection::new(document.text(), document.fold_state())
                );
                document.fold_command(FoldCommand::ExpandAll);
                assert!(Arc::ptr_eq(
                    &document.line_index.coordinates,
                    &document.projection().coordinates
                ));
            }
        }
    }

    #[test]
    fn shared_host_text_retains_snapshots_and_rejects_fresh_document_provenance() {
        use crate::editor::{Document, Edit};
        for text in ["", "文😀\r\nbody\n"] {
            let source = Arc::new(text.to_string());
            let mut document = Document::from_shared_text(source.clone());
            let first = document.projection();
            assert!(Arc::ptr_eq(&source, &document.shared_text()));
            assert!(first.shares_text_version(&document.clone().projection()));
            let replacement = Document::from_shared_text(source.clone()).projection();
            assert!(Arc::ptr_eq(&first.text, &replacement.text));
            assert!(!first.shares_text_version(&replacement));
            document
                .apply(
                    vec![Edit::replace(0..0, "new\n")],
                    vec![Selection::caret(4)],
                    None,
                )
                .unwrap();
            assert_eq!(source.as_str(), text);
            assert_eq!(first.text(), text);
            assert!(!Arc::ptr_eq(&source, &document.shared_text()));
            assert!(document.undo());
            assert_eq!(document.text(), text);
            assert_eq!(source.as_str(), text);
        }
    }

    #[test]
    fn empty_projection_versions_reject_distinct_documents_and_retain_clones() {
        use crate::editor::Document;
        let document = Document::new("");
        let first = document.projection();
        assert!(first.shares_text_version(&first.clone()));
        assert!(first.shares_text_version(&document.clone().projection()));
        let replacement = Document::new("").projection();
        assert_eq!(first.text().as_ptr(), replacement.text().as_ptr());
        assert!(!first.shares_text_version(&replacement));
    }

    proptest::proptest! {
        #[test]
        fn bounded_input_contexts_round_trip_arbitrary_unicode_and_folds(
            rows in proptest::collection::vec(".{0,30}", 3..12),
            budget in 4usize..128,
            location in 0usize..1000,
        ) {
            let source = rows.join("\r\n");
            let mut folds = FoldState::default();
            folds.set_ranges(vec![FoldRange { start_line: 0, end_line: rows.len() - 2 }], rows.len());
            folds.collapse_all();
            let full = FoldProjection::new(&source, &folds);
            let boundaries: Vec<_> = full.text().char_indices().map(|(at, _)| at).chain([full.text().len()]).filter(|&at| {
                !(at > 0 && full.text().as_bytes().get(at - 1) == Some(&b'\r') && full.text().as_bytes().get(at) == Some(&b'\n'))
            }).collect();
            let visible = boundaries[location % boundaries.len()];
            let caret = Selection::caret(full.source_offset(visible).unwrap());
            let context = full.input_context(caret, budget).unwrap();
            proptest::prop_assert!(context.text().len() <= budget);
            let local = context.input_selection(caret).unwrap();
            proptest::prop_assert_eq!(context.source_selection(local), Ok(caret));
            proptest::prop_assert_eq!(context.textarea_origin() + context.byte_to_textarea(local.head).unwrap(), full.byte_to_textarea(visible).unwrap());
        }

        #[test]
        fn every_visible_unicode_boundary_round_trips(rows in proptest::collection::vec(".*", 3..20)) {
            let source = rows.join("\r\n");
            let count = source.split('\n').count();
            let mut state = FoldState::default();
            state.set_ranges(vec![FoldRange { start_line: 0, end_line: count - 2 }], count);
            state.collapse_all();
            let projection = FoldProjection::new(&source, &state);
            for offset in 0..=projection.text.len() {
                if projection.text.is_char_boundary(offset) {
                    let original = projection.source_offset(offset).unwrap();
                    proptest::prop_assert_eq!(projection.visible_offset(original).unwrap(), offset);
                }
            }
        }
    }

    #[test]
    fn hidden_carriage_returns_do_not_change_visible_native_layout() {
        use crate::editor::{Document, FoldCommand};
        for (source, end, expected, uniform) in [
            (
                "head\nhidden\rbroken\nhidden\r\nlast\n",
                2,
                "head\nlast\n",
                true,
            ),
            (
                "head\rinside\nhidden\r\nlast\n",
                1,
                "head\rinside\nlast\n",
                false,
            ),
            ("head\r\nhidden\rthing\nlast\n", 1, "head\r\nlast\n", true),
            ("head\nhidden\rthing", 1, "head\n", true),
        ] {
            let mut document = Document::new(source);
            let rows = source.split('\n').count();
            document.fold_state_mut().set_ranges(
                vec![FoldRange {
                    start_line: 0,
                    end_line: end,
                }],
                rows,
            );
            document.fold_command(FoldCommand::CollapseAll);
            for projection in [
                document.projection(),
                FoldProjection::new(source, document.fold_state()),
            ] {
                assert_eq!(projection.text(), expected);
                assert_eq!(projection.has_uniform_rows(), uniform);
                assert_eq!(
                    projection.textarea_text(),
                    expected.replace("\r\n", "\n").replace('\r', "\n")
                );
                assert_eq!(
                    Arc::ptr_eq(&projection.text, &projection.textarea_text),
                    !expected.contains('\r')
                );
            }
            document.fold_command(FoldCommand::ExpandAll);
            assert_eq!(
                document.projection(),
                FoldProjection::new(source, document.fold_state())
            );
        }
    }

    #[test]
    fn window_offsets_preserve_native_utf16_after_unicode_crlf_and_folds() {
        assert!(FoldProjection::new("a\r\nb", &FoldState::default()).has_uniform_rows());
        assert!(!FoldProjection::new("a\rb", &FoldState::default()).has_uniform_rows());
        let source = "head 😀\r\nbody 文\r\nend\r\nnext 🦀";
        let mut folds = FoldState::default();
        folds.set_ranges(
            vec![FoldRange {
                start_line: 0,
                end_line: 2,
            }],
            4,
        );
        for collapsed in [false, true] {
            if collapsed {
                folds.collapse_all();
            }
            let projection = FoldProjection::new(source, &folds);
            for line in projection.lines() {
                assert_eq!(
                    line.textarea_start,
                    projection.text()[..line.visible_start]
                        .replace("\r\n", "\n")
                        .encode_utf16()
                        .count()
                );
            }
        }
    }

    #[test]
    fn input_only_events_replay_unicode_and_boundary_deletion_without_losing_hidden_code() {
        let source = "head\r\nbody 文\r\nend\r\nnext 😀";
        let mut state = FoldState::default();
        state.set_ranges(
            vec![FoldRange {
                start_line: 0,
                end_line: 2,
            }],
            4,
        );
        state.collapse_all();
        let projection = FoldProjection::new(source, &state);
        let next = source.find("next").unwrap();
        let (text, selection) = projection
            .replay_input(
                source,
                "head\nXnext 😀",
                Selection::caret(6),
                "insertText",
                Selection::caret(next),
            )
            .unwrap();
        assert_eq!(text, "head\nbody 文\nend\nXnext 😀");
        assert_eq!(selection.head, text.find("Xnext").unwrap() + 1);
        let (text, selection) = projection
            .replay_input(
                source,
                "headnext 😀",
                Selection::caret(4),
                "deleteContentBackward",
                Selection::caret(next),
            )
            .unwrap();
        assert_eq!(text, "head\nbody 文\nendnext 😀");
        assert_eq!(selection.head, text.find("next").unwrap());
        let (text, _) = projection
            .replay_input(
                source,
                "headnext 😀",
                Selection::caret(4),
                "deleteContentForward",
                Selection::caret(4),
            )
            .unwrap();
        assert_eq!(text, "headbody 文\nend\nnext 😀");
        let (text, _) = projection
            .replay_input(
                source,
                "headnext 😀",
                Selection::caret(4),
                "deleteContentBackward",
                Selection {
                    anchor: 4,
                    head: next,
                },
            )
            .unwrap();
        assert_eq!(
            text, "headnext 😀",
            "explicit selections still include folded source"
        );
    }

    #[test]
    fn unicode_crlf_projection_preserves_logical_rows_and_bidirectional_offsets() {
        let source = "😀 {\r\n    文\r\n}\r\nnext 😀\r\n";
        let mut folds = FoldState::default();
        folds.set_ranges(
            vec![FoldRange {
                start_line: 0,
                end_line: 2,
            }],
            5,
        );
        folds.toggle(0);
        let projection = FoldProjection::new(source, &folds);
        assert_eq!(projection.text(), "😀 {\r\nnext 😀\r\n");
        assert_eq!(
            projection
                .lines
                .iter()
                .map(|line| line.source_line)
                .collect::<Vec<_>>(),
            vec![0, 3, 4]
        );
        for offset in 0..=projection.text.len() {
            if projection.text.is_char_boundary(offset) {
                let source = projection.source_offset(offset).unwrap();
                assert_eq!(projection.visible_offset(source).unwrap(), offset);
            }
        }
        assert_eq!(
            projection.visible_offset(source.find('文').unwrap()),
            Err(ProjectionError::HiddenText)
        );
        assert_eq!(
            projection.visible_offset(1),
            Err(ProjectionError::InvalidOffset)
        );
        assert_eq!(
            projection.source_offset(1),
            Err(ProjectionError::InvalidOffset)
        );
        let source_selection = Selection {
            anchor: source.len(),
            head: source.find("next").unwrap(),
        };
        assert_eq!(
            projection
                .source_selection(projection.visible_selection(source_selection).unwrap())
                .unwrap(),
            source_selection
        );
        assert_eq!(
            projection
                .visible_selection(Selection::caret(source.find('文').unwrap()))
                .unwrap(),
            Selection::caret("😀 {".len())
        );
    }

    #[test]
    fn hidden_gaps_require_reveal_before_native_deletion_but_not_next_row_insertion() {
        let source = "head\nbody\nend\nnext";
        let mut folds = FoldState::default();
        folds.set_ranges(
            vec![FoldRange {
                start_line: 0,
                end_line: 2,
            }],
            4,
        );
        folds.collapse_all();
        let projection = FoldProjection::new(source, &folds);
        assert_eq!(
            projection.source_edit_range(4..5),
            Err(ProjectionError::HiddenText)
        );
        assert_eq!(
            projection.source_edit_range(0..6),
            Err(ProjectionError::HiddenText)
        );
        assert_eq!(projection.source_edit_range(5..5).unwrap(), 14..14);
        assert_eq!(projection.source_edit_range(5..6).unwrap(), 14..15);
        assert_eq!(
            projection.source_edit_range(std::ops::Range { start: 6, end: 5 }),
            Err(ProjectionError::InvalidOffset)
        );
    }

    #[test]
    fn nested_folds_reveal_outer_rows_without_losing_inner_collapse_and_eof_identity() {
        let source = "outer\ninner\nbody\nclose\nlast\n";
        let mut folds = FoldState::default();
        folds.set_ranges(
            vec![
                FoldRange {
                    start_line: 0,
                    end_line: 4,
                },
                FoldRange {
                    start_line: 1,
                    end_line: 3,
                },
            ],
            6,
        );
        folds.collapse_all();
        assert_eq!(FoldProjection::new(source, &folds).text(), "outer\n");
        folds.toggle(0);
        let projection = FoldProjection::new(source, &folds);
        assert_eq!(projection.text(), "outer\ninner\nlast\n");
        assert_eq!(projection.lines.last().unwrap().source_line, 5);
        assert_eq!(
            projection.source_offset(projection.text.len()).unwrap(),
            source.len()
        );
        folds.expand_all();
        assert_eq!(FoldProjection::new(source, &folds).text(), source);
        assert_eq!(FoldProjection::new("", &folds).source_offset(0).unwrap(), 0);
    }
}
