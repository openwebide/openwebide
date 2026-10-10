//! Exact styled glyph anchors used to prepare source slices before browser layout.
use std::ops::Range;

pub const MAX_ROW_GEOMETRY_ANCHORS: usize = 4096;
/// Exact range reads per cooperative cold-layout turn.
pub const ROW_GEOMETRY_BATCH: usize = 128;

/// A source-scoped geometry job publishes only the complete validated table.
/// Adapters supply real rectangles; neither scheduling nor sampling estimates
/// glyph advances. Dropping the job discards all incomplete measurements.
pub struct RowGeometryPreparation {
    glyphs: usize,
    width: f64,
    height: f64,
    horizontal: bool,
    targets: Vec<usize>,
    anchors: Vec<GlyphRectangle>,
}
impl RowGeometryPreparation {
    pub fn new(
        glyphs: usize,
        width: f64,
        height: f64,
        horizontal: bool,
        targets: Vec<usize>,
    ) -> Option<Self> {
        if glyphs == 0
            || !width.is_finite()
            || !(0.0..=1_000_000_000.0).contains(&width)
            || !height.is_finite()
            || !(0.0..=1_000_000.0).contains(&height)
            || height == 0.0
            || targets.is_empty()
            || targets.len() > MAX_ROW_GEOMETRY_ANCHORS
            || targets.first().copied() != Some(0)
            || targets.last().copied() != Some(glyphs - 1)
            || targets.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return None;
        }
        Some(Self {
            glyphs,
            width,
            height,
            horizontal,
            anchors: Vec::with_capacity(targets.len()),
            targets,
        })
    }
    pub fn pending(&self) -> &[usize] {
        let start = self.anchors.len();
        &self.targets[start..(start + ROW_GEOMETRY_BATCH).min(self.targets.len())]
    }
    /// Reject mismatched, invalid or partial batches without advancing the job.
    pub fn record(&mut self, rectangles: &[GlyphRectangle]) -> bool {
        let pending = self.pending();
        if pending.is_empty()
            || rectangles.len() != pending.len()
            || rectangles
                .iter()
                .zip(pending)
                .any(|(rectangle, glyph)| rectangle.glyph != *glyph || !rectangle.valid())
        {
            return false;
        }
        self.anchors.extend_from_slice(rectangles);
        true
    }
    pub fn finish(self) -> Option<MeasuredRowGeometry> {
        if self.anchors.len() != self.targets.len() {
            return None;
        }
        if self.horizontal {
            HorizontalGeometry::new(self.glyphs, self.width, self.height, self.anchors)
                .map(MeasuredRowGeometry::Horizontal)
        } else {
            WrappedGeometry::new(self.glyphs, self.width, self.height, self.anchors)
                .map(MeasuredRowGeometry::Wrapped)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphRectangle {
    pub glyph: usize,
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}
impl GlyphRectangle {
    pub(super) fn valid(self) -> bool {
        [self.left, self.top, self.width, self.height]
            .iter()
            .all(|v| v.is_finite())
            && self.left.abs() <= 1_000_000_000.0
            && self.top.abs() <= 1_000_000.0
            && (0.0..=1_000_000_000.0).contains(&self.width)
            && self.height > 0.0
            && self.height <= 1_000_000.0
    }
}

fn retained_caret(
    anchors: &[GlyphRectangle],
    glyphs: usize,
    glyph: usize,
) -> Option<GlyphRectangle> {
    if glyph > glyphs {
        return None;
    }
    let (anchor, left) = if glyph == glyphs {
        let anchor = *anchors.last()?;
        (anchor, anchor.left + anchor.width)
    } else {
        let at = anchors
            .binary_search_by_key(&glyph, |anchor| anchor.glyph)
            .ok()?;
        let anchor = anchors[at];
        (anchor, anchor.left)
    };
    Some(GlyphRectangle {
        glyph,
        left,
        width: 0.0,
        ..anchor
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct HorizontalGeometry {
    anchors: Vec<GlyphRectangle>,
    glyphs: usize,
    pub width: f64,
    pub height: f64,
}
impl HorizontalGeometry {
    /// The adapter supplies rectangles relative to the exact styled logical row.
    /// Retain this only within an immutable source/font/layout scope.
    pub fn new(
        glyphs: usize,
        width: f64,
        height: f64,
        anchors: Vec<GlyphRectangle>,
    ) -> Option<Self> {
        if glyphs == 0
            || anchors.is_empty()
            || anchors.len() > MAX_ROW_GEOMETRY_ANCHORS
            || anchors.capacity() > MAX_ROW_GEOMETRY_ANCHORS
            || !width.is_finite()
            || !(0.0..=1_000_000_000.0).contains(&width)
            || !height.is_finite()
            || !(0.0..=1_000_000.0).contains(&height)
            || height == 0.0
            || anchors.first()?.glyph != 0
            || anchors.last()?.glyph != glyphs - 1
            || anchors.iter().any(|a| !a.valid() || a.glyph >= glyphs)
            || anchors.windows(2).any(|pair| {
                pair[0].glyph >= pair[1].glyph
                    || pair[0].left > pair[1].left
                    || pair[0].left + pair[0].width > pair[1].left + pair[1].width
            })
        {
            return None;
        }
        Some(Self {
            anchors,
            glyphs,
            width,
            height,
        })
    }
    /// Exact caret boundaries from retained LTR glyph rectangles. A sparse gap
    /// has no geometry; never interpolate a caret through unmeasured glyphs.
    pub fn caret(&self, glyph: usize) -> Option<GlyphRectangle> {
        retained_caret(&self.anchors, self.glyphs, glyph)
    }
    /// Include a measured anchor before the window for shaping context and a
    /// complete anchor glyph after it for validation. The adapter can reject an
    /// exceptional source slice (for example an indivisible huge cluster).
    pub fn source_interval(&self, columns: Range<f64>) -> Option<Range<usize>> {
        if !columns.start.is_finite()
            || !columns.end.is_finite()
            || columns.start < 0.0
            || columns.end <= columns.start
        {
            return None;
        }
        let last = self.anchors.last()?;
        if columns.start > last.left + last.width {
            return Some(self.glyphs..self.glyphs);
        }
        let start = self
            .anchors
            .partition_point(|a| a.left + a.width < columns.start)
            .saturating_sub(1);
        let end = self
            .anchors
            .partition_point(|a| a.left < columns.end)
            .min(self.anchors.len() - 1);
        Some(self.anchors[start].glyph..self.anchors[end].glyph + 1)
    }
    pub fn anchors(&self, source: Range<usize>) -> Option<&[GlyphRectangle]> {
        if source.start > source.end || source.end > self.glyphs {
            return None;
        }
        let first = self.anchors.partition_point(|a| a.glyph < source.start);
        let last = self.anchors.partition_point(|a| a.glyph < source.end);
        Some(&self.anchors[first..last])
    }
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.anchors.capacity() * std::mem::size_of::<GlyphRectangle>()
    }
}

/// Measured origin coverage while the remaining paragraph is still preparing.
/// This is a lower bound, never a complete row extent or a temporary EOF.
#[derive(Clone, Debug, PartialEq)]
pub struct WrappedCoverage {
    geometry: WrappedGeometry,
    covered_height: f64,
}
impl WrappedCoverage {
    pub(super) fn new(geometry: WrappedGeometry, covered_height: f64) -> Option<Self> {
        (covered_height.is_finite() && covered_height > 0.0).then_some(Self {
            geometry,
            covered_height,
        })
    }
    pub fn covered_height(&self) -> f64 {
        self.covered_height
    }
    /// First uncovered glyph, not the source end-of-file.
    pub fn glyph_end(&self) -> usize {
        self.geometry.glyphs
    }
    pub fn caret(&self, glyph: usize) -> Option<GlyphRectangle> {
        // A partial endpoint has no measured next-glyph affinity.
        (glyph < self.geometry.glyphs - 1)
            .then(|| self.geometry.caret(glyph))
            .flatten()
    }
    pub fn source_interval(&self, rows: Range<f64>) -> Option<Range<usize>> {
        (rows.end <= self.covered_height)
            .then(|| self.geometry.source_interval(rows))
            .flatten()
    }
}

/// Styled anchors for wrapped rows. Source order follows vertical layout;
/// horizontal positions may reverse within a bidirectional visual row.
#[derive(Clone, Debug, PartialEq)]
pub struct WrappedGeometry {
    anchors: Vec<GlyphRectangle>,
    glyphs: usize,
    pub width: f64,
    pub height: f64,
}
impl WrappedGeometry {
    pub fn new(
        glyphs: usize,
        width: f64,
        height: f64,
        anchors: Vec<GlyphRectangle>,
    ) -> Option<Self> {
        if glyphs == 0
            || anchors.is_empty()
            || anchors.len() > MAX_ROW_GEOMETRY_ANCHORS
            || anchors.capacity() > MAX_ROW_GEOMETRY_ANCHORS
            || !width.is_finite()
            || !(0.0..=1_000_000_000.0).contains(&width)
            || !height.is_finite()
            || !(0.0..=1_000_000.0).contains(&height)
            || height == 0.0
            || anchors.first()?.glyph != 0
            || anchors.last()?.glyph != glyphs - 1
            || anchors.iter().any(|a| !a.valid() || a.glyph >= glyphs)
            || anchors.windows(2).any(|pair| {
                pair[0].glyph >= pair[1].glyph
                    || pair[0].top > pair[1].top
                    || pair[0].top + pair[0].height > pair[1].top + pair[1].height
            })
        {
            return None;
        }
        Some(Self {
            anchors,
            glyphs,
            width,
            height,
        })
    }
    /// Retain exact endpoints and adjacent same-row boundaries. An interior
    /// anchor after an unknown gap may sit on a soft wrap with a different
    /// caret affinity, so it must retain the complete browser fallback.
    pub fn caret(&self, glyph: usize) -> Option<GlyphRectangle> {
        if glyph != 0 && glyph != self.glyphs {
            let at = self
                .anchors
                .binary_search_by_key(&glyph, |anchor| anchor.glyph)
                .ok()?;
            let previous = self.anchors.get(at.checked_sub(1)?)?;
            let current = self.anchors.get(at)?;
            if previous.glyph + 1 != glyph || previous.top.to_bits() != current.top.to_bits() {
                return None;
            }
        }
        retained_caret(&self.anchors, self.glyphs, glyph)
    }
    pub fn source_interval(&self, rows: Range<f64>) -> Option<Range<usize>> {
        if !rows.start.is_finite()
            || !rows.end.is_finite()
            || rows.start < 0.0
            || rows.end <= rows.start
        {
            return None;
        }
        let start = self
            .anchors
            .partition_point(|a| a.top + a.height < rows.start)
            .saturating_sub(1);
        let end = self
            .anchors
            .partition_point(|a| a.top < rows.end)
            .min(self.anchors.len() - 1);
        Some(self.anchors[start].glyph..self.anchors[end].glyph + 1)
    }
    pub fn anchors(&self, source: Range<usize>) -> Option<&[GlyphRectangle]> {
        if source.start > source.end || source.end > self.glyphs {
            return None;
        }
        let first = self.anchors.partition_point(|a| a.glyph < source.start);
        let last = self.anchors.partition_point(|a| a.glyph < source.end);
        Some(&self.anchors[first..last])
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MeasuredRowGeometry {
    Horizontal(HorizontalGeometry),
    Wrapped(WrappedGeometry),
    /// Current measured coverage, not a complete logical-row extent.
    WrappedCoverage(std::sync::Arc<WrappedCoverage>),
}
impl MeasuredRowGeometry {
    pub fn caret(&self, glyph: usize) -> Option<GlyphRectangle> {
        match self {
            Self::Horizontal(geometry) => geometry.caret(glyph),
            Self::Wrapped(geometry) => geometry.caret(glyph),
            Self::WrappedCoverage(geometry) => geometry.caret(glyph),
        }
    }
    pub fn source_interval(
        &self,
        window: &super::RowPaintWindow,
        line_height: f64,
    ) -> Option<Range<usize>> {
        match (self, window) {
            (Self::Horizontal(geometry), super::RowPaintWindow::Horizontal(columns)) => {
                geometry.source_interval(columns.clone())
            }
            (Self::Wrapped(geometry), super::RowPaintWindow::Wrapped(window))
                if line_height.is_finite() && line_height > 0.0 =>
            {
                geometry.source_interval(
                    window.rows.start as f64 * line_height..window.rows.end as f64 * line_height,
                )
            }
            (Self::WrappedCoverage(geometry), super::RowPaintWindow::Wrapped(window))
                if line_height.is_finite() && line_height > 0.0 =>
            {
                geometry.source_interval(
                    window.rows.start as f64 * line_height..window.rows.end as f64 * line_height,
                )
            }
            _ => None,
        }
    }
    pub fn anchors(&self, source: Range<usize>) -> Option<&[GlyphRectangle]> {
        match self {
            Self::Horizontal(g) => g.anchors(source),
            Self::Wrapped(g) => g.anchors(source),
            Self::WrappedCoverage(g) => g.geometry.anchors(source),
        }
    }
    pub fn width(&self) -> f64 {
        match self {
            Self::Horizontal(g) => g.width,
            Self::Wrapped(g) => g.width,
            Self::WrappedCoverage(g) => g.geometry.width,
        }
    }
    pub fn height(&self) -> f64 {
        match self {
            Self::Horizontal(g) => g.height,
            Self::Wrapped(g) => g.height,
            Self::WrappedCoverage(g) => g.covered_height,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cooperative_geometry_matches_complete_tables_without_partial_publication() {
        for horizontal in [true, false] {
            let glyphs = 6000;
            let targets = (0..glyphs)
                .step_by(3)
                .chain([glyphs - 1])
                .collect::<Vec<_>>();
            let rectangles = targets
                .iter()
                .map(|&glyph| GlyphRectangle {
                    glyph,
                    left: if horizontal {
                        glyph as f64 * 8.0
                    } else {
                        (glyph % 40) as f64 * 8.0
                    },
                    top: if horizontal {
                        0.0
                    } else {
                        (glyph / 40) as f64 * 15.0
                    },
                    width: 8.0,
                    height: 15.0,
                })
                .collect::<Vec<_>>();
            let expected = if horizontal {
                MeasuredRowGeometry::Horizontal(
                    HorizontalGeometry::new(glyphs, 320.0, 3000.0, rectangles.clone()).unwrap(),
                )
            } else {
                MeasuredRowGeometry::Wrapped(
                    WrappedGeometry::new(glyphs, 320.0, 3000.0, rectangles.clone()).unwrap(),
                )
            };
            let mut plan =
                RowGeometryPreparation::new(glyphs, 320.0, 3000.0, horizontal, targets.clone())
                    .unwrap();
            let mut at = 0;
            while !plan.pending().is_empty() {
                let count = plan.pending().len();
                assert!(count <= ROW_GEOMETRY_BATCH);
                assert_eq!(plan.pending(), &targets[at..at + count]);
                assert!(plan.record(&rectangles[at..at + count]));
                at += count;
            }
            assert_eq!(plan.finish(), Some(expected));
            let mut cancelled =
                RowGeometryPreparation::new(glyphs, 320.0, 3000.0, horizontal, targets).unwrap();
            assert!(cancelled.record(&rectangles[..ROW_GEOMETRY_BATCH]));
            assert!(cancelled.finish().is_none());
        }
    }
    #[test]
    fn invalid_geometry_batches_cannot_advance_or_weaken_complete_validation() {
        let mut plan = RowGeometryPreparation::new(2, 100.0, 15.0, true, vec![0, 1]).unwrap();
        assert!(!plan.record(&[anchor(0, 0.0)]));
        assert!(!plan.record(&[anchor(0, 0.0), anchor(0, 8.0)]));
        assert!(!plan.record(&[anchor(0, f64::NAN), anchor(1, 8.0)]));
        assert_eq!(plan.pending(), &[0, 1]);
        assert!(plan.record(&[anchor(0, 8.0), anchor(1, 0.0)]));
        assert!(!plan.record(&[]));
        assert!(plan.finish().is_none());
        for targets in [vec![], vec![1], vec![0, 0, 1], vec![0, 2, 1]] {
            assert!(RowGeometryPreparation::new(2, 100.0, 15.0, true, targets).is_none());
        }
        assert!(RowGeometryPreparation::new(2, f64::NAN, 15.0, true, vec![0, 1]).is_none());
        assert!(RowGeometryPreparation::new(2, 100.0, 0.0, true, vec![0, 1]).is_none());
        assert!(
            RowGeometryPreparation::new(
                MAX_ROW_GEOMETRY_ANCHORS + 1,
                100.0,
                15.0,
                true,
                (0..=MAX_ROW_GEOMETRY_ANCHORS).collect()
            )
            .is_none()
        );
    }
    fn anchor(glyph: usize, left: f64) -> GlyphRectangle {
        GlyphRectangle {
            glyph,
            left,
            top: 2.0,
            width: 8.0,
            height: 15.0,
        }
    }
    #[test]
    fn retained_carets_use_exact_anchors_and_reject_sparse_gaps() {
        let geometry = HorizontalGeometry::new(
            1001,
            8010.0,
            20.0,
            vec![anchor(0, 0.0), anchor(250, 2000.0), anchor(1000, 8000.0)],
        )
        .unwrap();
        assert_eq!(
            geometry.caret(0),
            Some(GlyphRectangle {
                width: 0.0,
                ..anchor(0, 0.0)
            })
        );
        assert_eq!(
            geometry.caret(250),
            Some(GlyphRectangle {
                width: 0.0,
                ..anchor(250, 2000.0)
            })
        );
        assert!(geometry.caret(249).is_none());
        assert_eq!(
            geometry.caret(1001),
            Some(GlyphRectangle {
                glyph: 1001,
                left: 8008.0,
                width: 0.0,
                ..anchor(1000, 8000.0)
            })
        );
        assert!(geometry.caret(1002).is_none());
        assert!(geometry.caret(usize::MAX).is_none());
    }
    #[test]
    fn anchored_windows_keep_source_context_validation_and_offscreen_gaps() {
        let geometry = HorizontalGeometry::new(
            1001,
            8010.0,
            20.0,
            vec![
                anchor(0, 0.0),
                anchor(250, 2000.0),
                anchor(500, 4000.0),
                anchor(750, 6000.0),
                anchor(1000, 8000.0),
            ],
        )
        .unwrap();
        assert_eq!(geometry.source_interval(2100.0..3900.0), Some(250..501));
        assert_eq!(
            geometry.anchors(250..501).unwrap(),
            &[anchor(250, 2000.0), anchor(500, 4000.0)]
        );
        assert_eq!(geometry.source_interval(0.0..400.0), Some(0..251));
        assert_eq!(geometry.source_interval(7990.0..9000.0), Some(750..1001));
        assert_eq!(geometry.source_interval(9000.0..10000.0), Some(1001..1001));
        assert_eq!(geometry.anchors(1001..1001).unwrap(), &[]);
        assert!(geometry.anchors(1001..1002).is_none());
        assert!(geometry.source_interval(f64::NAN..500.0).is_none());
        assert!(geometry.source_interval(100.0..10.0).is_none());
    }
    #[test]
    fn wrapped_windows_follow_vertical_source_order_and_reject_stale_orientation() {
        let points = vec![
            anchor(0, 30.0),
            GlyphRectangle {
                glyph: 250,
                top: 202.0,
                left: 50.0,
                ..anchor(0, 0.0)
            },
            GlyphRectangle {
                glyph: 500,
                top: 402.0,
                left: 0.0,
                ..anchor(0, 0.0)
            },
        ];
        let geometry = WrappedGeometry::new(501, 100.0, 420.0, points.clone()).unwrap();
        assert_eq!(geometry.source_interval(210.0..390.0), Some(0..501));
        assert_eq!(geometry.source_interval(220.0..390.0), Some(250..501));
        assert_eq!(geometry.anchors(250..501).unwrap(), &points[1..]);
        assert_eq!(
            geometry.caret(0).unwrap().left.to_bits(),
            30.0_f64.to_bits()
        );
        assert!(geometry.caret(250).is_none());
        assert!(geometry.caret(500).is_none());
        assert_eq!(
            geometry.caret(501).unwrap().top.to_bits(),
            402.0_f64.to_bits()
        );
        assert_eq!(
            geometry.caret(501).unwrap().left.to_bits(),
            points[2].width.to_bits()
        );
        assert_eq!(
            geometry.caret(0).unwrap().width.to_bits(),
            0.0_f64.to_bits()
        );
        assert!(geometry.caret(1).is_none());
        assert!(geometry.caret(502).is_none());
        let adjacent = WrappedGeometry::new(
            3,
            100.0,
            40.0,
            vec![
                anchor(0, 0.0),
                anchor(1, 12.0),
                GlyphRectangle {
                    top: 22.0,
                    ..anchor(2, 0.0)
                },
            ],
        )
        .unwrap();
        assert_eq!(
            adjacent.caret(1).unwrap().left.to_bits(),
            12.0_f64.to_bits()
        );
        assert!(
            adjacent.caret(2).is_none(),
            "soft-wrap affinity needs a browser proof"
        );
        assert_eq!(adjacent.caret(3).unwrap().top.to_bits(), 22.0_f64.to_bits());
        let measured = MeasuredRowGeometry::Wrapped(geometry);
        let window = super::super::RowPaintWindow::Wrapped(super::super::EditorViewport {
            rows: 11..20,
            top: 220.0,
            height: 420.0,
        });
        assert_eq!(measured.source_interval(&window, 20.0), Some(250..501));
        assert!(measured.source_interval(&window, f64::NAN).is_none());
        assert!(
            measured
                .source_interval(&super::super::RowPaintWindow::Horizontal(0.0..100.0), 20.0)
                .is_none()
        );
        let mut reversed = points.clone();
        reversed[2].top = 100.0;
        assert!(WrappedGeometry::new(501, 100.0, 420.0, reversed).is_none());
        assert!(WrappedGeometry::new(502, 100.0, 420.0, points).is_none());
        let mut oversized = Vec::with_capacity(MAX_ROW_GEOMETRY_ANCHORS + 1);
        oversized.push(anchor(0, 0.0));
        assert!(WrappedGeometry::new(1, 100.0, 20.0, oversized).is_none());
    }
    #[test]
    fn incomplete_nonmonotonic_oversized_or_invalid_measurements_require_fallback() {
        for points in [
            vec![],
            vec![anchor(1, 0.0)],
            vec![anchor(0, 0.0)],
            vec![anchor(0, 10.0), anchor(1, 0.0)],
            vec![anchor(0, 0.0), anchor(0, 1.0)],
            vec![anchor(0, 0.0), anchor(1, f64::NAN)],
            vec![anchor(0, 0.0), anchor(1, f64::INFINITY)],
        ] {
            assert!(HorizontalGeometry::new(2, 20.0, 20.0, points).is_none());
        }
        assert!(HorizontalGeometry::new(1, 10.0, f64::INFINITY, vec![anchor(0, 0.0)]).is_none());
        let points: Vec<_> = (0..MAX_ROW_GEOMETRY_ANCHORS + 1)
            .map(|i| anchor(i, 0.0))
            .collect();
        assert!(HorizontalGeometry::new(points.len(), 10.0, 20.0, points).is_none());
        let mut reserved = Vec::with_capacity(MAX_ROW_GEOMETRY_ANCHORS + 1);
        reserved.push(anchor(0, 0.0));
        assert!(HorizontalGeometry::new(1, 10.0, 20.0, reserved).is_none());
        // Styled glyph rectangles may overflow the logical CSS row box.
        assert!(
            HorizontalGeometry::new(2, 20.0, 20.0, vec![anchor(0, 0.0), anchor(1, 100.0)])
                .is_some()
        );
        // Ligature rectangles can overlap; only source-monotonic positions are required.
        assert!(
            HorizontalGeometry::new(2, 10.0, 20.0, vec![anchor(0, 0.0), anchor(1, 0.0)]).is_some()
        );
    }
}
