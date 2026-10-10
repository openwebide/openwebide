use leptos::prelude::*;
use openwebide_core::{FileDiff, FileKind, diff_side_by_side_detailed, highlight::TokenKind};
use web_sys::wasm_bindgen::JsCast;

use crate::components::chat_pane::render_markdown;
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, CheckboxField, Icon, IconButton, IconName, PanelSearchRow,
    SegmentOption, SegmentedControl,
};
use crate::state::{git::GitState, projects::ProjectsState, workspace::WorkspaceState};
use crate::state_actions::editor::{EditorActions, EditorCommand};

pub(super) use crate::viewport::current_editor_target;

/// The DOM adapter supplies clipboard access; source selection and transactions
/// stay in EditorActions and the shared document engine.
fn editor_clipboard_copy(
    actions: EditorActions,
    textarea: &web_sys::HtmlTextAreaElement,
    event: &web_sys::ClipboardEvent,
    cut: bool,
    read_only: bool,
    error: RwSignal<Option<String>>,
) {
    let selection = projected_selection(actions, textarea);
    if selection.range().is_empty()
        && actions
            .current_selections()
            .iter()
            .skip(1)
            .all(|selection| selection.range().is_empty())
    {
        return;
    }
    // Clipboard writes must succeed before a source cut can remove any text.
    if cut {
        event.prevent_default();
    }
    match actions.clipboard_content(selection) {
        Ok(Some(content)) => {
            if let Some(clipboard) = event.clipboard_data()
                && clipboard.set_data("text/plain", &content.text).is_ok()
            {
                // Browsers/apps may strip custom types; plain text remains usable.
                let _ = clipboard.set_data(
                    openwebide_core::editor::CLIPBOARD_SELECTIONS_MIME,
                    &content.metadata,
                );
                event.prevent_default();
                error.set(None);
                if cut && !read_only {
                    match actions.cut(selection) {
                        Ok(Some(selection)) => {
                            refresh_editor_folds(actions);
                            render_editor_selection(actions, textarea, selection, false);
                        }
                        Err(failure) => error.set(Some(failure.to_string())),
                        Ok(None) => {}
                    }
                }
            } else {
                event.prevent_default();
                error.set(Some(
                    "Could not write the clipboard; the document was kept unchanged".into(),
                ));
            }
        }
        Err(failure) => {
            event.prevent_default();
            error.set(Some(failure.to_string()));
        }
        Ok(None) => {}
    }
}

#[derive(Clone, Copy)]
pub(super) struct EditorPaint {
    pub ticket: u64,
    pub flush: Callback<()>,
    pub neighborhood: Callback<(), Option<openwebide_core::editor::VisualLayout>>,
    pub caret: Callback<usize, Option<web_sys::DomRect>>,
}

fn editor_selection_key(
    actions: EditorActions,
    textarea: &web_sys::HtmlTextAreaElement,
    event: &web_sys::KeyboardEvent,
    error: RwSignal<Option<String>>,
    motion_adapter: super::editor_motion::MotionAdapter,
) -> bool {
    use openwebide_core::editor::{SelectionCommand as Command, SelectionMotion as Motion};
    let modified = event.ctrl_key() || event.meta_key();
    let multiple = actions.selection_count() > 1;
    if modified && !event.alt_key() && event.key().eq_ignore_ascii_case("a") {
        let selection = openwebide_core::editor::Selection {
            anchor: 0,
            head: actions.source_len(),
        };
        match actions.record_selection(selection) {
            Ok(()) => {
                render_editor_selection(actions, textarea, selection, false);
                error.set(None);
            }
            Err(failure) => error.set(Some(failure.to_string())),
        }
        return true;
    }
    if !modified && !event.alt_key() && matches!(event.key().as_str(), "PageUp" | "PageDown") {
        let selection = projected_selection(actions, textarea);
        if let Err(failure) = actions.record_native_selection(selection) {
            error.set(Some(failure.to_string()));
        } else {
            motion_adapter.page(textarea, event.key() == "PageDown", event.shift_key());
        }
        return true;
    }
    let command = match event.key().as_str() {
        "d" | "D" if modified && !event.alt_key() && !event.shift_key() => {
            Some(Command::NextOccurrence)
        }
        "l" | "L" if modified && event.shift_key() && !event.alt_key() => {
            Some(Command::AllOccurrences)
        }
        "ArrowUp" if modified && event.alt_key() => Some(Command::AddAbove),
        "ArrowDown" if modified && event.alt_key() => Some(Command::AddBelow),
        "ArrowRight" if event.alt_key() && event.shift_key() && !modified => Some(Command::Expand),
        "ArrowLeft" if event.alt_key() && event.shift_key() && !modified => Some(Command::Shrink),
        "Escape" if multiple && !modified && !event.alt_key() => Some(Command::Single),
        _ => None,
    };
    let motion = if command.is_none() {
        match event.key().as_str() {
            "ArrowLeft" if event.meta_key() => Some(Motion::LineStart),
            "ArrowRight" if event.meta_key() => Some(Motion::LineEnd),
            "ArrowLeft" => Some(if event.ctrl_key() || event.alt_key() {
                Motion::WordLeft
            } else {
                Motion::Left
            }),
            "ArrowRight" => Some(if event.ctrl_key() || event.alt_key() {
                Motion::WordRight
            } else {
                Motion::Right
            }),
            "ArrowUp" if event.meta_key() => Some(Motion::DocumentStart),
            "ArrowDown" if event.meta_key() => Some(Motion::DocumentEnd),
            "ArrowUp" if !modified && !event.alt_key() => Some(Motion::Up),
            "ArrowDown" if !modified && !event.alt_key() => Some(Motion::Down),
            "Home" => Some(if modified {
                Motion::DocumentStart
            } else {
                Motion::LineStart
            }),
            "End" => Some(if modified {
                Motion::DocumentEnd
            } else {
                Motion::LineEnd
            }),
            _ => None,
        }
    } else {
        None
    };
    if command.is_none() && motion.is_none() {
        return false;
    }
    let Some(project) = textarea
        .get_attribute("data-editor-project")
        .and_then(|value| value.parse().ok())
    else {
        return false;
    };
    let Some(path) = textarea.get_attribute("data-editor-path") else {
        return false;
    };
    if command.is_some() && !motion_adapter.flush(textarea) {
        return true;
    }
    let selection = projected_selection(actions, textarea);
    if let Err(failure) = actions.record_native_selection(selection) {
        error.set(Some(failure.to_string()));
        return true;
    }
    if let Some(motion) = motion
        && actions.queued_motion_ticket().is_some()
    {
        motion_adapter.queue(textarea, motion, event.shift_key());
        return true;
    }
    let source = actions.source();
    let result = if let Some(command) = command {
        actions.selection_command(project, &path, &source, command)
    } else if actions.preferences().word_wrap && matches!(motion, Some(Motion::Up | Motion::Down)) {
        if let Some(layout) = super::editor_geometry::visual_layout(actions, textarea) {
            actions.move_selections_with_layout(
                project,
                &path,
                &source,
                motion.unwrap(),
                event.shift_key(),
                &layout,
            )
        } else {
            motion_adapter.queue(textarea, motion.unwrap(), event.shift_key());
            return true;
        }
    } else {
        actions.move_selections(project, &path, &source, motion.unwrap(), event.shift_key())
    };
    match result {
        Ok(Some(selections)) => {
            error.set(None);
            if let Some(selection) = selections.first() {
                render_editor_selection(actions, textarea, *selection, false);
                reveal_editor_caret(actions, textarea, motion_adapter.paint);
            }
        }
        Err(failure) => error.set(Some(failure.to_string())),
        Ok(None) => {}
    }
    true
}

fn focus_editor_after_menu(
    workspace: WorkspaceState,
    actions: EditorActions,
    textarea: &web_sys::HtmlTextAreaElement,
) {
    let textarea = textarea.clone();
    let epoch = workspace.pending_epoch.get_untracked();
    let source = actions.source();
    let selections = actions.selections(&source);
    leptos::leptos_dom::helpers::queue_microtask(move || {
        if workspace.pending_epoch.try_get_untracked() != Some(epoch)
            || !current_editor_target(actions, &textarea)
            || actions.source() != source
            || actions.selections(&source) != selections
            || !super::modal::allows_focus(textarea.as_ref())
        {
            return;
        }
        let focus = web_sys::FocusOptions::new();
        focus.set_prevent_scroll(true);
        let _ = textarea.focus_with_options(&focus);
    });
}

fn editor_selection(textarea: &web_sys::HtmlTextAreaElement) -> openwebide_core::editor::Selection {
    use openwebide_core::editor::{Selection, utf16_to_byte};
    let text = textarea.value();
    let start = utf16_to_byte(
        &text,
        textarea.selection_start().ok().flatten().unwrap_or(0) as usize,
    );
    let end = utf16_to_byte(
        &text,
        textarea.selection_end().ok().flatten().unwrap_or(0) as usize,
    );
    if textarea.selection_direction().ok().flatten().as_deref() == Some("backward") {
        Selection {
            anchor: end,
            head: start,
        }
    } else {
        Selection {
            anchor: start,
            head: end,
        }
    }
}

fn native_selection_units(
    textarea: &web_sys::HtmlTextAreaElement,
) -> openwebide_core::editor::Selection {
    let start = textarea.selection_start().ok().flatten().unwrap_or(0) as usize;
    let end = textarea.selection_end().ok().flatten().unwrap_or(0) as usize;
    if textarea.selection_direction().ok().flatten().as_deref() == Some("backward") {
        openwebide_core::editor::Selection {
            anchor: end,
            head: start,
        }
    } else {
        openwebide_core::editor::Selection {
            anchor: start,
            head: end,
        }
    }
}

fn projected_selection(
    actions: EditorActions,
    textarea: &web_sys::HtmlTextAreaElement,
) -> openwebide_core::editor::Selection {
    actions.input_selection(native_selection_units(textarea), || textarea.value())
}

fn prepare_editor_edit(actions: EditorActions, textarea: &web_sys::HtmlTextAreaElement) {
    let selection = projected_selection(actions, textarea);
    if actions.prepare_edit(selection).is_ok() {
        render_editor_selection(actions, textarea, selection, true);
    }
}

fn refresh_editor_folds(actions: EditorActions) {
    let deadline = js_sys::Date::now() + 12.0;
    actions.refresh_fold_ranges(|| js_sys::Date::now() <= deadline);
}

fn stamp_editor_input(actions: EditorActions, input: &web_sys::HtmlTextAreaElement) {
    let _ = input.set_attribute(
        "data-editor-native-preparing",
        if actions.native_geometry_pending() {
            "true"
        } else {
            "false"
        },
    );
    let _ = input.set_attribute(
        "data-editor-native-value-scope",
        &actions.projection_revision().to_string(),
    );
    if let Some(generation) = actions.native_input_generation() {
        let _ = input.set_attribute("data-editor-native-bound", "true");
        let _ = input.set_attribute("data-editor-native-generation", &generation.to_string());
    } else {
        let _ = input.remove_attribute("data-editor-native-bound");
        let _ = input.remove_attribute("data-editor-native-generation");
    }
}

// Avoid replacing an unchanged textarea value/selection while its native IME
// owns the composition range. Commands explicitly restore their source caret.
pub(super) fn render_editor_selection(
    actions: EditorActions,
    textarea: &web_sys::HtmlTextAreaElement,
    selection: openwebide_core::editor::Selection,
    native: bool,
) {
    if let Some(projection) = actions.input_projection()
        && let Ok(visible) = projection.input_selection(selection)
    {
        let changed = textarea.value() != projection.textarea_text();
        let scroll = (
            crate::viewport::editor_scroll(textarea).scroll_top(),
            crate::viewport::editor_scroll(textarea).scroll_left(),
        );
        if changed {
            textarea.set_value(projection.text());
        }
        if changed || !native {
            restore_editor_selection(textarea, &projection, visible);
        }
        stamp_editor_input(actions, textarea);
        crate::viewport::set_editor_scroll_top(textarea, scroll.0);
        crate::viewport::set_editor_scroll_left(textarea, scroll.1);
    }
}

/// Reveal the source caret using paint geometry and the shared scroll viewport.
pub(super) fn reveal_editor_caret(
    actions: EditorActions,
    textarea: &web_sys::HtmlTextAreaElement,
    paint: RwSignal<Option<EditorPaint>>,
) {
    reveal_editor_caret_with_retry(actions, textarea, paint, true);
}

fn reveal_editor_caret_with_retry(
    actions: EditorActions,
    textarea: &web_sys::HtmlTextAreaElement,
    paint: RwSignal<Option<EditorPaint>>,
    retry: bool,
) {
    if !current_editor_target(actions, textarea) {
        return;
    }
    let source = actions.source();
    let Some(selection) = actions.selection(&source) else {
        return;
    };
    let Some(rect) = paint
        .get_untracked()
        .and_then(|paint| paint.caret.run(selection.head))
    else {
        // A source motion can precede the reactive paint/cache refresh. Retry
        // after that frame instead of silently losing the explicit reveal.
        // New source, selection, account or scroll intent cancels the request.
        if retry {
            let scope = untrack(|| actions.presentation_scope());
            let scroll = crate::viewport::editor_scroll(textarea);
            let position = (scroll.scroll_top(), scroll.scroll_left());
            let textarea = textarea.clone();
            leptos::leptos_dom::helpers::request_animation_frame(move || {
                leptos::leptos_dom::helpers::request_animation_frame(move || {
                    if paint.is_disposed() || !current_editor_target(actions, &textarea) {
                        return;
                    }
                    let scroll = crate::viewport::editor_scroll(&textarea);
                    if untrack(|| actions.presentation_scope()) == scope
                        && actions.source() == source
                        && actions.selection(&source) == Some(selection)
                        && (scroll.scroll_top(), scroll.scroll_left()) == position
                    {
                        reveal_editor_caret_with_retry(actions, &textarea, paint, false);
                    }
                });
            });
        }
        return;
    };
    let scroll = crate::viewport::editor_scroll(textarea);
    let viewport = scroll.get_bounding_client_rect();
    let bottom = viewport.top() + f64::from(scroll.client_height()) - 12.0;
    let delta = if rect.top() < viewport.top() + 12.0 {
        rect.top() - viewport.top() - 12.0
    } else if rect.bottom() > bottom {
        rect.bottom() - bottom
    } else {
        0.0
    };
    if delta != 0.0 {
        crate::viewport::set_editor_scroll_top(textarea, (scroll.scroll_top() + delta).max(0.0));
    }
    if rect.left() < viewport.left()
        || rect.right() > viewport.left() + f64::from(scroll.client_width()) - 16.0
    {
        let delta = if rect.left() < viewport.left() {
            rect.left() - viewport.left()
        } else {
            rect.right() - viewport.left() - f64::from(scroll.client_width()) + 16.0
        };
        crate::viewport::set_editor_scroll_left(textarea, (scroll.scroll_left() + delta).max(0.0));
    }
}

pub(super) fn editor_row_height(textarea: &web_sys::HtmlTextAreaElement) -> f64 {
    if let Some((height, _)) = super::editor_rows::uniform_row_dimensions(textarea) {
        return height;
    }
    window()
        .get_computed_style(textarea)
        .ok()
        .flatten()
        .and_then(|style| style.get_property_value("line-height").ok())
        .and_then(|value| value.trim_end_matches("px").parse::<f64>().ok())
        .unwrap_or(19.5)
}

fn navigate_editor(
    actions: EditorActions,
    textarea: &web_sys::HtmlTextAreaElement,
    offset: usize,
    paint: RwSignal<Option<EditorPaint>>,
) {
    if let (Some(projection), Some(measured)) = (actions.projection(), actions.measured_rows())
        && let Ok(visible) = projection.visible_offset(offset)
    {
        let index = projection
            .lines()
            .partition_point(|line| line.visible_start <= visible)
            .saturating_sub(1);
        if let Some(top) = measured.rows.top(index) {
            let height = measured
                .rows
                .top(index + 1)
                .unwrap_or(measured.rows.height())
                - top;
            if top < crate::viewport::editor_scroll(textarea).scroll_top()
                || top + height
                    > crate::viewport::editor_scroll(textarea).scroll_top()
                        + f64::from(crate::viewport::editor_scroll(textarea).client_height())
            {
                crate::viewport::set_editor_scroll_top(textarea, top);
            }
        }
    }

    navigate_editor_with_retry(actions, textarea, offset, true, paint);
}

fn navigate_editor_with_retry(
    actions: EditorActions,
    textarea: &web_sys::HtmlTextAreaElement,
    offset: usize,
    retry: bool,
    paint: RwSignal<Option<EditorPaint>>,
) {
    let Ok(selection) = actions.navigate(offset) else {
        return;
    };
    render_editor_selection(actions, textarea, selection, false);
    let _ = textarea.focus();
    let source_at_navigation = actions.source();
    let scope = (
        textarea.get_attribute("data-editor-project"),
        textarea.get_attribute("data-editor-path"),
    );
    let textarea = textarea.clone();
    // Fold reveal updates syntax paint on the next frame. Measure the target
    // afterward, with scope and caret guards so late work cannot steal a view.
    leptos::leptos_dom::helpers::request_animation_frame(move || {
        leptos::leptos_dom::helpers::request_animation_frame(move || {
            if !current_editor_target(actions, &textarea)
                || (
                    textarea.get_attribute("data-editor-project"),
                    textarea.get_attribute("data-editor-path"),
                ) != scope
                || actions.source() != source_at_navigation
            {
                return;
            }
            let Some(projection) = actions.projection() else {
                return;
            };
            if projection.visible_selection(selection).is_err() {
                return;
            }
            let source = actions.source();
            if actions.selection(&source) != Some(selection) {
                return;
            }
            let (line, _) = openwebide_core::editor::line_column(&source, offset);
            let row = projection
                .lines()
                .iter()
                .position(|row| row.source_line == line - 1)
                .unwrap_or(0);
            let height = editor_row_height(&textarea);
            if let Some(parent) = textarea.parent_element()
                && let Ok(Some(target)) =
                    parent.query_selector(&format!(".editor-source-line[data-line='{line}']"))
            {
                let start = source[..offset]
                    .rfind('\n')
                    .map_or(0, |newline| newline + 1);
                let column =
                    u32::try_from(source[start..offset].encode_utf16().count()).unwrap_or(u32::MAX);
                let gutter = window()
                    .get_computed_style(&textarea)
                    .ok()
                    .flatten()
                    .and_then(|style| style.get_property_value("padding-left").ok())
                    .and_then(|value| value.trim_end_matches("px").parse::<f64>().ok())
                    .unwrap_or(40.0);
                {
                    let rect = caret_rect(&target, column)
                        .or_else(|| paint.get_untracked()?.caret.run(offset))
                        .unwrap_or_else(|| target.get_bounding_client_rect());
                    crate::viewport::set_editor_scroll_top(
                        &textarea,
                        (crate::viewport::editor_scroll(&textarea).scroll_top() + rect.top()
                            - crate::viewport::editor_scroll(&textarea)
                                .get_bounding_client_rect()
                                .top()
                            - f64::from(crate::viewport::editor_scroll(&textarea).client_height())
                                / 2.0)
                            .max(0.0),
                    );
                    reveal_caret_column(&rect, &textarea, gutter);
                }
                reveal_match_column(&target.unchecked_into(), &textarea, column, gutter);
                if let Ok(Some(overlay)) = parent.query_selector(".editor-highlight") {
                    sync_highlight_scroll(&textarea, &overlay.unchecked_into());
                }
            } else {
                crate::viewport::set_editor_scroll_top(
                    &textarea,
                    (f64::from(u32::try_from(row).unwrap_or(u32::MAX)) * height
                        - f64::from(crate::viewport::editor_scroll(&textarea).client_height())
                            / 2.0)
                        .max(0.0),
                );
                if retry {
                    let textarea = textarea.clone();
                    leptos::leptos_dom::helpers::request_animation_frame(move || {
                        if current_editor_target(actions, &textarea)
                            && actions.source() == source_at_navigation
                            && actions.selection(&source_at_navigation) == Some(selection)
                        {
                            navigate_editor_with_retry(actions, &textarea, offset, false, paint);
                        }
                    });
                }
            }
        });
    });
}

fn apply_fold_command(
    actions: EditorActions,
    textarea: &web_sys::HtmlTextAreaElement,
    command: openwebide_core::editor::FoldCommand,
) {
    let scroll = (
        crate::viewport::editor_scroll(textarea).scroll_top(),
        crate::viewport::editor_scroll(textarea).scroll_left(),
    );
    let _ = actions.record_native_selection(projected_selection(actions, textarea));
    if let Some((projection, selection)) = actions.fold_command(command)
        && let Ok(visible) = projection.visible_selection(selection)
    {
        let _ = (projection, visible);
        render_editor_selection(actions, textarea, selection, false);
        let focus = web_sys::FocusOptions::new();
        focus.set_prevent_scroll(true);
        let _ = textarea.focus_with_options(&focus);
        crate::viewport::set_editor_scroll_top(textarea, scroll.0);
        crate::viewport::set_editor_scroll_left(textarea, scroll.1);
        if let Some(parent) = textarea.parent_element()
            && let Ok(Some(overlay)) = parent.query_selector(".editor-highlight")
        {
            sync_highlight_scroll(textarea, &overlay.unchecked_into());
        }
    }
}

fn restore_editor_selection(
    textarea: &web_sys::HtmlTextAreaElement,
    projection: &openwebide_core::editor::FoldProjection,
    selection: openwebide_core::editor::Selection,
) {
    let range = selection.range();
    if let (Ok(start), Ok(end)) = (
        projection.byte_to_textarea(range.start),
        projection.byte_to_textarea(range.end),
    ) && let (Ok(start), Ok(end)) = (u32::try_from(start), u32::try_from(end))
    {
        let _ = textarea.set_selection_range_with_direction(
            start,
            end,
            if selection.anchor > selection.head {
                "backward"
            } else {
                "forward"
            },
        );
    }
}

/// How the open file is displayed in the editor pane.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Editable syntax-highlighted code editor.
    Code,
    /// Rendered preview (Markdown HTML, Image asset, or Binary placeholder).
    Preview,
    /// Agent edit inline diff.
    InlineDiff,
    /// Agent edit side-by-side split diff.
    SideBySide,
}

pub(super) fn raw_text_position(
    node: &web_sys::Node,
    offset: &mut u32,
) -> Option<(web_sys::Node, u32)> {
    fn walk(
        node: &web_sys::Node,
        offset: &mut u32,
        end: &mut Option<(web_sys::Node, u32)>,
    ) -> Option<(web_sys::Node, u32)> {
        if node.node_type() == web_sys::Node::TEXT_NODE {
            let text = node.node_value().unwrap_or_default();
            let length = u32::try_from(text.encode_utf16().count()).ok()?;
            // A standalone line-ending node has no glyph caret rectangle in
            // Chromium. The preceding token's end is the same source offset
            // and retains its measured inline position (including bidi affinity).
            if *offset == 0 && text.starts_with('\n') && end.is_some() {
                return end.take();
            }
            if *offset < length {
                return Some((node.clone(), *offset));
            }
            if *offset == length {
                *end = Some((node.clone(), length));
            }
            *offset -= length;
        } else {
            let children = node.child_nodes();
            for index in 0..children.length() {
                if let Some(child) = children.item(index)
                    && let Some(position) = walk(&child, offset, end)
                {
                    return Some(position);
                }
            }
        }
        None
    }
    let mut end = None;
    walk(node, offset, &mut end).or(end)
}

pub(super) fn text_position(
    node: &web_sys::Node,
    offset: &mut u32,
) -> Option<(web_sys::Node, u32)> {
    super::editor_paint::position(node, *offset)
}

pub(super) fn caret_rect(text: &web_sys::Element, mut column: u32) -> Option<web_sys::DomRect> {
    let (node, at) = text_position(text.as_ref(), &mut column)?;
    let range = document().create_range().ok()?;
    range.set_start(&node, at).ok()?;
    range.set_end(&node, at).ok()?;
    Some(range.get_bounding_client_rect())
}

fn glyph_rect(range: &web_sys::Range) -> web_sys::DomRect {
    if let Some(rects) = range.get_client_rects() {
        for index in 0..rects.length() {
            if let Some(rect) = rects.item(index)
                && rect.width() > 0.0
            {
                return rect;
            }
        }
    }
    range.get_bounding_client_rect()
}

/// Reveal the actual match column, including tabs, Unicode and highlighted tokens.
fn reveal_match_column(
    text: &web_sys::Element,
    body: &web_sys::HtmlElement,
    mut offset: u32,
    gutter: f64,
) {
    let Some((node, offset)) = text_position(text.as_ref(), &mut offset) else {
        return;
    };
    let Ok(range) = document().create_range() else {
        return;
    };
    if range.set_start(&node, offset).is_err() || range.set_end(&node, offset).is_err() {
        return;
    }
    let caret = range.get_bounding_client_rect();
    let viewport = body.get_bounding_client_rect();
    if body.class_list().contains("editor-textarea")
        && (caret.top() < viewport.top() || caret.bottom() > viewport.bottom())
        && let Some(input) = body.dyn_ref::<web_sys::HtmlTextAreaElement>()
    {
        crate::viewport::set_editor_scroll_top(
            input,
            (crate::viewport::editor_scroll(input).scroll_top() + caret.top()
                - viewport.top()
                - 12.0)
                .max(0.0),
        );
    }
    reveal_caret_column(&caret, body, gutter);
}

fn reveal_caret_column(caret: &web_sys::DomRect, body: &web_sys::HtmlElement, gutter: f64) {
    let viewport = body.get_bounding_client_rect();
    let left = viewport.left() + gutter;
    if caret.left() < left || caret.left() > viewport.right() - 16.0 {
        if let Some(input) = body.dyn_ref::<web_sys::HtmlTextAreaElement>() {
            crate::viewport::set_editor_scroll_left(
                input,
                (crate::viewport::editor_scroll(input).scroll_left() + caret.left() - left)
                    .max(0.0),
            );
        } else {
            body.set_scroll_left((body.scroll_left() + caret.left() - left).max(0.0));
        }
    }
}

fn clear_find_marks(root: &web_sys::HtmlElement) {
    if let Ok(nodes) = root.query_selector_all(".editor-find-match") {
        for index in 0..nodes.length() {
            if let Some(node) = nodes
                .item(index)
                .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
            {
                let _ = node.class_list().remove_1("editor-find-match");
            }
        }
    }
}

/// Browser offsets are adapter data; matching policy belongs to the shared engine.
fn browser_matches(
    text: &str,
    matches: &[openwebide_core::editor::SearchMatch],
) -> Vec<(u32, u32, usize)> {
    let mut byte = 0;
    let mut utf16 = 0;
    matches
        .iter()
        .map(|matched| {
            utf16 += text[byte..matched.range.start].encode_utf16().count();
            let start = utf16;
            utf16 += text[matched.range.clone()].encode_utf16().count();
            byte = matched.range.end;
            (
                u32::try_from(start).unwrap_or(u32::MAX),
                u32::try_from(utf16).unwrap_or(u32::MAX),
                matched.line,
            )
        })
        .collect()
}

/// The CSS class for a token category.
fn token_class(kind: TokenKind) -> &'static str {
    match kind {
        TokenKind::Plain => "tok-plain",
        TokenKind::Keyword => "tok-keyword",
        TokenKind::Type => "tok-type",
        TokenKind::String => "tok-string",
        TokenKind::Char => "tok-char",
        TokenKind::Comment => "tok-comment",
        TokenKind::Number => "tok-number",
        TokenKind::Function => "tok-function",
        TokenKind::Operator => "tok-operator",
        TokenKind::Punct => "tok-punct",
        TokenKind::Attribute => "tok-attribute",
        TokenKind::Macro => "tok-macro",
        TokenKind::Lifetime => "tok-lifetime",
        TokenKind::Boolean => "tok-boolean",
    }
}

use crate::text::escape_html;

#[cfg(feature = "test-support")]
thread_local! {
    static VIEWPORT_HIGHLIGHT_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static HIGHLIGHT_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static HIGHLIGHT_SOURCE_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static HIGHLIGHT_SEGMENT_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// HTML generations including temporary probes, for browser performance regressions.
#[cfg(feature = "test-support")]
pub fn highlight_count() -> usize {
    HIGHLIGHT_COUNT.get()
}

/// Visible overlay generations, excluding temporary source geometry probes.
#[cfg(feature = "test-support")]
pub fn viewport_highlight_count() -> usize {
    VIEWPORT_HIGHLIGHT_COUNT.get()
}

/// Largest source row passed to HTML escaping since the last browser audit.
#[cfg(feature = "test-support")]
pub fn take_highlight_source_bytes() -> usize {
    HIGHLIGHT_SOURCE_BYTES.replace(0)
}

/// Largest token prefix actually visited by paint-run segmentation in this audit.
#[cfg(feature = "test-support")]
pub fn take_highlight_segment_bytes() -> usize {
    HIGHLIGHT_SEGMENT_BYTES.replace(0)
}

#[cfg(feature = "test-support")]
pub fn take_paragraph_suffix_probes() -> usize {
    super::editor_rows::take_paragraph_suffix_probes()
}

#[cfg(feature = "test-support")]
pub fn take_paragraph_prefix_reconciliations() -> usize {
    super::editor_rows::take_paragraph_prefix_reconciliations()
}

#[cfg(feature = "test-support")]
pub fn wrapped_coverage_matches_complete(
    input: &web_sys::HtmlTextAreaElement,
    snapshot: &crate::state::workspace::EditorParagraphCoverage,
) -> bool {
    let paint = &snapshot.paint;
    let html = highlight_html(
        &paint.tokens,
        paint.prepared_source,
        &paint.guides,
        PaintRows {
            indices: &[0],
            projection: None,
            source: Some(&paint.projection),
            source_slices: &[],
        },
        paint.indentation,
        paint.whitespace,
        false,
    );
    super::editor_rows::check_wrapped_coverage_geometry(input, snapshot, &html).unwrap_or(false)
}

#[cfg(feature = "test-support")]
pub async fn bounded_wrapped_matches_complete(
    input: &web_sys::HtmlTextAreaElement,
    scope: &crate::state::workspace::EditorRowPaint,
    actions: EditorActions,
) -> bool {
    let Some((scope, _)) = actions.prepare_row_measurements(
        scope.metrics.clone(),
        scope.projection.clone(),
        (scope.prepared_source, scope.tokens.clone()),
        scope.guides.clone(),
        scope.indentation,
        scope.whitespace,
    ) else {
        return false;
    };
    super::editor_rows::check_wrapped_paragraph_geometry(
        input,
        &scope,
        0,
        actions,
        |rows, suffix, slices| {
            highlight_html(
                &scope.tokens,
                scope.prepared_source,
                &scope.guides,
                PaintRows {
                    indices: rows,
                    projection: None,
                    source: Some(&scope.projection),
                    source_slices: slices,
                },
                scope.indentation,
                scope.whitespace,
                suffix,
            )
        },
    )
    .await
    .unwrap_or(false)
}

/// Differential browser oracle: compare bounded preparation with the complete
/// production renderer, including every retained glyph anchor. Test-only full
/// layout must never run in ordinary preparation.
#[cfg(feature = "test-support")]
pub async fn bounded_paragraph_matches_complete(
    input: &web_sys::HtmlTextAreaElement,
    scope: &crate::state::workspace::EditorRowPaint,
    row: usize,
) -> bool {
    paragraph_matches_complete(input, scope, row, None).await
}

#[cfg(feature = "test-support")]
pub async fn retained_paragraph_matches_complete(
    input: &web_sys::HtmlTextAreaElement,
    scope: &crate::state::workspace::EditorRowPaint,
    row: usize,
    actions: EditorActions,
) -> bool {
    paragraph_matches_complete(input, scope, row, Some(actions)).await
}

#[cfg(feature = "test-support")]
async fn paragraph_matches_complete(
    input: &web_sys::HtmlTextAreaElement,
    scope: &crate::state::workspace::EditorRowPaint,
    row: usize,
    actions: Option<EditorActions>,
) -> bool {
    super::editor_rows::check_paragraph_geometry(
        input,
        scope,
        row,
        actions,
        |rows, suffix, slices| {
            highlight_html(
                &scope.tokens,
                scope.prepared_source,
                &scope.guides,
                PaintRows {
                    indices: rows,
                    projection: None,
                    source: Some(&scope.projection),
                    source_slices: slices,
                },
                scope.indentation,
                scope.whitespace,
                suffix,
            )
        },
    )
    .await
    .unwrap_or(false)
}

/// Whitespace markers retain their original text node and therefore source offsets.
fn paint_text_range(
    text: &str,
    range: std::ops::Range<usize>,
    show_whitespace: bool,
    starts_paint_run: bool,
    paint_runs: Option<(&[usize], usize)>,
) -> String {
    let Some(selected) = text.get(range.clone()) else {
        return String::new();
    };
    if selected.is_empty() {
        return String::new();
    }
    if !show_whitespace {
        if text.len() <= 512 {
            return escape_html(selected);
        }
        if let Some((ends, offset)) = paint_runs
            && let Some(runs) = openwebide_core::editor::retained_paint_ranges(
                ends,
                offset..offset + text.len(),
                offset + range.start..offset + range.end,
            )
            && let Some(parts) = runs
                .into_iter()
                .map(|run| text.get(run))
                .collect::<Option<Vec<_>>>()
        {
            return parts
                .into_iter()
                .map(|part| {
                    format!(
                        "<span class=\"editor-text-run\">{}</span>",
                        escape_html(part)
                    )
                })
                .collect();
        }
        // A proven original run boundary lets segmentation resume at the
        // selected source, preserving the complete paint's span topology.
        let start = if starts_paint_run { range.start } else { 0 };
        return openwebide_core::editor::visual_text_run_ranges(&text[start..])
            .inspect(|run| {
                #[cfg(feature = "test-support")]
                HIGHLIGHT_SEGMENT_BYTES.set(HIGHLIGHT_SEGMENT_BYTES.get().max(run.end));
                #[cfg(not(feature = "test-support"))]
                let _ = run;
            })
            .map(|run| run.start + start..run.end + start)
            .take_while(|run| run.start < range.end)
            .filter(|run| run.end > range.start)
            .map(|run| {
                format!(
                    "<span class=\"editor-text-run\">{}</span>",
                    escape_html(&text[run.start.max(range.start)..run.end.min(range.end)])
                )
            })
            .collect();
    }
    let mut html = String::new();
    for ch in selected.chars() {
        match ch {
            ' ' => html.push_str("<span class=\"editor-space\"> </span>"),
            '\t' => html.push_str("<span class=\"editor-tab\">\t</span>"),
            _ => html.push_str(&escape_html(&ch.to_string())),
        }
    }
    html
}

/// Render the highlighted source as an HTML string for the overlay.
struct PaintRows<'a> {
    indices: &'a [usize],
    projection: Option<&'a openwebide_core::editor::FoldProjection>,
    source: Option<&'a openwebide_core::editor::FoldProjection>,
    source_slices: &'a [crate::state_actions::editor::EditorRowSourceSlice],
}
impl<'a> PaintRows<'a> {
    fn measured(indices: &'a [usize], source: &'a openwebide_core::editor::FoldProjection) -> Self {
        Self {
            indices,
            projection: None,
            source: Some(source),
            source_slices: &[],
        }
    }
    fn fragment(&self, source_line: usize) -> Option<(usize, usize)> {
        let projection = self.projection?;
        let index = projection
            .lines()
            .binary_search_by_key(&source_line, |line| line.source_line)
            .ok()?;
        let line = &projection.lines()[index];
        let end = projection
            .byte_to_textarea(line.visible_start + line.source.len())
            .ok()?;
        Some((line.textarea_start, end.checked_sub(line.textarea_start)?))
    }
}

fn highlight_html(
    lines: &[openwebide_core::highlight::TokenRow],
    prepared_source: bool,
    guides: &[usize],
    rows: PaintRows<'_>,
    indentation: openwebide_core::editor::Indentation,
    show_whitespace: bool,
    trailing_line_ending: bool,
) -> String {
    #[cfg(feature = "test-support")]
    {
        HIGHLIGHT_COUNT.set(HIGHLIGHT_COUNT.get() + 1);
        if rows.projection.is_some() {
            VIEWPORT_HIGHLIGHT_COUNT.set(VIEWPORT_HIGHLIGHT_COUNT.get() + 1);
        }
    }
    let mut html = String::new();
    for &idx in rows.indices {
        let plain = if !prepared_source && lines.get(idx).is_none() {
            rows.source.and_then(|source| {
                let row = source
                    .lines()
                    .binary_search_by_key(&idx, |line| line.source_line)
                    .ok()?;
                source.line_body(row)
            })
        } else {
            None
        };
        let line = match lines.get(idx) {
            Some(line) => line.as_ref(),
            None if plain.is_some() => &[],
            None => continue,
        };
        html.push_str(&format!(
            "<span class=\"editor-source-line\" data-line=\"{}\" style=\"--editor-indent-columns:{};--editor-indent-step:{}\">",
            idx + 1, guides.get(idx).copied().unwrap_or(0), indentation.width()
        ));
        let source_slice = rows
            .source_slices
            .iter()
            .find(|slice| slice.source_line == idx);
        let fragment = rows.fragment(idx);
        if let Some(slice) = source_slice {
            let end = html.len() - 1;
            html.insert_str(
                end,
                &format!(" data-source-start=\"{}\"", slice.native_start),
            );
        }
        if let Some((start, length)) = fragment {
            // Append metadata to the logical wrapper before inserting its text.
            let end = html.len() - 1;
            html.insert_str(
                end,
                &format!(" data-paint-length=\"{length}\" data-textarea-start=\"{start}\""),
            );
            html.push_str(&format!("<span class=\"editor-source-fragment\" data-paint-start=\"0\" data-paint-end=\"{length}\">"));
        }
        let mut source_offset = 0;
        #[cfg(feature = "test-support")]
        let mut painted_bytes = 0;
        if let Some(text) = plain {
            let range = if let Some(slice) = source_slice {
                slice.bytes.start.min(text.len())..slice.bytes.end.min(text.len())
            } else {
                0..text.len()
            };
            html.push_str(&paint_text_range(
                text,
                range.clone(),
                show_whitespace,
                source_slice.is_some_and(|slice| slice.starts_paint_run),
                source_slice
                    .and_then(|slice| slice.paint_runs.as_deref())
                    .map(|ends| (ends, 0)),
            ));
            #[cfg(feature = "test-support")]
            {
                painted_bytes += range.len();
            }
        }
        for (position, tok) in line.iter().enumerate() {
            let text = if prepared_source && idx + 1 < lines.len() && position + 1 == line.len() {
                tok.text.strip_suffix('\r').unwrap_or(&tok.text)
            } else {
                &tok.text
            };
            let token_start = source_offset;
            source_offset += text.len();
            let range = if let Some(slice) = source_slice {
                let start = slice.bytes.start.max(token_start);
                let finish = slice.bytes.end.min(source_offset);
                if start >= finish {
                    continue;
                }
                start - token_start..finish - token_start
            } else {
                0..text.len()
            };
            #[cfg(feature = "test-support")]
            {
                painted_bytes += range.len();
            }
            match tok.kind {
                TokenKind::Plain => html.push_str(&paint_text_range(
                    text,
                    range,
                    show_whitespace,
                    source_slice.is_some_and(|slice| slice.starts_paint_run),
                    source_slice
                        .and_then(|slice| slice.paint_runs.as_deref())
                        .map(|ends| (ends, token_start)),
                )),
                kind => {
                    html.push_str("<span class=\"");
                    html.push_str(token_class(kind));
                    html.push_str("\">");
                    html.push_str(&paint_text_range(
                        text,
                        range,
                        show_whitespace,
                        source_slice.is_some_and(|slice| slice.starts_paint_run),
                        source_slice
                            .and_then(|slice| slice.paint_runs.as_deref())
                            .map(|ends| (ends, token_start)),
                    ));
                    html.push_str("</span>");
                }
            }
        }
        #[cfg(feature = "test-support")]
        HIGHLIGHT_SOURCE_BYTES.set(HIGHLIGHT_SOURCE_BYTES.get().max(painted_bytes));
        if source_slice.is_none_or(|slice| slice.reaches_end)
            && (idx != *rows.indices.last().unwrap_or(&idx) || trailing_line_ending)
        {
            if show_whitespace {
                html.push_str("<span class=\"editor-line-ending\"></span>");
            }
            html.push('\n');
        }
        if fragment.is_some() {
            html.push_str("</span>");
        }
        html.push_str("</span>");
    }
    html
}

/// Translate paint from the document viewport, independently of native input.
pub(super) fn sync_highlight_scroll(
    textarea: &web_sys::HtmlTextAreaElement,
    overlay: &web_sys::HtmlElement,
) {
    crate::viewport::refresh_editor_scroll(textarea);
    // Scrollbars can settle between the observer and source paint.
    let _ = overlay.style().set_property(
        "--editor-text-width",
        &format!(
            "{}px",
            crate::viewport::editor_scroll(textarea).client_width()
        ),
    );
    let _ = overlay.style().set_property(
        "--editor-viewport-height",
        &format!(
            "{}px",
            crate::viewport::editor_scroll(textarea).client_height()
        ),
    );
    let _ = overlay.style().set_property(
        "--editor-scroll-x",
        &format!(
            "{}px",
            -crate::viewport::editor_scroll(textarea).scroll_left()
        ),
    );
    let _ = overlay.style().set_property(
        "--editor-scroll-y",
        &format!(
            "{}px",
            -crate::viewport::editor_scroll(textarea).scroll_top()
        ),
    );
    if let Some(parent) = textarea
        .parent_element()
        .and_then(|parent| parent.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let _ = parent.style().set_property(
            "--editor-scroll-x",
            &format!(
                "{}px",
                -crate::viewport::editor_scroll(textarea).scroll_left()
            ),
        );
        let _ = parent.style().set_property(
            "--editor-scroll-y",
            &format!(
                "{}px",
                -crate::viewport::editor_scroll(textarea).scroll_top()
            ),
        );
    }
    overlay.set_scroll_top(0.0);
    overlay.set_scroll_left(0.0);
}

#[component]
fn LargeTextViewer(
    content: ReadSignal<crate::state::workspace::EditorText>,
    limit: openwebide_core::editor::EditorLimit,
    #[prop(default = None)] old: Option<String>,
) -> impl IntoView {
    let requested = RwSignal::new(0_usize);
    let old = StoredValue::new(old);
    let show_old = RwSignal::new(false);
    let page = Memo::new(move |_| {
        if show_old.get() {
            old.with_value(|source| {
                openwebide_core::editor::TextPage::new(
                    source.as_deref().unwrap_or_default(),
                    requested.get(),
                )
            })
        } else {
            content.with(|source| openwebide_core::editor::TextPage::new(source, requested.get()))
        }
    });
    view! {
        <div class="editor-preview editor-large-file">
            <p class="form-hint" role="status">{format!("{limit}. Showing read-only text pages; the complete file remains unchanged.")}</p>
            <PanelSearchRow>
                {old.with_value(Option::is_some).then(move || view! {
                    <SegmentedControl options=vec![SegmentOption::new("Before", true), SegmentOption::new("After", false)] value=show_old.read_only().into() on_change=Callback::new(move |before| { requested.set(0); show_old.set(before); }) />
                })}
                <Button class="editor-page-previous" size=ButtonSize::Sm disabled=Signal::derive(move || page.get().index == 0) on_click=Callback::new(move |_| requested.set(page.get_untracked().index.saturating_sub(1)))>"Previous"</Button>
                <span class="form-hint" aria-live="polite">{move || page.with(|page| format!("Page {} of {} · bytes {}–{}", page.index + 1, page.pages, page.range.start, page.range.end))}</span>
                <Button class="editor-page-next" size=ButtonSize::Sm disabled=Signal::derive(move || page.with(|page| page.index + 1 == page.pages)) on_click=Callback::new(move |_| requested.set(page.get_untracked().index + 1))>"Next"</Button>
            </PanelSearchRow>
            <pre class="editor-large-file-page" data-page=move || page.get().index.to_string() tabindex="0" aria-label="Read-only file page"><code>{move || {
                let range = page.get().range;
                if show_old.get() { old.with_value(|source| source.as_deref().unwrap_or_default()[range].to_string()) }
                else { content.with(|source| source[range].to_string()) }
            }}</code></pre>
        </div>
    }
}

#[component]
fn HighlightOverlay(
    actions: EditorActions,
    paint_request: RwSignal<Option<EditorPaint>>,
    paint_epoch: RwSignal<u64>,
    content: ReadSignal<crate::state::workspace::EditorText>,
    open_file: ReadSignal<Option<String>>,
    node_ref: NodeRef<leptos::html::Div>,
    textarea_ref: NodeRef<leptos::html::Textarea>,
    ready: RwSignal<bool>,
    presentation: RwSignal<bool>,
    error: RwSignal<Option<String>>,
    visible: Memo<Vec<usize>>,
    viewport: Memo<openwebide_core::editor::EditorViewport>,
    textarea_start: Memo<usize>,
    indentation: Signal<openwebide_core::editor::Indentation>,
    show_whitespace: Signal<bool>,
    layout_revision: RwSignal<u64>,
    native_install_revision: RwSignal<u64>,
) -> impl IntoView {
    use wasm_bindgen::closure::Closure;

    // A remounted view starts cold; only its own published frame may be retained.
    presentation.set(false);
    ready.set(false);
    let presentation_scope = Memo::new(move |_| actions.presentation_scope());
    let presented_scope = StoredValue::new(None);
    let painted_whitespace = StoredValue::new(false);
    let painted_complete_rows = StoredValue::new(false);
    let painted_syntax =
        StoredValue::new(None::<(bool, std::sync::Arc<openwebide_core::highlight::TokenRows>)>);
    let layout_callback = StoredValue::new_local(Closure::<dyn FnMut(bool, bool)>::new(
        move |font_changed, font_loaded| {
            if layout_revision.is_disposed() {
                return;
            }
            if let (Some(input), Some(overlay)) =
                (textarea_ref.get_untracked(), node_ref.get_untracked())
            {
                super::editor_rows::update_measurements(
                    actions,
                    &input,
                    &overlay,
                    font_changed,
                    font_loaded,
                    painted_syntax.get_value(),
                    painted_whitespace.get_value(),
                );
            }
            layout_revision.update(|value| *value = value.wrapping_add(1));
        },
    ));
    let viewport_observer = StoredValue::new_local(None::<wasm_bindgen::JsValue>);
    Effect::new(move || {
        if let (Some(input), Some(overlay)) = (textarea_ref.get(), node_ref.get())
            && viewport_observer.get_value().is_none()
        {
            layout_callback.with_value(|callback| {
                viewport_observer.set_value(Some(crate::viewport::observe_editor_viewport(
                    &input,
                    &overlay,
                    callback.as_ref().unchecked_ref(),
                )));
            });
        }
    });
    on_cleanup(move || {
        viewport_observer.with_value(|stop| {
            if let Some(stop) = stop {
                let _ = stop
                    .unchecked_ref::<js_sys::Function>()
                    .call0(&wasm_bindgen::JsValue::NULL);
            }
        });
    });
    let prepared_paint = Memo::new(move |_| {
        content.track();
        actions.presentation_scope();
        actions.preparation_revision();
        actions.syntax_paint()
    });
    let tokens = Memo::new(move |_| EditorActions::render_paint(prepared_paint.get()));
    let guides = Memo::new(move |_| {
        content.track();
        actions.presentation_scope();
        actions.prepare_projection();
        actions.indent_guides(indentation.get())
    });
    let retain_style = Callback::new(move |()| {
        painted_syntax.with_value(|syntax| {
            syntax.as_ref().is_some_and(|syntax| {
                actions.retain_pending_paint(syntax.0, painted_complete_rows.get_value())
            })
        }) && presented_scope.get_value() == presentation_scope.get_untracked()
    });
    let fragment_cache =
        StoredValue::new(crate::state_actions::editor::EditorFragmentCache::default());
    let batch_key = StoredValue::new(None::<(u64, u64, String, usize, bool)>);
    let batch_ticket = StoredValue::new(None::<u64>);
    Effect::new(move || {
        layout_revision.track();
        actions.view_revision();
        actions.preferences();
        let syntax_revision = actions.preparation_revision();
        // Preparation readiness can advance without changing neutral geometry.
        prepared_paint.track();
        let prepared_tokens = tokens.get();
        if retain_style.run(()) || !actions.full_row_paint_ready() {
            batch_key.set_value(None);
            if let Some(ticket) = batch_ticket.get_value() {
                actions.end_row_preparation(ticket);
            }
            return;
        }
        let whitespace = show_whitespace.get();
        let tab = indentation.get();
        let Some(input) = textarea_ref.get() else {
            return;
        };
        if crate::viewport::editor_scroll(&input).client_width() <= 0
            || crate::viewport::editor_scroll(&input).client_height() <= 0
        {
            batch_key.set_value(None);
            if let Some(ticket) = batch_ticket.get_value() {
                actions.end_row_preparation(ticket);
            }
            return;
        }
        let Some(projection) = actions.projection() else {
            return;
        };
        if visible.get().len() == projection.lines().len()
            && !openwebide_core::editor::needs_measured_batches(
                projection.lines().len(),
                projection.text().len(),
            )
        {
            batch_key.set_value(None);
            return;
        }
        let Some(metrics) = super::editor_rows::metrics_identity(&input) else {
            return;
        };
        let revision = actions.view_revision();
        let key = (
            revision,
            syntax_revision,
            metrics.clone(),
            std::sync::Arc::as_ptr(&prepared_tokens.1) as usize,
            whitespace,
        );
        if let Some(measured) = actions.measured_rows() {
            if measured.metrics == metrics
                && measured.whitespace == whitespace
                && measured.syntax.as_ref().is_some_and(|syntax| {
                    syntax.0 == prepared_tokens.0
                        && std::sync::Arc::ptr_eq(&syntax.1, &prepared_tokens.1)
                })
            {
                batch_key.set_value(Some(key));
                return;
            }
            actions.invalidate_measured_rows();
            return;
        }
        if batch_key.get_value().as_ref() == Some(&key) {
            return;
        }
        batch_key.set_value(Some(key.clone()));
        let Some(ticket) = actions.begin_row_preparation(revision, projection.lines().len()) else {
            return;
        };
        batch_ticket.set_value(Some(ticket));
        let guides: std::sync::Arc<[usize]> = guides.get_untracked();
        fragment_cache.update_value(|cache| {
            actions.fragment_scope(
                cache,
                metrics.clone(),
                prepared_tokens.clone(),
                guides.clone(),
                tab,
                whitespace,
            );
        });
        wasm_bindgen_futures::spawn_local(async move {
            // Collapse a burst before allocating plans or rendering source probes.
            crate::util::yield_frame().await;
            if batch_key.try_get_value().as_ref().and_then(Option::as_ref) != Some(&key)
                || !actions.row_preparation_current(ticket)
            {
                actions.end_row_preparation(ticket);
                return;
            }
            let Some((paint, plan)) = actions.prepare_row_measurements(
                metrics.clone(),
                projection.clone(),
                prepared_tokens.clone(),
                guides.clone(),
                tab,
                whitespace,
            ) else {
                actions.end_row_preparation(ticket);
                return;
            };
            if !actions.retain_row_preparation(ticket, &paint) {
                actions.end_row_preparation(ticket);
                return;
            }
            let geometry_paint = paint.clone();
            let progress_paint = paint.clone();
            let result = super::editor_rows::measure_batches(
                actions,
                input,
                paint.clone(),
                plan,
                move || {
                    batch_key.try_get_value().as_ref().and_then(Option::as_ref) == Some(&key)
                        && actions.row_preparation_current(ticket)
                },
                move |completed, prefix| {
                    if let Some(prefix) = prefix {
                        actions.retain_measured_prefix(ticket, &progress_paint, prefix);
                    }
                    actions.report_row_preparation(ticket, completed);
                },
                move |row, geometry| {
                    if actions.row_preparation_current(ticket) && !fragment_cache.is_disposed() {
                        if let openwebide_core::editor::MeasuredRowGeometry::WrappedCoverage(
                            prefix,
                        ) = geometry
                        {
                            actions.retain_paragraph_coverage(ticket, &geometry_paint, row, prefix);
                            return;
                        }
                        fragment_cache.update_value(|cache| {
                            actions.retain_preparation_geometry(
                                cache,
                                &geometry_paint,
                                row,
                                geometry,
                            );
                        });
                    }
                },
                move |rows, suffix, source_slices| {
                    highlight_html(
                        &prepared_tokens.1,
                        prepared_tokens.0,
                        &guides,
                        PaintRows {
                            indices: rows,
                            projection: None,
                            source: Some(&projection),
                            source_slices,
                        },
                        tab,
                        whitespace,
                        suffix,
                    )
                },
            )
            .await;
            if batch_key.is_disposed() {
                return;
            }
            if let Some(message) = actions.finish_row_preparation(ticket, paint, result) {
                error.set(Some(message.into()));
            }
        });
    });
    on_cleanup(move || {
        if let Some(ticket) = batch_ticket.get_value() {
            actions.end_row_preparation(ticket);
        }
    });
    let fragment_windows = Memo::new(move |_| {
        layout_revision.track();
        let Some(input) = textarea_ref.get() else {
            return Vec::new();
        };
        let Some(projection) = actions.projection() else {
            return Vec::new();
        };
        visible
            .get()
            .iter()
            .filter_map(|source_line| {
                let index = projection
                    .lines()
                    .binary_search_by_key(source_line, |line| line.source_line)
                    .ok()?;
                let window = actions.paint_window(
                    index,
                    editor_row_height(&input),
                    (
                        crate::viewport::editor_scroll(&input).scroll_left(),
                        crate::viewport::editor_scroll(&input).scroll_top() - 12.0,
                    ),
                    (
                        f64::from(crate::viewport::editor_scroll(&input).client_width()),
                        f64::from(crate::viewport::editor_scroll(&input).client_height()),
                    ),
                )?;
                Some((index, window))
            })
            .collect::<Vec<_>>()
    });
    let geometry_pending = Memo::new(move |_| {
        !fragment_windows.with(Vec::is_empty)
            && actions.row_geometry_is_pending()
            && actions.measured_prefix().is_none()
            && actions.paragraph_coverage().is_none()
    });
    let partial_geometry = Memo::new(move |_| {
        actions.measured_rows().is_none()
            && (actions.paragraph_coverage().is_some()
                || actions.measured_prefix().is_some_and(|prefix| {
                    prefix.paint.word_wrap || !prefix.paint.projection.has_uniform_rows()
                }))
    });
    let rendered = RwSignal::new(String::new());
    let rendered_scope = RwSignal::new(0_u64);
    let request = StoredValue::new(None::<i32>);
    let generation = StoredValue::new(0_u64);
    let queued_generation = StoredValue::new(0_u64);
    let path = StoredValue::new(None::<String>);
    let paint = Callback::new(move |immediate: bool| {
        request.set_value(None);
        if generation.get_value() != queued_generation.get_value()
            || open_file.get_untracked() != path.get_value()
            || node_ref.get_untracked().is_none()
        {
            return;
        }
        if !immediate && retain_style.run(()) {
            return;
        }
        if !immediate && !actions.viewport_paint_ready(&visible.get_untracked()) {
            if actions.defer_viewport_paint(&visible.get_untracked()) {
                presentation.set(false);
            }
            return;
        }
        let render = |source_slices: &[crate::state_actions::editor::EditorRowSourceSlice]| {
            tokens.with_untracked(|(prepared, tokens)| {
                guides.with_untracked(|guides| {
                    highlight_html(
                        tokens,
                        *prepared,
                        guides,
                        PaintRows {
                            indices: &visible.get_untracked(),
                            projection: actions.projection().as_ref(),
                            source: actions.projection().as_ref(),
                            source_slices,
                        },
                        indentation.get_untracked(),
                        show_whitespace.get_untracked(),
                        actions.projection().is_some_and(|projection| {
                            viewport.get_untracked().rows.end < projection.lines().len()
                        }),
                    )
                })
            })
        };
        let input = textarea_ref.get_untracked();
        let windows = fragment_windows.get_untracked();
        // Native cold input remains visible. Explicit motion/caret probes can
        // still flush immediately; ordinary paint reuses the pending probe.
        if !immediate && !windows.is_empty() && geometry_pending.get_untracked() {
            return;
        }
        let mut cached = None;
        let mut cache_window = None;
        if let Some(input) = input.as_ref()
            && current_editor_target(actions, input)
            && let Some(metrics) = super::editor_rows::metrics_identity(input)
        {
            let key =
                (!windows.is_empty()).then(|| crate::state_actions::editor::EditorFragmentWindow {
                    rows: visible.get_untracked(),
                    windows: windows.clone(),
                    width: crate::viewport::editor_scroll(input).scroll_width(),
                    height: crate::viewport::editor_scroll(input).client_height(),
                    trailing: actions.projection().is_some_and(|projection| {
                        viewport.get_untracked().rows.end < projection.lines().len()
                    }),
                });
            fragment_cache.update_value(|cache| {
                if actions.fragment_scope(
                    cache,
                    metrics,
                    tokens.get_untracked(),
                    guides.get_untracked(),
                    indentation.get_untracked(),
                    show_whitespace.get_untracked(),
                ) && let Some(key) = key
                {
                    cached = actions.cached_fragment(cache, &key);
                    cache_window = Some(key);
                }
            });
        }
        let partial_paragraph =
            actions.measured_rows().is_none() && actions.paragraph_coverage().is_some();
        let html = cached.map(Some).unwrap_or_else(|| {
            let fragment = input.as_ref().and_then(|input| {
                let mut result = None;
                fragment_cache.update_value(|cache| {
                    result = super::editor_geometry::window_paint(
                        actions, input, render, &windows, cache,
                    );
                });
                result
            });
            fragment.map_or_else(
                || (!partial_paragraph).then(|| render(&[])),
                |fragment| {
                    if let Some(key) = cache_window {
                        fragment_cache.update_value(|cache| {
                            actions.retain_fragment(cache, key, fragment.clone());
                        });
                    }
                    Some(fragment)
                },
            )
        });
        let Some(html) = html else {
            // A failed partial shaping proof waits for complete geometry. It
            // cannot turn bounded preparation into synchronous full-row paint.
            ready.set(false);
            presentation.set(false);
            return;
        };
        if immediate && let Some(overlay) = node_ref.get_untracked() {
            if let Ok(Some(content)) = overlay.query_selector(".editor-highlight-content") {
                content.set_inner_html(&html);
                let _ = content.set_attribute(
                    "data-editor-scope",
                    &actions.projection_revision().to_string(),
                );
            }
            if let Some(parent) = overlay.parent_element() {
                if viewport.get_untracked().rows.is_empty() {
                    let _ = parent.class_list().remove_1("highlight-ready");
                } else {
                    let _ = parent.class_list().add_1("highlight-ready");
                }
            }
            if let Some(textarea) = textarea_ref.get_untracked() {
                sync_highlight_scroll(&textarea, &overlay);
            }
        }
        painted_whitespace.set_value(show_whitespace.get_untracked());
        painted_syntax.set_value(Some(tokens.get_untracked()));
        painted_complete_rows.set_value(!prepared_paint.get_untracked().1.is_empty());
        rendered_scope.set(actions.projection_revision());
        rendered.set(html);
        presented_scope.set_value(presentation_scope.get_untracked());
        presentation.set(!viewport.get_untracked().rows.is_empty());
        ready.set(
            !viewport.get_untracked().rows.is_empty()
                && (!partial_geometry.get_untracked() || actions.paragraph_coverage().is_some()),
        );
        let published_generation = generation.get_value();
        // Re-align after the highlighted HTML reaches the DOM.
        leptos::leptos_dom::helpers::queue_microtask(move || {
            if generation.try_get_value() != Some(published_generation)
                || open_file.try_get_untracked() != path.try_get_value()
            {
                return;
            }
            if let (Some(Some(textarea)), Some(Some(overlay))) = (
                textarea_ref.try_get_untracked(),
                node_ref.try_get_untracked(),
            ) {
                sync_highlight_scroll(&textarea, &overlay);
            }
        });
    });
    let callback = StoredValue::new_local(Closure::<dyn FnMut()>::new(move || paint.run(false)));
    paint_epoch.update(|epoch| *epoch = epoch.wrapping_add(1));
    let paint_ticket = paint_epoch.get_untracked();
    let render_probe = Callback::new(move |(rows, suffix): (Vec<usize>, bool)| {
        let Some(projection) = actions.projection() else {
            return String::new();
        };
        tokens.with_untracked(|(prepared, tokens)| {
            guides.with_untracked(|guides| {
                highlight_html(
                    tokens,
                    *prepared,
                    guides,
                    PaintRows::measured(&rows, &projection),
                    indentation.get_untracked(),
                    show_whitespace.get_untracked(),
                    suffix,
                )
            })
        })
    });
    let neighborhood = Callback::new(move |()| {
        if generation.is_disposed() {
            return None;
        }
        let input = textarea_ref.get_untracked()?;
        super::editor_geometry::neighborhood_layout(actions, &input, |rows, suffix| {
            render_probe.run((rows.to_vec(), suffix))
        })
    });
    paint_request.set(Some(EditorPaint {
        ticket: paint_ticket,
        neighborhood,
        caret: Callback::new(move |offset| {
            let input = textarea_ref.get_untracked()?;
            let mut retained = None;
            fragment_cache.update_value(|cache| {
                retained =
                    super::editor_geometry::painted_caret_rect(actions, &input, cache, offset)
                        .or_else(|| {
                            super::editor_geometry::measured_caret_rect(
                                actions, &input, cache, offset,
                            )
                        });
            });
            retained
                .or_else(|| {
                    neighborhood.run(()).and_then(|layout| {
                        super::editor_geometry::layout_caret_rect(actions, &input, &layout, offset)
                    })
                })
                .or_else(|| {
                    super::editor_geometry::probe_caret_rect(
                        actions,
                        &input,
                        offset,
                        |rows, suffix| render_probe.run((rows.to_vec(), suffix)),
                    )
                })
        }),
        flush: Callback::new(move |()| {
            if generation.is_disposed() {
                return;
            }
            if let Some(id) = request.get_value() {
                let _ = window().cancel_animation_frame(id);
            }
            queued_generation.set_value(generation.get_value());
            paint.run(true);
        }),
    }));

    #[derive(Clone, PartialEq)]
    struct NeutralFrame {
        scope: Option<crate::state_actions::editor::EditorPresentationScope>,
        projection: u64,
        font: u64,
        rows: Vec<usize>,
        metrics: Option<String>,
        indentation: openwebide_core::editor::Indentation,
        whitespace: bool,
        input: web_sys::HtmlTextAreaElement,
    }
    let neutral_frame = StoredValue::new_local(None::<NeutralFrame>);

    Effect::new(move || {
        content.track();
        native_install_revision.track();
        actions.projection_revision();
        actions.preparation_revision();
        actions.font_epoch();
        indentation.get();
        show_whitespace.get();
        tokens.with(|_| ());
        guides.with(|_| ());
        // Resolve the projection before scheduling paint, so reading it in the
        // frame callback cannot invalidate and queue a second paint afterward.
        visible.with(|_| ());
        if !fragment_windows.with(Vec::is_empty) {
            geometry_pending.get();
        }
        ready.set(false);
        if presented_scope.get_value() != presentation_scope.get() {
            presentation.set(false);
        }
        let current_path = open_file.get();
        let mounted = node_ref.get().is_some();
        if current_path != path.get_value() || !mounted {
            generation.update_value(|value| *value += 1);
            if let Some(id) = request.get_value() {
                let _ = window().cancel_animation_frame(id);
                request.set_value(None);
            }
            path.set_value(current_path);
            rendered.set(String::new());
        }
        // Native restoration stamps the installed source before this shortcut.
        // A pending neutral frame does not need to wait for rAF. Layout/native
        // binding notifications can revalidate that frame before the first rAF.
        let initial = mounted
            && (presented_scope.get_value() != presentation_scope.get_untracked()
                || painted_syntax.with_value(|paint| {
                    paint
                        .as_ref()
                        .is_none_or(|(prepared, _)| !prepared && !painted_complete_rows.get_value())
                }))
            && textarea_ref.get_untracked().is_some_and(|input| {
                current_editor_target(actions, &input)
                    && input
                        .get_attribute("data-editor-native-value-scope")
                        .and_then(|scope| scope.parse::<u64>().ok())
                        == Some(actions.projection_revision())
                    && crate::viewport::editor_scroll(&input).client_width() > 0
                    && crate::viewport::editor_scroll(&input).client_height() > 0
            })
            && prepared_paint.with_untracked(|(prepared, tokens)| {
                actions.initial_viewport_paint_ready(&visible.get_untracked(), (*prepared, tokens))
            });
        if initial {
            if let Some(id) = request.get_value() {
                let _ = window().cancel_animation_frame(id);
            }
            request.set_value(None);
            let input = textarea_ref.get_untracked().unwrap();
            let frame = NeutralFrame {
                scope: presentation_scope.get_untracked(),
                projection: actions.projection_revision(),
                font: actions.font_epoch(),
                rows: visible.get_untracked(),
                metrics: super::editor_rows::metrics_identity(&input),
                indentation: indentation.get_untracked(),
                whitespace: show_whitespace.get_untracked(),
                input,
            };
            if neutral_frame.with_value(|previous| previous.as_ref() == Some(&frame)) {
                ready.set(true);
                return;
            }
            neutral_frame.set_value(Some(frame));
            queued_generation.set_value(generation.get_value());
            paint.run(true);
            return;
        }
        neutral_frame.set_value(None);
        if mounted && request.get_value().is_none() {
            queued_generation.set_value(generation.get_value());
            let id = callback.with_value(|callback| {
                window().request_animation_frame(callback.as_ref().unchecked_ref())
            });
            if let Ok(id) = id {
                request.set_value(Some(id));
            }
        }
    });
    // Keep the JS closure owned here so cancellation also releases its captures.
    on_cleanup(move || {
        paint_request.try_update(|binding| {
            if binding
                .as_ref()
                .is_some_and(|binding| binding.ticket == paint_ticket)
            {
                *binding = None;
            }
        });
        if let Some(id) = request.get_value() {
            let _ = window().cancel_animation_frame(id);
        }
    });

    view! { <div class="editor-highlight" node_ref=node_ref><div class="editor-highlight-content" data-editor-scope=move || rendered_scope.get().to_string() data-viewport-top=move || viewport.get().top.to_string() data-document-height=move || (!partial_geometry.get()).then(|| viewport.get().height.to_string()) data-textarea-start=move || textarea_start.get().to_string() style=move || viewport.with(|view| if view.height > 0.0 { format!("padding-top:{}px;min-height:max(100%, {}px)", 12.0 + view.top, 24.0 + view.height) } else { String::new() }) inner_html=move || rendered.get() /></div>

    }
}

/// Syntax paint and word changes share spans without injecting source HTML.
pub(super) fn render_painted_diff(
    tokens: Vec<openwebide_core::diff::DiffPaintToken>,
) -> impl IntoView {
    use openwebide_core::diff::DiffChange;
    tokens
        .into_iter()
        .map(|part| {
            let change = match part.change {
                DiffChange::Unchanged => "",
                DiffChange::Deleted => " diff-word-del",
                DiffChange::Inserted => " diff-word-add",
            };
            let class = format!("{}{change}", token_class(part.token.kind));
            view! { <span class=class>{part.token.text}</span> }
        })
        .collect_view()
}

/// Render a full file edit as inline removed/added lines with intra-line word diffs.
pub(super) fn render_inline_diff(diff: FileDiff) -> impl IntoView {
    let digits = diff
        .old
        .as_deref()
        .unwrap_or_default()
        .lines()
        .count()
        .max(diff.new.lines().count())
        .max(1)
        .to_string()
        .len();
    render_diff_lines(openwebide_core::diff::paint_inline_diff(&diff), digits)
}

pub(super) fn render_commit_diff(patch: &str, path: &str) -> impl IntoView + use<> {
    let lines = openwebide_core::diff::paint_unified_diff(patch, path);
    let digits = lines
        .iter()
        .filter_map(|line| line.old_number.into_iter().chain(line.new_number).max())
        .max()
        .unwrap_or(1)
        .to_string()
        .len();
    render_diff_lines(lines, digits)
}

fn render_diff_lines(
    lines: Vec<openwebide_core::diff::DiffPaintLine>,
    digits: usize,
) -> impl IntoView {
    let body = lines
        .into_iter()
        .map(|dl| {
            let mark = dl.marker;
            let old_number = dl.old_number;
            let new_number = dl.new_number;
            let line_class = match mark {
                '+' => "diff-line add",
                '-' => "diff-line del",
                _ => "diff-line",
            };
            view! {
                <div class=line_class data-line=new_number>
                    <span class="editor-line-gutter">
                        <span class="editor-line-number">{old_number}</span>
                        <span class="editor-line-number">{new_number}</span>
                        <span class="diff-line-marker">{mark}</span>
                    </span>
                    <span class="editor-line-text">{render_painted_diff(dl.tokens)}
                        {dl.ending_note.map(|note| view! { <span class="form-hint">{note}</span> })}
                    </span>
                </div>
            }
        })
        .collect_view();
    view! {
        <div class="editor-diff editor-diff-inline" style=format!("--editor-number-width: {digits}ch")>
            <div class="inline-track">{body}</div>
        </div>
    }
}

/// Scroll aligned split panes together, including equal space for their longest line.
fn sync_split_scroll(source: NodeRef<leptos::html::Div>, target: NodeRef<leptos::html::Div>) {
    if let (Some(source), Some(target)) = (source.get_untracked(), target.get_untracked()) {
        if (source.scroll_left() - target.scroll_left()).abs() > 0.5 {
            target.set_scroll_left(source.scroll_left());
        }
        if (source.scroll_top() - target.scroll_top()).abs() > 0.5 {
            target.set_scroll_top(source.scroll_top());
        }
    }
}

/// Full aligned rows with a shared scroll extent and compact, pinned gutters.
fn render_side_by_side(diff: FileDiff) -> impl IntoView {
    let paint = openwebide_core::diff::DiffPaint::new(&diff);
    let digits = diff
        .old
        .as_deref()
        .unwrap_or_default()
        .lines()
        .count()
        .max(diff.new.lines().count())
        .max(1)
        .to_string()
        .len();
    let rows = diff_side_by_side_detailed(&diff);
    let left_pane = NodeRef::<leptos::html::Div>::new();
    let right_pane = NodeRef::<leptos::html::Div>::new();
    let left_track = NodeRef::<leptos::html::Div>::new();
    let right_track = NodeRef::<leptos::html::Div>::new();
    Effect::new(move || {
        let (Some(left), Some(right)) = (left_track.get(), right_track.get()) else {
            return;
        };
        let extent = |track: &web_sys::HtmlElement| {
            let gutter = track
                .query_selector(".editor-line-gutter")
                .ok()
                .flatten()
                .map_or(0, |node| {
                    node.unchecked_into::<web_sys::HtmlElement>().offset_width()
                });
            let text = track
                .query_selector_all(".editor-line-text")
                .ok()
                .map_or(0, |nodes| {
                    (0..nodes.length())
                        .filter_map(|index| nodes.item(index))
                        .map(|node| node.unchecked_into::<web_sys::HtmlElement>().scroll_width())
                        .max()
                        .unwrap_or(0)
                });
            gutter + text + 20
        };
        let width = extent(&left).max(extent(&right));
        for track in [left, right] {
            let _ = track
                .unchecked_ref::<web_sys::HtmlElement>()
                .style()
                .set_property("width", &format!("{width}px"));
        }
    });
    let mut old_line = 0;
    let mut new_line = 0;
    let (left, right): (Vec<_>, Vec<_>) = rows.into_iter().map(|(left, right)| {
        let changed = !matches!((&left, &right), (Some(l), Some(r)) if l.marker == ' ' && r.marker == ' ');
        let old_number = left.as_ref().map(|_| { old_line += 1; old_line });
        let new_number = right.as_ref().map(|_| { new_line += 1; new_line });
        let left_class = if left.is_none() { "sbs-cell sbs-empty" } else if changed { "sbs-cell sbs-del" } else { "sbs-cell" };
        let right_class = if right.is_none() { "sbs-cell sbs-empty" } else if changed { "sbs-cell sbs-add" } else { "sbs-cell" };
        let cell = |class, number: Option<usize>, line: Option<openwebide_core::DiffLine>, old| view! {
            <div class=class data-line=number>
                <span class="editor-line-gutter"><span class="editor-line-number">{number}</span></span>
                <span class="editor-line-text">{line.map(|line| view! {
                    {render_painted_diff(paint.line(old, number.unwrap(), line.chunks))}
                    {line.ending_note.map(|note| view! { <span class="form-hint">{note}</span> })}
                })}</span>
            </div>
        };
        (cell(left_class, old_number, left, true), cell(right_class, new_number, right, false))
    }).unzip();
    view! {
        <div class="editor-diff editor-diff-side" style=format!("--editor-number-width: {digits}ch")>
            <div class="sbs-pane" node_ref=left_pane on:scroll=move |_| sync_split_scroll(left_pane, right_pane)><div class="sbs-track" node_ref=left_track>{left}</div></div>
            <div class="sbs-pane" node_ref=right_pane on:scroll=move |_| sync_split_scroll(right_pane, left_pane)><div class="sbs-track" node_ref=right_track>{right}</div></div>
        </div>
    }
}

/// Render a friendly placeholder for binary or non-previewable files.
fn render_placeholder_view(
    path: &str,
    kind: FileKind,
    set_view_mode: WriteSignal<ViewMode>,
    on_open_lossy: Callback<()>,
) -> impl IntoView {
    let file_name = path.rsplit('/').next().unwrap_or(path).to_string();
    let glyph = kind.glyph(path);
    let desc = kind.description(path);
    let path_clone = path.to_string();
    let copied = RwSignal::new(false);

    let on_copy = Callback::new(move |_| {
        let p = path_clone.clone();
        if let Some(clipboard) = web_sys::window().map(|w| w.navigator().clipboard()) {
            let _ = clipboard.write_text(&p);
            copied.set(true);
        }
    });

    view! {
        <div class="editor-placeholder-view">
            <div class="placeholder-icon">{glyph}</div>
            <div class="placeholder-title">{file_name}</div>
            <div class="placeholder-desc">{desc}</div>
            <p class="placeholder-hint">
                "This file is binary or not directly previewable in the text editor."
            </p>
            <div class="placeholder-actions">
                <Button
                    variant=ButtonVariant::Default
                    size=ButtonSize::Sm
                    on_click=on_copy
                >
                    {move || if copied.get() { "✓ Copied" } else { "Copy Path" }}
                </Button>
                <Button
                    variant=ButtonVariant::Ghost
                    size=ButtonSize::Sm
                    on_click=Callback::new(move |_| {
                        set_view_mode.set(ViewMode::Code);
                        on_open_lossy.run(());
                    })
                >
                    "Open as Text anyway"
                </Button>
            </div>
        </div>
    }
}

/// Render the preview view for a file: image asset, markdown formatted HTML,
/// or binary placeholder.
fn render_preview_view(
    path: &str,
    content: &str,
    media_url: Option<String>,
    set_view_mode: WriteSignal<ViewMode>,
    on_open_lossy: Callback<()>,
) -> impl IntoView {
    let kind = FileKind::from_path(path);
    let file_name = path.rsplit('/').next().unwrap_or(path).to_string();

    if openwebide_core::file_type::extension(path).as_deref() == Some("pdf") {
        return media_url.map_or_else(
            || render_placeholder_view(path, kind, set_view_mode, on_open_lossy).into_any(),
            |url| view! { <iframe class="editor-pdf-preview" src=url title=format!("PDF preview: {file_name}") /> }.into_any(),
        );
    }
    match kind {
        FileKind::Image => {
            if let Some(url) = media_url {
                view! {
                    <div class="editor-media-preview">
                        <div class="editor-image-frame">
                            <img src=url alt=file_name.clone() class="editor-preview-image" />
                        </div>
                        <div class="editor-media-info">
                            <span class="media-filename">{file_name}</span>
                            <span class="media-type-pill">"Image Asset"</span>
                        </div>
                    </div>
                }
                .into_any()
            } else if std::path::Path::new(&path)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
                && !content.is_empty()
            {
                let src = crate::markdown::svg_data_url(content);
                view! {
                    <div class="editor-media-preview">
                        <div class="editor-image-frame"><img src=src alt=file_name.clone() class="editor-preview-image"/></div>
                        <div class="editor-media-info">
                            <span class="media-filename">{file_name}</span>
                            <span class="media-type-pill">"SVG Vector"</span>
                        </div>
                    </div>
                }
                .into_any()
            } else {
                render_placeholder_view(path, kind, set_view_mode, on_open_lossy).into_any()
            }
        }
        FileKind::Markdown => {
            let html = render_markdown(content);
            view! {
                <div class="editor-preview markdown" inner_html=html />
            }
            .into_any()
        }
        FileKind::Binary | FileKind::Media => {
            render_placeholder_view(path, kind, set_view_mode, on_open_lossy).into_any()
        }
        FileKind::Text => {
            if FileKind::is_plain_document(path) {
                view! { <pre class="editor-preview editor-document-preview">{content.to_string()}</pre> }.into_any()
            } else {
                render_placeholder_view(path, kind, set_view_mode, on_open_lossy).into_any()
            }
        }
    }
}

fn render_markdown_diff(diff: &FileDiff) -> impl IntoView {
    let html = if diff.old_unavailable {
        render_markdown(&diff.new)
    } else {
        crate::markdown::render_diff(diff.old.as_deref().unwrap_or_default(), &diff.new)
    };
    view! { <div class="editor-preview markdown rich-preview" inner_html=html /> }
}

/// The code editor: a full-height textarea bound to the open file's content,
/// with a header showing path, dirty indicator, view mode toggles, and Save button.
///
/// Automatically defaults to Preview mode for non-text files (images, binaries)
/// with informative placeholders for non-previewable files.
fn pair_character(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let ch = chars.next()?;
    (chars.next().is_none() && matches!(ch, '(' | ')' | '[' | ']' | '{' | '}' | '\'' | '"' | '`'))
        .then_some(ch)
}

fn apply_editor_command(
    actions: EditorActions,
    command: EditorCommand,
    textarea: &web_sys::HtmlTextAreaElement,
    error: RwSignal<Option<String>>,
) -> bool {
    if actions.command_requires_structure(command) {
        let selection = projected_selection(actions, textarea);
        let indentation = actions.rules_untracked().indentation;
        let textarea = textarea.clone();
        let request = actions.command_when_ready(command, selection, indentation);
        leptos::task::spawn_local(async move {
            let result = request.await;
            if error.is_disposed() || !current_editor_target(actions, &textarea) {
                return;
            }
            match result {
                Ok(Some(selection)) => {
                    error.set(None);
                    refresh_editor_folds(actions);
                    render_editor_selection(actions, &textarea, selection, false);
                }
                Err(failure) => error.set(Some(failure.to_string())),
                Ok(None) => (),
            }
        });
        return true;
    }
    if let Ok(Some(selection)) = actions.command(
        command,
        projected_selection(actions, textarea),
        actions.rules_untracked().indentation,
    ) {
        refresh_editor_folds(actions);
        render_editor_selection(actions, textarea, selection, false);
        true
    } else {
        false
    }
}

#[component]
pub fn Editor(
    #[prop(optional)] on_load_git_diff: Option<Callback<()>>,
    #[prop(optional)] on_discard_git_diff: Option<Callback<()>>,
    read_only: Signal<bool>,
    on_open_lossy: Callback<()>,
    on_save: Callback<()>,
    on_accept: Callback<()>,
    on_reject: Callback<()>,
) -> impl IntoView {
    let workspace = expect_context::<WorkspaceState>();
    let file_actions = use_context::<crate::state_actions::file_tree::FileTreeActions>();
    let panel_actions = use_context::<crate::state_actions::layout::LayoutActions>();
    let editor_actions = EditorActions::new(workspace);
    editor_actions.install_syntax_worker();
    Effect::new(move || {
        if let Some(owner) = workspace.editor_composition.get()
            && (!editor_actions.composition_owner_current(&owner)
                || !workspace.editor_documents.with(|documents| {
                    documents.get(&owner.key).is_some_and(|document| {
                        document.is_composing()
                            && workspace.content.with(|source| document.text() == source)
                    })
                }))
        {
            editor_actions.cancel_composition();
        }
    });
    let paint_indentation = Memo::new(move |_| editor_actions.rules().indentation);
    let paint_whitespace = Memo::new(move |_| editor_actions.preferences().show_whitespace);
    let tab_moves_focus = RwSignal::new(false);
    let tab_focus_announcement = RwSignal::new("");
    let paste_matches_indentation = RwSignal::new(false);
    let column_anchor =
        StoredValue::new(None::<crate::state_actions::editor::EditorColumnSelection>);
    Effect::new(move |_| {
        workspace.active_project.track();
        workspace.open_file.track();
        paste_matches_indentation.set(false);
    });
    let file_tree_actions = use_context::<crate::state_actions::file_tree::FileTreeActions>();
    // Temporary edit locks (loading, reviews and file operations) are not
    // encoding failures and must not insert a warning into the tab row.
    let file_read_only = read_only;
    let read_only = Signal::derive(move || {
        read_only.get()
            || workspace.editor_loading.get()
            || workspace.is_resolving()
            || file_tree_actions.is_some_and(|actions| actions.busy.get())
    });
    let recovery_blocks_save = Memo::new(move |_| {
        workspace
            .active_project
            .get()
            .zip(workspace.open_file.get())
            .is_some_and(|key| {
                workspace
                    .editor_recovery_checks
                    .with(|checks| checks.contains_key(&key))
            })
    });
    let projects = expect_context::<ProjectsState>();
    let git = expect_context::<GitState>();

    let open_file = workspace.open_file.read_only();
    let content = workspace.content.read_only();
    let projection = Memo::new(move |_| {
        content.track();
        open_file.track();
        workspace.active_project.track();
        workspace.editor_fold_revision.track();
        if editor_actions.limit().is_some() {
            return openwebide_core::editor::FoldProjection::new("", &Default::default());
        }
        editor_actions.prepare_projection().unwrap_or_else(|| {
            openwebide_core::editor::FoldProjection::new("", &Default::default())
        })
    });
    let source_line_count = Memo::new(move |_| {
        projection.with(|_| ());
        editor_actions.source_line_count().unwrap_or(1)
    });
    let fold_state = Memo::new(move |_| {
        workspace.editor_fold_revision.track();
        content.track();
        open_file.track();
        workspace.active_project.track();
        editor_actions.fold_state().unwrap_or_default()
    });
    Effect::new(move || {
        content.track();
        open_file.track();
        workspace.active_project.track();
        editor_actions.rules();
        workspace.editor_preparation_revision.track();
        untrack(|| refresh_editor_folds(editor_actions));
    });
    let dirty = workspace.dirty.read_only();
    let pending_diff: Signal<Option<FileDiff>> = Signal::from(workspace.pending_diff);
    let media_url = workspace.media_url.read_only();
    let preview_disabled = Signal::derive(move || {
        workspace.open_file.with(|path| {
            path.as_ref()
                .is_none_or(|path| !FileKind::supports_preview(path))
        })
    });
    let git_head_diff = Signal::derive(move || {
        let open_file = workspace.open_file.get()?;
        let project_id = projects.active_project.get();
        let head = git.head_content.get()?;
        if head.project_id != project_id || head.path != open_file || head.content.is_err() {
            return None;
        }
        let old = head.content.ok();
        let new = workspace.content.get();
        if old.is_none() && new.is_empty() {
            return None;
        }
        Some(FileDiff {
            path: open_file,
            old,
            new: new.into(),
            old_unavailable: false,
            backup_path: None,
        })
    });
    let binary_head = Signal::derive(move || {
        git.head_content.get().is_some_and(|head| {
            head.project_id == projects.active_project.get()
                && Some(head.path) == workspace.open_file.get()
                && head
                    .content
                    .as_ref()
                    .err()
                    .is_some_and(|error| error == "binary file")
        })
    });
    let can_revert = Signal::derive(move || {
        let Some(open_file) = workspace.open_file.get() else {
            return false;
        };
        let project_id = projects.active_project.get();
        git.head_content.get().is_some_and(|head| {
            head.project_id == project_id && head.path == open_file && head.content.is_ok()
        })
    });
    let ta = NodeRef::<leptos::html::Textarea>::new();
    let hl = NodeRef::<leptos::html::Div>::new();
    let highlight_ready = RwSignal::new(false);
    let native_install_revision = RwSignal::new(0_u64);
    let highlight_visible = RwSignal::new(false);
    let layout_revision = RwSignal::new(0_u64);
    let source_extent = Memo::new(move |_| {
        layout_revision.track();
        let measured = editor_actions.measured_rows()?;
        let input = ta.get()?;
        if input
            .parent_element()?
            .get_attribute("data-editor-account")
            .as_deref()
            != Some(measured.account_generation.to_string().as_str())
            || super::editor_rows::metrics_identity(&input).as_ref() != Some(&measured.metrics)
        {
            return None;
        }
        let extent = super::editor_rows::document_extent(&input, &measured.rows)?;
        Some((
            editor_actions.projection_revision(),
            measured.revision,
            measured.account_generation,
            extent,
        ))
    });
    let source_height = Memo::new(move |_| {
        layout_revision.track();
        highlight_ready.track();
        projection.track();
        editor_actions.track_native_context();
        if source_extent.get().is_some() {
            return None;
        }
        let input = ta.get()?;
        if !current_editor_target(editor_actions, &input) {
            return None;
        }
        let (row_height, padding) = super::editor_rows::uniform_row_dimensions(&input)?;
        let height = editor_actions.initial_native_height(row_height, padding)?;
        if !crate::viewport::check_editor_extent(0.0, height) {
            return None;
        }
        Some((
            editor_actions.projection_revision(),
            editor_actions.view_revision(),
            editor_actions.account_generation(),
            height,
        ))
    });
    let extent_owner = Memo::new(move |_| {
        source_extent
            .get()
            .map(|(scope, view, account, _)| (scope, view, account))
            .or_else(|| {
                source_height
                    .get()
                    .map(|(scope, view, account, _)| (scope, view, account))
            })
    });
    Effect::new(move || {
        source_extent.track();
        source_height.track();
        if let Some(input) = ta.get() {
            crate::viewport::refresh_editor_scroll(&input);
        }
    });
    let viewport = Memo::new(move |_| {
        layout_revision.track();
        highlight_ready.track();
        native_install_revision.track();
        let (rows, uniform) = projection.with(|view| (view.lines().len(), view.has_uniform_rows()));
        if editor_actions.preferences().word_wrap || !uniform {
            if let Some(measured) = editor_actions.measured_rows() {
                let input = ta.get();
                return measured.rows.window(
                    input.as_ref().map_or(0.0, |input| {
                        crate::viewport::editor_scroll(input).scroll_top()
                    }),
                    input.as_ref().map_or(390.0, |input| {
                        f64::from(crate::viewport::editor_scroll(input).client_height())
                    }),
                    12.0,
                );
            }
            // Completed-prefix paint is restricted to the origin until complete
            // source extents can place restored scroll requests.
            if editor_actions.scroll().top == 0.0
                && editor_actions.scroll().left == 0.0
                && let Some(prefix) = editor_actions.measured_prefix()
                && let Some(input) = ta.get()
                && super::editor_rows::metrics_identity(&input).as_ref()
                    == Some(&prefix.paint.metrics)
            {
                return prefix.rows.window(
                    0.0,
                    f64::from(crate::viewport::editor_scroll(&input).client_height()),
                    12.0,
                );
            }
            if editor_actions.scroll().top >= 0.0
                && editor_actions.scroll().left == 0.0
                && let Some(coverage) = editor_actions.paragraph_coverage()
                && let Some(input) = ta.get()
                && current_editor_target(editor_actions, &input)
                && super::editor_rows::metrics_identity(&input).as_ref()
                    == Some(&coverage.paint.metrics)
                && coverage.coverage.covered_height()
                    > editor_actions.scroll().top
                        + f64::from(crate::viewport::editor_scroll(&input).client_height())
                        + 24.0
            {
                return openwebide_core::editor::EditorViewport {
                    rows: 0..1,
                    top: 0.0,
                    height: coverage.coverage.covered_height(),
                };
            }
            let batched = projection.with(|view| {
                openwebide_core::editor::needs_measured_batches(rows, view.text().len())
            });
            return openwebide_core::editor::EditorViewport {
                rows: 0..if batched { 0 } else { rows },
                top: 0.0,
                height: 0.0,
            };
        }
        let input = ta.get();
        let row_height = input.as_ref().map_or(19.5, editor_row_height);
        openwebide_core::editor::EditorViewport::unwrapped(
            rows,
            input.as_ref().map_or(0.0, |input| {
                crate::viewport::editor_scroll(input).scroll_top()
            }),
            input.as_ref().map_or(390.0, |input| {
                f64::from(crate::viewport::editor_scroll(input).client_height())
            }),
            row_height,
            12.0,
        )
    });
    let visible_rows = Memo::new(move |_| {
        projection.with(|view| {
            view.lines()[viewport.get().rows]
                .iter()
                .map(|line| line.source_line)
                .collect::<Vec<_>>()
        })
    });
    let textarea_start = Memo::new(move |_| {
        projection.with(|view| {
            view.lines()
                .get(viewport.get().rows.start)
                .map_or(0, |line| line.textarea_start)
        })
    });
    let bracket_marks = RwSignal::new(Vec::<(f64, f64, f64, f64)>::new());
    let view_mode = RwSignal::new(ViewMode::Code);

    let root = NodeRef::<leptos::html::Div>::new();
    let find_input = NodeRef::<leptos::html::Input>::new();
    let find_open = RwSignal::new(false);
    let query = RwSignal::new(String::new());
    let search_options = RwSignal::new(openwebide_core::editor::SearchOptions::default());
    let replacement_text = RwSignal::new(String::new());
    let replace_open = RwSignal::new(false);
    let replacement_error = RwSignal::new(None::<String>);
    let action_error = RwSignal::new(None::<String>);
    let paint_request = RwSignal::new(None::<EditorPaint>);
    let paint_epoch = RwSignal::new(0_u64);
    let motion_adapter = super::editor_motion::MotionAdapter {
        actions: editor_actions,
        paint: paint_request,
        error: action_error,
    };
    Effect::new(move || {
        workspace.editor_documents.track();
        workspace.editor_composition.track();
        paint_epoch.track();
        layout_revision.track();
        if let Some(input) = ta.get() {
            let composing = editor_actions.is_composing();
            if !composing {
                crate::viewport::align_editor_native_input(&input, 0.0, 0.0, false);
            } else if let Some(selection) = content.with(|source| editor_actions.selection(source))
                && let Some(rect) = paint_request
                    .get()
                    .and_then(|paint| paint.caret.run(selection.head))
            {
                crate::viewport::align_editor_native_input(&input, rect.left(), rect.top(), true);
            }
        }
    });
    let native_scope = StoredValue::new(None);
    let native_input_node = StoredValue::new_local(None::<web_sys::HtmlTextAreaElement>);
    Effect::new(move || {
        let scope = editor_actions.presentation_scope();
        let mode = view_mode.get();
        let node = ta.get().filter(|input| input.is_connected());
        let remounted = native_input_node.with_value(|previous| previous != &node);
        if native_scope.get_value() != scope || remounted || mode != ViewMode::Code {
            editor_actions.release_native_context();
            native_scope.set_value(scope);
            native_input_node.set_value(node.clone());
        }
        if mode != ViewMode::Code || node.is_none() {
            return;
        }
        let ready = highlight_ready.get();
        let extent = source_extent.get();
        // Touch selection still relies on the full native surface. Keep that
        // capability fallback until source-owned touch handles are implemented.
        let touch = window()
            .match_media("(any-pointer: coarse)")
            .ok()
            .flatten()
            .is_some_and(|query| query.matches());
        if ready && extent.is_some() && editor_actions.native_geometry_pending() {
            let scroll = editor_actions.scroll();
            if editor_actions.finish_initial_native_context(editor_actions.view_revision())
                && let Some(input) = node.as_ref()
            {
                stamp_editor_input(editor_actions, input);
                crate::viewport::set_editor_scroll_top(input, scroll.top);
                crate::viewport::set_editor_scroll_left(input, scroll.left);
            }
        }
        if ready
            && !touch
            && editor_actions.bound_native_context().is_none()
            && let Some(input) = node.filter(|input| current_editor_target(editor_actions, input))
        {
            let frame_scope = hl.get().and_then(|overlay| {
                overlay
                    .query_selector(".editor-highlight-content")
                    .ok()
                    .flatten()?
                    .get_attribute("data-editor-scope")?
                    .parse::<u64>()
                    .ok()
            });
            let cold = extent.is_none()
                && frame_scope
                    .and_then(|scope| {
                        editor_actions.cold_native_extent(
                            scope,
                            f64::from(input.scroll_width()),
                            f64::from(input.scroll_height()),
                            || input.value(),
                        )
                    })
                    .is_some_and(|extent| {
                        crate::viewport::check_editor_extent(extent.width, extent.height)
                    });
            if extent.is_some() || cold {
                untrack(|| {
                    // Capture complete-source dimensions before replacing native text.
                    // The scroll adapter retains them until exact row measurements arrive.
                    crate::viewport::refresh_editor_scroll(&input);
                    editor_actions.bind_native_context();
                });
            }
        }
    });
    let pointer_adapter = super::editor_pointer::PointerAdapter::new(
        editor_actions,
        workspace,
        ta,
        highlight_ready,
        motion_adapter,
    );
    Effect::new(move || {
        workspace.active_project.track();
        workspace.open_file.track();
        workspace.pending_epoch.track();
        action_error.set(None);
    });
    let search_scope =
        RwSignal::new(None::<(crate::state::workspace::EditorText, std::ops::Range<usize>)>);
    let scope_candidate =
        RwSignal::new(None::<(crate::state::workspace::EditorText, std::ops::Range<usize>)>);

    let go_open = RwSignal::new(false);
    let go_query = RwSignal::new(String::new());
    let go_input = NodeRef::<leptos::html::Input>::new();
    let cursor_status = Memo::new(move |_| {
        content.track();
        open_file.track();
        workspace.active_project.track();
        workspace.editor_documents.track();
        editor_actions.cursor_status()
    });
    let go_target = Memo::new(move |_| {
        content.track();
        editor_actions.navigation_target(&go_query.get())
    });
    Effect::new(move || {
        highlight_ready.get();
        layout_revision.track();
        editor_actions.preparation_revision();
        cursor_status.get();
        open_file.track();
        view_mode.track();
        content.track();
        let Some(decorations) = editor_actions.decorations(highlight_ready.get_untracked()) else {
            return;
        };
        leptos::leptos_dom::helpers::queue_microtask(move || {
            let Some(Some(textarea)) = ta.try_get_untracked() else {
                return;
            };
            if !current_editor_target(editor_actions, &textarea)
                || !editor_actions.decorations_current(&decorations)
            {
                return;
            }
            let Some(parent) = textarea.parent_element() else {
                return;
            };
            if let Ok(rows) = parent.query_selector_all(".editor-active-line") {
                for index in 0..rows.length() {
                    if let Some(row) = rows
                        .item(index)
                        .and_then(|row| row.dyn_into::<web_sys::Element>().ok())
                    {
                        let _ = row.class_list().remove_1("editor-active-line");
                    }
                }
            }
            if let Ok(Some(row)) = parent.query_selector(&format!(
                ".editor-source-line[data-line='{}']",
                decorations.active_line,
            )) {
                let _ = row.class_list().add_1("editor-active-line");
            }
            let mut marks = Vec::new();
            if highlight_ready.get_untracked()
                && let Some(brackets) = decorations.brackets
            {
                for (line, column) in brackets {
                    if let Ok(Some(row)) =
                        parent.query_selector(&format!(".editor-source-line[data-line='{line}']"))
                    {
                        for range in
                            super::editor_paint::ranges(&row, column..column.saturating_add(1))
                        {
                            let rect = glyph_rect(&range);
                            let bounds = parent.get_bounding_client_rect();
                            marks.push((
                                rect.left() - bounds.left()
                                    + crate::viewport::editor_scroll(&textarea).scroll_left(),
                                rect.top() - bounds.top()
                                    + crate::viewport::editor_scroll(&textarea).scroll_top(),
                                rect.width(),
                                rect.height(),
                            ));
                        }
                    }
                }
            }
            if editor_actions.decorations_current(&decorations) {
                bracket_marks.set(marks);
            }
        });
    });
    let open_go = Callback::new(move |()| {
        let (line, column, _) = cursor_status.get_untracked();
        go_query.set(format!("{line}:{column}"));
        go_open.set(true);
    });
    let go = Callback::new(move |()| {
        if let (Some(offset), Some(textarea)) = (go_target.get_untracked(), ta.get_untracked())
            && current_editor_target(editor_actions, &textarea)
        {
            go_open.set(false);
            navigate_editor(editor_actions, &textarea, offset, paint_request);
        }
    });
    let jump_bracket = Callback::new(move |()| {
        if let Some(textarea) = ta.get_untracked()
            && current_editor_target(editor_actions, &textarea)
        {
            let selection = projected_selection(editor_actions, &textarea);
            if let Some((_, target)) = editor_actions.matching_bracket(selection.head) {
                navigate_editor(editor_actions, &textarea, target, paint_request);
            }
        }
    });
    Effect::new(move || {
        if go_open.get()
            && let Some(input) = go_input.get()
        {
            let _ = input.focus();
            input.select();
        }
    });
    Effect::new(move || {
        open_file.track();
        workspace.active_project.track();
        view_mode.track();
        go_open.set(false);
    });

    let match_index = RwSignal::new(0_usize);
    let find_source = Memo::new(move |_| {
        pending_diff
            .get()
            .map_or_else(|| content.get(), |diff| diff.new.into())
    });
    let search_result = Memo::new(move |_| {
        let source = find_source.get();
        let scope = search_scope
            .get()
            .filter(|(snapshot, _)| snapshot == &source)
            .map(|(_, scope)| scope);
        editor_actions.search(&source, &query.get(), search_options.get(), scope)
    });
    let matches = Memo::new(move |_| {
        search_result.with(|result| {
            result
                .as_ref()
                .map(|matches| browser_matches(&find_source.get(), matches))
                .unwrap_or_default()
        })
    });
    Effect::new(move || {
        let source = find_source.get();
        if search_scope.with_untracked(|scope| {
            scope
                .as_ref()
                .is_some_and(|(snapshot, _)| snapshot != &source)
        }) {
            search_scope.set(None);
        }
        replacement_error.set(None);
    });
    Effect::new(move || {
        open_file.track();
        workspace.active_project.track();
        search_scope.set(None);
        scope_candidate.set(None);
        replacement_error.set(None);
    });
    Effect::new(move || {
        if find_open.get() {
            let source = content.get_untracked();
            let selection = ta
                .get_untracked()
                .filter(|textarea| current_editor_target(editor_actions, textarea))
                .map(|textarea| projected_selection(editor_actions, &textarea));
            scope_candidate.set(
                selection
                    .filter(|selection| !selection.range().is_empty())
                    .map(|selection| (source, selection.range())),
            );
        }
    });
    let replace = Callback::new(move |all: bool| {
        if read_only.get_untracked()
            || view_mode.get_untracked() != ViewMode::Code
            || pending_diff.get_untracked().is_some()
        {
            return;
        }
        let Some(textarea) = ta
            .get_untracked()
            .filter(|textarea| current_editor_target(editor_actions, textarea))
        else {
            return;
        };
        let source = content.get_untracked();
        let scope = search_scope
            .get_untracked()
            .filter(|(snapshot, _)| snapshot == &source)
            .map(|(_, scope)| scope);
        let count = matches.get_untracked().len();
        if count == 0 {
            return;
        }
        let index = (!all).then(|| match_index.get_untracked() % count);
        match editor_actions.replace_search(
            &source,
            &query.get_untracked(),
            search_options.get_untracked(),
            scope.clone(),
            &replacement_text.get_untracked(),
            index,
        ) {
            Ok(Some(selection)) => {
                let updated = editor_actions.source();
                if let Some(scope) = scope {
                    // Retain the selection span by subtracting its unchanged suffix.
                    let end = updated
                        .len()
                        .saturating_sub(source.len() - scope.end)
                        .max(scope.start);
                    search_scope.set(Some((updated, scope.start..end)));
                }
                refresh_editor_folds(editor_actions);
                render_editor_selection(editor_actions, &textarea, selection, true);
                match_index.set(index.unwrap_or(0));
                replacement_error.set(None);
            }
            Ok(None) => (),
            Err(error) => replacement_error.set(Some(error.to_string())),
        }
    });
    let navigate = Callback::new(move |forward: bool| {
        let count = matches.get_untracked().len();
        if count > 0 {
            match_index.update(|index| {
                *index = if forward {
                    (*index + 1) % count
                } else {
                    (*index + count - 1) % count
                }
            });
        }
    });
    Effect::new(move || {
        query.track();
        search_options.track();
        search_scope.track();
        open_file.track();
        projects.active_project.track();
        match_index.set(0);
    });
    Effect::new(move || {
        if find_open.get()
            && let Some(input) = find_input.get()
        {
            let _ = input.focus();
        }
    });
    Effect::new(move || {
        let active = find_open.get() && view_mode.get() != ViewMode::Preview;
        if !active {
            if let Some(root) = root.get() {
                clear_find_marks(&root);
            }
            return;
        }
        highlight_ready.track();
        let results = matches.get();
        let selected = results
            .get(match_index.get() % results.len().max(1))
            .copied();
        let mode = view_mode.get();
        let path = open_file.get();
        let project = projects.active_project.get();
        let source = find_source.get();
        let search = query.get();
        let options = search_options.get();
        let scope = search_scope.get();
        let index = match_index.get();
        leptos::leptos_dom::helpers::queue_microtask(move || {
            if open_file.try_get_untracked() != Some(path)
                || projects.active_project.try_get_untracked() != Some(project)
                || find_source.try_with_untracked(|current| current == &source) != Some(true)
                || query.try_get_untracked() != Some(search)
                || search_options.try_get_untracked() != Some(options)
                || search_scope.try_get_untracked() != Some(scope)
                || match_index.try_get_untracked() != Some(index)
                || view_mode.try_get_untracked() != Some(mode)
                || find_open.try_get_untracked() != Some(active)
            {
                return;
            }
            let Some(Some(root)) = root.try_get_untracked() else {
                return;
            };
            clear_find_marks(&root);
            let Some((start, end, line)) = selected.filter(|_| active) else {
                return;
            };
            let line_start = source
                .split_inclusive('\n')
                .take(line.saturating_sub(1))
                .map(|part| part.encode_utf16().count())
                .sum::<usize>();
            let column = start.saturating_sub(u32::try_from(line_start).unwrap_or(u32::MAX));
            if mode == ViewMode::Code
                && let Some(Some(textarea)) = ta.try_get_untracked()
            {
                editor_actions.fold_command(openwebide_core::editor::FoldCommand::Reveal(line.saturating_sub(1)));
                if let Some(projection) = editor_actions.projection() {
                    let _ = projection;
                    let source_selection = openwebide_core::editor::Selection { anchor: openwebide_core::editor::utf16_to_byte(&source, start as usize), head: openwebide_core::editor::utf16_to_byte(&source, end as usize) };
                    let _ = editor_actions.record_selection(source_selection);
                    render_editor_selection(editor_actions, &textarea, source_selection, false);
                } else { let _ = textarea.set_selection_range(start, end); }
                if let Ok(Some(row)) =
                    root.query_selector(&format!(".editor-highlight .editor-source-line[data-line='{line}']"))
                {
                    let row: web_sys::HtmlElement = row.unchecked_into();
                    let gutter = window().get_computed_style(&textarea).ok().flatten().and_then(|style| style.get_property_value("padding-left").ok()).and_then(|padding| padding.trim_end_matches("px").parse::<f64>().ok()).unwrap_or(40.0);
                    let source_offset = openwebide_core::editor::utf16_to_byte(&source, start as usize);
                    let caret = caret_rect(&row, column)
                        .or_else(|| paint_request.get_untracked()?.caret.run(source_offset));
                    if let Some(caret) = caret {
                        crate::viewport::set_editor_scroll_top(&textarea, (crate::viewport::editor_scroll(&textarea).scroll_top() + caret.top()
                            - crate::viewport::editor_scroll(&textarea).get_bounding_client_rect().top() - 12.0).max(0.0));
                        reveal_caret_column(&caret, &textarea, gutter);
                    } else {
                        crate::viewport::set_editor_scroll_top(&textarea, f64::from(row.offset_top().saturating_sub(12)));
                    }
                    if let Some(Some(overlay)) = hl.try_get_untracked() {
                        sync_highlight_scroll(&textarea, &overlay);
                        reveal_match_column(&row, &textarea, column, gutter);
                        sync_highlight_scroll(&textarea, &overlay);
                    }
                } else if let (Some(measured), Some(projection)) = (editor_actions.measured_rows(), editor_actions.projection()) {
                    let row = projection.lines().partition_point(|row| row.source_line < line.saturating_sub(1));
                    if let Some(top) = measured.rows.top(row) { crate::viewport::set_editor_scroll_top(&textarea, top); }
                } else if !editor_actions.preferences().word_wrap
                    && let Some(projection) = editor_actions.projection()
                {
                    // The selected row may be outside the paint window. Scroll
                    // by its projected row; the next paint handles column reveal.
                    let row = projection.lines().partition_point(|row| row.source_line < line.saturating_sub(1));
                    crate::viewport::set_editor_scroll_top(&textarea, f64::from(u32::try_from(row).unwrap_or(u32::MAX)) * editor_row_height(&textarea));
                }
            } else if let Ok(Some(row)) =
                root.query_selector(&format!(".editor-diff-inline [data-line='{line}'], .sbs-pane:last-child [data-line='{line}']"))
            {
                let _ = row.class_list().add_1("editor-find-match");
                if let Ok(Some(body)) = row.closest(".sbs-pane, .editor-diff") {
                    let body: web_sys::HtmlElement = body.unchecked_into();
                    body.set_scroll_top(
                        body.scroll_top() + row.get_bounding_client_rect().top()
                            - body.get_bounding_client_rect().top(),
                    );
                    if let Ok(Some(text)) = row.query_selector(".editor-line-text") {
                        let gutter = row.query_selector(".editor-line-gutter").ok().flatten().map_or(0.0, |gutter| gutter.get_bounding_client_rect().width()) + 8.0;
                        reveal_match_column(&text, &body, column, gutter);
                    }
                }
            }
        });
    });

    // Default to Preview for non-text files; default to Code for source files.
    Effect::new(move || {
        if let Some(path) = open_file.get() {
            let kind = FileKind::from_path(&path);
            if kind.is_non_text() && FileKind::supports_preview(&path) {
                view_mode.set(ViewMode::Preview);
            } else if pending_diff.get().is_none() && kind != FileKind::Markdown {
                view_mode.set(ViewMode::Code);
            }
        }
    });

    // Keep the selected view while hunk decisions update the same file.
    let pending_path = Memo::new(move |_| pending_diff.get().map(|diff| diff.path));
    Effect::new(move || {
        if pending_path.get().is_some() {
            view_mode.set(ViewMode::InlineDiff);
        }
    });

    // Restore a remounted document independently of whether its text changed.
    // Existing nodes retain their current viewport during commands and external updates.
    let restored_textarea =
        StoredValue::new_local(None::<(web_sys::HtmlTextAreaElement, Option<(i64, String)>)>);
    let awaiting_initial_native_context = StoredValue::new(false);
    Effect::new(move || {
        let full_projection = projection.get();
        editor_actions.track_native_context();
        let key = workspace.active_project.get().zip(open_file.get());
        let Some(el) = ta.get() else {
            return;
        };
        if !current_editor_target(editor_actions, &el) {
            return;
        }
        let mounted = restored_textarea
            .with_value(|previous| previous.as_ref() != Some(&(el.clone(), key.clone())));
        let touch = window()
            .match_media("(any-pointer: coarse)")
            .ok()
            .flatten()
            .is_some_and(|query| query.matches());
        if mounted {
            awaiting_initial_native_context.set_value(true);
        }
        if awaiting_initial_native_context.get_value()
            && !touch
            && view_mode.get_untracked() == ViewMode::Code
        {
            workspace.editor_documents.track();
            awaiting_initial_native_context.set_value(
                editor_actions.prepare_initial_native_context()
                    == crate::state_actions::editor::InitialNativeContextPreparation::WaitingForDocument,
            );
        }
        let current_projection = editor_actions.input_projection().unwrap_or(full_projection);
        let value = current_projection.textarea_text();
        // FoldProjection already owns the textarea-normalized value.
        let changed = el.value() != value;
        if mounted || changed {
            let scroll = if mounted {
                editor_actions.scroll()
            } else {
                crate::state::workspace::EditorScroll {
                    top: crate::viewport::editor_scroll(&el).scroll_top(),
                    left: crate::viewport::editor_scroll(&el).scroll_left(),
                }
            };
            if changed {
                el.set_value(value);
            }
            if let Some(selection) = editor_actions
                .current_selection()
                .and_then(|selection| current_projection.input_selection(selection).ok())
                .or_else(|| mounted.then_some(openwebide_core::editor::Selection::caret(0)))
            {
                restore_editor_selection(&el, &current_projection, selection);
            }
            // Scroll reconciliation must already know that this value is a window;
            // otherwise it replaces retained source extents with local dimensions.
            stamp_editor_input(editor_actions, &el);
            crate::viewport::set_editor_scroll_top(&el, scroll.top);
            crate::viewport::set_editor_scroll_left(&el, scroll.left);
            restored_textarea.set_value(Some((el.clone(), key)));
            // The installed native value establishes cold source dimensions.
            // Let initial viewport paint observe it without waiting for ResizeObserver/rAF.
            if editor_actions.bound_native_context().is_none() {
                let installed = editor_actions.projection_revision();
                let input = el.clone();
                leptos::leptos_dom::helpers::queue_microtask(move || {
                    if !native_install_revision.is_disposed()
                        && current_editor_target(editor_actions, &input)
                        && editor_actions.projection_revision() == installed
                    {
                        native_install_revision
                            .update(|revision| *revision = revision.wrapping_add(1));
                    }
                });
            }
            if let Some(overlay) = hl.get_untracked() {
                sync_highlight_scroll(&el, &overlay);
            }
        }
        stamp_editor_input(editor_actions, &el);
    });

    let convert_indentation = Callback::new(move |()| {
        if read_only.get_untracked()
            || pending_diff.get_untracked().is_some()
            || view_mode.get_untracked() != ViewMode::Code
            || editor_actions.is_composing()
        {
            return;
        }
        if let Some(textarea) = ta.get_untracked()
            && current_editor_target(editor_actions, &textarea)
        {
            apply_editor_command(
                editor_actions,
                EditorCommand::ConvertIndentation,
                &textarea,
                action_error,
            );
            let _ = textarea.focus();
        }
    });
    if let Some(ui) = use_context::<crate::state::ui::UiState>() {
        ui.editor_convert.set(Some(convert_indentation));
        Effect::new(move |_| {
            ui.editor_can_convert.set(
                !read_only.get()
                    && pending_diff.get().is_none()
                    && view_mode.get() == ViewMode::Code
                    && open_file.with(|path| {
                        path.as_ref()
                            .is_some_and(|path| !FileKind::from_path(path).is_non_text())
                    })
                    && editor_actions.limit().is_none()
                    && {
                        workspace.editor_composition.track();
                        !editor_actions.is_composing()
                    },
            );
        });
        on_cleanup(move || {
            ui.editor_convert.set(None);
            ui.editor_can_convert.set(false);
        });
    }

    view! {
        <div class="editor" node_ref=root style=move || format!("{}; --editor-tab-width: {}", editor_actions.preferences().font_style(), editor_actions.rules().indentation.tab_width()) on:keydown=move |event: web_sys::KeyboardEvent| {
            if event.is_composing() { return; }
            if (event.ctrl_key() || event.meta_key()) && !event.alt_key() && view_mode.get_untracked() == ViewMode::Code && ta.get_untracked().is_some() && event.key().eq_ignore_ascii_case("g") {
                event.prevent_default(); event.stop_propagation(); open_go.run(());
            } else if (event.ctrl_key() || event.meta_key()) && event.shift_key() && !event.alt_key() && view_mode.get_untracked() == ViewMode::Code && (matches!(event.key().as_str(), "\\" | "|") || event.code() == "Backslash") {
                event.prevent_default(); event.stop_propagation(); jump_bracket.run(());
            } else if event.key() == "Escape" && go_open.get_untracked() {
                event.prevent_default(); event.stop_propagation(); go_open.set(false); if let Some(textarea) = ta.get_untracked() { let _ = textarea.focus(); }
            } else if (event.ctrl_key() || event.meta_key()) && !event.alt_key() && event.key().eq_ignore_ascii_case("f") && view_mode.get_untracked() != ViewMode::Preview {
                event.prevent_default(); event.stop_propagation(); find_open.set(true);
                if let Some(input) = find_input.get_untracked() { let _ = input.focus(); input.select(); }
            } else if ((event.ctrl_key() && event.key().eq_ignore_ascii_case("h")) || (event.meta_key() && event.alt_key() && event.key().eq_ignore_ascii_case("f"))) && view_mode.get_untracked() != ViewMode::Preview {
                event.prevent_default(); event.stop_propagation(); find_open.set(true); replace_open.set(true);
            } else if event.key() == "Escape" && find_open.get_untracked() {
                event.prevent_default(); event.stop_propagation(); find_open.set(false);
                if let Some(textarea) = ta.get_untracked() { let _ = textarea.focus(); }
            }
        }>
            <div class="editor-header">
                <super::editor_tabs::EditorTabs />
                <Show
                    when=move || pending_diff.get().is_some()
                    fallback=move || {
                        let on_load = on_load_git_diff;

                        view! {
                            <div class="editor-header-actions">
                                <Show when=move || open_file.get().is_some()>
                                    <super::dropdown::ActionMenu aria_label="Editor actions">
                                        <h3 class="ui-menu-heading">"File"</h3>
                                        <button role="menuitem" type="button" class="ui-dropdown-item recent-item" disabled=file_actions.is_none() on:click=move |_| { if let Some((actions, path)) = file_actions.zip(open_file.get_untracked()) { actions.copy_path(&path); } }><Icon name=IconName::Copy /><span>"Copy path"</span></button>
                                        <button role="menuitem" type="button" class="ui-dropdown-item recent-item" disabled=file_actions.is_none() on:click=move |_| { if let Some((actions, path)) = file_actions.zip(open_file.get_untracked()) { actions.reveal(&path); } }><Icon name=IconName::FolderOpen /><span>"Reveal in Files"</span></button>

                                        <Show when=move || can_revert.get()><button role="menuitem" type="button" class="ui-dropdown-item recent-item" on:click=move |_| { if let Some(action) = on_discard_git_diff { action.run(()); } }><Icon name=IconName::Undo2 /><span>"Discard changes…"</span></button></Show>
                                        <Show when=move || view_mode.get() == ViewMode::Code && !read_only.get() && open_file.with(|path| path.as_ref().is_some_and(|path| !FileKind::from_path(path).is_non_text()))>

                                        <h3 class="ui-menu-heading">"Indentation"</h3>
                                        <button role="menuitem" type="button" class="ui-dropdown-item recent-item" disabled=read_only on:click=move |_| convert_indentation.run(())><Icon name=IconName::ListIndentIncrease /><span>"Convert indentation"</span></button>

                                        <h3 class="ui-menu-heading">"Navigation"</h3>
                                        <button type="button" role="menuitem" class="ui-dropdown-item recent-item" title="Ctrl/Cmd+G" on:click=move |_| open_go.run(())><Icon name=IconName::MapPin /><span>"Go to line/column"</span></button>
                                        <button type="button" role="menuitem" class="ui-dropdown-item recent-item" title="Ctrl/Cmd+Shift+\\" on:click=move |_| jump_bracket.run(())><Icon name=IconName::Brackets /><span>"Jump to matching bracket"</span></button>

                                        <h3 class="ui-menu-heading">"Selection"</h3>
                                        {[
                                            ("Select next occurrence", openwebide_core::editor::SelectionCommand::NextOccurrence, "Ctrl/Cmd+D", IconName::TextSearch),
                                            ("Select all occurrences", openwebide_core::editor::SelectionCommand::AllOccurrences, "Ctrl/Cmd+Shift+L", IconName::ScanText),
                                            ("Add cursor above", openwebide_core::editor::SelectionCommand::AddAbove, "Ctrl/Cmd+Alt+Up", IconName::ArrowUpToLine),
                                            ("Add cursor below", openwebide_core::editor::SelectionCommand::AddBelow, "Ctrl/Cmd+Alt+Down", IconName::ArrowDownToLine),
                                            ("Expand selection", openwebide_core::editor::SelectionCommand::Expand, "Alt+Shift+Right", IconName::Expand),
                                            ("Shrink selection", openwebide_core::editor::SelectionCommand::Shrink, "Alt+Shift+Left", IconName::Shrink),
                                            ("Keep primary cursor", openwebide_core::editor::SelectionCommand::Single, "Escape", IconName::TextCursor),
                                        ].into_iter().map(move |(label, command, shortcut, icon)| view! {
                                            <button type="button" role="menuitem" class="ui-dropdown-item recent-item" title=shortcut disabled=move || { workspace.editor_composition.track(); editor_actions.is_composing() } on:click=move |_| {
                                                let Some(textarea) = ta.get_untracked().filter(|textarea| current_editor_target(editor_actions, textarea)) else { return; };
                                                let (Some(project), Some(path)) = (workspace.active_project.get_untracked(), open_file.get_untracked()) else { return; };
                                                let source = content.get_untracked();
                                                let selection = projected_selection(editor_actions, &textarea);
                                                if let Err(error) = editor_actions.record_native_selection(selection) { action_error.set(Some(error.to_string())); return; }
                                                match editor_actions.selection_command(project, &path, &source, command) {
                                                    Ok(Some(selections)) => { action_error.set(None); if let Some(selection) = selections.first() { render_editor_selection(editor_actions, &textarea, *selection, false); focus_editor_after_menu(workspace, editor_actions, &textarea); } }
                                                    Err(error) => action_error.set(Some(error.to_string())),
                                                    Ok(None) => {}
                                                }
                                            }><Icon name=icon /><span>{label}</span></button>
                                        }).collect_view()}

                                        <h3 class="ui-menu-heading">"Editing"</h3>
                                        {[
                                            ("Move lines up", EditorCommand::Line(openwebide_core::editor::LineCommand::MoveUp), "Alt+Up", IconName::ArrowUp),
                                            ("Move lines down", EditorCommand::Line(openwebide_core::editor::LineCommand::MoveDown), "Alt+Down", IconName::ArrowDown),
                                            ("Duplicate lines above", EditorCommand::Line(openwebide_core::editor::LineCommand::DuplicateAbove), "Alt+Shift+Up", IconName::CopyPlus),
                                            ("Duplicate lines below", EditorCommand::Line(openwebide_core::editor::LineCommand::Duplicate), "Alt+Shift+Down", IconName::CopyPlus),
                                            ("Duplicate selection", EditorCommand::DuplicateSelection, "Ctrl/Cmd+Shift+D", IconName::Copy),
                                            ("Delete lines", EditorCommand::Line(openwebide_core::editor::LineCommand::Delete), "Ctrl/Cmd+Shift+K", IconName::ListMinus),
                                            ("Insert line above", EditorCommand::Line(openwebide_core::editor::LineCommand::InsertAbove), "Ctrl/Cmd+Shift+Enter", IconName::ArrowUpToLine),
                                            ("Insert line below", EditorCommand::Line(openwebide_core::editor::LineCommand::InsertBelow), "Ctrl/Cmd+Enter", IconName::ArrowDownToLine),
                                            ("Toggle line comment", EditorCommand::LineComment, "Ctrl/Cmd+/", IconName::MessageSquare),
                                            ("Toggle block comment", EditorCommand::BlockComment, "Ctrl/Cmd+Shift+/", IconName::Brackets),
                                            ("Reindent selected lines", EditorCommand::Reindent, "", IconName::ListIndentIncrease),
                                        ].into_iter().map(move |(label, command, shortcut, icon)| view! {
                                            <button type="button" role="menuitem" class="ui-dropdown-item recent-item" title=shortcut disabled=move || read_only.get() || {
                                                let language = openwebide_core::highlight::language_from_path(&open_file.get().unwrap_or_default());
                                                match command {
                                                    EditorCommand::LineComment => openwebide_core::editor::line_comment(language).is_none() && openwebide_core::editor::block_comment(language).is_none(),
                                                    EditorCommand::BlockComment => openwebide_core::editor::block_comment(language).is_none(),
                                                    EditorCommand::Reindent => !openwebide_core::editor::supports_reindent(language),
                                                    _ => false,
                                                }
                                            } on:click=move |_| {
                                                if let Some(textarea) = ta.get() && !read_only.get_untracked() && current_editor_target(editor_actions, &textarea) { apply_editor_command(editor_actions, command, &textarea, action_error); let _ = textarea.focus(); }
                                            }><Icon name=icon /><span>{label}</span></button>
                                        }).collect_view()}
                                        <h3 class="ui-menu-heading">"Folding"</h3>
                                        {[
                                            ("Fold at cursor", openwebide_core::editor::FoldCommand::Collapse { recursive: false }, IconName::FoldVertical),
                                            ("Unfold at cursor", openwebide_core::editor::FoldCommand::Expand { recursive: false }, IconName::UnfoldVertical),
                                            ("Fold recursively", openwebide_core::editor::FoldCommand::Collapse { recursive: true }, IconName::ListCollapse),
                                            ("Unfold recursively", openwebide_core::editor::FoldCommand::Expand { recursive: true }, IconName::ListTree),
                                            ("Fold all", openwebide_core::editor::FoldCommand::CollapseAll, IconName::FoldVertical),
                                            ("Unfold all", openwebide_core::editor::FoldCommand::ExpandAll, IconName::UnfoldVertical),
                                        ].into_iter().map(move |(label, command, icon)| view! {
                                            <button type="button" role="menuitem" class="ui-dropdown-item recent-item" disabled=move || fold_state.with(|state| state.ranges().is_empty()) on:click=move |_| {
                                                if let Some(textarea) = ta.get_untracked() && current_editor_target(editor_actions, &textarea) { apply_fold_command(editor_actions, &textarea, command); }
                                            }><Icon name=icon /><span>{label}</span></button>
                                        }).collect_view()}
                                        </Show>

                                        <Show when=move || panel_actions.is_some()>
                                            <h3 class="ui-menu-heading">"Panel"</h3>
                                            <button role="menuitem" type="button" class="ui-dropdown-item recent-item" on:click=move |_| { if let Some(actions) = panel_actions { actions.move_panel.run((crate::state::layout::Panel::Editor, false)); } }><Icon name=IconName::ArrowLeft /><span>"Move panel left"</span></button>
                                            <button role="menuitem" type="button" class="ui-dropdown-item recent-item" on:click=move |_| { if let Some(actions) = panel_actions { actions.move_panel.run((crate::state::layout::Panel::Editor, true)); } }><Icon name=IconName::ArrowRight /><span>"Move panel right"</span></button>
                                        </Show>
</super::dropdown::ActionMenu>
                                </Show>
                                <Show
                                    when=move || view_mode.get() == ViewMode::InlineDiff || view_mode.get() == ViewMode::SideBySide
                                    fallback={
                                        move || {
                                            let on_load = on_load;
                                            view! {
                                                <super::editor_chrome::EditorViewSelector value=view_mode.read_only().into() preview_supported=Signal::derive(move || !preview_disabled.get()) on_change=Callback::new(move |mode| {
                                                    if (mode == ViewMode::InlineDiff || (mode == ViewMode::Preview && open_file.with(|path| path.as_ref().is_some_and(|path| FileKind::from_path(path) == FileKind::Markdown)))) && let Some(action) = on_load { action.run(()); }
                                                    view_mode.set(mode);
                                                }) />
                                                <Show when=move || file_read_only.get() fallback=|| ()>
                                                    <span class="form-hint" style="margin-left: 12px; align-self: center;">
                                                        "Not valid UTF-8 — shown read-only"
                                                    </span>
                                                </Show>
                                                <IconButton label="Save file (Ctrl/⌘S)" disabled=Signal::derive(move || read_only.get() || !dirty.get() || recovery_blocks_save.get()) on_click=Callback::new(move |_| on_save.run(()))><Icon name=IconName::Save /></IconButton>
                                            }
                                        }
                                    }
                                >
                                    <super::editor_chrome::EditorViewSelector value=view_mode.read_only().into() preview_supported=Signal::derive(move || !preview_disabled.get()) on_change=Callback::new(move |mode| view_mode.set(mode)) />
                                    <span class="changes-baseline" title="Compared with HEAD">"Against last commit"</span>
                                    <SegmentedControl class="changes-display" options=vec![SegmentOption::new("Inline", ViewMode::InlineDiff), SegmentOption::new("Split", ViewMode::SideBySide)] value=view_mode.read_only().into() on_change=Callback::new(move |mode| view_mode.set(mode)) />

                                    <IconButton label="Save file (Ctrl/⌘S)" disabled=Signal::derive(move || read_only.get() || !dirty.get() || recovery_blocks_save.get()) on_click=Callback::new(move |_| on_save.run(()))><Icon name=IconName::Save /></IconButton>
                                </Show>
                            </div>
                        }
                    }
                >
                    <div class="editor-diff-actions">
                        <Show when=move || {
                            if let Some(d) = pending_diff.get() {
                                d.old.is_none() || d.old_unavailable
                            } else { false }
                        }>
                            <span style="color: var(--warn); margin-right: 12px; font-size: 0.9em; flex: 1;">
                                {move || {
                                    if let Some(d) = pending_diff.get() {
                                        if d.old_unavailable {
                                            "Warning: Unreadable file was modified. The old contents were not sent to the agent and could not be included in this preview. Rejecting will restore the previous contents."
                                        } else {
                                            "New file created."
                                        }
                                    } else { "" }
                                }}
                            </span>
                        </Show>
                        <SegmentedControl
                            options=vec![
                                SegmentOption::new("Inline", ViewMode::InlineDiff),
                                SegmentOption::new("Split", ViewMode::SideBySide),
                                SegmentOption::new("Preview", ViewMode::Preview).disabled_when(preview_disabled),
                            ]
                            value=view_mode.read_only().into()
                            on_change=Callback::new(move |mode| view_mode.set(mode))
                        />
                        <Button
                            variant=ButtonVariant::Success
                            size=ButtonSize::Sm
                            disabled=Signal::derive(move || workspace.is_resolving())
                            on_click=Callback::new(move |_| on_accept.run(()))
                        >
                            <super::ui::Icon name=super::ui::IconName::Check />"Accept"
                        </Button>
                        <Button
                            variant=ButtonVariant::Danger
                            size=ButtonSize::Sm
                            disabled=Signal::derive(move || workspace.is_resolving())
                            on_click=Callback::new(move |_| on_reject.run(()))
                        >
                            <super::ui::Icon name=super::ui::IconName::X />"Reject"
                        </Button>
                    </div>
                </Show>
                <IconButton label="Find in file (Ctrl/⌘F)" disabled=Signal::derive(move || open_file.get().is_none() || editor_actions.limit().is_some() || view_mode.get() == ViewMode::Preview) on_click=Callback::new(move |_| find_open.set(!find_open.get_untracked()))><Icon name=IconName::Search /></IconButton>
                <super::tool_panel::PanelMinimize panel=crate::state::layout::Panel::Editor />
            </div>
            <Show when=move || go_open.get() && open_file.get().is_some() && view_mode.get() == ViewMode::Code>
                <PanelSearchRow class="editor-navigation">
                    <input class="form-input panel-search-input" node_ref=go_input aria-label="Go to line and column" placeholder="line:column" prop:value=move || go_query.get() on:input=move |event| go_query.set(event_target_value(&event)) on:keydown=move |event: web_sys::KeyboardEvent| { if event.key() == "Enter" { event.prevent_default(); go.run(()); } } />
                    <span class="form-hint" aria-live="polite">{move || if go_target.get().is_none() { "Use line or line:column" } else { "" }}</span>
                    <Button size=ButtonSize::Sm disabled=Signal::derive(move || go_target.get().is_none()) on_click=Callback::new(move |_| go.run(()))>"Go"</Button>
                    <IconButton label="Close navigation" on_click=Callback::new(move |_| go_open.set(false))><Icon name=IconName::X /></IconButton>
                </PanelSearchRow>
            </Show>
            <Show when=move || find_open.get() && open_file.get().is_some() && view_mode.get() != ViewMode::Preview>
                <PanelSearchRow class="editor-find">
                    <input class="form-input panel-search-input" type="search" node_ref=find_input placeholder="Find in file" aria-label="Find in file" title="Find text or a regular expression" prop:value=move || query.get() on:input=move |event| query.set(event_target_value(&event)) on:keydown=move |event: web_sys::KeyboardEvent| {
                        if event.key() == "Enter" { event.prevent_default(); navigate.run(!event.shift_key()); }
                    } />
                    <span class="form-hint" aria-live="polite">{move || { let count = matches.get().len(); format!("{} / {count}", if count == 0 { 0 } else { match_index.get() % count + 1 }) }}</span>
                    <IconButton label="Previous match" disabled=Signal::derive(move || matches.get().is_empty()) on_click=Callback::new(move |_| navigate.run(false))><Icon name=IconName::ChevronUp /></IconButton>
                    <IconButton label="Next match" disabled=Signal::derive(move || matches.get().is_empty()) on_click=Callback::new(move |_| navigate.run(true))><Icon name=IconName::ChevronDown /></IconButton>
                    <Button size=ButtonSize::Sm variant=ButtonVariant::Ghost on_click=Callback::new(move |_| replace_open.update(|open| *open = !*open))>"Replace"</Button>
                    <IconButton label="Close find" on_click=Callback::new(move |_| find_open.set(false))><Icon name=IconName::X /></IconButton>
                </PanelSearchRow>
                <PanelSearchRow class="editor-find-options">
                    <CheckboxField label="Match case" checked=Signal::derive(move || search_options.get().case_sensitive) on_change=Callback::new(move |checked| search_options.update(|options| options.case_sensitive = checked)) />
                    <CheckboxField label="Whole word" checked=Signal::derive(move || search_options.get().whole_word) on_change=Callback::new(move |checked| search_options.update(|options| options.whole_word = checked)) />
                    <CheckboxField label="Regex" checked=Signal::derive(move || search_options.get().regex) on_change=Callback::new(move |checked| search_options.update(|options| options.regex = checked)) />
                    <CheckboxField label="In selection" checked=Signal::derive(move || search_scope.get().is_some()) disabled=Signal::derive(move || search_scope.get().is_none() && scope_candidate.with(|candidate| candidate.as_ref().is_none_or(|(snapshot, _)| snapshot != &content.get())) || view_mode.get() != ViewMode::Code || pending_diff.get().is_some()) on_change=Callback::new(move |checked| search_scope.set(if checked { scope_candidate.get_untracked() } else { None })) />
                </PanelSearchRow>
                <Show when=move || replace_open.get()>
                    <PanelSearchRow class="editor-replace">
                        <input class="form-input panel-search-input" aria-label="Replace with" placeholder="Replace with" prop:value=move || replacement_text.get() on:input=move |event| replacement_text.set(event_target_value(&event)) on:keydown=move |event: web_sys::KeyboardEvent| { if event.key() == "Enter" { event.prevent_default(); replace.run(event.ctrl_key() || event.meta_key()); } } />
                        <Button size=ButtonSize::Sm disabled=Signal::derive(move || read_only.get() || view_mode.get() != ViewMode::Code || pending_diff.get().is_some() || matches.get().is_empty()) on_click=Callback::new(move |_| replace.run(false))>"Replace next"</Button>
                        <Button size=ButtonSize::Sm disabled=Signal::derive(move || read_only.get() || view_mode.get() != ViewMode::Code || pending_diff.get().is_some() || matches.get().is_empty()) on_click=Callback::new(move |_| replace.run(true))>"Replace all"</Button>
                    </PanelSearchRow>
                </Show>
                <Show when=move || search_result.with(Result::is_err) || replacement_error.get().is_some()>
                    <p class="form-error" role="alert">{move || search_result.with(|result| result.as_ref().err().map(ToString::to_string)).or_else(|| replacement_error.get()).unwrap_or_default()}</p>
                </Show>
            </Show>
            <super::editor_recovery::RecoveryStatus />
            <Show when=move || action_error.get().is_some()>
                <p class="editor-error" role="alert">{move || action_error.get().unwrap_or_default()}</p>
            </Show>
            <Show
                when=move || open_file.get().is_some()
                fallback=move || {
                    view! {
                        <p class="empty editor-empty">"Select a file to edit, or create a new one in the explorer."</p>
                    }
                }
            >
                {move || {
                    let editor_project = workspace.active_project.get();
                    let editor_account_generation = editor_actions.account_generation();
                    let mode = view_mode.get();
                    if let Some(limit) = editor_actions.limit() {
                        let old = pending_diff.with(|diff| diff.as_ref().and_then(|diff| diff.old.clone())).or_else(|| {
                            if matches!(mode, ViewMode::InlineDiff | ViewMode::SideBySide | ViewMode::Preview) { git_head_diff.with(|diff| diff.as_ref().and_then(|diff| diff.old.clone())) } else { None }
                        });
                        return view! { <LargeTextViewer content=content limit=limit old=old /> }.into_any();
                    }
                    if let Some(diff) = pending_diff.get() {
                        if let Some(limit) = openwebide_core::editor::editor_limit(&diff.new).or_else(|| diff.old.as_deref().and_then(openwebide_core::editor::editor_limit)) {
                            let source = RwSignal::new(crate::state::workspace::EditorText::from(diff.new));
                            return view! { <LargeTextViewer content=source.read_only() limit=limit old=diff.old /> }.into_any();
                        }
                        let metadata = workspace.open_file.get().and_then(|path| workspace.persisted_edits.with(|edits| edits.get(&path).and_then(|edit| edit.file.clone())));
                        if metadata.as_ref().is_some_and(|file| file.deleted) {
                            view! { <p class="empty editor-empty">"File deleted. Reject the file changes to restore it."</p> }.into_any()
                        } else if metadata.as_ref().is_some_and(|file| file.binary_after.is_some()) {
                            view! { <p class="empty editor-empty">"Binary contents changed. Use Accept or Reject to review this file."</p> }.into_any()
                        } else { match mode {
                            ViewMode::InlineDiff => render_inline_diff(diff).into_any(),
                            ViewMode::SideBySide => render_side_by_side(diff).into_any(),
                            ViewMode::Preview => {
                                let path = open_file.get().unwrap_or_default();
                                if FileKind::from_path(&path) == FileKind::Markdown {
                                    if let Some(limit) = diff.old.as_deref().and_then(openwebide_core::editor::editor_limit) {
                                        return view! { <LargeTextViewer content=content limit=limit old=diff.old /> }.into_any();
                                    }
                                    return render_markdown_diff(&diff).into_any();
                                }
                                render_preview_view(
                                    &path,
                                    &diff.new,
                                    media_url.get(),
                                    view_mode.write_only(),
                                    on_open_lossy,
                                )
                                .into_any()
                            }
                            ViewMode::Code => render_inline_diff(diff).into_any(),
                        }
                        }
                    } else if (mode == ViewMode::InlineDiff || mode == ViewMode::SideBySide) && binary_head.get() {
                        view! { <p class="empty editor-empty">"Binary file — no text diff"</p> }.into_any()
                    } else if (mode == ViewMode::InlineDiff || mode == ViewMode::SideBySide) && git_head_diff.get().is_some() {
                        let diff = git_head_diff.get().unwrap();
                        if let Some(limit) = diff.old.as_deref().and_then(openwebide_core::editor::editor_limit) {
                            return view! { <LargeTextViewer content=content limit=limit old=diff.old /> }.into_any();
                        }
                        match mode {
                            ViewMode::InlineDiff => render_inline_diff(diff).into_any(),
                            ViewMode::SideBySide => render_side_by_side(diff).into_any(),
                            _ => ().into_any(),
                        }
                    } else {
                        match mode {
                            ViewMode::Preview => {
                                let path = open_file.get().unwrap_or_default();
                                if FileKind::from_path(&path) == FileKind::Markdown && let Some(diff) = git_head_diff.get() {
                                    if let Some(limit) = diff.old.as_deref().and_then(openwebide_core::editor::editor_limit) {
                                        return view! { <LargeTextViewer content=content limit=limit old=diff.old /> }.into_any();
                                    }
                                    return render_markdown_diff(&diff).into_any();
                                }
                                let text = content.get();
                                render_preview_view(
                                    &path,
                                    &text,
                                    media_url.get(),
                                    view_mode.write_only(),
                                    on_open_lossy,
                                )
                                .into_any()
                            }
                            _ if open_file.with(|path| path.as_ref().is_some_and(|path| FileKind::from_path(path).is_non_text())) && !read_only.get() => {
                                let path = open_file.get().unwrap_or_default();
                                render_placeholder_view(&path, FileKind::from_path(&path), view_mode.write_only(), on_open_lossy).into_any()
                            }
                            _ => {
                                view! {
                                    <div class="editor-code" data-editor-view=move || editor_actions.view_revision().to_string() data-editor-account=editor_account_generation.to_string() data-editor-pointer-ready=move || highlight_ready.get().to_string() style=move || format!("--editor-gutter-width: calc({}ch + 42px); --editor-tab-width: {}", source_line_count.get().to_string().len(), editor_actions.rules().indentation.tab_width()) class:highlight-ready=move || highlight_visible.get() class:editor-word-wrap=move || editor_actions.preferences().word_wrap class:editor-uniform-rows=move || projection.with(openwebide_core::editor::FoldProjection::has_uniform_rows) on:editor-scroll-intent=move |event: web_sys::Event| {
                                                let Some(input) = ta.get_untracked().filter(|input| current_editor_target(editor_actions, input)) else { return; };
                                                let Some(event) = event.dyn_ref::<web_sys::CustomEvent>() else { return; };
                                                let detail = event.detail();
                                                let number = |name: &str| js_sys::Reflect::get(&detail, &wasm_bindgen::JsValue::from_str(name)).ok().and_then(|value| value.as_f64());
                                                let (Some(x), Some(y)) = (number("x"), number("y")) else { return; };
                                                let scroll = editor_actions.scroll();
                                                editor_actions.request_scroll(editor_project.unwrap_or_default(), &input.get_attribute("data-editor-path").unwrap_or_default(), editor_actions.view_revision(), editor_actions.account_generation(), scroll.top + y, scroll.left + x);
                                            }>
                                        <div class="editor-scroll-surface" aria-hidden="true" on:scroll=move |event: web_sys::Event| {
                                            let Some(textarea) = ta.get_untracked() else { return; };
                                            let Some(target) = event.current_target().and_then(|target| target.dyn_into::<web_sys::HtmlElement>().ok()) else { return; };
                                            if !target.is_connected() || target != crate::viewport::editor_scroll(&textarea)
                                                || editor_actions.account_generation() != editor_account_generation
                                                || !current_editor_target(editor_actions, &textarea) { return; }
                                            crate::viewport::sync_editor_scroll(&textarea, false);
                                            let _ = editor_actions.record_native_selection(projected_selection(editor_actions, &textarea));
                                            let scroll = crate::viewport::editor_scroll(&textarea);
                                            if extent_owner.get_untracked().is_some() {
                                                editor_actions.record_source_scroll(editor_project.unwrap_or_default(), &textarea.get_attribute("data-editor-path").unwrap_or_default(), scroll.scroll_top(), scroll.scroll_left(), source_extent.get_untracked().is_some());
                                            } else {
                                                editor_actions.record_scroll(editor_project.unwrap_or_default(), &textarea.get_attribute("data-editor-path").unwrap_or_default(), scroll.scroll_top(), scroll.scroll_left());
                                            }
                                            if let Some(overlay) = hl.get_untracked() { sync_highlight_scroll(&textarea, &overlay); }
                                            layout_revision.update(|revision| *revision = revision.wrapping_add(1));
                                        }><div class="editor-scroll-extent"
                                            data-editor-scope=move || extent_owner.get().map(|(scope, _, _)| scope.to_string())
                                            data-editor-view=move || extent_owner.get().map(|(_, view, _)| view.to_string())
                                            data-editor-account=move || extent_owner.get().map(|(_, _, account)| account.to_string())
                                            data-source-width=move || source_extent.get().map(|(_, _, _, extent)| extent.width.to_string())
                                            data-source-height=move || source_extent.get().map(|(_, _, _, extent)| extent.height).or_else(|| source_height.get().map(|(_, _, _, height)| height)).map(|height| height.to_string()) /></div>
                                        <HighlightOverlay actions=editor_actions paint_request=paint_request paint_epoch=paint_epoch content=content open_file=open_file node_ref=hl textarea_ref=ta ready=highlight_ready presentation=highlight_visible error=action_error visible=visible_rows viewport=viewport textarea_start=textarea_start indentation=Signal::from(paint_indentation) show_whitespace=Signal::from(paint_whitespace) layout_revision=layout_revision native_install_revision=native_install_revision />
                                        <super::editor_selections::SelectionOverlay textarea=ta ready=highlight_ready layout_revision=layout_revision />
                                        <div class="editor-bracket-layer" aria-hidden="true">{move || bracket_marks.get().into_iter().map(|(left, top, width, height)| view! { <span class="editor-bracket-match" style=format!("left:{left}px;top:{top}px;width:{width}px;height:{height}px")/> }).collect_view()}</div>
                                        <div class="editor-fold-column"><div class="editor-fold-track" style=move || format!("padding-top:{}px", viewport.get().top)>{move || {
                                            let state = fold_state.get();
                                            let headers: std::collections::HashSet<_> = state.indicator_headers().collect();
                                            let project = workspace.active_project.get_untracked();
                                            let path = open_file.get_untracked();
                                            visible_rows.get().into_iter().map(|header| {
                                                let control = headers.contains(&header);
                                                let collapsed = state.indicator_collapsed(header);
                                                let path = path.clone();
                                                view! { <div class="editor-fold-row">{control.then(move || view! {
                                                    <IconButton class="editor-fold-control" disabled=Signal::derive(move || !fold_state.with(|state| state.ranges().iter().any(|range| range.start_line == header))) label=format!("{} block at line {}", if collapsed { "Expand" } else { "Collapse" }, header + 1) on_click=Callback::new(move |event: web_sys::MouseEvent| {
                                                        if event.current_target().and_then(|target| target.dyn_into::<web_sys::Element>().ok()).is_none_or(|node| !node.is_connected()) { return; }
                                                        if let (Some(project), Some(path), Some(textarea)) = (project, path.as_ref(), ta.get_untracked())
                                                            && editor_actions.is_current(project, path) && current_editor_target(editor_actions, &textarea)
                                                        { apply_fold_command(editor_actions, &textarea, openwebide_core::editor::FoldCommand::Toggle(header)); }
                                                    })><Icon name=if collapsed { IconName::ChevronRight } else { IconName::ChevronDown } /></IconButton>
                                                })}</div> }
                                            }).collect_view()
                                        }}</div></div>
                                        <textarea
                                            {..view! { <{..}
                                                aria-label=move || open_file.get().map_or_else(|| "Code editor".into(), |path| format!("Code editor: {path}"))
                                                aria-description=move || if read_only.get() || tab_moves_focus.get() { "Tab and Shift+Tab move keyboard focus. Ctrl+M toggles Tab navigation." } else { "Tab indents and Shift+Tab outdents. Press Ctrl+M to let Tab move keyboard focus." }
                                                title=move || if read_only.get() { "Read-only editor; Tab moves focus" } else if tab_moves_focus.get() { "Tab moves focus; Ctrl+M restores indentation" } else { "Tab indents; Ctrl+M enables focus navigation" }
                                            /> }}
                                            data-editor-project=editor_project.map(|project| project.to_string())
                                            data-editor-path=open_file.get()
                                            data-editor-scope=move || editor_actions.projection_revision().to_string()
                                            class="editor-textarea"
                                            wrap=move || if editor_actions.preferences().word_wrap { "soft" } else { "off" }
                                            spellcheck="false"
                                            readonly=read_only
                                            node_ref=ta
                                            on:select=move |event: web_sys::Event| {
                                                if let Some(textarea) = event.target().and_then(|target| target.dyn_into::<web_sys::HtmlTextAreaElement>().ok()) && current_editor_target(editor_actions, &textarea) { let _ = editor_actions.record_native_selection(projected_selection(editor_actions, &textarea)); }
                                            }
                                            on:blur=move |event: web_sys::FocusEvent| {
                                                paste_matches_indentation.set(false);
                                                if let Some(textarea) = event.target().and_then(|target| target.dyn_into::<web_sys::HtmlTextAreaElement>().ok()) && current_editor_target(editor_actions, &textarea) { let _ = editor_actions.record_native_selection(projected_selection(editor_actions, &textarea)); }
                                            }
                                            on:keyup=move |event: web_sys::KeyboardEvent| { if event.key().eq_ignore_ascii_case("v") { paste_matches_indentation.set(false); }
                                                if let Some(textarea) = event.target().and_then(|target| target.dyn_into::<web_sys::HtmlTextAreaElement>().ok()) && current_editor_target(editor_actions, &textarea) { let _ = editor_actions.record_native_selection(projected_selection(editor_actions, &textarea)); } }
                                            on:click=move |event: web_sys::MouseEvent| { if !event.alt_key() && let Some(textarea) = event.target().and_then(|target| target.dyn_into::<web_sys::HtmlTextAreaElement>().ok()) && current_editor_target(editor_actions, &textarea) { let _ = editor_actions.record_selection(projected_selection(editor_actions, &textarea)); } }
                                            on:mousedown=move |event: web_sys::MouseEvent| {
                                                column_anchor.set_value(None);
                                                pointer_adapter.start(&event);
                                                if !event.alt_key() { return; }
                                                if !event.alt_key() || event.button() != 0 || editor_actions.is_composing() { return; }
                                                let Some(textarea) = event.target().and_then(|target| target.dyn_into::<web_sys::HtmlTextAreaElement>().ok()).filter(|textarea| current_editor_target(editor_actions, textarea)) else { return; };
                                                event.prevent_default();
                                                if !highlight_ready.get_untracked() { return; }
                                                let Some(offset) = crate::viewport::editor_caret_from_point(&textarea, event.client_x(), event.client_y()) else { return; };
                                                let Some(projection) = editor_actions.projection() else { return; };
                                                let Ok(offset) = projection.source_offset(projection.textarea_to_byte(offset as usize)) else { return; };
                                                let selection = projected_selection(editor_actions, &textarea);
                                                if let Err(error) = editor_actions.record_native_selection(selection) { action_error.set(Some(error.to_string())); return; }
                                                let (Some(project), Some(path)) = (workspace.active_project.get_untracked(), open_file.get_untracked()) else { return; };
                                                let result = if event.shift_key() {
                                                    editor_actions.begin_current_column_selection(project, &path, selection.anchor, offset)
                                                        .map(|result| result.map(|(gesture, selections)| {
                                                            column_anchor.set_value(Some(gesture)); selections
                                                        }))
                                                } else { editor_actions.toggle_current_cursor(project, &path, offset) };
                                                match result {
                                                    Ok(Some(selections)) => { action_error.set(None); if let Some(selection) = selections.first() { render_editor_selection(editor_actions, &textarea, *selection, false); let focus = web_sys::FocusOptions::new(); focus.set_prevent_scroll(true); let _ = textarea.focus_with_options(&focus); } }
                                                    Err(error) => action_error.set(Some(error.to_string())),
                                                    Ok(None) => {}
                                                }
                                            }
                                            on:mousemove=move |event: web_sys::MouseEvent| {
                                                if event.buttons() & 1 == 0 { column_anchor.set_value(None); return; }
                                                let Some(textarea) = event.target().and_then(|target| target.dyn_into::<web_sys::HtmlTextAreaElement>().ok()).filter(|textarea| current_editor_target(editor_actions, textarea)) else { return; };
                                                column_anchor.with_value(|owner| {
                                                    let Some(gesture) = owner else { return; };
                                                    let Some(offset) = crate::viewport::editor_caret_from_point(&textarea, event.client_x(), event.client_y()) else { return; };
                                                    let Some(projection) = editor_actions.projection() else { return; };
                                                    let Ok(offset) = projection.source_offset(projection.textarea_to_byte(offset as usize)) else { return; };
                                                    event.prevent_default();
                                                    match editor_actions.drag_current_column_selection(gesture, offset) {
                                                        Ok(Some(selections)) => { action_error.set(None); if let Some(selection) = selections.first() { render_editor_selection(editor_actions, &textarea, *selection, false); } }
                                                        Err(error) => action_error.set(Some(error.to_string())),
                                                        Ok(None) => {}
                                                    }
                                                });
                                            }
                                            on:mouseup=move |_| column_anchor.set_value(None)
                                            on:paste=move |event: web_sys::ClipboardEvent| {
                                                if let Some(textarea) = event.target().and_then(|target| target.dyn_into::<web_sys::HtmlTextAreaElement>().ok()).filter(|textarea| current_editor_target(editor_actions, textarea)) { if !motion_adapter.flush(&textarea) { event.prevent_default(); return; } prepare_editor_edit(editor_actions, &textarea); }
                                                let matching = paste_matches_indentation.get_untracked(); paste_matches_indentation.set(false);
                                                if read_only.get_untracked() { return; }
                                                let multiple = editor_actions.selection_count() > 1;
                                                if !matching && !multiple { return; }
                                                let Some(textarea) = event.target().and_then(|target| target.dyn_into::<web_sys::HtmlTextAreaElement>().ok()).filter(|textarea| current_editor_target(editor_actions, textarea)) else { return; };
                                                let Some(clipboard) = event.clipboard_data() else { return; };
                                                let Ok(pasted) = clipboard.get_data("text/plain") else { return; };
                                                if pasted.is_empty() { return; }
                                                event.prevent_default();
                                                let selection = projected_selection(editor_actions, &textarea);
                                                let metadata = clipboard.get_data(openwebide_core::editor::CLIPBOARD_SELECTIONS_MIME).ok();
                                                let result = if matching { editor_actions.paste_clipboard_with_indentation(&pasted, metadata.as_deref(), selection) } else { editor_actions.paste_clipboard(&pasted, metadata.as_deref(), selection) };
                                                match result {
                                                    Ok(Some(selection)) => { action_error.set(None); refresh_editor_folds(editor_actions); render_editor_selection(editor_actions, &textarea, selection, false); }
                                                    Err(error) => action_error.set(Some(error.to_string())),
                                                    Ok(None) => {}
                                                }
                                            }
                                            on:copy=move |event: web_sys::ClipboardEvent| {
                                                if let Some(textarea) = event.target().and_then(|target| target.dyn_into::<web_sys::HtmlTextAreaElement>().ok()).filter(|textarea| current_editor_target(editor_actions, textarea))
                                                { if !motion_adapter.flush(&textarea) { event.prevent_default(); return; }
                                                    editor_clipboard_copy(editor_actions, &textarea, &event, false, read_only.get_untracked(), action_error);
                                                }
                                            }
                                            on:cut=move |event: web_sys::ClipboardEvent| {
                                                if let Some(textarea) = event.target().and_then(|target| target.dyn_into::<web_sys::HtmlTextAreaElement>().ok()).filter(|textarea| current_editor_target(editor_actions, textarea)) { if !motion_adapter.flush(&textarea) { event.prevent_default(); return; } editor_clipboard_copy(editor_actions, &textarea, &event, true, read_only.get_untracked(), action_error); }
                                            }
                                            on:beforeinput=move |event: web_sys::InputEvent| {
                                                if let Some(textarea) = event.target().and_then(|target| target.dyn_into::<web_sys::HtmlTextAreaElement>().ok()) && current_editor_target(editor_actions, &textarea) {
                                                    editor_actions.cancel_native_text();
                                                    if !motion_adapter.flush(&textarea) { if event.cancelable() { event.prevent_default(); } return; }
                                                    if event.is_composing() && !editor_actions.is_composing() { if event.cancelable() { event.prevent_default(); } return; }
                                                    if !read_only.get_untracked() { prepare_editor_edit(editor_actions, &textarea); }
                                                    let selection = projected_selection(editor_actions, &textarea);
                                                    let _ = editor_actions.record_native_selection(selection);
                                                    if read_only.get_untracked() || event.is_composing() || !event.cancelable() { return; }
                                                    let command = match event.input_type().as_str() {
                                                        "insertLineBreak" | "insertParagraph" => Some(EditorCommand::Newline),
                                                        "deleteContentBackward" => Some(EditorCommand::DeletePair),
                                                        "insertText" => event.data().and_then(|text| pair_character(&text)).map(EditorCommand::TypeCharacter),
                                                        _ => None,
                                                    };
                                                    if let Some(command) = command && apply_editor_command(editor_actions, command, &textarea, action_error) { event.prevent_default(); return; }
                                                    if event.input_type() == "insertText" && !editor_actions.is_composing() && let Some(text) = event.data() {
                                                        editor_actions.begin_native_text(text);
                                                    }
                                                }
                                            }
                                            on:compositionstart=move |event: web_sys::CompositionEvent| {
                                                if let Some(textarea) = event.target().and_then(|target| target.dyn_into::<web_sys::HtmlTextAreaElement>().ok()) && current_editor_target(editor_actions, &textarea) {
                                                    if !motion_adapter.flush(&textarea) { editor_actions.cancel_queued_motion(None); }
                                                    prepare_editor_edit(editor_actions, &textarea);
                                                    let _ = editor_actions.record_native_selection(projected_selection(editor_actions, &textarea)); editor_actions.begin_composition();
                                                }
                                            }
                                            on:compositionend=move |event: web_sys::CompositionEvent| {
                                                if let Some(textarea) = event.target().and_then(|target| target.dyn_into::<web_sys::HtmlTextAreaElement>().ok()) && current_editor_target(editor_actions, &textarea) {
                                                    let installed = editor_actions.native_input_current(
                                                        textarea.get_attribute("data-editor-native-bound").as_deref() == Some("true"),
                                                        textarea.get_attribute("data-editor-native-generation").as_deref(),
                                                    );
                                                    if installed && editor_actions.is_composing() { let _ = editor_actions.projected_input(textarea.value(), editor_selection(&textarea), "insertCompositionText", event.time_stamp()); }
                                                    let _ = editor_actions.end_composition(); refresh_editor_folds(editor_actions);
                                                    if let Some(selection) = editor_actions.current_selection() { render_editor_selection(editor_actions, &textarea, selection, false); }
                                                }
                                            }
                                            on:keydown=move |event: web_sys::KeyboardEvent| {
                                                if event.is_composing() { return; }
                                                editor_actions.cancel_native_text();
                                                let Some(textarea) = event.target().and_then(|target| target.dyn_into::<web_sys::HtmlTextAreaElement>().ok()).filter(|textarea| current_editor_target(editor_actions, textarea)) else { return; };
                                                if editor_selection_key(editor_actions, &textarea, &event, action_error, motion_adapter) { event.prevent_default(); event.stop_propagation(); return; }
                                                if !motion_adapter.flush(&textarea) { event.prevent_default(); event.stop_propagation(); return; }
                                                let modified = event.ctrl_key() || event.meta_key();
                                                paste_matches_indentation.set(false);
                                                if modified && event.alt_key() && (matches!(event.key().as_str(), "[" | "]") || matches!(event.code().as_str(), "BracketLeft" | "BracketRight")) {
                                                    event.prevent_default(); event.stop_propagation();
                                                    let command = if event.key() == "[" || event.code() == "BracketLeft" { openwebide_core::editor::FoldCommand::Collapse { recursive: event.shift_key() } } else { openwebide_core::editor::FoldCommand::Expand { recursive: event.shift_key() } };
                                                    apply_fold_command(editor_actions, &textarea, command); return;
                                                }
                                                if modified && event.shift_key() && !event.alt_key() && event.key().eq_ignore_ascii_case("v") && !read_only.get_untracked() { paste_matches_indentation.set(true); return; }

                                                if modified && event.key().eq_ignore_ascii_case("m") {
                                                    event.prevent_default(); event.stop_propagation(); tab_moves_focus.update(|value| *value = !*value);
                                                    tab_focus_announcement.set(if read_only.get_untracked() { "Read-only editor. Tab moves keyboard focus." } else if tab_moves_focus.get_untracked() { "Tab now moves keyboard focus." } else { "Tab now indents. Press Ctrl+M to move keyboard focus." }); return;
                                                }
                                                if read_only.get_untracked() { return; }
                                                let command = match event.key().as_str() {
                                                    "Tab" if !tab_moves_focus.get_untracked() && !modified && !event.alt_key() => Some(if event.shift_key() { EditorCommand::Outdent } else { EditorCommand::Tab }),
                                                    "ArrowUp" if event.alt_key() && !modified => Some(EditorCommand::Line(if event.shift_key() { openwebide_core::editor::LineCommand::DuplicateAbove } else { openwebide_core::editor::LineCommand::MoveUp })),
                                                    "ArrowDown" if event.alt_key() && !modified => Some(EditorCommand::Line(if event.shift_key() { openwebide_core::editor::LineCommand::Duplicate } else { openwebide_core::editor::LineCommand::MoveDown })),
                                                    "Enter" if modified && !event.alt_key() => Some(EditorCommand::Line(if event.shift_key() { openwebide_core::editor::LineCommand::InsertAbove } else { openwebide_core::editor::LineCommand::InsertBelow })),
                                                    "Enter" if !modified && !event.alt_key() => Some(EditorCommand::Newline),
                                                    "/" | "?" if modified && !event.alt_key() => Some(if event.shift_key() { EditorCommand::BlockComment } else { EditorCommand::LineComment }),
                                                    "K" | "k" if modified && event.shift_key() && !event.alt_key() => Some(EditorCommand::Line(openwebide_core::editor::LineCommand::Delete)),
                                                    "D" | "d" if modified && event.shift_key() && !event.alt_key() => Some(EditorCommand::DuplicateSelection),
                                                    key if modified && key.eq_ignore_ascii_case("z") => Some(if event.shift_key() { EditorCommand::Redo } else { EditorCommand::Undo }),
                                                    key if modified && key.eq_ignore_ascii_case("y") => Some(EditorCommand::Redo),
                                                    "Backspace" if !modified && !event.alt_key() => Some(EditorCommand::DeletePair),
                                                    key if !modified && !event.alt_key() => pair_character(key).map(EditorCommand::TypeCharacter),
                                                    _ => None,
                                                };
                                                if let Some(command) = command && apply_editor_command(editor_actions, command, &textarea, action_error) {
                                                    event.prevent_default(); event.stop_propagation();
                                                }
                                            }
                                            on:input=move |e: web_sys::Event| {
                                                if let Some(target) = e.target()
                                                    && let Some(textarea) = target.dyn_ref::<web_sys::HtmlTextAreaElement>()
                                                    && current_editor_target(editor_actions, textarea)
                                                {
                                                    if read_only.get_untracked() { editor_actions.cancel_native_text(); return; }
                                                    if !editor_actions.native_input_current(
                                                        textarea.get_attribute("data-editor-native-bound").as_deref() == Some("true"),
                                                        textarea.get_attribute("data-editor-native-generation").as_deref(),
                                                    ) {
                                                        editor_actions.cancel_native_text();
                                                        action_error.set(Some(openwebide_core::editor::EditError::StaleContext.to_string()));
                                                        if let Some(selection) = content.with_untracked(|source| editor_actions.selection(source)) {
                                                            render_editor_selection(editor_actions, textarea, selection, false);
                                                        }
                                                        return;
                                                    }
                                                    let input_type = e.dyn_ref::<web_sys::InputEvent>().map_or_else(String::new, |event| if event.is_composing() { "insertCompositionText".to_string() } else { event.input_type() });
                                                    let mut retain_native_value = false;
                                                    let result = if input_type == "insertText" {
                                                        let data = e.dyn_ref::<web_sys::InputEvent>().and_then(web_sys::InputEvent::data);
                                                        match editor_actions.finish_native_text(data.as_deref(), native_selection_units(textarea), e.time_stamp()) {
                                                            Ok(Some(commit)) => { retain_native_value = e.is_trusted() && commit.retain_native_value; Ok(()) },
                                                            Ok(None) => editor_actions.projected_input(textarea.value(), editor_selection(textarea), &input_type, e.time_stamp()),
                                                            Err(error) => Err(error),
                                                        }
                                                    } else { editor_actions.cancel_native_text(); editor_actions.projected_input(textarea.value(), editor_selection(textarea), &input_type, e.time_stamp()) };
                                                    action_error.set(result.as_ref().err().map(ToString::to_string));
                                                    refresh_editor_folds(editor_actions);
                                                    // A trusted single-cursor commit already proves the browser's
                                                    // native value. Fold changes still require full reconciliation.
                                                    let retain_native_value = retain_native_value && editor_actions.projection().is_some_and(|projection| !projection.is_folded());
                                                    if !retain_native_value { content.with_untracked(|source| { if let Some(selection) = editor_actions.selection(source) { render_editor_selection(editor_actions, textarea, selection, result.is_ok()); } }); }
                                                }
                                            }
                                            on:scroll=move |event: web_sys::Event| {
                                                if let Some(textarea) = event.target().and_then(|target| target.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
                                                    && editor_actions.account_generation() == editor_account_generation
                                                    && current_editor_target(editor_actions, &textarea)
                                                {
                                                    // Native selection/IME can scroll its input. Keep paint
                                                    // aligned immediately, before the surface's queued event.
                                                    let moved = crate::viewport::sync_editor_scroll(&textarea, true);
                                                    if !moved && event.is_trusted() && crate::viewport::editor_scroll(&textarea).class_list().contains("editor-scroll-surface") { return; }
                                                    let _ = editor_actions.record_native_selection(projected_selection(editor_actions, &textarea));
                                                    editor_actions.record_scroll(editor_project.unwrap_or_default(), &textarea.get_attribute("data-editor-path").unwrap_or_default(), crate::viewport::editor_scroll(&textarea).scroll_top(), crate::viewport::editor_scroll(&textarea).scroll_left());
                                                    if let Some(overlay) = hl.get_untracked() { sync_highlight_scroll(&textarea, &overlay); }
                                                    layout_revision.update(|revision| *revision = revision.wrapping_add(1));
                                                }
                                            }
                                            on:wheel=move |event: web_sys::WheelEvent| {
                                                if let Some(textarea) = event.current_target().and_then(|target| target.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
                                                    && editor_actions.account_generation() == editor_account_generation
                                                    && current_editor_target(editor_actions, &textarea) { crate::viewport::forward_editor_wheel(&textarea, &event); }
                                            }
                                        />
                                        <span class="sr-only" data-editor-tab-announcement="" aria-live="polite" aria-atomic="true">{move || tab_focus_announcement.get()}</span>
                                    </div>
                                }.into_any()
                            }
                        }
                    }
                }}
            </Show>
            <Show when=move || open_file.get().is_some() && view_mode.get() == ViewMode::Code && open_file.with(|path| path.as_ref().is_some_and(|path| !FileKind::from_path(path).is_non_text()))>
                <super::editor_chrome::EditorFooter><div class="editor-footer">
                    <Button class="editor-cursor-status" size=ButtonSize::Sm variant=ButtonVariant::Ghost on_click=Callback::new(move |_| open_go.run(()))>{move || { let (line, column, count) = cursor_status.get(); format!("Ln {line}, Col {column}{}", if count == 0 { String::new() } else { format!(" · {count} selected") }) }}</Button>

                    <super::dropdown::Dropdown aria_label="Indentation settings" menu_role="dialog" above=true trigger_class="btn ghost sm" label=move || { let indentation = editor_actions.rules().indentation; format!("{}: {}", if indentation.style == openwebide_core::editor::IndentStyle::Tabs { "Tabs" } else { "Spaces" }, indentation.width()) }>
                        <super::editor_options::IndentationControls value=Signal::derive(move || editor_actions.rules().indentation) disabled=read_only on_change=Callback::new(move |indentation| editor_actions.set_indentation(indentation)) />
                        <span class="editor-rules-source" title=move || editor_actions.rules().source.unwrap_or_else(|| "Detected from this file, with editor defaults as fallback".into())>{move || if editor_actions.rules().source.is_some() { "EditorConfig" } else { "Detected / defaults" }}</span>
                    </super::dropdown::Dropdown>
                </div></super::editor_chrome::EditorFooter>
            </Show>
        </div>
    }.into_any()
}

#[cfg(test)]
mod tests {
    use super::{browser_matches, paint_text_range};
    use leptos::prelude::document;

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn cooperative_syntax_transport_stops_in_flight_and_rejects_new_requests() {
        use crate::editor_worker::{CooperativeClient, SyntaxTransport, WorkerError};
        let client = CooperativeClient::default();
        let source = "fn f() { call(); }\n".repeat(256);
        let request = openwebide_core::editor::SyntaxRequest::new(
            1,
            "editor-test".into(),
            openwebide_core::highlight::Language::Rust,
            &source,
            4,
            None,
        );
        let message = serde_json::to_string(&request).unwrap();
        let result = client
            .request_while(message.clone(), || {
                client.stop();
                true
            })
            .await;
        assert_eq!(result, Err(WorkerError::Unavailable));
        assert_eq!(
            client.request(1, message).await,
            Err(WorkerError::Unavailable)
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn deferred_caret_reveal_retries_once_and_rejects_new_intent() {
        use super::*;
        use openwebide_core::editor::Selection;
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        for change in [
            "none",
            "source",
            "selection",
            "file",
            "account",
            "scroll",
            "dispose",
        ] {
            let owner = Owner::new();
            let (workspace, auth, actions, paint, calls) = owner.with(|| {
                let auth = crate::state::auth::AuthState::new();
                provide_context(auth);
                let workspace = WorkspaceState::new();
                workspace.active_project.set(Some(1));
                workspace.open_file.set(Some("reveal.txt".into()));
                workspace.content.set("line\n".repeat(100).into());
                let actions = EditorActions::new(workspace);
                actions.prepare_edit(Selection::caret(0)).unwrap();
                let calls = Arc::new(AtomicUsize::new(0));
                (
                    workspace,
                    auth,
                    actions,
                    RwSignal::new(None::<EditorPaint>),
                    calls,
                )
            });
            let parent = document().create_element("div").unwrap();
            parent.set_attribute("data-editor-account", "0").unwrap();
            let input: web_sys::HtmlTextAreaElement = document()
                .create_element("textarea")
                .unwrap()
                .unchecked_into();
            input.set_attribute("data-editor-project", "1").unwrap();
            input
                .set_attribute("data-editor-path", "reveal.txt")
                .unwrap();
            input
                .set_attribute(
                    "style",
                    "width:300px;height:100px;line-height:20px;white-space:pre",
                )
                .unwrap();
            input.set_value(&"line\n".repeat(100));
            parent.append_child(&input).unwrap();
            document().body().unwrap().append_child(&parent).unwrap();
            let measured_input = input.clone();
            let measured_calls = calls.clone();
            owner.with(|| {
                paint.set(Some(EditorPaint {
                    ticket: 1,
                    flush: Callback::new(|()| ()),
                    neighborhood: Callback::new(|()| None),
                    caret: Callback::new(move |_| {
                        let count = measured_calls.fetch_add(1, Ordering::Relaxed);
                        if count == 0 {
                            return None;
                        }
                        let bounds = measured_input.get_bounding_client_rect();
                        web_sys::DomRect::new_with_x_and_y_and_width_and_height(
                            bounds.left(),
                            bounds.top() + 900.0 - measured_input.scroll_top(),
                            0.0,
                            20.0,
                        )
                        .ok()
                    }),
                }));
            });
            super::reveal_editor_caret(actions, &input, paint);
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            match change {
                "source" => workspace.content.set("changed".into()),
                "selection" => {
                    actions.prepare_edit(Selection::caret(5)).unwrap();
                }
                "file" => workspace.open_file.set(Some("other.txt".into())),
                "account" => auth.generation.update(|generation| *generation += 1),
                "scroll" => input.set_scroll_top(100.0),
                "dispose" => owner.cleanup(),
                _ => (),
            }
            crate::util::sleep_ms(100).await;
            assert_eq!(
                calls.load(Ordering::Relaxed),
                if change == "none" { 2 } else { 1 },
                "{change}"
            );
            if change == "none" {
                assert!(input.scroll_top() > 0.0);
            } else {
                assert_eq!(
                    input.scroll_top().to_bits(),
                    if change == "scroll" {
                        100.0_f64
                    } else {
                        0.0_f64
                    }
                    .to_bits(),
                    "{change}"
                );
            }
            parent.remove();
            owner.cleanup();
        }
    }
    fn find_matches(text: &str, query: &str) -> Vec<(u32, u32, usize)> {
        let pattern =
            openwebide_core::editor::SearchPattern::new(query, Default::default()).unwrap();
        browser_matches(text, &pattern.find(text, None).unwrap())
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn find_uses_utf16_offsets_and_logical_lines() {
        assert_eq!(
            find_matches("😀 café\n😀 café", "café"),
            vec![(3, 7, 1), (11, 15, 2)]
        );
        assert!(find_matches("text", "").is_empty());
        assert!(find_matches("Text", "text").is_empty());
        assert_eq!(
            find_matches("a\nb\na\nb", "a\nb"),
            vec![(0, 3, 1), (4, 7, 3)]
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn cropped_text_keeps_the_complete_paints_shaping_boundaries() {
        let source = format!(
            "{}e{} tail",
            "文😀e\u{301} <&> ".repeat(200),
            "\u{301}".repeat(600)
        );
        let full = document().create_element("div").unwrap();
        full.set_inner_html(&paint_text_range(
            &source,
            0..source.len(),
            false,
            false,
            None,
        ));
        // Both endpoints fall inside runs, not at the 512-byte source boundary.
        // A DOM Range clones the original markup without re-segmenting its text.
        let mut selections = vec![
            (3..517, false),
            (517..source.len() - 2, false),
            (3..10, false),
        ];
        for run in openwebide_core::editor::visual_text_run_ranges(&source).skip(2) {
            let short_end = run.start + source[run.start..].chars().next().unwrap().len_utf8();
            selections.push((run.start..short_end, true));
            selections.push((run.start..run.end, true));
        }
        for (selected, starts_paint_run) in selections {
            let dom_range = document().create_range().unwrap();
            let runs = full.query_selector_all(".editor-text-run").unwrap();
            let mut offset = 0;
            for index in 0..runs.length() {
                let node = runs.item(index).unwrap().first_child().unwrap();
                let text = node.text_content().unwrap();
                let end = offset + text.len();
                if (offset..end).contains(&selected.start) {
                    dom_range
                        .set_start(
                            &node,
                            u32::try_from(text[..selected.start - offset].encode_utf16().count())
                                .unwrap(),
                        )
                        .unwrap();
                }
                if selected.end > offset && selected.end <= end {
                    dom_range
                        .set_end(
                            &node,
                            u32::try_from(text[..selected.end - offset].encode_utf16().count())
                                .unwrap(),
                        )
                        .unwrap();
                }
                offset = end;
            }
            let expected = document().create_element("div").unwrap();
            let fragment = dom_range.clone_contents().unwrap();
            let ancestor = dom_range.common_ancestor_container().unwrap();
            if ancestor.node_type() == web_sys::Node::TEXT_NODE {
                // cloneContents omits the common ancestor when both endpoints
                // are in one text node; retain its original shaping span too.
                let wrapper = ancestor.parent_node().unwrap().clone_node().unwrap();
                wrapper.append_child(&fragment).unwrap();
                expected.append_child(&wrapper).unwrap();
            } else {
                expected.append_child(&fragment).unwrap();
            }
            let cropped = document().create_element("div").unwrap();
            cropped.set_inner_html(&paint_text_range(
                &source,
                selected.clone(),
                false,
                starts_paint_run,
                None,
            ));
            assert_eq!(cropped.inner_html(), expected.inner_html());
            assert_eq!(cropped.text_content().unwrap(), source[selected.clone()]);
            let ends = openwebide_core::editor::visual_text_run_ranges(&source)
                .map(|run| run.end)
                .collect::<Vec<_>>();
            cropped.set_inner_html(&paint_text_range(
                &source,
                selected,
                false,
                false,
                Some((&ends, 0)),
            ));
            assert_eq!(cropped.inner_html(), expected.inner_html());
        }
    }
}
