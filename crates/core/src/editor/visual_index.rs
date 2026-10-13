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
    word_runs: Vec<WordRun>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WordRun {
    start: usize,
    end: usize,
    kind: u8,
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
    whitespace: bool,
    word: bool,
    word_run: Option<WordRun>,
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
                word_runs: Vec::new(),
            },
            cluster: Position::default(),
            whitespace: true,
            word: false,
            word_run: None,
        })
    }
    /// Budget counts bounded source/context scan units, including suspensions
    /// within large graphemes. No partial index authorizes coordinates or paint.
    pub fn advance(&mut self, budget: usize) -> bool {
        self.advance_with_source_scan(budget, |_, _| {})
    }
    #[cfg(test)]
    pub(super) fn advance_with_character_scan(
        &mut self,
        budget: usize,
        mut visit: impl FnMut(usize, char),
    ) -> bool {
        let body = self.body;
        self.advance_with_source_scan(budget, |range, _| {
            for (offset, ch) in body[range.clone()].char_indices() {
                visit(range.start + offset, ch);
            }
        })
    }
    pub(super) fn advance_with_source_scan(
        &mut self,
        budget: usize,
        mut visit: impl FnMut(std::ops::Range<usize>, bool),
    ) -> bool {
        for _ in 0..budget {
            if self.scan.is_done() {
                break;
            }
            let coordinates = &mut self.coordinates;
            let cluster = &mut self.cluster;
            let body = self.body;
            let whitespace = &mut self.whitespace;
            let word = &mut self.word;
            let word_run = &mut self.word_run;
            self.scan.advance_spans(|step| {
                visit(step.consumed.clone(), step.ascii);
                if step.ascii {
                    let mut start = step.consumed.start;
                    while start < step.consumed.end {
                        let bytes = &body.as_bytes()[start..step.consumed.end];
                        let kind = ascii_word_kind(bytes[0]);
                        let length = bytes
                            .iter()
                            .position(|byte| ascii_word_kind(*byte) != kind)
                            .unwrap_or(bytes.len());
                        record_ascii(coordinates, cluster, word_run, length, kind);
                        start += length;
                    }
                    *whitespace = true;
                    *word = false;
                    return;
                }
                for ch in body[step.consumed].chars() {
                    coordinates.end.native += ch.len_utf16();
                    coordinates.tabs |= ch == '\t';
                    *whitespace &= ch.is_whitespace();
                    *word |= ch.is_alphanumeric() || ch == '_';
                    if coordinates.horizontal && !super::viewport::horizontal_paint_character(ch) {
                        coordinates.horizontal = false;
                    }
                }
                if let Some(end) = step.boundary {
                    let kind = if *whitespace {
                        0
                    } else if *word {
                        1
                    } else {
                        2
                    };
                    record_boundary(coordinates, cluster, word_run, end, kind);
                    *whitespace = true;
                    *word = false;
                }
            });
        }
        self.scan.is_done()
    }
    pub fn finish(mut self) -> Option<VisualLineIndex> {
        if !self.scan.is_done() {
            return None;
        }
        self.coordinates.end.byte = self.body.len();
        if let Some(run) = self
            .word_run
            .filter(|run| run.end - run.start >= STEP_BYTES)
        {
            self.coordinates.word_runs.push(run);
        }
        if self.coordinates.points.last()?.byte != self.body.len() {
            self.coordinates.points.push(self.coordinates.end);
        }
        Some(VisualLineIndex(Arc::new(self.coordinates)))
    }
}

fn record_boundary(
    coordinates: &mut Coordinates,
    cluster: &mut Position,
    word_run: &mut Option<WordRun>,
    end: usize,
    kind: u8,
) {
    record_word(coordinates, word_run, cluster.byte, end, kind);
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

fn ascii_word_kind(byte: u8) -> u8 {
    if byte == b' ' {
        0
    } else if byte.is_ascii_alphanumeric() || byte == b'_' {
        1
    } else {
        2
    }
}

fn record_ascii(
    coordinates: &mut Coordinates,
    cluster: &mut Position,
    word_run: &mut Option<WordRun>,
    length: usize,
    kind: u8,
) {
    let end = cluster.byte + length;
    record_word(coordinates, word_run, cluster.byte, end, kind);
    let mut byte = cluster
        .byte
        .max(coordinates.points.last().unwrap().byte + STEP_BYTES);
    while byte < end {
        let offset = byte - cluster.byte;
        coordinates.points.push(Position {
            byte,
            native: cluster.native + offset,
            glyph: cluster.glyph + offset,
        });
        byte += STEP_BYTES;
    }
    coordinates.end.native += length;
    coordinates.end.glyph += length;
    *cluster = Position {
        byte: end,
        ..coordinates.end
    };
}

fn record_word(
    coordinates: &mut Coordinates,
    word_run: &mut Option<WordRun>,
    start: usize,
    end: usize,
    kind: u8,
) {
    if word_run.as_ref().is_some_and(|run| run.kind == kind) {
        word_run.as_mut().unwrap().end = end;
    } else {
        if let Some(run) = word_run
            .take()
            .filter(|run| run.end - run.start >= STEP_BYTES)
        {
            coordinates.word_runs.push(run);
        }
        *word_run = Some(WordRun { start, end, kind });
    }
}

/// Immutable coordinates for one exact logical-line body. Callers retain the
/// associated source snapshot and must supply that same body to lookup methods.
/// Unchanged document rows and folded projections share the sparse allocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisualLineIndex(Arc<Coordinates>);
impl VisualLineIndex {
    fn word_step(&self, body: &str, cursor: usize, left: bool) -> Option<(usize, u8)> {
        let key = if left { cursor.checked_sub(1)? } else { cursor };
        let run = self
            .0
            .word_runs
            .partition_point(|run| run.start <= key)
            .checked_sub(1)
            .and_then(|index| self.0.word_runs.get(index))
            .filter(|run| key < run.end);
        if let Some(run) = run {
            Some((if left { run.start } else { run.end }, run.kind))
        } else if left {
            let (byte, glyph) = body[..cursor].grapheme_indices(true).next_back()?;
            Some((byte, super::motion::category(glyph)))
        } else {
            let glyph = body[cursor..].graphemes(true).next()?;
            Some((cursor + glyph.len(), super::motion::category(glyph)))
        }
    }

    /// Exact double-click unit, including whitespace and the final unit at EOF.
    pub(super) fn word_selection(&self, body: &str, at: usize) -> Option<std::ops::Range<usize>> {
        let glyph = self.index_at_byte(body, at)?;
        if body.is_empty() {
            return Some(0..0);
        }
        let glyph = if at == body.len() {
            glyph.checked_sub(1)?
        } else {
            glyph
        };
        let mut start = self.at(body, glyph)?.0;
        let (mut end, kind) = self.word_step(body, start, false)?;
        while start > 0 {
            let (boundary, previous) = self.word_step(body, start, true)?;
            if previous != kind {
                break;
            }
            start = boundary;
        }
        while end < body.len() {
            let (boundary, next) = self.word_step(body, end, false)?;
            if next != kind {
                break;
            }
            end = boundary;
        }
        Some(start..end)
    }
    /// Skip long category runs; short runs retain the original grapheme rules.
    /// A noncanonical byte caret uses the caller's complete-source fallback.
    pub(super) fn word_boundary(
        &self,
        body: &str,
        at: usize,
        left: bool,
    ) -> Option<(usize, Option<u8>)> {
        let glyph = self.index_at_byte(body, at)?;
        if self.at(body, glyph)?.0 != at {
            return None;
        }
        let mut cursor = at;
        let mut kind = None;
        while if left {
            cursor > 0
        } else {
            cursor < body.len()
        } {
            let (boundary, current) = self.word_step(body, cursor, left)?;
            if kind.is_some_and(|kind| kind != current) && (left || current != 0) {
                break;
            }
            cursor = boundary;
            if !left || current != 0 {
                kind = Some(current);
            }
        }
        Some((cursor, kind))
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn shared_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn new(body: &str) -> Option<Self> {
        let mut preparation = VisualLinePreparation::new(body)?;
        preparation.advance(usize::MAX);
        preparation.finish()
    }
    fn cached_word_run(&self, byte: usize) -> Option<&WordRun> {
        self.0
            .word_runs
            .partition_point(|run| run.start <= byte)
            .checked_sub(1)
            .and_then(|index| self.0.word_runs.get(index))
            .filter(|run| byte < run.end)
    }
    /// The document index proves both retained and inserted bodies are printable
    /// ASCII. Native/glyph coordinates then shift arithmetically; only category
    /// runs intersecting the edit need source inspection.
    pub(super) fn updated_ascii(
        &self,
        body: &str,
        changed: std::ops::Range<usize>,
        new_end: usize,
    ) -> Option<Self> {
        self.updated_ascii_with_cost(body, changed, new_end)
            .map(|(index, _)| index)
    }
    fn updated_ascii_with_cost(
        &self,
        body: &str,
        changed: std::ops::Range<usize>,
        new_end: usize,
    ) -> Option<(Self, usize)> {
        if body.len() > super::MAX_STRUCTURE_BYTES {
            return None;
        }
        let bytes = body.as_bytes();
        let mut inspected = 0;
        let mut start = changed.start.saturating_sub(1).min(body.len());
        if start < body.len() {
            let kind = ascii_word_kind(bytes[start]);
            while start > 0 && ascii_word_kind(bytes[start - 1]) == kind {
                if let Some(run) = self.cached_word_run(start - 1) {
                    start = run.start;
                    break;
                }
                start -= 1;
                inspected += 1;
            }
        }
        let mut end = new_end.saturating_add(1).min(body.len());
        if end > new_end && end < body.len() {
            let kind = ascii_word_kind(bytes[end - 1]);
            while end < body.len() && ascii_word_kind(bytes[end]) == kind {
                if let Some(run) = self.cached_word_run(changed.end + (end - new_end)) {
                    end = new_end + (run.end - changed.end);
                    break;
                }
                end += 1;
                inspected += 1;
            }
        }
        let old_end = changed.end + (end - new_end);
        // Retained runs plus inserted long runs and at most two boundary splits.
        let capacity =
            (self.0.word_runs.len() + (new_end - changed.start).div_ceil(STEP_BYTES) + 2)
                .min(body.len() / STEP_BYTES);
        let mut word_runs = Vec::with_capacity(capacity);
        word_runs.extend(
            self.0
                .word_runs
                .iter()
                .take_while(|run| run.end <= start)
                .copied(),
        );
        let mut word_run = None;
        let mut cursor = start;
        while cursor < end {
            let (old, boundary) = if cursor < changed.start {
                (Some(cursor), changed.start)
            } else if cursor >= new_end {
                (Some(changed.end + (cursor - new_end)), end)
            } else {
                (None, new_end)
            };
            let retained = old.and_then(|byte| self.cached_word_run(byte));
            let (next, kind) = if let Some(run) = retained {
                let next = if cursor < changed.start {
                    run.end
                } else {
                    new_end + (run.end - changed.end)
                };
                (next.min(boundary).min(end), run.kind)
            } else {
                let limit = boundary.min(end).min(cursor + STEP_BYTES);
                let kind = ascii_word_kind(bytes[cursor]);
                let count = bytes[cursor..limit]
                    .iter()
                    .position(|byte| ascii_word_kind(*byte) != kind)
                    .unwrap_or(limit - cursor);
                inspected += count;
                (cursor + count, kind)
            };
            if word_run
                .as_ref()
                .is_some_and(|run: &WordRun| run.kind == kind)
            {
                word_run.as_mut().unwrap().end = next;
            } else {
                if let Some(run) = word_run
                    .take()
                    .filter(|run: &WordRun| run.end - run.start >= STEP_BYTES)
                {
                    word_runs.push(run);
                }
                word_run = Some(WordRun {
                    start: cursor,
                    end: next,
                    kind,
                });
            }
            cursor = next;
        }
        if let Some(run) = word_run.filter(|run| run.end - run.start >= STEP_BYTES) {
            word_runs.push(run);
        }
        word_runs.extend(
            self.0
                .word_runs
                .iter()
                .filter(|run| run.start >= old_end)
                .map(|run| WordRun {
                    start: new_end + (run.start - changed.end),
                    end: new_end + (run.end - changed.end),
                    kind: run.kind,
                }),
        );
        Some((Self::ascii_coordinates(body.len(), word_runs), inspected))
    }
    fn ascii_coordinates(length: usize, word_runs: Vec<WordRun>) -> Self {
        let end_position = Position {
            byte: length,
            native: length,
            glyph: length,
        };
        // Include EOF up front so its append cannot double the sparse table.
        let mut points = Vec::with_capacity(length.div_ceil(STEP_BYTES) + 1);
        points.extend((0..length).step_by(STEP_BYTES).map(|byte| Position {
            byte,
            native: byte,
            glyph: byte,
        }));
        points.push(end_position);
        Self(Arc::new(Coordinates {
            points,
            long_clusters: Vec::new(),
            end: end_position,
            horizontal: length > super::MAX_MEASURE_BYTES,
            tabs: false,
            word_runs,
        }))
    }
    /// The document proves each retained slice and the inserted body are ASCII.
    /// Cached category runs skip retained bytes; uncached runs keep exact rules.
    pub(super) fn joined_ascii(
        body: &str,
        prefix: Option<&Self>,
        prefix_end: usize,
        suffix: Option<&Self>,
        suffix_start: usize,
        inserted_end: usize,
    ) -> Option<Self> {
        if body.len() > super::MAX_STRUCTURE_BYTES {
            return None;
        }
        let bytes = body.as_bytes();
        let mut start = prefix_end;
        if start > 0 {
            start -= 1;
            let kind = ascii_word_kind(bytes[start]);
            while start > 0 && ascii_word_kind(bytes[start - 1]) == kind {
                if let Some(run) = prefix.and_then(|index| index.cached_word_run(start - 1)) {
                    start = run.start;
                    break;
                }
                start -= 1;
            }
        }
        let mut end = inserted_end;
        if end < body.len() {
            end += 1;
            let kind = ascii_word_kind(bytes[end - 1]);
            while end < body.len() && ascii_word_kind(bytes[end]) == kind {
                if let Some(run) = suffix
                    .and_then(|index| index.cached_word_run(suffix_start + end - inserted_end))
                {
                    end = inserted_end + run.end - suffix_start;
                    break;
                }
                end += 1;
            }
        }
        let mut runs: Vec<_> = prefix
            .into_iter()
            .flat_map(|index| index.0.word_runs.iter())
            .take_while(|run| run.end <= start)
            .copied()
            .collect();
        let mut active: Option<WordRun> = None;
        let mut cursor = start;
        while cursor < end {
            let (index, old, boundary) = if cursor < prefix_end {
                (prefix, cursor, prefix_end)
            } else if cursor >= inserted_end {
                (suffix, suffix_start + cursor - inserted_end, end)
            } else {
                (None, 0, inserted_end)
            };
            let retained = index.and_then(|index| index.cached_word_run(old));
            let kind =
                retained.map_or_else(|| ascii_word_kind(body.as_bytes()[cursor]), |run| run.kind);
            let next = if let Some(run) = retained {
                (cursor + run.end - old).min(boundary)
            } else {
                let limit = boundary.min(cursor + STEP_BYTES);
                cursor
                    + body.as_bytes()[cursor..limit]
                        .iter()
                        .position(|byte| ascii_word_kind(*byte) != kind)
                        .unwrap_or(limit - cursor)
            };
            if active.as_ref().is_some_and(|run| run.kind == kind) {
                active.as_mut().unwrap().end = next;
            } else {
                if let Some(run) = active
                    .take()
                    .filter(|run| run.end - run.start >= STEP_BYTES)
                {
                    runs.push(run);
                }
                active = Some(WordRun {
                    start: cursor,
                    end: next,
                    kind,
                });
            }
            cursor = next;
        }
        if let Some(run) = active.filter(|run| run.end - run.start >= STEP_BYTES) {
            runs.push(run);
        }
        let old_suffix_end = suffix_start + end - inserted_end;
        runs.extend(
            suffix
                .into_iter()
                .flat_map(|index| index.0.word_runs.iter())
                .filter(|run| run.start >= old_suffix_end)
                .map(|run| WordRun {
                    start: inserted_end + run.start - suffix_start,
                    end: inserted_end + run.end - suffix_start,
                    kind: run.kind,
                }),
        );
        Some(Self::ascii_coordinates(body.len(), runs))
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
    #[test]
    fn joined_ascii_category_seams_match_complete_coordinates() {
        for left in ["a".repeat(70000), "word 123; ".repeat(7000)] {
            for right in ["a".repeat(70000), "tail _; ".repeat(9000)] {
                let prefix = VisualLineIndex::new(&left).unwrap();
                let suffix = VisualLineIndex::new(&right).unwrap();
                for prefix_end in [0, 511, 512, left.len() - 1, left.len()] {
                    for suffix_start in [0, 511, 512, right.len() - 1, right.len()] {
                        for inserted in ["", "a", " ; "] {
                            let body = format!(
                                "{}{inserted}{}",
                                &left[..prefix_end],
                                &right[suffix_start..]
                            );
                            let joined = VisualLineIndex::joined_ascii(
                                &body,
                                Some(&prefix),
                                prefix_end,
                                Some(&suffix),
                                suffix_start,
                                prefix_end + inserted.len(),
                            )
                            .unwrap();
                            assert_eq!(
                                joined,
                                VisualLineIndex::new(&body).unwrap(),
                                "prefix={prefix_end}, suffix={suffix_start}, inserted={inserted:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn incremental_ascii_tables_match_full_rebuilds_without_scanning_long_retained_runs() {
        for old in [
            "a".repeat(511),
            "a".repeat(512),
            "a".repeat(513),
            "word 123; ".repeat(7000),
            format!(
                "{} {}!{}",
                "a".repeat(20000),
                "b".repeat(30000),
                ";".repeat(20000)
            ),
            "x".repeat(1_000_000),
        ] {
            let previous = VisualLineIndex::new(&old).unwrap();
            for at in [0, 1, old.len() / 2, old.len() - 1, old.len()] {
                for deleted in [0, 1, 510, 513, old.len() - at] {
                    let end = at.saturating_add(deleted).min(old.len());
                    for inserted in ["", "a", ".", " x; "] {
                        let mut body = old.clone();
                        body.replace_range(at..end, inserted);
                        let (actual, inspected) = previous
                            .updated_ascii_with_cost(&body, at..end, at + inserted.len())
                            .unwrap();
                        assert_eq!(
                            actual,
                            VisualLineIndex::new(&body).unwrap(),
                            "at={at}, end={end}, inserted={inserted:?}, old length={}",
                            old.len()
                        );
                        if old.len() == 1_000_000 {
                            assert!(inspected < 1024, "inspected {inspected}");
                        }
                    }
                }
            }
            assert_eq!(previous, VisualLineIndex::new(&old).unwrap());
        }
    }

    use super::*;
    #[test]
    fn ascii_groups_keep_the_complete_scalar_coordinate_table() {
        for body in [
            "words _VALUE123 ! ".repeat(5000),
            "x".repeat(1025),
            "!x".repeat(5000),
            format!("{}a\u{301} {}", "x".repeat(511), "y".repeat(1025)),
            "ASCII 文😀e\u{301}\t\r words ".repeat(1000),
            format!("{}ab {}", "\u{0600}".repeat(600), "x".repeat(2000)),
        ] {
            let mut reference = Coordinates {
                points: vec![Position::default()],
                long_clusters: Vec::new(),
                end: Position::default(),
                horizontal: body.len() > super::super::MAX_MEASURE_BYTES,
                tabs: false,
                word_runs: Vec::new(),
            };
            let mut cluster = Position::default();
            let mut word_run = None;
            for (byte, text) in body.grapheme_indices(true) {
                for ch in text.chars() {
                    reference.end.native += ch.len_utf16();
                    reference.tabs |= ch == '\t';
                    reference.horizontal &= super::super::viewport::horizontal_paint_character(ch);
                }
                let kind = if text.chars().all(char::is_whitespace) {
                    0
                } else if text.chars().any(|ch| ch.is_alphanumeric() || ch == '_') {
                    1
                } else {
                    2
                };
                record_boundary(
                    &mut reference,
                    &mut cluster,
                    &mut word_run,
                    byte + text.len(),
                    kind,
                );
            }
            if let Some(run) = word_run.filter(|run| run.end - run.start >= STEP_BYTES) {
                reference.word_runs.push(run);
            }
            reference.end.byte = body.len();
            if reference.points.last().unwrap().byte != body.len() {
                reference.points.push(reference.end);
            }
            for budget in [1, 8, 64] {
                let mut preparation = VisualLinePreparation::new(&body).unwrap();
                while !preparation.advance(budget) {}
                assert_eq!(*preparation.finish().unwrap().0, reference);
            }
        }
    }
    #[test]
    fn word_run_summaries_are_sparse_cooperative_and_complete_only() {
        for body in [
            "x".repeat(100_000),
            "!x".repeat(50_000),
            format!("a{}", "\u{301}".repeat(40_000)),
        ] {
            let mut preparation = VisualLinePreparation::new(&body).unwrap();
            assert!(!preparation.advance(1));
            assert!(preparation.finish().is_none());
            let mut preparation = VisualLinePreparation::new(&body).unwrap();
            let mut batches = 0;
            while !preparation.advance(1) {
                batches += 1;
                assert!(batches < 10_000);
            }
            assert!(batches > 1);
            let index = preparation.finish().unwrap();
            assert_eq!(index, VisualLineIndex::new(&body).unwrap());
            assert!(index.0.word_runs.len() <= body.len() / STEP_BYTES);
            if body.starts_with('a') || body.starts_with('x') {
                assert_eq!(index.0.word_runs.len(), 1);
                assert_eq!(
                    index.word_boundary(&body, 0, false),
                    Some((body.len(), Some(1)))
                );
                assert_eq!(
                    index.word_boundary(&body, body.len(), true),
                    Some((0, Some(1)))
                );
            } else {
                assert!(index.0.word_runs.is_empty());
            }
        }
    }
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
