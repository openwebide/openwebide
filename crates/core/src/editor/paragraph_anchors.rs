//! Cooperative validation and exact source anchors before paragraph probing.
use super::{MAX_MEASURE_BYTES, MAX_ROW_GEOMETRY_ANCHORS, MAX_VISUAL_CARETS, VisualLineIndex};
use std::sync::Arc;

/// Opaque completed setup; partial work cannot authorize a measurement plan.
pub struct PreparedParagraphAnchors<'a> {
    pub(super) body: &'a str,
    pub(super) index: VisualLineIndex,
    pub(super) runs: Arc<[usize]>,
    pub(super) glyphs: Vec<usize>,
    pub(super) wrapped: bool,
}

pub struct ParagraphAnchorPreparation<'a> {
    prepared: PreparedParagraphAnchors<'a>,
    next: usize,
    failed: bool,
}
impl<'a> ParagraphAnchorPreparation<'a> {
    pub fn unwrapped(body: &'a str, index: VisualLineIndex, runs: Arc<[usize]>) -> Option<Self> {
        Self::new(body, index, runs, false)
    }
    pub fn wrapped(body: &'a str, index: VisualLineIndex, runs: Arc<[usize]>) -> Option<Self> {
        Self::new(body, index, runs, true)
    }
    fn new(
        body: &'a str,
        index: VisualLineIndex,
        runs: Arc<[usize]>,
        wrapped: bool,
    ) -> Option<Self> {
        if body.len() <= MAX_MEASURE_BYTES
            || !index.source_paint_eligible()
            || runs.last().copied() != Some(body.len())
            || (wrapped && runs.len() > MAX_VISUAL_CARETS)
            || index
                .anchor_glyphs()
                .take(MAX_ROW_GEOMETRY_ANCHORS + 1)
                .count()
                > MAX_ROW_GEOMETRY_ANCHORS
        {
            return None;
        }
        Some(Self {
            prepared: PreparedParagraphAnchors {
                body,
                index,
                runs,
                glyphs: vec![0],
                wrapped,
            },
            next: 0,
            failed: false,
        })
    }
    /// Budget counts original paint boundaries. Coordinate lookup uses retained
    /// checkpoints; an indivisible Unicode cluster can exceed an ordinary chunk.
    pub fn advance(&mut self, budget: usize) -> bool {
        if self.failed || self.next == self.prepared.runs.len() {
            return true;
        }
        let end = self
            .next
            .saturating_add(budget)
            .min(self.prepared.runs.len());
        let prepared = &mut self.prepared;
        for at in self.next..end {
            let byte = prepared.runs[at];
            if (at > 0 && prepared.runs[at - 1] >= byte)
                || (prepared.wrapped && !prepared.body.is_char_boundary(byte))
            {
                self.failed = true;
                return true;
            }
        }
        if prepared.runs.len() <= MAX_ROW_GEOMETRY_ANCHORS - 2 {
            let query_end = if end == prepared.runs.len() {
                end - 1
            } else {
                end
            };
            if let Some(glyphs) = prepared
                .index
                .boundary_glyphs(prepared.body, &prepared.runs[self.next..query_end])
            {
                prepared.glyphs.extend(glyphs);
            } else {
                self.failed = true;
                return true;
            }
        }
        self.next = end;
        self.next == prepared.runs.len()
    }
    pub fn finish(mut self) -> Option<PreparedParagraphAnchors<'a>> {
        if self.failed || self.next != self.prepared.runs.len() {
            return None;
        }
        let prepared = &mut self.prepared;
        if prepared.runs.len() <= MAX_ROW_GEOMETRY_ANCHORS - 2 {
            prepared.glyphs.push(prepared.index.len().checked_sub(2)?);
        } else {
            prepared.glyphs = prepared
                .index
                .anchor_glyphs_in(0..prepared.index.len() - 1)
                .collect();
        }
        prepared.glyphs.dedup();
        Some(self.prepared)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_segmentation::UnicodeSegmentation;

    #[test]
    fn cooperative_anchors_match_complete_unicode_coordinates_at_every_budget() {
        let body = "word 文😀e\u{301} ".repeat(5000);
        let index = VisualLineIndex::new(&body).unwrap();
        let boundaries = body
            .grapheme_indices(true)
            .map(|(byte, _)| byte)
            .chain(std::iter::once(body.len()))
            .collect::<Vec<_>>();
        for runs in [
            index.text_run_boundaries().collect::<Vec<_>>(),
            // Include a character boundary inside a combining cluster. The
            // original policy omits it rather than inventing a glyph anchor.
            vec![0, 12, 13, 16, 1024, body.len()],
            boundaries.iter().copied().skip(1).collect(),
        ] {
            let mut expected = if runs.len() <= MAX_ROW_GEOMETRY_ANCHORS - 2 {
                let mut expected = vec![0];
                expected.extend(
                    runs.iter()
                        .filter(|byte| **byte < body.len())
                        .filter_map(|byte| boundaries.binary_search(byte).ok()),
                );
                expected.push(boundaries.len() - 2);
                expected
            } else {
                index.anchor_glyphs().collect()
            };
            expected.dedup();
            for budget in [1, 2, 64, usize::MAX] {
                let mut preparation = ParagraphAnchorPreparation::unwrapped(
                    &body,
                    index.clone(),
                    runs.clone().into(),
                )
                .unwrap();
                assert!(!preparation.advance(0));
                while !preparation.advance(budget) {}
                assert_eq!(preparation.finish().unwrap().glyphs, expected);
            }
        }
    }

    #[test]
    fn incomplete_invalid_and_wrong_layout_setup_cannot_authorize_probes() {
        let body = "words 文😀 ".repeat(5000);
        let index = VisualLineIndex::new(&body).unwrap();
        let runs: Arc<[usize]> = index.text_run_boundaries().collect();
        assert!(
            ParagraphAnchorPreparation::wrapped(&body, index.clone(), runs.clone())
                .unwrap()
                .finish()
                .is_none()
        );
        for bad in [
            vec![512, 512, body.len()],
            vec![1024, 512, body.len()],
            vec![1, body.len() - 1],
            vec![8, body.len()],
        ] {
            let mut preparation =
                ParagraphAnchorPreparation::wrapped(&body, index.clone(), bad.into());
            if let Some(preparation) = preparation.as_mut() {
                while !preparation.advance(1) {}
            }
            assert!(
                preparation
                    .and_then(ParagraphAnchorPreparation::finish)
                    .is_none()
            );
        }
        let mut preparation =
            ParagraphAnchorPreparation::wrapped(&body, index.clone(), runs.clone()).unwrap();
        while !preparation.advance(64) {}
        assert!(
            super::super::ParagraphMeasurementPlan::with_prepared_anchors(
                preparation.finish().unwrap()
            )
            .is_none()
        );
        let mut preparation = ParagraphAnchorPreparation::unwrapped(&body, index, runs).unwrap();
        while !preparation.advance(64) {}
        assert!(
            super::super::WrappedParagraphPreparation::with_prepared_anchors(
                preparation.finish().unwrap()
            )
            .is_none()
        );
    }
}
