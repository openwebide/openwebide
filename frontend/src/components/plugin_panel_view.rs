//! Declarative UI renderer shared by host panels and the authoring playground.
//! Documents are validated before mounting; text never becomes browser markup.
use std::collections::BTreeMap;

use leptos::prelude::*;
use openwebide_core::plugins::panels::*;
use serde_json::Value;

use super::ui::{
    Button, CheckboxField, FormField, FormNotice, FormSection, InlineActions, NoticeTone, TextArea,
    TextInput,
};

#[derive(Clone, Debug, PartialEq)]
pub struct PanelSubmission {
    pub action: String,
    pub values: BTreeMap<String, Value>,
}

/// Mount a single revision. The caller keys this component by instance/revision,
/// keeping unsent form values in memory only until that view is replaced.
#[component]
pub fn PluginPanelDocument(
    panel: PanelDefinition,
    document: PanelDocument,
    busy: Signal<bool>,
    on_action: Callback<PanelSubmission>,
    #[prop(default = Callback::new(|()| {}))] on_edit: Callback<()>,
) -> impl IntoView {
    match validate_document(&panel, &document) {
        Ok(()) => view! {
            <div class="plugin-panel-document">{render_blocks(document.blocks, busy, on_action, on_edit)}</div>
        }.into_any(),
        Err(error) => view! { <FormNotice tone=NoticeTone::Error>{error}</FormNotice> }.into_any(),
    }
}

fn render_blocks(
    blocks: Vec<Block>,
    busy: Signal<bool>,
    action: Callback<PanelSubmission>,
    edit: Callback<()>,
) -> Vec<AnyView> {
    blocks
        .into_iter()
        .map(|block| render_block(block, busy, action, edit))
        .collect()
}

fn render_block(
    block: Block,
    busy: Signal<bool>,
    action: Callback<PanelSubmission>,
    edit: Callback<()>,
) -> AnyView {
    match block {
        Block::Text { text } => view! { <p class="plugin-panel-text">{text}</p> }.into_any(),
        Block::Heading { text } => view! { <h3>{text}</h3> }.into_any(),
        Block::Status { text, tone } => {
            let tone = match tone {
                Tone::Neutral => NoticeTone::Info, Tone::Success => NoticeTone::Success,
                Tone::Warning => NoticeTone::Warning, Tone::Error => NoticeTone::Error,
            };
            view! { <FormNotice tone>{text}</FormNotice> }.into_any()
        }
        Block::Group { title, blocks } => {
            let children = render_blocks(blocks, busy, action, edit);
            if let Some(title) = title {
                view! { <FormSection title>{children}</FormSection> }.into_any()
            } else {
                view! { <div class="ui-section-content">{children}</div> }.into_any()
            }
        }
        Block::Actions { buttons } => {
            let buttons = buttons.into_iter().map(|button| action_button(button, busy, action)).collect::<Vec<_>>();
            view! { <InlineActions>{buttons}</InlineActions> }.into_any()
        }
        Block::Form { id, fields, submit } => {
            let values = RwSignal::new(fields.iter().map(|field| (field.id.clone(), field.value.clone())).collect::<BTreeMap<_, _>>());
            let controls = fields.into_iter().map(|field| render_field(field, values, busy, edit)).collect::<Vec<_>>();
            let disabled = submit.disabled;
            view! {
                <form class="ui-section-content" data-panel-form=id on:submit=move |event| {
                    event.prevent_default();
                    if disabled || busy.get_untracked() { return; }
                    let mut submitted = values.get_untracked();
                    submitted.extend(submit.values.clone());
                    action.run(PanelSubmission { action: submit.action.clone(), values: submitted });
                }>
                    {controls}
                    <InlineActions><Button button_type="submit" disabled=Signal::derive(move || disabled || busy.get())>{submit.label}</Button></InlineActions>
                </form>
            }.into_any()
        }
        Block::List { items } => view! { <ul>{items.into_iter().map(|item| view! { <li>{item}</li> }).collect::<Vec<_>>()}</ul> }.into_any(),
        Block::Table { columns, rows } => view! {
            <div class="plugin-panel-table"><table>
                <thead><tr>{columns.into_iter().map(|column| view! { <th scope="col">{column}</th> }).collect::<Vec<_>>()}</tr></thead>
                <tbody>{rows.into_iter().map(|row| view! { <tr>{row.into_iter().map(|cell| view! { <td>{cell}</td> }).collect::<Vec<_>>()}</tr> }).collect::<Vec<_>>()}</tbody>
            </table></div>
        }.into_any(),
        Block::Code { text, language } => view! { <pre class="plugin-panel-code" aria-label=language.unwrap_or_else(|| "Code".into())><code>{text}</code></pre> }.into_any(),
        Block::Diff { before, after, path } => {
            let lines = openwebide_core::diff::line_lcs(&before, &after).into_iter().map(|operation| {
                use openwebide_core::diff::LineOp;
                let (class, marker, line) = match operation {
                    LineOp::Equal(line) => ("tui-diff-line", " ", line),
                    LineOp::Insert(line) => ("tui-diff-line add", "+", line),
                    LineOp::Delete(line) => ("tui-diff-line del", "−", line),
                };
                view! { <div class=class><span>{marker}</span>{line.text.to_owned()}</div> }
            }).collect::<Vec<_>>();
            view! { <div class="tui-diff-box"><div class="tui-diff-path">{path.unwrap_or_else(|| "Changes".into())}</div><div class="tui-diff-lines">{lines}</div></div> }.into_any()
        }
        Block::Link { text, url } => view! { <a href=url target="_blank" rel="noopener noreferrer">{text}</a> }.into_any(),
        Block::Divider => view! { <hr/> }.into_any(),
    }
}

fn action_button(
    button: PanelButton,
    busy: Signal<bool>,
    action: Callback<PanelSubmission>,
) -> AnyView {
    let disabled = button.disabled;
    view! {
        <Button disabled=Signal::derive(move || disabled || busy.get()) on_click=Callback::new(move |_| {
            if !disabled && !busy.get_untracked() {
                action.run(PanelSubmission { action: button.action.clone(), values: button.values.clone() });
            }
        })>{button.label}</Button>
    }.into_any()
}

fn render_field(
    field: PanelField,
    values: RwSignal<BTreeMap<String, Value>>,
    busy: Signal<bool>,
    edit: Callback<()>,
) -> AnyView {
    let id = field.id.clone();
    let initial = field.value.clone();
    let update = Callback::new(move |value| {
        edit.run(());
        values.update(|values| {
            values.insert(id.clone(), value);
        });
    });
    let secret = matches!(field.kind, FieldKind::Secret { .. });
    let multiline = matches!(
        field.kind,
        FieldKind::Text {
            multiline: true,
            ..
        }
    );
    match field.kind {
        FieldKind::Text { max_length, .. } | FieldKind::Secret { max_length } => {
            let value = RwSignal::new(initial.as_str().unwrap_or_default().to_owned());
            let change = Callback::new(move |next: String| {
                value.set(next.clone());
                update.run(Value::String(next));
            });
            let maxlength = u32::try_from(max_length).unwrap_or(32768);
            let label = field.label.clone();
            let control = if multiline {
                view! { <TextArea label value on_change=change maxlength required=field.required disabled=busy/> }
                    .into_any()
            } else {
                view! { <TextInput label value on_change=change maxlength required=field.required disabled=busy input_type=if secret {"password"}else{"text"} autocomplete=if secret {"new-password"}else{"off"}/> }
                    .into_any()
            };
            view! { <FormField label=field.label>{control}</FormField> }.into_any()
        }
        FieldKind::Checkbox => {
            let checked = RwSignal::new(initial.as_bool().unwrap_or(false));
            values.update(|values| {
                values.insert(field.id.clone(), Value::Bool(checked.get_untracked()));
            });
            view! { <CheckboxField label=field.label required=field.required checked=checked.into() disabled=busy on_change=Callback::new(move |next| { checked.set(next); update.run(Value::Bool(next)); })/> }.into_any()
        }
        FieldKind::Select { options } => {
            let value = RwSignal::new(initial.as_str().unwrap_or_default().to_owned());
            view! { <FormField label=field.label.clone()>
                <select class="form-input" aria-label=field.label required=field.required disabled=move ||busy.get() prop:value=move ||value.get()
                    on:change=move |event| { let next = event_target_value(&event); value.set(next.clone()); update.run(if next.is_empty() { Value::Null } else { Value::String(next) }); }>
                    <option value="">"Select…"</option>
                    {options.into_iter().map(|option| view! { <option value=option.value>{option.label}</option> }).collect::<Vec<_>>()}
                </select>
            </FormField> }.into_any()
        }
    }
}
