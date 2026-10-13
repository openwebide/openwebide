//! Schema-shaped property controls; applying a draft uses the shared transaction facade.
use super::ui::{
    Button, CheckboxField, FormField, FormNotice, Icon, IconButton, IconName, InlineActions,
    NoticeTone, TextArea, TextInput,
};
use crate::{
    panel_builder::{self, BlockPath, Edit},
    panel_playground::Playground,
};
use leptos::prelude::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Name(String),
    Index(usize),
}
type Path = Vec<Key>;
#[derive(Clone, Copy)]
struct Properties {
    draft: RwSignal<Value>,
    errors: RwSignal<BTreeMap<Path, String>>,
    original: StoredValue<Playground>,
}
impl Properties {
    fn value(self, path: &[Key]) -> Value {
        self.draft
            .with(|root| get(root, path).cloned().unwrap_or(Value::Null))
    }
    fn set(self, path: &[Key], value: Value) {
        self.draft.update(|root| {
            if let Some(target) = get_mut(root, path) {
                *target = value;
            }
        });
    }
    fn error(self, path: Path, message: Option<String>) {
        self.errors.update(|errors| {
            if let Some(message) = message {
                errors.insert(path, message);
            } else {
                errors.remove(&path);
            }
        });
    }
}
fn get<'a>(mut value: &'a Value, path: &[Key]) -> Option<&'a Value> {
    for key in path {
        value = match key {
            Key::Name(key) => value.get(key)?,
            Key::Index(index) => value.get(*index)?,
        };
    }
    Some(value)
}
fn get_mut<'a>(mut value: &'a mut Value, path: &[Key]) -> Option<&'a mut Value> {
    for key in path {
        value = match key {
            Key::Name(key) => value.get_mut(key)?,
            Key::Index(index) => value.get_mut(*index)?,
        };
    }
    Some(value)
}
fn name(path: &[Key]) -> String {
    path.iter()
        .map(|key| match key {
            Key::Name(name) => name.replace('_', " "),
            Key::Index(index) => (index + 1).to_string(),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}
fn child(path: &[Key], key: &str) -> Path {
    let mut result = path.to_vec();
    result.push(Key::Name(key.into()));
    result
}
fn last(path: &[Key]) -> &str {
    match path.last() {
        Some(Key::Name(name)) => name,
        _ => "",
    }
}

#[component]
pub fn PanelProperties(
    panel: Playground,
    selected: Option<BlockPath>,
    on_apply: Callback<Edit>,
) -> impl IntoView {
    let initial = selected.as_ref().map_or_else(
        || serde_json::to_value(&panel.panel).expect("panel definition"),
        |path| {
            serde_json::to_value(
                panel_builder::block(&panel.document.blocks, path).expect("selected block"),
            )
            .expect("block")
        },
    );
    let properties = Properties {
        draft: RwSignal::new(initial),
        errors: RwSignal::new(BTreeMap::new()),
        original: StoredValue::new(panel),
    };
    let selection = StoredValue::new(selected);
    let candidate = Memo::new(move |_| -> Result<Edit, String> {
        let errors = properties.errors.get();
        if let Some(message) = errors.values().next() {
            return Err(message.clone());
        }
        let value = properties.draft.get();
        let operation = match selection.get_value() {
            Some(path) => Edit::Replace {
                path,
                block: serde_json::from_value(value).map_err(|error| error.to_string())?,
            },
            None => {
                Edit::Definition(serde_json::from_value(value).map_err(|error| error.to_string())?)
            }
        };
        properties
            .original
            .with_value(|original| panel_builder::edit(original, operation.clone()))?;
        Ok(operation)
    });
    view! {
        <section class="panel-builder-properties" aria-label="Properties">
            <h3>{if selection.with_value(Option::is_some) {"Block properties"} else {"Panel properties"}}</h3>
            <p>"Apply changes to update the preview. Layout is edited on the canvas."</p>
            {property(vec![],properties)}
            <Show when=move ||candidate.get().is_err()><FormNotice tone=NoticeTone::Warning>{move ||candidate.get().err().unwrap_or_default()}</FormNotice></Show>
            <Button disabled=Signal::derive(move ||candidate.get().is_err()) on_click=Callback::new(move |_|{if let Ok(operation)=candidate.get_untracked() {on_apply.run(operation);}})>"Apply properties"</Button>
        </section>
    }
}

fn property(path: Path, properties: Properties) -> AnyView {
    let value = properties.value(&path);
    let label = name(&path);
    let key = last(&path).to_owned();
    if key == "values" {
        return json_property(path, properties);
    }
    if key == "value" && !path.is_empty() {
        let parent = &path[..path.len() - 1];
        if properties.value(parent).get("kind").is_some() {
            return field_value(path, properties);
        }
    }
    if key == "refresh_seconds" {
        return number_property(path, properties, true);
    }
    if key == "language"
        || key == "path"
        || key == "title"
            && properties
                .draft
                .with(|root| root.get("type").and_then(Value::as_str) == Some("group"))
    {
        return optional_text(path, properties);
    }
    if key == "tone" {
        return select_property(
            path,
            properties,
            vec!["neutral", "success", "warning", "error"]
                .into_iter()
                .map(String::from)
                .collect(),
        );
    }
    if key == "icon" {
        return select_property(
            path,
            properties,
            vec![
                "puzzle",
                "git_pull_request",
                "list",
                "calendar",
                "activity",
                "book_open",
                "terminal",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        );
    }
    if key == "action" {
        return select_property(
            path,
            properties,
            properties.original.with_value(|panel| {
                panel
                    .panel
                    .actions
                    .iter()
                    .map(|action| action.id.clone())
                    .collect()
            }),
        );
    }
    if key == "type" {
        return ().into_any();
    }
    match value {
        Value::Object(_) => {
            if key == "kind" {
                return kind_property(path, properties);
            }
            let listing = path.clone();
            let nesting = path.clone();
            view! {<div class="panel-property-object">
                <For each=move ||properties.value(&listing).as_object().map(|object|object.keys().filter(|key|key.as_str()!="type" && key.as_str()!="blocks").cloned().collect::<Vec<_>>()).unwrap_or_default()
                    key=|key|key.clone() children=move |key|property(child(&nesting,&key),properties)/>
            </div>}.into_any()
        }
        Value::Array(_) => array_property(path, properties),
        Value::Bool(_) => {
            let reading = path.clone();
            view! {<CheckboxField label=label checked=Signal::derive(move ||properties.value(&reading).as_bool().unwrap_or_default()) on_change=Callback::new(move |checked|properties.set(&path,Value::Bool(checked)))/>}.into_any()
        }
        Value::Number(_) => number_property(path, properties, false),
        Value::String(_) => {
            let reading = path.clone();
            let text = Signal::derive(move || {
                properties
                    .value(&reading)
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            });
            let change = Callback::new(move |text| properties.set(&path, Value::String(text)));
            view! {<FormField label=label.clone()>{if matches!(key.as_str(),"text"|"before"|"after") {
                view! {<TextArea label=label value=text on_change=change rows=4/>}.into_any()
            } else {view! {<TextInput label=label value=text on_change=change/>}.into_any()}}</FormField>}.into_any()
        }
        Value::Null => json_property(path, properties),
    }
}
fn select_property(path: Path, properties: Properties, options: Vec<String>) -> AnyView {
    let label = name(&path);
    let reading = path.clone();
    view! {<FormField label=label.clone()><select class="form-input" aria-label=label prop:value=move ||properties.value(&reading).as_str().unwrap_or_default().to_owned()
        on:change=move |event|properties.set(&path,Value::String(event_target_value(&event)))>
        {options.into_iter().map(|option|view! {<option value=option.clone()>{option.clone()}</option>}).collect::<Vec<_>>()}
    </select></FormField>}.into_any()
}
fn optional_text(path: Path, properties: Properties) -> AnyView {
    let label = name(&path);
    let check = path.clone();
    let toggle = path.clone();
    let reading = path.clone();
    let disabled = path.clone();
    view! {<div class="panel-property-optional">
        <CheckboxField label=format!("Set {label}") checked=Signal::derive(move ||!properties.value(&check).is_null()) on_change=Callback::new(move |enabled: bool|properties.set(&toggle,if enabled {Value::String(String::new())}else{Value::Null}))/>
        <TextInput label=label value=Signal::derive(move ||properties.value(&reading).as_str().unwrap_or_default().to_owned()) disabled=Signal::derive(move ||properties.value(&disabled).is_null()) on_change=Callback::new(move |text|properties.set(&path,Value::String(text)))/>
    </div>}.into_any()
}
fn number_property(path: Path, properties: Properties, optional: bool) -> AnyView {
    let raw = RwSignal::new(None::<String>);
    let reading = path.clone();
    let changing = path.clone();
    let toggle = path.clone();
    let checked = path.clone();
    let disabled = path.clone();
    let label = name(&path);
    view! {<FormField label=label.clone()>
        {optional.then(||view! {<CheckboxField label="Automatic refresh" checked=Signal::derive(move ||!properties.value(&checked).is_null()) on_change=Callback::new(move |enabled: bool| {raw.set(None);properties.error(toggle.clone(),None);properties.set(&toggle,if enabled {json!(30)}else{Value::Null});})/>})}
        <TextInput label=label.clone() input_type="number" disabled=Signal::derive(move ||optional&&properties.value(&disabled).is_null()) value=Signal::derive(move ||raw.get().unwrap_or_else(||properties.value(&reading).as_u64().map_or_else(String::new,|value|value.to_string())))
            on_change=Callback::new(move |text:String|match text.parse::<u64>() {Ok(number)=>{raw.set(None);properties.error(changing.clone(),None);properties.set(&changing,json!(number));},Err(_)=>{raw.set(Some(text));properties.error(changing.clone(),Some(format!("{label} must be a nonnegative whole number")));}})/>
    </FormField>}.into_any()
}
fn json_property(path: Path, properties: Properties) -> AnyView {
    let raw = RwSignal::new(None::<String>);
    let reading = path.clone();
    let label = name(&path);
    view! {<FormField label=format!("{label} (JSON)")><TextArea label=format!("{label} JSON") rows=4 value=Signal::derive(move ||raw.get().unwrap_or_else(||serde_json::to_string_pretty(&properties.value(&reading)).expect("property JSON")))
        on_change=Callback::new(move |text:String|match serde_json::from_str(&text) {Ok(value)=>{raw.set(None);properties.error(path.clone(),None);properties.set(&path,value);},Err(error)=>{raw.set(Some(text));properties.error(path.clone(),Some(format!("{label}: {error}")));}})/></FormField>}.into_any()
}
fn kind_property(path: Path, properties: Properties) -> AnyView {
    let reading = path.clone();
    let changing = path.clone();
    let listing = path.clone();
    let nesting = path.clone();
    let label = name(&path);
    view! {<div class="panel-property-kind">
        <FormField label=label.clone()><select class="form-input" aria-label=label disabled=move ||!properties.errors.get().is_empty()
            prop:value=move ||properties.value(&reading).get("type").and_then(Value::as_str).unwrap_or_default().to_owned()
            on:change=move |event| {
                let (kind,initial)=match event_target_value(&event).as_str() {
                    "secret"=>(json!({"type":"secret","max_length":256}),Value::Null),
                    "checkbox"=>(json!({"type":"checkbox"}),json!(false)),
                    "select"=>(json!({"type":"select","options":[{"value":"option_1","label":"Option"}]}),json!("option_1")),
                    _=>(json!({"type":"text","multiline":false,"max_length":256}),json!("")),
                };
                properties.set(&changing,kind);
                let parent=&changing[..changing.len()-1];properties.set(&child(parent,"value"),initial);
            }><option value="text">"Text"</option><option value="secret">"Secret"</option><option value="checkbox">"Checkbox"</option><option value="select">"Select"</option></select></FormField>
        <For each=move ||properties.value(&listing).as_object().map(|object|object.keys().filter(|key|key.as_str()!="type").cloned().collect::<Vec<_>>()).unwrap_or_default() key=|key|key.clone() children=move |key|property(child(&nesting,&key),properties)/>
    </div>}.into_any()
}
fn field_value(path: Path, properties: Properties) -> AnyView {
    let parent = path[..path.len() - 1].to_vec();
    let kind_parent = parent.clone();
    let rendering = path.clone();
    view! {<For each=move ||vec![properties.value(&kind_parent).get("kind").and_then(|kind|kind.get("type")).and_then(Value::as_str).unwrap_or("text").to_owned()] key=|kind|kind.clone() children=move |kind| {
        if kind=="secret" {return view! {<p>"Secret fields always have an empty initial value."</p>}.into_any();}
        let path=rendering.clone();let label=name(&path);let toggle=path.clone();let checked=path.clone();let disabled=path.clone();let reading=path.clone();let parent=parent.clone();let value_parent=parent.clone();
        let control=if kind=="checkbox" {view! {<CheckboxField label=label checked=Signal::derive(move ||properties.value(&reading).as_bool().unwrap_or_default()) disabled=Signal::derive(move ||properties.value(&disabled).is_null()) on_change=Callback::new(move |value|properties.set(&path,Value::Bool(value)))/>}.into_any()}
        else if kind=="select" {view! {<FormField label=label.clone()><select class="form-input" aria-label=label disabled=move ||properties.value(&disabled).is_null() prop:value=move ||properties.value(&reading).as_str().unwrap_or_default().to_owned() on:change=move |event|properties.set(&path,Value::String(event_target_value(&event)))>
            {move ||properties.value(&value_parent).get("kind").and_then(|kind|kind.get("options")).and_then(Value::as_array).cloned().unwrap_or_default().into_iter().map(|option|view! {<option value=option["value"].as_str().unwrap_or_default().to_owned()>{option["label"].as_str().unwrap_or_default().to_owned()}</option>}).collect::<Vec<_>>()}
        </select></FormField>}.into_any()}
        else {view! {<TextInput label=label value=Signal::derive(move ||properties.value(&reading).as_str().unwrap_or_default().to_owned()) disabled=Signal::derive(move ||properties.value(&disabled).is_null()) on_change=Callback::new(move |value|properties.set(&path,Value::String(value)))/>}.into_any()};
        view! {<div class="panel-property-initial"><CheckboxField label="Set initial value" checked=Signal::derive(move ||!properties.value(&checked).is_null()) on_change=Callback::new(move |enabled: bool| {
            let value=if !enabled {Value::Null}else if kind=="checkbox" {json!(false)}else if kind=="select" {properties.value(&parent)["kind"]["options"][0]["value"].clone()}else{json!("")};properties.set(&toggle,value);
        })/>{control}</div>}.into_any()
    }/>}.into_any()
}

fn array_property(path: Path, properties: Properties) -> AnyView {
    let fixed_cells =
        path.len() >= 2 && matches!(&path[path.len()-2],Key::Name(key) if key=="rows");
    let listing = path.clone();
    let nesting = path.clone();
    let adding = path.clone();
    let label = name(&path);
    let add_label = label.clone();
    let maximum = match last(&path) {
        "fields" => 16,
        "buttons" => 8,
        "columns" => 8,
        "actions" => 32,
        _ => 100,
    };
    let add_count = path.clone();
    view! {<fieldset class="panel-property-array"><legend>{label}</legend>
        <For each={move ||(0..properties.value(&listing).as_array().map_or(0,Vec::len)).collect::<Vec<_>>()} key=|index|*index children={move |index| {
            let mut item=nesting.clone();item.push(Key::Index(index));let up=nesting.clone();let down=nesting.clone();let delete=nesting.clone();let count=nesting.clone();
            view! {<div class="panel-property-item">{property(item,properties)}{(!fixed_cells).then(||view! {<InlineActions>
                <IconButton label="Move property item up" disabled=Signal::derive(move ||index==0||!properties.errors.get().is_empty()) on_click=Callback::new(move |_|array_change(properties,&up,ArrayChange::Move(index,index-1)))><Icon name=IconName::ChevronUp/></IconButton>
                <IconButton label="Move property item down" disabled=Signal::derive(move ||index+1>=properties.value(&count).as_array().map_or(0,Vec::len)||!properties.errors.get().is_empty()) on_click=Callback::new(move |_|array_change(properties,&down,ArrayChange::Move(index,index+1)))><Icon name=IconName::ChevronDown/></IconButton>
                <IconButton label="Remove property item" disabled=Signal::derive(move ||!properties.errors.get().is_empty()) on_click=Callback::new(move |_|array_change(properties,&delete,ArrayChange::Delete(index)))><Icon name=IconName::Trash2/></IconButton>
            </InlineActions>})}</div>}.into_any()
        }}/>
        {(!fixed_cells).then(||view! {<Button disabled=Signal::derive(move ||properties.value(&add_count).as_array().map_or(0,Vec::len)>=maximum||!properties.errors.get().is_empty()) on_click=Callback::new(move |_|array_change(properties,&adding,ArrayChange::Add))><Icon name=IconName::Plus/>{format!("Add {add_label} item")}</Button>})}
    </fieldset>}.into_any()
}
enum ArrayChange {
    Add,
    Delete(usize),
    Move(usize, usize),
}
fn array_change(properties: Properties, path: &[Key], change: ArrayChange) {
    let key = last(path);
    let panel = properties.original.get_value();
    let draft = properties.draft.get_untracked();
    let mut used = BTreeSet::new();
    fn ids(value: &Value, used: &mut BTreeSet<String>) {
        match value {
            Value::Object(object) => {
                if let Some(Value::Object(values)) = object.get("values") {
                    used.extend(values.keys().cloned());
                }
                if let Some(id) = object.get("id").and_then(Value::as_str) {
                    used.insert(id.into());
                }
                for value in object.values() {
                    ids(value, used);
                }
            }
            Value::Array(values) => {
                for value in values {
                    ids(value, used);
                }
            }
            _ => {}
        }
    }
    ids(
        &serde_json::to_value(&panel).expect("panel JSON"),
        &mut used,
    );
    ids(&draft, &mut used);
    let fresh = |prefix: &str| {
        (1usize..)
            .map(|index| format!("{prefix}_{index}"))
            .find(|id| !used.contains(id))
            .expect("bounded IDs")
    };
    let value = match key {
        "fields" => {
            json!({"id":fresh("field"),"label":"Field","kind":{"type":"text","multiline":false,"max_length":256},"value":null,"required":false})
        }
        "buttons" => {
            json!({"action":panel.panel.actions.first().map_or("submit",|action|action.id.as_str()),"label":"Action","values":{},"disabled":false})
        }
        "actions" => {
            let id = fresh("action");
            json!({"id":id,"handler":id,"title":"Action","requires_approval":true})
        }
        "options" => {
            let values = properties.value(path);
            let used = values
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|option| option["value"].as_str())
                .collect::<BTreeSet<_>>();
            let value = (1usize..)
                .map(|index| format!("option_{index}"))
                .find(|value| !used.contains(value.as_str()))
                .expect("bounded options");
            json!({"value":value,"label":"Option"})
        }
        "rows" => {
            let parent = &path[..path.len() - 1];
            let columns = properties
                .value(&child(parent, "columns"))
                .as_array()
                .map_or(1, Vec::len);
            Value::Array(vec![json!(""); columns])
        }
        _ => json!(""),
    };
    properties.draft.update(|root| {
        if key == "columns" {
            let parent = &path[..path.len() - 1];
            if let Some(Value::Array(rows)) = get_mut(root, &child(parent, "rows")) {
                for row in rows {
                    if let Value::Array(cells) = row {
                        match change {
                            ArrayChange::Add => cells.push(json!("")),
                            ArrayChange::Delete(index) => {
                                if index < cells.len() {
                                    cells.remove(index);
                                }
                            }
                            ArrayChange::Move(a, b) => {
                                if a < cells.len() && b < cells.len() {
                                    cells.swap(a, b);
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some(Value::Array(values)) = get_mut(root, path) {
            match change {
                ArrayChange::Add => values.push(value),
                ArrayChange::Delete(index) => {
                    if index < values.len() {
                        values.remove(index);
                    }
                }
                ArrayChange::Move(a, b) => {
                    if a < values.len() && b < values.len() {
                        values.swap(a, b);
                    }
                }
            }
        }
    });
}
