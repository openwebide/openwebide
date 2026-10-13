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
    ascii_body: bool,
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
        let mut collector = NativeCoordinates::new(text.len() > STEP_BYTES);
        collector.ascii_body_end = body.len();
        Self {
            text,
            next: 0,
            visual_complete: visual.is_none(),
            visual,
            collector,
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
                        .advance_with_source_scan(1, |range, ascii| {
                            if ascii {
                                collector.visit_ascii(range.clone());
                            } else {
                                for (offset, ch) in text[range.clone()].char_indices() {
                                    collector.visit(text, range.start + offset, ch);
                                }
                            }
                            *next = range.end;
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
                ascii_body: self.collector.ascii_body,
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
    ascii_body_end: usize,
    ascii_body: bool,
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
            ascii_body_end: usize::MAX,
            ascii_body: true,
        }
    }
    fn visit(&mut self, text: &str, byte: usize, ch: char) {
        if self.ascii_body && byte < self.ascii_body_end && !matches!(ch, ' '..='~') {
            self.ascii_body = false;
        }
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
    fn visit_ascii(&mut self, range: std::ops::Range<usize>) {
        if self.sparse {
            let mut byte = range.start.max(self.last + STEP_BYTES);
            while byte < range.end {
                let offset = byte - range.start;
                self.checkpoints.push(Coordinate {
                    byte,
                    native: self.position.native + offset,
                    chars: self.position.chars + offset,
                });
                self.last = byte;
                self.position.byte = byte;
                byte += STEP_BYTES;
            }
        }
        self.position.native += range.len();
        self.position.chars += range.len();
        self.summary.utf16_len += range.len();
        self.width += range.len();
        self.summary.oversized |= self.width > super::MAX_EDITOR_LINE_BYTES;
        self.previous_cr = false;
    }
}

impl LineCoordinates {
    pub fn new(text: &str) -> Self {
        if text.len() <= STEP_BYTES {
            let body = text
                .strip_suffix("\r\n")
                .or_else(|| text.strip_suffix('\n'))
                .unwrap_or(text);
            return Self {
                ascii_body: body.bytes().all(|byte| matches!(byte, b' '..=b'~')),
                ..Self::default()
            };
        }
        Self::with_summary(text).0
    }

    /// Reuse the admitted printable-ASCII body and scan only inserted bytes.
    /// The caller proves this is one row edit preserving its original ending.
    pub fn updated_ascii(
        &self,
        text: &str,
        changed: std::ops::Range<usize>,
        new_end: usize,
    ) -> Option<(Self, RowSummary)> {
        if !self.ascii_body
            || !text
                .get(changed.start..new_end)?
                .bytes()
                .all(|byte| matches!(byte, b' '..=b'~'))
        {
            return None;
        }
        let body = text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix('\n'))
            .unwrap_or(text);
        let mut collector = NativeCoordinates::new(text.len() > STEP_BYTES);
        collector.ascii_body_end = body.len();
        collector.visit_ascii(0..body.len());
        for (offset, ch) in text[body.len()..].char_indices() {
            collector.visit(text, body.len() + offset, ch);
        }
        let visual = if body.len() > super::MAX_MEASURE_BYTES {
            match &self.visual {
                Some(previous) => previous.updated_ascii(body, changed, new_end),
                None => super::VisualLineIndex::new(body),
            }
        } else {
            None
        };
        Some((
            Self {
                native: collector.sparse.then(|| collector.checkpoints.into()),
                visual,
                ascii_body: true,
            },
            collector.summary,
        ))
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
    pub fn char_to_byte(&self, text: &str, offset: usize) -> usize {
        let start = self
            .checkpoints()
            .partition_point(|point| point.chars <= offset)
            .checked_sub(1)
            .map_or_else(Coordinate::default, |index| self.checkpoints()[index]);
        text[start.byte..]
            .char_indices()
            .nth(offset - start.chars)
            .map_or(text.len(), |(byte, _)| start.byte + byte)
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
    fn printable_body_proof_is_latched_and_ignores_only_the_original_ending() {
        for (text, expected) in [
            ("ascii\r\n", true),
            ("文ascii\r\n", false),
            ("ascii\tascii\n", false),
            ("ascii\rascii\n", false),
        ] {
            let body = text
                .strip_suffix("\r\n")
                .or_else(|| text.strip_suffix('\n'))
                .unwrap_or(text);
            let mut collector = NativeCoordinates::new(true);
            collector.ascii_body_end = body.len();
            let mut rejected = false;
            for (byte, ch) in text.char_indices() {
                collector.visit(text, byte, ch);
                if rejected {
                    assert!(!collector.ascii_body);
                }
                rejected |= !collector.ascii_body;
            }
            assert_eq!(collector.ascii_body, expected);
            assert_eq!(
                collector.ascii_body,
                LineCoordinates::with_summary(text).0.ascii_body
            );
        }
    }

    #[test]
    fn ascii_spans_preserve_every_native_checkpoint_and_admission_total() {
        for text in [
            "word 123 _; ".repeat(9000),
            format!("{}\r\n{}\r\n", "x".repeat(511), "y".repeat(1024)),
            format!("{}a{} tail", "\u{0600}".repeat(900), "\u{301}".repeat(900)),
            "ASCII 文😀e\u{301}\t\r\n".repeat(9000),
        ] {
            let mut reference = NativeCoordinates::new(true);
            for (byte, ch) in text.char_indices() {
                reference.visit(&text, byte, ch);
            }
            let mut actual = NativeCoordinates::new(true);
            let mut scan = super::super::grapheme_scan::GraphemeScan::new(&text);
            while !scan.is_done() {
                scan.advance_spans(|step| {
                    if step.ascii {
                        actual.visit_ascii(step.consumed);
                    } else {
                        for (offset, ch) in text[step.consumed.clone()].char_indices() {
                            actual.visit(&text, step.consumed.start + offset, ch);
                        }
                    }
                });
            }
            assert_eq!(actual.checkpoints, reference.checkpoints);
            assert_eq!(actual.position, reference.position);
            assert_eq!(actual.summary.utf16_len, reference.summary.utf16_len);
            assert_eq!(actual.summary.breaks, reference.summary.breaks);
            assert_eq!(actual.summary.oversized, reference.summary.oversized);
            assert_eq!(actual.width, reference.width);
            assert_eq!(actual.last, reference.last);
            assert_eq!(actual.previous_cr, reference.previous_cr);
        }
    }
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
                coordinates.ascii_body,
                body.bytes().all(|byte| matches!(byte, b' '..=b'~'))
            );
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
