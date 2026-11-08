//! Browser events and geometry for the shared language-service request facade.
use crate::{
    components::editor::{EditorPaint, current_editor_target},
    state::workspace::WorkspaceState,
    state_actions::editor::{EditorIntelligence, EditorPresentationScope, IntelligenceCommand},
};
use leptos::{
    leptos_dom::helpers::{TimeoutHandle, set_timeout_with_handle},
    prelude::*,
};
use openwebide_core::{
    editor::Selection,
    plugins::languages::intelligence::{Query, results},
};
use std::{
    ops::Range,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use wasm_bindgen::JsCast;

const MAX_CHOICES: usize = 100;
const MAX_WORD: usize = 256;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Context {
    provider: u64,
    document: EditorPresentationScope,
    revision: u64,
    selection: Selection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Choice {
    id: String,
    label: String,
    detail: String,
}

fn word_character(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '$')
}

fn prefix(source: &str, offset: usize) -> Option<&str> {
    let before = source.get(..offset)?;
    let mut start = offset;
    for (count, (at, ch)) in before.char_indices().rev().enumerate() {
        if !word_character(ch) {
            break;
        }
        if count >= MAX_WORD {
            return None;
        }
        start = at;
    }
    Some(&source[start..offset])
}

fn word(source: &str, offset: usize) -> Option<Range<usize>> {
    let tail = source.get(offset..)?;
    let start = offset.checked_sub(prefix(source, offset)?.len())?;
    let mut end = offset;
    for (count, ch) in tail.chars().enumerate() {
        if !word_character(ch) {
            break;
        }
        if count >= MAX_WORD {
            return None;
        }
        end += ch.len_utf8();
    }
    (start < end && end - start <= MAX_WORD * 4).then_some(start..end)
}

fn choices(value: &serde_json::Value, prefix: &str) -> Vec<Choice> {
    let Some(values) = value.as_array().filter(|items| {
        items.len() <= openwebide_core::plugins::languages::intelligence::MAX_RECORDS
    }) else {
        return vec![];
    };
    let prefix = prefix.to_lowercase();
    let mut values = values
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let id = item["id"]
                .as_str()
                .filter(|id| id.len() == 32 && id.bytes().all(|ch| ch.is_ascii_hexdigit()))?;
            let label = item["label"]
                .as_str()
                .filter(|label| !label.is_empty() && label.len() <= 4096)?;
            let filter = item["filterText"].as_str().unwrap_or(label);
            if filter.len() > 4096 || !filter.to_lowercase().starts_with(&prefix) {
                return None;
            }
            let sort = item["sortText"]
                .as_str()
                .filter(|text| text.len() <= 4096)
                .unwrap_or(label);
            Some((
                sort.to_string(),
                index,
                Choice {
                    id: id.into(),
                    label: label.into(),
                    detail: item["detail"]
                        .as_str()
                        .filter(|text| text.len() <= 4096)
                        .unwrap_or_default()
                        .into(),
                },
            ))
        })
        .collect::<Vec<_>>();
    values.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    values
        .into_iter()
        .take(MAX_CHOICES)
        .map(|(_, _, item)| item)
        .collect()
}

fn placement(
    x: f64,
    top: f64,
    bottom: f64,
    width: f64,
    height: f64,
    desired: f64,
) -> Option<(f64, f64, f64, f64)> {
    if ![x, top, bottom, width, height, desired]
        .iter()
        .all(|v| v.is_finite())
        || width <= 16.0
        || height <= 16.0
    {
        return None;
    }
    if bottom < top || top > height || bottom < 0.0 {
        return None;
    }
    let popup_width = 420.0_f64.min(width - 16.0);
    let below = (height - bottom - 12.0).max(0.0);
    let above = (top - 12.0).max(0.0);
    let down = below >= desired.min(180.0) || below >= above;
    let popup_height = desired
        .min(if down { below } else { above })
        .min(height - 16.0);
    if popup_height < 24.0 {
        return None;
    }
    Some((
        x.clamp(8.0, width - popup_width - 8.0),
        (if down {
            bottom + 4.0
        } else {
            top - popup_height - 4.0
        })
        .clamp(8.0, height - popup_height - 8.0),
        popup_width,
        popup_height,
    ))
}

#[derive(Clone, Copy)]
pub(in crate::components) struct PopupAdapter {
    actions: EditorIntelligence,
    workspace: WorkspaceState,
    input: NodeRef<leptos::html::Textarea>,
    ready: RwSignal<bool>,
    paint: RwSignal<Option<EditorPaint>>,
    read_only: Signal<bool>,
    timer: StoredValue<Option<TimeoutHandle>>,
    hide: StoredValue<Option<TimeoutHandle>>,
    expected: StoredValue<Option<Context>>,
    hovered: StoredValue<Option<Range<usize>>>,
    completing: StoredValue<bool>,
    selected: RwSignal<usize>,
    items: Memo<Vec<Choice>>,
    bounds: RwSignal<Option<(f64, f64, f64, f64)>>,
    id: StoredValue<String>,
    list: NodeRef<leptos::html::Div>,
    completion_box: NodeRef<leptos::html::Div>,
    hover_box: NodeRef<leptos::html::Div>,
}

impl PopupAdapter {
    pub fn new(
        actions: EditorIntelligence,
        workspace: WorkspaceState,
        input: NodeRef<leptos::html::Textarea>,
        ready: RwSignal<bool>,
        paint: RwSignal<Option<EditorPaint>>,
        layout: RwSignal<u64>,
        read_only: Signal<bool>,
    ) -> Self {
        let items = Memo::new(move |_| {
            actions.popup.with(|value| {
                let Some((Query::Completion { .. }, response)) = value else {
                    return vec![];
                };
                let Some(offset) = actions.popup_offset() else {
                    return vec![];
                };
                workspace.content.with_untracked(|source| {
                    choices(&response.result, prefix(source, offset).unwrap_or_default())
                })
            })
        });
        let adapter = Self {
            actions,
            workspace,
            input,
            ready,
            paint,
            read_only,
            timer: StoredValue::new(None),
            hide: StoredValue::new(None),
            expected: StoredValue::new(None),
            hovered: StoredValue::new(None),
            completing: StoredValue::new(false),
            selected: RwSignal::new(0),
            items,
            bounds: RwSignal::new(None),
            id: StoredValue::new(format!(
                "editor-completions-{}",
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            )),
            list: NodeRef::new(),
            completion_box: NodeRef::new(),
            hover_box: NodeRef::new(),
        };
        Effect::new(move |_| {
            actions.context_revision.track();
            workspace.editor_source_revision.track();
            workspace.editor_documents.track();
            workspace.editor_composition.track();
            actions.available.track();
            actions.busy.track();
            actions.result.track();
            actions.review.track();
            actions.rename_open.track();
            read_only.track();
            let mounted = adapter.input.get().is_some();
            if !mounted
                || actions.busy.get_untracked()
                || actions.result.with_untracked(Option::is_some)
                || actions.rename_open.get_untracked()
                || actions.review.with_untracked(|files| !files.is_empty())
                || adapter.expected.with_value(|expected| {
                    expected
                        .as_ref()
                        .is_some_and(|expected| adapter.context().as_ref() != Some(expected))
                })
                || actions.editor.is_composing()
                || !actions.available.get_untracked()
                || (read_only.get_untracked() && actions.completion_active())
            {
                adapter.cancel();
            }
        });
        Effect::new(move |_| {
            actions.popup.track();
            adapter.selected.set(0);
            if items.get().is_empty()
                && !actions.popup_busy.get_untracked()
                && actions.popup.with_untracked(|value| {
                    value
                        .as_ref()
                        .is_some_and(|(query, _)| matches!(query, Query::Completion { .. }))
                })
            {
                actions.popup.set(None);
                actions.popup_error.set(None);
            }
        });
        Effect::new(move |_| {
            layout.track();
            ready.track();
            actions.popup.track();
            items.track();
            let _ = adapter.completion_box.get();
            let _ = adapter.hover_box.get();
            adapter.position();
        });
        let resize = window_event_listener(leptos::ev::resize, move |_| adapter.cancel());
        let blur = window_event_listener(leptos::ev::blur, move |_| adapter.cancel());
        let escape = window_event_listener(leptos::ev::keydown, move |event| {
            if event.key() == "Escape"
                && !event.is_composing()
                && actions.popup.with_untracked(Option::is_some)
            {
                adapter.cancel();
            }
        });
        on_cleanup(move || {
            adapter.clear_timer();
            adapter.clear_hide();
            resize.remove();
            blur.remove();
            escape.remove();
        });
        adapter
    }
    fn context(self) -> Option<Context> {
        self.input
            .get_untracked()
            .filter(|input| current_editor_target(self.actions.editor, input))?;
        Some(Context {
            provider: self.actions.context_revision.get_untracked(),
            document: self.actions.editor.presentation_scope()?,
            revision: self.actions.editor.source_revision(),
            selection: self.actions.editor.current_selection()?,
        })
    }
    fn clear_timer(self) {
        let _ = self.timer.try_update_value(|timer| {
            if let Some(timer) = timer.take() {
                timer.clear();
            }
        });
        self.completing.set_value(false);
    }
    fn clear_hide(self) {
        let _ = self.hide.try_update_value(|timer| {
            if let Some(timer) = timer.take() {
                timer.clear();
            }
        });
    }
    pub fn viewport_changed(self) {
        if self.completing.get_value() || self.actions.completion_active() {
            self.position();
        } else {
            self.cancel();
        }
    }
    pub fn cancel(self) {
        self.clear_timer();
        self.clear_hide();
        self.hovered.set_value(None);
        self.expected.set_value(None);
        self.actions.dismiss_popup.run(());
        self.bounds.set(None);
    }
    fn schedule(self, command: IntelligenceCommand, delay: u64, completion: bool) {
        self.clear_timer();
        self.clear_hide();
        let Some(expected) = self.context() else {
            return;
        };
        if expected.selection.anchor != expected.selection.head
            || self.actions.editor.selection_count() > 1
            || self.actions.editor.is_composing()
            || !self.actions.available.get_untracked()
            || (completion && self.read_only.get_untracked())
        {
            return;
        }
        self.expected.set_value(Some(expected.clone()));
        if completion {
            self.hovered.set_value(None);
        }
        self.completing.set_value(completion);
        let timer = set_timeout_with_handle(
            move || {
                if self.timer.is_disposed() {
                    return;
                }
                self.timer.set_value(None);
                self.completing.set_value(false);
                if self.context().as_ref() == Some(&expected)
                    && !self.actions.editor.is_composing()
                    && (!completion || (!self.read_only.get_untracked() && self.focused()))
                {
                    self.actions.request.run(command);
                }
            },
            Duration::from_millis(delay),
        )
        .ok();
        self.timer.set_value(timer);
    }
    fn owns_event(self, event: &web_sys::Event) -> bool {
        self.input
            .get_untracked()
            .filter(|input| current_editor_target(self.actions.editor, input))
            .is_some_and(|input| {
                event
                    .target()
                    .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
                    .is_some_and(|target| {
                        target.is_connected() && input.is_same_node(Some(&target))
                    })
            })
    }
    pub fn dismiss_input(self, event: &web_sys::Event) {
        if self.owns_event(event) {
            self.cancel();
        }
    }
    pub fn leave_input(self, event: &web_sys::MouseEvent) {
        if self.owns_event(event) {
            self.leave();
        }
    }
    fn focused(self) -> bool {
        self.input.get_untracked().is_some_and(|input| {
            web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.active_element())
                .is_some_and(|active| active == **input)
        })
    }
    pub fn typed(self, input_type: &str) {
        let refresh = input_type == "deleteContentBackward" && self.actions.completion_active();
        if input_type != "insertText" && !refresh {
            self.cancel();
            return;
        }
        let eligible = self
            .actions
            .editor
            .current_selection()
            .is_some_and(|selection| {
                self.workspace.content.with_untracked(|source| {
                    source
                        .get(..selection.head)
                        .and_then(|before| before.chars().next_back())
                        .is_some_and(|ch| word_character(ch) || matches!(ch, '.' | ':' | '>'))
                })
            });
        if eligible {
            self.schedule(IntelligenceCommand::Suggest, 150, true);
        } else {
            self.cancel();
        }
    }
    fn accept(self, id: String) {
        if self.bounds.get_untracked().is_some()
            && self.actions.popup_offset().is_some()
            && !self.read_only.get_untracked()
            && !self.actions.popup_busy.get_untracked()
        {
            self.actions
                .request
                .run(IntelligenceCommand::InsertSuggestion(id));
        }
    }
    pub fn key(self, event: &web_sys::KeyboardEvent, tab_moves_focus: bool) -> bool {
        if !self.owns_event(event) {
            return false;
        }
        if event.is_composing() || self.actions.editor.is_composing() {
            self.cancel();
            return false;
        }
        if event.ctrl_key()
            && !event.alt_key()
            && event.key() == " "
            && !self.read_only.get_untracked()
        {
            self.schedule(IntelligenceCommand::Suggest, 0, true);
            return true;
        }
        if event.key() == "Escape" {
            let active = self.actions.popup.with_untracked(Option::is_some)
                || self.actions.popup_busy.get_untracked()
                || self.timer.with_value(Option::is_some);
            self.cancel();
            return active;
        }
        if self.bounds.get_untracked().is_some()
            && self.actions.popup_offset().is_some()
            && !event.ctrl_key()
            && !event.meta_key()
            && !event.alt_key()
        {
            let items = self.items.get_untracked();
            if !items.is_empty() {
                match event.key().as_str() {
                    "ArrowDown" | "ArrowUp" => {
                        let down = event.key() == "ArrowDown";
                        self.selected.update(|index| {
                            *index = if down {
                                (*index + 1) % items.len()
                            } else {
                                (*index + items.len() - 1) % items.len()
                            }
                        });
                        self.reveal();
                        return true;
                    }
                    "Enter" | "Tab"
                        if !event.shift_key() && (event.key() != "Tab" || !tab_moves_focus) =>
                    {
                        if let Some(choice) = items.get(self.selected.get_untracked()) {
                            self.accept(choice.id.clone());
                        }
                        return true;
                    }
                    _ => {}
                }
            }
        }
        if matches!(
            event.key().as_str(),
            "ArrowLeft"
                | "ArrowRight"
                | "ArrowUp"
                | "ArrowDown"
                | "Home"
                | "End"
                | "PageUp"
                | "PageDown"
                | "Tab"
                | "Enter"
        ) || event.ctrl_key()
            || event.meta_key()
            || event.alt_key()
        {
            self.cancel();
        }
        false
    }
    pub fn pointer(self, event: &web_sys::MouseEvent) {
        if !self.owns_event(event) {
            return;
        }
        if self.completing.get_value() || self.actions.completion_active() {
            return;
        }
        if event.buttons() != 0 || !self.ready.get_untracked() {
            self.cancel();
            return;
        }
        let Some(input) = self
            .input
            .get_untracked()
            .filter(|input| current_editor_target(self.actions.editor, input))
        else {
            return;
        };
        let offset =
            crate::viewport::editor_caret_from_point(&input, event.client_x(), event.client_y())
                .and_then(|offset| {
                    let projection = self.actions.editor.projection()?;
                    projection
                        .source_offset(projection.textarea_to_byte(offset as usize))
                        .ok()
                });
        let range = offset.and_then(|offset| {
            self.workspace
                .content
                .with_untracked(|source| word(source, offset))
        });
        let Some(range) = range else {
            self.cancel();
            return;
        };
        let Some(paint) = self.paint.get_untracked() else {
            return;
        };
        let (Some(start), Some(end)) = (paint.caret.run(range.start), paint.caret.run(range.end))
        else {
            self.cancel();
            return;
        };
        let x = event.client_x();
        let y = event.client_y();
        let same_row = (start.top() - end.top()).abs() < 1.0;
        let on_start = y >= start.top() && y <= start.bottom();
        let on_end = y >= end.top() && y <= end.bottom();
        let outside = if same_row {
            !on_start
                || x < start.left().min(end.left()) - 1.0
                || x > start.left().max(end.left()) + 1.0
        } else {
            y < start.top()
                || y > end.bottom()
                || (on_start && x < start.left() - 1.0)
                || (on_end && x > end.left() + 1.0)
        };
        if outside {
            self.cancel();
            return;
        }
        self.clear_hide();
        if self
            .hovered
            .with_value(|previous| previous.as_ref() == Some(&range))
        {
            return;
        }
        self.actions.dismiss_popup.run(());
        self.hovered.set_value(Some(range.clone()));
        self.schedule(IntelligenceCommand::HoverAt(range.start), 400, false);
    }
    pub fn leave(self) {
        self.clear_hide();
        self.hide.set_value(
            set_timeout_with_handle(
                move || {
                    if !self.hide.is_disposed() {
                        self.cancel();
                    }
                },
                Duration::from_millis(180),
            )
            .ok(),
        );
    }
    pub fn blur(self, event: &web_sys::FocusEvent) {
        if !self.expected.with_value(|expected| {
            expected
                .as_ref()
                .is_some_and(|expected| self.context().as_ref() == Some(expected))
        }) {
            return;
        }
        let inside = event
            .related_target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .is_some_and(|element| {
                element
                    .closest("[data-editor-hover]")
                    .ok()
                    .flatten()
                    .is_some_and(|node| {
                        node.get_attribute("data-editor-hover").as_deref()
                            == Some(self.id.get_value().as_str())
                    })
            });
        if !inside {
            self.cancel();
        }
    }
    fn reveal(self) {
        let (Some(list), Some(document)) = (
            self.list.get_untracked(),
            web_sys::window().and_then(|w| w.document()),
        ) else {
            return;
        };
        let Some(item) = document.get_element_by_id(&self.option_id(self.selected.get_untracked()))
        else {
            return;
        };
        let item = item.get_bounding_client_rect();
        let bounds = list.get_bounding_client_rect();
        let delta = if item.top() < bounds.top() {
            item.top() - bounds.top()
        } else {
            (item.bottom() - bounds.bottom()).max(0.0)
        };
        list.set_scroll_top(list.scroll_top() + delta);
    }
    fn option_id(self, index: usize) -> String {
        format!("{}-{index}", self.id.get_value())
    }
    pub fn controls(self) -> Option<String> {
        (!self.items.get().is_empty() && self.bounds.get().is_some()).then(|| self.id.get_value())
    }
    pub fn description(self) -> Option<String> {
        (self.bounds.get().is_some()
            && self.actions.popup.with(|value| {
                value
                    .as_ref()
                    .is_some_and(|(query, _)| matches!(query, Query::Hover { .. }))
            }))
        .then(|| format!("{}-hover", self.id.get_value()))
    }
    pub fn active_descendant(self) -> Option<String> {
        self.controls().map(|_| self.option_id(self.selected.get()))
    }
    fn position(self) {
        let rectangle = (|| {
            if !self.ready.get_untracked() {
                return None;
            }
            let offset = self.actions.popup_offset()?;
            let input = self
                .input
                .get_untracked()
                .filter(|input| current_editor_target(self.actions.editor, input))?;
            let rect = self.paint.get_untracked()?.caret.run(offset)?;
            let viewport = input.get_bounding_client_rect();
            if rect.bottom() < viewport.top()
                || rect.top() > viewport.bottom()
                || rect.left() < viewport.left()
                || rect.left() > viewport.right()
            {
                return None;
            }
            let window = web_sys::window()?;
            let measured = self
                .completion_box
                .get_untracked()
                .or_else(|| self.hover_box.get_untracked())
                .map(|node| node.get_bounding_client_rect().height())
                .filter(|height| *height > 0.0);
            let height = if self.items.get_untracked().is_empty() {
                300.0
            } else {
                (self.items.with_untracked(Vec::len) as f64 * 44.0 + 36.0).min(300.0)
            };
            placement(
                rect.left(),
                rect.top(),
                rect.bottom(),
                window.inner_width().ok()?.as_f64()?,
                window.inner_height().ok()?.as_f64()?,
                measured.unwrap_or(height),
            )
        })();
        self.bounds.set(rectangle);
    }
}

#[component]
pub(in crate::components) fn IntelligencePopups(adapter: PopupAdapter) -> impl IntoView {
    let style = move || {
        adapter
            .bounds
            .get()
            .map(|(x, y, width, height)| {
                format!("left:{x}px;top:{y}px;width:{width}px;max-height:{height}px")
            })
            .unwrap_or_default()
    };
    view! {
        <Show when=move || adapter.bounds.get().is_some() && !adapter.items.get().is_empty()>
            <div class="editor-completion-popup ui-dropdown-menu recent-menu" node_ref=adapter.completion_box style=style on:mousedown=move |event: web_sys::MouseEvent| event.prevent_default() on:mouseenter=move |_| adapter.clear_hide()>
                <div class="editor-completion-list" role="listbox" aria-label="Code completions" id=move || adapter.id.get_value() node_ref=adapter.list>
                    {move || adapter.items.get().into_iter().enumerate().map(move |(index, choice)| {
                        let id = choice.id;
                        view! { <div role="option" class="editor-completion-option recent-item" id=adapter.option_id(index)
                            aria-selected=move || (adapter.selected.get() == index).to_string()
                            on:mouseenter=move |_| adapter.selected.set(index)
                            on:click=move |_| adapter.accept(id.clone())>
                            <span>{choice.label}</span><small>{choice.detail}</small>
                        </div> }
                    }).collect_view()}
                </div>
                <Show when=move || adapter.actions.popup_error.with(Option::is_some)><p class="form-hint" role="alert">{move || adapter.actions.popup_error.get()}</p></Show>
                <div class="editor-completion-hint" role="status">{move || if adapter.actions.popup_busy.get() { "Inserting…" } else { "↑↓ select · Enter insert · Esc close" }}</div>
            </div>
        </Show>
        <Show when=move || adapter.bounds.get().is_some() && adapter.actions.popup.with(|value| value.as_ref().is_some_and(|(query, _)| matches!(query, Query::Hover { .. })))>
            <div class="editor-hover-popup ui-dropdown-menu recent-menu markdown" node_ref=adapter.hover_box data-editor-hover=move || adapter.id.get_value() id=move || format!("{}-hover", adapter.id.get_value()) role="dialog" aria-modal="false" aria-label="Symbol information" style=style
                on:mouseenter=move |_| adapter.clear_hide() on:mouseleave=move |_| adapter.leave() on:focusout=move |event: web_sys::FocusEvent| adapter.blur(&event)
                on:keydown=move |event: web_sys::KeyboardEvent| { if event.key() == "Escape" { event.stop_propagation(); adapter.cancel(); } }
                inner_html=move || adapter.actions.popup.with(|value| value.as_ref().and_then(|(_, response)| results::hover(&response.result).ok().flatten()).map(|text| ammonia::Builder::default().rm_tags(&["img"]).clean(&crate::markdown::render(&text)).to_string()).unwrap_or_default()) />
        </Show>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn word_queries_preserve_unicode_boundaries_and_stop_at_crlf_and_punctuation() {
        let text = "文😀\r\nobject.Ångström_42";
        assert_eq!(prefix(text, text.len()), Some("Ångström_42"));
        let start = text.find('Å').unwrap();
        assert_eq!(word(text, start + 'Å'.len_utf8()), Some(start..text.len()));
        assert_eq!(word(text, start + 1), None, "Never split a UTF-8 character");
        assert_eq!(prefix("x.\r\n", 4), Some(""));
        assert_eq!(prefix(&"a".repeat(MAX_WORD + 1), MAX_WORD + 1), None);
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn suggestions_use_filter_text_and_server_order_with_bounded_rendering() {
        let data = json!([
            {"id":"a".repeat(32),"label":"different label","filterText":"Ångström","sortText":"2"},
            {"id":"b".repeat(32),"label":"Ångle","sortText":"1"},
            {"id":"c".repeat(32),"label":"other","sortText":"0"},
            {"id":"forged","label":"Ångry"}
        ]);
        let result = choices(&data, "ång");
        assert_eq!(
            result
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            ["Ångle", "different label"]
        );
        let many = serde_json::Value::Array(
            (0..MAX_CHOICES * 2)
                .map(
                    |index| json!({"id":format!("{index:032x}"),"label":format!("item{index:03}")}),
                )
                .collect(),
        );
        assert_eq!(choices(&many, "").len(), MAX_CHOICES);
        assert!(choices(&serde_json::Value::Null, "").is_empty());
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn popup_placement_stays_on_screen_at_edges_and_rejects_invalid_geometry() {
        for width in [32.0, 180.0, 1280.0] {
            for height in [64.0, 180.0, 900.0] {
                for x in [-10.0, 8.0, width - 1.0, width + 20.0] {
                    for top in [-2.0, 20.0, height - 20.0] {
                        if let Some((left, y, w, h)) =
                            placement(x, top, top + 18.0, width, height, 300.0)
                        {
                            assert!(left >= 8.0 && y >= 8.0);
                            assert!(left + w <= width - 8.0 + 0.01);
                            assert!(y + h <= height - 8.0 + 0.01);
                        }
                    }
                }
            }
        }
        assert!(placement(f64::NAN, 20.0, 40.0, 800.0, 600.0, 300.0).is_none());
        assert!(placement(20.0, 700.0, 720.0, 800.0, 600.0, 300.0).is_none());
        assert!(placement(20.0, 20.0, 40.0, 8.0, 8.0, 300.0).is_none());
    }
}
