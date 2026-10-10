//! Ordered cursor requests while a view prepares fresh measurements.
use super::{
    Document, EditError, FoldProjection, Indentation, Selection, SelectionError, SelectionMotion,
    VisualLayout,
};
use std::collections::VecDeque;

const MAX_PENDING_MOTIONS: usize = 1024;
const MAX_WAIT_FRAMES: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MotionRequest {
    pub motion: SelectionMotion,
    pub extend: bool,
    pub wrapped: bool,
}
impl MotionRequest {
    pub fn needs_layout(self) -> bool {
        self.wrapped && matches!(self.motion, SelectionMotion::Up | SelectionMotion::Down)
    }
}

#[derive(Clone, Debug)]
pub struct MotionQueue {
    identity: std::sync::Arc<()>,
    projection: FoldProjection,
    selections: Vec<Selection>,
    revision: u64,
    requests: VecDeque<MotionRequest>,
    waits: usize,
    preparation: Option<(u64, usize)>,
}
impl MotionQueue {
    pub fn new(document: &Document) -> Result<Self, SelectionError> {
        if document.is_composing() {
            return Err(EditError::CompositionActive.into());
        }
        if document.admission().limit().is_some() {
            return Err(SelectionError::TooLarge);
        }
        Ok(Self {
            identity: document.identity.clone(),
            projection: document.projection(),
            selections: document.selections().to_vec(),
            revision: document.revision(),
            requests: VecDeque::new(),
            waits: 0,
            preparation: None,
        })
    }
    pub fn matches(&self, document: &Document) -> bool {
        !document.is_composing()
            && self.revision == document.revision()
            && std::sync::Arc::ptr_eq(&self.identity, &document.identity)
            && self.selections == document.selections()
            && self.projection == document.projection()
    }
    pub fn push(&mut self, request: MotionRequest) -> Result<(), SelectionError> {
        if self.requests.len() == MAX_PENDING_MOTIONS {
            return Err(SelectionError::TooManyMotions);
        }
        self.requests.push_back(request);
        Ok(())
    }
    pub fn next_request(&self) -> Option<MotionRequest> {
        self.requests.front().copied()
    }
    pub fn wait_for_layout(&mut self) -> Result<(), SelectionError> {
        self.waits += 1;
        if self.waits > MAX_WAIT_FRAMES {
            return Err(SelectionError::LayoutUnavailable);
        }
        Ok(())
    }
    /// Continued measured progress permits longer preparation, while a stalled
    /// adapter retains the same bounded wait as an unavailable paint frame.
    pub fn wait_for_preparation(
        &mut self,
        progress: Option<(u64, usize)>,
    ) -> Result<(), SelectionError> {
        if let Some(progress) =
            progress.filter(|(_, completed)| *completed <= super::MAX_EDITOR_LINES)
            && self
                .preparation
                .is_none_or(|previous| progress.0 != previous.0 || progress.1 > previous.1)
        {
            self.waits = 0;
            self.preparation = Some(progress);
        }
        self.wait_for_layout()
    }
    pub fn apply_next(
        &mut self,
        document: &mut Document,
        indentation: Indentation,
        layout: Option<&VisualLayout>,
    ) -> Result<Option<Vec<Selection>>, SelectionError> {
        if !self.matches(document) {
            return Err(EditError::StaleContext.into());
        }
        let Some(request) = self.next_request() else {
            return Ok(None);
        };
        if request.needs_layout() {
            let Some(layout) = layout else {
                return Ok(None);
            };
            document.move_selections_with_layout(
                request.motion,
                request.extend,
                indentation,
                layout,
            )?;
        } else {
            document.move_selections(request.motion, request.extend, indentation)?;
        }
        self.requests.pop_front();
        self.selections = document.selections().to_vec();
        self.waits = 0;
        Ok(Some(self.selections.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_motion_rejects_replacements_even_with_identical_text_revision_and_selection() {
        let original = Document::new("文😀\r\ntail");
        let mut queue = MotionQueue::new(&original).unwrap();
        queue
            .push(MotionRequest {
                motion: SelectionMotion::Right,
                extend: false,
                wrapped: false,
            })
            .unwrap();
        let mut replacement = Document::new(original.text());
        assert_eq!(original, replacement);
        assert!(!queue.matches(&replacement));
        let before = replacement.clone();
        assert_eq!(
            queue.apply_next(&mut replacement, Indentation::default(), None),
            Err(EditError::StaleContext.into())
        );
        assert_eq!(replacement, before);
        assert!(queue.next_request().is_some());
        let mut same_version = original.clone();
        assert!(queue.matches(&same_version));
        assert!(
            queue
                .apply_next(&mut same_version, Indentation::default(), None)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn queued_motion_rejects_divergent_clones_with_the_same_revision() {
        let original = Document::new("abc");
        let mut left = original.clone();
        let mut right = original;
        for (document, text) in [(&mut left, "x"), (&mut right, "y")] {
            document
                .apply(
                    vec![super::super::Edit {
                        range: 0..1,
                        text: text.into(),
                    }],
                    vec![Selection::caret(0)],
                    None,
                )
                .unwrap();
        }
        assert_eq!(left.revision(), right.revision());
        assert_eq!(left.selections(), right.selections());
        assert!(!std::sync::Arc::ptr_eq(&left.identity, &right.identity));
        let mut queue = MotionQueue::new(&left).unwrap();
        queue
            .push(MotionRequest {
                motion: SelectionMotion::Right,
                extend: false,
                wrapped: false,
            })
            .unwrap();
        assert!(!queue.matches(&right));
        assert_eq!(
            queue.apply_next(&mut right, Indentation::default(), None),
            Err(EditError::StaleContext.into())
        );
        assert_eq!(right.text(), "ybc");
        assert_eq!(right.selections(), [Selection::caret(0)]);
    }

    #[test]
    fn measured_progress_extends_wait_but_a_stalled_preparation_still_fails() {
        let document = Document::new("one\ntwo");
        let mut queue = MotionQueue::new(&document).unwrap();
        for completed in 0..100 {
            queue.wait_for_preparation(Some((1, completed))).unwrap();
        }
        for _ in 0..MAX_WAIT_FRAMES - 1 {
            queue.wait_for_preparation(Some((1, 98))).unwrap();
        }
        assert_eq!(
            queue.wait_for_preparation(Some((1, 99))),
            Err(SelectionError::LayoutUnavailable)
        );
    }
    use crate::editor::VisualCaret;

    #[test]
    fn queue_limit_preserves_accepted_requests_and_document() {
        let doc = Document::new("abc");
        let before = doc.clone();
        let mut queue = MotionQueue::new(&doc).unwrap();
        let request = MotionRequest {
            motion: SelectionMotion::Right,
            extend: false,
            wrapped: false,
        };
        for _ in 0..MAX_PENDING_MOTIONS {
            queue.push(request).unwrap();
        }
        assert_eq!(queue.push(request), Err(SelectionError::TooManyMotions));
        assert_eq!(queue.requests.len(), MAX_PENDING_MOTIONS);
        assert_eq!(queue.next_request(), Some(request));
        assert_eq!(doc, before);
    }

    #[test]
    fn delayed_requests_preserve_order_and_shift_without_changing_text_or_history() {
        let mut doc = Document::new("abc\nxy\nabc");
        doc.set_selections(vec![Selection::caret(1), Selection::caret(8)])
            .unwrap();
        let mut queue = MotionQueue::new(&doc).unwrap();
        for motion in [
            SelectionMotion::Down,
            SelectionMotion::Left,
            SelectionMotion::Up,
        ] {
            queue
                .push(MotionRequest {
                    motion,
                    extend: true,
                    wrapped: true,
                })
                .unwrap();
        }
        let before = doc.clone();
        assert!(
            queue
                .apply_next(&mut doc, Indentation::default(), None)
                .unwrap()
                .is_none()
        );
        assert_eq!(doc, before);
        let carets = [
            (0, 0, 0),
            (1, 10, 0),
            (2, 20, 0),
            (3, 30, 0),
            (4, 0, 1),
            (5, 10, 1),
            (6, 20, 1),
            (7, 0, 2),
            (8, 10, 2),
            (9, 20, 2),
            (10, 30, 2),
        ]
        .into_iter()
        .map(|(offset, column, row)| VisualCaret {
            offset,
            column,
            row,
        })
        .collect();
        let layout =
            VisualLayout::new(doc.text(), doc.projection(), "paint".into(), 3, carets).unwrap();
        assert!(
            queue
                .apply_next(&mut doc, Indentation::default(), Some(&layout))
                .unwrap()
                .is_some()
        );
        assert!(
            queue
                .apply_next(&mut doc, Indentation::default(), None)
                .unwrap()
                .is_some()
        );
        assert!(
            queue
                .apply_next(&mut doc, Indentation::default(), Some(&layout))
                .unwrap()
                .is_some()
        );
        assert!(queue.next_request().is_none());
        assert_eq!(
            doc.selections(),
            &[
                Selection { anchor: 1, head: 0 },
                Selection { anchor: 8, head: 4 }
            ]
        );
        assert!(!doc.is_dirty());
        assert!(!doc.undo());
    }

    #[test]
    fn edits_undo_folds_and_composition_cannot_revive_old_requests() {
        use crate::editor::{Edit, FoldCommand, FoldRange};
        for change in 0..3 {
            let mut doc = Document::new("abc\nxy\nlast");
            let mut queue = MotionQueue::new(&doc).unwrap();
            queue
                .push(MotionRequest {
                    motion: SelectionMotion::Down,
                    extend: false,
                    wrapped: false,
                })
                .unwrap();
            match change {
                0 => {
                    doc.apply(
                        vec![Edit {
                            range: 0..0,
                            text: "X".into(),
                        }],
                        vec![Selection::caret(1)],
                        None,
                    )
                    .unwrap();
                    doc.undo();
                }
                1 => {
                    doc.fold_state_mut().set_ranges(
                        vec![FoldRange {
                            start_line: 0,
                            end_line: 1,
                        }],
                        3,
                    );
                    doc.fold_command(FoldCommand::Toggle(0));
                }
                _ => {
                    doc.begin_composition(None);
                }
            }
            let before = doc.clone();
            assert!(
                queue
                    .apply_next(&mut doc, Indentation::default(), None)
                    .is_err()
            );
            assert_eq!(doc, before);
            assert!(queue.next_request().is_some());
        }
    }

    #[test]
    fn stale_carets_and_unavailable_layout_reject_without_applying_requests() {
        let mut doc = Document::new("abc\nxy");
        let mut queue = MotionQueue::new(&doc).unwrap();
        queue
            .push(MotionRequest {
                motion: SelectionMotion::Down,
                extend: false,
                wrapped: false,
            })
            .unwrap();
        doc.set_selections(vec![Selection::caret(1)]).unwrap();
        let before = doc.clone();
        assert!(
            queue
                .apply_next(&mut doc, Indentation::default(), None)
                .is_err()
        );
        assert_eq!(doc, before);
        assert!(queue.next_request().is_some());
        let mut waits = MotionQueue::new(&doc).unwrap();
        for _ in 0..MAX_WAIT_FRAMES {
            waits.wait_for_layout().unwrap();
        }
        assert_eq!(
            waits.wait_for_layout(),
            Err(SelectionError::LayoutUnavailable)
        );
    }
}
