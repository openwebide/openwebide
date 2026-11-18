//! Shared editor UI actions and a footer mount owned by the app layout.
use leptos::prelude::*;
use wasm_bindgen::JsCast;

/// DOM destination supplied by the app; editor actions live in UiState.
#[derive(Clone, Copy, Default)]
pub struct EditorFooterMount(pub NodeRef<leptos::html::Div>);
impl EditorFooterMount {
    pub fn new() -> Self {
        Self(NodeRef::new())
    }
}

#[component]
pub(super) fn EditorFooter(children: ChildrenFn) -> impl IntoView {
    if let Some(chrome) = use_context::<EditorFooterMount>() {
        view! { <Show when=move || chrome.0.get().is_some()>{
            let children = children.clone();
            move || chrome.0.get().map(|mount| { let children = children.clone(); view! { <leptos::portal::Portal mount={mount.unchecked_into::<web_sys::Element>()}>{children()}</leptos::portal::Portal> } })
        }</Show> }.into_any()
    } else {
        children().into_any()
    }
}

#[component]
pub(super) fn EditorViewSelector(
    value: Signal<super::editor::ViewMode>,
    preview_supported: Signal<bool>,
    on_change: Callback<super::editor::ViewMode>,
) -> impl IntoView {
    use super::{
        dropdown::{DropdownSelect, SelectOption},
        editor::ViewMode,
        ui::{SegmentOption, SegmentedControl},
    };
    let normalized = Signal::derive(move || {
        if value.get() == ViewMode::SideBySide {
            ViewMode::InlineDiff
        } else {
            value.get()
        }
    });
    view! { <div class="editor-view-selector">
        <div class="editor-view-desktop">
            <SegmentedControl label="Editor view" options=vec![
                SegmentOption::new("Edit", ViewMode::Code),
                SegmentOption::new("Diff", ViewMode::InlineDiff),
                SegmentOption::new("Preview", ViewMode::Preview).visible_when(preview_supported),
            ] value=normalized on_change=on_change />
        </div>
        <div class="editor-view-narrow"><DropdownSelect label="Editor view" trigger_class="btn ghost sm" value=Signal::derive(move || match normalized.get() { ViewMode::Code => "edit", ViewMode::Preview => "preview", _ => "changes" }.to_string()) options=Signal::derive(move || {
            let mut options = vec![SelectOption::new("edit", "Edit"), SelectOption::new("changes", "Diff")];
            if preview_supported.get() { options.push(SelectOption::new("preview", "Preview")); }
            options
        }) on_change=Callback::new(move |value: String| on_change.run(match value.as_str() { "preview" => ViewMode::Preview, "changes" => ViewMode::InlineDiff, _ => ViewMode::Code })) /></div>
    </div> }
}
