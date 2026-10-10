//! Thin DOM measurements for the shared row-height/window policy.
use super::editor::current_editor_target;
use crate::state_actions::editor::EditorActions;
use leptos::prelude::*;
use openwebide_core::editor::MeasuredRows;
use wasm_bindgen::JsCast;

pub(super) struct RowProbe(web_sys::HtmlElement);
impl Drop for RowProbe {
    fn drop(&mut self) {
        self.0.remove();
    }
}

/// One styled DOM primitive shared by height and cursor-neighborhood probes.
pub(super) fn styled_row_probe(
    input: &web_sys::HtmlTextAreaElement,
) -> Result<(RowProbe, web_sys::Element), ()> {
    let style = window()
        .get_computed_style(input)
        .map_err(|_| ())?
        .ok_or(())?;
    let probe = document()
        .create_element("div")
        .map_err(|_| ())?
        .dyn_into::<web_sys::HtmlElement>()
        .map_err(|_| ())?;
    probe.set_class_name("editor-highlight editor-row-measure");
    if let Some(parent) = input.parent_element() {
        for class in ["editor-uniform-rows", "editor-word-wrap"] {
            if parent.class_list().contains(class) {
                probe.class_list().add_1(class).map_err(|_| ())?;
            }
        }
    }
    let gutter = input.offset_left();
    let width = crate::viewport::editor_scroll(input).client_width() + gutter;
    probe
        .set_attribute("style", &format!("position:fixed;left:-10000px;top:0;width:{width}px;height:auto;visibility:hidden;pointer-events:none;contain:layout style paint;--editor-gutter-width:{gutter}px"))
        .map_err(|_| ())?;
    for property in [
        "font-family",
        "font-size",
        "font-style",
        "font-weight",
        "font-stretch",
        "font-variation-settings",
        "font-variant-caps",
        "font-variant-numeric",
        "line-height",
        "tab-size",
        "white-space",
        "overflow-wrap",
        "letter-spacing",
        "font-kerning",
        "font-feature-settings",
        "font-variant-ligatures",
    ] {
        probe
            .style()
            .set_property(
                property,
                &style.get_property_value(property).map_err(|_| ())?,
            )
            .map_err(|_| ())?;
    }
    let paint = document().create_element("div").map_err(|_| ())?;
    paint.set_class_name("editor-highlight-content");
    paint.set_attribute("style", &format!("transform:none;will-change:auto;min-height:0;min-width:0;width:{width}px;padding-left:{gutter}px;padding-right:{}", style.get_property_value("padding-right").map_err(|_| ())?)).map_err(|_| ())?;
    probe.append_child(&paint).map_err(|_| ())?;
    document()
        .body()
        .ok_or(())?
        .append_child(&probe)
        .map_err(|_| ())?;
    Ok((RowProbe(probe), paint))
}

/// CSS padding is a DOM primitive; dimensions and validity come from shared rows.
/// Explicit CSS dimensions only; no default metrics authorize source extents.
pub(super) fn uniform_row_dimensions(input: &web_sys::HtmlTextAreaElement) -> Option<(f64, f64)> {
    if !web_sys::css::supports_with_value("height", "1lh").ok()? {
        return None;
    }
    let parent = input.parent_element()?;
    if !parent.class_list().contains("editor-uniform-rows")
        || parent.class_list().contains("editor-word-wrap")
    {
        return None;
    }
    let row = parent.query_selector(".editor-source-line").ok()??;
    // DOM layout quantizes CSS dimensions. Reuse the actual fixed row box,
    // rather than multiply an unquantized computed line-height.
    let row_height = row.get_bounding_client_rect().height();
    let style = window().get_computed_style(input).ok()??;
    let pixels = |name| {
        style
            .get_property_value(name)
            .ok()?
            .strip_suffix("px")?
            .parse::<f64>()
            .ok()
    };
    Some((
        row_height,
        pixels("padding-top")? + pixels("padding-bottom")?,
    ))
}

pub(super) fn document_extent(
    input: &web_sys::HtmlTextAreaElement,
    rows: &MeasuredRows,
) -> Option<openwebide_core::editor::DocumentExtent> {
    let style = window().get_computed_style(input).ok()??;
    let padding = |name| {
        style
            .get_property_value(name)
            .ok()?
            .trim_end_matches("px")
            .parse::<f64>()
            .ok()
    };
    rows.extent(
        padding("padding-right")? + padding("padding-left")?,
        padding("padding-top")? + padding("padding-bottom")?,
    )
}
fn extent_supported(input: &web_sys::HtmlTextAreaElement, rows: &MeasuredRows) -> bool {
    document_extent(input, rows)
        .is_some_and(|extent| crate::viewport::check_editor_extent(extent.width, extent.height))
}

#[cfg(feature = "test-support")]
thread_local! { static SUFFIX_PROBES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(feature = "test-support")]
pub(super) fn take_paragraph_suffix_probes() -> usize {
    SUFFIX_PROBES.replace(0)
}

#[cfg(feature = "test-support")]
thread_local! { static PREFIX_RECONCILIATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(feature = "test-support")]
pub(super) fn take_paragraph_prefix_reconciliations() -> usize {
    PREFIX_RECONCILIATIONS.replace(0)
}

async fn measure_paragraph(
    paint: &web_sys::Element,
    scope: &crate::state::workspace::EditorRowPaint,
    logical: usize,
    current: &impl Fn() -> bool,
    render: &impl Fn(&[usize], bool, &[crate::state_actions::editor::EditorRowSourceSlice]) -> String,
    actions: Option<EditorActions>,
) -> Result<Option<(f64, openwebide_core::editor::HorizontalGeometry)>, ()> {
    if !current() {
        return Ok(None);
    }
    let plan = if let Some(actions) = actions {
        actions
            .prepare_paragraph_measurements_cooperatively(scope, logical)
            .await
    } else {
        EditorActions::paragraph_measurements(scope, logical)
    };
    if !current() {
        return Ok(None);
    }
    let Some(mut plan) = plan else {
        return Ok(None);
    };
    if let Some((actions, prefix)) = actions.and_then(|actions| {
        actions
            .paragraph_prefix(scope, logical)
            .map(|prefix| (actions, prefix))
    }) {
        while current() && actions.resume_paragraph_prefix_batch(&prefix, &mut plan) > 0 {
            crate::util::yield_task().await;
        }
    }
    let suffix = actions.and_then(|actions| actions.paragraph_suffix(scope, logical));
    let mut probes = 0_usize;
    while plan.probe().is_some() {
        if !current() {
            return Ok(None);
        }
        if let Some((actions, suffix)) = actions.zip(suffix.as_ref()) {
            let reused = actions.resume_paragraph_suffix_batch(suffix, &mut plan);
            #[cfg(feature = "test-support")]
            SUFFIX_PROBES.set(SUFFIX_PROBES.get() + reused);
            if reused > 0 {
                crate::util::yield_task().await;
                continue;
            }
        }
        let Some(targets) = plan.targets() else {
            return Ok(None);
        };
        if !current() {
            return Ok(None);
        }
        let Some(layout) = measure_paragraph_probe(paint, scope, logical, &plan, &targets, render)?
        else {
            return Ok(None);
        };
        if plan.retry_origin(&layout.rectangles) {
            paint.set_inner_html("");
            crate::util::yield_task().await;
            continue;
        }
        if !plan.record(
            layout.width,
            layout.height,
            layout.scroll_width,
            &layout.rectangles,
        ) {
            return Ok(None);
        }
        paint.set_inner_html("");
        probes += 1;
        if probes.is_multiple_of(openwebide_core::editor::MAX_MEASURE_BATCHES_PER_FRAME) {
            crate::util::yield_frame().await;
        } else {
            crate::util::yield_task().await;
        }
    }
    let Some((width, geometry, measurements)) = plan.finish_with_measurements() else {
        return Ok(None);
    };
    if current()
        && let Some(actions) = actions
    {
        actions.retain_paragraph_measurements(scope, logical, measurements);
    }
    Ok(Some((width, geometry)))
}

async fn measure_wrapped_paragraph(
    actions: EditorActions,
    paint: &web_sys::Element,
    scope: &crate::state::workspace::EditorRowPaint,
    logical: usize,
    current: &impl Fn() -> bool,
    coverage: &impl Fn(openwebide_core::editor::WrappedCoverage),
    render: &impl Fn(&[usize], bool, &[crate::state_actions::editor::EditorRowSourceSlice]) -> String,
) -> Result<Option<(f64, openwebide_core::editor::WrappedGeometry)>, ()> {
    if !current() {
        return Ok(None);
    }
    let Some(mut plan) = actions
        .prepare_wrapped_paragraph_cooperatively(scope, logical)
        .await
    else {
        return Ok(None);
    };
    if !current() {
        return Ok(None);
    }
    if let Some(prefix) = actions.paragraph_prefix(scope, logical) {
        while current() && actions.resume_paragraph_prefix_batch(&prefix, &mut plan) > 0 {
            crate::util::yield_task().await;
        }
    }
    let suffix = actions.paragraph_suffix(scope, logical);
    let source_line = scope.projection.lines()[logical].source_line;
    let body = scope.projection.line_body(logical).ok_or(())?;
    let timing = ProbeTiming::installed();
    let mut probes = 0;
    let mut coverage_published = false;
    // Unavailable metrics disable early paint; complete preparation retains its
    // existing fallback contract.
    let line_height = window()
        .get_computed_style(paint)
        .ok()
        .flatten()
        .and_then(|style| style.get_property_value("line-height").ok())
        .and_then(|value| value.strip_suffix("px")?.parse::<f64>().ok());
    while let Some(probe) = plan.probe().cloned() {
        if !current() {
            return Ok(None);
        }
        if let Some(suffix) = suffix.as_ref() {
            #[cfg(feature = "test-support")]
            let before = plan.reconciled_prefixes();
            let reused = actions.resume_paragraph_suffix_batch(suffix, &mut plan);
            #[cfg(feature = "test-support")]
            PREFIX_RECONCILIATIONS
                .set(PREFIX_RECONCILIATIONS.get() + plan.reconciled_prefixes() - before);
            #[cfg(feature = "test-support")]
            SUFFIX_PROBES.set(SUFFIX_PROBES.get() + reused);
            if reused > 0 {
                crate::util::yield_task().await;
                continue;
            }
        }
        let Some(targets) = plan.targets() else {
            return Ok(None);
        };
        let slice = crate::state_actions::editor::EditorRowSourceSlice {
            source_line,
            bytes: probe.bytes.clone(),
            native_start: probe.native_start,
            reaches_end: probe.bytes.end == body.len(),
            starts_paint_run: false,
            paint_runs: Some(plan.paint_runs()),
        };
        let started = timing.as_ref().map(|trace| trace.clock.now());
        let html = render(&[source_line], false, &[slice]);
        if let (Some(trace), Some(started)) = (&timing, started) {
            trace.report(
                paint,
                "paragraph-render",
                trace.clock.now() - started,
                probe.bytes.len(),
            );
        }
        let started = timing.as_ref().map(|trace| trace.clock.now());
        paint.set_inner_html(&html);
        drop(html);
        let Some(row) = paint
            .query_selector(".editor-source-line")
            .map_err(|_| ())?
        else {
            return Ok(None);
        };
        if probe.origin > 0.0 {
            let gap = document().create_element("span").map_err(|_| ())?;
            gap.set_attribute(
                "style",
                &format!("display:inline-block;width:{}px", probe.origin),
            )
            .map_err(|_| ())?;
            row.insert_before(&gap, row.first_child().as_ref())
                .map_err(|_| ())?;
        }
        let bounds = row.get_bounding_client_rect();
        if let (Some(trace), Some(started)) = (&timing, started) {
            trace.report(
                paint,
                "paragraph-layout",
                trace.clock.now() - started,
                probe.bytes.len(),
            );
        }
        let started = timing.as_ref().map(|trace| trace.clock.now());
        let Some(rectangles) = super::editor_geometry::paragraph_rectangles(
            &row,
            &body[probe.bytes.clone()],
            probe.glyph_start,
            &targets,
        ) else {
            return Ok(None);
        };
        if !plan.record(
            bounds.width(),
            bounds.height(),
            f64::from(row.scroll_width()),
            &rectangles,
        ) {
            #[cfg(feature = "test-support")]
            web_sys::console::error_1(&format!("wrapped record rejected probe={probe:?} dimensions={}/{}/{} expected={:?} actual={:?}", bounds.width(),bounds.height(),row.scroll_width(),plan.expected_overlap().first(),rectangles.first()).into());
            return Ok(None);
        }
        if let (Some(trace), Some(started)) = (&timing, started) {
            trace.report(
                paint,
                "paragraph-geometry",
                trace.clock.now() - started,
                targets.len(),
            );
        }
        if !coverage_published
            && current()
            && let Some(prefix) = line_height.and_then(|height| plan.viewport_coverage(height))
        {
            coverage(prefix);
            coverage_published = true;
        }
        paint.set_inner_html("");
        probes += 1;
        if probes % openwebide_core::editor::MAX_MEASURE_BATCHES_PER_FRAME == 0 {
            crate::util::yield_frame().await;
        } else {
            crate::util::yield_task().await;
        }
    }
    if !current() {
        return Ok(None);
    }
    let Some((width, geometry, measurements)) = plan.finish_with_measurements() else {
        return Ok(None);
    };
    actions.retain_paragraph_measurements(scope, logical, measurements);
    Ok(Some((width, geometry)))
}

struct ParagraphLayout {
    width: f64,
    height: f64,
    scroll_width: Option<f64>,
    rectangles: Vec<openwebide_core::editor::GlyphRectangle>,
}

fn measure_paragraph_probe(
    paint: &web_sys::Element,
    scope: &crate::state::workspace::EditorRowPaint,
    logical: usize,
    plan: &openwebide_core::editor::ParagraphMeasurementPlan<'_>,
    targets: &[usize],
    render: &impl Fn(&[usize], bool, &[crate::state_actions::editor::EditorRowSourceSlice]) -> String,
) -> Result<Option<ParagraphLayout>, ()> {
    let Some(probe) = plan.probe() else {
        return Ok(None);
    };
    let phase = plan.local_origin();
    let measure_extent = plan.requires_extent();
    let body = scope.projection.line_body(logical).ok_or(())?;
    let source_line = scope.projection.lines()[logical].source_line;
    let timing = ProbeTiming::installed();
    let slice = crate::state_actions::editor::EditorRowSourceSlice {
        source_line,
        bytes: probe.bytes.clone(),
        native_start: probe.native_start,
        reaches_end: probe.bytes.end == body.len(),
        starts_paint_run: true,
        paint_runs: None,
    };
    let started = timing.as_ref().map(|trace| trace.clock.now());
    let html = render(&[source_line], false, &[slice]);
    if let (Some(trace), Some(started)) = (&timing, started) {
        trace.report(
            paint,
            "paragraph-render",
            trace.clock.now() - started,
            probe.bytes.len(),
        );
    }
    let started = timing.as_ref().map(|trace| trace.clock.now());
    paint.set_inner_html(&html);
    drop(html);
    let Some(row) = paint
        .query_selector(".editor-source-line")
        .map_err(|_| ())?
    else {
        return Ok(None);
    };
    // Read overflow from a zero-width unwrapped row so a short final slice is
    // not padded to viewport width. Keep the actual viewport content width in
    // the shared geometry, and let CSS measure ink overhang and pixel rounding.
    let original_style = row.get_attribute("style").unwrap_or_default();
    row.set_attribute("style", &format!("{original_style};width:0px"))
        .map_err(|_| ())?;
    if phase > 0.0 || probe.origin > 0.0 {
        let gap = document().create_element("span").map_err(|_| ())?;
        gap.set_attribute("style", &format!("display:inline-block;width:{phase}px"))
            .map_err(|_| ())?;
        row.insert_before(&gap, row.first_child().as_ref())
            .map_err(|_| ())?;
    }
    let style = window()
        .get_computed_style(paint)
        .map_err(|_| ())?
        .ok_or(())?;
    let padding = |name| -> Result<f64, ()> {
        style
            .get_property_value(name)
            .map_err(|_| ())?
            .trim_end_matches("px")
            .parse()
            .map_err(|_| ())
    };
    let viewport_width =
        f64::from(paint.client_width()) - padding("padding-left")? - padding("padding-right")?;
    let bounds = row.get_bounding_client_rect();
    if let (Some(trace), Some(started)) = (&timing, started) {
        trace.report(
            paint,
            "paragraph-layout",
            trace.clock.now() - started,
            probe.bytes.len(),
        );
    }
    let started = timing.as_ref().map(|trace| trace.clock.now());
    let Some(mut rectangles) = super::editor_geometry::paragraph_rectangles(
        &row,
        &body[probe.bytes.clone()],
        probe.glyph_start,
        targets,
    ) else {
        return Ok(None);
    };
    for rectangle in &mut rectangles {
        rectangle.left += probe.origin - phase;
    }
    // Read the final overflow at document coordinates only after capturing
    // precise local rectangles. Inline layout and transformed overflow round
    // differently at large coordinates; use the original inline layout path.
    if measure_extent && probe.origin > 0.0 {
        let gap = row.first_element_child().ok_or(())?;
        gap.set_attribute(
            "style",
            &format!("display:inline-block;width:{}px", probe.origin.fract()),
        )
        .map_err(|_| ())?;
        // Keep the fractional phase separate from large CSS lengths: a single
        // large fractional width can lose subpixel precision before layout.
        let mut remaining = probe.origin.floor();
        while remaining > 0.0 {
            let width = remaining.min(1_048_576.0);
            let integer_gap = document().create_element("span").map_err(|_| ())?;
            integer_gap
                .set_attribute("style", &format!("display:inline-block;width:{width}px"))
                .map_err(|_| ())?;
            row.insert_before(&integer_gap, Some(&gap))
                .map_err(|_| ())?;
            remaining -= width;
        }
    }
    let source_width =
        measure_extent.then(|| f64::from(row.scroll_width()).max(viewport_width.ceil()));
    if let (Some(trace), Some(started)) = (&timing, started) {
        trace.report(
            paint,
            "paragraph-geometry",
            trace.clock.now() - started,
            targets.len(),
        );
    }
    Ok(Some(ParagraphLayout {
        width: viewport_width,
        height: bounds.height(),
        scroll_width: source_width,
        rectangles,
    }))
}

/// Opt-in diagnostics supplied by the production measurement harness. Keep
/// clocks and callbacks out of ordinary editor preparation.
struct ProbeTiming {
    sink: js_sys::Function,
    clock: web_sys::Performance,
}
impl ProbeTiming {
    fn installed() -> Option<Self> {
        let sink = js_sys::Reflect::get(&js_sys::global(), &"__openwebideEditorProbeTiming".into())
            .ok()?
            .dyn_into::<js_sys::Function>()
            .ok()?;
        Some(Self {
            sink,
            clock: window().performance()?,
        })
    }
    fn report(&self, paint: &web_sys::Element, phase: &str, elapsed: f64, units: usize) {
        // Diagnostics must never interfere with source ownership or publication.
        let _ = self.sink.call4(
            &wasm_bindgen::JsValue::NULL,
            paint.as_ref(),
            &phase.into(),
            &elapsed.into(),
            &units.into(),
        );
    }
}

/// Browser primitives only: styled HTML, exact rectangles and yielding. The
/// shared core chooses batch sizes and validates the completed height table;
/// the editor facade rechecks document/layout ownership before publication.
#[allow(
    clippy::too_many_arguments,
    reason = "Measurement primitives accompany the source-owned facade and batch callbacks"
)]
pub(super) async fn measure_batches(
    actions: EditorActions,
    input: web_sys::HtmlTextAreaElement,
    scope: crate::state::workspace::EditorRowPaint,
    mut plan: openwebide_core::editor::RowMeasurementPlan,
    current: impl Fn() -> bool,
    progress: impl Fn(usize, Option<MeasuredRows>),
    geometry: impl Fn(usize, openwebide_core::editor::MeasuredRowGeometry),
    render: impl Fn(&[usize], bool, &[crate::state_actions::editor::EditorRowSourceSlice]) -> String,
) -> Result<Option<MeasuredRows>, ()> {
    if !current()
        || input
            .parent_element()
            .and_then(|parent| parent.get_attribute("data-editor-account"))
            .as_deref()
            != Some(scope.account_generation.to_string().as_str())
        || !input.is_connected()
        || crate::viewport::editor_scroll(&input).client_width() <= 0
        || crate::viewport::editor_scroll(&input).client_height() <= 0
    {
        return Ok(None);
    }
    let projection = &scope.projection;
    let metrics = &scope.metrics;
    if plan.completed() < projection.lines().len()
        && crate::viewport::settle_editor_font(
            &input,
            openwebide_core::editor::MAX_MEASURE_FONT_WAIT_MS,
        )
        .await
        .unwrap_or(false)
    {
        // Font completion notifications are task-based. Allow their layout
        // observers to invalidate the old ticket before allocating a full probe.
        crate::util::yield_frame().await;
        crate::util::yield_task().await;
    }
    if !current() || !input.is_connected() || metrics_identity(&input).as_ref() != Some(metrics) {
        return Ok(None);
    }
    let (probe, paint) = styled_row_probe(&input)?;
    probe
        .0
        .class_list()
        .add_1("editor-height-measure")
        .map_err(|_| ())?;
    for (name, value) in [
        ("data-measure-view", scope.view_revision),
        ("data-measure-layout", scope.layout_epoch),
        ("data-measure-font", scope.font_epoch),
        ("data-measure-read", scope.read_revision),
        ("data-measure-source", scope.epoch),
        ("data-measure-account", scope.account_generation),
    ] {
        probe
            .0
            .set_attribute(name, &value.to_string())
            .map_err(|_| ())?;
    }
    probe
        .0
        .set_attribute("data-measure-prepared", &scope.prepared_source.to_string())
        .map_err(|_| ())?;
    let lengths = projection
        .lines()
        .iter()
        .map(|line| line.source.len())
        .collect::<Vec<_>>();
    let mut prefix_published = false;
    let mut report = |plan: &openwebide_core::editor::RowMeasurementPlan| {
        let prefix = (!prefix_published)
            .then(|| plan.measured_prefix())
            .flatten()
            .filter(|rows| {
                rows.height() >= f64::from(crate::viewport::editor_scroll(&input).client_height())
                    || rows.len() == openwebide_core::editor::MAX_MEASURE_ROWS
                    || plan.completed() == projection.lines().len()
            });
        prefix_published |= prefix.is_some();
        progress(plan.completed(), prefix);
    };
    report(&plan);
    let timing = ProbeTiming::installed();
    let mut batches = 0_usize;
    while let Some(range) = plan.pending_batch(&lengths) {
        if !current() || !input.is_connected() || metrics_identity(&input).as_ref() != Some(metrics)
        {
            return Ok(None);
        }
        let start = range.start;
        let end = range.end;
        let count = range.len();
        let rows = projection.lines()[start..end]
            .iter()
            .map(|line| line.source_line)
            .collect::<Vec<_>>();
        let wrapped = input
            .parent_element()
            .is_some_and(|parent| parent.class_list().contains("editor-word-wrap"));
        if count == 1 && lengths[start] > openwebide_core::editor::MAX_MEASURE_BYTES {
            let result = if wrapped {
                measure_wrapped_paragraph(
                    actions,
                    &paint,
                    &scope,
                    start,
                    &current,
                    &|prefix| {
                        if current()
                            && input.is_connected()
                            && metrics_identity(&input).as_ref() == Some(metrics)
                        {
                            geometry(
                                start,
                                openwebide_core::editor::MeasuredRowGeometry::WrappedCoverage(
                                    std::sync::Arc::new(prefix),
                                ),
                            );
                        }
                    },
                    &render,
                )
                .await?
                .map(|(width, geometry)| {
                    (
                        width,
                        openwebide_core::editor::MeasuredRowGeometry::Wrapped(geometry),
                    )
                })
            } else {
                measure_paragraph(&paint, &scope, start, &current, &render, Some(actions))
                    .await?
                    .map(|(width, geometry)| {
                        (
                            width,
                            openwebide_core::editor::MeasuredRowGeometry::Horizontal(geometry),
                        )
                    })
            };
            if !current()
                || !input.is_connected()
                || metrics_identity(&input).as_ref() != Some(metrics)
            {
                return Ok(None);
            }
            if let Some((width, measured)) = result {
                if !plan.record_layout(range, &[measured.height()], &[width]) {
                    return Err(());
                }
                geometry(start, measured);
                report(&plan);
                paint.set_inner_html("");
                continue;
            }
        }
        let started = timing.as_ref().map(|trace| trace.clock.now());
        let html = render(&rows, end < projection.lines().len(), &[]);
        if let (Some(trace), Some(started)) = (&timing, started) {
            trace.report(
                &paint,
                "render",
                trace.clock.now() - started,
                lengths[start..end].iter().sum(),
            );
        }
        let started = timing.as_ref().map(|trace| trace.clock.now());
        paint.set_inner_html(&html);
        if let (Some(trace), Some(started)) = (&timing, started) {
            trace.report(&paint, "install", trace.clock.now() - started, html.len());
        }
        drop(html);
        let measured = paint
            .query_selector_all(".editor-source-line")
            .map_err(|_| ())?;
        if usize::try_from(measured.length()).ok() != Some(count) {
            return Err(());
        }
        let mut previous_bottom = None;
        let mut batch = Vec::with_capacity(count);
        let mut widths = Vec::with_capacity(count);
        let mut layout_ms = 0.0;
        let mut geometry_ms = 0.0;
        for index in 0..measured.length() {
            let row: web_sys::Element = measured.item(index).ok_or(())?.unchecked_into();
            let started = timing.as_ref().map(|trace| trace.clock.now());
            let bounds = row.get_bounding_client_rect();
            if previous_bottom.is_some_and(|bottom: f64| (bounds.top() - bottom).abs() > 0.25) {
                return Err(());
            }
            previous_bottom = Some(bounds.bottom());
            batch.push(bounds.height());
            widths.push(f64::from(row.scroll_width()));
            if let (Some(trace), Some(started)) = (&timing, started) {
                layout_ms += trace.clock.now() - started;
            }
            let started = timing.as_ref().map(|trace| trace.clock.now());
            let logical = start + usize::try_from(index).map_err(|_| ())?;
            let line = &projection.lines()[logical];
            let end = projection
                .lines()
                .get(logical + 1)
                .map_or(projection.text().len(), |next| next.visible_start);
            let raw = &projection.text()[line.visible_start..end];
            let body = raw
                .strip_suffix("\r\n")
                .or_else(|| raw.strip_suffix('\n'))
                .unwrap_or(raw);
            let wrapped = input
                .parent_element()
                .is_some_and(|parent| parent.class_list().contains("editor-word-wrap"));
            if let Some(index) = projection.visual_line_index(logical)
                && let Some(measured) = super::editor_geometry::preparation_geometry(
                    &row,
                    body,
                    index,
                    &bounds,
                    wrapped,
                    &|| {
                        current()
                            && input.is_connected()
                            && metrics_identity(&input).as_ref() == Some(metrics)
                    },
                )
                .await
                && current()
            {
                geometry(logical, measured);
            }
            if let (Some(trace), Some(started)) = (&timing, started) {
                geometry_ms += trace.clock.now() - started;
            }
        }
        if let Some(trace) = &timing {
            trace.report(&paint, "layout", layout_ms, count);
            trace.report(&paint, "geometry", geometry_ms, count);
        }
        if !current() || !input.is_connected() || metrics_identity(&input).as_ref() != Some(metrics)
        {
            return Ok(None);
        }
        if !plan.record_layout(range, &batch, &widths) {
            return Err(());
        }
        report(&plan);
        // Release the previous batch before allowing another input/render task.
        paint.set_inner_html("");
        batches += 1;
        if end < projection.lines().len() {
            if batches.is_multiple_of(openwebide_core::editor::MAX_MEASURE_BATCHES_PER_FRAME) {
                crate::util::yield_frame().await;
            } else {
                crate::util::yield_task().await;
            }
        }
    }
    if !current() || !input.is_connected() || metrics_identity(&input).as_ref() != Some(metrics) {
        return Ok(None);
    }
    let rows = plan.finish().ok_or(())?;
    if !extent_supported(&input, &rows) {
        return Err(());
    }
    Ok(Some(rows))
}

pub(super) fn metrics_identity(input: &web_sys::HtmlTextAreaElement) -> Option<String> {
    let style = window().get_computed_style(input).ok()??;
    Some(format!(
        "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
        crate::viewport::editor_scroll(input).client_width(),
        crate::viewport::editor_font_identity(input),
        style.get_property_value("line-height").ok()?,
        style.get_property_value("tab-size").ok()?,
        style.get_property_value("white-space").ok()?,
        style.get_property_value("overflow-wrap").ok()?,
        style.get_property_value("padding-right").ok()?,
        input.offset_left(),
        style.get_property_value("letter-spacing").ok()?,
        style.get_property_value("font-kerning").ok()?,
        style.get_property_value("font-feature-settings").ok()?,
        style.get_property_value("font-variant-ligatures").ok()?
    ))
}

/// Independent complete-renderer oracle for committed partial anchors. The full
/// source is attached only in this test adapter, never in coverage publication.
#[cfg(feature = "test-support")]
pub(super) fn check_wrapped_coverage_geometry(
    input: &web_sys::HtmlTextAreaElement,
    snapshot: &crate::state::workspace::EditorParagraphCoverage,
    html: &str,
) -> Option<bool> {
    if metrics_identity(input).as_ref() != Some(&snapshot.paint.metrics) {
        return None;
    }
    let (_probe, paint) = styled_row_probe(input).ok()?;
    paint.set_inner_html(html);
    let row = paint.query_selector(".editor-source-line").ok()??;
    let body = snapshot.paint.projection.line_body(0)?;
    let geometry =
        openwebide_core::editor::MeasuredRowGeometry::WrappedCoverage(snapshot.coverage.clone());
    let anchors = geometry.anchors(0..snapshot.coverage.glyph_end())?;
    let glyphs = anchors
        .iter()
        .map(|anchor| anchor.glyph)
        .collect::<Vec<_>>();
    let expected = super::editor_geometry::paragraph_rectangles_individual(&row, body, 0, &glyphs)?;
    Some(
        anchors.len() == expected.len()
            && anchors.iter().zip(&expected).all(|(a, b)| {
                a.glyph == b.glyph
                    && (a.left - b.left).abs() <= 0.25
                    && (a.top - b.top).abs() <= 0.25
                    && (a.width - b.width).abs() <= 0.25
                    && (a.height - b.height).abs() <= 0.25
            }),
    )
}

#[cfg(feature = "test-support")]
pub(super) async fn check_wrapped_paragraph_geometry(
    input: &web_sys::HtmlTextAreaElement,
    scope: &crate::state::workspace::EditorRowPaint,
    logical: usize,
    actions: EditorActions,
    render: impl Fn(&[usize], bool, &[crate::state_actions::editor::EditorRowSourceSlice]) -> String,
) -> Result<bool, ()> {
    let (_probe, paint) = styled_row_probe(input)?;
    let Some((width, bounded)) =
        measure_wrapped_paragraph(actions, &paint, scope, logical, &|| true, &|_| {}, &render)
            .await?
    else {
        web_sys::console::error_1(&"bounded wrapped paragraph rejected".into());
        return Ok(false);
    };
    paint.set_inner_html(&render(
        &[scope.projection.lines()[logical].source_line],
        false,
        &[],
    ));
    let row = paint
        .query_selector(".editor-source-line")
        .map_err(|_| ())?
        .ok_or(())?;
    let bounds = row.get_bounding_client_rect();
    let body = scope.projection.line_body(logical).ok_or(())?;
    let index = scope.projection.visual_line_index(logical).ok_or(())?;
    let actual = bounded.anchors(0..index.len() - 1).ok_or(())?;
    let Some(expected) = super::editor_geometry::paragraph_rectangles_individual(
        &row,
        body,
        0,
        &actual.iter().map(|anchor| anchor.glyph).collect::<Vec<_>>(),
    ) else {
        return Ok(false);
    };
    if (width - f64::from(row.scroll_width())).abs() > 0.25
        || (bounded.height - bounds.height()).abs() > 0.25
    {
        web_sys::console::error_1(
            &format!(
                "wrapped extents {width}/{} vs {}/{}",
                bounded.height,
                row.scroll_width(),
                bounds.height()
            )
            .into(),
        );
        return Ok(false);
    }
    if let Some((a, b)) = actual.iter().zip(&expected).find(|(a, b)| {
        (a.left - b.left).abs() > 0.25
            || (a.top - b.top).abs() > 0.25
            || (a.width - b.width).abs() > 0.25
            || (a.height - b.height).abs() > 0.25
    }) {
        web_sys::console::error_1(&format!("wrapped anchor {a:?} vs {b:?}").into());
        return Ok(false);
    }
    Ok(actual.len() == expected.len())
}

#[cfg(feature = "test-support")]
pub(super) async fn check_paragraph_geometry(
    input: &web_sys::HtmlTextAreaElement,
    scope: &crate::state::workspace::EditorRowPaint,
    logical: usize,
    actions: Option<EditorActions>,
    render: impl Fn(&[usize], bool, &[crate::state_actions::editor::EditorRowSourceSlice]) -> String,
) -> Result<bool, ()> {
    let (_probe, paint) = styled_row_probe(input)?;
    let Some((width, bounded)) =
        measure_paragraph(&paint, scope, logical, &|| true, &render, actions).await?
    else {
        web_sys::console::error_1(&"bounded paragraph rejected".into());
        return Ok(false);
    };
    let source_line = scope.projection.lines()[logical].source_line;
    paint.set_inner_html(&render(&[source_line], false, &[]));
    let row = paint
        .query_selector(".editor-source-line")
        .map_err(|_| ())?
        .ok_or(())?;
    let body = scope.projection.line_body(logical).ok_or(())?;
    let index = scope.projection.visual_line_index(logical).ok_or(())?;
    let glyphs = index.len() - 1;
    let bounds = row.get_bounding_client_rect();
    let anchors = bounded.anchors(0..glyphs).ok_or(())?;
    let Some(openwebide_core::editor::MeasuredRowGeometry::Horizontal(complete)) =
        super::editor_geometry::paragraph_geometry(
            &row,
            body,
            index.clone(),
            &bounds,
            anchors.iter().map(|anchor| anchor.glyph).collect(),
        )
    else {
        return Ok(false);
    };
    if (width - f64::from(row.scroll_width())).abs() > 0.25
        || (bounded.height - complete.height).abs() > 0.25
    {
        web_sys::console::error_1(
            &format!(
                "paragraph extent bounded={width}/{} complete={}/{} final={:?}/{:?}",
                bounded.height,
                row.scroll_width(),
                complete.height,
                bounded.anchors(glyphs.saturating_sub(1)..glyphs),
                complete.anchors(glyphs.saturating_sub(1)..glyphs)
            )
            .into(),
        );
        return Ok(false);
    }
    // Independent browser collapsed ranges prove the cached endpoint rectangles
    // are carets, not merely glyph boxes that happen to match another probe.
    for glyph in [0, glyphs] {
        let cached = bounded.caret(glyph).ok_or(())?;
        let (_, native) = index.at(body, glyph).ok_or(())?;
        let caret =
            super::editor::caret_rect(&row, u32::try_from(native).map_err(|_| ())?).ok_or(())?;
        if (cached.left - (caret.left() - bounds.left())).abs() > 0.25
            || (cached.top - (caret.top() - bounds.top())).abs() > 0.25
            || (cached.height - caret.height()).abs() > 0.25
        {
            web_sys::console::error_1(&format!("retained endpoint caret mismatch glyph={glyph} cached={cached:?} actual={}/{}/{}",
                caret.left()-bounds.left(), caret.top()-bounds.top(), caret.height()).into());
            return Ok(false);
        }
    }
    let a = bounded.anchors(0..glyphs).ok_or(())?;
    let b = complete.anchors(0..glyphs).ok_or(())?;
    if let Some((a, b)) = a
        .iter()
        .zip(b)
        .find(|(a, b)| (a.left - b.left).abs() > 0.25 || (a.width - b.width).abs() > 0.25)
    {
        web_sys::console::error_1(&format!("paragraph anchor bounded={a:?} complete={b:?}").into());
    }
    Ok(a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.glyph == b.glyph
                && (a.left - b.left).abs() <= 0.25
                && (a.top - b.top).abs() <= 0.25
                && (a.width - b.width).abs() <= 0.25
                && (a.height - b.height).abs() <= 0.25
        }))
}

pub(super) fn update_measurements(
    actions: EditorActions,
    input: &web_sys::HtmlTextAreaElement,
    overlay: &web_sys::HtmlElement,
    font_changed: bool,
    font_loaded: bool,
    syntax: Option<(bool, std::sync::Arc<openwebide_core::highlight::TokenRows>)>,
    whitespace: bool,
) {
    if !current_editor_target(actions, input)
        || input
            .parent_element()
            .and_then(|parent| parent.get_attribute("data-editor-account"))
            .as_deref()
            != Some(actions.account_generation().to_string().as_str())
    {
        return;
    }
    if font_changed
        || (font_loaded && actions.font_measurements_changed(metrics_identity(input).as_deref()))
    {
        actions.invalidate_measured_font();
    }
    if crate::viewport::editor_scroll(input).client_width() <= 0
        || crate::viewport::editor_scroll(input).client_height() <= 0
    {
        return;
    }
    let Some(projection) = actions.projection() else {
        return;
    };
    // Large or fragmented paints require full-source batch measurements. A
    // visible fragment cannot establish the dimensions of its omitted source.
    if openwebide_core::editor::needs_measured_batches(
        projection.lines().len(),
        projection.text().len(),
    ) {
        return;
    }
    let Some(metrics) = metrics_identity(input) else {
        return;
    };
    if let Some(cached) = actions.measured_rows() {
        if cached.metrics == metrics
            && cached.whitespace == whitespace
            && cached
                .syntax
                .as_ref()
                .zip(syntax.as_ref())
                .is_some_and(|(old, new)| old.0 == new.0 && std::sync::Arc::ptr_eq(&old.1, &new.1))
        {
            return;
        }
        actions.invalidate_measured_rows();
    }
    let revision = actions.view_revision();
    let Ok(Some(paint)) = overlay.query_selector(".editor-highlight-content") else {
        return;
    };
    if paint.get_attribute("data-editor-scope").as_deref()
        != Some(actions.projection_revision().to_string().as_str())
    {
        return;
    }
    let Ok(rows) = paint.query_selector_all(".editor-source-line") else {
        return;
    };
    if usize::try_from(rows.length()).ok() != Some(projection.lines().len()) {
        return;
    }
    let mut heights = Vec::with_capacity(projection.lines().len());
    let mut widths = Vec::with_capacity(projection.lines().len());
    let mut previous_bottom = None;
    for (index, line) in projection.lines().iter().enumerate() {
        let Some(row) = u32::try_from(index).ok().and_then(|index| rows.item(index)) else {
            return;
        };
        let row: web_sys::Element = row.unchecked_into();
        if row.get_attribute("data-line").as_deref()
            != Some((line.source_line + 1).to_string().as_str())
        {
            return;
        }
        let bounds = row.get_bounding_client_rect();
        if previous_bottom.is_some_and(|bottom: f64| (bounds.top() - bottom).abs() > 0.25) {
            return;
        }
        previous_bottom = Some(bounds.bottom());
        heights.push(bounds.height());
        widths.push(f64::from(row.scroll_width()));
    }
    let Some(rows) = MeasuredRows::layout(heights, widths) else {
        return;
    };
    if !extent_supported(input, &rows) {
        return;
    }
    actions.publish_measured_paint(revision, metrics, rows, syntax, whitespace);
}
