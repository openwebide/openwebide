//! Browser text measurements for shared visual-row cursor movement.
use super::editor::{current_editor_target, sync_highlight_scroll};
use crate::state_actions::editor::EditorActions;
use leptos::prelude::*;
use openwebide_core::editor::{MAX_VISUAL_CARETS, VisualCaret, VisualLayout, VisualLineIndex};
use std::collections::BTreeMap;
use wasm_bindgen::{JsCast, prelude::wasm_bindgen};

// The browser adapter reads native ranges in one call. Source coordinates,
// target selection, ownership, geometry validation and fallback stay in Rust.
#[wasm_bindgen(inline_js = r#"
export function paragraphRangeRectangles(range, nodes, spans) {
    const values = new Float64Array(spans.length);
    for (let at = 0; at < spans.length; at += 4) {
        range.setStart(nodes[spans[at]], spans[at + 1]);
        range.setEnd(nodes[spans[at + 2]], spans[at + 3]);
        const rectangles = range.getClientRects();
        let measured = null;
        for (let i = 0; i < rectangles.length; ++i) {
            const rectangle = rectangles.item(i);
            if (rectangle && rectangle.height > 0) {
                measured = rectangle;
                break;
            }
        }
        if (!measured) throw new RangeError('No text rectangle');
        values[at] = measured.left;
        values[at + 1] = measured.top;
        values[at + 2] = measured.width;
        values[at + 3] = measured.height;
    }
    return values;
}
"#)]
extern "C" {
    #[wasm_bindgen(catch, js_name = paragraphRangeRectangles)]
    fn paragraph_range_rectangles(
        range: &web_sys::Range,
        nodes: &js_sys::Array,
        spans: &js_sys::Uint32Array,
    ) -> Result<js_sys::Float64Array, wasm_bindgen::JsValue>;
}

/// Range cloning omits its common ancestor. Preserve every inline paint wrapper
/// between that ancestor and the logical row, including token color/run spans.
fn clone_paint_range(row: &web_sys::Element, range: &web_sys::Range) -> Option<web_sys::Node> {
    let mut result: web_sys::Node = range.clone_contents().ok()?.into();
    let mut ancestor = range.common_ancestor_container().ok()?;
    if ancestor.node_type() == web_sys::Node::TEXT_NODE {
        ancestor = ancestor.parent_node()?;
    }
    while !ancestor.is_same_node(Some(row.as_ref())) {
        // A retained fragment supplies absolute geometry to the caller. It is
        // rebuilt around the new range; nesting its old position would move text.
        if ancestor
            .dyn_ref::<web_sys::Element>()
            .is_some_and(|element| element.class_list().contains("editor-source-fragment"))
        {
            break;
        }
        let wrapper = ancestor.clone_node().ok()?;
        wrapper.append_child(&result).ok()?;
        result = wrapper;
        ancestor = ancestor.parent_node()?;
    }
    Some(result)
}

/// DOM values are rounded first and bounded well inside exact integer precision.
#[allow(
    clippy::cast_possible_truncation,
    reason = "The finite rounded DOM value is range checked before its integer conversion"
)]
fn measured_integer(value: f64) -> Option<i64> {
    if !value.is_finite() || value.abs() > 1_000_000_000_000.0 {
        return None;
    }
    Some(value as i64)
}
fn measured_row(value: f64) -> Option<usize> {
    usize::try_from(measured_integer(value)?).ok()
}

struct TextNodes(Vec<(web_sys::Node, u32, u32)>);
struct TextNodePreparation {
    pending: Vec<(web_sys::Node, bool)>,
    result: Vec<(web_sys::Node, u32, u32)>,
    offset: u32,
    visited: usize,
}
impl TextNodePreparation {
    fn new(row: &web_sys::Element) -> Self {
        Self {
            pending: vec![(row.clone().into(), true)],
            result: Vec::new(),
            offset: 0,
            visited: 0,
        }
    }
    fn advance(&mut self, budget: usize) -> Option<bool> {
        for _ in 0..budget {
            let Some((node, root)) = self.pending.pop() else {
                return Some(true);
            };
            self.visited += 1;
            if self.visited > MAX_VISUAL_CARETS {
                return None;
            }
            // Queue one sibling at a time: a wide parent must not materialize
            // every child before the traversal's budget or node limit applies.
            if !root && let Some(sibling) = node.next_sibling() {
                self.pending.push((sibling, false));
            }
            if node.node_type() == web_sys::Node::TEXT_NODE {
                let length = node.dyn_ref::<web_sys::Text>()?.length();
                self.result.push((node, self.offset, length));
                self.offset = self.offset.checked_add(length)?;
            } else if let Some(child) = node.first_child() {
                self.pending.push((child, false));
            }
        }
        Some(self.pending.is_empty())
    }
}
impl TextNodes {
    fn new(row: &web_sys::Element) -> Option<Self> {
        let mut preparation = TextNodePreparation::new(row);
        while !preparation.advance(MAX_VISUAL_CARETS)? {}
        Some(Self(preparation.result))
    }
    async fn prepare(row: &web_sys::Element, current: &impl Fn() -> bool) -> Option<Self> {
        let mut preparation = TextNodePreparation::new(row);
        let mut batches = 0;
        loop {
            if !current() || !row.is_connected() {
                return None;
            }
            if preparation.advance(openwebide_core::editor::ROW_GEOMETRY_BATCH)? {
                return current().then_some(Self(preparation.result));
            }
            batches += 1;
            if batches % openwebide_core::editor::MAX_MEASURE_BATCHES_PER_FRAME == 0 {
                crate::util::yield_frame().await;
            } else {
                crate::util::yield_task().await;
            }
        }
    }
    #[cfg(feature = "test-support")]
    fn complete(row: &web_sys::Element) -> Option<Self> {
        let mut pending = vec![web_sys::Node::from(row.clone())];
        let mut result = Vec::new();
        let mut offset = 0_u32;
        let mut visited = 0;
        while let Some(node) = pending.pop() {
            visited += 1;
            if visited > MAX_VISUAL_CARETS {
                return None;
            }
            if node.node_type() == web_sys::Node::TEXT_NODE {
                let length = node.dyn_ref::<web_sys::Text>()?.length();
                result.push((node, offset, length));
                offset = offset.checked_add(length)?;
            } else {
                let children = node.child_nodes();
                pending.extend(
                    (0..children.length())
                        .rev()
                        .filter_map(|index| children.item(index)),
                );
            }
        }
        Some(Self(result))
    }
    fn position(&self, offset: u32) -> Option<(&web_sys::Node, u32)> {
        let (index, at) = self.position_index(offset)?;
        Some((&self.0[index].0, at))
    }
    fn position_index(&self, offset: u32) -> Option<(usize, u32)> {
        let index = self
            .0
            .partition_point(|(_, start, length)| start + length <= offset)
            .min(self.0.len().checked_sub(1)?);
        let (_, start, length) = self.0.get(index)?;
        let at = offset.checked_sub(*start)?;
        (at <= *length).then_some((index, at))
    }
}

fn range_rectangle(
    range: &web_sys::Range,
    nodes: &TextNodes,
    start: usize,
    end: usize,
) -> Option<web_sys::DomRect> {
    let (start_node, start_at) = nodes.position(u32::try_from(start).ok()?)?;
    let (end_node, end_at) = nodes.position(u32::try_from(end).ok()?)?;
    range.set_start(start_node, start_at).ok()?;
    range.set_end(end_node, end_at).ok()?;
    let rects = range.get_client_rects()?;
    (0..rects.length())
        .filter_map(|index| rects.item(index))
        .find(|rect| rect.height() > 0.0)
}

struct Glyphs<'a> {
    body: &'a str,
    index: VisualLineIndex,
    nodes: TextNodes,
    range: web_sys::Range,
    measured: BTreeMap<usize, web_sys::DomRect>,
}
impl<'a> Glyphs<'a> {
    fn new(row: &web_sys::Element, body: &'a str, index: Option<VisualLineIndex>) -> Option<Self> {
        Some(Self {
            body,
            index: index.or_else(|| VisualLineIndex::new(body))?,
            nodes: TextNodes::new(row)?,
            range: document().create_range().ok()?,
            measured: BTreeMap::new(),
        })
    }
    fn at(&self, glyph: usize) -> Option<(usize, usize)> {
        self.index.at(self.body, glyph)
    }
    fn rect(&mut self, index: usize) -> Option<web_sys::DomRect> {
        if let Some(rect) = self.measured.get(&index) {
            return Some(rect.clone());
        }
        if self.measured.len() >= MAX_VISUAL_CARETS {
            return None;
        }
        let rect = range_rectangle(
            &self.range,
            &self.nodes,
            self.at(index)?.1,
            self.at(index + 1)?.1,
        )?;
        self.measured.insert(index, rect.clone());
        Some(rect)
    }
    fn row(&mut self, index: usize, metrics: &VisualMetrics) -> Option<usize> {
        measured_row(((self.rect(index)?.top() - metrics.top) / metrics.line_height).floor())
    }
    fn first_at_column(&mut self, minimum: f64, trailing: bool) -> Option<usize> {
        let mut first = 0;
        let mut last = self.index.len().checked_sub(1)?;
        while first < last {
            let middle = first + (last - first) / 2;
            let rect = self.rect(middle)?;
            let column = if trailing { rect.right() } else { rect.left() };
            if column < minimum {
                first = middle + 1;
            } else {
                last = middle;
            }
        }
        Some(first)
    }
    // Visual rows increase in source order even when a row contains bidi text.
    fn first_on_row(&mut self, minimum: usize, metrics: &VisualMetrics) -> Option<usize> {
        let mut start = 0;
        let mut end = self.index.len().checked_sub(1)?;
        while start < end {
            let middle = start + (end - start) / 2;
            if self.row(middle, metrics)? < minimum {
                start = middle + 1;
            } else {
                end = middle;
            }
        }
        Some(start)
    }
}

fn sample_geometry(
    glyphs: &mut Glyphs<'_>,
    bounds: &web_sys::DomRect,
    horizontal: bool,
    samples: Option<Vec<usize>>,
) -> Option<openwebide_core::editor::MeasuredRowGeometry> {
    if !horizontal && !glyphs.index.source_paint_eligible() {
        return None;
    }
    let mut indices = samples.unwrap_or_else(|| glyphs.index.anchor_glyphs().collect::<Vec<_>>());
    indices.dedup();
    let mut plan = openwebide_core::editor::RowGeometryPreparation::new(
        glyphs.index.len() - 1,
        bounds.width(),
        bounds.height(),
        horizontal,
        indices,
    )?;
    while !plan.pending().is_empty() {
        let rectangles = geometry_batch(glyphs, bounds, plan.pending())?;
        if !plan.record(&rectangles) {
            return None;
        }
    }
    plan.finish()
}

/// Exact DOM primitives shared by synchronous viewport queries and cooperative
/// cold preparation. Batch admission and final validation belong to Rust core.
fn geometry_batch(
    glyphs: &mut Glyphs<'_>,
    bounds: &web_sys::DomRect,
    targets: &[usize],
) -> Option<Vec<openwebide_core::editor::GlyphRectangle>> {
    targets
        .iter()
        .map(|&glyph| {
            let rect = glyphs.rect(glyph)?;
            Some(openwebide_core::editor::GlyphRectangle {
                glyph,
                left: rect.left() - bounds.left(),
                top: rect.top() - bounds.top(),
                width: rect.width(),
                height: rect.height(),
            })
        })
        .collect()
}

/// Exact browser rectangles for one bounded source probe. The shared plan owns
/// global glyph numbering, overlap requirements and geometry admission.
pub(super) fn paragraph_rectangles(
    row: &web_sys::Element,
    body: &str,
    glyph_start: usize,
    targets: &[usize],
) -> Option<Vec<openwebide_core::editor::GlyphRectangle>> {
    if row.text_content()?.as_str() != body {
        return None;
    }
    let bounds = row.get_bounding_client_rect();
    let offsets = openwebide_core::editor::visual_line_offsets(body).ok()?;
    let nodes = TextNodes::new(row)?;
    let mut spans = Vec::with_capacity(targets.len().checked_mul(4)?);
    for glyph in targets {
        let local = glyph.checked_sub(glyph_start)?;
        let start = u32::try_from(offsets.get(local)?.1).ok()?;
        let end = u32::try_from(offsets.get(local.checked_add(1)?)?.1).ok()?;
        let (start_node, start_at) = nodes.position_index(start)?;
        let (end_node, end_at) = nodes.position_index(end)?;
        spans.extend([
            u32::try_from(start_node).ok()?,
            start_at,
            u32::try_from(end_node).ok()?,
            end_at,
        ]);
    }
    let native_nodes = js_sys::Array::new();
    for (node, _, _) in &nodes.0 {
        native_nodes.push(node.as_ref());
    }
    let native_spans = js_sys::Uint32Array::from(spans.as_slice());
    let range = document().create_range().ok()?;
    let rectangles = paragraph_range_rectangles(&range, &native_nodes, &native_spans)
        .ok()?
        .to_vec();
    if rectangles.len() != spans.len() {
        return None;
    }
    let left = bounds.left();
    let top = bounds.top();
    Some(
        targets
            .iter()
            .zip(rectangles.as_chunks::<4>().0)
            .map(|(glyph, rect)| openwebide_core::editor::GlyphRectangle {
                glyph: *glyph,
                left: rect[0] - left,
                top: rect[1] - top,
                width: rect[2],
                height: rect[3],
            })
            .collect(),
    )
}

/// Independent per-range reads keep browser geometry oracles separate from the
/// production batch primitive, including DOM exceptions and missing rectangles.
#[cfg(feature = "test-support")]
pub(super) fn paragraph_rectangles_individual(
    row: &web_sys::Element,
    body: &str,
    glyph_start: usize,
    targets: &[usize],
) -> Option<Vec<openwebide_core::editor::GlyphRectangle>> {
    if row.text_content()?.as_str() != body {
        return None;
    }
    let bounds = row.get_bounding_client_rect();
    // Continuation probes are bounded, and their overlap targets are dense.
    // Build coordinates once instead of repeatedly segmenting each sparse
    // checkpoint's prefix; keep general viewport queries on the sparse index.
    let offsets = openwebide_core::editor::visual_line_offsets(body).ok()?;
    let nodes = TextNodes::new(row)?;
    let range = document().create_range().ok()?;
    targets
        .iter()
        .map(|glyph| {
            let local = glyph.checked_sub(glyph_start)?;
            let start = offsets.get(local)?.1;
            let end = offsets.get(local.checked_add(1)?)?.1;
            let rect = range_rectangle(&range, &nodes, start, end)?;
            Some(openwebide_core::editor::GlyphRectangle {
                glyph: *glyph,
                left: rect.left() - bounds.left(),
                top: rect.top() - bounds.top(),
                width: rect.width(),
                height: rect.height(),
            })
        })
        .collect()
}

#[cfg(feature = "test-support")]
pub fn paragraph_rectangles_match_individual(
    row: &web_sys::Element,
    body: &str,
    glyph_start: usize,
    targets: &[usize],
    expected_count: Option<usize>,
) -> bool {
    let batched = paragraph_rectangles(row, body, glyph_start, targets);
    batched.as_ref().map(Vec::len) == expected_count
        && batched == paragraph_rectangles_individual(row, body, glyph_start, targets)
}

/// Reuse the styled logical row already laid out by the cold height probe.
pub(super) async fn preparation_geometry(
    row: &web_sys::Element,
    body: &str,
    index: VisualLineIndex,
    bounds: &web_sys::DomRect,
    wrapped: bool,
    current: &impl Fn() -> bool,
) -> Option<openwebide_core::editor::MeasuredRowGeometry> {
    if !index.source_paint_eligible() {
        return None;
    }
    let text = row.text_content()?;
    if !openwebide_core::editor::textarea_value_matches(
        body,
        text.strip_suffix('\n').unwrap_or(&text),
    ) {
        return None;
    }
    let nodes = TextNodes::prepare(row, current).await?;
    // Enumeration can yield. Revalidate source before using its node offsets.
    let text = row.text_content()?;
    if !openwebide_core::editor::textarea_value_matches(
        body,
        text.strip_suffix('\n').unwrap_or(&text),
    ) {
        return None;
    }
    let mut glyphs = Glyphs {
        body,
        index,
        nodes,
        range: document().create_range().ok()?,
        measured: BTreeMap::new(),
    };
    let mut targets = glyphs.index.anchor_glyphs().collect::<Vec<_>>();
    targets.dedup();
    let mut plan = openwebide_core::editor::RowGeometryPreparation::new(
        glyphs.index.len() - 1,
        bounds.width(),
        bounds.height(),
        !wrapped,
        targets,
    )?;
    let mut batches = 0;
    while !plan.pending().is_empty() {
        if !current() || !row.is_connected() {
            return None;
        }
        let rectangles = geometry_batch(&mut glyphs, bounds, plan.pending())?;
        if !plan.record(&rectangles) {
            return None;
        }
        if !plan.pending().is_empty() {
            batches += 1;
            if batches % openwebide_core::editor::MAX_MEASURE_BATCHES_PER_FRAME == 0 {
                crate::util::yield_frame().await;
            } else {
                crate::util::yield_task().await;
            }
        }
    }
    current().then(|| plan.finish()).flatten()
}

/// Compare lazy DOM enumeration with the original eager traversal and cancel
/// during an actual yield, including a disconnected root and changed source.
#[cfg(feature = "test-support")]
pub async fn cooperative_text_nodes_match_complete_and_cancel(
    row: &web_sys::Element,
    body: &str,
) -> bool {
    macro_rules! fail {
        ($reason:literal) => {{
            web_sys::console::error_1(&$reason.into());
            return false;
        }};
    }
    let Some(expected) = TextNodes::complete(row) else {
        fail!("complete text-node traversal unavailable");
    };
    if expected.0.len() <= openwebide_core::editor::ROW_GEOMETRY_BATCH {
        fail!("fixture must span multiple node batches");
    }
    let mut first = TextNodePreparation::new(row);
    if first.advance(1) != Some(false) || first.pending.len() != 1 {
        fail!("wide root queued more than one child");
    }
    let equal = |actual: &TextNodes| {
        actual.0.len() == expected.0.len()
            && actual.0.iter().zip(&expected.0).all(|(actual, expected)| {
                actual.0.is_same_node(Some(&expected.0))
                    && actual.1 == expected.1
                    && actual.2 == expected.2
            })
    };
    if !TextNodes::new(row).as_ref().is_some_and(&equal)
        || !TextNodes::prepare(row, &|| true)
            .await
            .as_ref()
            .is_some_and(equal)
        || TextNodes::prepare(row, &|| false).await.is_some()
    {
        fail!("text-node ordering/offsets or initial cancellation differ");
    }
    let Some(index) = VisualLineIndex::new(body) else {
        fail!("fixture coordinate index unavailable");
    };
    if !index.source_paint_eligible() {
        fail!("fixture must be an admitted long row");
    }
    let bounds = row.get_bounding_client_rect();
    let Some(expected_geometry) = paragraph_geometry(
        row,
        body,
        index.clone(),
        &bounds,
        index.anchor_glyphs().collect(),
    ) else {
        fail!("complete native geometry unavailable");
    };
    let eligible = index.source_paint_eligible();
    let actual = preparation_geometry(row, body, index, &bounds, false, &|| true).await;
    if actual.as_ref() != Some(&expected_geometry) {
        web_sys::console::error_1(
            &format!(
                "eligible={eligible} bounds={}/{} actual={actual:?} expected={expected_geometry:?}",
                bounds.width(),
                bounds.height()
            )
            .into(),
        );
        fail!("cooperative native geometry differs");
    }
    let current = std::rc::Rc::new(std::cell::Cell::new(true));
    let cancelled = current.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(
            &wasm_bindgen::JsValue::NULL,
        ))
        .await;
        cancelled.set(false);
    });
    let checks = std::cell::Cell::new(0);
    let result = TextNodes::prepare(row, &|| {
        checks.set(checks.get() + 1);
        current.get()
    })
    .await;
    if result.is_some() || current.get() || checks.get() < 2 {
        fail!("text-node cancellation was not observed during yield");
    }
    let Ok(clone) = row.clone_node_with_deep(true) else {
        fail!("could not clone traversal fixture");
    };
    let Ok(clone) = clone.dyn_into::<web_sys::Element>() else {
        fail!("clone is not an element");
    };
    if TextNodes::prepare(&clone, &|| true).await.is_some() {
        fail!("disconnected traversal root was accepted");
    }
    let Some(parent) = row.parent_node() else {
        fail!("traversal fixture parent unavailable");
    };
    if parent.append_child(&clone).is_err() {
        fail!("could not attach mutation fixture");
    }
    let bounds = clone.get_bounding_client_rect();
    let changed = clone.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(
            &wasm_bindgen::JsValue::NULL,
        ))
        .await;
        changed.set_text_content(Some("changed source"));
    });
    let result = if let Some(index) = VisualLineIndex::new(body) {
        preparation_geometry(&clone, body, index, &bounds, false, &|| true).await
    } else {
        clone.remove();
        fail!("mutation fixture index unavailable");
    };
    clone.remove();
    result.is_none()
}

/// Independent complete-renderer oracle and cancellation during an actual yield.
#[cfg(feature = "test-support")]
pub async fn cooperative_geometry_matches_complete_and_cancels(
    row: &web_sys::Element,
    body: &str,
    wrapped: bool,
) -> bool {
    use openwebide_core::editor::{HorizontalGeometry, MeasuredRowGeometry, WrappedGeometry};
    let Some(index) = VisualLineIndex::new(body) else {
        return false;
    };
    let bounds = row.get_bounding_client_rect();
    let Some(mut glyphs) = Glyphs::new(row, body, Some(index.clone())) else {
        return false;
    };
    let mut targets = index.anchor_glyphs().collect::<Vec<_>>();
    targets.dedup();
    if targets.len() <= openwebide_core::editor::ROW_GEOMETRY_BATCH {
        return false;
    }
    let Some(rectangles) = geometry_batch(&mut glyphs, &bounds, &targets) else {
        return false;
    };
    let expected = if wrapped {
        WrappedGeometry::new(index.len() - 1, bounds.width(), bounds.height(), rectangles)
            .map(MeasuredRowGeometry::Wrapped)
    } else {
        HorizontalGeometry::new(index.len() - 1, bounds.width(), bounds.height(), rectangles)
            .map(MeasuredRowGeometry::Horizontal)
    };
    if expected.is_none() {
        return false;
    }
    let current = std::rc::Rc::new(std::cell::Cell::new(true));
    let cancelled = current.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(
            &wasm_bindgen::JsValue::NULL,
        ))
        .await;
        cancelled.set(false);
    });
    let checks = std::cell::Cell::new(0);
    let result = preparation_geometry(row, body, index.clone(), &bounds, wrapped, &|| {
        checks.set(checks.get() + 1);
        current.get()
    })
    .await;
    if result.is_some() || current.get() || checks.get() < 2 {
        return false;
    }
    preparation_geometry(row, body, index, &bounds, wrapped, &|| true).await == expected
}

/// The complete renderer independently measures every anchor selected by the
/// bounded paragraph policy, which can differ from coordinate-index checkpoints.
#[cfg(feature = "test-support")]
pub(super) fn paragraph_geometry(
    row: &web_sys::Element,
    body: &str,
    index: VisualLineIndex,
    bounds: &web_sys::DomRect,
    samples: Vec<usize>,
) -> Option<openwebide_core::editor::MeasuredRowGeometry> {
    let text = row.text_content()?;
    if !openwebide_core::editor::textarea_value_matches(
        body,
        text.strip_suffix('\n').unwrap_or(&text),
    ) {
        return None;
    }
    let mut glyphs = Glyphs::new(row, body, Some(index))?;
    sample_geometry(&mut glyphs, bounds, true, Some(samples))
}

/// Retain the logical row's exact height while copying only its measured visual
/// interval. Cloning the DOM range preserves token/whitespace spans. Verify every
/// retained glyph after reshaping: paragraph-dependent bidi, tabs or ligatures
/// must never silently move text to a different source position.
fn window_paint_row(
    row: &web_sys::Element,
    body: &str,
    window: &openwebide_core::editor::RowPaintWindow,
    line_height: f64,
    index: Option<VisualLineIndex>,
    geometry: &mut Option<openwebide_core::editor::MeasuredRowGeometry>,
) -> Option<()> {
    let text = row.text_content()?;
    if !openwebide_core::editor::textarea_value_matches(
        body,
        text.strip_suffix('\n').unwrap_or(&text),
    ) {
        return None;
    }
    use openwebide_core::editor::RowPaintWindow;
    let bounds = row.get_bounding_client_rect();
    let mut glyphs = Glyphs::new(row, body, index)?;
    let (first, last, top, height, horizontal) = match window {
        RowPaintWindow::Wrapped(window) => {
            if (bounds.height() - window.height).abs() > 0.5 {
                return None;
            }
            let metrics = VisualMetrics {
                identity: String::new(),
                left: bounds.left(),
                top: bounds.top(),
                line_height,
                caret_height: line_height,
                caret_inset: 0.0,
                rows: measured_row((window.height / line_height).round())?,
            };
            (
                glyphs.first_on_row(window.rows.start, &metrics)?,
                glyphs.first_on_row(window.rows.end, &metrics)?,
                window.top,
                window.height,
                false,
            )
        }
        RowPaintWindow::Horizontal(columns) => {
            if (bounds.height() - line_height).abs() > 0.5 {
                return None;
            }
            (
                glyphs.first_at_column(bounds.left() + columns.start, true)?,
                glyphs.first_at_column(bounds.left() + columns.end, false)?,
                0.0,
                bounds.height(),
                true,
            )
        }
    };
    let sampled = sample_geometry(&mut glyphs, &bounds, horizontal, None);
    if horizontal && first == last {
        let original_style = row.get_attribute("style").unwrap_or_default();
        row.set_attribute(
            "style",
            &format!(
                "{original_style};height:{height}px;width:{}px",
                bounds.width()
            ),
        )
        .ok()?;
        row.set_inner_html("");
        row.set_attribute("data-paint-top", "0").ok()?;
        *geometry = sampled;
        return Some(());
    }
    if first == last || last - first > MAX_VISUAL_CARETS {
        return None;
    }
    let byte_start = glyphs.at(first)?.0;
    let byte_end = glyphs.at(last)?.0;
    let native_start = glyphs.at(first)?.1;
    let native_end = if last + 1 == glyphs.index.len() {
        row.get_attribute("data-paint-length")?.parse().ok()?
    } else {
        glyphs.at(last)?.1
    };
    let expected = (first..last)
        .map(|index| glyphs.rect(index))
        .collect::<Option<Vec<_>>>()?;
    let (start, at) = glyphs.nodes.position(u32::try_from(native_start).ok()?)?;
    let (end, to) = glyphs.nodes.position(u32::try_from(native_end).ok()?)?;
    let range = document().create_range().ok()?;
    range.set_start(start, at).ok()?;
    range.set_end(end, to).ok()?;
    let fragment = document().create_element("span").ok()?;
    fragment.set_class_name("editor-source-fragment");
    fragment
        .set_attribute("data-paint-start", &native_start.to_string())
        .ok()?;
    fragment
        .set_attribute("data-paint-end", &native_end.to_string())
        .ok()?;
    fragment
        .set_attribute(
            "style",
            &format!("position:absolute;left:0;right:0;top:{top}px;display:block"),
        )
        .ok()?;
    if horizontal {
        let gap = document().create_element("span").ok()?;
        gap.set_attribute("aria-hidden", "true").ok()?;
        gap.set_attribute(
            "style",
            &format!(
                "display:inline-block;width:{}px",
                expected.first()?.left() - bounds.left()
            ),
        )
        .ok()?;
        fragment.append_child(&gap).ok()?;
    }
    fragment
        .append_child(&clone_paint_range(row, &range)?)
        .ok()?;
    let original = row.inner_html();
    let original_style = row.get_attribute("style").unwrap_or_default();
    row.set_attribute(
        "style",
        &format!(
            "{original_style};height:{height}px;width:{}px",
            bounds.width()
        ),
    )
    .ok()?;
    row.set_inner_html("");
    let valid = (|| {
        row.append_child(&fragment).ok()?;
        let mut painted = Glyphs::new(&fragment, &body[byte_start..byte_end], None)?;
        for (index, old) in expected.iter().enumerate() {
            let new = painted.rect(index)?;
            if (new.left() - old.left()).abs() > 0.5
                || (new.top() - old.top()).abs() > 0.5
                || (new.width() - old.width()).abs() > 0.5
                || (new.height() - old.height()).abs() > 0.5
            {
                return None;
            }
        }
        ((row.get_bounding_client_rect().height() - bounds.height()).abs() <= 0.5).then_some(())
    })();
    if valid.is_none() {
        row.set_inner_html(&original);
        let _ = row.set_attribute("style", &original_style);
    } else {
        let _ = row.set_attribute("data-paint-top", &top.to_string());
        if horizontal {
            let _ = row.set_attribute(
                "data-paint-left",
                &(expected.first()?.left() - bounds.left()).to_string(),
            );
        }
    }
    if valid.is_some() {
        *geometry = sampled;
    }
    valid
}

/// Crop source before asking the browser for layout, using exact anchors from
/// this immutable styled row. Validate retained anchors and restore complete
/// source on any reshaping discrepancy.
fn anchored_row(
    row: &web_sys::Element,
    body: &str,
    index: &VisualLineIndex,
    window: &openwebide_core::editor::RowPaintWindow,
    line_height: f64,
    geometry: &openwebide_core::editor::MeasuredRowGeometry,
) -> Option<()> {
    let interval = geometry.source_interval(window, line_height)?;
    let original = row.inner_html();
    let original_style = row.get_attribute("style").unwrap_or_default();
    if interval.is_empty() {
        row.set_attribute(
            "style",
            &format!(
                "{original_style};height:{}px;width:{}px",
                geometry.height(),
                geometry.width()
            ),
        )
        .ok()?;
        row.set_inner_html("");
        row.set_attribute("data-paint-top", "0").ok()?;
        return Some(());
    }
    let (start_byte, start_native) = index.at(body, interval.start)?;
    let (end_byte, end_native) = index.at(body, interval.end)?;
    let source = body.get(start_byte..end_byte)?;
    if source.len() > openwebide_core::editor::MAX_MEASURE_BYTES {
        return None;
    }
    let end_native = if interval.end + 1 == index.len() {
        row.get_attribute("data-paint-length")?
            .parse::<usize>()
            .ok()?
    } else {
        end_native
    };
    let anchors = geometry.anchors(interval.clone())?;
    let left = anchors.first()?.left;
    let top = if matches!(window, openwebide_core::editor::RowPaintWindow::Wrapped(_)) {
        (anchors.first()?.top / line_height).floor() * line_height
    } else {
        0.0
    };
    let source_start = row
        .get_attribute("data-source-start")
        .map(|offset| offset.parse::<usize>().ok())
        .unwrap_or(Some(0))?;
    let nodes = TextNodes::new(row)?;
    let (start, at) =
        nodes.position(u32::try_from(start_native.checked_sub(source_start)?).ok()?)?;
    let (end, to) = nodes.position(u32::try_from(end_native.checked_sub(source_start)?).ok()?)?;
    let range = document().create_range().ok()?;
    range.set_start(start, at).ok()?;
    range.set_end(end, to).ok()?;
    let fragment = document().create_element("span").ok()?;
    fragment.set_class_name("editor-source-fragment");
    fragment
        .set_attribute("data-paint-start", &start_native.to_string())
        .ok()?;
    fragment
        .set_attribute("data-paint-end", &end_native.to_string())
        .ok()?;
    fragment
        .set_attribute(
            "style",
            &format!("position:absolute;left:0;right:0;top:{top}px;display:block"),
        )
        .ok()?;
    let gap = document().create_element("span").ok()?;
    gap.set_attribute("style", &format!("display:inline-block;width:{left}px"))
        .ok()?;
    fragment.append_child(&gap).ok()?;
    fragment
        .append_child(&clone_paint_range(row, &range)?)
        .ok()?;
    // No geometry read occurred while the complete row was attached.
    row.set_attribute(
        "style",
        &format!(
            "{original_style};height:{}px;width:{}px",
            geometry.height(),
            geometry.width()
        ),
    )
    .ok()?;
    row.set_inner_html("");
    let valid = (|| {
        row.append_child(&fragment).ok()?;
        let bounds = row.get_bounding_client_rect();
        let mut glyphs = Glyphs::new(&fragment, source, None)?;
        for anchor in anchors {
            let rect = glyphs.rect(anchor.glyph - interval.start)?;
            if (rect.left() - bounds.left() - anchor.left).abs() > 0.5
                || (rect.top() - bounds.top() - anchor.top).abs() > 0.5
                || (rect.width() - anchor.width).abs() > 0.5
                || (rect.height() - anchor.height).abs() > 0.5
            {
                return None;
            }
        }
        let full_length = row.get_attribute("data-paint-length")?;
        row.set_attribute(
            "data-paint-length",
            &(end_native - start_native).to_string(),
        )
        .ok()?;
        let mut ignored = None;
        let cropped = window_paint_row(row, source, window, line_height, None, &mut ignored);
        row.set_attribute("data-paint-length", &full_length).ok()?;
        cropped?;
        let fragments = row
            .query_selector_all(":scope > .editor-source-fragment")
            .ok()?;
        for at in 0..fragments.length() {
            let fragment = fragments.item(at)?.dyn_into::<web_sys::Element>().ok()?;
            for attribute in ["data-paint-start", "data-paint-end"] {
                let local = fragment.get_attribute(attribute)?.parse::<usize>().ok()?;
                fragment
                    .set_attribute(attribute, &(local + start_native).to_string())
                    .ok()?;
            }
        }
        Some(())
    })();
    if valid.is_none() {
        row.set_inner_html(&original);
        let _ = row.set_attribute("style", &original_style);
    }
    valid
}

/// A temporary styled probe supplies exact glyph boundaries. Source ownership,
/// layout identity and window policy come from the common editor facade.
pub(super) fn window_paint(
    actions: EditorActions,
    input: &web_sys::HtmlTextAreaElement,
    render: impl Fn(&[crate::state_actions::editor::EditorRowSourceSlice]) -> String,
    windows: &[(usize, openwebide_core::editor::RowPaintWindow)],
    cache: &mut crate::state_actions::editor::EditorFragmentCache,
) -> Option<String> {
    if windows.is_empty() {
        return None;
    }
    if !current_editor_target(actions, input) {
        return None;
    }
    let identity = super::editor_rows::metrics_identity(input)?;
    if actions
        .measured_rows()
        .is_some_and(|measured| measured.metrics != identity)
    {
        return None;
    }
    let projection = actions.projection()?;
    let revision = actions.view_revision();
    let style = window().get_computed_style(input).ok()??;
    let line_height = style
        .get_property_value("line-height")
        .ok()?
        .trim_end_matches("px")
        .parse::<f64>()
        .ok()?;
    let source = actions.source();
    let (_probe, paint) = super::editor_rows::styled_row_probe(input).ok()?;
    if windows.iter().any(|(_, window)| {
        matches!(
            window,
            openwebide_core::editor::RowPaintWindow::Horizontal(_)
        )
    }) {
        paint
            .dyn_ref::<web_sys::HtmlElement>()?
            .style()
            .set_property(
                "width",
                &format!(
                    "{}px",
                    crate::viewport::editor_scroll(input).scroll_width() + input.offset_left()
                ),
            )
            .ok()?;
    }
    let slices = windows
        .iter()
        .filter_map(|(index, window)| actions.row_source_slice(cache, *index, window, line_height))
        .collect::<Vec<_>>();
    if actions.measured_rows().is_none()
        && actions.paragraph_coverage().is_some()
        && windows.iter().any(|(index, _)| {
            !slices
                .iter()
                .any(|slice| slice.source_line == projection.lines()[*index].source_line)
        })
    {
        for (index, _) in windows {
            actions.forget_measured_row_geometry(cache, *index);
        }
        return None;
    }
    paint.set_inner_html(&render(&slices));
    let mut changed = false;
    for (index, window) in windows {
        let line = &projection.lines()[*index];
        let raw = &source[line.source.clone()];
        let body = raw
            .strip_suffix("\r\n")
            .or_else(|| raw.strip_suffix('\n'))
            .unwrap_or(raw);
        let row = paint
            .query_selector(&format!(
                ".editor-source-line[data-line='{}']",
                line.source_line + 1
            ))
            .ok()??;
        if let Some(geometry) = actions.measured_row_geometry(cache, *index)
            && let Some(source_index) = projection.visual_line_index(*index)
            && anchored_row(&row, body, &source_index, window, line_height, &geometry).is_some()
        {
            changed = true;
            continue;
        }
        if slices
            .iter()
            .any(|slice| slice.source_line == line.source_line)
        {
            // Never measure a partial row as complete. The caller renders the full
            // source on a shaping mismatch; the next probe obtains fresh anchors.
            actions.forget_measured_row_geometry(cache, *index);
            return None;
        }
        let mut geometry = None;
        changed |= window_paint_row(
            &row,
            body,
            window,
            line_height,
            projection.visual_line_index(*index),
            &mut geometry,
        )
        .is_some();
        if let Some(geometry) = geometry {
            actions.retain_measured_row_geometry(cache, *index, geometry);
        }
    }
    if !changed
        || actions.view_revision() != revision
        || !current_editor_target(actions, input)
        || super::editor_rows::metrics_identity(input).as_ref() != Some(&identity)
    {
        return None;
    }
    Some(paint.inner_html())
}

pub(super) struct VisualMetrics {
    pub identity: String,
    pub left: f64,
    pub top: f64,
    pub line_height: f64,
    pub caret_height: f64,
    pub caret_inset: f64,
    pub rows: usize,
}

/// Scroll events can follow queued commands by a frame. Align the current
/// source paint before reading its browser coordinates, including retained
/// carets and complete-layout fallbacks.
fn current_source_paint(
    actions: EditorActions,
    input: &web_sys::HtmlTextAreaElement,
) -> Option<web_sys::Element> {
    if !current_editor_target(actions, input) {
        return None;
    }
    let paint = input
        .parent_element()?
        .query_selector(".editor-highlight-content")
        .ok()??;
    if paint.get_attribute("data-editor-scope").as_deref()
        != Some(actions.projection_revision().to_string().as_str())
    {
        return None;
    }
    let overlay = paint
        .parent_element()?
        .dyn_into::<web_sys::HtmlElement>()
        .ok()?;
    sync_highlight_scroll(input, &overlay);
    Some(paint)
}

pub(super) fn visual_metrics(
    actions: EditorActions,
    input: &web_sys::HtmlTextAreaElement,
) -> Option<VisualMetrics> {
    let paint = current_source_paint(actions, input)?;
    let first = paint.query_selector(".editor-source-line").ok()??;
    let last = paint
        .query_selector(".editor-source-line:last-child")
        .ok()??;
    let first_rect = first.get_bounding_client_rect();
    let last_rect = last.get_bounding_client_rect();
    let style = window().get_computed_style(input).ok()??;
    let line_height: f64 = style
        .get_property_value("line-height")
        .ok()?
        .trim_end_matches("px")
        .parse()
        .ok()?;
    if !line_height.is_finite() || line_height <= 0.0 {
        return None;
    }
    let geometry = super::editor_rows::metrics_identity(input)?;
    if actions
        .measured_rows()
        .is_some_and(|rows| rows.metrics != geometry)
    {
        return None;
    }
    let identity = format!("{geometry}:{}", actions.view_revision());
    let window_top: f64 = paint.get_attribute("data-viewport-top")?.parse().ok()?;
    let document_height: f64 = paint.get_attribute("data-document-height")?.parse().ok()?;
    if !window_top.is_finite()
        || window_top < 0.0
        || !document_height.is_finite()
        || document_height < 0.0
    {
        return None;
    }
    let caret_offset = first
        .query_selector(".editor-source-fragment")
        .ok()?
        .and_then(|fragment| fragment.get_attribute("data-paint-start"))
        .and_then(|offset| offset.parse().ok())
        .unwrap_or(0);
    let paint_top = first
        .get_attribute("data-paint-top")
        .and_then(|top| top.parse::<f64>().ok())
        .unwrap_or(0.0);
    let caret = super::editor::caret_rect(&first, caret_offset).filter(|rect| rect.height() > 0.0);
    Some(VisualMetrics {
        identity,
        left: first_rect.left(),
        top: first_rect.top() - window_top,
        line_height,
        caret_height: caret.as_ref().map_or(line_height, web_sys::DomRect::height),
        caret_inset: caret
            .as_ref()
            .map_or(0.0, |caret| caret.top() - first_rect.top() - paint_top),
        rows: measured_row(
            (if document_height > 0.0 {
                document_height
            } else {
                last_rect.bottom() - first_rect.top()
            } / line_height)
                .round(),
        )?,
    })
}

pub(super) fn visual_layout(
    actions: EditorActions,
    input: &web_sys::HtmlTextAreaElement,
) -> Option<VisualLayout> {
    if !current_editor_target(actions, input) {
        return None;
    }
    let source = actions.source();
    let projection = actions.projection()?;
    let parent = input.parent_element()?;
    let paint = current_source_paint(actions, input)?;
    let painted = paint.text_content()?;
    let start: usize = paint.get_attribute("data-textarea-start")?.parse().ok()?;
    let end = start.checked_add(painted.encode_utf16().count())?;
    let start_byte = projection.textarea_to_byte(start);
    let end_byte = projection.textarea_to_byte(end);
    if !parent.class_list().contains("highlight-ready")
        || projection.byte_to_textarea(start_byte).ok()? != start
        || projection.byte_to_textarea(end_byte).ok()? != end
        || !openwebide_core::editor::textarea_value_matches(
            &projection.text()[start_byte..end_byte],
            &painted,
        )
    {
        return None;
    }
    let metrics = visual_metrics(actions, input)?;
    if metrics.rows == 0 {
        return None;
    }
    measured_layout(
        actions,
        input,
        &source,
        projection,
        Some(&metrics),
        |index| {
            parent
                .query_selector(&format!(".editor-source-line[data-line='{}']", index + 1))
                .ok()
                .flatten()
        },
    )
}

fn selected_lines(
    actions: EditorActions,
    source: &str,
    projection: &openwebide_core::editor::FoldProjection,
) -> Option<BTreeMap<usize, Vec<(usize, usize)>>> {
    let mut selected = BTreeMap::<usize, Vec<(usize, usize)>>::new();
    for (caret, selection) in actions.selections(source).iter().enumerate() {
        let visible = projection.visible_selection(*selection).ok()?;
        let index = projection
            .lines()
            .partition_point(|line| line.visible_start <= visible.head)
            .saturating_sub(1);
        for row in index.saturating_sub(1)..=(index + 1).min(projection.lines().len() - 1) {
            selected.entry(row).or_default();
        }
        selected
            .entry(index)
            .or_default()
            .push((caret, visible.head));
    }
    Some(selected)
}

struct RowMeasurement<'a> {
    actions: EditorActions,
    source: &'a str,
    projection: &'a openwebide_core::editor::FoldProjection,
    identity: &'a str,
    line_height: f64,
}
impl RowMeasurement<'_> {
    fn sample(
        &self,
        index: usize,
        cursors: &[(usize, usize)],
        row: web_sys::Element,
    ) -> Option<(openwebide_core::editor::VisualLineRows, Vec<VisualCaret>)> {
        use openwebide_core::editor::{VisualLineRows, visual_probe_rows, visual_row_id};
        let line = &self.projection.lines()[index];
        let raw = &self.source[line.source.clone()];
        let body = raw
            .strip_suffix("\r\n")
            .or_else(|| raw.strip_suffix('\n'))
            .unwrap_or(raw);
        let painted = row.text_content()?;
        if !openwebide_core::editor::textarea_value_matches(
            body,
            painted.strip_suffix('\n').unwrap_or(&painted),
        ) {
            return None;
        }
        let bounds = row.get_bounding_client_rect();
        let rows = measured_row((bounds.height() / self.line_height).round())?;
        if rows == 0 || (bounds.height() - rows as f64 * self.line_height).abs() > 0.5 {
            return None;
        }
        let metrics = VisualMetrics {
            identity: self.identity.into(),
            left: bounds.left(),
            top: bounds.top(),
            line_height: self.line_height,
            caret_height: self.line_height,
            caret_inset: 0.0,
            rows,
        };
        let mut glyphs = if body.is_empty() {
            None
        } else {
            Some(Glyphs::new(
                &row,
                body,
                self.projection.visual_line_index(index),
            )?)
        };
        let mut current = Vec::new();
        for &(caret, head) in cursors {
            let within =
                if let Some(goal) = self.actions.visual_caret(self.source, caret, self.identity) {
                    goal.row.checked_sub(line.visible_start)?
                } else if body.is_empty() {
                    0
                } else if body.ends_with('\r') && head == line.visible_start + body.len() {
                    rows - 1
                } else {
                    let glyphs = glyphs.as_mut()?;
                    let byte = head.saturating_sub(line.visible_start).min(body.len());
                    let glyph = glyphs
                        .index
                        .index_at_byte(body, byte)?
                        .min(glyphs.index.len() - 2);
                    glyphs.row(glyph, &metrics)?
                };
            current.push(within);
        }
        let mut carets = Vec::new();
        for wanted in visual_probe_rows(rows, &current).ok()? {
            let id = visual_row_id(self.projection, index, wanted).ok()?;
            if body.is_empty() || (body.ends_with('\r') && wanted + 1 == rows) {
                carets.push(VisualCaret {
                    offset: line.visible_start + body.len(),
                    column: 0,
                    row: id,
                });
                continue;
            }
            let glyphs = glyphs.as_mut()?;
            let start = glyphs.first_on_row(wanted, &metrics)?;
            let end = glyphs.first_on_row(wanted + 1, &metrics)?;
            if carets.len() + (end - start) * 2 > MAX_VISUAL_CARETS {
                return None;
            }
            for glyph in start..end {
                let rect = glyphs.rect(glyph)?;
                for (offset, x) in [
                    (glyphs.at(glyph)?.0, rect.left()),
                    (glyphs.at(glyph + 1)?.0, rect.right()),
                ] {
                    carets.push(VisualCaret {
                        offset: line.visible_start + offset,
                        column: measured_integer(((x - bounds.left()) * 64.0).round())?,
                        row: id,
                    });
                }
            }
        }
        Some((VisualLineRows { line: index, rows }, carets))
    }
}

fn measurement_context<'a>(
    actions: EditorActions,
    input: &web_sys::HtmlTextAreaElement,
    source: &'a str,
    projection: &'a openwebide_core::editor::FoldProjection,
    identity: &'a str,
) -> Option<RowMeasurement<'a>> {
    let style = window().get_computed_style(input).ok()??;
    let line_height: f64 = style
        .get_property_value("line-height")
        .ok()?
        .trim_end_matches("px")
        .parse()
        .ok()?;
    if !line_height.is_finite() || line_height <= 0.0 {
        return None;
    }
    Some(RowMeasurement {
        actions,
        source,
        projection,
        identity,
        line_height,
    })
}

fn measured_layout(
    actions: EditorActions,
    input: &web_sys::HtmlTextAreaElement,
    source: &crate::state::workspace::EditorText,
    projection: openwebide_core::editor::FoldProjection,
    global: Option<&VisualMetrics>,
    row_element: impl Fn(usize) -> Option<web_sys::Element>,
) -> Option<VisualLayout> {
    let geometry = super::editor_rows::metrics_identity(input)?;
    let revision = actions.view_revision();
    let identity = format!("{geometry}:{revision}");
    let context = measurement_context(actions, input, source, &projection, &identity)?;
    let mut lines = Vec::new();
    let mut carets = Vec::new();
    for (index, cursors) in selected_lines(actions, source, &projection)? {
        let row = row_element(projection.lines()[index].source_line)?;
        if let (Some(global), Some(measured)) = (global, actions.measured_rows())
            && (row.get_bounding_client_rect().top() - global.top - measured.rows.top(index)?).abs()
                > 0.5
        {
            return None;
        }
        let (line, sampled) = context.sample(index, &cursors, row)?;
        lines.push(line);
        carets.extend(sampled);
        if carets.len() > MAX_VISUAL_CARETS {
            return None;
        }
    }
    if actions.view_revision() != revision
        || super::editor_rows::metrics_identity(input).as_ref() != Some(&geometry)
        || !current_editor_target(actions, input)
    {
        return None;
    }
    VisualLayout::neighborhood_source(source.shared(), projection, identity, &lines, carets).ok()
}

/// Cold measurement uses the same exact row sampler as warm paint, with bounded
/// temporary DOM batches. No document-height table is needed for neighbor links.
pub(super) fn neighborhood_layout(
    actions: EditorActions,
    input: &web_sys::HtmlTextAreaElement,
    render: impl Fn(&[usize], bool) -> String,
) -> Option<VisualLayout> {
    if !current_editor_target(actions, input)
        || crate::viewport::editor_scroll(input).client_width() <= 0
        || crate::viewport::editor_scroll(input).client_height() <= 0
    {
        return None;
    }
    let revision = actions.view_revision();
    let geometry = super::editor_rows::metrics_identity(input)?;
    let identity = format!("{geometry}:{revision}");
    let source = actions.source();
    let projection = actions.projection()?;
    let selected = selected_lines(actions, &source, &projection)?
        .into_iter()
        .collect::<Vec<_>>();
    let context = measurement_context(actions, input, &source, &projection, &identity)?;
    let (_probe, paint) = super::editor_rows::styled_row_probe(input).ok()?;
    let mut lines = Vec::new();
    let mut carets = Vec::new();
    let mut start = 0;
    while start < selected.len() {
        let count = openwebide_core::editor::row_measurement_batch(
            selected[start..]
                .iter()
                .map(|(index, _)| projection.lines()[*index].source.len()),
        );
        if count == 0 {
            return None;
        }
        let batch = &selected[start..start + count];
        let indices = batch
            .iter()
            .map(|(index, _)| projection.lines()[*index].source_line)
            .collect::<Vec<_>>();
        let suffix = batch.last()?.0 + 1 < projection.lines().len();
        paint.set_inner_html(&render(&indices, suffix));
        for (index, cursors) in batch {
            let row = paint
                .query_selector(&format!(
                    ".editor-source-line[data-line='{}']",
                    projection.lines()[*index].source_line + 1
                ))
                .ok()??;
            let (line, sampled) = context.sample(*index, cursors, row)?;
            lines.push(line);
            carets.extend(sampled);
            if carets.len() > MAX_VISUAL_CARETS {
                return None;
            }
        }
        paint.set_inner_html("");
        start += count;
    }
    if actions.view_revision() != revision
        || super::editor_rows::metrics_identity(input).as_ref() != Some(&geometry)
        || !current_editor_target(actions, input)
    {
        return None;
    }
    VisualLayout::neighborhood_source(source.shared(), projection, identity, &lines, carets).ok()
}

/// Current painted coverage already contains exact browser caret geometry. Do
/// not prepare a complete row when a source-owned fragment contains this caret.
pub(super) fn painted_caret_rect(
    actions: EditorActions,
    input: &web_sys::HtmlTextAreaElement,
    cache: &crate::state_actions::editor::EditorFragmentCache,
    offset: usize,
) -> Option<web_sys::DomRect> {
    if !current_editor_target(actions, input) {
        return None;
    }
    let revision = actions.view_revision();
    let identity = super::editor_rows::metrics_identity(input)?;
    let (line, column) = actions.painted_source_caret(cache, offset, &identity)?;
    let paint = current_source_paint(actions, input)?;
    let row = paint
        .query_selector(&format!(".editor-source-line[data-line='{}']", line + 1))
        .ok()??;
    let caret = super::editor::caret_rect(&row, u32::try_from(column).ok()?)?;
    if caret.height() <= 0.0
        || ![caret.left(), caret.top(), caret.width(), caret.height()]
            .iter()
            .all(|value| value.is_finite())
        || actions.view_revision() != revision
        || !current_editor_target(actions, input)
        || super::editor_rows::metrics_identity(input).as_ref() != Some(&identity)
    {
        return None;
    }
    Some(caret)
}

/// Translate source-owned exact anchors without preparing a complete movement
/// neighborhood. Unsupported or absent anchors retain the existing DOM fallback.
pub(super) fn measured_caret_rect(
    actions: EditorActions,
    input: &web_sys::HtmlTextAreaElement,
    cache: &mut crate::state_actions::editor::EditorFragmentCache,
    offset: usize,
) -> Option<web_sys::DomRect> {
    if !current_editor_target(actions, input) {
        return None;
    }
    let revision = actions.view_revision();
    let identity = super::editor_rows::metrics_identity(input)?;
    let (row, caret) = actions.measured_source_caret(cache, offset, &identity)?;
    let metrics = visual_metrics(actions, input)?;
    let top = actions.measured_rows()?.rows.top(row)?;
    if actions.view_revision() != revision
        || !current_editor_target(actions, input)
        || super::editor_rows::metrics_identity(input).as_ref() != Some(&identity)
    {
        return None;
    }
    web_sys::DomRect::new_with_x_and_y_and_width_and_height(
        metrics.left + caret.left,
        metrics.top + top + caret.top,
        0.0,
        caret.height,
    )
    .ok()
}

/// Reveal an omitted source caret using the same validated neighborhood layout
/// as wrapped movement, translated from probe coordinates into the viewport.
pub(super) fn layout_caret_rect(
    actions: EditorActions,
    input: &web_sys::HtmlTextAreaElement,
    layout: &VisualLayout,
    offset: usize,
) -> Option<web_sys::DomRect> {
    if !current_editor_target(actions, input) {
        return None;
    }
    let metrics = visual_metrics(actions, input)?;
    let visible = actions.projection()?.visible_offset(offset).ok()?;
    let caret = layout.caret(visible)?;
    web_sys::DomRect::new_with_x_and_y_and_width_and_height(
        metrics.left + caret.column as f64 / 64.0,
        caret_top(actions, input, &metrics, caret.row)?,
        0.0,
        metrics.caret_height,
    )
    .ok()
}

/// A single source-point measurement also works when an unwrapped visual row
/// contains more glyphs than a movement neighborhood can admit.
pub(super) fn probe_caret_rect(
    actions: EditorActions,
    input: &web_sys::HtmlTextAreaElement,
    offset: usize,
    render: impl Fn(&[usize], bool) -> String,
) -> Option<web_sys::DomRect> {
    if !current_editor_target(actions, input) {
        return None;
    }
    let identity = super::editor_rows::metrics_identity(input)?;
    let revision = actions.view_revision();
    let projection = actions.projection()?;
    let visible = projection.visible_offset(offset).ok()?;
    let index = projection
        .lines()
        .partition_point(|line| line.visible_start <= visible)
        .checked_sub(1)?;
    let line = &projection.lines()[index];
    let actual = current_source_paint(actions, input)?
        .query_selector(&format!(
            ".editor-source-line[data-line='{}']",
            line.source_line + 1
        ))
        .ok()??
        .get_bounding_client_rect();
    let (_probe, paint) = super::editor_rows::styled_row_probe(input).ok()?;
    paint.set_inner_html(&render(
        &[line.source_line],
        index + 1 < projection.lines().len(),
    ));
    let row = paint.query_selector(".editor-source-line").ok()??;
    let column = projection
        .byte_to_textarea(visible)
        .ok()?
        .checked_sub(line.textarea_start)?;
    let caret = super::editor::caret_rect(&row, u32::try_from(column).ok()?)?;
    let bounds = row.get_bounding_client_rect();
    if actions.view_revision() != revision
        || !current_editor_target(actions, input)
        || super::editor_rows::metrics_identity(input).as_ref() != Some(&identity)
    {
        return None;
    }
    web_sys::DomRect::new_with_x_and_y_and_width_and_height(
        actual.left() + caret.left() - bounds.left(),
        actual.top() + caret.top() - bounds.top(),
        caret.width(),
        caret.height(),
    )
    .ok()
}

/// Translate a line-relative caret ID only when painting into a real viewport.
pub(super) fn caret_top(
    actions: EditorActions,
    input: &web_sys::HtmlTextAreaElement,
    metrics: &VisualMetrics,
    id: usize,
) -> Option<f64> {
    let projection = actions.projection()?;
    let index = projection
        .lines()
        .partition_point(|line| line.visible_start <= id)
        .checked_sub(1)?;
    let within = id.checked_sub(projection.lines()[index].visible_start)?;
    let top = if let Some(rows) = actions.measured_rows() {
        metrics.top + rows.rows.top(index)?
    } else if !actions.preferences().word_wrap && projection.has_uniform_rows() {
        // Fixed source-row heights also locate carets outside the painted window.
        metrics.top + f64::from(u32::try_from(index).ok()?) * metrics.line_height
    } else {
        input
            .parent_element()?
            .query_selector(&format!(
                ".editor-source-line[data-line='{}']",
                projection.lines()[index].source_line + 1
            ))
            .ok()??
            .get_bounding_client_rect()
            .top()
    };
    Some(top + within as f64 * metrics.line_height + metrics.caret_inset)
}
