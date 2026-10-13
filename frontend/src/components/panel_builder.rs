//! Visual block editing. Gestures emit validated Rust transactions, never host calls.
use super::ui::{
    Button, ButtonVariant, FormField, FormNotice, Icon, IconButton, IconName, InlineActions,
    NoticeTone,
};
use super::{dropdown::ActionMenu, panel_properties::PanelProperties};
use crate::{
    panel_builder::{self, BlockKind, BlockPath, Edit},
    panel_playground::Playground,
};
use leptos::prelude::*;
use openwebide_core::plugins::panels::Block;
use wasm_bindgen::JsCast;

#[derive(Clone)]
enum DragItem {
    Palette(BlockKind),
    Block(BlockPath),
}
#[derive(Clone)]
struct Drag {
    item: DragItem,
    generation: u64,
    pointer: i32,
    x: f64,
    y: f64,
    moved: bool,
    current_x: f64,
    current_y: f64,
}
type Destination = (BlockPath, usize);
#[derive(Clone, Copy)]
struct Canvas {
    selected: RwSignal<Option<BlockPath>>,
    hover: RwSignal<Option<Destination>>,
    apply: Callback<Edit>,
    start: Callback<(DragItem, web_sys::PointerEvent)>,
    motion: Callback<web_sys::PointerEvent>,
    finish: Callback<web_sys::PointerEvent>,
    cancel: Callback<web_sys::PointerEvent>,
    activate: Callback<(DragItem, bool)>,
}

#[component]
pub fn VisualPanelEditor(
    source: Signal<String>,
    generation: Signal<u64>,
    on_change: Callback<String>,
) -> impl IntoView {
    let parsed = Memo::new(move |_| Playground::parse(&source.get()));
    let selected = RwSignal::new(None::<BlockPath>);
    let definition_open = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let hover = RwSignal::new(None::<Destination>);
    let drag = RwSignal::new(None::<Drag>);
    let ignore_click = RwSignal::new(false);
    let own_source = RwSignal::new(source.get_untracked());
    let moving = Memo::new(move |_| drag.get().is_some_and(|gesture| gesture.moved));
    let scroll_timer = StoredValue::new(None::<leptos::leptos_dom::helpers::IntervalHandle>);
    Effect::new(move || {
        if moving.get() {
            let timer = leptos::leptos_dom::helpers::set_interval_with_handle(
                move || {
                    let Some(gesture) = drag.get_untracked().filter(|gesture| gesture.moved) else {
                        return;
                    };
                    let Some(window) = web_sys::window() else {
                        return;
                    };
                    let height = window
                        .inner_height()
                        .ok()
                        .and_then(|height| height.as_f64())
                        .unwrap_or_default();
                    let delta = if gesture.current_y < 40.0 {
                        -12.0
                    } else if gesture.current_y > height - 40.0 {
                        12.0
                    } else {
                        return;
                    };
                    window.scroll_by_with_x_and_y(0.0, delta);
                    hover.set(destination_at(gesture.current_x, gesture.current_y));
                },
                std::time::Duration::from_millis(33),
            )
            .ok();
            scroll_timer.set_value(timer);
        } else {
            scroll_timer.update_value(|timer| {
                if let Some(timer) = timer.take() {
                    timer.clear();
                }
            });
        }
    });
    on_cleanup(move || {
        scroll_timer.update_value(|timer| {
            if let Some(timer) = timer.take() {
                timer.clear();
            }
        });
    });
    Effect::new(move || {
        if source.get() != own_source.get_untracked() {
            if drag.get_untracked().is_some() {
                ignore_click.set(true);
            }
            selected.set(None);
            definition_open.set(false);
            drag.set(None);
            hover.set(None);
            error.set(None);
        }
    });
    let apply = Callback::new(move |operation: Edit| {
        let result = parsed
            .get_untracked()
            .and_then(|panel| panel_builder::edit(&panel, operation));
        match result {
            Ok(result) => {
                selected.set(result.selected);
                let json = result.panel.json();
                own_source.set(json.clone());
                on_change.run(json);
                error.set(None);
            }
            Err(message) => error.set(Some(message)),
        }
    });
    let start = Callback::new(move |(item, event): (DragItem, web_sys::PointerEvent)| {
        ignore_click.set(false);
        if !event.is_primary() || event.button() != 0 {
            return;
        }
        let Some(target) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .and_then(|target| target.closest("button").ok().flatten())
        else {
            return;
        };
        if target.set_pointer_capture(event.pointer_id()).is_err() {
            return;
        }
        event.prevent_default();
        drag.set(Some(Drag {
            item,
            generation: generation.get_untracked(),
            pointer: event.pointer_id(),
            x: event.client_x(),
            y: event.client_y(),
            moved: false,
            current_x: event.client_x(),
            current_y: event.client_y(),
        }));
    });
    let motion = Callback::new(move |event: web_sys::PointerEvent| {
        let Some(mut gesture) = drag.get_untracked() else {
            return;
        };
        if gesture.pointer != event.pointer_id() {
            return;
        }
        gesture.current_x = event.client_x();
        gesture.current_y = event.client_y();
        if (event.client_x() - gesture.x).abs() + (event.client_y() - gesture.y).abs() > 6.0 {
            gesture.moved = true;
        }
        if gesture.moved {
            event.prevent_default();
            hover.set(destination(&event));
        }
        drag.set(Some(gesture));
    });
    let finish = Callback::new(move |event: web_sys::PointerEvent| {
        let Some(gesture) = drag.get_untracked() else {
            return;
        };
        if gesture.pointer != event.pointer_id() {
            return;
        }
        drag.set(None);
        hover.set(None);
        if !gesture.moved {
            return;
        }
        ignore_click.set(true);
        if generation.get_untracked() != gesture.generation {
            error.set(Some("The document changed. Drag the block again.".into()));
            return;
        }
        let Some((parent, index)) = destination(&event) else {
            return;
        };
        apply.run(match gesture.item {
            DragItem::Palette(kind) => Edit::Insert {
                kind,
                parent,
                index,
            },
            DragItem::Block(path) => Edit::Move {
                path,
                parent,
                index,
            },
        });
    });
    let cancel = Callback::new(move |event: web_sys::PointerEvent| {
        if drag
            .get_untracked()
            .is_some_and(|gesture| gesture.pointer == event.pointer_id())
        {
            ignore_click.set(true);
            drag.set(None);
            hover.set(None);
        }
    });
    let activate = Callback::new(move |(item, pointer_click): (DragItem, bool)| {
        if pointer_click && ignore_click.get_untracked() {
            ignore_click.set(false);
            return;
        }
        match item {
            DragItem::Block(path) => selected.set(Some(path)),
            DragItem::Palette(kind) => {
                if let Ok(panel) = parsed.get_untracked() {
                    let parent = selected
                        .get_untracked()
                        .filter(|path| {
                            matches!(
                                panel_builder::block(&panel.document.blocks, path),
                                Ok(Block::Group { .. })
                            )
                        })
                        .unwrap_or_default();
                    if let Ok(blocks) = panel_builder::children(&panel.document.blocks, &parent) {
                        apply.run(Edit::Insert {
                            kind,
                            parent,
                            index: blocks.len(),
                        });
                    }
                }
            }
        }
    });
    let canvas = Canvas {
        selected,
        hover,
        apply,
        start,
        motion,
        finish,
        cancel,
        activate,
    };
    view! {
        <section class="visual-panel-editor" aria-label="Visual panel editor" class:is-dragging=move ||drag.get().is_some_and(|gesture|gesture.moved)
            on:pointerdown=move |event:web_sys::PointerEvent| {if event.is_primary()&&drag.get_untracked().is_none() {ignore_click.set(false);}}>
            <div class="panel-builder-heading"><h2>"Visual editor"</h2>
                <InlineActions><IconButton label="Panel properties" on_click=Callback::new(move |_| {selected.set(None);definition_open.set(true);})><Icon name=IconName::Settings/></IconButton></InlineActions>
            </div>
            <p>"Add blocks with the palette, then drag their handles to reorder or nest them."</p>
            <Show when=move ||error.get().is_some()><FormNotice tone=NoticeTone::Error>{move ||error.get().unwrap_or_default()}</FormNotice></Show>
            <div class="panel-builder-palette" aria-label="Block palette">
                {BlockKind::ALL.into_iter().map(|kind|view! {
                    <div class="panel-builder-palette-item">
                        {handle(DragItem::Palette(kind), canvas)}
                        <Button disabled=Signal::derive(move ||parsed.get().is_err()) on_click=Callback::new(move |event:web_sys::MouseEvent|activate.run((DragItem::Palette(kind),event.detail()!=0)))><Icon name=IconName::Plus/>{kind.label()}</Button>
                    </div>
                }).collect::<Vec<_>>()}
            </div>
            <div class="panel-builder-canvas" aria-label="Block layout">
                <For each={move ||parsed.get().ok().into_iter().map(|panel|(generation.get(),panel)).collect::<Vec<_>>()} key=|(generation,_)|*generation children={move |(_,panel)|outline(panel.document.blocks,vec![],canvas)}/>
                <Show when=move ||parsed.get().is_err()><FormNotice tone=NoticeTone::Warning>"Fix the source JSON to resume visual editing."</FormNotice></Show>
            </div>
            <For each={move ||parsed.get().ok().filter(|_|definition_open.get()||selected.get().is_some()).into_iter().map(|panel|((generation.get(),selected.get()),panel)).collect::<Vec<_>>()}
                key=|(key,_)|key.clone() children={move |((_,path),panel)|{
                    let placement=path.clone().map(|path|view! {<Placement panel=panel.clone() path=path on_move=apply/>});
                    view! {<div class="panel-builder-inspector"><InlineActions><IconButton label="Close properties" on_click=Callback::new(move |_|{selected.set(None);definition_open.set(false);})><Icon name=IconName::X/></IconButton></InlineActions>
                        {placement}<PanelProperties panel=panel selected=path on_apply=apply/></div>
                }}}/>
        </section>
    }
}
#[component]
fn Placement(panel: Playground, path: BlockPath, on_move: Callback<Edit>) -> impl IntoView {
    let original = StoredValue::new(panel);
    let source = StoredValue::new(path.clone());
    let parent = RwSignal::new(path[..path.len() - 1].to_vec());
    let index = RwSignal::new(*path.last().expect("selected block"));
    fn groups(
        blocks: &[Block],
        parent: &[usize],
        excluded: &[usize],
        result: &mut Vec<(BlockPath, String)>,
    ) {
        for (index, block) in blocks.iter().enumerate() {
            if let Block::Group { title, blocks } = block {
                let mut path = parent.to_vec();
                path.push(index);
                if path.starts_with(excluded) {
                    continue;
                }
                let label = format!(
                    "{} · {}",
                    path.iter()
                        .map(|index| (index + 1).to_string())
                        .collect::<Vec<_>>()
                        .join("."),
                    title.as_deref().unwrap_or("Group")
                );
                result.push((path.clone(), label));
                groups(blocks, &path, excluded, result);
            }
        }
    }
    let mut options = vec![(vec![], "Root".to_owned())];
    original.with_value(|panel| groups(&panel.document.blocks, &[], &path, &mut options));
    view! {<fieldset class="panel-builder-placement"><legend>"Move selected block"</legend>
        <FormField label="Destination group"><select class="form-input" aria-label="Destination group" prop:value=move ||serde_json::to_string(&parent.get()).expect("group path") on:change=move |event| {
            if let Ok(path)=serde_json::from_str::<BlockPath>(&event_target_value(&event))
                && let Ok(length)=original.with_value(|panel|panel_builder::children(&panel.document.blocks,&path).map(<[Block]>::len)) {parent.set(path);index.set(length);}
        }>{options.into_iter().map(|(path,label)|view! {<option value=serde_json::to_string(&path).expect("group path")>{label}</option>}).collect::<Vec<_>>()}</select></FormField>
        <FormField label="Position"><select class="form-input" aria-label="Block position" prop:value=move ||index.get().to_string() on:change=move |event|{if let Ok(value)=event_target_value(&event).parse::<usize>() {index.set(value);}}>
            {move ||original.with_value(|panel|panel_builder::children(&panel.document.blocks,&parent.get()).map_or(Vec::new(),|blocks|(0..=blocks.len()).map(|index|view! {<option value=index.to_string()>{format!("Position {}",index+1)}</option>}).collect::<Vec<_>>()))}
        </select></FormField>
        <Button on_click=Callback::new(move |_|on_move.run(Edit::Move {path:source.get_value(),parent:parent.get_untracked(),index:index.get_untracked()}))><Icon name=IconName::CornerDownRight/>"Move block"</Button>
    </fieldset>}
}
fn destination(event: &web_sys::PointerEvent) -> Option<Destination> {
    destination_at(event.client_x(), event.client_y())
}
#[allow(
    clippy::cast_possible_truncation,
    reason = "web-sys hit testing takes f32 CSS viewport coordinates; subpixel rounding is sufficient"
)]
fn destination_at(x: f64, y: f64) -> Option<Destination> {
    let document = web_sys::window()?.document()?;
    let target = document.element_from_point(x as f32, y as f32)?;
    let drop = target.closest("[data-panel-drop]").ok()??;
    serde_json::from_str(&drop.get_attribute("data-panel-drop")?).ok()
}
fn handle(item: DragItem, canvas: Canvas) -> AnyView {
    let label = match &item {
        DragItem::Palette(kind) => format!("Add or drag {}", kind.label()),
        DragItem::Block(path) => format!(
            "Select or drag block {}",
            path.iter()
                .map(|index| (index + 1).to_string())
                .collect::<Vec<_>>()
                .join(".")
        ),
    };
    let click = item.clone();
    view! {
        <span class="panel-builder-handle" on:pointerdown=move |event|canvas.start.run((item.clone(),event))
            on:pointermove=move |event|canvas.motion.run(event) on:pointerup=move |event|canvas.finish.run(event)
            on:pointercancel=move |event|canvas.cancel.run(event) on:lostpointercapture=move |event|canvas.cancel.run(event)>
            <IconButton label=label on_click=Callback::new(move |event:web_sys::MouseEvent|canvas.activate.run((click.clone(),event.detail()!=0)))><Icon name=IconName::GripVertical/></IconButton>
        </span>
    }.into_any()
}
fn drop_zone(parent: BlockPath, index: usize, canvas: Canvas) -> AnyView {
    let destination = (parent, index);
    let attribute = serde_json::to_string(&destination).expect("block destination");
    view! { <div class="panel-builder-drop" data-panel-drop=attribute
        class:active=move ||canvas.hover.get().as_ref()==Some(&destination)><span>"Drop here"</span></div> }.into_any()
}
fn outline(blocks: Vec<Block>, parent: BlockPath, canvas: Canvas) -> AnyView {
    let length = blocks.len();
    let items = blocks.into_iter().enumerate().map(|(index,block)| {
        let mut path = parent.clone(); path.push(index);
        let selected_path = path.clone(); let select_path=path.clone();
        let up_path=path.clone(); let up_parent=parent.clone();
        let down_path=path.clone(); let down_parent=parent.clone();
        let duplicate=path.clone(); let delete=path.clone();
        let title = match &block {
            Block::Text {text} | Block::Heading {text} | Block::Status {text,..} => text.clone(),
            Block::Group {title,..} => title.clone().unwrap_or_else(||"Untitled group".into()),
            Block::Form {id,..} => id.clone(), Block::Link {text,..} => text.clone(),
            _ => String::new(),
        };
        let kind=BlockKind::of(&block).label();
        let nested=if let Block::Group {blocks,..}=block {outline(blocks,path.clone(),canvas)} else {().into_any()};
        view! {
            {drop_zone(parent.clone(),index,canvas)}
            <div class="panel-builder-block" class:selected=move ||canvas.selected.get().as_ref()==Some(&selected_path)>
                <div class="panel-builder-block-heading">
                    {handle(DragItem::Block(path),canvas)}
                    <Button variant=ButtonVariant::Ghost class="panel-builder-select" on_click=Callback::new(move |_|canvas.selected.set(Some(select_path.clone())))>
                        <strong>{kind}</strong><span>{title.chars().take(64).collect::<String>()}</span>
                    </Button>
                    <ActionMenu aria_label="Block actions">
                        <Button disabled=Signal::derive(move ||index==0) on_click=Callback::new({let path=up_path.clone();let parent=up_parent.clone();move |_|canvas.apply.run(Edit::Move {path:path.clone(),parent:parent.clone(),index:index.saturating_sub(1)})})><Icon name=IconName::ChevronUp/>"Move up"</Button>
                        <Button disabled=Signal::derive(move ||index+1==length) on_click=Callback::new({let path=down_path.clone();let parent=down_parent.clone();move |_|canvas.apply.run(Edit::Move {path:path.clone(),parent:parent.clone(),index:index+2})})><Icon name=IconName::ChevronDown/>"Move down"</Button>
                        <Button on_click=Callback::new({let path=duplicate.clone();move |_|canvas.apply.run(Edit::Duplicate {path:path.clone()})})><Icon name=IconName::Copy/>"Duplicate"</Button>
                        <Button on_click=Callback::new({let path=delete.clone();move |_|canvas.apply.run(Edit::Delete {path:path.clone()})})><Icon name=IconName::Trash2/>"Delete"</Button>
                    </ActionMenu>
                </div>
                {nested}
            </div>
        }.into_any()
    }).collect::<Vec<_>>();
    view! {<div class="panel-builder-list">{items}{drop_zone(parent,length,canvas)}</div>}
        .into_any()
}
