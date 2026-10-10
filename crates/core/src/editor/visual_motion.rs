//! Measured visual rows are data; movement and selection policy stay in Rust.
use super::{EditError, FoldProjection, SelectionError};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use unicode_segmentation::UnicodeSegmentation;

pub const MAX_VISUAL_CARETS: usize = 65_536;

/// One insertion position; columns are measured in 1/64 CSS pixels.
/// A soft-wrap boundary can have a position on both adjacent visual rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisualCaret {
    pub offset: usize,
    pub column: i64,
    /// A global row index for `new`, or a stable line-relative ID for
    /// `neighborhood`. Neighbor links, rather than numeric gaps, drive motion.
    pub row: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisualLayout {
    pub(super) source: Arc<String>,
    pub(super) projection: FoldProjection,
    pub(super) identity: String,
    carets: BTreeMap<usize, Vec<VisualCaret>>,
    neighbors: BTreeMap<usize, (Option<usize>, Option<usize>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct VisualGoal {
    pub column: i64,
    pub caret: VisualCaret,
    pub source_head: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MotionColumns {
    Logical(Vec<usize>),
    Visual {
        identity: String,
        goals: Vec<VisualGoal>,
    },
}

/// Bounded grapheme positions for the browser measurement adapter.
pub fn visual_caret_offsets(text: &str) -> Result<Vec<usize>, SelectionError> {
    let offsets: Vec<_> = text
        .grapheme_indices(true)
        .map(|(offset, _)| offset)
        .chain(std::iter::once(text.len()))
        .take(MAX_VISUAL_CARETS + 1)
        .collect();
    if offsets.len() > MAX_VISUAL_CARETS {
        return Err(SelectionError::TooLarge);
    }
    Ok(offsets)
}

/// Index a bounded logical line independently of the number of measured carets.
/// Each pair is a grapheme boundary in source bytes and browser UTF-16 units.
pub fn visual_line_offsets(text: &str) -> Result<Vec<(usize, usize)>, SelectionError> {
    if text.len() > super::MAX_STRUCTURE_BYTES {
        return Err(SelectionError::TooLarge);
    }
    let mut utf16 = 0;
    let mut offsets = Vec::new();
    for (byte, grapheme) in text.grapheme_indices(true) {
        offsets.push((byte, utf16));
        utf16 += grapheme.encode_utf16().count();
    }
    offsets.push((text.len(), utf16));
    Ok(offsets)
}

/// Keep browser shaping/range primitives on short text runs without splitting
/// grapheme clusters. A single unusually large cluster remains indivisible.
pub fn visual_text_runs(text: &str) -> Vec<&str> {
    visual_text_run_ranges(text)
        .map(|range| &text[range])
        .collect()
}

/// Original source boundaries remain stable when adapters paint only part of a
/// paragraph. Iteration stops with the requested window, without allocating or
/// segmenting the unused suffix.
pub fn visual_text_run_ranges(text: &str) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
    let mut clusters = text.grapheme_indices(true);
    let mut start = 0;
    std::iter::from_fn(move || {
        if start == text.len() {
            return None;
        }
        let end = clusters
            .find(|(offset, _)| offset - start >= 512)
            .map_or(text.len(), |(offset, _)| offset);
        let range = start..end;
        start = end;
        Some(range)
    })
}

fn valid_caret_offsets(projection: &FoldProjection, carets: &[VisualCaret]) -> bool {
    let mut by_line: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for caret in carets {
        let Some(line) = projection
            .lines()
            .partition_point(|line| line.visible_start <= caret.offset)
            .checked_sub(1)
        else {
            return false;
        };
        by_line
            .entry(line)
            .or_default()
            .insert(caret.offset - projection.lines()[line].visible_start);
    }
    for (line, offsets) in by_line {
        let start = projection.lines()[line].visible_start;
        let end = projection
            .lines()
            .get(line + 1)
            .map_or(projection.text().len(), |next| next.visible_start);
        let text = &projection.text()[start..end];
        let body = text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix('\n'))
            .unwrap_or(text);
        if let Some(index) = projection.visual_line_index(line) {
            let bytes: Vec<_> = offsets
                .iter()
                .copied()
                .take_while(|offset| *offset <= body.len())
                .collect();
            for chunk in bytes.chunks(super::MAX_ROW_GEOMETRY_ANCHORS) {
                if index
                    .boundary_glyphs(body, chunk)
                    .is_none_or(|glyphs| glyphs.len() != chunk.len())
                {
                    return false;
                }
            }
            if offsets
                .iter()
                .any(|offset| *offset > body.len() && *offset != text.len())
            {
                return false;
            }
        } else {
            // Short rows need one bounded scan, not a scan of the file prefix.
            // Larger rows must carry complete prepared visual coordinates.
            if body.len() > super::MAX_MEASURE_BYTES {
                return false;
            }
            let mut pending = offsets;
            for offset in text
                .grapheme_indices(true)
                .map(|(offset, _)| offset)
                .chain([text.len()])
            {
                pending.remove(&offset);
                if pending.is_empty() {
                    break;
                }
            }
            if !pending.is_empty() {
                return false;
            }
        }
    }
    true
}

impl VisualLayout {
    /// Partial measurement is allowed, but every moved cursor must have its
    /// current and neighboring visual row. Missing coverage rejects atomically.
    pub fn new(
        source: &str,
        projection: FoldProjection,
        identity: String,
        rows: usize,
        carets: Vec<VisualCaret>,
    ) -> Result<Self, SelectionError> {
        if source.len() > super::MAX_EDITOR_BYTES {
            return Err(SelectionError::TooLarge);
        }
        Self::from_source(
            Arc::new(source.to_owned()),
            projection,
            identity,
            rows,
            carets,
        )
    }

    fn from_source(
        source: Arc<String>,
        projection: FoldProjection,
        identity: String,
        rows: usize,
        carets: Vec<VisualCaret>,
    ) -> Result<Self, SelectionError> {
        if source.len() > super::MAX_EDITOR_BYTES || carets.len() > MAX_VISUAL_CARETS {
            return Err(SelectionError::TooLarge);
        }
        if identity.is_empty()
            || identity.len() > 1024
            || rows == 0
            || carets.is_empty()
            || carets.iter().any(|caret| {
                caret.row >= rows
                    || caret.column.unsigned_abs() > i64::MAX as u64 / 4
                    || caret.offset > projection.text().len()
            })
        {
            return Err(EditError::InvalidSelection.into());
        }
        if !valid_caret_offsets(&projection, &carets) {
            return Err(EditError::InvalidSelection.into());
        }
        let mut by_row: BTreeMap<usize, Vec<VisualCaret>> = BTreeMap::new();
        for caret in carets {
            by_row.entry(caret.row).or_default().push(caret);
        }
        for row in by_row.values_mut() {
            row.sort_by_key(|caret| (caret.column, caret.offset));
            row.dedup();
        }
        let neighbors = by_row
            .keys()
            .map(|&row| {
                (
                    row,
                    (Some(row.saturating_sub(1)), Some((row + 1).min(rows - 1))),
                )
            })
            .collect();
        Ok(Self {
            source,
            projection,
            identity,
            carets: by_row,
            neighbors,
        })
    }

    /// Line-relative row IDs remain stable without a full document-height table.
    /// The same target policy consumes either complete or neighborhood geometry.
    pub fn neighborhood(
        source: &str,
        projection: FoldProjection,
        identity: String,
        lines: &[super::VisualLineRows],
        carets: Vec<VisualCaret>,
    ) -> Result<Self, SelectionError> {
        if source.len() > super::MAX_EDITOR_BYTES {
            return Err(SelectionError::TooLarge);
        }
        Self::neighborhood_source(
            Arc::new(source.to_owned()),
            projection,
            identity,
            lines,
            carets,
        )
    }

    /// Retain the exact source while validating only measured logical rows.
    pub fn neighborhood_source(
        source: Arc<String>,
        projection: FoldProjection,
        identity: String,
        lines: &[super::VisualLineRows],
        carets: Vec<VisualCaret>,
    ) -> Result<Self, SelectionError> {
        for caret in &carets {
            let line = |at| {
                projection
                    .lines()
                    .partition_point(|line| line.visible_start <= at)
                    .checked_sub(1)
            };
            if line(caret.row).is_none() || line(caret.row) != line(caret.offset) {
                return Err(EditError::InvalidSelection.into());
            }
        }
        let ids = carets
            .iter()
            .map(|caret| caret.row)
            .collect::<BTreeSet<_>>();
        let neighbors = super::visual_neighbors::neighbors(&projection, lines, &ids)?;
        let bound = projection
            .text()
            .len()
            .checked_add(1)
            .ok_or(SelectionError::TooLarge)?;
        let mut result = Self::from_source(source, projection, identity, bound, carets)?;
        result.neighbors = neighbors;
        Ok(result)
    }

    /// Default soft-wrap affinity for a measured source caret. Consumers that
    /// reveal a selection use the same following-row affinity as cursor motion.
    pub fn caret(&self, offset: usize) -> Option<VisualCaret> {
        self.carets
            .values()
            .flat_map(|row| row.iter())
            .filter(|caret| caret.offset == offset)
            .max_by_key(|caret| caret.row)
            .copied()
    }

    pub(super) fn target(
        &self,
        at: usize,
        down: bool,
        previous: Option<&VisualGoal>,
    ) -> Result<(usize, VisualGoal), SelectionError> {
        // Default affinity is the start of the following wrapped row. Vertical
        // motion retains its selected row when the same byte has two positions.
        let current = self
            .carets
            .values()
            .flat_map(|row| row.iter())
            .filter(|caret| caret.offset == at)
            .max_by_key(|caret| {
                (
                    previous
                        .is_some_and(|goal| goal.caret.offset == at && goal.caret.row == caret.row),
                    caret.row,
                )
            })
            .ok_or(EditError::InvalidSelection)?;
        let column = previous.map_or(current.column, |goal| goal.column);
        let neighbors = self
            .neighbors
            .get(&current.row)
            .ok_or(EditError::InvalidSelection)?;
        let target_row =
            if down { neighbors.1 } else { neighbors.0 }.ok_or(EditError::InvalidSelection)?;
        let target = if target_row == current.row {
            current
        } else {
            self.carets
                .get(&target_row)
                .and_then(|row| {
                    row.iter()
                        .min_by_key(|caret| (caret.column.abs_diff(column), caret.offset))
                })
                .ok_or(EditError::InvalidSelection)?
        };
        Ok((
            target.offset,
            VisualGoal {
                column,
                caret: *target,
                source_head: 0,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Document, Indentation, Selection, SelectionMotion};

    #[test]
    fn large_neighborhoods_share_source_and_support_queued_motion_with_sparse_validation() {
        let row = "abcdefghijklmnopqrstuvwx 文😀\r\n";
        let source = row.repeat(super::super::MAX_STRUCTURE_BYTES / row.len() + 1);
        let mut doc = Document::for_editor(source.clone()).unwrap();
        let projection = doc.projection();
        let line = projection.lines().len() / 2;
        let start = projection.lines()[line].visible_start;
        doc.set_selections(vec![Selection::caret(start + 1)])
            .unwrap();
        let lines: Vec<_> = (line - 1..=line + 1)
            .map(|line| super::super::VisualLineRows { line, rows: 1 })
            .collect();
        let carets: Vec<_> = lines
            .iter()
            .flat_map(|line| {
                let start = projection.lines()[line.line].visible_start;
                row.strip_suffix("\r\n")
                    .unwrap()
                    .grapheme_indices(true)
                    .map(move |(offset, _)| VisualCaret {
                        offset: start + offset,
                        column: i64::try_from(offset).unwrap() * 64,
                        row: start,
                    })
            })
            .collect();
        let retained = doc.shared_text();
        let layout = VisualLayout::neighborhood_source(
            retained.clone(),
            projection,
            "large".into(),
            &lines,
            carets,
        )
        .unwrap();
        assert!(Arc::ptr_eq(&layout.source, &retained));
        let mut queue = super::super::MotionQueue::new(&doc).unwrap();
        queue
            .push(super::super::MotionRequest {
                motion: SelectionMotion::Down,
                extend: false,
                wrapped: true,
            })
            .unwrap();
        queue
            .apply_next(&mut doc, Indentation::default(), Some(&layout))
            .unwrap();
        assert_eq!(doc.selections(), &[Selection::caret(start + row.len() + 1)]);
        assert_eq!(doc.text(), source);
        assert!(!doc.can_undo());
        assert!(!doc.is_dirty());
    }

    #[test]
    fn sparse_caret_validation_matches_complete_unicode_and_line_ending_boundaries() {
        for body in [
            "文😀e\u{301} ".repeat(8_000),
            format!("e{} tail", "\u{301}".repeat(70_000)),
            "🇦🇧🇨🇩 ".repeat(5_000),
        ] {
            let source = format!("short\n{body}\r\nlast");
            let document = Document::for_editor(source.clone()).unwrap();
            let projection = document.projection();
            let expected: BTreeSet<_> = source
                .grapheme_indices(true)
                .map(|(offset, _)| offset)
                .chain([source.len()])
                .collect();
            let offsets = source
                .char_indices()
                .map(|(offset, _)| offset)
                .step_by(127)
                .chain([
                    6,
                    6 + body.len(),
                    7 + body.len(),
                    8 + body.len(),
                    source.len(),
                ]);
            for offset in offsets {
                assert_eq!(
                    valid_caret_offsets(
                        &projection,
                        &[VisualCaret {
                            offset,
                            column: 0,
                            row: 0
                        }]
                    ),
                    expected.contains(&offset),
                    "boundary at {offset}"
                );
            }
        }
        let document = Document::for_editor("x".repeat(65_537)).unwrap();
        let carets: Vec<_> = (0..MAX_VISUAL_CARETS)
            .map(|offset| VisualCaret {
                offset,
                column: 0,
                row: 0,
            })
            .collect();
        assert!(valid_caret_offsets(&document.projection(), &carets));
    }

    #[test]
    fn long_line_index_keeps_unicode_byte_and_utf16_boundaries() {
        let text = "文😀e\u{301} ".repeat(20_000);
        assert_eq!(visual_caret_offsets(&text), Err(SelectionError::TooLarge));
        let offsets = visual_line_offsets(&text).unwrap();
        assert_eq!(&offsets[..5], &[(0, 0), (3, 1), (7, 3), (10, 5), (11, 6)]);
        assert_eq!(offsets.last(), Some(&(text.len(), 120_000)));
        assert_eq!(offsets.len(), 80_001);
        assert_eq!(
            visual_line_offsets(&"x".repeat(super::super::MAX_STRUCTURE_BYTES + 1)),
            Err(SelectionError::TooLarge)
        );
    }

    #[test]
    fn shaping_runs_preserve_complete_clusters_source_and_empty_text() {
        let text = format!(
            "{}e{} tail",
            "文😀e\u{301} ".repeat(1000),
            "\u{301}".repeat(600)
        );
        let runs = visual_text_runs(&text);
        assert_eq!(runs.concat(), text);
        let boundaries: BTreeSet<_> = text
            .grapheme_indices(true)
            .map(|(offset, _)| offset)
            .chain(std::iter::once(text.len()))
            .collect();
        let mut offset = 0;
        for run in runs {
            assert!(boundaries.contains(&offset));
            offset += run.len();
            assert!(boundaries.contains(&offset));
        }
        assert!(visual_text_runs("").is_empty());
    }

    fn wrapped(doc: &Document, identity: &str) -> VisualLayout {
        let carets = [
            (0, 0, 0),
            (1, 10, 0),
            (2, 20, 0),
            (3, 30, 0),
            (3, 0, 1),
            (4, 10, 1),
            (5, 20, 1),
            (6, 30, 1),
            (7, 0, 2),
            (8, 10, 2),
            (9, 20, 2),
            (10, 0, 3),
            (11, 10, 3),
            (12, 20, 3),
            (13, 30, 3),
            (13, 0, 4),
            (14, 10, 4),
            (15, 20, 4),
            (16, 30, 4),
        ]
        .into_iter()
        .map(|(offset, column, row)| VisualCaret {
            offset,
            column,
            row,
        })
        .collect();
        VisualLayout::new(doc.text(), doc.projection(), identity.into(), 5, carets).unwrap()
    }

    #[test]
    fn vertical_motion_keeps_pixel_goals_wrap_affinity_and_selection_direction() {
        let mut doc = Document::new("abcdef\nxy\nabcdef");
        doc.set_selections(vec![Selection::caret(6), Selection::caret(16)])
            .unwrap();
        let layout = wrapped(&doc, "wide");
        let step = |doc: &mut Document, motion, extend| {
            doc.move_selections_with_layout(motion, extend, Indentation::default(), &layout)
                .unwrap()
        };
        step(&mut doc, SelectionMotion::Down, true);
        assert_eq!(
            doc.selections(),
            &[Selection { anchor: 6, head: 9 }, Selection::caret(16)]
        );
        step(&mut doc, SelectionMotion::Down, true);
        assert_eq!(
            doc.selections()[0],
            Selection {
                anchor: 6,
                head: 13
            }
        );
        step(&mut doc, SelectionMotion::Up, true);
        assert_eq!(doc.selections()[0], Selection { anchor: 6, head: 9 });
        step(&mut doc, SelectionMotion::Up, true);
        assert_eq!(doc.selections()[0], Selection::caret(6));
        assert!(!doc.is_dirty());
        assert_eq!(doc.revision(), 0);
        assert!(!doc.undo());
    }

    #[test]
    fn layout_changes_reset_goals_and_missing_rows_or_stale_sources_reject_atomically() {
        let mut doc = Document::new("abcdef\nxy\nabcdef");
        doc.set_selections(vec![Selection::caret(6), Selection::caret(16)])
            .unwrap();
        let layout = wrapped(&doc, "wide");
        doc.move_selections_with_layout(
            SelectionMotion::Down,
            false,
            Indentation::default(),
            &layout,
        )
        .unwrap();
        let resized = wrapped(&doc, "narrow");
        doc.move_selections_with_layout(
            SelectionMotion::Down,
            false,
            Indentation::default(),
            &resized,
        )
        .unwrap();
        assert_eq!(doc.selections()[0], Selection::caret(12));
        let missing = VisualLayout::new(
            doc.text(),
            doc.projection(),
            "missing".into(),
            5,
            vec![
                VisualCaret {
                    offset: 12,
                    column: 20,
                    row: 3,
                },
                VisualCaret {
                    offset: 16,
                    column: 30,
                    row: 4,
                },
            ],
        )
        .unwrap();
        let before = doc.clone();
        assert!(
            doc.move_selections_with_layout(
                SelectionMotion::Up,
                false,
                Indentation::default(),
                &missing
            )
            .is_err()
        );
        assert_eq!(doc, before);
        let mut different = Document::new("abcdeg\nxy\nabcdef");
        let before = different.clone();
        assert_eq!(
            different.move_selections_with_layout(
                SelectionMotion::Down,
                false,
                Indentation::default(),
                &layout
            ),
            Err(EditError::StaleContext.into())
        );
        assert_eq!(different, before);
    }

    #[test]
    fn fold_changes_invalidate_measurements_and_transient_caret_affinity() {
        use crate::editor::{FoldCommand, FoldRange};
        let mut doc = Document::new("header\nhidden\ntail");
        let layout = VisualLayout::new(
            doc.text(),
            doc.projection(),
            "fold".into(),
            3,
            vec![
                VisualCaret {
                    offset: 0,
                    column: 0,
                    row: 0,
                },
                VisualCaret {
                    offset: 7,
                    column: 0,
                    row: 1,
                },
                VisualCaret {
                    offset: 14,
                    column: 0,
                    row: 2,
                },
            ],
        )
        .unwrap();
        doc.move_selections_with_layout(
            SelectionMotion::Down,
            false,
            Indentation::default(),
            &layout,
        )
        .unwrap();
        assert!(doc.visual_caret(0, "fold").is_some());
        doc.fold_state_mut().set_ranges(
            vec![FoldRange {
                start_line: 0,
                end_line: 1,
            }],
            3,
        );
        doc.fold_command(FoldCommand::Toggle(0));
        assert!(doc.visual_caret(0, "fold").is_none());
        let before = doc.clone();
        assert_eq!(
            doc.move_selections_with_layout(
                SelectionMotion::Down,
                false,
                Indentation::default(),
                &layout
            ),
            Err(EditError::StaleContext.into())
        );
        assert_eq!(doc, before);
    }

    #[test]
    fn measurements_cannot_split_graphemes_or_exceed_shared_limits() {
        let doc = Document::new("e\u{301}😀\r\n");
        assert_eq!(visual_caret_offsets(doc.text()).unwrap(), vec![0, 3, 7, 9]);
        assert!(
            VisualLayout::new(
                doc.text(),
                doc.projection(),
                "bad".into(),
                1,
                vec![VisualCaret {
                    offset: 1,
                    column: 0,
                    row: 0
                }]
            )
            .is_err()
        );
        assert!(visual_caret_offsets(&"a".repeat(MAX_VISUAL_CARETS)).is_err());
    }
}
