//! Sparse source coordinates shared by paint and visual cursor measurements.
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

const STEP_BYTES: usize = 512;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Position {
    byte: usize,
    native: usize,
    glyph: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Coordinates {
    points: Vec<Position>,
    long_clusters: Vec<LongCluster>,
    end: Position,
    horizontal: bool,
    tabs: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LongCluster {
    start: Position,
    end: usize,
}

/// Resumable construction of exact visual coordinates. Only complete indexes
/// can be published; dropping pending work leaves its source and prior index intact.
pub struct VisualLinePreparation<'a> {
    body: &'a str,
    scan: super::grapheme_scan::GraphemeScan<'a>,
    coordinates: Coordinates,
    cluster: Position,
}
impl<'a> VisualLinePreparation<'a> {
    pub fn new(body: &'a str) -> Option<Self> {
        (body.len() <= super::MAX_STRUCTURE_BYTES).then(|| Self {
            body,
            scan: super::grapheme_scan::GraphemeScan::new(body),
            coordinates: Coordinates {
                points: vec![Position::default()],
                long_clusters: Vec::new(),
                end: Position::default(),
                horizontal: body.len() > super::MAX_MEASURE_BYTES,
                tabs: false,
            },
            cluster: Position::default(),
        })
    }
    /// Budget counts bounded source/context scan units, including suspensions
    /// within large graphemes. No partial index authorizes coordinates or paint.
    pub fn advance(&mut self, budget: usize) -> bool {
        self.advance_with_character_scan(budget, |_, _| {})
    }
    pub(super) fn advance_with_character_scan(
        &mut self,
        budget: usize,
        mut visit: impl FnMut(usize, char),
    ) -> bool {
        for _ in 0..budget {
            if self.scan.is_done() {
                break;
            }
            let coordinates = &mut self.coordinates;
            let cluster = &mut self.cluster;
            let body = self.body;
            self.scan.advance(|step| {
                for (offset, ch) in body[step.consumed.clone()].char_indices() {
                    visit(step.consumed.start + offset, ch);
                    coordinates.end.native += ch.len_utf16();
                    coordinates.tabs |= ch == '\t';
                    if coordinates.horizontal && !super::viewport::horizontal_paint_character(ch) {
                        coordinates.horizontal = false;
                    }
                }
                if let Some(end) = step.boundary {
                    if cluster.byte - coordinates.points.last().unwrap().byte >= STEP_BYTES {
                        coordinates.points.push(*cluster);
                    }
                    if end - cluster.byte > STEP_BYTES {
                        coordinates.long_clusters.push(LongCluster {
                            start: *cluster,
                            end,
                        });
                    }
                    coordinates.end.glyph += 1;
                    *cluster = Position {
                        byte: end,
                        ..coordinates.end
                    };
                }
                false
            });
        }
        self.scan.is_done()
    }
    pub fn finish(mut self) -> Option<VisualLineIndex> {
        if !self.scan.is_done() {
            return None;
        }
        self.coordinates.end.byte = self.body.len();
        if self.coordinates.points.last()?.byte != self.body.len() {
            self.coordinates.points.push(self.coordinates.end);
        }
        Some(VisualLineIndex(Arc::new(self.coordinates)))
    }
}

/// Immutable coordinates for one exact logical-line body. Callers retain the
/// associated source snapshot and must supply that same body to lookup methods.
/// Unchanged document rows and folded projections share the sparse allocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisualLineIndex(Arc<Coordinates>);
impl VisualLineIndex {
    #[cfg(any(test, feature = "test-support"))]
    pub fn shared_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn new(body: &str) -> Option<Self> {
        Self::with_character_scan(body, |_, _| {})
    }
    /// Share the exact character traversal with source-coordinate construction.
    /// Grapheme checkpoints and original plain paint boundaries stay unchanged.
    pub(super) fn with_character_scan(
        body: &str,
        mut visit: impl FnMut(usize, char),
    ) -> Option<Self> {
        let mut preparation = VisualLinePreparation::new(body)?;
        preparation.advance_with_character_scan(usize::MAX, &mut visit);
        preparation.finish()
    }
    /// Exact cluster checkpoints for a styled geometry adapter; omit EOF.
    pub fn anchor_glyphs(&self) -> impl Iterator<Item = usize> + '_ {
        self.0
            .points
            .iter()
            .map(|point| point.glyph)
            .filter(|glyph| *glyph < self.0.end.glyph)
            .chain(self.0.end.glyph.checked_sub(1))
    }
    /// Exact cluster checkpoints within one probe, without traversing other probes'
    /// checkpoints. Preserve the complete iterator's terminal anchor and duplicates.
    pub(super) fn anchor_glyphs_in(
        &self,
        range: std::ops::Range<usize>,
    ) -> impl Iterator<Item = usize> + '_ {
        let end = range.end.min(self.0.end.glyph);
        let start = range.start.min(end);
        let first = self.0.points.partition_point(|point| point.glyph < start);
        let last = self.0.points.partition_point(|point| point.glyph < end);
        self.0.points[first..last]
            .iter()
            .map(|point| point.glyph)
            .chain(
                self.0
                    .end
                    .glyph
                    .checked_sub(1)
                    .filter(|glyph| range.contains(glyph)),
            )
    }
    /// The plain renderer's original grapheme-safe 512-byte run endings.
    pub fn text_run_boundaries(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.points.iter().skip(1).map(|point| point.byte)
    }
    pub fn is_text_run_boundary(&self, byte: usize) -> bool {
        self.0
            .points
            .binary_search_by_key(&byte, |point| point.byte)
            .is_ok()
    }
    /// Includes the terminal insertion point, matching visual_line_offsets.
    pub fn len(&self) -> usize {
        self.0.end.glyph + 1
    }
    pub fn is_empty(&self) -> bool {
        self.0.end.byte == 0
    }
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Coordinates>()
            + self.0.points.capacity() * std::mem::size_of::<Position>()
            + self.0.long_clusters.capacity() * std::mem::size_of::<LongCluster>()
    }
    /// Long source-monotonic paragraphs can be sliced before shaping. Bidi
    /// controls, reversed scripts and display breaks retain full-paragraph probes.
    pub fn source_paint_eligible(&self) -> bool {
        self.0.horizontal
    }
    /// Tab presence belongs to this exact immutable source snapshot, avoiding
    /// another long-row scan when admitting native input during preparation.
    pub fn has_tabs(&self) -> bool {
        self.0.tabs
    }
    pub fn horizontal_paint_bounds(&self, scroll: f64, width: f64) -> Option<std::ops::Range<f64>> {
        super::viewport::horizontal_paint_geometry(self.0.horizontal, scroll, width)
    }
    fn valid_body(&self, body: &str) -> bool {
        body.len() == self.0.end.byte
    }
    fn long_cluster_at(&self, byte: usize) -> Option<&LongCluster> {
        let index = self
            .0
            .long_clusters
            .partition_point(|cluster| cluster.start.byte <= byte);
        self.0
            .long_clusters
            .get(index.checked_sub(1)?)
            .filter(|cluster| byte < cluster.end)
    }
    /// Stop local segmentation before an already indexed indivisible cluster.
    /// Its known start is a complete grapheme boundary, so truncation is exact.
    fn local_end(&self, byte: usize) -> usize {
        let index = self
            .0
            .long_clusters
            .partition_point(|cluster| cluster.start.byte < byte);
        self.0
            .long_clusters
            .get(index)
            .map_or(self.0.end.byte, |cluster| cluster.start.byte)
    }
    pub fn at(&self, body: &str, glyph: usize) -> Option<(usize, usize)> {
        if !self.valid_body(body) || glyph >= self.len() {
            return None;
        }
        if let Ok(index) = self
            .0
            .long_clusters
            .binary_search_by_key(&glyph, |cluster| cluster.start.glyph)
        {
            let start = self.0.long_clusters[index].start;
            return Some((start.byte, start.native));
        }
        let index = self
            .0
            .points
            .partition_point(|point| point.glyph <= glyph)
            .checked_sub(1)?;
        let point = self.0.points[index];
        let suffix = body.get(point.byte..self.local_end(point.byte))?;
        let byte = suffix
            .grapheme_indices(true)
            .map(|(byte, _)| byte)
            .chain(std::iter::once(suffix.len()))
            .nth(glyph - point.glyph)?;
        Some((
            point.byte + byte,
            point.native + suffix[..byte].encode_utf16().count(),
        ))
    }
    /// Exact coordinates for ordered glyph queries, sharing traversal between
    /// nearby positions and skipping sparse gaps through retained checkpoints.
    /// Duplicates and EOF are valid; unordered or over-budget queries are rejected.
    pub fn positions(&self, body: &str, glyphs: &[usize]) -> Option<Vec<(usize, usize)>> {
        if !self.valid_body(body)
            || glyphs.len() > super::MAX_VISUAL_CARETS
            || glyphs.last().is_some_and(|glyph| *glyph >= self.len())
            || glyphs.windows(2).any(|pair| pair[0] > pair[1])
        {
            return None;
        }
        let mut cursor = Position::default();
        let mut result = Vec::with_capacity(glyphs.len());
        for &target in glyphs {
            let checkpoint = self
                .0
                .points
                .partition_point(|point| point.glyph <= target)
                .checked_sub(1)?;
            let point = self.0.points[checkpoint];
            if point.glyph > cursor.glyph {
                cursor = point;
            }
            for cluster in body
                .get(cursor.byte..)?
                .graphemes(true)
                .take(target - cursor.glyph)
            {
                cursor.byte += cluster.len();
                cursor.native += cluster.encode_utf16().count();
                cursor.glyph += 1;
            }
            if cursor.glyph != target {
                return None;
            }
            result.push((cursor.byte, cursor.native));
        }
        Some(result)
    }

    /// Exact glyphs at ordered paint boundaries. Interior cluster bytes are
    /// omitted, matching paragraph anchor admission. Nearby queries share one
    /// traversal; sparse gaps resume at retained source checkpoints.
    pub(super) fn boundary_glyphs(&self, body: &str, bytes: &[usize]) -> Option<Vec<usize>> {
        if !self.valid_body(body)
            || bytes.len() > super::MAX_ROW_GEOMETRY_ANCHORS
            || bytes.last().is_some_and(|byte| *byte > body.len())
            || bytes.windows(2).any(|pair| pair[0] > pair[1])
        {
            return None;
        }
        let mut cursor = Position::default();
        let mut result = Vec::with_capacity(bytes.len());
        for &target in bytes {
            if let Some(cluster) = self.long_cluster_at(target) {
                if target == cluster.start.byte {
                    result.push(cluster.start.glyph);
                }
                continue;
            }
            let checkpoint = self
                .0
                .points
                .partition_point(|point| point.byte <= target)
                .checked_sub(1)?;
            let point = self.0.points[checkpoint];
            if point.byte > cursor.byte {
                cursor = point;
            }
            let mut clusters = body
                .get(cursor.byte..self.local_end(cursor.byte))?
                .graphemes(true);
            while cursor.byte < target {
                let cluster = clusters.next()?;
                cursor.byte += cluster.len();
                cursor.native += cluster.encode_utf16().count();
                cursor.glyph += 1;
            }
            if cursor.byte == target {
                result.push(cursor.glyph);
            }
        }
        Some(result)
    }

    pub fn index_at_byte(&self, body: &str, byte: usize) -> Option<usize> {
        if !self.valid_body(body) || byte > body.len() {
            return None;
        }
        if let Some(cluster) = self.long_cluster_at(byte) {
            return Some(cluster.start.glyph);
        }
        let index = self
            .0
            .points
            .partition_point(|point| point.byte <= byte)
            .checked_sub(1)?;
        let point = self.0.points[index];
        let count = body
            .get(point.byte..self.local_end(point.byte))?
            .grapheme_indices(true)
            .take_while(|(at, _)| point.byte + at <= byte)
            .count();
        Some((point.glyph + count.saturating_sub(1)).min(self.0.end.glyph))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cooperative_indexes_visit_source_once_and_publish_only_complete_coordinates() {
        for body in [
            String::new(),
            "a".repeat(513),
            format!("{}🇺🇸", "a".repeat(508)),
            "word 文😀e\u{301}\t\r ".repeat(6000),
            format!("x👩{}\u{200d}👩 last", "\u{301}".repeat(40_000)),
            format!("क{}ष last", "\u{94d}\u{93c}".repeat(10_000)),
            "אב word ".repeat(10_000),
        ] {
            let expected = super::super::visual_line_offsets(&body).unwrap();
            let runs = super::super::visual_text_run_ranges(&body)
                .map(|run| run.end)
                .collect::<Vec<_>>();
            for budget in [1, 8, 64] {
                assert!(
                    VisualLinePreparation::new(&body)
                        .unwrap()
                        .finish()
                        .is_none()
                );
                let mut prep = VisualLinePreparation::new(&body).unwrap();
                assert!(!prep.advance(0));
                let mut visited = Vec::new();
                let mut turns = 0;
                loop {
                    let before = prep.scan.progress();
                    let done = prep
                        .advance_with_character_scan(budget, |byte, ch| visited.push((byte, ch)));
                    assert!(prep.scan.progress() - before <= budget * 1536);
                    turns += 1;
                    if done {
                        break;
                    }
                    assert!(turns < body.len() * 4 + 1);
                }
                assert_eq!(visited, body.char_indices().collect::<Vec<_>>());
                let index = prep.finish().unwrap();
                assert_eq!(index.len(), expected.len());
                assert_eq!(index.text_run_boundaries().collect::<Vec<_>>(), runs);
                assert_eq!(index.has_tabs(), body.contains('\t'));
                assert_eq!(
                    index.source_paint_eligible(),
                    body.len() > super::super::MAX_MEASURE_BYTES
                        && body
                            .chars()
                            .all(super::super::viewport::horizontal_paint_character)
                );
                for glyph in (0..expected.len()).step_by(97).chain([expected.len() - 1]) {
                    assert_eq!(index.at(&body, glyph), Some(expected[glyph]));
                }
                if body.contains(&"\u{301}".repeat(1000)) {
                    assert!(turns > 1);
                }
            }
        }
        assert!(
            VisualLinePreparation::new(&"a".repeat(super::super::MAX_STRUCTURE_BYTES + 1))
                .is_none()
        );
    }
    #[test]
    fn long_clusters_keep_local_queries_bounded_and_original_paint_seams_exact() {
        for prefix in [0, 1, 200, 511, 512, 513] {
            for cluster in [
                format!("e{}", "\u{301}".repeat(40_000)),
                format!("👩{}\u{200d}👩", "\u{301}".repeat(40_000)),
                format!("क{}ष", "\u{94d}\u{93c}".repeat(10_000)),
            ] {
                let body = format!("{}{cluster} tail", "x".repeat(prefix));
                let index = VisualLineIndex::new(&body).unwrap();
                let boundaries = body
                    .grapheme_indices(true)
                    .map(|(byte, _)| byte)
                    .chain([body.len()])
                    .collect::<Vec<_>>();
                let expected_runs = super::super::visual_text_run_ranges(&body)
                    .map(|run| run.end)
                    .collect::<Vec<_>>();
                assert_eq!(
                    index.text_run_boundaries().collect::<Vec<_>>(),
                    expected_runs
                );
                for (glyph, &byte) in boundaries.iter().enumerate() {
                    assert_eq!(
                        index.at(&body, glyph),
                        Some((byte, body[..byte].encode_utf16().count()))
                    );
                }
                let end = prefix + cluster.len();
                assert_eq!(index.0.long_clusters.len(), 1);
                assert_eq!(index.local_end(0), prefix);
                for byte in (prefix..end).step_by(97) {
                    assert_eq!(index.index_at_byte(&body, byte), Some(prefix));
                    let expected = if byte == prefix {
                        vec![prefix]
                    } else {
                        Vec::new()
                    };
                    assert_eq!(index.boundary_glyphs(&body, &[byte]), Some(expected));
                }
                let queries = [
                    prefix.saturating_sub(1),
                    prefix,
                    prefix + 1,
                    end - 1,
                    end,
                    body.len(),
                ];
                let expected = queries
                    .iter()
                    .filter_map(|byte| boundaries.binary_search(byte).ok())
                    .collect();
                assert_eq!(index.boundary_glyphs(&body, &queries), Some(expected));
                // A query immediately before the cluster scans only the prefix,
                // rather than materializing the giant next grapheme for lookahead.
                if prefix > 0 {
                    assert_eq!(index.index_at_byte(&body, prefix - 1), Some(prefix - 1));
                }
            }
        }
    }
    #[test]
    fn ordered_paint_boundaries_match_complete_grapheme_coordinates() {
        for body in [
            String::new(),
            "word 文😀e\u{301}\t ".repeat(800),
            "🇺🇸🇫🇷👩‍👩‍👧‍👦क्ष ".repeat(400),
            format!("{}e{}tail", "x".repeat(513), "\u{301}".repeat(1000)),
        ] {
            let index = VisualLineIndex::new(&body).unwrap();
            let boundaries = body
                .grapheme_indices(true)
                .map(|(byte, _)| byte)
                .chain(std::iter::once(body.len()))
                .collect::<Vec<_>>();
            let queries = (0..=body.len()).collect::<Vec<_>>();
            for queries in queries.chunks(1024) {
                let expected = queries
                    .iter()
                    .filter_map(|byte| boundaries.binary_search(byte).ok())
                    .collect::<Vec<_>>();
                assert_eq!(index.boundary_glyphs(&body, queries), Some(expected));
            }
            assert_eq!(
                index.boundary_glyphs(&body, &[0, 0, body.len(), body.len()]),
                Some(vec![0, 0, boundaries.len() - 1, boundaries.len() - 1])
            );
            let sparse = boundaries.iter().copied().step_by(97).collect::<Vec<_>>();
            assert_eq!(
                index.boundary_glyphs(&body, &sparse),
                Some((0..boundaries.len()).step_by(97).collect())
            );
            assert_eq!(index.boundary_glyphs(&body, &[]), Some(Vec::new()));
            assert!(index.boundary_glyphs(&body, &[body.len() + 1]).is_none());
            assert!(index.boundary_glyphs(&body, &[1, 0]).is_none());
            assert!(index.boundary_glyphs("mismatched length", &[0]).is_none());
            assert!(
                index
                    .boundary_glyphs(&body, &vec![0; super::super::MAX_ROW_GEOMETRY_ANCHORS + 1])
                    .is_none()
            );
        }
    }

    #[test]
    fn tab_metadata_retains_exact_source_snapshot() {
        let prefix = "文😀e\u{301}".repeat(10000);
        for (body, tabs) in [
            (String::new(), false),
            ("short\trow".into(), true),
            (prefix.clone(), false),
            (format!("\t{prefix}"), true),
            (format!("{prefix}\t"), true),
            (format!("{prefix}\t{prefix}"), true),
        ] {
            let index = VisualLineIndex::new(&body).unwrap();
            let retained = index.clone();
            assert_eq!(index.has_tabs(), tabs);
            assert_eq!(retained.has_tabs(), tabs);
            assert!(index.shared_with(&retained));
        }
        let original = VisualLineIndex::new(&prefix).unwrap();
        let changed = VisualLineIndex::new(&format!("{prefix}\t")).unwrap();
        assert!(!original.has_tabs());
        assert!(changed.has_tabs());
        assert!(!original.shared_with(&changed));
    }

    #[test]
    fn probe_anchor_ranges_match_complete_anchor_filtering() {
        for body in [
            String::new(),
            "x".into(),
            "x".repeat(513),
            "word 文😀e\u{301} ".repeat(10000),
            format!("{}e{}tail", "x".repeat(513), "\u{301}".repeat(1000)),
            "x".repeat(crate::editor::MAX_EDITOR_LINE_BYTES),
        ] {
            let index = VisualLineIndex::new(&body).unwrap();
            let anchors = index.anchor_glyphs().collect::<Vec<_>>();
            let mut boundaries = anchors.clone();
            boundaries.extend([0, 1, index.len() - 1, index.len(), usize::MAX]);
            for anchor in anchors.iter().step_by(127) {
                boundaries.extend([anchor.saturating_sub(1), anchor + 1]);
            }
            boundaries.sort_unstable();
            boundaries.dedup();
            for (at, start) in boundaries.iter().copied().enumerate() {
                for end in boundaries.iter().copied().skip(at).step_by(31) {
                    let range = start..end;
                    assert_eq!(
                        index.anchor_glyphs_in(range.clone()).collect::<Vec<_>>(),
                        anchors
                            .iter()
                            .copied()
                            .filter(|glyph| range.contains(glyph))
                            .collect::<Vec<_>>()
                    );
                }
                assert_eq!(index.anchor_glyphs_in(start..start).count(), 0);
            }
            assert_eq!(
                index
                    .anchor_glyphs_in(std::ops::Range {
                        start: usize::MAX,
                        end: 0
                    })
                    .count(),
                0
            );
        }
    }
    #[test]
    fn ordered_positions_match_complete_unicode_coordinates() {
        for body in [
            "文😀e\u{301}\t words ".repeat(2000),
            "🇺🇸🇫🇷🇦🇺👩‍👩‍👧‍👦क्ष abc ".repeat(2000),
            format!("{}e{}tail", "x".repeat(513), "\u{301}".repeat(1000)),
            "\r literal ".repeat(2000),
            String::new(),
        ] {
            let index = VisualLineIndex::new(&body).unwrap();
            let expected = crate::editor::visual_line_offsets(&body).unwrap();
            for mut targets in [
                (0..expected.len()).collect::<Vec<_>>(),
                (0..expected.len()).step_by(97).collect(),
                vec![0, 0, expected.len() - 1, expected.len() - 1],
                Vec::new(),
            ] {
                targets.sort_unstable();
                assert_eq!(
                    index.positions(&body, &targets),
                    Some(targets.iter().map(|target| expected[*target]).collect())
                );
            }
            assert!(index.positions(&body, &[expected.len()]).is_none());
            if !body.is_empty() {
                assert!(index.positions(&body, &[1, 0]).is_none());
            }
            assert!(index.positions("mismatched length", &[0]).is_none());
            assert!(
                index
                    .positions(&body, &vec![0; super::super::MAX_VISUAL_CARETS + 1])
                    .is_none()
            );
        }
    }

    #[test]
    fn sparse_glyph_queries_match_complete_cluster_coordinates() {
        for body in [
            "文😀e\u{301}\t words ".repeat(7000),
            "🇺🇸🇫🇷🇦🇺👩‍👩‍👧‍👦क्ष abc ".repeat(3000),
            format!("{}e{}tail", "x".repeat(513), "\u{301}".repeat(1000)),
            "\r literal ".repeat(7000),
            String::new(),
        ] {
            let index = VisualLineIndex::new(&body).unwrap();
            let expected = crate::editor::visual_line_offsets(&body).unwrap();
            let run_starts = crate::editor::visual_text_run_ranges(&body)
                .map(|run| run.start)
                .chain(std::iter::once(body.len()))
                .collect::<Vec<_>>();
            for byte in 0..=body.len() {
                assert_eq!(
                    index.is_text_run_boundary(byte),
                    run_starts.binary_search(&byte).is_ok()
                );
            }
            assert_eq!(index.len(), expected.len());
            for (glyph, &(byte, native)) in expected.iter().enumerate().step_by(31) {
                assert_eq!(index.at(&body, glyph), Some((byte, native)));
                assert_eq!(index.index_at_byte(&body, byte), Some(glyph));
                if let Some(&(next, _)) = expected.get(glyph + 1) {
                    for within in byte..next {
                        assert_eq!(index.index_at_byte(&body, within), Some(glyph));
                    }
                }
            }
            assert_eq!(
                index.at(&body, expected.len() - 1),
                expected.last().copied()
            );
            assert_eq!(
                index.index_at_byte(&body, body.len()),
                Some(expected.len() - 1)
            );
            assert!(index.at(&body, expected.len()).is_none());
            assert!(index.at("mismatched length", 0).is_none());
            assert_eq!(
                index.horizontal_paint_bounds(1000.0, 400.0),
                crate::editor::horizontal_paint_bounds(&body, 1000.0, 400.0)
            );
        }
        let ascii = "x".repeat(crate::editor::MAX_EDITOR_LINE_BYTES);
        let index = VisualLineIndex::new(&ascii).unwrap();
        assert_eq!(index.len(), ascii.len() + 1);
        assert!(index.retained_bytes() < 128 * 1024);
        assert!(
            VisualLineIndex::new(&"x".repeat(crate::editor::MAX_STRUCTURE_BYTES + 1)).is_none()
        );
    }
}
