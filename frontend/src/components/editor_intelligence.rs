//! Shared editor controls for installed language-server providers.
use super::{
    modal::Modal,
    ui::{Button, DialogActions, DialogBody, FormField},
};
use crate::state_actions::editor::{EditorIntelligence, IntelligenceCommand};
use leptos::prelude::*;
use openwebide_core::plugins::languages::intelligence::{
    Query,
    results::{self, Symbol, Target, TextEdit, WorkspaceEdit},
};

fn title(query: &Query) -> &'static str {
    match query {
        Query::Hover { .. } => "Symbol information",
        Query::Completion { .. } | Query::ResolveCompletion { .. } => "Completions",
        Query::Definition { .. } => "Definitions",
        Query::References { .. } => "References",
        Query::Symbols => "Document symbols",
        Query::Rename { .. } => "Review rename",
        Query::Formatting { .. } => "Review formatting",
        Query::CodeActions { .. } | Query::ResolveAction { .. } => "Code actions",
        Query::ReadResource { .. } => "External source · read-only",
        _ => "Language service",
    }
}

#[component]
pub fn EditorIntelligenceToolbar(
    actions: EditorIntelligence,
    read_only: Signal<bool>,
) -> impl IntoView {
    let rename = actions.rename_open;
    let name = RwSignal::new(String::new());
    let problems = RwSignal::new(false);
    let close = actions.dismiss;
    view! {
        <Show when=move || actions.available.get()>
            <super::dropdown::ActionMenu aria_label="Code intelligence" icon=super::ui::IconName::Brackets>
                {[
                    ("Symbol information", IntelligenceCommand::Hover, false),
                    ("Complete at cursor", IntelligenceCommand::Completion, true),
                    ("Go to definition", IntelligenceCommand::Definition, false),
                    ("Find references", IntelligenceCommand::References, false),
                    ("Document symbols", IntelligenceCommand::Symbols, false),
                    ("Format document", IntelligenceCommand::Formatting, true),
                    ("Code actions", IntelligenceCommand::CodeActions, true),
                ].into_iter().map(move |(label, command, writes)| view! {
                    <button type="button" role="menuitem" class="ui-dropdown-item"
                        disabled=move || actions.busy.get() || (writes && read_only.get())
                        on:click=move |_| actions.request.run(command.clone())>{label}</button>
                }).collect_view()}
                <button type="button" role="menuitem" class="ui-dropdown-item"
                    disabled=move || read_only.get() || actions.busy.get()
                    on:click=move |_| { name.set(String::new()); rename.set(true); }>"Rename symbol…"</button>
                <button type="button" role="menuitem" class="ui-dropdown-item"
                    on:click=move |_| problems.set(true)>"Problems ("{move || actions.diagnostics.with(Vec::len)}")"</button>
            </super::dropdown::ActionMenu>
        </Show>
        <Show when=move || actions.busy.get()><span class="form-hint" role="status">"Analyzing…"</span></Show>
        <Show when=move || actions.error.with(Option::is_some)>
            <span class="form-hint" role="alert">{move || actions.error.get()}</span>
        </Show>
        <Show when=move || rename.get()>
            <Modal title=Signal::derive(|| "Rename symbol".to_string()) on_close=Callback::new(move |()| rename.set(false))>
                <DialogBody><FormField label="New name">
                    <input class="form-input" type="text" prop:value=move || name.get()
                        on:input=move |event| name.set(event_target_value(&event)) />
                </FormField></DialogBody>
                <DialogActions><Button disabled=Signal::derive(move || name.with(|name| name.trim().is_empty()) || read_only.get())
                    on_click=Callback::new(move |_| { rename.set(false); actions.request.run(IntelligenceCommand::Rename(name.get_untracked())); })>"Review rename"</Button></DialogActions>
            </Modal>
        </Show>
        <Show when=move || problems.get()>
            <Modal title=Signal::derive(|| "Problems".to_string()) on_close=Callback::new(move |()| problems.set(false))>
                <DialogBody>
                    <Show when=move || actions.diagnostics.with(Vec::is_empty)><p class="form-hint">"No reported problems in this document."</p></Show>
                    {move || actions.diagnostics.get().into_iter().map(move |diagnostic| {
                        let location = format!("{}:{}", diagnostic.range.start.line + 1, diagnostic.range.start.character + 1);
                        view! { <button type="button" class="ui-dropdown-item" on:click=move |_| {
                            actions.navigate_diagnostic(diagnostic.range);
                            problems.set(false);
                        }><span>{location}" "{diagnostic.message}</span></button> }
                    }).collect_view()}
                </DialogBody>
            </Modal>
        </Show>
        <Show when=move || actions.result.with(Option::is_some)>
            <Modal title=Signal::derive(move || actions.result.with(|result| result.as_ref().map_or("Language service", |(query, _)| title(query)).to_string())) on_close=close size=super::ui::DialogSize::Wide>
                <DialogBody class="editor-intelligence-result">
                    {move || actions.result.get().map(|(query, response)| result_view(actions, query, response.result, read_only))}
                    <Show when=move || actions.error.with(Option::is_some)><p class="form-hint" role="alert">{move || actions.error.get()}</p></Show>
                </DialogBody>
            </Modal>
        </Show>
    }
}

fn result_view(
    actions: EditorIntelligence,
    query: Query,
    result: serde_json::Value,
    read_only: Signal<bool>,
) -> AnyView {
    match query {
        Query::Hover { .. } => {
            let text = results::hover(&result)
                .ok()
                .flatten()
                .unwrap_or_else(|| "No symbol information.".into());
            view! { <div class="markdown" inner_html=crate::markdown::render(&text) /> }.into_any()
        }
        Query::Definition { .. } | Query::References { .. } => {
            let targets = serde_json::from_value::<Vec<Target>>(result).unwrap_or_default();
            navigation(
                actions,
                targets
                    .into_iter()
                    .map(|target| {
                        (
                            format!(
                                "{}:{}:{}",
                                target.label,
                                target.range.start.line + 1,
                                target.range.start.character + 1
                            ),
                            target,
                        )
                    })
                    .collect(),
            )
        }
        Query::Symbols => {
            let symbols = serde_json::from_value::<Vec<Symbol>>(result).unwrap_or_default();
            navigation(
                actions,
                symbols
                    .into_iter()
                    .map(|symbol| (symbol.name, symbol.target))
                    .collect(),
            )
        }
        Query::Completion { .. } | Query::CodeActions { .. } => {
            let completion = matches!(query, Query::Completion { .. });
            let values = result.as_array().cloned().unwrap_or_default();
            let empty = values.is_empty();
            view! {
                <Show when=move || empty><p class="form-hint">"No suggestions."</p></Show>
                {values.into_iter().filter_map(move |value| {
                    let id = value["id"].as_str()?.to_string();
                    let label = value[if completion { "label" } else { "title" }].as_str()?.to_string();
                    let disabled = !value["disabled"].is_null();
                    Some(view! { <button type="button" class="ui-dropdown-item" disabled=move || disabled || read_only.get() || actions.busy.get()
                        on:click=move |_| actions.request.run(IntelligenceCommand::Query(if completion { Query::ResolveCompletion { id: id.clone() } } else { Query::ResolveAction { id: id.clone() } }))>{label}</button> })
                }).collect_view()}
            }.into_any()
        }
        Query::ResolveCompletion { .. } => {
            let label = result["label"].as_str().unwrap_or("Completion").to_string();
            let detail = result["detail"].as_str().unwrap_or_default().to_string();
            view! { <p>{label}</p><pre>{detail}</pre><Button disabled=read_only on_click=Callback::new(move |_| actions.apply_completion(&result))>"Insert completion"</Button> }.into_any()
        }
        Query::Formatting { .. } => {
            let edits = if result.is_null() {
                Ok(vec![])
            } else {
                serde_json::from_value::<Vec<TextEdit>>(result)
            };
            match edits {
                Ok(edits) => {
                    let before = actions.editor.source();
                    let after = results::apply_edits(&before, &edits);
                    match after {
                        Ok(after) => view! {
                            {super::editor::render_inline_diff(openwebide_core::FileDiff { path: actions.document_path().unwrap_or_default(), old: Some(before.to_string()), new: after, old_unavailable: false, backup_path: None })}
                            <Button disabled=read_only on_click=Callback::new(move |_| actions.apply_text.run(edits.clone()))>"Apply formatting"</Button>
                        }.into_any(),
                        Err(error) => view! { <p role="alert">{error}</p> }.into_any(),
                    }
                }
                Err(_) => view! { <p role="alert">"Invalid formatting result."</p> }.into_any(),
            }
        }
        Query::Rename { .. } | Query::ResolveAction { .. } => {
            match serde_json::from_value::<WorkspaceEdit>(result) {
                Ok(edit) => view! { <WorkspaceEditReview actions edit read_only /> }.into_any(),
                Err(_) => view! { <p role="alert">"Invalid workspace edit."</p> }.into_any(),
            }
        }
        Query::ReadResource { .. } => {
            let name = result["name"]
                .as_str()
                .unwrap_or("External source")
                .to_string();
            let source = result["source"].as_str().unwrap_or_default().to_string();
            view! { <p class="form-hint">{name}</p><pre>{source}</pre> }.into_any()
        }
        _ => ().into_any(),
    }
}
fn navigation(actions: EditorIntelligence, targets: Vec<(String, Target)>) -> AnyView {
    let empty = targets.is_empty();
    view! {
        <Show when=move || empty><p class="form-hint">"No results."</p></Show>
        {targets.into_iter().map(move |(label, target)| view! {
            <button type="button" class="ui-dropdown-item" on:click=move |_| actions.navigate.run(target.clone())>{label}</button>
        }).collect_view()}
    }.into_any()
}

#[component]
fn WorkspaceEditReview(
    actions: EditorIntelligence,
    edit: WorkspaceEdit,
    read_only: Signal<bool>,
) -> impl IntoView {
    let edit = StoredValue::new(edit);
    Effect::new(move |_| actions.prepare_workspace(edit.get_value()));
    view! {
        <p class="form-hint">"Apply to open tabs as unsaved changes. Each file keeps its undo history; save when ready."</p>
        {move || actions.review.get().into_iter().map(|file| view! {
            <h3>{file.path.clone()}</h3>
            {super::editor::render_inline_diff(openwebide_core::FileDiff { path: file.path, old: Some(file.before.as_ref().clone()), new: file.after.text().to_string(), old_unavailable: false, backup_path: None })}
        }).collect_view()}
        <Button disabled=Signal::derive(move || read_only.get() || actions.busy.get() || actions.review.with(Vec::is_empty))
            on_click=Callback::new(move |_| actions.apply_workspace(read_only))>"Apply to editor tabs"</Button>
    }
}

#[component]
pub fn DiagnosticOverlay(
    actions: EditorIntelligence,
    textarea: NodeRef<leptos::html::Textarea>,
    ready: RwSignal<bool>,
    layout_revision: RwSignal<u64>,
) -> impl IntoView {
    use wasm_bindgen::JsCast;
    let marks = RwSignal::new(Vec::<(f64, f64, f64, bool)>::new());
    Effect::new(move |_| {
        ready.track();
        layout_revision.track();
        actions.editor.source_revision();
        actions.editor.preparation_revision();
        let diagnostics = actions.diagnostics.get();
        marks.set(Vec::new());
        if diagnostics.is_empty() || !actions.diagnostics_current() {
            return;
        }
        let scope = actions.editor.presentation_scope();
        let revision = actions.editor.source_revision();
        leptos::leptos_dom::helpers::queue_microtask(move || {
            if actions.editor.presentation_scope() != scope
                || actions.editor.source_revision() != revision
                || !actions.diagnostics_current()
            {
                return;
            }
            let Some(Some(textarea)) = textarea.try_get_untracked() else {
                return;
            };
            let Some(parent) = textarea.parent_element() else {
                return;
            };
            let Ok(rows) = parent.query_selector_all(".editor-source-line[data-line]") else {
                return;
            };
            let bounds = parent.get_bounding_client_rect();
            let scroll = crate::viewport::editor_scroll(&textarea);
            let mut next = Vec::new();
            for index in 0..rows.length() {
                let Some(row) = rows
                    .item(index)
                    .and_then(|row| row.dyn_into::<web_sys::Element>().ok())
                else {
                    continue;
                };
                let Some(line) = row
                    .get_attribute("data-line")
                    .and_then(|line| line.parse::<u32>().ok())
                    .and_then(|line| line.checked_sub(1))
                else {
                    continue;
                };
                let length = row
                    .get_attribute("data-paint-length")
                    .and_then(|length| length.parse::<u32>().ok())
                    .unwrap_or_else(|| {
                        u32::try_from(
                            row.text_content()
                                .unwrap_or_default()
                                .encode_utf16()
                                .count(),
                        )
                        .unwrap_or(u32::MAX)
                    });
                for diagnostic in diagnostics.iter().filter(|diagnostic| {
                    diagnostic.range.start.line <= line && line <= diagnostic.range.end.line
                }) {
                    let start = if diagnostic.range.start.line == line {
                        diagnostic.range.start.character
                    } else {
                        0
                    };
                    let end = if diagnostic.range.end.line == line {
                        diagnostic.range.end.character
                    } else {
                        length
                    };
                    for range in
                        super::editor_paint::ranges(&row, start.min(length)..end.min(length))
                    {
                        if let Some(rects) = range.get_client_rects() {
                            for rect in (0..rects.length()).filter_map(|index| rects.item(index)) {
                                if next.len() >= 8192 {
                                    break;
                                }
                                next.push((
                                    rect.left() - bounds.left() + scroll.scroll_left(),
                                    rect.bottom() - bounds.top() + scroll.scroll_top() - 2.0,
                                    rect.width().max(3.0),
                                    diagnostic.severity == 1,
                                ));
                            }
                        }
                    }
                }
            }
            if actions.editor.presentation_scope() == scope
                && actions.editor.source_revision() == revision
                && actions.diagnostics_current()
            {
                marks.set(next);
            }
        });
    });
    view! {
        <div class="editor-bracket-layer" aria-hidden="true">
            <Show when=move || { actions.diagnostics.track(); actions.diagnostics_current() }>
                {move || marks.get().into_iter().map(|(left, top, width, error)| view! {
                    <span class="editor-diagnostic" class:editor-diagnostic-error=error style=format!("left:{left}px;top:{top}px;width:{width}px") />
                }).collect_view()}
            </Show>
        </div>
    }
}
