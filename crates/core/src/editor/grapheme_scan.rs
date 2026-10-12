//! Bounded Unicode source/context traversal shared by editor preparations.
use std::ops::Range;
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

pub(super) struct GraphemeStep {
    pub consumed: Range<usize>,
    pub boundary: Option<usize>,
}

pub(super) struct GraphemeSpan {
    pub consumed: Range<usize>,
    pub boundary: Option<usize>,
    pub ascii: bool,
}

pub(super) struct GraphemeScan<'a> {
    text: &'a str,
    cursor: GraphemeCursor,
    chunk: Range<usize>,
    progress: usize,
    at_boundary: bool,
    done: bool,
}
impl<'a> GraphemeScan<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            cursor: GraphemeCursor::new(0, text.len(), true),
            chunk: 0..text.floor_char_boundary(512.min(text.len())),
            progress: 0,
            at_boundary: true,
            done: false,
        }
    }
    pub fn progress(&self) -> usize {
        self.progress
    }
    pub fn is_done(&self) -> bool {
        self.done
    }

    fn take_ascii(&mut self, maximum: usize) -> Option<Range<usize>> {
        if !self.at_boundary || self.done {
            return None;
        }
        let start = self.cursor.cur_cursor();
        let limit = self.chunk.end.min(start.saturating_add(maximum.min(512)));
        let bytes = self.text.as_bytes();
        let mut end = start;
        while end < limit
            && (0x20..=0x7e).contains(&bytes[end])
            && (end + 1 == bytes.len() || (0x20..=0x7e).contains(&bytes[end + 1]))
        {
            end += 1;
        }
        if end == start {
            return None;
        }
        self.cursor.set_cursor(end);
        self.progress += end - start;
        self.done = end == bytes.len();
        Some(start..end)
    }

    /// Complete printable-ASCII spans share one cursor update. Other source and
    /// Unicode context use the same single-step scanner; no span crosses an
    /// unproved grapheme boundary or exceeds the source/context unit budget.
    pub fn advance_spans(&mut self, mut consume: impl FnMut(GraphemeSpan)) {
        let before = self.progress;
        for _ in 0..512 {
            if self.done || self.progress - before >= 512 {
                break;
            }
            if let Some(consumed) = self.take_ascii(512 - (self.progress - before)) {
                consume(GraphemeSpan {
                    boundary: Some(consumed.end),
                    consumed,
                    ascii: true,
                });
            } else {
                self.advance(|step| {
                    consume(GraphemeSpan {
                        consumed: step.consumed,
                        boundary: step.boundary,
                        ascii: false,
                    });
                    true
                });
            }
        }
    }

    /// One bounded scan unit. A consumer can stop at an exact boundary earlier.
    /// Forward/context progress and cursor calls both have fixed limits; context
    /// requests need not move the forward cursor. Each supplied chunk is <=512 bytes.
    pub fn advance(&mut self, mut consume: impl FnMut(GraphemeStep) -> bool) {
        let before = self.progress;
        for _ in 0..512 {
            if self.done || self.progress - before >= 512 {
                break;
            }
            let offset = self.cursor.cur_cursor();
            // At a proven boundary, adjacent printable ASCII scalars cannot
            // combine. Do not skip Unicode lookahead (Extend/ZWJ/SpacingMark),
            // CRLF, or a cursor suspended inside an unfinished cluster.
            if self.at_boundary
                && offset < self.chunk.end
                && self
                    .text
                    .as_bytes()
                    .get(offset)
                    .is_some_and(|byte| (0x20..=0x7e).contains(byte))
                && (offset + 1 == self.text.len()
                    || self
                        .text
                        .as_bytes()
                        .get(offset + 1)
                        .is_some_and(|byte| (0x20..=0x7e).contains(byte)))
            {
                let end = offset + 1;
                self.cursor.set_cursor(end);
                self.progress += 1;
                self.done = end == self.text.len();
                if consume(GraphemeStep {
                    consumed: offset..end,
                    boundary: Some(end),
                }) {
                    break;
                }
                continue;
            }
            let result = self
                .cursor
                .next_boundary(&self.text[self.chunk.clone()], self.chunk.start);
            let end = self.cursor.cur_cursor();
            self.progress += end - offset;
            let boundary = match result {
                Ok(boundary) => {
                    self.done = boundary.is_none() || boundary == Some(self.text.len());
                    boundary
                }
                Err(GraphemeIncomplete::NextChunk) => {
                    // Preserve the preceding scalar. At an exact chunk start
                    // regional context would otherwise count known parity twice.
                    let start = self
                        .text
                        .floor_char_boundary(self.chunk.end.saturating_sub(4));
                    self.chunk = start
                        ..self
                            .text
                            .floor_char_boundary((start + 512).min(self.text.len()));
                    None
                }
                Err(GraphemeIncomplete::PreContext(end)) => {
                    let start = self.text.ceil_char_boundary(end.saturating_sub(512));
                    self.cursor.provide_context(&self.text[start..end], start);
                    self.progress += end - start;
                    None
                }
                Err(GraphemeIncomplete::PrevChunk | GraphemeIncomplete::InvalidOffset) => {
                    unreachable!("forward cursor always retains its exact source chunk");
                }
            };
            self.at_boundary = boundary.is_some();
            if consume(GraphemeStep {
                consumed: offset..end,
                boundary,
            }) {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use unicode_segmentation::UnicodeSegmentation;

    fn check(text: &str) {
        let expected: Vec<_> = text
            .grapheme_indices(true)
            .map(|(start, glyph)| start + glyph.len())
            .collect();
        let mut scan = GraphemeScan::new(text);
        let mut boundaries = Vec::new();
        let mut consumed = 0;
        while !scan.is_done() {
            scan.advance(|step| {
                assert_eq!(step.consumed.start, consumed);
                consumed = step.consumed.end;
                if let Some(boundary) = step.boundary {
                    boundaries.push(boundary);
                }
                true
            });
        }
        assert_eq!(consumed, text.len());
        assert_eq!(boundaries, expected);
        let mut scan = GraphemeScan::new(text);
        let mut boundaries = Vec::new();
        let mut consumed = 0;
        while !scan.is_done() {
            let before = scan.progress();
            scan.advance_spans(|step| {
                assert_eq!(step.consumed.start, consumed);
                consumed = step.consumed.end;
                if step.ascii {
                    assert!(step.consumed.len() <= 512);
                    boundaries.extend(step.consumed.start + 1..=step.consumed.end);
                } else if let Some(boundary) = step.boundary {
                    boundaries.push(boundary);
                }
            });
            assert!(scan.progress() - before <= 1536);
        }
        assert_eq!(consumed, text.len());
        assert_eq!(boundaries, expected);
    }

    proptest! {
        #[test]
        fn sparse_ascii_and_arbitrary_unicode_match_full_segmentation(
            prefix in 0..1025usize,
            characters in prop::collection::vec(any::<char>(), 0..300),
        ) {
            check(&format!("{}{} tail", "x".repeat(prefix), characters.into_iter().collect::<String>()));
        }
    }

    #[test]
    fn suspended_long_clusters_never_use_ascii_shortcuts() {
        for text in [
            format!("{}a{} tail", "x".repeat(511), "\u{0301}".repeat(2048)),
            format!("{}ab tail", "\u{0600}".repeat(2048)),
            format!("{}ab tail", "🇺".repeat(2049)),
        ] {
            check(&text);
        }
    }

    #[test]
    fn ascii_shortcuts_preserve_unicode_boundaries_and_consumed_source() {
        let atoms = [
            "ab",
            " ",
            "\r\n",
            "\u{0600}a",
            "a\u{0301}",
            "a\u{200d}",
            "🇺🇸🇨🇦",
            "👩🏽‍💻",
            "क\u{093e}",
            "\t",
            "文",
        ];
        for left in atoms {
            for right in atoms {
                for padding in [0, 510, 511, 512, 513] {
                    let text = format!("{}{left}{right} tail", "x".repeat(padding));
                    let expected: Vec<_> = text
                        .grapheme_indices(true)
                        .map(|(start, glyph)| start + glyph.len())
                        .collect();
                    let mut scan = GraphemeScan::new(&text);
                    let mut boundaries = Vec::new();
                    let mut consumed = 0;
                    while !scan.is_done() {
                        scan.advance(|step| {
                            assert_eq!(step.consumed.start, consumed);
                            consumed = step.consumed.end;
                            if let Some(boundary) = step.boundary {
                                boundaries.push(boundary);
                            }
                            true // Exercise suspension after every step.
                        });
                    }
                    assert_eq!(consumed, text.len());
                    assert_eq!(
                        boundaries, expected,
                        "{left:?} + {right:?}, padding {padding}"
                    );
                }
            }
        }
    }
}
