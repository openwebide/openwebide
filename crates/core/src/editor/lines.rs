//! Logical-line commands, independent of the browser and workspace transport.
use super::{Document, Edit, EditError, LineEnding, Selection};
use std::{collections::BTreeSet, ops::Range};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineCommand {
    MoveUp,
    MoveDown,
    Duplicate,
    DuplicateAbove,
    Delete,
    InsertAbove,
    InsertBelow,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Line {
    pub start: usize,
    pub body_end: usize,
    pub end: usize,
}

/// Tree-sitter columns count source bytes, retaining CR before an LF separator.
#[cfg(feature = "editor-parser")]
#[derive(Clone, Copy)]
pub(super) struct SyntaxLines<'a> {
    source: &'a str,
    rows: &'a [Line],
}
#[cfg(feature = "editor-parser")]
impl<'a> SyntaxLines<'a> {
    pub fn new(source: &'a str, rows: &'a [Line]) -> Self {
        Self { source, rows }
    }
    pub fn len(self) -> usize {
        self.rows.len()
    }
    pub fn get(self, index: usize) -> Option<&'a str> {
        let row = self.rows.get(index)?;
        let text = self.source.get(row.start..row.end)?;
        Some(text.strip_suffix('\n').unwrap_or(text))
    }
}

fn line_iter(text: &str) -> impl Iterator<Item = Line> + '_ {
    let mut start = 0;
    text.split_inclusive('\n')
        .map(move |line| {
            let end = start + line.len();
            let body_end = if line.ends_with("\r\n") {
                end - 2
            } else if line.ends_with('\n') {
                end - 1
            } else {
                end
            };
            let row = Line {
                start,
                body_end,
                end,
            };
            start = end;
            row
        })
        .chain((text.is_empty() || text.ends_with('\n')).then_some(Line {
            start: text.len(),
            body_end: text.len(),
            end: text.len(),
        }))
}

pub(super) fn lines(text: &str) -> Vec<Line> {
    line_iter(text).collect()
}

/// One changed-row envelope for source coordinates and their derived indexes.
pub(super) struct LineEdit {
    pub rows: Range<usize>,
    pub bytes: Range<usize>,
    has_suffix: bool,
    old_end: usize,
}
impl LineEdit {
    pub fn new(
        rows: &[Line],
        old_len: usize,
        new_len: usize,
        changed: Range<usize>,
        new_end: usize,
    ) -> Self {
        let start_row = row_at(rows, changed.start).saturating_sub(1);
        let mut end_row = (row_at(rows, changed.end) + 1).min(rows.len());
        if rows.get(end_row).is_some_and(|row| row.start == old_len) {
            end_row = rows.len();
        }
        let start = rows[start_row].start;
        let old_end = rows.get(end_row).map_or(old_len, |row| row.start);
        let end = new_len - (old_len - old_end);
        debug_assert!(start <= changed.start && changed.end <= old_end && new_end <= end);
        Self {
            rows: start_row..end_row,
            bytes: start..end,
            has_suffix: end < new_len,
            old_end,
        }
    }

    /// The unchanged suffix starts on an old row boundary. Retain that row only
    /// when the replacement also ends at the document origin or a complete LF boundary; otherwise
    /// its text joins the changed row and needs fresh coordinates.
    pub fn retain_suffix(&mut self, rows: &[Line], changed_end: usize, new_end: usize, new: &str) {
        let suffix = row_at(rows, changed_end);
        if rows[suffix].start == changed_end
            && rows[suffix].start < rows[suffix].end
            && suffix >= self.rows.start
            && (new_end == 0 || new.as_bytes().get(new_end - 1) == Some(&b'\n'))
        {
            self.rows.end = suffix;
            self.bytes.end = new_end;
            self.old_end = changed_end;
            self.has_suffix = true;
        }
    }

    /// A temporary trailing empty row belongs to the retained suffix, not the edit.
    pub fn trim_suffix_row(&self, row: Option<&Line>) -> bool {
        self.has_suffix && row.is_some_and(|row| row.start == self.bytes.len())
    }

    #[cfg(feature = "editor-parser")]
    pub fn prepare_replacement(
        &self,
        source: std::sync::Arc<String>,
        limit: usize,
    ) -> LineReplacement {
        LineReplacement::new(source, self.bytes.clone(), self.has_suffix, limit)
    }

    #[cfg(feature = "editor-parser")]
    pub fn replacement(&self, source: &str, limit: usize) -> Option<Vec<Line>> {
        let mut scan = LineReplacementScan::new(self.bytes.clone(), self.has_suffix, limit);
        while scan.status == LineReplacementStatus::Pending {
            scan.advance(source, usize::MAX);
        }
        (scan.status == LineReplacementStatus::Ready).then_some(scan.rows)
    }

    #[cfg(feature = "editor-parser")]
    pub fn prepare_publication(&self, replacement: Vec<Line>) -> LinePublication {
        LinePublication {
            replaced: self.rows.clone(),
            start: self.bytes.start,
            end: self.bytes.end,
            old_end: self.old_end,
            replacement,
            next: Vec::new(),
            complete: false,
        }
    }

    pub fn apply(&self, rows: &mut Vec<Line>, mut replacement: Vec<Line>) {
        for row in &mut rows[self.rows.end..] {
            *row = shifted_line(row, self.old_end, self.bytes.end);
        }
        for row in &mut replacement {
            *row = shifted_line(row, 0, self.bytes.start);
        }
        rows.splice(self.rows.clone(), replacement);
    }
}

fn shifted_line(row: &Line, old_start: usize, new_start: usize) -> Line {
    Line {
        start: new_start + (row.start - old_start),
        body_end: new_start + (row.body_end - old_start),
        end: new_start + (row.end - old_start),
    }
}

/// Retain the complete old index while assembling its replacement in row batches.
/// Dropping pending work leaves the original coordinates untouched.
#[cfg(feature = "editor-parser")]
pub(super) struct LinePublication {
    replaced: Range<usize>,
    start: usize,
    end: usize,
    old_end: usize,
    replacement: Vec<Line>,
    next: Vec<Line>,
    complete: bool,
}
#[cfg(feature = "editor-parser")]
impl LinePublication {
    pub fn is_complete(&self) -> bool {
        self.complete
    }
    pub fn advance(&mut self, rows: &[Line], budget: usize) -> bool {
        let total = rows.len() - self.replaced.len() + self.replacement.len();
        let end = total.min(self.next.len().saturating_add(budget));
        while self.next.len() < end {
            let index = self.next.len();
            let row = if index < self.replaced.start {
                rows[index].clone()
            } else if index < self.replaced.start + self.replacement.len() {
                let row = &self.replacement[index - self.replaced.start];
                shifted_line(row, 0, self.start)
            } else {
                let row = &rows[index - self.replacement.len() + self.replaced.len()];
                shifted_line(row, self.old_end, self.end)
            };
            self.next.push(row);
        }
        self.complete = self.next.len() == total;
        self.complete
    }
    pub fn finish(self) -> Option<Vec<Line>> {
        self.complete.then_some(self.next)
    }
}

#[cfg(feature = "editor-parser")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LineReplacementStatus {
    Pending,
    Ready,
    TooLarge,
}

#[cfg(feature = "editor-parser")]
struct LineReplacementScan {
    bytes: Range<usize>,
    has_suffix: bool,
    limit: usize,
    next: usize,
    row_start: usize,
    rows: Vec<Line>,
    status: LineReplacementStatus,
}
#[cfg(feature = "editor-parser")]
impl LineReplacementScan {
    fn new(bytes: Range<usize>, has_suffix: bool, limit: usize) -> Self {
        Self {
            bytes,
            has_suffix,
            limit,
            next: 0,
            row_start: 0,
            rows: Vec::new(),
            status: LineReplacementStatus::Pending,
        }
    }
    fn push(&mut self, body_end: usize, end: usize) -> bool {
        if self.rows.len() == self.limit {
            self.status = LineReplacementStatus::TooLarge;
            return false;
        }
        self.rows.push(Line {
            start: self.row_start,
            body_end,
            end,
        });
        self.row_start = end;
        true
    }
    fn advance(&mut self, source: &str, budget: usize) {
        if self.status != LineReplacementStatus::Pending || budget == 0 {
            return;
        }
        let bytes = &source.as_bytes()[self.bytes.clone()];
        let end = bytes.len().min(self.next.saturating_add(budget));
        let first_rows = self.rows.len();
        while self.next < end {
            let byte = bytes[self.next];
            self.next += 1;
            if byte == b'\n' {
                let body_end = self.next
                    - if self.next >= 2 && bytes[self.next - 2] == b'\r' {
                        2
                    } else {
                        1
                    };
                if !self.push(body_end, self.next) {
                    return;
                }
                if self.rows.len() - first_rows == 256 {
                    return;
                }
            }
        }
        if self.next == bytes.len() {
            if (self.row_start < bytes.len() || !self.has_suffix)
                && !self.push(bytes.len(), bytes.len())
            {
                return;
            }
            self.status = LineReplacementStatus::Ready;
        }
    }
}

#[cfg(feature = "editor-parser")]
pub(super) struct LineReplacement {
    source: std::sync::Arc<String>,
    scan: LineReplacementScan,
}
#[cfg(feature = "editor-parser")]
impl LineReplacement {
    fn new(
        source: std::sync::Arc<String>,
        bytes: Range<usize>,
        has_suffix: bool,
        limit: usize,
    ) -> Self {
        Self {
            source,
            scan: LineReplacementScan::new(bytes, has_suffix, limit),
        }
    }
    pub fn status(&self) -> LineReplacementStatus {
        self.scan.status
    }
    pub fn advance(&mut self, budget: usize) -> LineReplacementStatus {
        self.scan.advance(&self.source, budget);
        self.status()
    }
    pub fn finish(self) -> Option<Vec<Line>> {
        (self.scan.status == LineReplacementStatus::Ready).then_some(self.scan.rows)
    }
}

pub(super) fn row_at(lines: &[Line], position: usize) -> usize {
    lines
        .partition_point(|line| line.start <= position)
        .saturating_sub(1)
}
pub(super) fn selected_rows(lines: &[Line], selections: &[Selection]) -> Vec<Range<usize>> {
    let mut selected = BTreeSet::new();
    for selection in selections {
        let range = selection.range();
        let first = row_at(lines, range.start);
        let mut last = row_at(lines, range.end);
        if !range.is_empty() && range.end == lines[last].start {
            last = last.saturating_sub(1);
        }
        selected.extend(first..=last);
    }
    let mut blocks: Vec<Range<usize>> = Vec::new();
    for row in selected {
        if let Some(block) = blocks.last_mut().filter(|block| block.end == row) {
            block.end += 1;
        } else {
            blocks.push(row..row + 1);
        }
    }
    blocks
}

impl Document {
    pub fn line_command(
        &mut self,
        command: LineCommand,
        ending: Option<LineEnding>,
    ) -> Result<bool, EditError> {
        let rows = &self.line_index.rows;
        let blocks = selected_rows(rows, &self.selections);
        let ending = ending
            .unwrap_or_else(|| LineEnding::detect(&self.text))
            .text();
        match command {
            LineCommand::MoveUp | LineCommand::MoveDown => {
                let mut order: Vec<_> = (0..rows.len()).collect();
                for block in &blocks {
                    if command == LineCommand::MoveUp && block.start > 0 {
                        order[block.start - 1..block.end].rotate_left(1);
                    } else if command == LineCommand::MoveDown && block.end < rows.len() {
                        order[block.start..=block.end].rotate_right(1);
                    }
                }
                let mut text = String::new();
                let mut destinations = vec![
                    Line {
                        start: 0,
                        body_end: 0,
                        end: 0
                    };
                    rows.len()
                ];
                for (position, &original) in order.iter().enumerate() {
                    let start = text.len();
                    text.push_str(&self.text[rows[original].start..rows[original].body_end]);
                    let body_end = text.len();
                    // Separators belong to logical row positions, including the
                    // final row; moving an unterminated last line is safe.
                    text.push_str(&self.text[rows[position].body_end..rows[position].end]);
                    destinations[original] = Line {
                        start,
                        body_end,
                        end: text.len(),
                    };
                }
                let map = |position, previous| {
                    let mut row = row_at(rows, position);
                    if previous && row > 0 && position == rows[row].start {
                        row -= 1;
                        return destinations[row].end;
                    }
                    destinations[row].start
                        + (position - rows[row].start).min(rows[row].body_end - rows[row].start)
                };
                let selections = self
                    .selections
                    .iter()
                    .map(|selection| {
                        let range = selection.range();
                        Selection {
                            anchor: map(
                                selection.anchor,
                                !range.is_empty() && selection.anchor == range.end,
                            ),
                            head: map(
                                selection.head,
                                !range.is_empty() && selection.head == range.end,
                            ),
                        }
                    })
                    .collect();
                let start = self
                    .text
                    .chars()
                    .zip(text.chars())
                    .take_while(|(left, right)| left == right)
                    .map(|(ch, _)| ch.len_utf8())
                    .sum::<usize>();
                let suffix = self.text[start..]
                    .chars()
                    .rev()
                    .zip(text[start..].chars().rev())
                    .take_while(|(left, right)| left == right)
                    .map(|(ch, _)| ch.len_utf8())
                    .sum::<usize>();
                self.apply(
                    vec![Edit::replace(
                        start..self.text.len() - suffix,
                        &text[start..text.len() - suffix],
                    )],
                    selections,
                    None,
                )
            }
            LineCommand::Delete => {
                let edits = blocks
                    .iter()
                    .map(|block| {
                        let start = if block.end == rows.len() && block.start > 0 {
                            rows[block.start - 1].body_end
                        } else {
                            rows[block.start].start
                        };
                        Edit::replace(start..rows[block.end - 1].end, "")
                    })
                    .collect();
                self.apply_mapped(edits)
            }
            LineCommand::Duplicate | LineCommand::DuplicateAbove => {
                let mut edits = Vec::new();
                let mut copies = Vec::new();
                let mut inserted = 0;
                for block in &blocks {
                    let start = rows[block.start].start;
                    let end = rows[block.end - 1].end;
                    let separator = if rows[block.end - 1].body_end == end {
                        ending
                    } else {
                        ""
                    };
                    let above = command == LineCommand::DuplicateAbove;
                    let text = if above {
                        format!("{}{separator}", &self.text[start..end])
                    } else {
                        format!("{separator}{}", &self.text[start..end])
                    };
                    let position = if above { start } else { end };
                    copies.push((
                        start..end,
                        position + inserted + if above { 0 } else { separator.len() },
                    ));
                    inserted += text.len();
                    edits.push(Edit::replace(position..position, text));
                }
                let selections = self
                    .selections
                    .iter()
                    .map(|selection| {
                        let range = selection.range();
                        let (source, destination) = copies
                            .iter()
                            .find(|(source, _)| {
                                source.start <= range.start && range.end <= source.end
                            })
                            .unwrap();
                        Selection {
                            anchor: destination + selection.anchor - source.start,
                            head: destination + selection.head - source.start,
                        }
                    })
                    .collect();
                self.apply(edits, selections, None)
            }
            LineCommand::InsertAbove | LineCommand::InsertBelow => {
                let heads: BTreeSet<_> = self
                    .selections
                    .iter()
                    .map(|selection| row_at(rows, selection.head))
                    .collect();
                let changes = heads
                    .into_iter()
                    .map(|row| {
                        let line = &rows[row];
                        let prefix: String = self.text[line.start..line.body_end]
                            .chars()
                            .take_while(|ch| matches!(ch, ' ' | '\t'))
                            .collect();
                        if command == LineCommand::InsertAbove {
                            (
                                Edit::replace(line.start..line.start, format!("{prefix}{ending}")),
                                Selection::caret(prefix.len()),
                            )
                        } else if line.body_end < line.end {
                            (
                                Edit::replace(line.end..line.end, format!("{prefix}{ending}")),
                                Selection::caret(prefix.len()),
                            )
                        } else {
                            (
                                Edit::replace(line.end..line.end, format!("{ending}{prefix}")),
                                Selection::caret(ending.len() + prefix.len()),
                            )
                        }
                    })
                    .collect();
                self.apply_caret_edits(changes)
            }
        }
    }

    pub fn duplicate_selections(&mut self) -> Result<bool, EditError> {
        if self
            .selections
            .iter()
            .any(|selection| selection.range().is_empty())
        {
            return self.line_command(LineCommand::Duplicate, None);
        }
        let changes = self
            .selections
            .iter()
            .map(|selection| {
                let range = selection.range();
                let length = range.len();
                (
                    Edit::replace(range.end..range.end, &self.text[range]),
                    if selection.anchor <= selection.head {
                        Selection {
                            anchor: 0,
                            head: length,
                        }
                    } else {
                        Selection {
                            anchor: length,
                            head: 0,
                        }
                    },
                )
            })
            .collect();
        self.apply_caret_edits(changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn move_duplicate_delete_preserve_unicode_selection_and_eof() {
        let mut doc = Document::new("a\r\n😀");
        doc.set_selections(vec![Selection { anchor: 7, head: 3 }])
            .unwrap();
        doc.line_command(LineCommand::MoveUp, None).unwrap();
        assert_eq!(doc.text(), "😀\r\na");
        assert_eq!(doc.selections(), &[Selection { anchor: 4, head: 0 }]);
        doc.undo();
        doc.line_command(LineCommand::Duplicate, None).unwrap();
        assert_eq!(doc.text(), "a\r\n😀\r\n😀");
        assert_eq!(
            doc.selections(),
            &[Selection {
                anchor: 13,
                head: 9
            }]
        );
        doc.undo();
        doc.line_command(LineCommand::Delete, None).unwrap();
        assert_eq!(doc.text(), "a");
        doc.undo();
        assert_eq!(doc.text(), "a\r\n😀");
    }
    #[test]
    fn line_boundaries_multiple_blocks_and_blank_lines_are_deliberate() {
        let mut doc = Document::new("a\nb\nc\nd\n");
        doc.set_selections(vec![Selection { anchor: 2, head: 4 }, Selection::caret(6)])
            .unwrap();
        doc.line_command(LineCommand::MoveUp, None).unwrap();
        assert_eq!(doc.text(), "b\na\nd\nc\n");
        assert_eq!(doc.selections()[0], Selection { anchor: 0, head: 2 });
        doc.undo();
        doc.set_selections(vec![Selection::caret(doc.text().len())])
            .unwrap();
        doc.line_command(LineCommand::Delete, None).unwrap();
        assert_eq!(doc.text(), "a\nb\nc\nd");
        let mut doc = Document::new("  x");
        doc.line_command(LineCommand::InsertBelow, Some(LineEnding::CrLf))
            .unwrap();
        assert_eq!(doc.text(), "  x\r\n  ");
        assert_eq!(doc.selections()[0].head, 7);
    }
    #[test]
    fn every_logical_line_round_trips_moves_and_duplicate_delete() {
        for original in ["a\n\n😀", "a\r\nb\r\n", "aaa\nb\naaa\nlast"] {
            let rows = lines(original);
            for row in &rows {
                let mut doc = Document::new(original);
                doc.set_selections(vec![Selection::caret(row.start)])
                    .unwrap();
                doc.line_command(LineCommand::Duplicate, None).unwrap();
                doc.line_command(LineCommand::Delete, None).unwrap();
                assert_eq!(doc.text(), original);
            }
            for row in rows.iter().skip(1) {
                let mut doc = Document::new(original);
                doc.set_selections(vec![Selection::caret(row.start)])
                    .unwrap();
                doc.line_command(LineCommand::MoveUp, None).unwrap();
                doc.line_command(LineCommand::MoveDown, None).unwrap();
                assert_eq!(doc.text(), original);
                assert_eq!(doc.selections(), &[Selection::caret(row.start)]);
            }
        }
    }

    #[test]
    fn snippet_duplication_is_one_undoable_transaction() {
        let mut doc = Document::new("a😀z");
        doc.set_selections(vec![Selection { anchor: 5, head: 1 }])
            .unwrap();
        doc.duplicate_selections().unwrap();
        assert_eq!(doc.text(), "a😀😀z");
        assert_eq!(doc.selections(), &[Selection { anchor: 9, head: 5 }]);
        doc.undo();
        assert_eq!(doc.text(), "a😀z");
    }
}

#[cfg(all(test, feature = "editor-parser"))]
mod publication_tests {
    use super::*;

    #[test]
    fn batched_index_matches_complete_unicode_crlf_indexes_for_all_edit_positions() {
        for old in ["", "文😀", "a\r\nb\n", "a\n\nlast", "\r\n", "first\nlast\r"] {
            let original = lines(old);
            let positions = old
                .char_indices()
                .map(|(index, _)| index)
                .chain(std::iter::once(old.len()))
                .collect::<Vec<_>>();
            for &start in &positions {
                for &end in positions.iter().filter(|&&end| end >= start) {
                    for inserted in ["", "😀", "\n", "\r\n", "文\r\nnew\n"] {
                        let source = format!("{}{inserted}{}", &old[..start], &old[end..]);
                        let edit = LineEdit::new(
                            &original,
                            old.len(),
                            source.len(),
                            start..end,
                            start + inserted.len(),
                        );
                        let replacement = line_iter(&source[edit.bytes.clone()])
                            .filter(|row| !edit.trim_suffix_row(Some(row)))
                            .collect::<Vec<_>>();
                        let mut synchronous = original.clone();
                        edit.apply(&mut synchronous, replacement.clone());
                        assert_eq!(synchronous, lines(&source));
                        for budget in [1, 2, 256] {
                            let mut job = edit.prepare_publication(replacement.clone());
                            assert!(!job.advance(&original, 0));
                            loop {
                                let before = job.next.len();
                                let complete = job.advance(&original, budget);
                                assert!(job.next.len() - before <= budget);
                                assert_eq!(
                                    original,
                                    lines(old),
                                    "pending publication preserves its base"
                                );
                                if complete {
                                    break;
                                }
                            }
                            assert_eq!(
                                job.finish().unwrap(),
                                lines(&source),
                                "{old:?} {start}..{end} {inserted:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn abandoning_large_prefix_and_suffix_publication_keeps_original_coordinates() {
        let old = "文😀\r\n".repeat(40_000);
        let original = lines(&old);
        for row in [0, 20_000, 39_999] {
            let start = original[row].start;
            let source = format!("{}new\n{}", &old[..start], &old[start..]);
            let edit = LineEdit::new(&original, old.len(), source.len(), start..start, start + 4);
            let replacement = line_iter(&source[edit.bytes.clone()])
                .filter(|row| !edit.trim_suffix_row(Some(row)))
                .collect();
            let mut job = edit.prepare_publication(replacement);
            assert!(!job.advance(&original, 256));
            assert_eq!(job.next.len(), 256);
            assert!(job.finish().is_none());
            assert_eq!(original, lines(&old));
        }
    }
}

#[cfg(all(test, feature = "editor-parser"))]
mod replacement_tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn bounded_replacements_match_complete_rows_and_limits_without_copying_sources() {
        for body in [
            String::new(),
            "文😀".repeat(100_000),
            "\r".into(),
            "\n".into(),
            "文😀\r\n\nlast\r".into(),
            "x\r\n".repeat(1_000),
        ] {
            for suffix in [false, true] {
                let source = Arc::new(format!("prefix\n{body}suffix"));
                let bytes = 7..7 + body.len();
                let edit = LineEdit {
                    rows: 0..0,
                    bytes: bytes.clone(),
                    has_suffix: suffix,
                    old_end: 0,
                };
                let expected = line_iter(&body)
                    .filter(|row| !edit.trim_suffix_row(Some(row)))
                    .collect::<Vec<_>>();
                for limit in [
                    0,
                    expected.len().saturating_sub(1),
                    expected.len(),
                    expected.len() + 1,
                ] {
                    let expected = (expected.len() <= limit).then(|| expected.clone());
                    assert_eq!(edit.replacement(&source, limit), expected);
                    for budget in [1, 7, 64 * 1024] {
                        let mut job = edit.prepare_replacement(source.clone(), limit);
                        assert!(Arc::ptr_eq(&job.source, &source));
                        assert_eq!(job.advance(0), LineReplacementStatus::Pending);
                        while job.status() == LineReplacementStatus::Pending {
                            let next = job.scan.next;
                            let rows = job.scan.rows.len();
                            job.advance(budget);
                            assert!(job.scan.next - next <= budget);
                            assert!(job.scan.rows.len() - rows <= 256);
                        }
                        assert_eq!(job.finish(), expected);
                    }
                }
            }
        }
    }

    #[test]
    fn abandoned_replacement_releases_its_source_without_publishing_rows() {
        let source = Arc::new("文😀\r\n".repeat(1_000));
        let weak = Arc::downgrade(&source);
        let mut job = LineReplacement::new(source.clone(), 0..source.len(), false, 2_000);
        assert_eq!(job.advance(7), LineReplacementStatus::Pending);
        drop(source);
        assert!(weak.upgrade().is_some());
        assert!(job.finish().is_none());
        assert!(weak.upgrade().is_none());
    }
}
