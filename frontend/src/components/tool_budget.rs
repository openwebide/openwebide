//! Shared schema inventory and server selection controls.
use super::{
    dropdown::{DropdownSelect, SelectOption},
    ui::{CheckboxField, FormField, FormSection},
};
use leptos::prelude::*;
use openwebide_core::{ToolDefinition, ToolSelection};

pub fn catalog() -> Vec<ToolDefinition> {
    let mut tools = openwebide_agent::tools::ToolName::ALL
        .iter()
        .map(|tool| tool.definition())
        .collect::<Vec<_>>();
    tools.extend(openwebide_agent::plugins::management_tools::definitions());
    tools.push(openwebide_agent::tasks::executor::definition());
    tools
}

pub fn budget(selection: &ToolSelection, enabled: bool, context: Option<usize>) -> String {
    let mut tools = catalog();
    selection.apply(&mut tools);
    if !enabled {
        tools.clear();
    }
    let tokens = openwebide_core::context::tool_schema_tokens(&tools);
    let share = context
        .filter(|limit| *limit > 0)
        .map_or_else(String::new, |limit| {
            format!(
                " · {:.1}% of {limit} context tokens",
                tokens as f64 * 100.0 / limit as f64
            )
        });
    format!(
        "{} {} · ~{tokens} schema tokens{share}",
        tools.len(),
        if tools.len() == 1 { "tool" } else { "tools" }
    )
}

#[component]
pub fn ToolBudget(
    selection: RwSignal<ToolSelection>,
    on_edit: Callback<()>,
    #[prop(default = Signal::derive(|| false))] disabled: Signal<bool>,
) -> impl IntoView {
    let mode = Signal::derive(move || {
        match selection.get() {
            ToolSelection::All => "all",
            ToolSelection::Selected(_) => "selected",
            ToolSelection::ChatOnly => "chat",
        }
        .to_string()
    });
    let definitions = catalog();
    view! {
        <FormSection title="Available tools" description="Choose what this server's models receive. Project, model and host capabilities may reduce this set. Approval rules still apply.">
            <FormField label="Tool selection"><DropdownSelect label="Tool selection" disabled=disabled value=mode options=Signal::derive(|| vec![SelectOption::new("all", "All tools"), SelectOption::new("selected", "Selected tools"), SelectOption::new("chat", "Chat only")]) on_change=Callback::new(move |value: String| {
                on_edit.run(());
                selection.set(match value.as_str() { "selected" => ToolSelection::Selected(catalog().into_iter().map(|tool| tool.name).collect()), "chat" => ToolSelection::ChatOnly, _ => ToolSelection::All });
            }) /></FormField>
            <p class="form-hint tool-budget" aria-live="polite">{move || budget(&selection.get(), true, None)}</p>
            <p class="form-hint">"Conservative estimates; provider tokenization varies. Costs include parameters. A model's context details show the actual request breakdown after sending."</p>
            <Show when=move || matches!(selection.get(), ToolSelection::Selected(_))>
                {definitions.clone().into_iter().map(|tool| {
                    let name = StoredValue::new(tool.name.clone());
                    let label = format!("{} · ~{} tokens", tool.name, openwebide_core::context::tool_schema_tokens(std::slice::from_ref(&tool)));
                    view! { <CheckboxField label=label disabled=disabled checked=Signal::derive(move || selection.with(|selection| selection.allows(&name.get_value()))) on_change=Callback::new(move |checked| {
                        on_edit.run(());
                        selection.update(|selection| if let ToolSelection::Selected(names) = selection {
                            names.retain(|selected| *selected != name.get_value());
                            if checked { names.push(name.get_value()); }
                        });
                    }) /> }
                }).collect_view()}
                {move || selection.with(|policy| if let ToolSelection::Selected(names) = policy {
                    names.iter().filter(|name| !catalog().iter().any(|tool| tool.name == **name)).cloned().map(|name| {
                        let label = format!("{name} (unavailable)");
                        view! { <CheckboxField label=label disabled=disabled checked=Signal::derive(|| true) on_change=Callback::new(move |_| { on_edit.run(()); selection.update(|selection| if let ToolSelection::Selected(names) = selection { names.retain(|selected| *selected != name); }); }) /> }
                    }).collect_view()
                } else { Vec::new().collect_view() })}
            </Show>
        </FormSection>
    }
}
