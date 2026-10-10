//! Shared full-editor admission and bounded read-only text pages.
use std::ops::Range;

pub const MAX_EDITOR_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_EDITOR_LINES: usize = 100_000;
pub const MAX_EDITOR_LINE_BYTES: usize = 1024 * 1024;
pub const TEXT_PAGE_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorLimit {
    Bytes,
    Lines,
    LineBytes,
}
impl std::fmt::Display for EditorLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Bytes => "File exceeds the full editor's 8 MiB limit",
            Self::Lines => "File exceeds the full editor's 100,000 line limit",
            Self::LineBytes => "File contains a line longer than the full editor's 1 MiB limit",
        })
    }
}

/// Check before allocating line/projection metadata. LF and standalone CR both
/// count as display breaks; CRLF counts once. The scan allocates no metadata and
/// stops at the first exceeded limit.
pub fn editor_limit(source: &str) -> Option<EditorLimit> {
    let mut preparation = EditorAdmission::new(source);
    preparation.advance(usize::MAX);
    preparation.finish().unwrap()
}

/// A completed admission decision bound to its exact immutable source slice.
/// Private fields prevent hosts from manufacturing a decision for another file.
#[derive(Clone, Copy, Debug)]
pub struct EditorAdmissionResult<'a> {
    source: &'a str,
    limit: Option<EditorLimit>,
}
impl EditorAdmissionResult<'_> {
    pub fn matches(self, source: &str) -> bool {
        std::ptr::eq(self.source, source)
    }
    pub fn limit(self) -> Option<EditorLimit> {
        self.limit
    }
}
impl<'a> EditorAdmissionResult<'a> {
    pub(super) fn from_index(source: &'a str, index: &super::index::LineIndex) -> Self {
        Self {
            source,
            limit: if index.admitted(source.len()) {
                None
            } else {
                editor_limit(source)
            },
        }
    }
}

/// Allocation-free, complete-only interactive admission. Each unit inspects at
/// most 8 KiB; byte limits reject immediately before building any row metadata.
pub struct EditorAdmission<'a> {
    source: &'a str,
    scan: AdmissionScan,
    next: usize,
    result: Option<Option<EditorLimit>>,
}
impl<'a> EditorAdmission<'a> {
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            scan: AdmissionScan::default(),
            next: 0,
            result: (source.len() > MAX_EDITOR_BYTES).then_some(Some(EditorLimit::Bytes)),
        }
    }
    pub fn advance(&mut self, budget: usize) -> bool {
        for _ in 0..budget {
            if self.result.is_some() {
                break;
            }
            let end = self.next.saturating_add(8192).min(self.source.len());
            let limit = self.scan.bytes(&self.source.as_bytes()[self.next..end]);
            self.next = end;
            if limit.is_some() || end == self.source.len() {
                self.result = Some(limit);
            }
        }
        self.result.is_some()
    }
    /// `None` is pending; `Some(None)` is admitted without a limit.
    pub fn finish(self) -> Option<Option<EditorLimit>> {
        self.finish_with_source().map(EditorAdmissionResult::limit)
    }
    pub fn finish_with_source(self) -> Option<EditorAdmissionResult<'a>> {
        self.result.map(|limit| EditorAdmissionResult {
            source: self.source,
            limit,
        })
    }
}

/// Validate a proposed transaction without joining its unchanged source pieces.
pub(super) fn editor_limit_parts(parts: &[&str]) -> Option<EditorLimit> {
    if parts
        .iter()
        .try_fold(0_usize, |size, part| size.checked_add(part.len()))
        .is_none_or(|size| size > MAX_EDITOR_BYTES)
    {
        return Some(EditorLimit::Bytes);
    }
    let mut scan = AdmissionScan::default();
    for part in parts {
        if let Some(limit) = scan.text(part) {
            return Some(limit);
        }
    }
    None
}

/// Logical rows end at LF, so their admission summaries start with no carried CR.
/// Standalone CR still counts as a break for the shared admission contract.
#[cfg(test)]
pub(super) fn row_admission(text: &str) -> (usize, bool) {
    let mut breaks = 0;
    let mut width = 0;
    let mut oversized = false;
    let mut carriage_return = false;
    for byte in text.bytes() {
        if matches!(byte, b'\r' | b'\n') {
            breaks += usize::from(byte != b'\n' || !carriage_return);
            width = 0;
        } else {
            width += 1;
            oversized |= width > MAX_EDITOR_LINE_BYTES;
        }
        carriage_return = byte == b'\r';
    }
    (breaks, oversized)
}

struct AdmissionScan {
    lines: usize,
    width: usize,
    carriage_return: bool,
    #[cfg(test)]
    scanned: usize,
}
impl Default for AdmissionScan {
    fn default() -> Self {
        Self {
            lines: 1,
            width: 0,
            carriage_return: false,
            #[cfg(test)]
            scanned: 0,
        }
    }
}
impl AdmissionScan {
    fn text(&mut self, text: &str) -> Option<EditorLimit> {
        self.bytes(text.as_bytes())
    }

    fn bytes(&mut self, bytes: &[u8]) -> Option<EditorLimit> {
        for &byte in bytes {
            #[cfg(test)]
            {
                self.scanned += 1;
            }
            match byte {
                b'\r' | b'\n' => {
                    if byte != b'\n' || !self.carriage_return {
                        self.lines += 1;
                        if self.lines > MAX_EDITOR_LINES {
                            return Some(EditorLimit::Lines);
                        }
                    }
                    self.width = 0;
                }
                _ => {
                    self.width += 1;
                    if self.width > MAX_EDITOR_LINE_BYTES {
                        return Some(EditorLimit::LineBytes);
                    }
                }
            }
            self.carriage_return = byte == b'\r';
        }
        None
    }

    fn source(
        &mut self,
        source: &str,
        index: &super::index::LineIndex,
        range: Range<usize>,
    ) -> Option<EditorLimit> {
        if range.is_empty() {
            return None;
        }
        let first = super::lines::row_at(&index.rows, range.start);
        let prefix_end = index.rows[first].end.min(range.end);
        if let Some(limit) = self.text(&source[range.start..prefix_end]) {
            return Some(limit);
        }
        if prefix_end == range.end {
            return None;
        }
        let last = super::lines::row_at(&index.rows, range.end);
        // The prefix ended at LF; untouched whole rows cannot introduce an
        // oversized line in an admitted source. Preserve error order by checking
        // their break count before scanning the joining tail.
        self.lines += index.row_breaks(first + 1, last);
        if self.lines > MAX_EDITOR_LINES {
            return Some(EditorLimit::Lines);
        }
        self.text(&source[index.rows[last].start..range.end])
    }
}

/// Scan changed text and source boundary rows; reuse unchanged complete rows.
/// Non-admitted documents retain the full scan, including earliest-error order.
pub(super) fn editor_limit_edits<T: AsRef<str>>(
    source: &str,
    index: &super::index::LineIndex,
    edits: &[super::Edit<T>],
) -> Option<EditorLimit> {
    scan_edits(source, index, edits, &mut AdmissionScan::default())
}
fn scan_edits<T: AsRef<str>>(
    source: &str,
    index: &super::index::LineIndex,
    edits: &[super::Edit<T>],
    scan: &mut AdmissionScan,
) -> Option<EditorLimit> {
    if !index.admitted(source.len()) {
        return editor_limit_parts(&super::edit_parts(source, edits));
    }
    let size = edits.iter().try_fold(source.len(), |size, edit| {
        size.checked_sub(edit.range.len())?
            .checked_add(edit.text.as_ref().len())
    });
    if size.is_none_or(|size| size > MAX_EDITOR_BYTES) {
        return Some(EditorLimit::Bytes);
    }
    let mut start = 0;
    for edit in edits {
        if let Some(limit) = scan
            .source(source, index, start..edit.range.start)
            .or_else(|| scan.text(edit.text.as_ref()))
        {
            return Some(limit);
        }
        start = edit.range.end;
    }
    scan.source(source, index, start..source.len())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextPage {
    pub index: usize,
    pub pages: usize,
    pub range: Range<usize>,
}
impl TextPage {
    /// Adjacent pages cover the exact source without splitting UTF-8 characters.
    /// The source remains separate; a viewer cannot accidentally save a page.
    pub fn new(source: &str, requested: usize) -> Self {
        let pages = source.len().div_ceil(TEXT_PAGE_BYTES).max(1);
        let index = requested.min(pages - 1);
        let boundary = |offset: usize| {
            let mut offset = offset.min(source.len());
            while !source.is_char_boundary(offset) {
                offset -= 1;
            }
            offset
        };
        Self {
            index,
            pages,
            range: boundary(index * TEXT_PAGE_BYTES)..boundary((index + 1) * TEXT_PAGE_BYTES),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admission_results_require_complete_preparation_and_exact_source() {
        let source = "文😀\r\n".repeat(9000);
        assert!(EditorAdmission::new(&source).finish_with_source().is_none());
        let mut preparation = EditorAdmission::new(&source);
        while !preparation.advance(1) {}
        let result = preparation.finish_with_source().unwrap();
        assert_eq!(result.limit(), None);
        assert!(result.matches(&source));
        assert!(!result.matches(&source.clone()));
        assert!(!result.matches(&source[..3]));
    }

    #[test]
    fn indexed_admission_proofs_follow_edits_history_and_failure_precedence() {
        use crate::editor::{Document, Edit, Selection};
        for source in [
            "文😀\r\n".repeat(3000),
            "\r\n".repeat(MAX_EDITOR_LINES),
            "x".repeat(MAX_EDITOR_LINE_BYTES + 1),
            format!(
                "{}{}",
                "\r".repeat(MAX_EDITOR_LINES),
                "x".repeat(MAX_EDITOR_LINE_BYTES + 1)
            ),
            format!(
                "{}{}",
                "x".repeat(MAX_EDITOR_LINE_BYTES + 1),
                "\r".repeat(MAX_EDITOR_LINES)
            ),
        ] {
            let mut document = Document::new(source);
            for stage in 0..4 {
                let proof = document.admission();
                assert!(proof.matches(document.text()));
                assert_eq!(proof.limit(), editor_limit(document.text()));
                match stage {
                    0 => {
                        document
                            .apply(
                                vec![Edit::replace(0..document.text().len(), "small\r\n")],
                                vec![Selection::caret(0)],
                                None,
                            )
                            .unwrap();
                    }
                    1 => {
                        assert!(document.undo());
                    }
                    2 => {
                        assert!(document.redo());
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn cooperative_admission_preserves_limits_across_byte_and_crlf_seams() {
        let sources = [
            String::new(),
            format!("x{}\r\n{}", "é".repeat(4095), "文😀".repeat(9000)),
            "x".repeat(MAX_EDITOR_BYTES + 1),
            "x".repeat(MAX_EDITOR_LINE_BYTES + 1),
            "\r\n".repeat(MAX_EDITOR_LINES),
            format!(
                "{}\n{}",
                "x".repeat(MAX_EDITOR_LINE_BYTES + 1),
                "\n".repeat(MAX_EDITOR_LINES)
            ),
            format!(
                "{}{}",
                "\n".repeat(MAX_EDITOR_LINES),
                "x".repeat(MAX_EDITOR_LINE_BYTES + 1)
            ),
        ];
        for source in sources {
            let expected = editor_limit_parts(&[&source]);
            for budget in [1, 8, 64] {
                let mut preparation = EditorAdmission::new(&source);
                while !preparation.advance(budget) {
                    assert_eq!(preparation.result, None);
                    assert!(preparation.next < source.len());
                }
                assert_eq!(preparation.finish(), Some(expected));
            }
        }
        assert_eq!(EditorAdmission::new("pending").finish(), None);
    }

    #[test]
    fn indexed_admission_matches_full_scan_at_unicode_and_newline_joins() {
        use super::super::{Edit, index::LineIndex};
        let mut seed = 17_u64;
        for _ in 0..80 {
            let mut source = String::new();
            for _ in 0..40 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                source.push_str(["a", "文", "😀", "\r", "\n", "\r\n", "\t"][(seed % 7) as usize]);
            }
            let index = LineIndex::new(&source);
            let positions: Vec<_> = source
                .char_indices()
                .map(|(at, _)| at)
                .chain([source.len()])
                .collect();
            for at in positions.iter().step_by(3).copied() {
                for end in positions
                    .iter()
                    .copied()
                    .filter(|end| *end >= at)
                    .step_by(4)
                {
                    for text in ["", "x", "\r", "\n", "文\r\n😀"] {
                        let edits = [Edit {
                            range: at..end,
                            text,
                        }];
                        assert_eq!(
                            editor_limit_edits(&source, &index, &edits),
                            editor_limit_parts(&super::super::edit_parts(&source, &edits)),
                            "source={source:?} edit={edits:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn indexed_admission_preserves_limits_and_failure_priority() {
        use super::super::{Edit, index::LineIndex};
        let long = "x".repeat(MAX_EDITOR_LINE_BYTES);
        let rows = "x\r\ny\r".repeat((MAX_EDITOR_LINES - 2) / 2);
        for source in [
            format!("{long}\r\n{long}\n"),
            rows,
            "x".repeat(MAX_EDITOR_LINE_BYTES + 1),
            "\r\n".repeat(MAX_EDITOR_LINES),
        ] {
            let index = LineIndex::new(&source);
            for at in [0, 1, source.len() / 2, source.len() - 1, source.len()] {
                for text in ["", "x", "\r", "\n", long.as_str()] {
                    let edits = [Edit {
                        range: at..at,
                        text,
                    }];
                    assert_eq!(
                        editor_limit_edits(&source, &index, &edits),
                        editor_limit_parts(&super::super::edit_parts(&source, &edits))
                    );
                }
            }
            let edits = [
                Edit {
                    range: 0..1,
                    text: "\n\n",
                },
                Edit {
                    range: source.len() - 1..source.len(),
                    text: long.as_str(),
                },
            ];
            assert_eq!(
                editor_limit_edits(&source, &index, &edits),
                editor_limit_parts(&super::super::edit_parts(&source, &edits))
            );
        }
        let source = "x\n".repeat(MAX_EDITOR_LINES - 2);
        let index = LineIndex::new(&source);
        let edits = [
            Edit {
                range: 0..0,
                text: "\n\n",
            },
            Edit {
                range: source.len()..source.len(),
                text: long.as_str(),
            },
        ];
        assert_eq!(
            editor_limit_edits(&source, &index, &edits),
            Some(EditorLimit::Lines)
        );
        let too_long = "x".repeat(MAX_EDITOR_LINE_BYTES + 1);
        let edits = [
            Edit {
                range: 0..0,
                text: too_long.as_str(),
            },
            Edit {
                range: source.len()..source.len(),
                text: "\n\n",
            },
        ];
        assert_eq!(
            editor_limit_edits(&source, &index, &edits),
            Some(EditorLimit::LineBytes)
        );
    }

    #[test]
    fn indexed_admission_rechecks_merged_rows_and_invalid_history() {
        use super::super::{Document, Edit, EditError, Selection, index::LineIndex};
        let long = "x".repeat(MAX_EDITOR_LINE_BYTES);
        let source = format!("{long}\r\n{long}");
        let index = LineIndex::new(&source);
        for range in [
            long.len()..long.len() + 2,
            long.len()..long.len() + 1,
            long.len() + 1..long.len() + 2,
        ] {
            let edits = [Edit::replace(range, "")];
            assert_eq!(
                editor_limit_edits(&source, &index, &edits),
                editor_limit_parts(&super::super::edit_parts(&source, &edits))
            );
        }
        let mut document = Document::new(format!("{long}x"));
        document.enforce_editor_limits();
        assert!(!document.line_index.admitted(document.text().len()));
        document
            .apply(
                vec![Edit::replace(long.len()..long.len() + 1, "")],
                vec![Selection::caret(0)],
                None,
            )
            .unwrap();
        assert!(document.line_index.admitted(document.text().len()));
        assert!(document.undo());
        assert!(!document.line_index.admitted(document.text().len()));
        assert_eq!(
            document.apply(
                vec![Edit::replace(0..0, "x")],
                vec![Selection::caret(0)],
                None
            ),
            Err(EditError::Capacity(EditorLimit::LineBytes))
        );
        assert!(document.can_redo());
        assert!(document.redo());
        assert!(document.line_index.admitted(document.text().len()));
    }

    #[test]
    fn admitted_unchanged_rows_do_not_scan_full_source_on_edit() {
        use super::super::{Edit, index::LineIndex};
        let source = "abc\r\n".repeat(90_000);
        let index = LineIndex::new(&source);
        for at in [0, source.len() / 2, source.len()] {
            let edits = [Edit {
                range: at..at,
                text: "文",
            }];
            let mut scan = AdmissionScan::default();
            assert_eq!(scan_edits(&source, &index, &edits, &mut scan), None);
            assert!(
                scan.scanned <= 21,
                "scanned {} bytes for a {} byte source",
                scan.scanned,
                source.len()
            );
        }
        let mut changed = source.clone();
        let at = source.len() / 2;
        changed.replace_range(at..at + 3, "\r\nnew\r");
        let mut updated = index;
        updated.update(source.len(), &changed, at..at + 3, at + 7);
        assert_eq!(updated, LineIndex::new(&changed));
        let edits = [Edit {
            range: at..at,
            text: "\n",
        }];
        assert_eq!(
            editor_limit_edits(&changed, &updated, &edits),
            editor_limit_parts(&super::super::edit_parts(&changed, &edits))
        );
    }

    #[test]
    fn proposed_parts_preserve_admission_priority_and_cross_piece_line_endings() {
        for source in [
            "".to_string(),
            "文😀\r\nx\r\n\r".to_string(),
            "\r\n".repeat(MAX_EDITOR_LINES),
            "x".repeat(MAX_EDITOR_LINE_BYTES + 1),
            "x".repeat(MAX_EDITOR_BYTES + 1),
        ] {
            for at in [
                0,
                source.len() / 2,
                source.len().saturating_sub(1),
                source.len(),
            ] {
                let at = source.floor_char_boundary(at);
                assert_eq!(
                    editor_limit_parts(&[&source[..at], "", &source[at..]]),
                    editor_limit(&source)
                );
            }
        }
        assert_eq!(editor_limit_parts(&["\r", "\n"]), None);
    }

    #[test]
    fn admission_bounds_bytes_rows_and_single_lines_without_confusing_crlf() {
        assert_eq!(
            editor_limit(&"x".repeat(MAX_EDITOR_BYTES + 1)),
            Some(EditorLimit::Bytes)
        );
        assert_eq!(editor_limit(&"\n".repeat(MAX_EDITOR_LINES - 1)), None);
        for ending in ["\n", "\r", "\r\n"] {
            assert_eq!(
                editor_limit(&ending.repeat(MAX_EDITOR_LINES)),
                Some(EditorLimit::Lines)
            );
        }
        assert_eq!(editor_limit(&"x".repeat(MAX_EDITOR_LINE_BYTES)), None);
        assert_eq!(
            editor_limit(&"x".repeat(MAX_EDITOR_LINE_BYTES + 1)),
            Some(EditorLimit::LineBytes)
        );
        assert_eq!(
            editor_limit(&format!(
                "{}\r\n{}",
                "x".repeat(MAX_EDITOR_LINE_BYTES),
                "x".repeat(MAX_EDITOR_LINE_BYTES)
            )),
            None
        );
    }

    #[test]
    fn interactive_transactions_reject_capacity_before_indexing_and_keep_history() {
        use crate::editor::{Document, Edit, EditError, Selection};
        let mut document = Document::for_editor("base").unwrap();
        document
            .apply(
                vec![Edit::replace(4..4, "!")],
                vec![Selection::caret(5)],
                None,
            )
            .unwrap();
        let before = document.clone();
        for text in [
            "x".repeat(MAX_EDITOR_LINE_BYTES + 1),
            "\n".repeat(MAX_EDITOR_LINES),
        ] {
            let expected = editor_limit(&text).unwrap();
            assert_eq!(
                document.apply(
                    vec![Edit::replace(0..5, &text)],
                    vec![Selection::caret(0)],
                    None
                ),
                Err(EditError::Capacity(expected))
            );
            assert_eq!(document, before);
        }
        assert!(document.undo());
        assert_eq!(document.text(), "base");
        assert!(document.redo());
        assert_eq!(document.text(), "base!");
        assert_eq!(
            Document::for_editor("\r".repeat(MAX_EDITOR_LINES)),
            Err(EditError::Capacity(EditorLimit::Lines))
        );
    }

    proptest::proptest! {
        #[test]
        fn pages_cover_unicode_source_exactly(source in ".{0,25000}") {
            let first = TextPage::new(&source, 0);
            let mut rebuilt = String::new();
            let mut previous_end = 0;
            for index in 0..first.pages {
                let page = TextPage::new(&source, index);
                proptest::prop_assert_eq!(page.range.start, previous_end);
                proptest::prop_assert!(page.range.len() <= TEXT_PAGE_BYTES + 3);
                rebuilt.push_str(&source[page.range.clone()]);
                previous_end = page.range.end;
            }
            proptest::prop_assert_eq!(rebuilt, source.clone());
            proptest::prop_assert_eq!(TextPage::new(&source, usize::MAX).index, first.pages - 1);
        }
    }
}
