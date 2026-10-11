//! Selection movement is source policy; the browser only maps keys and paints.
use super::visual_motion::{MotionColumns, VisualGoal, VisualLayout};
use super::{
    Document, EditError, Indentation, Selection, SelectionError,
    lines::Line,
    selections::{byte_at_column, display_column},
};
use unicode_segmentation::GraphemeCursor;
#[cfg(test)]
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionMotion {
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    LineStart,
    LineEnd,
    DocumentStart,
    DocumentEnd,
}

fn previous(text: &str, at: usize) -> usize {
    GraphemeCursor::new(at, text.len(), true)
        .prev_boundary(text, 0)
        .expect("complete source supplies the grapheme cursor's context")
        .unwrap_or(0)
}
fn next(text: &str, at: usize) -> usize {
    GraphemeCursor::new(at, text.len(), true)
        .next_boundary(text, 0)
        .expect("complete source supplies the grapheme cursor's context")
        .unwrap_or(text.len())
}
pub(super) fn category(grapheme: &str) -> u8 {
    if grapheme.chars().all(char::is_whitespace) {
        0
    } else if grapheme
        .chars()
        .any(|character| character.is_alphanumeric() || character == '_')
    {
        1
    } else {
        2
    }
}
// A run of ASCII whitespace consists entirely of category-zero clusters, except
// where its endpoint joins a neighboring Unicode cluster (e.g. Prepend or Extend).
// Prove that seam before bypassing per-grapheme classification. Newlines and
// CRLF stay part of the same run; the cursor sees the original sliced source.
fn whitespace_end(text: &str, at: usize, left: bool) -> Option<usize> {
    let offset = if left {
        at - text.as_bytes()[..at]
            .iter()
            .rev()
            .take_while(|byte| byte.is_ascii_whitespace())
            .count()
    } else {
        at + text.as_bytes()[at..]
            .iter()
            .take_while(|byte| byte.is_ascii_whitespace())
            .count()
    };
    if offset == at {
        return None;
    }
    let mut seam = GraphemeCursor::new(offset, text.len(), true);
    let end = if seam
        .is_boundary(text, 0)
        .expect("complete source supplies whitespace seam context")
    {
        offset
    } else if left {
        seam.next_boundary(text, 0)
            .expect("complete source supplies whitespace seam context")?
    } else {
        seam.prev_boundary(text, 0)
            .expect("complete source supplies whitespace seam context")?
    };
    // Leave a joined endpoint cluster to ordinary classification, without
    // rescanning the entire run once for each preceding whitespace character.
    ((left && end < at) || (!left && end > at)).then_some(end)
}

fn word_left(text: &str, at: usize) -> usize {
    let text = &text[..at];
    let mut result = at;
    let mut kind = None;
    let mut cursor = GraphemeCursor::new(at, text.len(), true);
    while cursor.cur_cursor() > 0 {
        let end = cursor.cur_cursor();
        if kind.is_none()
            && let Some(offset) = whitespace_end(text, end, true)
        {
            result = offset;
            cursor = GraphemeCursor::new(offset, text.len(), true);
            continue;
        }
        let offset = cursor
            .prev_boundary(text, 0)
            .expect("complete source supplies word cursor context")
            .unwrap_or(0);
        let current = category(&text[offset..end]);
        if kind.is_some_and(|kind| kind != current) {
            break;
        }
        result = offset;
        if current != 0 {
            kind = Some(current);
        }
    }
    result
}
fn word_right(text: &str, at: usize) -> usize {
    let text = &text[at..];
    let mut result = 0;
    let mut kind = None;
    let mut cursor = GraphemeCursor::new(0, text.len(), true);
    while cursor.cur_cursor() < text.len() {
        let offset = cursor.cur_cursor();
        if let Some(end) = whitespace_end(text, offset, false) {
            result = end;
            kind = Some(0);
            cursor = GraphemeCursor::new(end, text.len(), true);
            continue;
        }
        let end = cursor
            .next_boundary(text, 0)
            .expect("complete source supplies word cursor context")
            .unwrap_or(text.len());
        let current = category(&text[offset..end]);
        if kind.is_some_and(|kind| kind != current) && current != 0 {
            break;
        }
        result = end;
        kind = Some(current);
    }
    at + result
}

impl Document {
    /// Affinity is transient view state, validated against current measurements.
    pub fn visual_caret(&self, index: usize, identity: &str) -> Option<super::VisualCaret> {
        let MotionColumns::Visual {
            identity: current,
            goals,
        } = self.motion_columns.as_ref()?
        else {
            return None;
        };
        if current != identity {
            return None;
        }
        let goal = goals.get(index)?;
        (self.selections.get(index)?.head == goal.source_head).then_some(goal.caret)
    }

    pub fn move_selections(
        &mut self,
        motion: SelectionMotion,
        extend: bool,
        indentation: Indentation,
    ) -> Result<bool, SelectionError> {
        self.move_selections_in(motion, extend, indentation, None)
    }

    pub fn move_selections_with_layout(
        &mut self,
        motion: SelectionMotion,
        extend: bool,
        indentation: Indentation,
        layout: &VisualLayout,
    ) -> Result<bool, SelectionError> {
        if layout.source != self.text || layout.projection != self.projection() {
            return Err(EditError::StaleContext.into());
        }
        self.move_selections_in(motion, extend, indentation, Some(layout))
    }

    fn move_selections_in(
        &mut self,
        motion: SelectionMotion,
        extend: bool,
        indentation: Indentation,
        layout: Option<&VisualLayout>,
    ) -> Result<bool, SelectionError> {
        if self.is_composing() {
            return Err(EditError::CompositionActive.into());
        }
        if self.admission().limit().is_some() {
            return Err(SelectionError::TooLarge);
        }
        let projection = self.projection();
        let text = projection.text();
        let rows = projection.lines();
        let line_at = |row: usize| {
            let visible = &rows[row];
            let source = &self.line_index.rows[visible.source_line];
            Line {
                start: visible.visible_start,
                body_end: visible.visible_start + source.body_end - source.start,
                end: visible.visible_start + visible.source.len(),
            }
        };
        let vertical = matches!(motion, SelectionMotion::Up | SelectionMotion::Down);
        let mut columns = Vec::with_capacity(self.selections.len());
        let mut selections = Vec::with_capacity(self.selections.len());
        let mut visual_goals: Vec<VisualGoal> = Vec::with_capacity(self.selections.len());
        for (index, selection) in self.selections.iter().enumerate() {
            let visible = projection
                .visible_selection(*selection)
                .map_err(|_| EditError::InvalidSelection)?;
            let at = visible.head;
            let row = rows
                .partition_point(|line| line.visible_start <= at)
                .saturating_sub(1);
            let line = line_at(row);
            let column = if vertical {
                self.motion_columns
                    .as_ref()
                    .and_then(|columns| match columns {
                        MotionColumns::Logical(columns)
                            if vertical && columns.len() == self.selections.len() =>
                        {
                            Some(columns)
                        }
                        _ => None,
                    })
                    .map_or_else(
                        || {
                            display_column(
                                &text[line.start..at.min(line.body_end)],
                                indentation.tab_width(),
                            )
                        },
                        |columns| columns[index],
                    )
            } else {
                0
            };
            columns.push(column);
            let head = if !extend
                && visible.anchor != visible.head
                && matches!(motion, SelectionMotion::Left | SelectionMotion::WordLeft)
            {
                visible.range().start
            } else if !extend
                && visible.anchor != visible.head
                && matches!(motion, SelectionMotion::Right | SelectionMotion::WordRight)
            {
                visible.range().end
            } else {
                match motion {
                    SelectionMotion::Left => previous(text, at),
                    SelectionMotion::Right => next(text, at),
                    SelectionMotion::WordLeft | SelectionMotion::WordRight => {
                        let left = motion == SelectionMotion::WordLeft;
                        let indexed = (at <= line.body_end)
                            .then(|| {
                                self.line_index.coordinates[rows[row].source_line]
                                    .visual()?
                                    .word_boundary(
                                        &text[line.start..line.body_end],
                                        at - line.start,
                                        left,
                                    )
                            })
                            .flatten();
                        if let Some((offset, kind)) = indexed {
                            if left && offset == 0 && kind.is_none() {
                                word_left(text, line.start)
                            } else if !left && offset == line.body_end - line.start {
                                line.body_end + word_right(&text[line.body_end..], 0)
                            } else {
                                line.start + offset
                            }
                        } else if left {
                            word_left(text, at)
                        } else {
                            word_right(text, at)
                        }
                    }
                    SelectionMotion::LineStart => line.start,
                    SelectionMotion::LineEnd => line.body_end,
                    SelectionMotion::DocumentStart => 0,
                    SelectionMotion::DocumentEnd => text.len(),
                    SelectionMotion::Up | SelectionMotion::Down if layout.is_some() => {
                        let layout = layout.unwrap();
                        let previous =
                            self.motion_columns
                                .as_ref()
                                .and_then(|columns| match columns {
                                    MotionColumns::Visual { identity, goals }
                                        if identity == &layout.identity
                                            && goals.len() == self.selections.len() =>
                                    {
                                        goals.get(index)
                                    }
                                    _ => None,
                                });
                        let (target, goal) =
                            layout.target(at, motion == SelectionMotion::Down, previous)?;
                        visual_goals.push(goal);
                        target
                    }
                    SelectionMotion::Up | SelectionMotion::Down => {
                        let target = if motion == SelectionMotion::Up {
                            row.saturating_sub(1)
                        } else {
                            (row + 1).min(rows.len() - 1)
                        };
                        let line = line_at(target);
                        line.start
                            + byte_at_column(
                                &text[line.start..line.body_end],
                                column,
                                indentation.tab_width(),
                                false,
                            )
                    }
                }
            };
            let head = projection
                .source_offset(head)
                .map_err(|_| EditError::InvalidSelection)?;
            if layout.is_some()
                && vertical
                && let Some(goal) = visual_goals.last_mut()
            {
                goal.source_head = head;
            }
            selections.push(Selection {
                anchor: if extend { selection.anchor } else { head },
                head,
            });
        }
        let before = self.selections.clone();
        self.set_selections(selections)?;
        self.motion_columns = if vertical && self.selections.len() == columns.len() {
            Some(if let Some(layout) = layout {
                MotionColumns::Visual {
                    identity: layout.identity.clone(),
                    goals: visual_goals,
                }
            } else {
                MotionColumns::Logical(columns)
            })
        } else {
            None
        };
        Ok(self.selections != before)
    }
}

#[cfg(test)]
mod tests {

    fn reference_word(text: &str, at: usize, left: bool) -> usize {
        let mut result = at;
        let mut kind = None;
        let graphemes: Box<dyn Iterator<Item = (usize, &str)> + '_> = if left {
            Box::new(text[..at].grapheme_indices(true).rev())
        } else {
            Box::new(text[at..].grapheme_indices(true))
        };
        for (offset, grapheme) in graphemes {
            let current = category(grapheme);
            if kind.is_some_and(|kind| kind != current) && (left || current != 0) {
                break;
            }
            result = if left {
                offset
            } else {
                at + offset + grapheme.len()
            };
            if !left || current != 0 {
                kind = Some(current);
            }
        }
        result
    }

    #[test]
    fn whitespace_word_jumps_match_grapheme_reference_at_every_scalar_boundary() {
        let sources = [
            "",
            " ",
            "a ",
            "a \t\r\n b",
            "!  word + tail",
            "a\u{00a0} b",
            "a \u{0301} b",
            "\u{0600}  a",
            "a\u{0600}  \u{0301}b",
            "a\r\n\t \r b",
            "👩‍💻  文_\n  fin",
            "🇺🇸 \u{0301} 🇨🇦",
            "a \u{200d}👩  b",
            "\u{0600}\r\n  z",
        ];
        for text in sources {
            for at in text.char_indices().map(|(at, _)| at).chain([text.len()]) {
                assert_eq!(
                    word_left(text, at),
                    reference_word(text, at, true),
                    "left {text:?} @{at}"
                );
                assert_eq!(
                    word_right(text, at),
                    reference_word(text, at, false),
                    "right {text:?} @{at}"
                );
            }
        }
    }

    proptest::proptest! {
        #[test]
        fn word_cursor_matches_reference_through_arbitrary_unicode_seams(
            parts in proptest::collection::vec(
                proptest::sample::select(vec![
                    'a', '_', '+', ' ', '\t', '\r', '\n', '\u{000b}', '\u{000c}',
                    '\u{0301}', '\u{0600}', '\u{200d}', '\u{00a0}', '\u{2028}',
                    '文', '🇺', '🇸', '👩', '💻', '\u{1100}', '\u{1161}', '\u{11a8}',
                ]), 0..32,
            )
        ) {
            let text: String = parts.into_iter().collect();
            for at in text.char_indices().map(|(at, _)| at).chain([text.len()]) {
                proptest::prop_assert_eq!(word_left(&text, at), reference_word(&text, at, true));
                proptest::prop_assert_eq!(word_right(&text, at), reference_word(&text, at, false));
            }
        }
    }

    #[test]
    fn cross_row_whitespace_jumps_preserve_joined_endpoint_clusters() {
        for ending in ["\n", "\r\n"] {
            let spaces = format!(" \t{ending}").repeat(20_000);
            for (before, after) in [("word", "next"), ("\u{0600}", "\u{0301}word"), ("", "")] {
                let text = format!("{before}{spaces}{after}");
                for at in [0, before.len(), before.len() + spaces.len(), text.len()] {
                    assert_eq!(word_left(&text, at), reference_word(&text, at, true));
                    assert_eq!(word_right(&text, at), reference_word(&text, at, false));
                }
            }
        }
        let text = format!("\u{0600}{}\u{0301}word", " ".repeat(100_000));
        for at in [0, "\u{0600}".len(), text.len()] {
            assert_eq!(word_left(&text, at), reference_word(&text, at, true));
            assert_eq!(word_right(&text, at), reference_word(&text, at, false));
        }
    }

    #[test]
    fn indexed_word_motion_matches_complete_grapheme_traversal() {
        for body in [
            format!("{}  ++ next", "word_".repeat(20_000)),
            format!("{}word ++ tail", " \t".repeat(40_000)),
            format!("word{}tail", "!".repeat(80_000)),
            format!("! a{} ++ end", "\u{301}".repeat(40_000)),
            format!(
                "{} ++ {}  tail",
                "🇺🇸🇨🇦👩‍💻".repeat(2000),
                "文_אב".repeat(4000)
            ),
            format!("{}_word ++ end", "\u{600}a\u{301}".repeat(16_000)),
        ] {
            for ending in ["\n", "\r\n"] {
                let source = format!("prefix{ending}{body}{ending}  tail{ending}");
                let mut document = super::Document::new(&source);
                assert!(document.line_index.coordinates[1].visual().is_some());
                let count = source.chars().count();
                let positions: Vec<_> = source
                    .char_indices()
                    .step_by((count / 16).max(1))
                    .map(|(at, _)| at)
                    .chain([
                        source.len(),
                        source.find(&body).unwrap(),
                        source.find(&body).unwrap() + body.len(),
                    ])
                    .collect();
                for at in positions {
                    for motion in [
                        super::SelectionMotion::WordLeft,
                        super::SelectionMotion::WordRight,
                    ] {
                        let expected = if motion == super::SelectionMotion::WordLeft {
                            super::word_left(&source, at)
                        } else {
                            super::word_right(&source, at)
                        };
                        document
                            .set_selections(vec![super::Selection::caret(at)])
                            .unwrap();
                        document
                            .move_selections(motion, false, super::Indentation::default())
                            .unwrap();
                        assert_eq!(
                            document.selections(),
                            [super::Selection::caret(expected)],
                            "at={at}, motion={motion:?}"
                        );
                    }
                }
            }
        }
    }
    use super::*;
    #[test]
    fn admitted_large_documents_support_basic_motion_without_structure_analysis() {
        let row = "\t文😀e\u{301} words more words\r\n";
        let source = row.repeat(super::super::MAX_STRUCTURE_BYTES / row.len() + 1);
        let mut doc = Document::for_editor(source.clone()).unwrap();
        assert!(source.len() > super::super::MAX_STRUCTURE_BYTES);
        for (motion, expected) in [
            (SelectionMotion::DocumentEnd, source.len()),
            (SelectionMotion::Left, source.len() - 2),
            (SelectionMotion::LineStart, source.len() - row.len()),
            (SelectionMotion::Right, source.len() - row.len() + 1),
            (SelectionMotion::DocumentStart, 0),
            (SelectionMotion::WordRight, 1),
            (SelectionMotion::WordRight, 4),
            (SelectionMotion::LineEnd, row.len() - 2),
            (SelectionMotion::Down, row.len() * 2 - 2),
            (SelectionMotion::Up, row.len() - 2),
        ] {
            doc.move_selections(motion, false, Indentation::default())
                .unwrap();
            assert_eq!(
                doc.selections(),
                &[Selection::caret(expected)],
                "{motion:?}"
            );
        }
        doc.move_selections(SelectionMotion::DocumentStart, true, Indentation::default())
            .unwrap();
        assert_eq!(
            doc.selections(),
            &[Selection {
                anchor: row.len() - 2,
                head: 0
            }]
        );
        assert_eq!(doc.text(), source);
        assert!(!doc.can_undo());
        assert!(!doc.is_dirty());
    }

    #[test]
    fn direct_grapheme_cursors_match_complete_segmentation_at_every_character_boundary() {
        for text in [
            "",
            "ascii words\r\nlast",
            "a\u{301}\u{302}😀e\u{301}",
            "👨\u{200d}👩\u{200d}👧\u{200d}👦!",
            "🇦🇧🇨🇩🇪🇫🇬",
            "\u{600}a\rstandalone\nend",
            "क्\u{200d}ष क्क",
        ] {
            let starts: Vec<_> = text.grapheme_indices(true).map(|(byte, _)| byte).collect();
            for at in text
                .char_indices()
                .map(|(byte, _)| byte)
                .chain([text.len()])
            {
                let expected_previous = starts
                    .iter()
                    .copied()
                    .take_while(|byte| *byte < at)
                    .last()
                    .unwrap_or(0);
                let expected_next = starts
                    .iter()
                    .copied()
                    .find(|byte| *byte > at)
                    .unwrap_or(text.len());
                assert_eq!(
                    previous(text, at),
                    expected_previous,
                    "previous {text:?} at {at}"
                );
                assert_eq!(next(text, at), expected_next, "next {text:?} at {at}");
            }
        }
    }

    #[test]
    fn large_folded_motion_uses_visible_source_rows_and_keeps_hidden_text_intact() {
        let hidden = "hidden words in a row\n".repeat(100_000 - 3);
        let source = format!("head\n{hidden}tail");
        let mut doc = Document::for_editor(source.clone()).unwrap();
        assert!(source.len() > super::super::MAX_STRUCTURE_BYTES);
        doc.fold_state_mut().set_ranges(
            vec![super::super::FoldRange {
                start_line: 0,
                end_line: 100_000 - 3,
            }],
            100_000 - 1,
        );
        doc.fold_command(super::super::FoldCommand::CollapseAll);
        doc.set_selections(vec![Selection::caret(4)]).unwrap();
        doc.move_selections(SelectionMotion::Right, false, Indentation::default())
            .unwrap();
        assert_eq!(doc.selections(), &[Selection::caret(5 + hidden.len())]);
        assert!(doc.fold_state().collapsed_at(0).is_some());
        doc.move_selections(SelectionMotion::Up, false, Indentation::default())
            .unwrap();
        assert_eq!(doc.selections(), &[Selection::caret(0)]);
        assert_eq!(doc.text(), source);
        assert!(!doc.can_undo());
        assert!(!doc.is_dirty());
    }
    #[test]
    fn oversized_motion_keeps_selections_history_and_dirty_state_unchanged() {
        let mut doc = Document::new("x".repeat(super::super::MAX_STRUCTURE_BYTES + 1));
        doc.set_selections(vec![Selection::caret(0), Selection::caret(1)])
            .unwrap();
        let before = doc.clone();
        assert_eq!(
            doc.move_selections(SelectionMotion::Right, false, Indentation::default()),
            Err(SelectionError::TooLarge)
        );
        assert_eq!(doc, before);
        doc.selection_command(
            super::super::SelectionCommand::Single,
            crate::highlight::Language::Plain,
            Indentation::default(),
        )
        .unwrap();
        assert_eq!(doc.selections(), &[Selection::caret(0)]);
    }
    #[test]
    fn grapheme_motion_preserves_all_cursors_and_crlf_and_does_not_edit_history() {
        let mut doc = Document::new("a\u{301}😀\r\na\u{301}😀");
        doc.set_selections(vec![Selection::caret(7), Selection::caret(16)])
            .unwrap();
        doc.move_selections(SelectionMotion::Left, false, Indentation::default())
            .unwrap();
        assert_eq!(
            doc.selections(),
            &[Selection::caret(3), Selection::caret(12)]
        );
        doc.move_selections(SelectionMotion::Left, true, Indentation::default())
            .unwrap();
        assert_eq!(doc.selected_text(), "a\u{301}\na\u{301}");
        doc.move_selections(SelectionMotion::Right, false, Indentation::default())
            .unwrap();
        assert_eq!(
            doc.selections(),
            &[Selection::caret(3), Selection::caret(12)]
        );
        assert!(!doc.can_undo());
        assert!(!doc.is_dirty());
        doc.move_selections(SelectionMotion::Right, false, Indentation::default())
            .unwrap();
        doc.move_selections(SelectionMotion::Right, false, Indentation::default())
            .unwrap();
        assert_eq!(
            doc.selections(),
            &[Selection::caret(9), Selection::caret(16)]
        );
    }
    #[test]
    fn vertical_motion_retains_tab_columns_across_short_rows_and_resets_after_horizontal_motion() {
        let mut doc = Document::new("\tx\nq\n\tx\nq\n\tx");
        doc.set_selections(vec![Selection::caret(2), Selection::caret(7)])
            .unwrap();
        doc.move_selections(SelectionMotion::Down, false, Indentation::default())
            .unwrap();
        assert_eq!(
            doc.selections(),
            &[Selection::caret(4), Selection::caret(9)]
        );
        doc.move_selections(SelectionMotion::Down, false, Indentation::default())
            .unwrap();
        assert_eq!(
            doc.selections(),
            &[Selection::caret(7), Selection::caret(12)]
        );
        doc.move_selections(SelectionMotion::Left, false, Indentation::default())
            .unwrap();
        doc.move_selections(SelectionMotion::Up, false, Indentation::default())
            .unwrap();
        doc.move_selections(SelectionMotion::Up, false, Indentation::default())
            .unwrap();
        assert_eq!(
            doc.selections(),
            &[Selection::caret(1), Selection::caret(6)]
        );
    }
    #[test]
    fn word_and_boundary_motion_preserve_direction_and_merge_colliding_cursors() {
        let mut doc = Document::new("文_word  +  next\r\nlast");
        doc.set_selections(vec![Selection::caret(10), Selection::caret(17)])
            .unwrap();
        doc.move_selections(SelectionMotion::WordLeft, true, Indentation::default())
            .unwrap();
        assert_eq!(doc.selected_text(), "文_word  \nnext");
        doc.move_selections(
            SelectionMotion::DocumentStart,
            false,
            Indentation::default(),
        )
        .unwrap();
        assert_eq!(doc.selections(), &[Selection::caret(0)]);
        doc.move_selections(SelectionMotion::WordRight, false, Indentation::default())
            .unwrap();
        assert_eq!(doc.selections(), &[Selection::caret(10)]);
        doc.move_selections(SelectionMotion::LineEnd, true, Indentation::default())
            .unwrap();
        assert_eq!(doc.selections()[0].head, 17);
    }
    #[test]
    fn folded_motion_skips_hidden_source_and_composition_failure_is_atomic() {
        let mut doc = Document::new("head\nhidden\nend\ntail");
        doc.fold_state_mut().set_ranges(
            vec![super::super::FoldRange {
                start_line: 0,
                end_line: 2,
            }],
            4,
        );
        doc.fold_command(super::super::FoldCommand::CollapseAll);
        doc.set_selections(vec![Selection::caret(4)]).unwrap();
        doc.move_selections(SelectionMotion::Right, false, Indentation::default())
            .unwrap();
        assert_eq!(doc.selections(), &[Selection::caret(16)]);
        assert!(doc.fold_state().collapsed_at(0).is_some());
        doc.move_selections(SelectionMotion::Up, false, Indentation::default())
            .unwrap();
        assert_eq!(doc.selections(), &[Selection::caret(0)]);
        doc.begin_composition(None);
        let before = doc.clone();
        assert_eq!(
            doc.move_selections(SelectionMotion::Down, false, Indentation::default()),
            Err(SelectionError::Edit(EditError::CompositionActive))
        );
        assert_eq!(doc, before);
    }
}
