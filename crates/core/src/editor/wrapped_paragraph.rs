//! Exact continuation policy for bounded, source-monotonic wrapped paragraphs.
use super::paragraph::{MAX_RETAINED_RECTANGLES, ParagraphMeasurement};
use super::{
    GlyphRectangle, MAX_PARAGRAPH_PROBE_BYTES, ParagraphMeasurements, ParagraphProbe,
    ParagraphReplay, VisualLineIndex, WrappedGeometry,
};
use std::{ops::Range, sync::Arc};
const TOLERANCE: f64 = 0.25;
const CONTEXT_BYTES: usize = 1024;

/// Continuations start after complete words and retain original paint-run clipping.
/// The adapter measures styled text with the exact preceding horizontal gap;
/// dense overlap must reconnect before any completed geometry can publish.
/// Unsupported seams (including an oversized unbroken word) require full layout.
pub struct WrappedParagraphPreparation<'a> {
    body: &'a str,
    index: VisualLineIndex,
    runs: Arc<[usize]>,
    probe: ParagraphProbe,
    next: Option<(usize, usize)>,
    expected: Vec<GlyphRectangle>,
    anchors: Vec<GlyphRectangle>,
    anchor_glyphs: Vec<usize>,
    records: Vec<ParagraphMeasurement>,
    retained_rectangles: usize,
    width: Option<f64>,
    scroll_width: f64,
    height: Option<f64>,
    complete: bool,
    #[cfg(any(test, feature = "test-support"))]
    reconciled_prefixes: usize,
}
impl<'a> WrappedParagraphPreparation<'a> {
    pub fn new(body: &'a str, index: VisualLineIndex, runs: Arc<[usize]>) -> Option<Self> {
        let mut preparation = super::ParagraphAnchorPreparation::wrapped(body, index, runs)?;
        preparation.advance(usize::MAX);
        Self::with_prepared_anchors(preparation.finish()?)
    }
    /// Accept only the complete, validated source-owned cooperative setup.
    pub fn with_prepared_anchors(prepared: super::PreparedParagraphAnchors<'a>) -> Option<Self> {
        if !prepared.wrapped {
            return None;
        }
        let super::PreparedParagraphAnchors {
            body,
            index,
            runs,
            glyphs: anchor_glyphs,
            ..
        } = prepared;
        let mut result = Self {
            body,
            index,
            runs,
            probe: ParagraphProbe {
                bytes: 0..0,
                native_start: 0,
                glyph_start: 0,
                origin: 0.0,
            },
            next: None,
            expected: Vec::new(),
            anchors: Vec::new(),
            anchor_glyphs,
            records: Vec::new(),
            retained_rectangles: 0,
            width: None,
            scroll_width: 0.0,
            height: None,
            complete: false,
            #[cfg(any(test, feature = "test-support"))]
            reconciled_prefixes: 0,
        };
        result.probe = result.probe_at(0, 0.0)?;
        result.next = result.continuation(&result.probe)?;
        Some(result)
    }
    fn boundary(&self, range: Range<usize>) -> Option<usize> {
        self.body
            .as_bytes()
            .get(range.clone())?
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, byte)| **byte == b' ')
            .map(|(offset, _)| range.start + offset + 1)
            .find(|&byte| {
                self.index
                    .index_at_byte(self.body, byte)
                    .and_then(|glyph| self.index.at(self.body, glyph))
                    .is_some_and(|(at, _)| at == byte)
            })
    }
    pub fn paint_runs(&self) -> Arc<[usize]> {
        self.runs.clone()
    }
    fn probe_at(&self, start: usize, origin: f64) -> Option<ParagraphProbe> {
        let limit = (start + MAX_PARAGRAPH_PROBE_BYTES).min(self.body.len());
        let end = if limit == self.body.len() {
            limit
        } else {
            self.boundary(start..limit)?
        };
        let glyph_start = self.index.index_at_byte(self.body, start)?;
        let (at, native_start) = self.index.at(self.body, glyph_start)?;
        (at == start && end > start).then_some(ParagraphProbe {
            bytes: start..end,
            native_start,
            glyph_start,
            origin,
        })
    }
    fn continuation(&self, probe: &ParagraphProbe) -> Option<Option<(usize, usize)>> {
        if probe.bytes.end == self.body.len() {
            return Some(None);
        }
        let start = self.boundary(
            probe.bytes.start + CONTEXT_BYTES..probe.bytes.end.checked_sub(CONTEXT_BYTES)?,
        )?;
        if probe.bytes.end - start > 4 * CONTEXT_BYTES {
            return None;
        }
        let end_glyph = self
            .index
            .index_at_byte(self.body, probe.bytes.end.checked_sub(CONTEXT_BYTES / 2)?)?;
        let proof_end = self.index.at(self.body, end_glyph)?.0;
        if proof_end <= start {
            return None;
        }
        Some(Some((start, proof_end)))
    }
    pub fn probe(&self) -> Option<&ParagraphProbe> {
        (!self.complete).then_some(&self.probe)
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn expected_overlap(&self) -> &[GlyphRectangle] {
        &self.expected
    }
    pub fn targets(&self) -> Option<Vec<usize>> {
        let end = self.index.index_at_byte(self.body, self.probe.bytes.end)?;
        let first = self
            .anchor_glyphs
            .partition_point(|glyph| *glyph < self.probe.glyph_start);
        let last = self.anchor_glyphs.partition_point(|glyph| *glyph < end);
        let mut targets = self.anchor_glyphs[first..last].to_vec();
        targets.extend(self.expected.iter().map(|rect| rect.glyph));
        if let Some((start, end)) = self.next {
            targets.extend(
                self.index.index_at_byte(self.body, start)?
                    ..self.index.index_at_byte(self.body, end)?,
            );
        }
        targets.sort_unstable();
        targets.dedup();
        Some(targets)
    }
    /// Raw rectangles are relative to the styled probe. Vertical translation is
    /// measured from the same first overlap glyph, never from estimated line heights.
    pub fn record(
        &mut self,
        width: f64,
        height: f64,
        scroll_width: f64,
        rectangles: &[GlyphRectangle],
    ) -> bool {
        if self.complete
            || !width.is_finite()
            || width <= 0.0
            || width > 1_000_000_000.0
            || !height.is_finite()
            || height <= 0.0
            || height > 1_000_000.0
            || !scroll_width.is_finite()
            || scroll_width < width
            || scroll_width > 1_000_000_000.0
            || self
                .width
                .is_some_and(|old| (old - width).abs() > TOLERANCE)
        {
            return false;
        }
        let Some(targets) = self.targets() else {
            return false;
        };
        if targets.len() != rectangles.len()
            || targets
                .iter()
                .zip(rectangles)
                .any(|(glyph, rect)| *glyph != rect.glyph || !rect.valid())
        {
            return false;
        }
        let lookup = |glyph| {
            rectangles
                .binary_search_by_key(&glyph, |rect| rect.glyph)
                .ok()
                .map(|at| rectangles[at])
        };
        let shift = if let Some(first) = self.expected.first() {
            let Some(current) = lookup(first.glyph) else {
                return false;
            };
            first.top - current.top
        } else {
            0.0
        };
        if self.expected.iter().any(|old| {
            lookup(old.glyph).is_none_or(|current| {
                (old.left - current.left).abs() > TOLERANCE
                    || (old.top - current.top - shift).abs() > TOLERANCE
                    || (old.width - current.width).abs() > TOLERANCE
                    || (old.height - current.height).abs() > TOLERANCE
            })
        }) {
            return false;
        }
        let global = rectangles
            .iter()
            .map(|rect| GlyphRectangle {
                top: rect.top + shift,
                ..*rect
            })
            .collect::<Vec<_>>();
        if global.iter().any(|rect| !rect.valid()) {
            return false;
        }
        let lookup = |glyph| {
            global
                .binary_search_by_key(&glyph, |rect| rect.glyph)
                .ok()
                .map(|at| global[at])
        };
        let next = if let Some((start, proof_end)) = self.next {
            let Some(glyph) = self.index.index_at_byte(self.body, start) else {
                return false;
            };
            let Some(origin) = lookup(glyph).map(|rect| rect.left) else {
                return false;
            };
            let Some(probe) = self.probe_at(start, origin) else {
                return false;
            };
            let Some(continuation) = self.continuation(&probe) else {
                return false;
            };
            let Some(end) = self.index.index_at_byte(self.body, proof_end) else {
                return false;
            };
            Some((
                probe,
                continuation,
                global
                    .iter()
                    .copied()
                    .filter(|rect| (glyph..end).contains(&rect.glyph))
                    .collect::<Vec<_>>(),
                glyph,
            ))
        } else {
            None
        };
        let commit_end = next
            .as_ref()
            .map_or(self.index.len() - 1, |(_, _, _, glyph)| *glyph);
        let first = self
            .anchor_glyphs
            .partition_point(|glyph| *glyph < self.probe.glyph_start);
        let last = self
            .anchor_glyphs
            .partition_point(|glyph| *glyph < commit_end);
        for &glyph in &self.anchor_glyphs[first..last] {
            let Some(rect) = lookup(glyph) else {
                return false;
            };
            if self.anchors.last().is_none_or(|last| last.glyph != glyph) {
                self.anchors.push(rect);
            }
        }
        self.retained_rectangles = self.retained_rectangles.saturating_add(rectangles.len());
        if self.retained_rectangles <= MAX_RETAINED_RECTANGLES {
            self.records.push(ParagraphMeasurement {
                bytes: self.probe.bytes.clone(),
                origin: self.probe.origin,
                width,
                height,
                scroll_width: Some(scroll_width),
                rectangles: rectangles.into(),
            });
        } else {
            self.records.clear();
        }
        self.width = Some(width);
        self.scroll_width = self.scroll_width.max(scroll_width);
        if let Some((probe, continuation, expected, _)) = next {
            self.probe = probe;
            self.next = continuation;
            self.expected = expected;
        } else {
            self.height = Some(height + shift);
            self.complete = true;
        }
        true
    }
    /// A complete unchanged prefix needs no browser work. Never replay the
    /// terminal probe: it must establish the new complete extent freshly.
    pub fn reuse_prefix(
        &mut self,
        old_body: &str,
        old: &ParagraphMeasurements,
        style_end: usize,
    ) -> usize {
        self.reuse_prefix_batch(old_body, old, style_end, usize::MAX)
    }
    pub fn reuse_prefix_batch(
        &mut self,
        old_body: &str,
        old: &ParagraphMeasurements,
        style_end: usize,
        max_records: usize,
    ) -> usize {
        if !old.wrapped {
            return 0;
        }
        let mut reused = 0;
        while reused < max_records {
            let Some(probe) = self.probe().cloned() else {
                break;
            };
            if probe.bytes.end >= self.body.len() || probe.bytes.end > style_end {
                break;
            }
            let Some(record) = old
                .records
                .binary_search_by_key(&probe.bytes.start, |record| record.bytes.start)
                .ok()
                .map(|at| &old.records[at])
                .filter(|record| record.bytes == probe.bytes)
            else {
                break;
            };
            if !self.replay_record(old_body, old, record, false) {
                break;
            }
            reused += 1;
        }
        reused
    }
    /// Reconnect only unchanged source/style probes with the exact incoming
    /// horizontal origin or a densely proved first-row translation. Vertical
    /// offsets come from measured overlap, never from estimated line advances.
    /// Missing shifted anchors fall back to fresh bounded measurement.
    pub fn reuse_suffix_batch(
        &mut self,
        old_body: &str,
        old: &ParagraphMeasurements,
        style_start: usize,
        limit: usize,
    ) -> usize {
        if !old.wrapped || self.expected.is_empty() {
            return 0;
        }
        let mut reused = 0;
        while reused < limit {
            let Some(probe) = self.probe().cloned() else {
                break;
            };
            if probe.bytes.start < style_start || probe.bytes.end == self.body.len() {
                break;
            }
            let Some(start) = old_body
                .len()
                .checked_sub(self.body.len().saturating_sub(probe.bytes.start))
            else {
                break;
            };
            let Some(record) = old
                .records
                .binary_search_by_key(&start, |record| record.bytes.start)
                .ok()
                .map(|at| &old.records[at])
            else {
                break;
            };
            if !self.replay_record(old_body, old, record, true) {
                break;
            }
            reused += 1;
        }
        reused
    }
    #[cfg(feature = "test-support")]
    pub fn reconciled_prefixes(&self) -> usize {
        self.reconciled_prefixes
    }
    /// Replace only the already measured incoming prefix. Reuse the remaining
    /// layout after a complete word starts the same visual row in both layouts
    /// and every later overlap glyph proves the same vertical translation.
    fn reconcile_prefix(
        &self,
        record: &ParagraphMeasurement,
        rectangles: &mut [GlyphRectangle],
    ) -> Option<f64> {
        let first = *rectangles.first()?;
        let expected_first = *self.expected.first()?;
        if first.glyph != expected_first.glyph
            || (first.left - record.origin).abs() > TOLERANCE
            || (expected_first.left - self.probe.origin).abs() > TOLERANCE
        {
            return None;
        }
        let shift = expected_first.top - first.top;
        let lookup = |glyph| {
            rectangles
                .binary_search_by_key(&glyph, |rect| rect.glyph)
                .ok()
                .map(|at| rectangles[at])
        };
        let mut reconnect = None;
        for (at, expected) in self.expected.iter().enumerate().skip(1) {
            let previous = self.expected[at - 1];
            let old = lookup(expected.glyph)?;
            let old_previous = lookup(previous.glyph)?;
            if expected.glyph != previous.glyph + 1
                || expected.left != 0.0
                || old.left != 0.0
                || previous.top >= expected.top
                || old_previous.top >= old.top
                || previous.left + previous.width <= 0.0
                || old_previous.left + old_previous.width <= 0.0
                || self.expected.last()?.top <= expected.top
            {
                continue;
            }
            let (byte, _) = self.index.at(self.body, expected.glyph)?;
            if self.body.as_bytes().get(byte.checked_sub(1)?) != Some(&b' ') {
                continue;
            }
            let delta = expected.top - shift - old.top;
            if self.expected[at..].iter().all(|expected| {
                lookup(expected.glyph).is_some_and(|old| {
                    (old.left - expected.left).abs() <= TOLERANCE
                        && (old.top + delta + shift - expected.top).abs() <= TOLERANCE
                        && (old.width - expected.width).abs() <= TOLERANCE
                        && (old.height - expected.height).abs() <= TOLERANCE
                })
            }) {
                reconnect = Some((expected.glyph, delta));
                break;
            }
        }
        let (glyph, delta) = reconnect?;
        // Every requested prefix target must have an exact measured replacement.
        // Its extents were already measured by the preceding fresh probe; glyph
        // ranges may hang beyond the row box without increasing scroll width.
        for rect in rectangles.iter().filter(|rect| rect.glyph < glyph) {
            let at = self
                .expected
                .binary_search_by_key(&rect.glyph, |rect| rect.glyph)
                .ok()?;
            let expected = self.expected[at];
            if !expected.valid() {
                return None;
            }
        }
        let height = record.height + delta;
        if !height.is_finite() || height <= 0.0 {
            return None;
        }
        for rect in rectangles {
            if rect.glyph < glyph {
                let at = self
                    .expected
                    .binary_search_by_key(&rect.glyph, |rect| rect.glyph)
                    .ok()?;
                *rect = GlyphRectangle {
                    top: self.expected[at].top - shift,
                    ..self.expected[at]
                };
            } else {
                rect.top += delta;
            }
        }
        Some(height)
    }
    fn replay_record(
        &mut self,
        old_body: &str,
        old: &ParagraphMeasurements,
        record: &ParagraphMeasurement,
        shifted: bool,
    ) -> bool {
        let probe = &self.probe;
        let old_byte = |byte: usize| {
            if shifted {
                old_body
                    .len()
                    .checked_sub(self.body.len().checked_sub(byte)?)
            } else {
                Some(byte)
            }
        };
        if old_byte(probe.bytes.end) != Some(record.bytes.end) {
            return false;
        }
        if old_body.get(record.bytes.clone()) != self.body.get(probe.bytes.clone()) {
            return false;
        }
        if self
            .width
            .is_some_and(|width| width.to_bits() != record.width.to_bits())
        {
            return false;
        }
        let run_slice = |runs: &[usize], bytes: &Range<usize>| {
            runs.partition_point(|byte| *byte <= bytes.start)
                ..runs.partition_point(|byte| *byte <= bytes.end)
        };
        let old_runs = &old.runs[run_slice(&old.runs, &record.bytes)];
        let new_runs = &self.runs[run_slice(&self.runs, &probe.bytes)];
        if old_runs.len() != new_runs.len()
            || old_runs
                .iter()
                .zip(new_runs)
                .any(|(old, new)| old_byte(*new) != Some(*old))
        {
            return false;
        }
        let Some(targets) = self.targets() else {
            return false;
        };
        let Some(positions) = self.index.positions(self.body, &targets) else {
            return false;
        };
        let Some(old_start) = old.index.index_at_byte(old_body, record.bytes.start) else {
            return false;
        };
        let Some(old_targets) = targets
            .iter()
            .map(|glyph| old_start.checked_add(glyph.checked_sub(probe.glyph_start)?))
            .collect::<Option<Vec<_>>>()
        else {
            return false;
        };
        let Some(old_positions) = old.index.positions(old_body, &old_targets) else {
            return false;
        };
        let mut rectangles = Vec::with_capacity(targets.len());
        for ((glyph, old_glyph), ((byte, _), (old_position, _))) in targets
            .into_iter()
            .zip(old_targets)
            .zip(positions.into_iter().zip(old_positions))
        {
            if old_byte(byte) != Some(old_position) {
                return false;
            }
            let Ok(at) = record
                .rectangles
                .binary_search_by_key(&old_glyph, |rect| rect.glyph)
            else {
                return false;
            };
            rectangles.push(GlyphRectangle {
                glyph,
                ..record.rectangles[at]
            });
        }
        let mut height = record.height;
        #[cfg(any(test, feature = "test-support"))]
        let mut reconciled_prefix = false;
        if record.origin.to_bits() != probe.origin.to_bits() {
            // A changed gap must not reuse an overflow width that belonged to
            // the old first row. Nonoverflowing rows retain the exact row box;
            // overflow-sensitive probes require fresh browser dimensions.
            if record.scroll_width != Some(record.width.ceil()) || probe.origin > record.width {
                return false;
            }
            if let Some(reconciled) = self.reconcile_prefix(record, &mut rectangles) {
                height = reconciled;
                #[cfg(any(test, feature = "test-support"))]
                {
                    reconciled_prefix = true;
                }
            } else {
                let Some(first) = rectangles.first().copied() else {
                    return false;
                };
                let Some(expected) = self
                    .expected
                    .first()
                    .filter(|expected| expected.glyph == first.glyph)
                else {
                    return false;
                };
                // A changed incoming gap can translate only the first visual row.
                // Require dense proof past its line break; the ordinary overlap gate
                // must reconnect every glyph before any cached continuation is used.
                if !self.expected.iter().any(|expected| {
                    rectangles
                        .binary_search_by_key(&expected.glyph, |rect| rect.glyph)
                        .ok()
                        .is_some_and(|at| rectangles[at].top > first.top)
                }) {
                    return false;
                }
                let delta = expected.left - first.left;
                for rect in &mut rectangles {
                    if rect.top.to_bits() == first.top.to_bits() {
                        rect.left += delta;
                        if rect.left + rect.width > record.width {
                            return false;
                        }
                    }
                }
            }
        }
        // The same record gate used for fresh browser probes validates every
        // dense overlap, including existing subpixel layout quantization.
        let Some(scroll_width) = record.scroll_width else {
            return false;
        };
        let valid = self.record(record.width, height, scroll_width, &rectangles);
        #[cfg(any(test, feature = "test-support"))]
        if valid && reconciled_prefix {
            self.reconciled_prefixes += 1;
        }
        valid
    }
    /// Only committed current anchors can cover an origin viewport. The final
    /// anchor supplies context beyond the conservative lower height boundary.
    pub fn viewport_coverage(&self, line_height: f64) -> Option<super::WrappedCoverage> {
        if self.complete || !line_height.is_finite() || !(1.0..=4096.0).contains(&line_height) {
            return None;
        }
        let last = self.anchors.last()?;
        let covered_height = (last.top / line_height).floor() * line_height;
        let geometry = WrappedGeometry::new(
            last.glyph.checked_add(1)?,
            self.width?,
            covered_height,
            self.anchors.clone(),
        )?;
        super::WrappedCoverage::new(geometry, covered_height)
    }
    pub fn finish(self) -> Option<(f64, WrappedGeometry)> {
        self.finish_with_measurements()
            .map(|(width, geometry, _)| (width, geometry))
    }
    pub fn finish_with_measurements(self) -> Option<(f64, WrappedGeometry, ParagraphMeasurements)> {
        if !self.complete {
            return None;
        }
        Some((
            self.scroll_width,
            WrappedGeometry::new(
                self.index.len() - 1,
                self.width?,
                self.height?,
                self.anchors,
            )?,
            ParagraphMeasurements {
                runs: self.runs,
                index: self.index,
                records: self.records,
                wrapped: true,
            },
        ))
    }
}

impl ParagraphReplay for WrappedParagraphPreparation<'_> {
    fn reuse_prefix_batch(
        &mut self,
        body: &str,
        old: &ParagraphMeasurements,
        end: usize,
        limit: usize,
    ) -> usize {
        self.reuse_prefix_batch(body, old, end, limit)
    }
    fn reuse_suffix_batch(
        &mut self,
        body: &str,
        old: &ParagraphMeasurements,
        start: usize,
        limit: usize,
    ) -> usize {
        self.reuse_suffix_batch(body, old, start, limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn layout(plan: &WrappedParagraphPreparation<'_>) -> (f64, Vec<GlyphRectangle>) {
        let probe = plan.probe().unwrap();
        let end = plan
            .index
            .index_at_byte(plan.body, probe.bytes.end)
            .unwrap();
        let offset = probe.origin / 7.0;
        let rectangles = plan
            .targets()
            .unwrap()
            .into_iter()
            .map(|glyph| {
                let position = offset + (glyph - probe.glyph_start) as f64;
                GlyphRectangle {
                    glyph,
                    left: (position % 40.0) * 7.0,
                    top: (position / 40.0).floor() * 15.0 + 2.0,
                    width: 7.0,
                    height: 15.0,
                }
            })
            .collect();
        (
            ((offset + (end - probe.glyph_start) as f64) / 40.0).ceil() * 15.0,
            rectangles,
        )
    }
    #[test]
    fn measured_origin_coverage_rejects_uncovered_rows_and_temporary_eof() {
        for body in ["word ".repeat(40_000), "word 文😀e\u{301} ".repeat(12_000)] {
            let index = VisualLineIndex::new(&body).unwrap();
            let runs = index.text_run_boundaries().collect::<Vec<_>>().into();
            let mut plan = WrappedParagraphPreparation::new(&body, index.clone(), runs).unwrap();
            assert!(plan.viewport_coverage(15.0).is_none());
            let (height, rectangles) = layout(&plan);
            assert!(plan.record(280.0, height, 280.0, &rectangles));
            let coverage = plan.viewport_coverage(15.0).unwrap();
            assert!(coverage.glyph_end() < index.len() - 1);
            assert!(coverage.covered_height() < height);
            assert_eq!(coverage.caret(0).unwrap().top.to_bits(), 2.0_f64.to_bits());
            assert!(coverage.caret(coverage.glyph_end()).is_none());
            assert!(coverage.caret(coverage.glyph_end() - 1).is_none());
            let interval = coverage.source_interval(0.0..300.0).unwrap();
            assert!(interval.start == 0 && interval.end < coverage.glyph_end());
            for bounds in [
                0.0..coverage.covered_height() + 1.0,
                -1.0..20.0,
                0.0..f64::INFINITY,
                f64::NAN..20.0,
            ] {
                assert!(coverage.source_interval(bounds).is_none());
            }
            assert!(plan.viewport_coverage(f64::NAN).is_none());
            while plan.probe().is_some() {
                let (height, rectangles) = layout(&plan);
                assert!(plan.record(280.0, height, 280.0, &rectangles));
            }
            assert!(plan.viewport_coverage(15.0).is_none());
            let (_, complete) = plan.finish().unwrap();
            assert!(complete.caret(index.len() - 1).is_some());
        }
    }
    #[test]
    fn bounded_wrapped_continuations_preserve_complete_extents_and_every_anchor() {
        for body in ["word ".repeat(40_000), "word 文😀e\u{301} ".repeat(12_000)] {
            let index = VisualLineIndex::new(&body).unwrap();
            let runs = index.text_run_boundaries().collect::<Vec<_>>().into();
            let mut plan = WrappedParagraphPreparation::new(&body, index.clone(), runs).unwrap();
            let mut turns = 0;
            while plan.probe().is_some() {
                assert!(plan.probe().unwrap().bytes.len() <= MAX_PARAGRAPH_PROBE_BYTES);
                let (height, rectangles) = layout(&plan);
                assert!(plan.record(280.0, height, 280.0, &rectangles));
                turns += 1;
                assert!(turns < 100);
            }
            let (width, geometry) = plan.finish().unwrap();
            assert_eq!(width.to_bits(), 280.0_f64.to_bits());
            assert_eq!(
                geometry.height.to_bits(),
                (((index.len() - 1) as f64 / 40.0).ceil() * 15.0).to_bits()
            );
            let actual = geometry.anchors(0..index.len() - 1).unwrap();
            let mut targets = index.anchor_glyphs().collect::<Vec<_>>();
            targets.dedup();
            let expected = targets
                .into_iter()
                .map(|glyph| GlyphRectangle {
                    glyph,
                    left: (glyph % 40) as f64 * 7.0,
                    top: (glyph / 40) as f64 * 15.0 + 2.0,
                    width: 7.0,
                    height: 15.0,
                })
                .collect::<Vec<_>>();
            assert_eq!(actual, expected);
            assert!(turns > 5);
        }
    }
    fn complete(body: &str) -> (WrappedGeometry, ParagraphMeasurements) {
        let index = VisualLineIndex::new(body).unwrap();
        let runs = index.text_run_boundaries().collect();
        let mut plan = WrappedParagraphPreparation::new(body, index, runs).unwrap();
        while plan.probe().is_some() {
            let (height, rectangles) = layout(&plan);
            assert!(plan.record(280.0, height, 280.0, &rectangles));
        }
        let (_, geometry, measurements) = plan.finish_with_measurements().unwrap();
        (geometry, measurements)
    }
    #[test]
    fn wrapped_replay_requires_source_style_overlap_and_fresh_terminal_extent() {
        let body = "word ".repeat(40_000);
        let (_, old) = complete(&body);
        for at in [0, body.len() - 5] {
            let mut changed = body.clone();
            changed.replace_range(at..at + 1, "z");
            let index = VisualLineIndex::new(&changed).unwrap();
            let runs = index.text_run_boundaries().collect();
            let mut plan = WrappedParagraphPreparation::new(&changed, index, runs).unwrap();
            assert_eq!(plan.reuse_prefix_batch(&body, &old, at, 0), 0);
            let mut reused = 0;
            loop {
                let next = plan.reuse_prefix_batch(&body, &old, at, 2);
                assert!(next <= 2);
                if next == 0 {
                    break;
                }
                reused += next;
            }
            if at > 0 {
                assert!(reused > 2);
            } else {
                assert_eq!(reused, 0);
            }
            let mut fresh = 0;
            let mut suffix = 0;
            while plan.probe().is_some() {
                let reused = plan.reuse_suffix_batch(&body, &old, at + 1, 2);
                assert!(reused <= 2);
                if reused > 0 {
                    suffix += reused;
                    continue;
                }
                let (height, rectangles) = layout(&plan);
                assert!(plan.record(280.0, height, 280.0, &rectangles));
                fresh += 1;
            }
            if at == 0 {
                assert!(suffix > 2);
            }
            assert!(fresh > 0, "new extent must be measured");
            assert_eq!(plan.finish().unwrap().1, complete(&changed).0);
        }
        let index = VisualLineIndex::new(&body).unwrap();
        let mut plan =
            WrappedParagraphPreparation::new(&body, index.clone(), old.runs.clone()).unwrap();
        let (height, rectangles) = layout(&plan);
        assert!(plan.record(280.0, height, 280.0, &rectangles));
        assert_eq!(plan.reuse_suffix_batch(&body, &old, body.len(), 2), 0);
        let mut corrupt = old.clone();
        let mut rectangles = corrupt.records[1].rectangles.to_vec();
        rectangles[0].left += 1.0;
        corrupt.records[1].rectangles = rectangles.into();
        assert_eq!(plan.reuse_suffix_batch(&body, &corrupt, 0, 2), 0);
        assert!(plan.probe().is_some());
        corrupt = old.clone();
        corrupt.wrapped = false;
        assert_eq!(plan.reuse_suffix_batch(&body, &corrupt, 0, 2), 0);
        let mut boundaries = old.runs.to_vec();
        boundaries[40] += 1;
        corrupt = old.clone();
        corrupt.runs = boundaries.into();
        assert_eq!(plan.reuse_suffix_batch(&body, &corrupt, 0, 2), 0);
        assert!(plan.finish().is_none());
    }
    #[test]
    fn changed_wrapped_origins_require_dense_line_break_proof_without_overflow() {
        let body = "word ".repeat(40_000);
        let (_, old) = complete(&body);
        for case in 0..4 {
            let index = VisualLineIndex::new(&body).unwrap();
            let mut plan =
                WrappedParagraphPreparation::new(&body, index, old.runs.clone()).unwrap();
            let (height, rectangles) = layout(&plan);
            assert!(plan.record(280.0, height, 280.0, &rectangles));
            plan.probe.origin -= 1.0;
            let top = plan.expected.first().unwrap().top;
            for rect in &mut plan.expected {
                if rect.top.to_bits() == top.to_bits() {
                    rect.left -= 1.0;
                }
            }
            let mut previous = old.clone();
            match case {
                1 => previous.records[1].scroll_width = Some(281.0),
                2 => plan
                    .expected
                    .retain(|rect| rect.top.to_bits() == top.to_bits()),
                3 => plan.expected.last_mut().unwrap().left += 1.0,
                _ => {}
            }
            let reused = plan.reuse_suffix_batch(&body, &previous, 0, 2);
            if case == 0 {
                assert_eq!(reused, 2);
            } else {
                assert_eq!(reused, 0, "invalid incoming proof case={case}");
            }
            assert!(
                plan.probe().is_some(),
                "replay must retain fresh terminal extent"
            );
        }
    }
    #[test]
    fn measured_prefix_reconnection_replaces_geometry_without_guessing_line_advances() {
        let body = "word ".repeat(40_000);
        let (_, previous) = complete(&body);
        for case in 0..4 {
            let index = VisualLineIndex::new(&body).unwrap();
            let mut plan =
                WrappedParagraphPreparation::new(&body, index, previous.runs.clone()).unwrap();
            let (height, rectangles) = layout(&plan);
            assert!(plan.record(280.0, height, 280.0, &rectangles));
            let record = &previous.records[1];
            let start = plan.probe.glyph_start;
            let first = record
                .rectangles
                .iter()
                .find(|rect| rect.glyph == start)
                .unwrap();
            let reconnect = record
                .rectangles
                .iter()
                .find(|rect| rect.glyph > start && rect.left == 0.0)
                .unwrap()
                .glyph;
            assert_eq!(reconnect, start + 5);
            plan.probe.origin = record.origin - 7.0;
            for expected in &mut plan.expected {
                if expected.glyph < reconnect - 1 {
                    expected.left -= 7.0;
                } else if expected.glyph == reconnect - 1 {
                    expected.left = 0.0;
                    expected.top += 15.0;
                } else {
                    expected.top += 15.0;
                }
            }
            let shift = plan.expected[0].top - first.top;
            let targets = plan.targets().unwrap();
            let mut rectangles = targets
                .iter()
                .map(|glyph| {
                    *record
                        .rectangles
                        .iter()
                        .find(|rect| rect.glyph == *glyph)
                        .unwrap()
                })
                .collect::<Vec<_>>();
            match case {
                1 => {
                    plan.expected.remove(1);
                }
                2 => plan.expected.last_mut().unwrap().left += 1.0,
                3 => plan.expected[0].width = f64::NAN,
                _ => {}
            }
            let original = rectangles.clone();
            let reconciled = plan.reconcile_prefix(record, &mut rectangles);
            if case == 0 {
                assert_eq!(reconciled, Some(record.height + 15.0));
                for (old, new) in original.iter().zip(&rectangles) {
                    if new.glyph < reconnect {
                        let expected = plan
                            .expected
                            .iter()
                            .find(|rect| rect.glyph == new.glyph)
                            .unwrap();
                        assert_eq!(new.left.to_bits(), expected.left.to_bits());
                        assert_eq!(new.top.to_bits(), (expected.top - shift).to_bits());
                    } else {
                        assert_eq!(new.top.to_bits(), (old.top + 15.0).to_bits());
                        assert_eq!(new.left.to_bits(), old.left.to_bits());
                    }
                }
                assert!(plan.replay_record(&body, &previous, record, false));
                assert_eq!(
                    plan.records.last().unwrap().height.to_bits(),
                    (record.height + 15.0).to_bits()
                );
            } else {
                assert!(
                    reconciled.is_none(),
                    "invalid measured-prefix proof case={case}"
                );
                assert_eq!(
                    rectangles, original,
                    "rejected proofs leave local geometry intact"
                );
            }
        }
    }

    #[test]
    fn wrapped_overlap_failures_and_incomplete_jobs_cannot_publish() {
        let body = "word ".repeat(40_000);
        let index = VisualLineIndex::new(&body).unwrap();
        let runs: Arc<[usize]> = index.text_run_boundaries().collect();
        let mut plan =
            WrappedParagraphPreparation::new(&body, index.clone(), runs.clone()).unwrap();
        let (height, rectangles) = layout(&plan);
        assert!(plan.record(280.0, height, 280.0, &rectangles));
        let (height, mut changed) = layout(&plan);
        changed[0].left += 1.0;
        assert!(!plan.record(280.0, height, 280.0, &changed));
        assert!(plan.finish().is_none());
        assert!(
            WrappedParagraphPreparation::new(&body, index, runs)
                .unwrap()
                .finish()
                .is_none()
        );
        for body in ["x".repeat(100_000), "word العربية ".repeat(12_000)] {
            let index = VisualLineIndex::new(&body).unwrap();
            let runs = index.text_run_boundaries().collect();
            assert!(WrappedParagraphPreparation::new(&body, index, runs).is_none());
        }
    }
}
