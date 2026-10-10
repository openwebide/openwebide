//! Shared ownership and exact paint comparison for incremental row measurements.
use super::EditorActions;
use crate::state::workspace::EditorRowPaint;
use leptos::prelude::*;
use openwebide_core::{
    editor::{FoldProjection, Indentation, RowMeasurementPlan},
    highlight::Token,
};
use std::{
    hash::{Hash, Hasher},
    sync::Arc,
};

struct PaintRow<'a> {
    tokens: &'a [Token],
    plain: Option<&'a str>,
    guide: usize,
    normalize_cr: bool,
    ending: bool,
}
impl PartialEq for PaintRow<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.plain == other.plain
            && self.guide == other.guide
            && self.ending == other.ending
            && self.tokens.len() == other.tokens.len()
            && self
                .tokens
                .iter()
                .zip(other.tokens)
                .enumerate()
                .all(|(index, (a, b))| {
                    let last = index + 1 == self.tokens.len();
                    let a_text = if last && self.normalize_cr {
                        a.text.strip_suffix('\r').unwrap_or(&a.text)
                    } else {
                        &a.text
                    };
                    let b_text = if last && other.normalize_cr {
                        b.text.strip_suffix('\r').unwrap_or(&b.text)
                    } else {
                        &b.text
                    };
                    a.kind == b.kind && a_text == b_text
                })
    }
}
impl Eq for PaintRow<'_> {}
impl Hash for PaintRow<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.plain.hash(state);
        self.guide.hash(state);
        self.ending.hash(state);
        self.tokens.len().hash(state);
        for (index, token) in self.tokens.iter().enumerate() {
            std::mem::discriminant(&token.kind).hash(state);
            let text = if self.normalize_cr && index + 1 == self.tokens.len() {
                token.text.strip_suffix('\r').unwrap_or(&token.text)
            } else {
                &token.text
            };
            text.hash(state);
        }
    }
}
fn paint_row(paint: &EditorRowPaint, index: usize) -> Option<PaintRow<'_>> {
    let source = paint.projection.lines().get(index)?.source_line;
    let mut tokens = match paint.tokens.get(source) {
        Some(tokens) => tokens.as_ref(),
        None if !paint.prepared_source => &[],
        None => return None,
    };
    let normalize_cr = paint.prepared_source && source + 1 < paint.tokens.len();
    let plain = if let [token] = tokens
        && token.kind == openwebide_core::highlight::TokenKind::Plain
    {
        // Both render paths call paint_text once with the same row body. Keep
        // multiple token runs distinct: their span boundaries can affect shaping.
        let body = if normalize_cr {
            token.text.strip_suffix('\r').unwrap_or(&token.text)
        } else {
            &token.text
        };
        tokens = &[];
        Some(body)
    } else if !paint.prepared_source && paint.tokens.get(source).is_none() {
        Some(paint.projection.line_body(index)?)
    } else {
        None
    };
    Some(PaintRow {
        tokens,
        plain,
        guide: paint.guides.get(source).copied().unwrap_or(0),
        normalize_cr,
        ending: index + 1 < paint.projection.lines().len(),
    })
}
fn paint_rows(paint: &EditorRowPaint) -> Option<Vec<PaintRow<'_>>> {
    (0..paint.projection.lines().len())
        .map(|index| paint_row(paint, index))
        .collect()
}
fn paint_prefix_end(old: &PaintRow<'_>, new: &PaintRow<'_>) -> usize {
    if old.guide != new.guide || old.ending != new.ending {
        return 0;
    }
    if let (Some(old), Some(new)) = (old.plain, new.plain) {
        return old.len().min(new.len());
    }
    if old.plain.is_some() || new.plain.is_some() || old.normalize_cr != new.normalize_cr {
        return 0;
    }
    let mut end = 0;
    for (old, new) in old.tokens.iter().zip(new.tokens) {
        if old.kind != new.kind || (old.text.len() > 512) != (new.text.len() > 512) {
            break;
        }
        let same = old
            .text
            .bytes()
            .zip(new.text.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        end += same;
        if same != old.text.len() || same != new.text.len() {
            break;
        }
    }
    end
}
fn paint_suffix_start(old: &PaintRow<'_>, new: &PaintRow<'_>) -> Option<usize> {
    if old.guide != new.guide || old.ending != new.ending {
        return None;
    }
    if old.plain.is_some() && new.plain.is_some() {
        return Some(0);
    }
    if old.plain.is_some() || new.plain.is_some() || old.normalize_cr != new.normalize_cr {
        return None;
    }
    fn text<'a>(row: &PaintRow<'_>, token: &'a Token, last: bool) -> &'a str {
        if row.normalize_cr && last {
            token.text.strip_suffix('\r').unwrap_or(&token.text)
        } else {
            &token.text
        }
    }
    let length = |row: &PaintRow<'_>| {
        row.tokens
            .iter()
            .enumerate()
            .map(|(index, token)| text(row, token, index + 1 == row.tokens.len()).len())
            .sum::<usize>()
    };
    let mut start = length(new);
    for (index, (a, b)) in old
        .tokens
        .iter()
        .rev()
        .zip(new.tokens.iter().rev())
        .enumerate()
    {
        let a_text = text(old, a, index == 0);
        let b_text = text(new, b, index == 0);
        if a.kind != b.kind || (a_text.len() > 512) != (b_text.len() > 512) {
            break;
        }
        let same = a_text
            .bytes()
            .rev()
            .zip(b_text.bytes().rev())
            .take_while(|(a, b)| a == b)
            .count();
        start -= same;
        if same != a_text.len() || same != b_text.len() {
            break;
        }
    }
    Some(start)
}
const MAX_PAINT_RUN_ROWS: usize = 8;
const MAX_RETAINED_PAINT_RUNS: usize = 16 * 1024;
#[cfg(feature = "test-support")]
thread_local! { static PAINT_RUN_PREPARATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(feature = "test-support")]
thread_local! { static PAINT_RUN_SEGMENT_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(feature = "test-support")]
pub(super) fn take_paint_run_segment_bytes() -> usize {
    PAINT_RUN_SEGMENT_BYTES.replace(0)
}
fn cacheable_styled_row(paint: &EditorRowPaint, row: usize) -> Option<(&str, PaintRow<'_>)> {
    let body = paint.projection.line_body(row)?;
    if body.len() <= openwebide_core::editor::MAX_MEASURE_BYTES
        || body.len() > openwebide_core::editor::MAX_EDITOR_LINE_BYTES
    {
        return None;
    }
    let style = paint_row(paint, row)?;
    style.plain.is_none().then_some((body, style))
}
fn unwrapped_paragraph_row(
    paint: &EditorRowPaint,
    row: usize,
) -> Option<(&str, openwebide_core::editor::VisualLineIndex, PaintRow<'_>)> {
    let body = paint.projection.line_body(row)?;
    let index = paint.projection.visual_line_index(row)?;
    // Local overlap does not prove the complete paragraph's rounded tab grid.
    if index.has_tabs() {
        return None;
    }
    Some((body, index, paint_row(paint, row)?))
}
fn styled_paint_runs(body: &str, row: &PaintRow<'_>, max_runs: usize) -> Option<Arc<[usize]>> {
    if row.plain.is_some() {
        return None;
    }
    let mut preparation = openwebide_core::editor::PaintRunPreparation::new(
        row.tokens,
        body.len(),
        row.normalize_cr,
        max_runs,
    );
    preparation.advance(usize::MAX);
    #[cfg(feature = "test-support")]
    PAINT_RUN_SEGMENT_BYTES.set(PAINT_RUN_SEGMENT_BYTES.get() + preparation.segmented_bytes());
    preparation.finish()
}
fn same_measurement_environment(old: &EditorRowPaint, paint: &EditorRowPaint) -> bool {
    old.font_epoch == paint.font_epoch
        && old.key == paint.key
        && old.epoch == paint.epoch
        && old.read_revision == paint.read_revision
        && old.account_generation == paint.account_generation
        && old.metrics == paint.metrics
        && old.indentation == paint.indentation
        && old.whitespace == paint.whitespace
        && old.word_wrap == paint.word_wrap
}
fn same_paint_scope(old: &EditorRowPaint, paint: &EditorRowPaint) -> bool {
    old.view_revision == paint.view_revision
        && old.layout_epoch == paint.layout_epoch
        && same_measurement_environment(old, paint)
        && old.prepared_source == paint.prepared_source
        && Arc::ptr_eq(&old.tokens, &paint.tokens)
        && Arc::ptr_eq(&old.guides, &paint.guides)
}
fn same_paint_runs(old: &EditorRowPaint, paint: &EditorRowPaint) -> bool {
    same_measurement_environment(old, paint)
        && old.view_revision == paint.view_revision
        && old.prepared_source == paint.prepared_source
        && old.projection.shares_text_version(&paint.projection)
        && Arc::ptr_eq(&old.tokens, &paint.tokens)
        && Arc::ptr_eq(&old.guides, &paint.guides)
}
/// Exact source interval selected from immutable styled anchors before HTML generation.
#[derive(Clone, Debug)]
pub struct EditorRowSourceSlice {
    pub source_line: usize,
    pub bytes: std::ops::Range<usize>,
    pub native_start: usize,
    pub reaches_end: bool,
    /// The continuation plan or plain source index proved this endpoint is an
    /// original paint-run boundary; other viewport slices make no declaration.
    pub starts_paint_run: bool,
    /// Immutable original text-run ends for exact clipping at arbitrary source seams.
    pub paint_runs: Option<Arc<[usize]>>,
}
/// Immutable source/style proof prepared once for yielding prefix replay.
pub struct EditorParagraphPrefix {
    scope: EditorRowPaint,
    previous: EditorRowPaint,
    row: usize,
    measurements: Arc<openwebide_core::editor::ParagraphMeasurements>,
    style_end: usize,
}
/// Immutable source/style proof for an eligible unchanged paragraph suffix.
pub struct EditorParagraphSuffix {
    scope: EditorRowPaint,
    previous: EditorRowPaint,
    row: usize,
    measurements: Arc<openwebide_core::editor::ParagraphMeasurements>,
    style_start: usize,
}
impl EditorActions {
    pub fn prepare_wrapped_paragraph<'a>(
        self,
        paint: &'a EditorRowPaint,
        row: usize,
    ) -> Option<openwebide_core::editor::WrappedParagraphPreparation<'a>> {
        if !self.row_paint_current(paint) {
            return None;
        }
        let body = paint.projection.line_body(row)?;
        let index = paint.projection.visual_line_index(row)?;
        let runs = if paint_row(paint, row)?.plain.is_some() {
            index.text_run_boundaries().collect()
        } else {
            self.cached_styled_paint_runs(paint, row)?
        };
        openwebide_core::editor::WrappedParagraphPreparation::new(body, index, runs)
    }
    pub async fn prepare_wrapped_paragraph_cooperatively<'a>(
        self,
        paint: &'a EditorRowPaint,
        row: usize,
    ) -> Option<openwebide_core::editor::WrappedParagraphPreparation<'a>> {
        if !self.prepare_styled_paint_runs(paint, row).await || !self.row_paint_current(paint) {
            return None;
        }
        let body = paint.projection.line_body(row)?;
        let index = paint.projection.visual_line_index(row)?;
        let runs = if paint_row(paint, row)?.plain.is_some() {
            index.text_run_boundaries().collect()
        } else {
            self.cached_styled_paint_runs(paint, row)?
        };
        let preparation =
            openwebide_core::editor::ParagraphAnchorPreparation::wrapped(body, index, runs)?;
        openwebide_core::editor::WrappedParagraphPreparation::with_prepared_anchors(
            self.prepare_paragraph_anchors(paint, preparation).await?,
        )
    }
    async fn prepare_paragraph_anchors<'a>(
        self,
        paint: &EditorRowPaint,
        mut preparation: openwebide_core::editor::ParagraphAnchorPreparation<'a>,
    ) -> Option<openwebide_core::editor::PreparedParagraphAnchors<'a>> {
        loop {
            if !self.row_paint_current(paint) {
                return None;
            }
            if preparation.advance(openwebide_core::editor::PAINT_RUN_BATCH_UNITS) {
                return preparation.finish();
            }
            crate::util::yield_task().await;
        }
    }
    pub fn paragraph_measurements(
        paint: &EditorRowPaint,
        row: usize,
    ) -> Option<openwebide_core::editor::ParagraphMeasurementPlan<'_>> {
        use openwebide_core::editor::ParagraphMeasurementPlan;
        let (body, index, row) = unwrapped_paragraph_row(paint, row)?;
        if let Some(plain) = row.plain {
            return (plain == body)
                .then(|| ParagraphMeasurementPlan::new(body, index))
                .flatten();
        }
        let runs = styled_paint_runs(body, &row, usize::MAX)?;
        ParagraphMeasurementPlan::with_shared_run_boundaries(body, index, runs)
    }
    /// DOM preparation and viewport paint share the same original styled runs.
    /// Cached boundaries publish no dimensions or partially measured geometry.
    pub fn prepare_paragraph_measurements<'a>(
        self,
        paint: &'a EditorRowPaint,
        row: usize,
    ) -> Option<openwebide_core::editor::ParagraphMeasurementPlan<'a>> {
        if !self.row_paint_current(paint) {
            return None;
        }
        let (body, index, _) = unwrapped_paragraph_row(paint, row)?;
        if let Some(runs) = self.cached_styled_paint_runs(paint, row) {
            return openwebide_core::editor::ParagraphMeasurementPlan::with_shared_run_boundaries(
                body, index, runs,
            );
        }
        Self::paragraph_measurements(paint, row)
    }

    pub fn resume_paragraph_measurements(
        self,
        paint: &EditorRowPaint,
        row: usize,
        plan: &mut impl openwebide_core::editor::ParagraphReplay,
    ) -> usize {
        self.paragraph_prefix(paint, row).map_or(0, |prefix| {
            self.resume_paragraph_prefix_limit(&prefix, plan, usize::MAX)
        })
    }
    pub fn paragraph_prefix(
        self,
        paint: &EditorRowPaint,
        row: usize,
    ) -> Option<EditorParagraphPrefix> {
        if !self.row_paint_current(paint) {
            return None;
        }
        self.workspace
            .editor_paragraph_cache
            .with_untracked(|cache| {
                let cache = cache
                    .as_ref()
                    .filter(|cache| same_measurement_environment(&cache.paint, paint))?;
                let (_, measurements) = cache.rows.iter().find(|(index, _)| *index == row)?;
                let (old, new) = paint_row(&cache.paint, row).zip(paint_row(paint, row))?;
                Some(EditorParagraphPrefix {
                    scope: paint.clone(),
                    previous: cache.paint.clone(),
                    row,
                    measurements: measurements.clone(),
                    style_end: paint_prefix_end(&old, &new),
                })
            })
    }
    pub fn resume_paragraph_prefix_batch(
        self,
        prefix: &EditorParagraphPrefix,
        plan: &mut impl openwebide_core::editor::ParagraphReplay,
    ) -> usize {
        self.resume_paragraph_prefix_limit(
            prefix,
            plan,
            openwebide_core::editor::MAX_MEASURE_BATCHES_PER_FRAME,
        )
    }
    fn resume_paragraph_prefix_limit(
        self,
        prefix: &EditorParagraphPrefix,
        plan: &mut impl openwebide_core::editor::ParagraphReplay,
        limit: usize,
    ) -> usize {
        if !self.row_paint_current(&prefix.scope) {
            return 0;
        }
        let Some(body) = prefix.previous.projection.line_body(prefix.row) else {
            return 0;
        };
        plan.reuse_prefix_batch(body, &prefix.measurements, prefix.style_end, limit)
    }
    pub fn paragraph_suffix(
        self,
        paint: &EditorRowPaint,
        row: usize,
    ) -> Option<EditorParagraphSuffix> {
        if !self.row_paint_current(paint) {
            return None;
        }
        self.workspace
            .editor_paragraph_cache
            .with_untracked(|cache| {
                let cache = cache
                    .as_ref()
                    .filter(|cache| same_measurement_environment(&cache.paint, paint))?;
                let (_, measurements) = cache.rows.iter().find(|(index, _)| *index == row)?;
                let (old, new) = paint_row(&cache.paint, row).zip(paint_row(paint, row))?;
                let style_start = paint_suffix_start(&old, &new)?;
                Some(EditorParagraphSuffix {
                    scope: paint.clone(),
                    previous: cache.paint.clone(),
                    row,
                    measurements: measurements.clone(),
                    style_start,
                })
            })
    }
    pub fn resume_paragraph_suffix(
        self,
        suffix: &EditorParagraphSuffix,
        plan: &mut impl openwebide_core::editor::ParagraphReplay,
    ) -> usize {
        self.resume_paragraph_suffix_limit(suffix, plan, usize::MAX)
    }
    pub fn resume_paragraph_suffix_batch(
        self,
        suffix: &EditorParagraphSuffix,
        plan: &mut impl openwebide_core::editor::ParagraphReplay,
    ) -> usize {
        self.resume_paragraph_suffix_limit(
            suffix,
            plan,
            openwebide_core::editor::MAX_MEASURE_BATCHES_PER_FRAME,
        )
    }
    fn resume_paragraph_suffix_limit(
        self,
        suffix: &EditorParagraphSuffix,
        plan: &mut impl openwebide_core::editor::ParagraphReplay,
        limit: usize,
    ) -> usize {
        if !self.row_paint_current(&suffix.scope) {
            return 0;
        }
        let Some(body) = suffix.previous.projection.line_body(suffix.row) else {
            return 0;
        };
        plan.reuse_suffix_batch(body, &suffix.measurements, suffix.style_start, limit)
    }
    pub fn retain_paragraph_measurements(
        self,
        paint: &EditorRowPaint,
        row: usize,
        measurements: openwebide_core::editor::ParagraphMeasurements,
    ) {
        if !self.row_paint_current(paint) {
            return;
        }
        self.workspace.editor_paragraph_cache.update(|cache| {
            // A pending plain paint must not evict the last styled measurements.
            // Consumers still require exact current source/style ownership; the
            // retained cache is only a candidate for validated incremental replay.
            if !paint.prepared_source
                && cache.as_ref().is_some_and(|old| {
                    old.paint.prepared_source && same_measurement_environment(&old.paint, paint)
                })
            {
                return;
            }
            let mut rows = cache
                .take()
                .filter(|old| same_measurement_environment(&old.paint, paint))
                .map_or_else(Vec::new, |old| {
                    old.rows
                        .into_iter()
                        .filter(|(index, _)| {
                            paint_row(&old.paint, *index)
                                .zip(paint_row(paint, *index))
                                .is_some_and(|(a, b)| a == b)
                        })
                        .collect()
                });
            rows.retain(|(index, _)| *index != row);
            if rows.len() == 8 {
                rows.remove(0);
            }
            rows.push((row, Arc::new(measurements)));
            *cache = Some(crate::state::workspace::EditorParagraphCache {
                paint: paint.clone(),
                rows,
            });
        });
    }
    pub fn paint_window(
        self,
        row: usize,
        line_height: f64,
        scroll: (f64, f64),
        size: (f64, f64),
    ) -> Option<openwebide_core::editor::RowPaintWindow> {
        self.wrapped_paint_window(row, line_height, scroll.1, size.1)
            .map(openwebide_core::editor::RowPaintWindow::Wrapped)
            .or_else(|| {
                self.horizontal_paint_bounds(row, scroll.0, size.0)
                    .map(openwebide_core::editor::RowPaintWindow::Horizontal)
            })
    }
    pub fn horizontal_paint_bounds(
        self,
        row: usize,
        scroll: f64,
        width: f64,
    ) -> Option<std::ops::Range<f64>> {
        if self.preferences().word_wrap {
            return None;
        }
        let projection = self.projection()?;
        projection
            .visual_line_index(row)?
            .horizontal_paint_bounds(scroll, width)
    }
    /// Shared selection of visual paint windows; DOM adapters supply exact row
    /// geometry and source boundaries, independent of the workspace transport.
    pub fn wrapped_paint_window(
        self,
        row: usize,
        line_height: f64,
        scroll: f64,
        viewport_height: f64,
    ) -> Option<openwebide_core::editor::EditorViewport> {
        if !self.preferences().word_wrap {
            return None;
        }
        let projection = self.projection()?;
        if row == 0
            && self.measured_rows().is_none()
            && let Some(coverage) = self.paragraph_coverage()
        {
            let window = openwebide_core::editor::wrapped_paint_window(
                projection.lines().get(row)?.source.len(),
                coverage.coverage.covered_height(),
                line_height,
                scroll,
                viewport_height,
            )?;
            coverage.coverage.source_interval(
                window.rows.start as f64 * line_height..window.rows.end as f64 * line_height,
            )?;
            return Some(window);
        }
        let rows = self
            .measured_rows()
            .map(|measured| measured.rows)
            .or_else(|| self.measured_prefix().map(|prefix| prefix.rows))?;
        let top = rows.top(row)?;
        openwebide_core::editor::wrapped_paint_window(
            projection.lines().get(row)?.source.len(),
            rows.top(row + 1)? - top,
            line_height,
            scroll - top,
            viewport_height,
        )
    }
    pub(super) fn row_paint_current(self, paint: &EditorRowPaint) -> bool {
        if self.workspace.content.is_disposed() || self.capacity.try_get_untracked().is_none() {
            return false;
        }
        self.preferences().word_wrap == paint.word_wrap
            && self.workspace.editor_font_epoch.get_untracked() == paint.font_epoch
            && self.workspace.editor_view_revision.get_untracked() == paint.view_revision
            && self.workspace.editor_layout_epoch.get_untracked() == paint.layout_epoch
            && self.key().as_ref() == Some(&paint.key)
            && self.workspace.pending_epoch.get_untracked() == paint.epoch
            && self.workspace.editor_read_revision.get_untracked() == paint.read_revision
            && self.auth.map_or(0, |auth| auth.generation.get_untracked())
                == paint.account_generation
    }
    /// Guide queries reuse the shared source document's indexed logical rows.
    pub fn indent_guides(self, indentation: Indentation) -> Arc<[usize]> {
        let Some(key) = self.key() else {
            return Arc::from([]);
        };
        self.workspace.content.with_untracked(|source| {
            self.workspace.editor_documents.with_untracked(|documents| {
                documents
                    .get(&key)
                    .filter(|document| document.text() == source)
                    .map_or_else(
                        || Arc::from([]),
                        |document| document.indent_guide_columns(indentation),
                    )
            })
        })
    }

    fn row_paint_snapshot(
        self,
        metrics: String,
        projection: FoldProjection,
        tokens: (bool, Arc<openwebide_core::highlight::TokenRows>),
        guides: Arc<[usize]>,
        indentation: Indentation,
        whitespace: bool,
    ) -> Option<EditorRowPaint> {
        Some(EditorRowPaint {
            view_revision: self.workspace.editor_view_revision.get_untracked(),
            layout_epoch: self.workspace.editor_layout_epoch.get_untracked(),
            font_epoch: self.workspace.editor_font_epoch.get_untracked(),
            key: self.key()?,
            epoch: self.workspace.pending_epoch.get_untracked(),
            read_revision: self.workspace.editor_read_revision.get_untracked(),
            account_generation: self.auth.map_or(0, |auth| auth.generation.get_untracked()),
            metrics,
            projection,
            tokens: tokens.1,
            prepared_source: tokens.0,
            guides,
            indentation,
            whitespace,
            word_wrap: self.preferences().word_wrap,
        })
    }
    pub fn prepare_row_measurements(
        self,
        metrics: String,
        projection: FoldProjection,
        tokens: (bool, Arc<openwebide_core::highlight::TokenRows>),
        guides: Arc<[usize]>,
        indentation: Indentation,
        whitespace: bool,
    ) -> Option<(EditorRowPaint, RowMeasurementPlan)> {
        if !self.full_row_paint_ready() {
            return None;
        }
        let paint =
            self.row_paint_snapshot(metrics, projection, tokens, guides, indentation, whitespace)?;
        let current_rows = paint_rows(&paint)?;
        let reused = self.workspace.editor_row_cache.with_untracked(|cache| {
            let cache = cache.as_ref()?;
            let old = &cache.paint;
            if !same_measurement_environment(old, &paint) {
                return None;
            }
            RowMeasurementPlan::reuse_layout(&paint_rows(old)?, &current_rows, &cache.rows)
        });
        let mut plan =
            reused.or_else(|| RowMeasurementPlan::new(paint.projection.lines().len()))?;
        if !plan.share_repeated(&current_rows) {
            return None;
        }
        Some((paint, plan))
    }
    /// Keep the original measurement environment until this ticket ends. Native
    /// font notifications may arrive after the job already observed loaded faces.
    pub fn retain_row_preparation(self, ticket: u64, paint: &EditorRowPaint) -> bool {
        if !self.row_preparation_current(ticket) || !self.row_paint_current(paint) {
            return false;
        }
        self.workspace
            .editor_row_preparation
            .try_update(|preparation| {
                let Some(preparation) = preparation.as_mut() else {
                    return false;
                };
                if let Some(original) = preparation.paint.as_ref() {
                    return same_paint_scope(original, paint)
                        && original.projection.shares_text_version(&paint.projection);
                }
                preparation.paint = Some(paint.clone());
                true
            })
            .unwrap_or(false)
    }

    fn retain_origin_measurement(
        self,
        ticket: u64,
        paint: &EditorRowPaint,
        retain: impl FnOnce(&mut crate::state::workspace::EditorRowPreparation),
    ) -> bool {
        if !self.row_preparation_current(ticket)
            || !self.row_paint_current(paint)
            || paint.whitespace != self.preferences().show_whitespace
            || self
                .projection()
                .is_none_or(|projection| !paint.projection.shares_text_version(&projection))
        {
            return false;
        }
        self.workspace
            .editor_row_preparation
            .try_update(|preparation| {
                let Some(preparation) = preparation.as_mut() else {
                    return false;
                };
                if preparation.syntax_revision
                    != self.workspace.editor_preparation_revision.get_untracked()
                    || preparation.paint.as_ref().is_none_or(|original| {
                        !same_paint_scope(original, paint)
                            || !original.projection.shares_text_version(&paint.projection)
                    })
                {
                    return false;
                }
                retain(preparation);
                true
            })
            .unwrap_or(false)
    }
    pub fn retain_measured_prefix(
        self,
        ticket: u64,
        paint: &EditorRowPaint,
        rows: openwebide_core::editor::MeasuredRows,
    ) -> bool {
        if rows.is_empty()
            || rows.len() > openwebide_core::editor::MAX_MEASURE_ROWS
            || rows.len() > paint.projection.lines().len()
        {
            return false;
        }
        self.retain_origin_measurement(ticket, paint, |preparation| {
            preparation.prefix = Some(rows);
        })
    }
    pub fn retain_paragraph_coverage(
        self,
        ticket: u64,
        paint: &EditorRowPaint,
        row: usize,
        coverage: Arc<openwebide_core::editor::WrappedCoverage>,
    ) -> bool {
        if row != 0
            || self.is_composing()
            || !paint.word_wrap
            || paint.projection.visual_line_index(row).is_none_or(|index| {
                !index.source_paint_eligible() || coverage.glyph_end() >= index.len()
            })
        {
            return false;
        }
        self.retain_origin_measurement(ticket, paint, |preparation| {
            preparation.paragraph_coverage = Some(coverage);
        })
    }

    fn with_origin_measurement<T>(
        self,
        read: impl FnOnce(&crate::state::workspace::EditorRowPreparation, &EditorRowPaint) -> Option<T>,
    ) -> Option<T> {
        self.workspace.content.track();
        let projection = self.projection()?;
        self.view_revision();
        self.layout_epoch();
        self.font_epoch();
        let preferences = self.preferences();
        let syntax_revision = self.workspace.editor_preparation_revision.get();
        if let Some(auth) = self.auth {
            auth.generation.track();
        }
        self.workspace.editor_row_preparation.with(|preparation| {
            let preparation = preparation.as_ref()?;
            let paint = preparation.paint.as_ref()?;
            if preparation.syntax_revision != syntax_revision
                || !paint.projection.shares_text_version(&projection)
                || paint.whitespace != preferences.show_whitespace
                || !self.row_preparation_current(preparation.ticket)
                || !self.row_paint_current(paint)
            {
                return None;
            }
            read(preparation, paint)
        })
    }
    /// Early paint can use completed origin rows without exposing partial extents.
    pub fn measured_prefix(self) -> Option<crate::state::workspace::EditorRowCache> {
        self.with_origin_measurement(|preparation, paint| {
            Some(crate::state::workspace::EditorRowCache {
                paint: paint.clone(),
                rows: preparation.prefix.clone()?,
            })
        })
    }
    /// Partial logical-row coverage is never a completed row table or EOF.
    pub fn paragraph_coverage(self) -> Option<crate::state::workspace::EditorParagraphCoverage> {
        self.workspace.editor_composition.track();
        if self.is_composing() {
            return None;
        }
        self.with_origin_measurement(|preparation, paint| {
            Some(crate::state::workspace::EditorParagraphCoverage {
                paint: paint.clone(),
                coverage: preparation.paragraph_coverage.clone()?,
            })
        })
    }

    /// Retain completed or in-flight source/account-owned measurements only when
    /// actual face availability and CSS metrics match. Unknown geometry refreshes.
    pub fn font_measurements_changed(self, metrics: Option<&str>) -> bool {
        // A source edit invalidates current row geometry, not the measured font
        // identity. Matching trusted notifications must preserve replay candidates.
        let retained_font_current = |paint: &EditorRowPaint| {
            paint.font_epoch == self.workspace.editor_font_epoch.get_untracked()
                && Some(paint.key.clone()) == self.key()
                && paint.epoch == self.workspace.pending_epoch.get_untracked()
                && paint.read_revision == self.workspace.editor_read_revision.get_untracked()
                && paint.account_generation == self.account_generation()
                && Some(paint.metrics.as_str()) == metrics
        };
        let retained_font = self
            .workspace
            .editor_paragraph_cache
            .with_untracked(|cache| {
                cache
                    .as_ref()
                    .is_some_and(|cache| retained_font_current(&cache.paint))
            })
            || self.workspace.editor_row_cache.with_untracked(|cache| {
                cache
                    .as_ref()
                    .is_some_and(|cache| retained_font_current(&cache.paint))
            });
        if retained_font {
            return false;
        }
        if let Some(rows) = self.measured_rows() {
            return Some(rows.metrics.as_str()) != metrics;
        }
        !self
            .workspace
            .editor_row_preparation
            .with_untracked(|preparation| {
                preparation.as_ref().is_some_and(|preparation| {
                    preparation.revision == self.workspace.editor_view_revision.get_untracked()
                        && preparation.paint.as_ref().is_some_and(|paint| {
                            self.row_paint_current(paint) && Some(paint.metrics.as_str()) == metrics
                        })
                })
            })
    }
    pub fn invalidate_measured_font(self) {
        self.workspace
            .editor_font_epoch
            .update(|epoch| *epoch = epoch.wrapping_add(1));
        self.workspace.editor_row_cache.set(None);
        self.workspace.editor_paragraph_cache.set(None);
        self.invalidate_measured_rows();
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EditorFragmentWindow {
    pub rows: Vec<usize>,
    pub windows: Vec<(usize, openwebide_core::editor::RowPaintWindow)>,
    pub width: i32,
    pub height: i32,
    pub trailing: bool,
}
#[derive(Default)]
pub struct EditorFragmentCache {
    scope: Option<(EditorRowPaint, u64, u64)>,
    geometry:
        std::collections::VecDeque<(usize, Arc<openwebide_core::editor::MeasuredRowGeometry>)>,
    paint: openwebide_core::editor::PaintCache<EditorFragmentWindow>,
}
impl EditorActions {
    /// Select one exact source/paint/layout scope before retaining any DOM result.
    #[allow(
        clippy::too_many_arguments,
        reason = "Immutable paint primitives accompany the adapter's measured layout"
    )]
    pub fn fragment_scope(
        self,
        cache: &mut EditorFragmentCache,
        metrics: String,
        tokens: (bool, Arc<openwebide_core::highlight::TokenRows>),
        guides: Arc<[usize]>,
        indentation: Indentation,
        whitespace: bool,
    ) -> bool {
        let paint = self.projection().and_then(|projection| {
            self.row_paint_snapshot(metrics, projection, tokens, guides, indentation, whitespace)
        });
        let Some(paint) = paint else {
            cache.scope = None;
            cache.paint.clear();
            cache.geometry.clear();
            return false;
        };
        let revision = self.workspace.editor_view_revision.get_untracked();
        let layout = self.workspace.editor_layout_epoch.get_untracked();
        let same = cache
            .scope
            .as_ref()
            .is_some_and(|(old, old_revision, old_layout)| {
                *old_revision == revision && *old_layout == layout && same_paint_scope(old, &paint)
            });
        if !same {
            let reusable = cache.scope.as_ref().and_then(|(old, _, _)| {
                // Geometry follows actual fonts and exact styled rows. Height
                // reconciliation can advance the layout scope after an equivalent
                // syntax result without changing either; no height cache is needed.
                if !same_measurement_environment(old, &paint) {
                    return None;
                }
                Some(
                    cache
                        .geometry
                        .iter()
                        .filter(|(row, _)| {
                            paint_row(old, *row)
                                .zip(paint_row(&paint, *row))
                                .is_some_and(|(old, new)| old == new)
                        })
                        .cloned()
                        .collect(),
                )
            });
            cache.paint.clear();
            cache.geometry = reusable.unwrap_or_default();
            cache.scope = Some((paint, revision, layout));
        }
        true
    }
    pub fn measured_row_geometry(
        self,
        cache: &mut EditorFragmentCache,
        row: usize,
    ) -> Option<Arc<openwebide_core::editor::MeasuredRowGeometry>> {
        let (paint, revision, layout) = cache.scope.as_ref()?;
        if !self.row_paint_current(paint)
            || *revision != self.workspace.editor_view_revision.get_untracked()
            || *layout != self.workspace.editor_layout_epoch.get_untracked()
        {
            return None;
        }
        let Some(at) = cache.geometry.iter().position(|(index, _)| *index == row) else {
            let coverage = self.paragraph_coverage()?;
            if row != 0
                || !same_paint_scope(paint, &coverage.paint)
                || !paint
                    .projection
                    .shares_text_version(&coverage.paint.projection)
            {
                return None;
            }
            return Some(Arc::new(
                openwebide_core::editor::MeasuredRowGeometry::WrappedCoverage(coverage.coverage),
            ));
        };
        let entry = cache.geometry.remove(at)?;
        let result = entry.1.clone();
        cache.geometry.push_back(entry);
        Some(result)
    }
    /// Map a source caret into current source-owned paint. The DOM adapter must
    /// additionally prove that this UTF-16 column is present in its paint coverage.
    pub fn painted_source_caret(
        self,
        cache: &EditorFragmentCache,
        offset: usize,
        metrics: &str,
    ) -> Option<(usize, usize)> {
        let paint = &cache.scope.as_ref()?.0;
        if paint.metrics != metrics || !self.row_paint_current(paint) {
            return None;
        }
        let projection = &paint.projection;
        let visible = projection.visible_offset(offset).ok()?;
        let row = projection
            .lines()
            .partition_point(|line| line.visible_start <= visible)
            .checked_sub(1)?;
        let line = projection.lines().get(row)?;
        let local = visible.checked_sub(line.visible_start)?;
        let body = projection.line_body(row)?;
        if local > body.len() || !body.is_char_boundary(local) {
            return None;
        }
        let column = projection
            .byte_to_textarea(visible)
            .ok()?
            .checked_sub(line.textarea_start)?;
        Some((line.source_line, column))
    }
    /// Return only an exactly retained caret within the current source/style
    /// scope. Runtime adapters translate these row-relative pixels into the view.
    pub fn measured_source_caret(
        self,
        cache: &mut EditorFragmentCache,
        offset: usize,
        metrics: &str,
    ) -> Option<(usize, openwebide_core::editor::GlyphRectangle)> {
        if cache.scope.as_ref()?.0.metrics != metrics {
            return None;
        }
        let projection = self.projection()?;
        let visible = projection.visible_offset(offset).ok()?;
        let row = projection
            .lines()
            .partition_point(|line| line.visible_start <= visible)
            .checked_sub(1)?;
        let line = projection.lines().get(row)?;
        let body = projection.line_body(row)?;
        let local = visible.checked_sub(line.visible_start)?;
        let index = projection.visual_line_index(row)?;
        if !index.source_paint_eligible() {
            return None;
        }
        let glyph = if local == 0 {
            0
        } else if local == body.len() {
            index.len().checked_sub(1)?
        } else {
            let glyph = index.index_at_byte(body, local)?;
            if index.at(body, glyph)?.0 != local {
                return None;
            }
            glyph
        };
        let geometry = self.measured_row_geometry(cache, row)?;
        if matches!(
            geometry.as_ref(),
            openwebide_core::editor::MeasuredRowGeometry::Wrapped(_)
                | openwebide_core::editor::MeasuredRowGeometry::WrappedCoverage(_)
        ) != self.preferences().word_wrap
        {
            return None;
        }
        Some((row, geometry.caret(glyph)?))
    }
    pub fn row_source_slice(
        self,
        cache: &mut EditorFragmentCache,
        row: usize,
        window: &openwebide_core::editor::RowPaintWindow,
        line_height: f64,
    ) -> Option<EditorRowSourceSlice> {
        let geometry = self.measured_row_geometry(cache, row)?;
        let paint = &cache.scope.as_ref()?.0;
        let line = paint.projection.lines().get(row)?;
        let end = paint
            .projection
            .lines()
            .get(row + 1)
            .map_or(paint.projection.text().len(), |next| next.visible_start);
        let raw = paint.projection.text().get(line.visible_start..end)?;
        let body = raw
            .strip_suffix("\r\n")
            .or_else(|| raw.strip_suffix('\n'))
            .unwrap_or(raw);
        let index = paint.projection.visual_line_index(row)?;
        if !index.source_paint_eligible() {
            return None;
        }
        let interval = geometry.source_interval(window, line_height)?;
        let (mut start, mut native_start) = index.at(body, interval.start)?;
        let (end, _) = index.at(body, interval.end)?;
        if end.checked_sub(start)? > openwebide_core::editor::MAX_MEASURE_BYTES {
            return None;
        }
        let mut starts_paint_run =
            paint_row(paint, row)?.plain.is_some() && index.is_text_run_boundary(start);
        if !starts_paint_run
            && let Some(run_start) = self.paragraph_paint_run_start(paint, row, start)
            && end.checked_sub(run_start)? <= openwebide_core::editor::MAX_MEASURE_BYTES
        {
            // Include the small preceding part of the original run. The DOM
            // adapter still crops to and validates the measured source interval.
            let glyph = index.index_at_byte(body, run_start)?;
            let (byte, native) = index.at(body, glyph)?;
            if byte == run_start {
                start = byte;
                native_start = native;
                starts_paint_run = true;
            }
        }
        Some(EditorRowSourceSlice {
            source_line: line.source_line,
            bytes: start..end,
            native_start,
            reaches_end: end == body.len(),
            starts_paint_run,
            paint_runs: None,
        })
    }
    fn retained_styled_paint_runs(
        self,
        paint: &EditorRowPaint,
        row: usize,
    ) -> Option<Option<Arc<[usize]>>> {
        self.workspace.editor_paint_runs.with_untracked(|cache| {
            let cache = cache
                .as_ref()
                .filter(|old| same_paint_runs(&old.paint, paint))?;
            cache
                .rows
                .iter()
                .find(|(index, _)| *index == row)
                .map(|(_, runs)| runs.clone())
        })
    }
    fn retain_styled_paint_runs(
        self,
        paint: &EditorRowPaint,
        row: usize,
        runs: Option<Arc<[usize]>>,
    ) {
        if !self.row_paint_current(paint) {
            return;
        }
        self.workspace.editor_paint_runs.update(|cache| {
            let cache = cache.get_or_insert_with(|| crate::state::workspace::EditorPaintRuns {
                paint: paint.clone(),
                rows: Vec::new(),
            });
            if !same_paint_runs(&cache.paint, paint) {
                cache.paint = paint.clone();
                cache.rows.clear();
            }
            if let Some(entry) = cache.rows.iter_mut().find(|(index, _)| *index == row) {
                entry.1 = runs;
                return;
            }
            if cache.rows.len() == MAX_PAINT_RUN_ROWS {
                cache.rows.remove(0);
            }
            cache.rows.push((row, runs));
        });
    }
    /// Prepare original styled runs above DOM measurement, yielding without
    /// restarting Unicode segmentation or publishing an incomplete run table.
    pub async fn prepare_styled_paint_runs(self, paint: &EditorRowPaint, row: usize) -> bool {
        if !self.row_paint_current(paint) {
            return false;
        }
        if self.retained_styled_paint_runs(paint, row).is_some() {
            return true;
        }
        let Some((body, style)) = cacheable_styled_row(paint, row) else {
            return true;
        };
        let Ok(runs) = self
            .prepare_row_runs(paint, row, body, &style, MAX_RETAINED_PAINT_RUNS)
            .await
        else {
            return false;
        };
        self.retain_styled_paint_runs(paint, row, runs);
        true
    }
    async fn prepare_row_runs(
        self,
        paint: &EditorRowPaint,
        row: usize,
        body: &str,
        style: &PaintRow<'_>,
        max_runs: usize,
    ) -> Result<Option<Arc<[usize]>>, ()> {
        #[cfg(feature = "test-support")]
        PAINT_RUN_PREPARATIONS.set(PAINT_RUN_PREPARATIONS.get() + 1);
        let mut preparation = openwebide_core::editor::PaintRunPreparation::new(
            style.tokens,
            body.len(),
            style.normalize_cr,
            max_runs,
        );
        loop {
            if !self.row_paint_current(paint) {
                return Err(());
            }
            #[cfg(feature = "test-support")]
            let before = preparation.segmented_bytes();
            let done = preparation.advance(openwebide_core::editor::PAINT_RUN_BATCH_UNITS);
            #[cfg(feature = "test-support")]
            PAINT_RUN_SEGMENT_BYTES
                .set(PAINT_RUN_SEGMENT_BYTES.get() + preparation.segmented_bytes() - before);
            if done {
                break;
            }
            crate::util::yield_task().await;
            if !self.row_paint_current(paint) {
                return Err(());
            }
            if let Some(runs) = self.retained_styled_paint_runs(paint, row)
                && (runs.is_some() || max_runs == MAX_RETAINED_PAINT_RUNS)
            {
                return Ok(runs);
            }
        }
        if !self.row_paint_current(paint) {
            return Err(());
        }
        Ok(preparation.finish())
    }
    /// Preserve uncapped paragraph fallback without segmenting a complete styled
    /// row in one task. Over-budget tables belong only to the returned plan.
    pub async fn prepare_paragraph_measurements_cooperatively<'a>(
        self,
        paint: &'a EditorRowPaint,
        row: usize,
    ) -> Option<openwebide_core::editor::ParagraphMeasurementPlan<'a>> {
        if !self.row_paint_current(paint) {
            return None;
        }
        let (body, index, style) = unwrapped_paragraph_row(paint, row)?;
        let runs = if let Some(plain) = style.plain {
            if plain != body {
                return None;
            }
            index.text_run_boundaries().collect()
        } else {
            match self.retained_styled_paint_runs(paint, row) {
                Some(Some(runs)) => runs,
                _ => {
                    // Build once even when the complete table exceeds retention.
                    // A negative cache entry limits retention, not the ephemeral plan.
                    let runs = self
                        .prepare_row_runs(paint, row, body, &style, usize::MAX)
                        .await
                        .ok()??;
                    if cacheable_styled_row(paint, row).is_some() {
                        self.retain_styled_paint_runs(
                            paint,
                            row,
                            (runs.len() <= MAX_RETAINED_PAINT_RUNS).then(|| runs.clone()),
                        );
                    }
                    runs
                }
            }
        };
        let preparation =
            openwebide_core::editor::ParagraphAnchorPreparation::unwrapped(body, index, runs)?;
        openwebide_core::editor::ParagraphMeasurementPlan::with_prepared_anchors(
            self.prepare_paragraph_anchors(paint, preparation).await?,
        )
    }
    fn cached_styled_paint_runs(self, paint: &EditorRowPaint, row: usize) -> Option<Arc<[usize]>> {
        if !self.row_paint_current(paint) {
            return None;
        }
        if let Some(runs) = self.retained_styled_paint_runs(paint, row) {
            return runs;
        }
        let (body, style) = cacheable_styled_row(paint, row)?;
        let runs = styled_paint_runs(body, &style, MAX_RETAINED_PAINT_RUNS);
        self.retain_styled_paint_runs(paint, row, runs.clone());
        runs
    }
    fn paragraph_paint_run_start(
        self,
        paint: &EditorRowPaint,
        row: usize,
        byte: usize,
    ) -> Option<usize> {
        let retained = self
            .workspace
            .editor_paragraph_cache
            .with_untracked(|cache| {
                let cache = cache.as_ref()?;
                let old = &cache.paint;
                // Finishing row preparation can advance layout reconciliation. Run
                // boundaries depend on exact source/style ownership, not that epoch.
                if !same_paint_runs(old, paint) {
                    return None;
                }
                cache
                    .rows
                    .iter()
                    .find(|(index, _)| *index == row)?
                    .1
                    .paint_run_start(byte)
            });
        if retained.is_some() {
            return retained;
        }
        let runs = self.cached_styled_paint_runs(paint, row)?;
        if byte > *runs.last()? {
            return None;
        }
        let end = runs.partition_point(|end| *end <= byte);
        Some(end.checked_sub(1).map_or(0, |index| runs[index]))
    }
    pub fn forget_measured_row_geometry(self, cache: &mut EditorFragmentCache, row: usize) {
        cache.geometry.retain(|(index, _)| *index != row);
        if row == 0
            && let Some((paint, _, _)) = cache.scope.as_ref()
            && let Some(coverage) = self.paragraph_coverage()
            && same_paint_scope(paint, &coverage.paint)
        {
            self.workspace.editor_row_preparation.update(|preparation| {
                if let Some(preparation) = preparation {
                    preparation.paragraph_coverage = None;
                }
            });
        }
    }
    pub fn retain_measured_row_geometry(
        self,
        cache: &mut EditorFragmentCache,
        row: usize,
        geometry: openwebide_core::editor::MeasuredRowGeometry,
    ) {
        let Some((paint, revision, layout)) = cache.scope.as_ref() else {
            return;
        };
        if !self.row_paint_current(paint)
            || *revision != self.workspace.editor_view_revision.get_untracked()
            || *layout != self.workspace.editor_layout_epoch.get_untracked()
        {
            return;
        }
        if matches!(
            geometry,
            openwebide_core::editor::MeasuredRowGeometry::Wrapped(_)
                | openwebide_core::editor::MeasuredRowGeometry::WrappedCoverage(_)
        ) && !paint
            .projection
            .visual_line_index(row)
            .is_some_and(|index| index.source_paint_eligible())
        {
            return;
        }
        if !cache
            .geometry
            .iter()
            .any(|(index, old)| *index == row && old.as_ref() == &geometry)
            && let Some(line) = paint.projection.lines().get(row)
        {
            let source_line = line.source_line;
            cache
                .paint
                .retain(|window| !window.rows.contains(&source_line));
        }
        cache.geometry.retain(|(index, _)| *index != row);
        if cache.geometry.len() == 8 {
            cache.geometry.pop_front();
        }
        cache.geometry.push_back((row, Arc::new(geometry)));
    }
    /// A cold probe may only add geometry to the exact styled scope it measured.
    pub fn retain_preparation_geometry(
        self,
        cache: &mut EditorFragmentCache,
        paint: &EditorRowPaint,
        row: usize,
        geometry: openwebide_core::editor::MeasuredRowGeometry,
    ) {
        if cache
            .scope
            .as_ref()
            .is_some_and(|(current, _, _)| same_paint_scope(current, paint))
        {
            self.retain_measured_row_geometry(cache, row, geometry);
        }
    }
    pub fn cached_fragment(
        self,
        cache: &mut EditorFragmentCache,
        window: &EditorFragmentWindow,
    ) -> Option<String> {
        let (paint, revision, layout) = cache.scope.as_ref()?;
        if !self.row_paint_current(paint)
            || *revision != self.workspace.editor_view_revision.get_untracked()
            || *layout != self.workspace.editor_layout_epoch.get_untracked()
        {
            return None;
        }
        cache.paint.get(window)
    }
    pub fn retain_fragment(
        self,
        cache: &mut EditorFragmentCache,
        window: EditorFragmentWindow,
        html: String,
    ) {
        if let Some((paint, revision, layout)) = cache.scope.as_ref()
            && self.row_paint_current(paint)
            && *revision == self.workspace.editor_view_revision.get_untracked()
            && *layout == self.workspace.editor_layout_epoch.get_untracked()
        {
            cache.paint.insert(window, html);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwebide_core::highlight::TokenKind;

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn styled_run_preparation_yields_preserves_boundaries_and_rejects_stale_owners() {
        use std::{
            future::Future,
            task::{Context, Poll, Waker},
        };
        for giant in [false, true] {
            for stale in [
                "complete", "source", "read", "project", "account", "dispose",
            ] {
                let owner = Owner::new();
                let body = if giant {
                    format!("👩{}\u{200d}👩 last", "\u{301}".repeat(40_000))
                } else {
                    "word 文😀e\u{301} ".repeat(6000)
                };
                if giant {
                    let source = Arc::new(body.clone());
                    let mut preparation =
                        openwebide_core::editor::DocumentPreparation::new(&source);
                    assert!(!preparation.advance(1));
                    while !preparation.advance(openwebide_core::editor::PAINT_RUN_BATCH_UNITS) {
                        crate::util::yield_task().await;
                    }
                    let document = preparation.finish().unwrap();
                    assert!(Arc::ptr_eq(&document.shared_text(), &source));
                    assert!(!document.is_dirty());
                    let index = document.projection().visual_line_index(0).unwrap();
                    let expected = openwebide_core::editor::visual_line_offsets(&body).unwrap();
                    for (glyph, &(byte, native)) in expected.iter().enumerate() {
                        assert_eq!(index.at(&body, glyph), Some((byte, native)));
                        assert_eq!(index.index_at_byte(&body, byte), Some(glyph));
                    }
                    assert_eq!(index.index_at_byte(&body, body.len() / 2), Some(0));
                }
                let (workspace, auth, actions, paint, expected) = owner.with(|| {
                    let auth = crate::state::auth::AuthState::new();
                    provide_context(auth);
                    let workspace = crate::state::workspace::WorkspaceState::new();
                    workspace.active_project.set(Some(1));
                    workspace.open_file.set(Some("styled.rs".into()));
                    workspace.content.set(body.clone().into());
                    let actions = EditorActions::new(workspace);
                    let tokens = vec![Token {
                        kind: TokenKind::String,
                        text: body.clone(),
                    }];
                    let expected: Arc<[usize]> =
                        openwebide_core::editor::visual_text_run_ranges(&body)
                            .map(|run| run.end)
                            .collect();
                    let paint = actions
                        .row_paint_snapshot(
                            "font metrics".into(),
                            FoldProjection::new(&body, &Default::default()),
                            (true, Arc::new(vec![tokens.into()])),
                            Arc::from([]),
                            Indentation::default(),
                            false,
                        )
                        .unwrap();
                    (workspace, auth, actions, paint, expected)
                });
                take_paint_run_segment_bytes();
                let mut preparation = Box::pin(actions.prepare_styled_paint_runs(&paint, 0));
                assert!(matches!(
                    preparation
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop())),
                    Poll::Pending
                ));
                assert!(workspace.editor_paint_runs.get_untracked().is_none());
                let scanned = take_paint_run_segment_bytes();
                if giant {
                    assert_eq!(
                        scanned, 0,
                        "the incomplete cluster must yield before emitting a run"
                    );
                } else {
                    assert!(scanned > 0 && scanned < body.len());
                }
                match stale {
                    "source" => workspace.pending_epoch.update(|epoch| *epoch += 1),
                    "read" => workspace
                        .editor_read_revision
                        .update(|revision| *revision += 1),
                    "project" => workspace.active_project.set(Some(2)),
                    "account" => auth.generation.update(|generation| *generation += 1),
                    "dispose" => owner.cleanup(),
                    _ => {}
                }
                assert_eq!(preparation.await, stale == "complete");
                if stale == "complete" {
                    assert_eq!(
                        actions.cached_styled_paint_runs(&paint, 0).unwrap(),
                        expected
                    );
                    take_paint_run_segment_bytes();
                    assert!(actions.prepare_styled_paint_runs(&paint, 0).await);
                    assert_eq!(take_paint_run_segment_bytes(), 0);
                } else if stale != "dispose" {
                    assert!(workspace.editor_paint_runs.get_untracked().is_none());
                }
            }
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn uncapped_paragraph_runs_yield_without_expanding_the_retained_cache() {
        use std::{
            future::Future,
            task::{Context, Poll, Waker},
        };
        for negative_cached in [false, true] {
            for stale in [
                "complete", "source", "read", "project", "account", "dispose",
            ] {
                let owner = Owner::new();
                let (workspace, auth, actions, paint) = owner.with(|| {
                    let auth = crate::state::auth::AuthState::new();
                    provide_context(auth);
                    let workspace = crate::state::workspace::WorkspaceState::new();
                    workspace.active_project.set(Some(1));
                    workspace.open_file.set(Some("uncapped.rs".into()));
                    let tokens = vec![
                        Token {
                            kind: TokenKind::Keyword,
                            text: "word".into()
                        };
                        MAX_RETAINED_PAINT_RUNS + 1000
                    ];
                    let body = "word".repeat(tokens.len());
                    workspace.content.set(body.clone().into());
                    let actions = EditorActions::new(workspace);
                    let paint = actions
                        .row_paint_snapshot(
                            "font metrics".into(),
                            FoldProjection::new(&body, &Default::default()),
                            (true, Arc::new(vec![tokens.into()])),
                            Arc::from([]),
                            Indentation::default(),
                            false,
                        )
                        .unwrap();
                    (workspace, auth, actions, paint)
                });
                if negative_cached {
                    assert!(actions.prepare_styled_paint_runs(&paint, 0).await);
                    assert_eq!(actions.retained_styled_paint_runs(&paint, 0), Some(None));
                }
                PAINT_RUN_PREPARATIONS.set(0);
                let mut preparation =
                    Box::pin(actions.prepare_paragraph_measurements_cooperatively(&paint, 0));
                assert!(matches!(
                    preparation
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop())),
                    Poll::Pending
                ));
                assert_eq!(PAINT_RUN_PREPARATIONS.get(), 1);
                if !negative_cached {
                    assert!(actions.retained_styled_paint_runs(&paint, 0).is_none());
                }
                match stale {
                    "source" => workspace.pending_epoch.update(|epoch| *epoch += 1),
                    "read" => workspace
                        .editor_read_revision
                        .update(|revision| *revision += 1),
                    "project" => workspace.active_project.set(Some(2)),
                    "account" => auth.generation.update(|generation| *generation += 1),
                    "dispose" => owner.cleanup(),
                    _ => {}
                }
                let plan = preparation.await;
                assert_eq!(plan.is_some(), stale == "complete");
                if let Some(plan) = plan {
                    let expected = EditorActions::paragraph_measurements(&paint, 0).unwrap();
                    assert_eq!(plan.probe().unwrap().bytes, expected.probe().unwrap().bytes);
                    assert_eq!(plan.targets(), expected.targets());
                }
                if stale != "dispose" {
                    assert_eq!(
                        actions.retained_styled_paint_runs(&paint, 0),
                        (negative_cached || stale == "complete").then_some(None),
                    );
                    assert_eq!(PAINT_RUN_PREPARATIONS.get(), 1);
                }
            }
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn paragraph_anchor_setup_yields_without_rescanning_and_rejects_stale_owners() {
        use std::{
            future::Future,
            task::{Context, Poll, Waker},
        };
        for wrapped in [false, true] {
            for stale in [
                "complete", "source", "read", "project", "account", "dispose",
            ] {
                let owner = Owner::new();
                let (workspace, auth, actions, paint) = owner.with(|| {
                    let auth = crate::state::auth::AuthState::new();
                    provide_context(auth);
                    let workspace = crate::state::workspace::WorkspaceState::new();
                    workspace.active_project.set(Some(1));
                    workspace.open_file.set(Some("anchors.rs".into()));
                    let body = "word 文😀e\u{301} ".repeat(6000);
                    workspace.content.set(body.clone().into());
                    let actions = EditorActions::new(workspace);
                    let paint = actions
                        .row_paint_snapshot(
                            "anchor metrics".into(),
                            FoldProjection::new(&body, &Default::default()),
                            (
                                true,
                                Arc::new(vec![
                                    vec![Token {
                                        kind: TokenKind::String,
                                        text: body,
                                    }]
                                    .into(),
                                ]),
                            ),
                            Arc::from([]),
                            Indentation::default(),
                            false,
                        )
                        .unwrap();
                    (workspace, auth, actions, paint)
                });
                assert!(actions.prepare_styled_paint_runs(&paint, 0).await);
                take_paint_run_segment_bytes();
                let mut preparation = Box::pin(async {
                    if wrapped {
                        actions
                            .prepare_wrapped_paragraph_cooperatively(&paint, 0)
                            .await
                            .is_some()
                    } else {
                        actions
                            .prepare_paragraph_measurements_cooperatively(&paint, 0)
                            .await
                            .is_some()
                    }
                });
                assert!(matches!(
                    preparation
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop())),
                    Poll::Pending
                ));
                assert_eq!(
                    take_paint_run_segment_bytes(),
                    0,
                    "cached runs must not be segmented again"
                );
                assert!(
                    workspace.editor_row_cache.get_untracked().is_none(),
                    "partial anchors cannot publish row geometry"
                );
                match stale {
                    "source" => workspace.pending_epoch.update(|epoch| *epoch += 1),
                    "read" => workspace
                        .editor_read_revision
                        .update(|revision| *revision += 1),
                    "project" => workspace.active_project.set(Some(2)),
                    "account" => auth.generation.update(|generation| *generation += 1),
                    "dispose" => owner.cleanup(),
                    _ => {}
                }
                assert_eq!(preparation.await, stale == "complete", "{wrapped}/{stale}");
            }
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn unavailable_styled_run_tables_are_scoped_bounded_and_not_rescanned() {
        Owner::new().with(|| {
            let workspace = crate::state::workspace::WorkspaceState::new();
            workspace.active_project.set(Some(1));
            workspace.open_file.set(Some("over-budget.rs".into()));
            let mut tokens = vec![
                Token {
                    kind: TokenKind::Keyword,
                    text: "a".into()
                };
                MAX_RETAINED_PAINT_RUNS - 2
            ];
            tokens.push(Token {
                kind: TokenKind::String,
                text: "word 文😀e\u{301} ".repeat(5000),
            });
            let body = tokens
                .iter()
                .map(|token| token.text.as_str())
                .collect::<String>();
            let source = std::iter::repeat_n(body.as_str(), MAX_PAINT_RUN_ROWS + 1)
                .collect::<Vec<_>>()
                .join("\n");
            workspace.content.set(source.clone().into());
            let actions = EditorActions::new(workspace);
            let paint = actions
                .row_paint_snapshot(
                    "font metrics".into(),
                    FoldProjection::new(&source, &Default::default()),
                    (true, Arc::new(vec![tokens.into(); MAX_PAINT_RUN_ROWS + 1])),
                    Arc::from([]),
                    Indentation::default(),
                    false,
                )
                .unwrap();
            take_paint_run_segment_bytes();
            assert!(actions.cached_styled_paint_runs(&paint, 0).is_none());
            assert!(take_paint_run_segment_bytes() > 0);
            for _ in 0..4 {
                assert!(actions.cached_styled_paint_runs(&paint, 0).is_none());
                assert_eq!(take_paint_run_segment_bytes(), 0);
            }
            for case in 0..5 {
                let mut stale = paint.clone();
                match case {
                    0 => stale.account_generation += 1,
                    1 => stale.key.0 += 1,
                    2 => stale.key.1 = "other.rs".into(),
                    3 => stale.epoch += 1,
                    _ => stale.read_revision += 1,
                }
                assert!(actions.cached_styled_paint_runs(&stale, 0).is_none());
                assert_eq!(take_paint_run_segment_bytes(), 0);
                assert_eq!(
                    workspace
                        .editor_paint_runs
                        .get_untracked()
                        .unwrap()
                        .rows
                        .len(),
                    1
                );
            }
            workspace.editor_layout_epoch.update(|epoch| *epoch += 1);
            let mut reconciled = paint.clone();
            reconciled.layout_epoch = workspace.editor_layout_epoch.get_untracked();
            reconciled.view_revision = workspace.editor_view_revision.get_untracked();
            assert!(actions.cached_styled_paint_runs(&reconciled, 0).is_none());
            assert!(take_paint_run_segment_bytes() > 0);
            assert!(actions.cached_styled_paint_runs(&reconciled, 0).is_none());
            assert_eq!(take_paint_run_segment_bytes(), 0);
            let mut restyled = reconciled.clone();
            restyled.tokens = Arc::new((*paint.tokens).clone());
            assert!(actions.cached_styled_paint_runs(&restyled, 0).is_none());
            assert!(take_paint_run_segment_bytes() > 0);
            for row in 1..=MAX_PAINT_RUN_ROWS {
                assert!(actions.cached_styled_paint_runs(&restyled, row).is_none());
            }
            let cache = workspace.editor_paint_runs.get_untracked().unwrap();
            assert_eq!(cache.rows.len(), MAX_PAINT_RUN_ROWS);
            assert!(cache.rows.iter().all(|(_, runs)| runs.is_none()));
            assert!(cache.rows.iter().all(|(row, _)| *row != 0));
            take_paint_run_segment_bytes();
            assert!(actions.cached_styled_paint_runs(&restyled, 0).is_none());
            assert!(take_paint_run_segment_bytes() > 0, "evicted rows may retry");
            // A rejected retained table must not disable complete paragraph preparation.
            assert!(
                actions
                    .prepare_paragraph_measurements(&restyled, 0)
                    .is_some()
            );
        });
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn styled_run_budget_stops_before_segmenting_unused_token_suffix() {
        let tokens = [Token {
            kind: TokenKind::String,
            text: "a".repeat(openwebide_core::editor::MAX_EDITOR_LINE_BYTES),
        }];
        let row = PaintRow {
            tokens: &tokens,
            plain: None,
            guide: 0,
            normalize_cr: false,
            ending: false,
        };
        for budget in [0, 1, 4, 16, 64] {
            take_paint_run_segment_bytes();
            assert!(styled_paint_runs(&tokens[0].text, &row, budget).is_none());
            let scanned = take_paint_run_segment_bytes();
            assert!(
                scanned <= (budget + 1) * 512,
                "budget {budget}: scanned {scanned} bytes"
            );
        }
        let mut nearly_full = vec![
            Token {
                kind: TokenKind::Keyword,
                text: "a".into()
            };
            MAX_RETAINED_PAINT_RUNS - 2
        ];
        nearly_full.push(Token {
            kind: TokenKind::String,
            text: "a".repeat(70_000),
        });
        let full = nearly_full
            .iter()
            .map(|token| token.text.as_str())
            .collect::<String>();
        assert!(full.len() < openwebide_core::editor::MAX_EDITOR_LINE_BYTES);
        let limited = PaintRow {
            tokens: &nearly_full,
            ..row
        };
        take_paint_run_segment_bytes();
        assert!(styled_paint_runs(&full, &limited, MAX_RETAINED_PAINT_RUNS).is_none());
        assert!(
            take_paint_run_segment_bytes() <= 3 * 512,
            "the production cap stops inside the final long token"
        );
        let prefix = Token {
            kind: TokenKind::Keyword,
            text: "let".into(),
        };
        let combined = [prefix, tokens[0].clone()];
        let full = combined
            .iter()
            .map(|token| token.text.as_str())
            .collect::<String>();
        let row = PaintRow {
            tokens: &combined,
            ..row
        };
        take_paint_run_segment_bytes();
        assert!(styled_paint_runs(&full, &row, 1).is_none());
        assert_eq!(
            take_paint_run_segment_bytes(),
            0,
            "an exhausted token budget does not begin long-token segmentation"
        );
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn bounded_styled_runs_preserve_complete_grapheme_and_crlf_boundaries() {
        for body in [
            "word 文😀e\u{301} ".repeat(1000),
            format!("{}🇺🇸", "a".repeat(508)),
            format!("👩{}\u{200d}👩 last", "\u{301}".repeat(2000)),
        ] {
            for suffix in ["", "\r"] {
                let tokens = [Token {
                    kind: TokenKind::String,
                    text: body.clone() + suffix,
                }];
                let row = PaintRow {
                    tokens: &tokens,
                    plain: None,
                    guide: 0,
                    normalize_cr: !suffix.is_empty(),
                    ending: !suffix.is_empty(),
                };
                let expected = openwebide_core::editor::visual_text_run_ranges(&body)
                    .map(|run| run.end)
                    .collect::<Vec<_>>();
                let runs = styled_paint_runs(&body, &row, expected.len()).unwrap();
                assert_eq!(runs.as_ref(), expected);
                assert!(styled_paint_runs(&body, &row, expected.len() - 1).is_none());
                assert_eq!(
                    styled_paint_runs(&body, &row, usize::MAX).unwrap().as_ref(),
                    expected
                );
            }
        }
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn paint_run_ownership_survives_reconciliation_but_rejects_stale_inputs() {
        let old = EditorRowPaint {
            view_revision: 1,
            layout_epoch: 2,
            font_epoch: 3,
            key: (1, "file.rs".into()),
            epoch: 4,
            read_revision: 5,
            account_generation: 6,
            metrics: "font and width".into(),
            projection: FoldProjection::new("let value = 1;", &Default::default()),
            tokens: Arc::new(Vec::new()),
            prepared_source: true,
            guides: Arc::from([0]),
            indentation: Indentation::default(),
            whitespace: false,
            word_wrap: false,
        };
        let mut fresh = old.clone();
        fresh.layout_epoch += 1;
        assert!(same_paint_runs(&old, &fresh));
        for change in 0..15 {
            let mut stale = fresh.clone();
            match change {
                0 => stale.view_revision += 1,
                1 => stale.font_epoch += 1,
                2 => stale.key.0 += 1,
                3 => stale.key.1 = "other.rs".into(),
                4 => stale.epoch += 1,
                5 => stale.read_revision += 1,
                6 => stale.account_generation += 1,
                7 => stale.metrics.push_str("changed"),
                8 => stale.prepared_source = false,
                9 => {
                    stale.projection =
                        FoldProjection::new(old.projection.text(), &Default::default());
                }
                10 => stale.tokens = Arc::new((*old.tokens).clone()),
                11 => stale.guides = Arc::from([0]),
                12 => stale.indentation.width += 1,
                13 => stale.whitespace = true,
                _ => stale.word_wrap = true,
            }
            assert!(!same_paint_runs(&old, &stale), "change {change}");
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn paragraph_prefix_stops_before_a_changed_token_style_or_span_topology() {
        let plain = Token {
            kind: TokenKind::Plain,
            text: "let value = ".into(),
        };
        let comment = Token {
            kind: TokenKind::Comment,
            text: "word ".repeat(1000),
        };
        let old = [plain.clone(), comment.clone()];
        let mut changed = old.clone();
        changed[1].kind = TokenKind::Plain;
        fn row(tokens: &[Token]) -> PaintRow<'_> {
            PaintRow {
                tokens,
                plain: None,
                guide: 0,
                normalize_cr: false,
                ending: false,
            }
        }
        assert_eq!(
            paint_prefix_end(&row(&old), &row(&changed)),
            plain.text.len()
        );
        changed[1] = Token {
            text: format!("{}fresh", comment.text),
            ..comment.clone()
        };
        assert_eq!(
            paint_prefix_end(&row(&old), &row(&changed)),
            plain.text.len() + comment.text.len()
        );
        changed[1] = Token {
            text: "word ".into(),
            ..comment
        };
        assert_eq!(
            paint_prefix_end(&row(&old), &row(&changed)),
            plain.text.len()
        );
    }
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn paragraph_prefix_rejects_changed_guides_and_line_ending_paint() {
        let old = PaintRow {
            tokens: &[],
            plain: Some("same words"),
            guide: 1,
            normalize_cr: false,
            ending: false,
        };
        let changed = PaintRow { guide: 2, ..old };
        assert_eq!(paint_prefix_end(&old, &changed), 0);
        let changed = PaintRow {
            ending: true,
            ..old
        };
        assert_eq!(paint_prefix_end(&old, &changed), 0);
    }
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn paragraph_suffix_requires_unchanged_style_and_paint_topology() {
        let tokens = [Token {
            kind: TokenKind::Comment,
            text: format!("a{}", "word 文😀 ".repeat(100)),
        }];
        fn row(tokens: &[Token]) -> PaintRow<'_> {
            PaintRow {
                tokens,
                plain: None,
                guide: 0,
                normalize_cr: false,
                ending: false,
            }
        }
        let old = row(&tokens);
        assert_eq!(paint_suffix_start(&old, &old), Some(0));
        let mut changed = tokens.clone();
        changed[0].text.replace_range(0..1, "z");
        assert_eq!(paint_suffix_start(&old, &row(&changed)), Some(1));
        changed[0].kind = TokenKind::String;
        assert_eq!(
            paint_suffix_start(&old, &row(&changed)),
            Some(tokens[0].text.len())
        );
        changed[0].text.push('x');
        assert_eq!(
            paint_suffix_start(&old, &row(&changed)),
            Some(changed[0].text.len())
        );
        for changed in [
            PaintRow {
                guide: 1,
                ..row(&tokens)
            },
            PaintRow {
                ending: true,
                ..row(&tokens)
            },
            PaintRow {
                normalize_cr: true,
                ..row(&tokens)
            },
            PaintRow {
                tokens: &[],
                plain: Some(&tokens[0].text),
                ..row(&tokens)
            },
        ] {
            assert_eq!(paint_suffix_start(&old, &changed), None);
        }
    }
}
