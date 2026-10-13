//! Docs-only UI. No API client, bridge, plugin runtime or persisted browser state.
use super::{
    panel_builder::VisualPanelEditor,
    plugin_panel_view::{PanelSubmission, PluginPanelDocument},
    ui::{
        Button, FormField, FormNotice, Icon, IconButton, IconName, InlineActions, NoticeTone,
        TextArea,
    },
};
use crate::panel_builder::History;
use crate::panel_playground::{MAX_SOURCE_BYTES, Playground, gallery};
use leptos::prelude::*;
use openwebide_core::plugins::panels::{MAX_DOCUMENT_BYTES, PanelInput, approval_values};

#[derive(Clone)]
struct Pending {
    example: Playground,
    input: PanelInput,
    generation: u64,
}

#[component]
pub fn PanelPlayground() -> impl IntoView {
    let examples = StoredValue::new(gallery());
    let draft = RwSignal::new(examples.with_value(|examples| examples[0].1.json()));
    let history = RwSignal::new(History::new(draft.get_untracked()));
    let generation = RwSignal::new(0u64);
    let parsed = Memo::new(move |_| Playground::parse(&draft.get()));
    let selected = Signal::derive(move || {
        parsed
            .get()
            .ok()
            .and_then(|panel| {
                examples.with_value(|examples| {
                    examples.iter().position(|(_, example)| *example == panel)
                })
            })
            .map_or_else(|| "custom".into(), |index| index.to_string())
    });
    let custom_response = RwSignal::new(None::<String>);
    let response = Signal::derive(move || {
        custom_response.get().unwrap_or_else(|| {
            parsed
                .get()
                .map_or_else(|_| String::new(), |example| example.response())
        })
    });
    let trace = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let pending = RwSignal::new(None::<Pending>);
    let theme = RwSignal::new("dark".to_owned());
    let phone = RwSignal::new(false);
    Effect::new(move || {
        let value = theme.get();
        if let Some(document) = web_sys::window().and_then(|window| window.document())
            && let Some(root) = document.document_element()
        {
            let _ = root.set_attribute("data-theme", &value);
        }
    });
    let publish = Callback::new(move |source: String| {
        if source == draft.get_untracked() {
            return;
        }
        draft.set(source);
        generation.update(|value| *value += 1);
        pending.set(None);
        error.set(None);
        custom_response.set(None);
    });
    let edit = Callback::new(move |source: String| {
        history.update(|history| history.record(source.clone()));
        publish.run(source);
    });
    let apply = Callback::new(move |request: Pending| {
        if generation.get_untracked() != request.generation {
            error.set(Some("The document changed. Try the action again.".into()));
            return;
        }
        match request
            .example
            .simulate(&request.input, &response.get_untracked(), true)
        {
            Ok(example) => {
                edit.run(example.json());
            }
            Err(message) => error.set(Some(message)),
        }
    });
    let action = Callback::new(move |submission: PanelSubmission| {
        let Ok(example) = parsed.get_untracked() else {
            return;
        };
        match example.input(submission.action, submission.values) {
            Err(message) => error.set(Some(message)),
            Ok(input) => {
                let requires_approval = example.panel.actions.iter().any(|action| {
                    Some(action.id.as_str()) == input.action.as_deref() && action.requires_approval
                });
                let mut redacted = input.clone();
                redacted.values = approval_values(&example.document, &input.values);
                trace.set(serde_json::to_string_pretty(&redacted).expect("serializable input"));
                let request = Pending {
                    example,
                    input,
                    generation: generation.get_untracked(),
                };
                if requires_approval {
                    pending.set(Some(request));
                } else {
                    apply.run(request);
                }
            }
        }
    });
    let valid = Signal::derive(move || parsed.get().is_ok());
    let download = Callback::new(move |rust: bool| {
        let Ok(example) = parsed.get_untracked() else {
            return;
        };
        let result = if rust {
            example.rust().and_then(|text| save("lib.rs", &text))
        } else {
            save(
                "panel.json",
                &serde_json::to_string_pretty(&example.document).expect("serializable document"),
            )
        };
        if let Err(message) = result {
            error.set(Some(message));
        }
    });
    view! {
        <main class="panel-playground">
            <header class="playground-heading"><div><h1>"Panel playground"</h1><p>"Build a panel with the same components and validation used by Open WebIDE."</p></div>
                <InlineActions>
                    <Button disabled=Signal::derive(move ||!valid.get()) on_click=Callback::new(move |_|download.run(false))><Icon name=IconName::Download/>"Export JSON"</Button>
                    <Button disabled=Signal::derive(move ||!valid.get()) on_click=Callback::new(move |_|download.run(true))><Icon name=IconName::Code/>"Export Rust"</Button>
                </InlineActions>
            </header>
            <div class="playground-toolbar">
                <Button on_click=Callback::new(move |_|edit.run(Playground::blank().json()))><Icon name=IconName::Plus/>"New panel"</Button>
                <FormField label="Component example"><select class="form-input" aria-label="Component example" prop:value=move ||selected.get() on:change=move |event| {
                    if let Ok(index)=event_target_value(&event).parse::<usize>()
                        && let Some(source)=examples.with_value(|examples|examples.get(index).map(|(_,example)|example.json())) {
                        edit.run(source);trace.set(String::new());
                    }
                }><option value="custom" disabled>"Custom panel"</option>{examples.with_value(|examples|examples.iter().enumerate().map(|(index,(name,_))|view! {<option value=index.to_string()>{*name}</option>}).collect::<Vec<_>>())}</select></FormField>
                <FormField label="Theme"><select class="form-input" aria-label="Theme" prop:value=move ||theme.get() on:change=move |event|theme.set(event_target_value(&event))><option value="dark">"Dark"</option><option value="light">"Light"</option></select></FormField>
                <FormField label="Preview width"><select class="form-input" aria-label="Preview width" on:change=move |event|phone.set(event_target_value(&event)=="phone")><option value="desktop">"Desktop · 800 px"</option><option value="phone">"Phone · 375 px"</option></select></FormField>
                <IconButton label="Format JSON" disabled=Signal::derive(move ||!valid.get()) on_click=Callback::new(move |_| {if let Ok(example)=parsed.get_untracked() {edit.run(example.json());}})><Icon name=IconName::Braces/></IconButton>
                <IconButton label="Undo" disabled=Signal::derive(move ||!history.with(History::can_undo)) on_click=Callback::new(move |_| {
                    let mut source=None;history.update(|history|source=history.undo());if let Some(source)=source {publish.run(source);}
                })><Icon name=IconName::Undo2/></IconButton>
                <IconButton label="Redo" disabled=Signal::derive(move ||!history.with(History::can_redo)) on_click=Callback::new(move |_| {
                    let mut source=None;history.update(|history|source=history.redo());if let Some(source)=source {publish.run(source);}
                })><Icon name=IconName::Redo2/></IconButton>
            </div>
            <div class="playground-columns">
                <section class="playground-editor" aria-label="Panel source">
                    <VisualPanelEditor source=draft.into() generation=generation.into() on_change=edit/>
                    <div class="playground-validation" role="status" aria-live="polite">{move ||match parsed.get() {Ok(example)=>format!("Valid · UI schema {} · {} top-level blocks",example.document.schema_version,example.document.blocks.len()),Err(message)=>format!("Invalid: {message}")}}</div>
                    <details><summary>"Source JSON"</summary>
                        <TextArea label="Panel JSON" value=draft on_change=edit rows=26 maxlength=u32::try_from(MAX_SOURCE_BYTES).unwrap_or(u32::MAX)/>
                    </details>
                    <details><summary>"Simulated response"</summary><p>"Actions use this replacement document. It must return the next revision."</p>
                        <TextArea label="Simulated response JSON" value=response on_change=Callback::new(move |value|custom_response.set(Some(value))) rows=12 maxlength=u32::try_from(MAX_DOCUMENT_BYTES).unwrap_or(u32::MAX)/>
                    </details>
                </section>
                <section class="playground-preview" aria-label="Live preview"><h2>"Live preview"</h2>
                    <p class="playground-hint">"Interactions are simulated here. No plugin or host capability executes."</p>
                    <Show when=move ||error.get().is_some()><FormNotice tone=NoticeTone::Error>{move ||error.get().unwrap_or_default()}</FormNotice></Show>
                    <Show when=move ||pending.get().is_some()><FormNotice tone=NoticeTone::Info>"This action requires approval. Inspect its inputs below."</FormNotice><InlineActions>
                        <Button on_click=Callback::new(move |_| {if let Some(request)=pending.get_untracked() {apply.run(request);}})>"Approve simulation"</Button>
                        <Button on_click=Callback::new(move |_|pending.set(None))>"Cancel"</Button>
                    </InlineActions></Show>
                    <div class="playground-preview-frame" style=move ||format!("max-width:{}px",if phone.get(){375}else{800})>
                        <For each={move ||parsed.get().ok().into_iter().map(|example|(generation.get(),example)).collect::<Vec<_>>()} key=|(generation,_)|*generation children={move |(_,example)|view! {<PluginPanelDocument panel=example.panel document=example.document busy=Signal::derive(||false) on_action=action/>}}/>
                        <Show when=move ||parsed.get().is_err()><FormNotice tone=NoticeTone::Warning>"Fix the JSON to restore the preview."</FormNotice></Show>
                    </div>
                    <details open><summary>"Action input"</summary><pre class="playground-action-input">{move ||trace.get()}</pre></details>
                </section>
            </div>
            <footer>"Drafts stay in this page’s memory. Export your document or Rust example to keep your work."</footer>
        </main>
    }
}
fn save(name: &str, text: &str) -> Result<(), String> {
    use wasm_bindgen::JsCast;
    let window = web_sys::window().ok_or("Browser unavailable")?;
    let document = window.document().ok_or("Document unavailable")?;
    let parts = js_sys::Array::new();
    parts.push(&wasm_bindgen::JsValue::from_str(text));
    let blob =
        web_sys::Blob::new_with_str_sequence(&parts).map_err(|_| "Could not prepare the export")?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)
        .map_err(|_| "Could not prepare the download")?;
    let result = (|| {
        let anchor = document
            .create_element("a")
            .map_err(|_| "Could not create the download")?
            .dyn_into::<web_sys::HtmlAnchorElement>()
            .map_err(|_| "Download unavailable")?;
        anchor.set_href(&url);
        anchor.set_download(name);
        document
            .body()
            .ok_or("Document body unavailable")?
            .append_child(&anchor)
            .map_err(|_| "Could not attach the download")?;
        anchor.click();
        anchor.remove();
        Ok(())
    })();
    leptos::task::spawn_local(async move {
        crate::util::sleep_ms(1000).await;
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    result
}
