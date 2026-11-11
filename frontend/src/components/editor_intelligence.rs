//! Shared editor controls for installed language-server providers.
mod popups;
use super::{
    modal::Modal,
    ui::{Button, DialogActions, DialogBody, FormField},
};
use crate::state_actions::editor::{EditorIntelligence, IntelligenceCommand};
use leptos::prelude::*;
use openwebide_core::plugins::languages::intelligence::{
    Query,
    actions::ActionReview,
    results::{self, Symbol, Target, TextEdit, WorkspaceEdit, WorkspaceSymbol},
};
pub(super) use popups::{IntelligencePopups, PopupAdapter};

fn title(query: &Query) -> &'static str {
    match query {
        Query::Hover { .. } => "Symbol information",
        Query::Completion { .. } | Query::ResolveCompletion { .. } => "Completions",
        Query::Definition { .. } => "Definitions",
        Query::TypeDefinition { .. } => "Type definitions",
        Query::Implementation { .. } => "Implementations",
        Query::References { .. } => "References",
        Query::Symbols => "Document symbols",
        Query::WorkspaceSymbols { .. } | Query::ResolveSymbol { .. } => "Workspace symbols",
        Query::Rename { .. } => "Review rename",
        Query::Formatting { .. } => "Review formatting",
        Query::RangeFormatting { .. } => "Review selection formatting",
        Query::CodeActions { .. } | Query::ResolveAction { .. } | Query::ReviewAction { .. } => {
            "Code actions"
        }
        Query::ReadResource { .. } => "External source · read-only",
        _ => "Language service",
    }
}

#[component]
fn LanguageServiceFeedback(
    busy: ReadSignal<bool>,
    error: ReadSignal<Option<String>>,
) -> impl IntoView {
    view! {
        <span class="form-hint editor-language-feedback"
            role=move || if error.with(Option::is_some) { "alert" } else { "status" }
            title=move || error.get().unwrap_or_default()>
            {move || error.get().unwrap_or_else(|| if busy.get() { "Analyzing…".into() } else { String::new() })}
        </span>
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
    let symbols_open = RwSignal::new(false);
    let symbol_query = RwSignal::new(String::new());
    let find_symbols = Callback::new(move |()| {
        let query = symbol_query.get_untracked().trim().to_string();
        if query.is_empty() || query.len() > 256 {
            return;
        }
        symbols_open.set(false);
        actions
            .request
            .run(IntelligenceCommand::WorkspaceSymbols(query));
    });
    let close = actions.dismiss;
    view! {
        <Show when=move || actions.available.get()>
            <super::dropdown::ActionMenu aria_label="Code intelligence" icon=super::ui::IconName::Brackets>
                <Show when=move || actions.error.with(Option::is_some)>
                    <p class="form-hint" role="alert">{move || actions.error.get()}</p>
                </Show>
                <Show when=move || actions.primary_available.get()>
                {[
                    ("Symbol information", IntelligenceCommand::Hover, false),
                    ("Complete at cursor", IntelligenceCommand::Completion, true),
                    ("Go to definition", IntelligenceCommand::Definition, false),
                    ("Go to type definition", IntelligenceCommand::TypeDefinition, false),
                    ("Go to implementation", IntelligenceCommand::Implementation, false),
                    ("Find references", IntelligenceCommand::References, false),
                    ("Document symbols", IntelligenceCommand::Symbols, false),
                    ("Format document", IntelligenceCommand::Formatting, true),
                    ("Format selection", IntelligenceCommand::RangeFormatting, true),
                    ("Code actions", IntelligenceCommand::CodeActions, true),
                ].into_iter().map(move |(label, command, writes)| view! {
                    <button type="button" role="menuitem" class="ui-dropdown-item recent-item"
                        disabled=move || actions.busy.get() || (writes && read_only.get())
                        on:click=move |_| actions.request.run(command.clone())>{label}</button>
                }).collect_view()}
                <button type="button" role="menuitem" class="ui-dropdown-item recent-item"
                    disabled=move || actions.busy.get() on:click=move |_| symbols_open.set(true)>"Find workspace symbol…"</button>
                <button type="button" role="menuitem" class="ui-dropdown-item recent-item"
                    disabled=move || read_only.get() || actions.busy.get()
                    on:click=move |_| { name.set(String::new()); rename.set(true); }>"Rename symbol…"</button>
                </Show>
                <button type="button" role="menuitem" class="ui-dropdown-item recent-item" title="F8"
                    disabled=move || actions.diagnostics.with(Vec::is_empty) on:click=move |_| { actions.navigate_next_diagnostic(false); }>"Next problem"</button>
                <button type="button" role="menuitem" class="ui-dropdown-item recent-item" title="Shift+F8"
                    disabled=move || actions.diagnostics.with(Vec::is_empty) on:click=move |_| { actions.navigate_next_diagnostic(true); }>"Previous problem"</button>

                <button type="button" role="menuitem" class="ui-dropdown-item recent-item"
                    on:click=move |_| problems.set(true)>"Problems ("{move || actions.diagnostics.with(Vec::len)}")"</button>
            </super::dropdown::ActionMenu>
            <LanguageServiceFeedback busy=actions.busy.read_only() error=actions.error.read_only() />
        </Show>
        <Show when=move || symbols_open.get()>
            <Modal title=Signal::derive(|| "Find workspace symbol".to_string()) on_close=Callback::new(move |()| symbols_open.set(false))>
                <DialogBody><FormField label="Symbol name">
                    <input type="search" class="form-input" maxlength="256" prop:value=move || symbol_query.get()
                        on:input=move |event| symbol_query.set(event_target_value(&event))
                        on:keydown=move |event: web_sys::KeyboardEvent| { if event.key()=="Enter" { event.prevent_default(); find_symbols.run(()); } } />
                </FormField></DialogBody>
                <DialogActions><Button disabled=Signal::derive(move || symbol_query.with(|query| query.trim().is_empty() || query.len() > 256))
                    on_click=Callback::new(move |_| find_symbols.run(()))>"Find symbols"</Button></DialogActions>
            </Modal>
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
                    <Show when=move || actions.provider_diagnostics.with(Vec::is_empty)>
                        <p class="form-hint" role="status">"Waiting for analysis…"</p>
                    </Show>
                    {move || actions.provider_diagnostics.get().into_iter().map(move |report| {
                        let primary = report.provider.definition.features.language_server.as_ref().is_some_and(|server|server.role.is_language());
                        let namespace = report.provider.namespace;
                        let label = format!("{} · {}", report.provider.plugin_label, report.provider.definition.name);
                        let status = report.error.unwrap_or_else(|| if report.ready {
                            if report.diagnostics.is_empty() { "No reported problems." } else { "Analysis ready." }
                        } else { "Waiting for analysis…" }.to_string());
                        view! { <section aria-label=label.clone()>
                            <p><strong>{label.clone()}</strong></p><p class="form-hint" role="status">{status}</p>
                            {report.diagnostics.into_iter().map(move |diagnostic| {
                                let provider = namespace.clone();
                                let location = format!("{}:{}", diagnostic.range.start.line + 1, diagnostic.range.start.character + 1);
                                view! { <div>
                                    <button type="button" class="ui-dropdown-item" on:click=move |_| {
                                        actions.navigate_diagnostic(diagnostic.range);
                                        problems.set(false);
                                    }><span>{location}" "{diagnostic.message}</span></button>
                                    <Button disabled=Signal::derive(move || read_only.get() || actions.busy.get() || !actions.diagnostics_current())
                                        on_click=Callback::new(move |_| {
                                            let query = Query::CodeActions { range: diagnostic.range };
                                            if primary {
                                                actions.request.run(IntelligenceCommand::Query(query));
                                            } else {
                                                actions.request.run(IntelligenceCommand::ProviderQuery {provider:provider.clone(), query});
                                            }
                                            problems.set(false);
                                        })>"Find fixes"</Button>
                                </div> }
                            }).collect_view()}
                        </section> }
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
        Query::Definition { .. }
        | Query::TypeDefinition { .. }
        | Query::Implementation { .. }
        | Query::References { .. }
        | Query::ResolveSymbol { .. } => {
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
        Query::WorkspaceSymbols { .. } => {
            let symbols =
                serde_json::from_value::<Vec<WorkspaceSymbol>>(result).unwrap_or_default();
            let empty = symbols.is_empty();
            view! {
                <Show when=move || empty><p class="form-hint">"No matching symbols."</p></Show>
                {symbols.into_iter().map(move |symbol| {
                    let label = symbol.container.as_ref().map_or_else(||symbol.name.clone(),|container|format!("{} · {container}",symbol.name));
                    view! { <button type="button" class="ui-dropdown-item" disabled=move || actions.busy.get()
                        on:click=move |_| {
                            if let Some(target)=&symbol.target { actions.navigate.run(target.clone()); }
                            else if let Some(id)=&symbol.id { actions.request.run(IntelligenceCommand::Query(Query::ResolveSymbol {id:id.clone()})); }
                        }>{label}</button> }
                }).collect_view()}
            }.into_any()
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
                        on:click=move |_| actions.request.run(IntelligenceCommand::Query(if completion { Query::ResolveCompletion { id: id.clone() } } else { Query::ReviewAction { id: id.clone() } }))>{label}</button> })
                }).collect_view()}
            }.into_any()
        }
        Query::ResolveCompletion { .. } => {
            let label = result["label"].as_str().unwrap_or("Completion").to_string();
            let detail = result["detail"].as_str().unwrap_or_default().to_string();
            view! { <p>{label}</p><pre>{detail}</pre><Button disabled=read_only on_click=Callback::new(move |_| actions.apply_completion(&result))>"Insert completion"</Button> }.into_any()
        }
        Query::Formatting { .. } | Query::RangeFormatting { .. } => {
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
        Query::ReviewAction {..} => {
            match serde_json::from_value::<ActionReview>(result) {
                Ok(review)=>view! { <WorkspaceEditReview actions edit=review.edit.clone() read_only reviewed_action=review /> }.into_any(),
                Err(_)=>view! { <p role="alert">"Invalid code action review."</p> }.into_any(),
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
    let filter = RwSignal::new(String::new());
    let targets = StoredValue::new(targets);
    view! {
        <input type="search" class="form-input" aria-label="Filter symbols and locations" placeholder="Filter results"
            prop:value=move || filter.get() on:input=move |event| filter.set(event_target_value(&event)) />
        <Show when=move || empty><p class="form-hint">"No results."</p></Show>
        {move || { let query=filter.get().to_lowercase(); targets.get_value().into_iter().filter(|(label,_)|label.to_lowercase().contains(&query)).map(move |(label, target)| view! {
            <button type="button" class="ui-dropdown-item" on:click=move |_| actions.navigate.run(target.clone())>{label}</button>
        }).collect_view() }}
    }.into_any()
}

#[component]
fn WorkspaceEditReview(
    actions: EditorIntelligence,
    edit: WorkspaceEdit,
    read_only: Signal<bool>,
    #[prop(optional)] reviewed_action: Option<ActionReview>,
) -> impl IntoView {
    let edit = StoredValue::new(edit);
    let reviewed_action = StoredValue::new(reviewed_action);
    Effect::new(move |_| {
        // The declared review is immutable. Preparing it must not subscribe this
        // view effect to incidental editor reads and restart its own work.
        untrack(|| {
            if let Some(review) = reviewed_action.get_value() {
                actions.prepare_action(review);
            } else {
                actions.prepare_workspace(edit.get_value());
            }
        });
    });
    view! {
        {move || actions.resources.get().map(|review| {
            let approved = review.clone();
            view! {
            <p class="form-hint">"Approve the complete file operation. Text edits remain unsaved drafts in editor tabs."</p>
            {review.command().map(|command|view! {
                <p class="form-hint">"After the complete edit, this server command runs once. It can change files or other project state."</p>
                <h3>{command.name.clone()}</h3>
                <pre>{serde_json::to_string_pretty(&command.arguments).unwrap_or_default()}</pre>
            })}
            <ul>{review.operations.iter().cloned().map(|operation| view! { <li>{operation}</li> }).collect_view()}</ul>
            {review.previews.iter().cloned().map(super::editor::render_inline_diff).collect_view()}
            <Button disabled=Signal::derive(move || read_only.get() || actions.busy.get())
                on_click=Callback::new(move |_| actions.apply_resource_workspace(approved.clone(), read_only))>{if review.command().is_some() {"Apply edits and run command"} else {"Apply file operations and editor edits"}}</Button>
        }})}
        <Show when=move || actions.resources.get().is_none()>
        <p class="form-hint">"Apply to open tabs as unsaved changes. Each file keeps its undo history; save when ready."</p>
        {move || actions.review.get().map(|review| {
            let approved = review.clone();
            let empty = review.files.is_empty();
            view! {
                {review.files.iter().map(|file| view! {
                    <h3>{file.path.clone()}</h3>
                    {super::editor::render_inline_diff(openwebide_core::FileDiff { path: file.path.clone(), old: Some(file.before.as_ref().clone()), new: file.after.text().to_string(), old_unavailable: false, backup_path: None })}
                }).collect_view()}
                <Button disabled=Signal::derive(move || read_only.get() || actions.busy.get() || empty)
                    on_click=Callback::new(move |_| actions.apply_workspace(approved.clone(), read_only))>"Apply to editor tabs"</Button>
            }
        })}
        </Show>
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

#[cfg(test)]
mod feedback_tests {
    use super::*;
    use wasm_bindgen::JsCast;

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn background_language_feedback_preserves_editor_geometry_at_narrow_and_wide_widths() {
        let document = web_sys::window().unwrap().document().unwrap();
        let owner = Owner::new();
        let (busy, error) = owner.with(|| (RwSignal::new(false), RwSignal::new(None::<String>)));
        let root: web_sys::HtmlElement = document.create_element("div").unwrap().unchecked_into();
        document.body().unwrap().append_child(&root).unwrap();
        let handle = owner.with(|| leptos::mount::mount_to(root.clone(), move || view! {
            <style>{include_str!("../../styles.css")}</style>
            <div class="editor" style="height:320px">
                <div class="editor-header">
                    <div class="editor-file-tabs">"example.cs"</div>
                    <div class="editor-header-actions"><button class="btn">"Save"</button></div>
                    <LanguageServiceFeedback busy=busy.read_only() error=error.read_only() />
                </div>
                <textarea aria-label="Source" style="flex:1;min-height:0">"class Example {}"</textarea>
            </div>
        }));
        let input = root.query_selector("textarea").unwrap().unwrap();
        let feedback = root
            .query_selector(".editor-language-feedback")
            .unwrap()
            .unwrap();
        let mut stable = true;
        let mut readable = true;
        for width in [260, 420, 840] {
            root.style()
                .set_property("width", &format!("{width}px"))
                .unwrap();
            busy.set(false);
            error.set(None);
            crate::util::sleep_ms(0).await;
            let before = input.get_bounding_client_rect();
            for (analyzing, message) in [
                (true, None),
                (
                    false,
                    Some("The language service is unavailable.".to_string()),
                ),
                (
                    false,
                    Some(
                        "Reconnect this folder using Grant folder access in the file tree. "
                            .repeat(8),
                    ),
                ),
                (false, None),
            ] {
                busy.set(analyzing);
                error.set(message.clone());
                crate::util::sleep_ms(0).await;
                let after = input.get_bounding_client_rect();
                stable &= (before.top() - after.top()).abs() < 0.5
                    && (before.height() - after.height()).abs() < 0.5;
                if let Some(message) = message {
                    readable &= feedback.text_content().as_deref() == Some(message.as_str())
                        && feedback.get_attribute("title").as_deref() == Some(message.as_str())
                        && feedback.get_attribute("role").as_deref() == Some("alert");
                }
            }
        }
        drop(handle);
        root.remove();
        owner.cleanup();
        assert!(
            stable,
            "Language feedback moved the editor during background updates"
        );
        assert!(
            readable,
            "Language errors must retain their complete accessible text"
        );
    }
}
