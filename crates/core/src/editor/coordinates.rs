//! Sparse coordinates keep native offset queries bounded within long lines.
use super::EditError;
use std::sync::Arc;

const STEP_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Coordinate {
    byte: usize,
    native: usize,
    chars: usize,
}

/// Checkpoints are relative to a logical source row, so unchanged rows retain
/// their index through insertions/deletions above them and folded projections.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct LineCoordinates {
    native: Option<Arc<[Coordinate]>>,
    visual: Option<super::VisualLineIndex>,
}

#[derive(Default)]
pub(super) struct RowSummary {
    pub utf16_len: usize,
    pub breaks: usize,
    pub oversized: bool,
}

/// Complete-only native/visual coordinates and admission totals for one row.
/// The visual traversal also visits native characters; short and unsupported
/// visual rows use bounded character chunks rather than a second feature path.
pub(super) struct LineCoordinatePreparation<'a> {
    text: &'a str,
    next: usize,
    visual: Option<super::VisualLinePreparation<'a>>,
    visual_complete: bool,
    collector: NativeCoordinates,
    done: bool,
}
impl<'a> LineCoordinatePreparation<'a> {
    pub fn new(text: &'a str) -> Self {
        let body = text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix('\n'))
            .unwrap_or(text);
        let visual = (body.len() > super::MAX_MEASURE_BYTES)
            .then(|| super::VisualLinePreparation::new(body))
            .flatten();
        Self {
            text,
            next: 0,
            visual_complete: visual.is_none(),
            visual,
            collector: NativeCoordinates::new(text.len() > STEP_BYTES),
            done: false,
        }
    }
    /// Each unit bounds source/context traversal, including within an indivisible
    /// Unicode cluster. Only finish exposes the totals or completed coordinates.
    pub fn advance(&mut self, budget: usize) -> bool {
        for _ in 0..budget {
            if self.done {
                break;
            }
            if !self.visual_complete {
                let text = self.text;
                let collector = &mut self.collector;
                let next = &mut self.next;
                self.visual_complete =
                    self.visual
                        .as_mut()
                        .unwrap()
                        .advance_with_character_scan(1, |byte, ch| {
                            collector.visit(text, byte, ch);
                            *next = byte + ch.len_utf8();
                        });
                self.done = self.visual_complete && self.next == self.text.len();
            } else {
                let end = self
                    .text
                    .floor_char_boundary((self.next + STEP_BYTES).min(self.text.len()));
                for (offset, ch) in self.text[self.next..end].char_indices() {
                    self.collector.visit(self.text, self.next + offset, ch);
                }
                self.next = end;
                self.done = end == self.text.len();
            }
        }
        self.done
    }
    pub fn finish(self) -> Option<(LineCoordinates, RowSummary)> {
        if !self.done {
            return None;
        }
        let visual = match self.visual {
            Some(visual) => Some(visual.finish()?),
            None => None,
        };
        Some((
            LineCoordinates {
                native: self
                    .collector
                    .sparse
                    .then(|| self.collector.checkpoints.into()),
                visual,
            },
            self.collector.summary,
        ))
    }
}

struct NativeCoordinates {
    sparse: bool,
    checkpoints: Vec<Coordinate>,
    position: Coordinate,
    summary: RowSummary,
    width: usize,
    last: usize,
    previous_cr: bool,
}
impl NativeCoordinates {
    fn new(sparse: bool) -> Self {
        Self {
            sparse,
            checkpoints: Vec::new(),
            position: Coordinate::default(),
            summary: RowSummary::default(),
            width: 0,
            last: 0,
            previous_cr: false,
        }
    }
    fn visit(&mut self, text: &str, byte: usize, ch: char) {
        // Never start a query between CR and LF: their native offset is equal,
        // and the inverse must select the original CR.
        if self.sparse && byte - self.last >= STEP_BYTES && !(self.previous_cr && ch == '\n') {
            self.position.byte = byte;
            self.checkpoints.push(self.position);
            self.last = byte;
        }
        self.position.chars += 1;
        self.summary.utf16_len += ch.len_utf16();
        if !(ch == '\r' && text.as_bytes().get(byte + 1) == Some(&b'\n')) {
            self.position.native += ch.len_utf16();
        }
        if matches!(ch, '\r' | '\n') {
            self.summary.breaks += usize::from(ch != '\n' || !self.previous_cr);
            self.width = 0;
        } else {
            self.width += ch.len_utf8();
            self.summary.oversized |= self.width > super::MAX_EDITOR_LINE_BYTES;
        }
        self.previous_cr = ch == '\r';
    }
}

impl LineCoordinates {
    pub fn new(text: &str) -> Self {
        if text.len() <= STEP_BYTES {
            return Self::default();
        }
        Self::with_summary(text).0
    }

    /// Admission and native totals share the exact coordinate traversal. Long
    /// rows also share the visual index's character pass, without changing its
    /// grapheme-safe paint boundaries.
    pub fn with_summary(text: &str) -> (Self, RowSummary) {
        let mut preparation = LineCoordinatePreparation::new(text);
        preparation.advance(usize::MAX);
        preparation.finish().unwrap()
    }
    pub fn visual(&self) -> Option<super::VisualLineIndex> {
        self.visual.clone()
    }
    fn checkpoints(&self) -> &[Coordinate] {
        self.native.as_deref().unwrap_or_default()
    }
    fn byte_start(&self, offset: usize) -> Coordinate {
        self.checkpoints()
            .partition_point(|point| point.byte <= offset)
            .checked_sub(1)
            .map_or_else(Coordinate::default, |index| self.checkpoints()[index])
    }
    pub fn byte_to_textarea(&self, text: &str, offset: usize) -> Result<usize, EditError> {
        if offset > text.len() || !text.is_char_boundary(offset) {
            return Err(EditError::InvalidSelection);
        }
        let start = self.byte_start(offset);
        Ok(start.native + super::byte_to_textarea(&text[start.byte..], offset - start.byte)?)
    }
    pub fn textarea_to_byte(&self, text: &str, offset: usize) -> usize {
        let start = self
            .checkpoints()
            .partition_point(|point| point.native <= offset)
            .checked_sub(1)
            .map_or_else(Coordinate::default, |index| self.checkpoints()[index]);
        start.byte + super::textarea_to_byte(&text[start.byte..], offset - start.native)
    }
    pub fn chars_before(&self, text: &str, offset: usize) -> usize {
        let start = self.byte_start(offset);
        start.chars + text[start.byte..offset].chars().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cooperative_row_coordinates_keep_native_and_admission_contracts_at_every_budget() {
        for text in [
            String::new(),
            "文😀\r\n\r\t".into(),
            format!("{}\r\n{}\r\n", "a".repeat(511), "文😀".repeat(200)),
            "word 文😀e\u{301}\t\r ".repeat(6000) + "\r\n",
            format!("👩{}\u{200d}👩\r\n", "\u{301}".repeat(40_000)),
            "a".repeat(super::super::MAX_EDITOR_LINE_BYTES) + "😀\r\n",
            "a".repeat(super::super::MAX_STRUCTURE_BYTES + 1) + "\r\n",
        ] {
            let (breaks, oversized) = super::super::capacity::row_admission(&text);
            let body = text
                .strip_suffix("\r\n")
                .or_else(|| text.strip_suffix('\n'))
                .unwrap_or(&text);
            let visual = (body.len() > super::super::MAX_MEASURE_BYTES)
                .then(|| super::super::VisualLineIndex::new(body))
                .flatten();
            for budget in [1, 8, 64] {
                assert!(LineCoordinatePreparation::new(&text).finish().is_none());
                let mut prep = LineCoordinatePreparation::new(&text);
                assert!(!prep.advance(0));
                let mut turns = 0;
                loop {
                    let before = prep.next;
                    let done = prep.advance(budget);
                    assert!(prep.next - before <= budget * 1536);
                    turns += 1;
                    if done {
                        break;
                    }
                    assert!(turns < text.len() * 4 + 1);
                }
                assert_eq!(prep.collector.position.chars, text.chars().count());
                let (coordinates, summary) = prep.finish().unwrap();
                assert_eq!(summary.utf16_len, text.encode_utf16().count());
                assert_eq!((summary.breaks, summary.oversized), (breaks, oversized));
                assert_eq!(coordinates.visual(), visual);
                for byte in text
                    .char_indices()
                    .map(|(byte, _)| byte)
                    .step_by((text.len() / 97).max(1))
                    .chain([text.len()])
                {
                    let expected = super::super::byte_to_textarea(&text, byte).unwrap();
                    assert_eq!(coordinates.byte_to_textarea(&text, byte), Ok(expected));
                    assert_eq!(
                        coordinates.chars_before(&text, byte),
                        text[..byte].chars().count()
                    );
                    for native in expected.saturating_sub(1)..=expected + 1 {
                        assert_eq!(
                            coordinates.textarea_to_byte(&text, native),
                            super::super::textarea_to_byte(&text, native)
                        );
                    }
                }
                if text.len() > 64 * 1536 {
                    assert!(turns > 1);
                }
            }
        }
    }

    #[test]
    fn combined_row_summaries_match_independent_byte_and_utf16_scans() {
        for text in [
            String::new(),
            "文😀\r\n\r\t".into(),
            "文😀e\u{301}\r\n".repeat(3000),
            "🇺🇸👩‍👩‍👧‍👦क्ष \rword\n".repeat(1000),
            "a".repeat(super::super::MAX_EDITOR_LINE_BYTES),
            "a".repeat(super::super::MAX_EDITOR_LINE_BYTES) + "😀\r\n",
            "a".repeat(super::super::MAX_STRUCTURE_BYTES + 1) + "\r\n",
        ] {
            let (coordinates, summary) = LineCoordinates::with_summary(&text);
            let (breaks, oversized) = super::super::capacity::row_admission(&text);
            assert_eq!(summary.utf16_len, text.encode_utf16().count());
            assert_eq!((summary.breaks, summary.oversized), (breaks, oversized));
            let body = text
                .strip_suffix("\r\n")
                .or_else(|| text.strip_suffix('\n'))
                .unwrap_or(&text);
            assert_eq!(
                coordinates.visual(),
                (body.len() > super::super::MAX_MEASURE_BYTES)
                    .then(|| super::super::VisualLineIndex::new(body))
                    .flatten()
            );
        }
    }

    #[test]
    fn long_unicode_and_crlf_queries_match_reference_and_bound_scan_distance() {
        for text in [
            "a".repeat(511) + "\r\n😀文e\u{301}\t\r" + &"文😀".repeat(1000),
            "文😀e\u{301}\t\r\n".repeat(1000),
            "a".repeat(512) + "\r\n" + &"a".repeat(512) + "\r\n",
        ] {
            let index = LineCoordinates::new(&text);
            for byte in text
                .char_indices()
                .map(|(byte, _)| byte)
                .chain([text.len()])
            {
                assert!(byte - index.byte_start(byte).byte <= STEP_BYTES + 4);
                assert_eq!(
                    index.byte_to_textarea(&text, byte),
                    super::super::byte_to_textarea(&text, byte)
                );
                assert_eq!(
                    index.chars_before(&text, byte),
                    text[..byte].chars().count()
                );
            }
            for native in 0..=text.encode_utf16().count() + 2 {
                assert_eq!(
                    index.textarea_to_byte(&text, native),
                    super::super::textarea_to_byte(&text, native)
                );
            }
            assert_eq!(
                index.byte_to_textarea(&text, text.len() + 1),
                Err(EditError::InvalidSelection)
            );
        }
    }
}
