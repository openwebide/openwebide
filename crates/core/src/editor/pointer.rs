//! Pointer selection units and drag direction are source policy, independent of pixels.
use super::{Document, EditError, Selection, lines::row_at};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unit {
    Caret,
    Word,
    Line,
}

/// A pointer gesture retains its original source unit while dragging either way.
/// The caller must discard it when the source document changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PointerSelection {
    anchor: Range<usize>,
    unit: Unit,
    revision: u64,
    identity: std::sync::Arc<()>,
}

/// Distance to scroll one selection-drag frame along an axis, in CSS pixels.
/// A narrow edge zone accelerates smoothly; outside pointers saturate at 720 px/s.
/// Clamp delayed frames so returning from a suspended tab cannot jump the document.
pub fn selection_scroll_delta(point: f64, start: f64, end: f64, elapsed_ms: f64) -> f64 {
    if ![point, start, end, elapsed_ms]
        .iter()
        .all(|value| value.is_finite())
        || end <= start
        || elapsed_ms <= 0.0
    {
        return 0.0;
    }
    let edge = ((end - start) / 4.0).min(24.0);
    let proximity = if point < start + edge {
        -((start + edge - point) / edge).min(1.0)
    } else if point > end - edge {
        ((point - (end - edge)) / edge).min(1.0)
    } else {
        0.0
    };
    proximity * proximity.abs() * 720.0 * elapsed_ms.min(50.0) / 1000.0
}

impl Document {
    fn pointer_unit(&self, offset: usize, unit: Unit) -> Result<Range<usize>, EditError> {
        if !super::valid_position(&self.text, offset) {
            return Err(EditError::InvalidSelection);
        }
        let index = row_at(&self.line_index.rows, offset);
        let row = &self.line_index.rows[index];
        if unit == Unit::Line {
            return Ok(row.start..row.end);
        }
        let body = &self.text[row.start..row.body_end];
        let at = offset.min(row.body_end) - row.start;
        let snapped = self.line_index.coordinates[index]
            .visual()
            .and_then(|coordinates| {
                coordinates
                    .index_at_byte(body, at)
                    .and_then(|glyph| coordinates.at(body, glyph))
                    .map(|(byte, _)| byte)
            })
            .unwrap_or_else(|| {
                if at == body.len() {
                    at
                } else {
                    body.grapheme_indices(true)
                        .take_while(|(byte, _)| *byte <= at)
                        .last()
                        .map_or(0, |(byte, _)| byte)
                }
            });
        if unit == Unit::Caret || body.is_empty() {
            return Ok(row.start + snapped..row.start + snapped);
        }
        if let Some(range) = self.line_index.coordinates[index]
            .visual()
            .and_then(|coordinates| coordinates.word_selection(body, snapped))
        {
            return Ok(row.start + range.start..row.start + range.end);
        }
        let mut start = 0;
        let mut previous = None;
        for (byte, glyph) in body.grapheme_indices(true) {
            let kind = super::motion::category(glyph);
            if previous.is_some_and(|before| before != kind) {
                if snapped < byte {
                    return Ok(row.start + start..row.start + byte);
                }
                start = byte;
            }
            previous = Some(kind);
        }
        Ok(row.start + start..row.body_end)
    }

    pub fn begin_pointer_selection(
        &mut self,
        offset: usize,
        clicks: u32,
        extend: bool,
    ) -> Result<PointerSelection, EditError> {
        let unit = match clicks {
            0 | 1 => Unit::Caret,
            2 => Unit::Word,
            _ => Unit::Line,
        };
        let hit = self.pointer_unit(offset, unit)?;
        let anchor = if extend {
            let anchor = self.selections[0].anchor;
            anchor..anchor
        } else {
            hit
        };
        let pointer = PointerSelection {
            anchor,
            unit,
            revision: self.revision,
            identity: self.identity.clone(),
        };
        self.drag_pointer_selection(&pointer, offset)?;
        Ok(pointer)
    }

    pub fn drag_pointer_selection(
        &mut self,
        pointer: &PointerSelection,
        offset: usize,
    ) -> Result<bool, EditError> {
        if self.revision != pointer.revision
            || !std::sync::Arc::ptr_eq(&self.identity, &pointer.identity)
        {
            return Err(EditError::StaleContext);
        }
        let hit = self.pointer_unit(offset, pointer.unit)?;
        let selection = if hit.start < pointer.anchor.start {
            Selection {
                anchor: pointer.anchor.end,
                head: hit.start,
            }
        } else {
            Selection {
                anchor: pointer.anchor.start,
                head: hit.end.max(pointer.anchor.end),
            }
        };
        let changed = self.selections != [selection];
        self.set_selections(vec![selection])?;
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indexed_pointer_words_match_complete_units_including_cluster_interiors_and_eof() {
        for body in [
            format!(
                "{}{}{}{} end",
                "word_".repeat(16_000),
                "!".repeat(1024),
                " \t".repeat(512),
                "👩‍💻🇺🇸".repeat(2048)
            ),
            format!("before a{} \t end", "\u{301}".repeat(40_000)),
            format!("{} ++ end", "\u{600}a\u{301}文".repeat(16_000)),
        ] {
            let units: Vec<_> = body
                .grapheme_indices(true)
                .map(|(start, glyph)| {
                    (
                        start..start + glyph.len(),
                        super::super::motion::category(glyph),
                    )
                })
                .collect();
            let count = body.chars().count();
            let positions: Vec<_> = body
                .char_indices()
                .step_by((count / 16).max(1))
                .map(|(at, _)| at)
                .chain([body.len()])
                .collect();
            for ending in ["\n", "\r\n"] {
                let source = format!("prefix{ending}{body}{ending}tail");
                let base = "prefix".len() + ending.len();
                let mut document = Document::new(&source);
                assert!(document.line_index.coordinates[1].visual().is_some());
                for &at in &positions {
                    let hit = units
                        .partition_point(|(range, _)| range.start <= at)
                        .saturating_sub(1);
                    let kind = units[hit].1;
                    let mut first = hit;
                    let mut last = hit;
                    while first > 0 && units[first - 1].1 == kind {
                        first -= 1;
                    }
                    while last + 1 < units.len() && units[last + 1].1 == kind {
                        last += 1;
                    }
                    document
                        .begin_pointer_selection(base + at, 2, false)
                        .unwrap();
                    assert_eq!(
                        document.selections(),
                        [Selection {
                            anchor: base + units[first].0.start,
                            head: base + units[last].0.end
                        }],
                        "at={at}"
                    );
                }
            }
        }
    }
    #[test]
    fn selection_scroll_preserves_direction_speed_and_suspended_frame_bounds() {
        assert!(selection_scroll_delta(100.0, 0.0, 200.0, 20.0).abs() < f64::EPSILON);
        let right = selection_scroll_delta(200.0, 0.0, 200.0, 20.0);
        let left = selection_scroll_delta(0.0, 0.0, 200.0, 20.0);
        assert!((right + left).abs() < f64::EPSILON);
        assert!((right - 14.4).abs() < 1e-10);
        assert!(selection_scroll_delta(188.0, 0.0, 200.0, 20.0) < right);
        assert!((selection_scroll_delta(1000.0, 0.0, 200.0, 20.0) - right).abs() < f64::EPSILON);
        assert!((selection_scroll_delta(200.0, 0.0, 200.0, 10.0) * 2.0 - right).abs() < 1e-10);
        assert!((selection_scroll_delta(200.0, 0.0, 200.0, 5000.0) - 36.0).abs() < f64::EPSILON);
        assert!(selection_scroll_delta(5.0, 0.0, 10.0, 20.0).abs() < f64::EPSILON);
        for (point, start, end, time) in [
            (f64::NAN, 0.0, 200.0, 20.0),
            (0.0, 0.0, 0.0, 20.0),
            (0.0, 20.0, 0.0, 20.0),
            (0.0, 0.0, 200.0, -1.0),
            (0.0, 0.0, 200.0, f64::INFINITY),
        ] {
            assert!(selection_scroll_delta(point, start, end, time).abs() < f64::EPSILON);
        }
    }
    #[test]
    fn pointer_units_preserve_unicode_direction_and_crlf() {
        let text = "a\u{301}bc  文_foo!\r\nlast";
        let mut document = Document::new(text);
        let pointer = document.begin_pointer_selection(1, 1, false).unwrap();
        assert_eq!(document.selections(), &[Selection::caret(0)]);
        document.drag_pointer_selection(&pointer, 4).unwrap();
        assert_eq!(document.selections(), &[Selection { anchor: 0, head: 4 }]);
        let word = document.begin_pointer_selection(10, 2, false).unwrap();
        assert_eq!(&text[document.selections()[0].range()], "文_foo");
        document.drag_pointer_selection(&word, 0).unwrap();
        assert_eq!(
            &text[document.selections()[0].range()],
            "a\u{301}bc  文_foo"
        );
        assert!(document.selections()[0].anchor > document.selections()[0].head);
        let line = document
            .begin_pointer_selection(text.len(), 3, false)
            .unwrap();
        assert_eq!(&text[document.selections()[0].range()], "last");
        document.drag_pointer_selection(&line, 0).unwrap();
        assert_eq!(
            document.selections()[0],
            Selection {
                anchor: text.len(),
                head: 0
            }
        );
        assert!(!document.is_dirty());
        assert!(!document.undo());
    }
    #[test]
    fn shift_pointer_uses_existing_anchor_and_rejects_invalid_offsets() {
        let mut document = Document::new("one two\r\n");
        document
            .set_selections(vec![Selection { anchor: 5, head: 6 }])
            .unwrap();
        let pointer = document.begin_pointer_selection(1, 1, true).unwrap();
        assert_eq!(document.selections(), &[Selection { anchor: 5, head: 1 }]);
        document.drag_pointer_selection(&pointer, 7).unwrap();
        assert_eq!(document.selections(), &[Selection { anchor: 5, head: 7 }]);
        let before = document.clone();
        assert_eq!(
            document.begin_pointer_selection(10, 1, false),
            Err(EditError::InvalidSelection)
        );
        assert_eq!(document, before);
        let mut document = Document::new("");
        document.begin_pointer_selection(0, 3, false).unwrap();
        assert_eq!(document.selections(), &[Selection::caret(0)]);
    }
    #[test]
    fn replacement_documents_cannot_reuse_a_pointer_gesture() {
        let mut original = Document::new("one two");
        let pointer = original.begin_pointer_selection(1, 1, false).unwrap();
        let mut shared = original.clone();
        assert!(shared.drag_pointer_selection(&pointer, 6).unwrap());
        for text in ["one two", "new two"] {
            let mut replacement = Document::new(text);
            let before = replacement.clone();
            assert_eq!(replacement.revision(), original.revision());
            assert_eq!(
                replacement.drag_pointer_selection(&pointer, 6),
                Err(EditError::StaleContext)
            );
            assert_eq!(replacement, before);
        }
    }
    #[test]
    fn edits_invalidate_pointer_gestures_without_changing_new_selections() {
        let mut document = Document::new("one two");
        let pointer = document.begin_pointer_selection(1, 2, false).unwrap();
        document
            .apply(
                vec![super::super::Edit::replace(0..3, "new")],
                vec![Selection::caret(3)],
                None,
            )
            .unwrap();
        let before = document.clone();
        assert_eq!(
            document.drag_pointer_selection(&pointer, 6),
            Err(EditError::StaleContext)
        );
        assert_eq!(document, before);
    }
}
