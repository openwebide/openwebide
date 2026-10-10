//! Incremental logical-line and UTF-16 coordinates owned by the source document.
use super::{
    EditError, Selection,
    coordinates::{LineCoordinatePreparation, LineCoordinates},
    lines::{Line, LineEdit, row_at},
};
use std::{ops::Range, sync::Arc};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LineIndex {
    pub rows: Vec<Line>,
    offsets: Vec<(usize, usize, usize)>,
    oversized_rows: Vec<bool>,
    oversized_count: usize,
    breaks: usize,
    pub coordinates: Arc<Vec<LineCoordinates>>,
    utf16_len: usize,
    textarea_len: usize,
    guides: RowCache<CachedGuides>,
    visible_rows: RowCache<Arc<Vec<super::VisibleLine>>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LineEndings {
    pub carriage_returns: bool,
    pub uniform_rows: bool,
}

#[derive(Clone, Debug)]
struct CachedGuides {
    limited: bool,
    indentation: super::Indentation,
    columns: std::sync::Arc<[usize]>,
}

// Presentation caches are excluded from logical row identity and history.
#[derive(Debug)]
struct RowCache<T>(std::sync::Mutex<Option<T>>);
impl<T> Default for RowCache<T> {
    fn default() -> Self {
        Self(std::sync::Mutex::new(None))
    }
}
impl<T: Clone> Clone for RowCache<T> {
    fn clone(&self) -> Self {
        Self(std::sync::Mutex::new(
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
        ))
    }
}
impl<T> PartialEq for RowCache<T> {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}
impl<T> Eq for RowCache<T> {}

/// Initial row discovery and coordinates share one complete-only construction
/// path. Retained indexes are never modified while a replacement is preparing.
pub(super) struct LineIndexPreparation<'a> {
    source: &'a str,
    index: LineIndex,
    coordinates: Vec<LineCoordinates>,
    row_start: usize,
    scan: usize,
    active: Option<(Line, LineCoordinatePreparation<'a>)>,
    done: bool,
}
impl<'a> LineIndexPreparation<'a> {
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            index: LineIndex {
                rows: Vec::new(),
                offsets: Vec::new(),
                oversized_rows: Vec::new(),
                oversized_count: 0,
                breaks: 0,
                coordinates: Arc::new(Vec::new()),
                utf16_len: 0,
                textarea_len: 0,
                guides: RowCache::default(),
                visible_rows: RowCache::default(),
            },
            coordinates: Vec::new(),
            row_start: 0,
            scan: 0,
            active: None,
            done: false,
        }
    }
    /// One unit discovers at most 8 KiB of source, or advances one bounded row
    /// coordinate step. EOF/newline handling and complete publication are shared
    /// by synchronous construction and cooperative hosts.
    pub fn advance(&mut self, budget: usize) -> bool {
        for _ in 0..budget {
            if self.done {
                break;
            }
            if let Some((_, coordinates)) = &mut self.active {
                if !coordinates.advance(1) {
                    continue;
                }
                let (row, coordinates) = self.active.take().unwrap();
                let (coordinates, summary) = coordinates.finish().unwrap();
                let text = &self.source[row.start..row.end];
                self.index.offsets.push((
                    self.index.utf16_len,
                    self.index.textarea_len,
                    self.index.breaks,
                ));
                self.index.breaks += summary.breaks;
                self.index.oversized_count += usize::from(summary.oversized);
                self.index.oversized_rows.push(summary.oversized);
                self.coordinates.push(coordinates);
                self.index.utf16_len += summary.utf16_len;
                self.index.textarea_len += summary.utf16_len - usize::from(text.ends_with("\r\n"));
                self.done = row.end == self.source.len()
                    && (row.start == row.end || !self.source.ends_with('\n'));
                self.row_start = row.end;
                self.scan = row.end;
                self.index.rows.push(row);
                continue;
            }
            let scan_end = self.scan.saturating_add(8192).min(self.source.len());
            let newline = self.source.as_bytes()[self.scan..scan_end]
                .iter()
                .position(|byte| *byte == b'\n');
            self.scan = newline.map_or(scan_end, |offset| self.scan + offset + 1);
            if newline.is_some() || self.scan == self.source.len() {
                let text = &self.source[self.row_start..self.scan];
                let ending = if text.ends_with("\r\n") {
                    2
                } else {
                    usize::from(text.ends_with('\n'))
                };
                let row = Line {
                    start: self.row_start,
                    body_end: self.scan - ending,
                    end: self.scan,
                };
                self.active = Some((row, LineCoordinatePreparation::new(text)));
            }
        }
        self.done
    }
    pub fn finish(mut self) -> Option<LineIndex> {
        if !self.done {
            return None;
        }
        self.index.coordinates = Arc::new(self.coordinates);
        Some(self.index)
    }
}

impl LineIndex {
    pub fn new(source: &str) -> Self {
        let mut preparation = LineIndexPreparation::new(source);
        preparation.advance(usize::MAX);
        preparation.finish().unwrap()
    }

    /// Re-scan changed logical rows, preserving all unchanged suffix coordinates.
    /// The edit envelope includes every replacement in one overlapping row batch.
    pub fn update(&mut self, old_len: usize, new: &str, changed: Range<usize>, new_end: usize) {
        let old_rows = self.rows.len();
        let changed_start = changed.start;
        let changed_end = changed.end;
        let mut edit = LineEdit::new(&self.rows, old_len, new.len(), changed, new_end);
        // The conservative envelope includes the preceding logical row. Its
        // complete LF/CRLF ending is unchanged when it precedes the replacement,
        // so retain its admission summary and exact native/visual coordinates.
        // An unterminated row or an edit inside its ending must still be scanned.
        let prefix = &self.rows[edit.rows.start];
        if edit.rows.len() > 1 && prefix.body_end < prefix.end && prefix.end <= changed_start {
            edit.rows.start += 1;
            edit.bytes.start = prefix.end;
        }
        edit.retain_suffix(&self.rows, changed_end, new_end, new);
        let (start_row, end_row) = (edit.rows.start, edit.rows.end);
        let (start, end) = (edit.bytes.start, edit.bytes.end);
        let mut replacement = Self::new(&new[start..end]);
        if edit.trim_suffix_row(replacement.rows.last()) {
            replacement.rows.pop();
            replacement.offsets.pop();
            replacement.oversized_rows.pop();
            Arc::make_mut(&mut replacement.coordinates).pop();
        }
        let replacement_rows = replacement.rows.len();
        let (raw_start, native_start, breaks_start) = self.offsets[start_row];
        let (raw_end, native_end, breaks_end) = self.offsets.get(end_row).copied().unwrap_or((
            self.utf16_len,
            self.textarea_len,
            self.breaks,
        ));
        let next_raw = raw_start + replacement.utf16_len;
        let next_native = native_start + replacement.textarea_len;
        let next_breaks = breaks_start + replacement.breaks;
        for (raw, native, breaks) in &mut self.offsets[end_row..] {
            *breaks = next_breaks + (*breaks - breaks_end);
            *raw = next_raw + (*raw - raw_end);
            *native = next_native + (*native - native_end);
        }
        for (raw, native, breaks) in &mut replacement.offsets {
            *breaks += breaks_start;
            *raw += raw_start;
            *native += native_start;
        }
        self.breaks = next_breaks + (self.breaks - breaks_end);
        self.oversized_count = self.oversized_count
            - self.oversized_rows[start_row..end_row]
                .iter()
                .filter(|invalid| **invalid)
                .count()
            + replacement.oversized_count;
        self.oversized_rows
            .splice(start_row..end_row, replacement.oversized_rows);
        self.utf16_len = next_raw + (self.utf16_len - raw_end);
        self.textarea_len = next_native + (self.textarea_len - native_end);
        edit.apply(&mut self.rows, replacement.rows);
        self.offsets.splice(start_row..end_row, replacement.offsets);
        Arc::make_mut(&mut self.coordinates).splice(
            start_row..end_row,
            Arc::unwrap_or_clone(replacement.coordinates),
        );
        self.update_visible_rows(
            start_row..end_row,
            replacement_rows,
            old_len != new.len() || next_native != native_end,
        );
        self.update_guides(old_len, new, start_row..end_row, replacement_rows, old_rows);
    }

    fn visible_line(&self, row: usize) -> super::VisibleLine {
        let source = &self.rows[row];
        super::VisibleLine {
            source_line: row,
            source: source.start..source.end,
            visible_start: source.start,
            textarea_start: self.offsets[row].1,
        }
    }

    pub fn visible_rows(&self) -> Arc<Vec<super::VisibleLine>> {
        self.visible_rows
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert_with(|| {
                Arc::new(
                    (0..self.rows.len())
                        .map(|row| self.visible_line(row))
                        .collect(),
                )
            })
            .clone()
    }

    fn update_visible_rows(&self, changed: Range<usize>, replacement_rows: usize, shifted: bool) {
        let mut cached = self
            .visible_rows
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(rows) = cached.as_mut() else {
            return;
        };
        let rows = Arc::make_mut(rows);
        // One inserted line must not double a multi-megabyte retained table.
        const ROW_HEADROOM: usize = 256;
        let required = rows.len() - changed.len() + replacement_rows;
        if required > rows.capacity() {
            rows.reserve_exact(required + ROW_HEADROOM - rows.len());
        }
        if shifted || replacement_rows != changed.len() {
            for (offset, row) in rows[changed.end..].iter_mut().enumerate() {
                *row = self.visible_line(changed.start + replacement_rows + offset);
            }
        }
        rows.splice(
            changed.clone(),
            (changed.start..changed.start + replacement_rows).map(|row| self.visible_line(row)),
        );
        if rows.capacity() > rows.len().saturating_mul(2) + ROW_HEADROOM {
            rows.shrink_to(rows.len() + ROW_HEADROOM);
        }
    }

    fn guide_edit_rows(&self, source: &str, changed: Range<usize>) -> Range<usize> {
        let blank = |row: usize| {
            source[self.rows[row].start..self.rows[row].end]
                .chars()
                .all(char::is_whitespace)
        };
        // Blank guides depend on the nearest nonblank row in both directions.
        let mut start = changed.start;
        while start > 0 {
            start -= 1;
            if !blank(start) {
                break;
            }
        }
        let mut end = changed.end;
        while end < self.rows.len() {
            let last = !blank(end);
            end += 1;
            if last {
                break;
            }
        }
        start..end
    }

    fn update_guides(
        &mut self,
        old_len: usize,
        source: &str,
        changed: Range<usize>,
        replacement_rows: usize,
        old_rows: usize,
    ) {
        let limit = super::MAX_STRUCTURE_BYTES;
        if old_len > limit && source.len() > limit && old_rows == self.rows.len() {
            return;
        }
        let cached = self
            .guides
            .0
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some(mut cached) = cached.filter(|cache| !cache.limited && source.len() <= limit)
        else {
            return;
        };
        let replacement_end = changed.start + replacement_rows;
        let rows = self.guide_edit_rows(source, changed.start..replacement_end);
        let old_end = changed.end + (rows.end - replacement_end);
        let columns = super::navigation::guide_columns(
            self.rows[rows.clone()]
                .iter()
                .map(|row| &source[row.start..row.end]),
            cached.indentation,
        );
        if cached.columns[rows.start..old_end] != columns {
            let mut updated = Vec::with_capacity(self.rows.len());
            updated.extend_from_slice(&cached.columns[..rows.start]);
            updated.extend(columns);
            updated.extend_from_slice(&cached.columns[old_end..]);
            cached.columns = updated.into();
        }
        *self
            .guides
            .0
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(cached);
    }

    pub fn guide_columns(
        &self,
        source: &str,
        indentation: super::Indentation,
    ) -> std::sync::Arc<[usize]> {
        let limited = source.len() > super::MAX_STRUCTURE_BYTES;
        let mut cache = self
            .guides
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(guides) = cache.as_ref()
            && guides.limited == limited
            && (limited || guides.indentation == indentation)
        {
            return guides.columns.clone();
        }
        let columns: std::sync::Arc<[usize]> = if limited {
            vec![0; self.rows.len()].into()
        } else {
            super::navigation::guide_columns(
                self.rows.iter().map(|row| &source[row.start..row.end]),
                indentation,
            )
            .into()
        };
        *cache = Some(CachedGuides {
            limited,
            indentation,
            columns: columns.clone(),
        });
        columns
    }

    pub fn admitted(&self, bytes: usize) -> bool {
        bytes <= super::MAX_EDITOR_BYTES
            && self.breaks < super::MAX_EDITOR_LINES
            && self.oversized_count == 0
    }

    pub fn row_breaks(&self, start: usize, end: usize) -> usize {
        self.offsets.get(end).map_or(self.breaks, |offset| offset.2) - self.offsets[start].2
    }

    /// Derive native normalization and fixed-row eligibility from indexed
    /// CRLF suppression and display-break counts, without reading source bytes.
    pub fn line_endings(&self, rows: Range<usize>) -> LineEndings {
        debug_assert!(rows.start <= rows.end && rows.end <= self.rows.len());
        let total = (self.utf16_len, self.textarea_len, self.breaks);
        let first = self.offsets.get(rows.start).copied().unwrap_or(total);
        let last = self.offsets.get(rows.end).copied().unwrap_or(total);
        let logical_breaks =
            rows.end.min(self.rows.len() - 1) - rows.start.min(self.rows.len() - 1);
        let uniform_rows = last.2 - first.2 == logical_breaks;
        LineEndings {
            carriage_returns: last.0 - first.0 != last.1 - first.1 || !uniform_rows,
            uniform_rows,
        }
    }

    pub fn native_line_len(&self, row: usize) -> usize {
        self.offsets
            .get(row + 1)
            .map_or(self.textarea_len, |offset| offset.1)
            - self.offsets[row].1
    }
    pub fn byte_to_textarea(&self, text: &str, offset: usize) -> Result<usize, EditError> {
        if offset > text.len() || !text.is_char_boundary(offset) {
            return Err(EditError::InvalidSelection);
        }
        let row = row_at(&self.rows, offset);
        let line = &self.rows[row];
        // Include the following LF when the offset lies between CR and LF, so
        // the shared native mapping keeps the CR zero-width.
        let local = self.coordinates[row]
            .byte_to_textarea(&text[line.start..line.end], offset - line.start)?;
        Ok(self.offsets[row].1 + local)
    }
    pub fn textarea_to_byte(&self, text: &str, offset: usize) -> usize {
        let row = self
            .offsets
            .partition_point(|prefix| prefix.1 <= offset)
            .saturating_sub(1);
        let line = &self.rows[row];
        line.start
            + self.coordinates[row].textarea_to_byte(
                &text[line.start..line.end],
                offset.saturating_sub(self.offsets[row].1),
            )
    }
    pub fn native_selection(&self, text: &str, selection: Selection) -> Selection {
        Selection {
            anchor: self.textarea_to_byte(text, selection.anchor),
            head: self.textarea_to_byte(text, selection.head),
        }
    }
    pub fn line_column(&self, text: &str, offset: usize) -> (usize, usize) {
        let offset = text.floor_char_boundary(offset.min(text.len()));
        let row = row_at(&self.rows, offset);
        (
            row + 1,
            self.coordinates[row].chars_before(
                &text[self.rows[row].start..self.rows[row].end],
                offset.min(self.rows[row].body_end) - self.rows[row].start,
            ) + 1,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cooperative_line_tables_preserve_eof_rows_and_exact_native_coordinates() {
        for source in [
            String::new(),
            "x".into(),
            "\n".into(),
            "\r\n".into(),
            "head 文😀\r\nbody\rword\n".repeat(300),
            format!("{}\r\nlast\n", "文😀e\u{301}\t".repeat(9000)),
            format!("👩{}\u{200d}👩\r\n", "\u{301}".repeat(40_000)),
        ] {
            let expected_rows = super::super::lines::lines(&source);
            let (breaks, _) = super::super::capacity::row_admission(&source);
            for budget in [1, 8, 64] {
                assert!(LineIndexPreparation::new(&source).finish().is_none());
                let mut prep = LineIndexPreparation::new(&source);
                assert!(!prep.advance(0));
                let mut turns = 0;
                loop {
                    let rows = prep.index.rows.len();
                    let done = prep.advance(budget);
                    assert!(prep.index.rows.len() - rows <= budget);
                    turns += 1;
                    if done {
                        break;
                    }
                    assert!(turns < source.len() * 4 + 3);
                }
                let index = prep.finish().unwrap();
                assert_eq!(index.rows, expected_rows);
                assert_eq!(index.utf16_len, source.encode_utf16().count());
                assert_eq!(
                    index.textarea_len,
                    super::super::byte_to_textarea(&source, source.len()).unwrap()
                );
                assert_eq!(index.breaks, breaks);
                for byte in source
                    .char_indices()
                    .map(|(byte, _)| byte)
                    .step_by((source.len() / 97).max(1))
                    .chain([source.len()])
                {
                    let expected = super::super::byte_to_textarea(&source, byte).unwrap();
                    assert_eq!(index.byte_to_textarea(&source, byte), Ok(expected));
                    assert_eq!(
                        index.line_column(&source, byte),
                        super::super::line_column(&source, byte)
                    );
                    for native in expected.saturating_sub(1)..=expected + 1 {
                        assert_eq!(
                            index.textarea_to_byte(&source, native),
                            super::super::textarea_to_byte(&source, native)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn edits_ending_at_row_boundaries_retain_only_separate_suffix_coordinates() {
        let body = "文😀e\u{301}\t words ".repeat(6000);
        for ending in ["\n", "\r\n"] {
            for tail in ["", ending, "\nlast"] {
                let prefix = format!("head{ending}");
                let source = format!("{prefix}{body}{tail}");
                for (start, replacement, retain) in [
                    (0, "new\n", true),
                    (0, "new\r\n", true),
                    (4, "\n", true),
                    (prefix.len(), "new\n", true),
                    (0, "", true),
                    (4, "", false),
                    (0, "new\r", false),
                    (prefix.len(), "new", false),
                ] {
                    let mut index = LineIndex::new(&source);
                    let snapshot = index.clone();
                    let retained = index.coordinates[1].visual().unwrap();
                    let mut next = source.clone();
                    next.replace_range(start..prefix.len(), replacement);
                    index.update(
                        source.len(),
                        &next,
                        start..prefix.len(),
                        start + replacement.len(),
                    );
                    assert_eq!(index, LineIndex::new(&next));
                    assert_eq!(snapshot, LineIndex::new(&source));
                    let row = row_at(&index.rows, start + replacement.len());
                    assert_eq!(
                        index.coordinates[row]
                            .visual()
                            .unwrap()
                            .shared_with(&retained),
                        retain
                    );
                }
            }
        }
    }

    #[test]
    fn edits_after_complete_long_rows_retain_exact_coordinates() {
        let body = "文😀e\u{301}\t words ".repeat(6000);
        for ending in ["\n", "\r\n"] {
            let prefix = format!("{body}{ending}");
            let source = format!("{prefix}next{ending}tail");
            for (range, replacement) in [
                (prefix.len()..prefix.len(), "😀"),
                (prefix.len()..prefix.len() + 4, ""),
                (prefix.len() + 2..prefix.len() + 2, "\r\nnew\n"),
                (source.len()..source.len(), "\nmore"),
            ] {
                let mut index = LineIndex::new(&source);
                let retained = index.coordinates[0].visual().unwrap();
                let snapshot = index.clone();
                let mut next = source.clone();
                next.replace_range(range.clone(), replacement);
                index.update(
                    source.len(),
                    &next,
                    range.clone(),
                    range.start + replacement.len(),
                );
                assert_eq!(index, LineIndex::new(&next));
                assert!(
                    index.coordinates[0]
                        .visual()
                        .unwrap()
                        .shared_with(&retained)
                );
                assert_eq!(snapshot, LineIndex::new(&source));
            }
        }
    }

    #[test]
    fn row_envelope_reuse_matches_fresh_indexes_at_unicode_and_ending_boundaries() {
        for source in [
            "",
            "x",
            "\n",
            "\r\n",
            "文😀\r\n\t\rword\nlast",
            "a\n\n😀\r\n",
        ] {
            let boundaries: Vec<_> = source
                .char_indices()
                .map(|(byte, _)| byte)
                .chain([source.len()])
                .collect();
            for &start in &boundaries {
                for &end in boundaries.iter().filter(|&&end| end >= start) {
                    for replacement in ["", "x", "文😀", "\r", "\n", "\r\n", "\n\r", "x\r\ny"] {
                        let mut index = LineIndex::new(source);
                        let mut next = source.to_owned();
                        next.replace_range(start..end, replacement);
                        index.update(source.len(), &next, start..end, start + replacement.len());
                        assert_eq!(
                            index,
                            LineIndex::new(&next),
                            "{source:?}: {start}..{end} -> {replacement:?}"
                        );
                    }
                }
            }
        }
    }

    fn reference_guides(source: &str, indentation: super::super::Indentation) -> Vec<usize> {
        let rows: Vec<_> = source.split('\n').collect();
        if source.len() > super::super::MAX_STRUCTURE_BYTES {
            return vec![0; rows.len()];
        }
        let mut columns: Vec<_> = rows
            .iter()
            .map(|row| {
                let prefix = row
                    .bytes()
                    .take_while(|ch| matches!(ch, b' ' | b'\t'))
                    .count();
                indentation.visual_width(&row[..prefix]) / indentation.width() * indentation.width()
            })
            .collect();
        let mut following = vec![0; rows.len()];
        let mut next = 0;
        for row in (0..rows.len()).rev() {
            following[row] = next;
            if !rows[row].trim().is_empty() {
                next = columns[row];
            }
        }
        let mut previous = 0;
        for (row, text) in rows.iter().enumerate() {
            if text.trim().is_empty() {
                columns[row] = previous.min(following[row]);
            } else {
                previous = columns[row];
            }
        }
        columns
    }

    #[test]
    fn indexed_guides_share_blank_continuation_tabs_unicode_and_endings() {
        use super::super::{Document, Edit, Indentation, Selection};
        for source in [
            "",
            "  first\n\n    next\n",
            "\n\tparent\r\n \tchild\r\n\u{2003}\r\n    sibling\r\nlast\r\n",
            "\t\tdeep\n \t \tchild\n    \n\tpeer\n",
            "\r\n \r\n\n",
            "    文😀\n\u{2003}blank?\n\t \n  next",
        ] {
            for (width, tab_width) in [(2, 4), (4, 8), (3, 2), (0, 0)] {
                let indentation = Indentation {
                    width,
                    tab_width,
                    ..Default::default()
                };
                let mut document = Document::new(source);
                let expected = reference_guides(source, indentation);
                let guides = document.indent_guide_columns(indentation);
                assert_eq!(guides.as_ref(), expected);
                assert_eq!(
                    super::super::indent_guide_columns(source, indentation),
                    expected
                );
                assert!(std::sync::Arc::ptr_eq(
                    &guides,
                    &document.indent_guide_columns(indentation)
                ));
                let original = document.clone();
                assert!(std::sync::Arc::ptr_eq(
                    &guides,
                    &original.indent_guide_columns(indentation)
                ));
                document
                    .apply(
                        vec![Edit::replace(0..0, "\tchanged\r\n\n")],
                        vec![Selection::caret(0)],
                        None,
                    )
                    .unwrap();
                assert_eq!(
                    document.indent_guide_columns(indentation).as_ref(),
                    reference_guides(document.text(), indentation)
                );
                assert_eq!(
                    guides.as_ref(),
                    expected,
                    "retained snapshots cannot change"
                );
                assert!(document.undo());
                assert_eq!(
                    document.indent_guide_columns(indentation).as_ref(),
                    expected
                );
                assert_eq!(
                    original.indent_guide_columns(indentation).as_ref(),
                    expected
                );
            }
        }
    }

    #[test]
    fn disabled_guides_reuse_tables_across_edits_until_rows_or_limit_change() {
        use super::super::{Document, Edit, Indentation, MAX_STRUCTURE_BYTES, Selection};
        let row = "    let body = \"文😀\"; // a longer source line\r\n";
        let mut document = Document::new(row.repeat(MAX_STRUCTURE_BYTES / row.len() + 1));
        let indentation = Indentation::default();
        let guides = document.indent_guide_columns(indentation);
        assert!(guides.iter().all(|column| *column == 0));
        document
            .apply(
                vec![Edit::replace(4..7, "record")],
                vec![Selection::caret(10)],
                None,
            )
            .unwrap();
        assert!(std::sync::Arc::ptr_eq(
            &guides,
            &document.indent_guide_columns(indentation)
        ));
        assert!(std::sync::Arc::ptr_eq(
            &guides,
            &document.indent_guide_columns(Indentation {
                width: 2,
                tab_width: 8,
                ..Default::default()
            })
        ));
        let end = document.text().len();
        document
            .apply(
                vec![Edit::replace(end..end, "\n")],
                vec![Selection::caret(end + 1)],
                None,
            )
            .unwrap();
        let expanded = document.indent_guide_columns(indentation);
        assert_eq!(expanded.len(), guides.len() + 1);
        assert!(!std::sync::Arc::ptr_eq(&guides, &expanded));
        let end = document.text().len();
        document
            .apply(
                vec![Edit::replace(0..end, "    child\r\n\tparent\r\n")],
                vec![Selection::caret(0)],
                None,
            )
            .unwrap();
        assert_eq!(
            document.indent_guide_columns(indentation).as_ref(),
            [4, 4, 0]
        );
    }

    #[test]
    fn guide_edits_match_full_policy_across_blank_runs_and_row_changes() {
        use super::super::Indentation;
        for source in [
            "",
            "  a\n\n    b\n",
            "\n\t文\r\n\u{2003}\r\n  b\r\n",
            "\n\n\n",
            "  a\n \n\t\n    b\n\nlast",
        ] {
            let boundaries: Vec<_> = source
                .char_indices()
                .map(|(offset, _)| offset)
                .chain(std::iter::once(source.len()))
                .collect();
            for indentation in [
                Indentation::default(),
                Indentation {
                    width: 3,
                    tab_width: 8,
                    ..Default::default()
                },
            ] {
                let original = LineIndex::new(source);
                let retained = original.guide_columns(source, indentation);
                for (at, start) in boundaries.iter().copied().enumerate() {
                    for end in boundaries[at..].iter().copied() {
                        for replacement in ["", "word", "\t", "\n", "\r\n\u{2003}\n", "  x\n\n"] {
                            let mut next = source.to_owned();
                            next.replace_range(start..end, replacement);
                            let mut index = original.clone();
                            index.update(
                                source.len(),
                                &next,
                                start..end,
                                start + replacement.len(),
                            );
                            assert_eq!(
                                index.guide_columns(&next, indentation).as_ref(),
                                reference_guides(&next, indentation),
                                "source={source:?}, edit={start}..{end}, replacement={replacement:?}"
                            );
                            assert_eq!(retained.as_ref(), reference_guides(source, indentation));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn guide_edits_visit_local_rows_and_retain_unchanged_values() {
        use super::super::Indentation;
        let source = "    body\r\n".repeat(10_000);
        let mut index = LineIndex::new(&source);
        let indentation = Indentation::default();
        let retained = index.guide_columns(&source, indentation);
        let at = index.rows[5000].start + 4;
        let mut next = source.clone();
        next.replace_range(at..at + 4, "changed");
        index.update(source.len(), &next, at..at + 4, at + 7);
        assert!(std::sync::Arc::ptr_eq(
            &retained,
            &index.guide_columns(&next, indentation)
        ));
        assert_eq!(index.guide_edit_rows(&next, 4999..5001), 4998..5002);
        let start = index.rows[5000].start;
        let mut indented = next.clone();
        indented.insert_str(start, "    ");
        index.update(next.len(), &indented, start..start, start + 4);
        let updated = index.guide_columns(&indented, indentation);
        assert!(!std::sync::Arc::ptr_eq(&retained, &updated));
        assert_eq!(updated[5000], 8);
        assert_eq!(updated.as_ref(), reference_guides(&indented, indentation));
        assert_eq!(retained[5000], 4);
    }

    #[test]
    fn visual_source_metadata_is_shared_and_rebuilt_only_for_changed_rows() {
        let body = "文😀e\u{301}\t words ".repeat(7000);
        let old = format!("head\r\n{body}\r\n{body}א\r\n");
        let mut index = LineIndex::new(&old);
        let ltr = index.coordinates[1].visual().unwrap();
        let rtl = index.coordinates[2].visual().unwrap();
        assert!(ltr.horizontal_paint_bounds(0.0, 400.0).is_some());
        assert!(rtl.horizontal_paint_bounds(0.0, 400.0).is_none());
        let next = old.replacen("head", "header changed", 1);
        index.update(old.len(), &next, 0..4, "header changed".len());
        assert!(index.coordinates[1].visual().unwrap().shared_with(&ltr));
        assert!(index.coordinates[2].visual().unwrap().shared_with(&rtl));
        let projection = super::super::FoldProjection::indexed(
            &next,
            &super::super::FoldState::default(),
            &index,
        );
        assert!(projection.visual_line_index(1).unwrap().shared_with(&ltr));
        let at = next.find(&body).unwrap();
        let mut changed = next.clone();
        changed.insert(at, 'א');
        index.update(next.len(), &changed, at..at, at + 'א'.len_utf8());
        let updated = index.coordinates[1].visual().unwrap();
        assert!(!updated.shared_with(&ltr));
        assert!(updated.horizontal_paint_bounds(0.0, 400.0).is_none());
        assert!(index.coordinates[2].visual().unwrap().shared_with(&rtl));
        assert_eq!(index, LineIndex::new(&changed));
    }
    #[test]
    fn long_row_coordinates_survive_edits_history_composition_and_folds() {
        use super::super::{Document, Edit, FoldCommand, FoldRange, NativeInputKind};
        fn verify(document: &Document) {
            assert_eq!(*document.line_index, LineIndex::new(document.text()));
            let projection = document.projection();
            for byte in document
                .text()
                .char_indices()
                .map(|(byte, _)| byte)
                .chain([document.text().len()])
                .step_by(97)
            {
                assert_eq!(
                    document.byte_to_textarea(byte),
                    super::super::byte_to_textarea(document.text(), byte)
                );
                assert_eq!(
                    document.line_column(byte),
                    super::super::line_column(document.text(), byte)
                );
            }
            for byte in projection
                .text()
                .char_indices()
                .map(|(byte, _)| byte)
                .chain([projection.text().len()])
                .step_by(83)
            {
                let native = super::super::byte_to_textarea(projection.text(), byte).unwrap();
                assert_eq!(projection.byte_to_textarea(byte).unwrap(), native);
                for near in native.saturating_sub(1)..=native + 1 {
                    assert_eq!(
                        projection.textarea_to_byte(near),
                        super::super::textarea_to_byte(projection.text(), near)
                    );
                }
            }
        }
        let long = "文😀e\u{301}\t\rword ".repeat(600);
        let source = format!("header {{\r\n{long}\r\n}}\r\n{long}\r\n");
        let mut document = Document::new(&source);
        verify(&document);
        document
            .apply(
                vec![
                    Edit::replace(0..0, "😀 new\r\n"),
                    Edit::replace(source.len()..source.len(), &long),
                ],
                vec![Selection::caret(0)],
                None,
            )
            .unwrap();
        verify(&document);
        assert!(document.undo());
        verify(&document);
        assert!(document.redo());
        verify(&document);
        assert!(document.begin_composition(Some(1)));
        document
            .native_input(
                &format!("文{}", document.text()),
                Selection::caret(3),
                NativeInputKind::Insert,
                Some(1),
            )
            .unwrap();
        verify(&document);
        document.cancel_composition();
        verify(&document);
        assert!(document.undo());
        document.set_fold_ranges(vec![FoldRange {
            start_line: 0,
            end_line: 2,
        }]);
        document.fold_command(FoldCommand::CollapseAll);
        verify(&document);
        assert!(document.projection().is_folded());
    }
    #[test]
    fn transactions_grouped_history_and_composition_keep_the_index_exact() {
        use super::super::{Document, Edit};
        fn verify(document: &Document) {
            assert_eq!(*document.line_index, LineIndex::new(document.text()));
            for offset in document
                .text()
                .char_indices()
                .map(|(offset, _)| offset)
                .chain(std::iter::once(document.text().len()))
            {
                assert_eq!(
                    document.byte_to_textarea(offset),
                    super::super::byte_to_textarea(document.text(), offset)
                );
            }
        }
        let source = "head 😀\r\nbody 文\nlast\r";
        let mut document = Document::new(source);
        for step in 0..10 {
            let end = document.text().len();
            document
                .apply(
                    vec![
                        super::super::Edit::replace(0..0, format!("文{step}\r\n")),
                        Edit::replace(end..end, "😀\n"),
                    ],
                    vec![Selection::caret(0)],
                    Some(step / 3),
                )
                .unwrap();
            verify(&document);
        }
        let final_text = document.text().to_owned();
        while document.undo() {
            verify(&document);
        }
        assert_eq!(document.text(), source);
        while document.redo() {
            verify(&document);
        }
        assert_eq!(document.text(), final_text);
        assert!(document.begin_composition(Some(999)));
        let preview = format!("文\r\n{}", document.text());
        document
            .native_input(
                &preview,
                Selection::caret(5),
                super::super::NativeInputKind::Insert,
                Some(999),
            )
            .unwrap();
        verify(&document);
        document.cancel_composition();
        verify(&document);
    }

    #[test]
    fn visible_row_growth_and_large_deletion_keep_capacity_near_current_rows() {
        let source = "body\n".repeat(10_000);
        let mut index = LineIndex::new(&source);
        drop(index.visible_rows());
        let expanded = format!("new\n{source}");
        index.update(source.len(), &expanded, 0..0, 4);
        let rows = index.visible_rows();
        assert!(rows.capacity() <= rows.len() + 256);
        assert_eq!(rows, LineIndex::new(&expanded).visible_rows());
        let retained = rows.clone();
        drop(rows);
        index.update(expanded.len(), "tail", 0..expanded.len(), 4);
        let rows = index.visible_rows();
        assert_eq!(rows, LineIndex::new("tail").visible_rows());
        assert!(rows.capacity() <= rows.len() + 256);
        assert_eq!(retained, LineIndex::new(&expanded).visible_rows());
    }

    #[test]
    fn visible_rows_stay_lazy_until_requested_and_rebase_unique_native_offsets() {
        let mut index = LineIndex::new("head\n文\r\ntail\n");
        assert!(index.visible_rows.0.lock().unwrap().is_none());
        let changed = "HEAD\n文\r\ntail\n";
        index.update(changed.len(), changed, 0..4, 4);
        assert!(index.visible_rows.0.lock().unwrap().is_none());
        let rows = index.visible_rows();
        let address = Arc::as_ptr(&rows);
        let prefix = rows[0].clone();
        drop(rows);
        let changed = "HEAD\nabc\r\ntail\n";
        index.update(changed.len(), changed, 5..8, 8);
        let rows = index.visible_rows();
        assert_eq!(Arc::as_ptr(&rows), address);
        assert_eq!(rows[0], prefix);
        assert_eq!(rows, LineIndex::new(changed).visible_rows());
        assert_eq!(rows[2].textarea_start, 9);
    }

    proptest::proptest! {
        #[test]
        fn incremental_coordinates_equal_full_reconstruction(
            old in "[a-z文😀\\r\\n\\t]{0,80}",
            inserted in "[a-z文😀\\r\\n\\t]{0,30}",
            first in proptest::num::usize::ANY,
            second in proptest::num::usize::ANY,
        ) {
            let boundaries: Vec<_> = old.char_indices().map(|(offset, _)| offset).chain(std::iter::once(old.len())).collect();
            let a = boundaries[first % boundaries.len()];
            let b = boundaries[second % boundaries.len()];
            let changed = a.min(b)..a.max(b);
            let new = format!("{}{}{}", &old[..changed.start], inserted, &old[changed.end..]);
            let mut index = LineIndex::new(&old);
            let retained_rows = index.visible_rows();
            index.update(old.len(), &new, changed.clone(), changed.start + inserted.len());
            let rebuilt = LineIndex::new(&new);
            proptest::prop_assert_eq!(&index, &rebuilt);
            proptest::prop_assert_eq!(index.visible_rows(), rebuilt.visible_rows());
            proptest::prop_assert_eq!(retained_rows, LineIndex::new(&old).visible_rows());
            let mut unique = LineIndex::new(&old);
            drop(unique.visible_rows());
            unique.update(old.len(), &new, changed.clone(), changed.start + inserted.len());
            proptest::prop_assert_eq!(unique.visible_rows(), rebuilt.visible_rows());
            for first in 0..=index.rows.len() {
                for last in first..=index.rows.len() {
                    let start = index.rows.get(first).map_or(new.len(), |row| row.start);
                    let end = index.rows.get(last).map_or(new.len(), |row| row.start);
                    let text = &new[start..end];
                    let endings = index.line_endings(first..last);
                    proptest::prop_assert_eq!(endings.carriage_returns, text.contains('\r'));
                    proptest::prop_assert_eq!(endings.uniform_rows, !text.as_bytes().iter().enumerate().any(|(offset, byte)| *byte == b'\r' && text.as_bytes().get(offset + 1) != Some(&b'\n')));
                    proptest::prop_assert_eq!(endings, rebuilt.line_endings(first..last));
                }
            }
            for offset in new.char_indices().map(|(offset, _)| offset).chain(std::iter::once(new.len())) {
                proptest::prop_assert_eq!(index.byte_to_textarea(&new, offset), super::super::byte_to_textarea(&new, offset));
                proptest::prop_assert_eq!(index.line_column(&new, offset), super::super::line_column(&new, offset));
            }
            for offset in 0..=new.encode_utf16().count() + 2 {
                proptest::prop_assert_eq!(index.textarea_to_byte(&new, offset), super::super::textarea_to_byte(&new, offset));
            }
        }
    }
}
