//! One menu surface, focus policy and option style for every dropdown.
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use super::ui::{Icon, IconName};

fn items(menu: &web_sys::HtmlElement) -> Vec<web_sys::HtmlElement> {
    let nodes = menu
        .query_selector_all(if menu.get_attribute("role").as_deref() == Some("dialog") {
            "button, input, select, textarea, summary"
        } else {
            "[role=menuitem], [role=menuitemradio], [role=menuitemcheckbox]"
        })
        .unwrap();
    (0..nodes.length())
        .filter_map(|index| nodes.item(index)?.dyn_into::<web_sys::HtmlElement>().ok())
        .filter(|item| !item.has_attribute("disabled") && item.offset_height() > 0)
        .collect()
}

#[component]
pub fn Dropdown(
    #[prop(into)] label: ViewFn,
    aria_label: &'static str,
    #[prop(default = "menu")] menu_role: &'static str,
    #[prop(default = "")] class: &'static str,
    #[prop(default = "btn ghost")] trigger_class: &'static str,
    #[prop(default = "")] menu_class: &'static str,
    #[prop(optional)] open: Option<RwSignal<bool>>,
    /// Context menus anchor at a pointer; ordinary dropdowns retain the trigger anchor.
    #[prop(optional)]
    pointer_anchor: Option<Signal<Option<(f64, f64)>>>,
    #[prop(into, optional)] disabled: Option<Signal<bool>>,
    #[prop(into, optional)] ready: Option<Signal<bool>>,
    #[prop(default = Callback::new(|()| ()))] on_open: Callback<()>,
    #[prop(default = false)] above: bool,
    #[prop(default = false)] hide_caret: bool,
    #[prop(default = 260.0)] preferred_width: f64,
    #[prop(default = 320.0)] max_height: f64,
    children: ChildrenFn,
) -> impl IntoView {
    let open = open.unwrap_or_else(|| RwSignal::new(false));
    let displayed = RwSignal::new(false);
    Effect::new(move |_| {
        if !open.get() {
            displayed.set(false);
        } else if ready.is_none_or(|ready| ready.get()) {
            displayed.set(true);
        }
    });
    let trigger = NodeRef::<leptos::html::Button>::new();
    let menu = NodeRef::<leptos::html::Div>::new();
    let content_id = format!(
        "dropdown-{}",
        NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let close = Callback::new(move |()| {
        open.set(false);
        if let Some(trigger) = trigger.get_untracked()
            && super::modal::allows_focus(trigger.as_ref())
        {
            let _ = trigger.focus();
        }
    });
    let was_open = StoredValue::new(false);
    Effect::new(move |_| {
        let next = open.get();
        if was_open.get_value()
            && !next
            && let Some(trigger) = trigger.get_untracked()
            && super::modal::allows_focus(trigger.as_ref())
        {
            let _ = trigger.focus();
        }
        was_open.set_value(next);
    });
    Effect::new(move |_| {
        if disabled.is_some_and(|disabled| disabled.get()) {
            open.set(false);
        }
    });
    Effect::new(move |_| {
        if !open.get() || ready.is_some_and(|ready| !ready.get()) {
            return;
        }
        let (Some(trigger), Some(menu)) = (trigger.get(), menu.get()) else {
            return;
        };
        let viewport = window().visual_viewport();
        let viewport_width = viewport.as_ref().map_or_else(
            || f64::from(document().document_element().unwrap().client_width()),
            web_sys::VisualViewport::width,
        );
        let viewport_height = viewport.as_ref().map_or_else(
            || f64::from(document().document_element().unwrap().client_height()),
            web_sys::VisualViewport::height,
        );
        let offset = viewport
            .as_ref()
            .map_or(0.0, web_sys::VisualViewport::offset_top);
        let rect = trigger.get_bounding_client_rect();
        let pointer = pointer_anchor.and_then(|anchor| anchor.get());
        let (anchor_left, anchor_top, anchor_bottom) =
            pointer.map_or((rect.left(), rect.top(), rect.bottom()), |(x, y)| (x, y, y));
        let width = pointer
            .map_or(rect.width(), |_| preferred_width)
            .max(preferred_width)
            .min((viewport_width - 16.0).max(0.0));
        let below = (offset + viewport_height - anchor_bottom - 8.0).max(0.0);
        let above_space = (anchor_top - offset - 8.0).max(0.0);
        let up = if above {
            above_space >= below || above_space >= 160.0
        } else {
            below < 160.0 && above_space > below
        };
        let available = if up { above_space } else { below };
        let style = menu.unchecked_ref::<web_sys::HtmlElement>().style();
        let _ = style.set_property("width", &format!("{width}px"));
        let _ = style.set_property("max-height", &format!("{}px", available.min(max_height)));
        let _ = style.set_property(
            "--dropdown-available-height",
            &format!("{}px", available.min(max_height)),
        );
        let _ = style.set_property(
            "left",
            &format!(
                "{}px",
                anchor_left.clamp(8.0, (viewport_width - width - 8.0).max(8.0))
            ),
        );
        if up {
            let height = window()
                .inner_height()
                .ok()
                .and_then(|height| height.as_f64())
                .unwrap_or(viewport_height);
            let _ = style.set_property("top", "auto");
            let _ = style.set_property("bottom", &format!("{}px", height - anchor_top + 4.0));
        } else {
            let _ = style.set_property("bottom", "auto");
            let _ = style.set_property("top", &format!("{}px", anchor_bottom + 4.0));
        }
        let _ = style.set_property("visibility", "visible");
        let choices = items(&menu);
        if let Some(item) = choices
            .iter()
            .find(|item| item.get_attribute("aria-checked").as_deref() == Some("true"))
            .or_else(|| choices.first())
        {
            let _ = item.focus();
        }
    });
    let resize = window_event_listener(leptos::ev::resize, move |_| {
        if open.get_untracked() {
            close.run(());
        }
    });
    let wheel = window_event_listener(leptos::ev::wheel, move |event| {
        if open.get_untracked()
            && !event
                .target()
                .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                .is_some_and(|target| target.closest(".ui-dropdown-menu").ok().flatten().is_some())
        {
            close.run(());
        }
    });
    on_cleanup(move || {
        resize.remove();
        wheel.remove();
    });
    let children = std::sync::Arc::new(children);
    view! {
        <span class=format!("ui-dropdown {class}") on:keydown=move |event: web_sys::KeyboardEvent| {
            if event.key() == "Escape" && open.get_untracked() { event.prevent_default(); event.stop_propagation(); close.run(()); return; }
            if menu_role == "dialog" && open.get_untracked() { return; }
            if event.key() == "Tab" && open.get_untracked() { close.run(()); return; }
            if !open.get_untracked() && matches!(event.key().as_str(), "ArrowDown" | "ArrowUp") { event.prevent_default(); open.set(true); on_open.run(()); return; }
            let Some(menu) = menu.get_untracked() else { return; };
            let choices = items(&menu);
            if choices.is_empty() { return; }
            let current = document().active_element().and_then(|active| choices.iter().position(|item| item.is_same_node(Some(active.as_ref())))).unwrap_or(0);
            let index = match event.key().as_str() {
                "ArrowDown" => Some((current + 1) % choices.len()),
                "ArrowUp" => Some((current + choices.len() - 1) % choices.len()),
                "Home" => Some(0), "End" => Some(choices.len() - 1),
                key if key.chars().count() == 1 && !event.ctrl_key() && !event.meta_key() => (1..=choices.len()).map(|step| (current + step) % choices.len()).find(|index| choices[*index].text_content().unwrap_or_default().trim().to_lowercase().starts_with(&key.to_lowercase())),
                _ => None,
            };
            if let Some(index) = index { event.prevent_default(); event.stop_propagation(); let _ = choices[index].focus(); }
        }>
            <button type="button" class=format!("ui-dropdown-trigger {trigger_class}") node_ref=trigger aria-label=aria_label aria-haspopup=menu_role aria-expanded=move || open.get().to_string() aria-controls=content_id.clone() aria-busy=move || (open.get() && ready.is_some_and(|ready| !ready.get())).to_string()
                disabled=move || disabled.is_some_and(|disabled| disabled.get()) on:click=move |event| { event.prevent_default(); event.stop_propagation(); let next = !open.get_untracked(); open.set(next); if next { on_open.run(()); } }>
                <span class="ui-dropdown-label">{label.run()}</span>{(!hide_caret).then(|| view! { <span class="ui-dropdown-indicator"><Show when=move || open.get() && ready.is_some_and(|ready| !ready.get()) fallback=|| view! { <Icon name=IconName::ChevronDown /> }><span class="tui-spinner" role="status" aria-label="Loading options" /></Show></span> })}
            </button>
            <Show when=move || open.get()>
                <div class="ui-dropdown-backdrop recent-backdrop" on:click=move |event| { event.stop_propagation(); event.prevent_default(); close.run(()); } />
            </Show>
            <Show when=move || open.get() && (displayed.get() || ready.is_none_or(|ready| ready.get()))>
                <div class=format!("ui-dropdown-menu recent-menu {menu_class}") role=menu_role aria-label=aria_label id=content_id.clone() node_ref=menu style="visibility:hidden" on:click=move |event| { if menu_role != "dialog" { event.prevent_default(); } event.stop_propagation(); }>{children()}</div>
            </Show>
        </span>
    }
}

static NEXT_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
    pub disabled: bool,
}
impl SelectOption {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            disabled: false,
        }
    }
}

#[component]
pub fn DropdownSelect(
    label: &'static str,
    #[prop(into)] value: Signal<String>,
    #[prop(into)] options: Signal<Vec<SelectOption>>,
    on_change: Callback<String>,
    #[prop(default = "form-input")] trigger_class: &'static str,
    #[prop(default = "")] class: &'static str,
    #[prop(into, optional)] disabled: Option<Signal<bool>>,
    #[prop(into, optional)] ready: Option<Signal<bool>>,
    #[prop(default = Callback::new(|()| ()))] on_open: Callback<()>,
    #[prop(default = false)] above: bool,
) -> impl IntoView {
    let open = RwSignal::new(false);
    view! {
        <Dropdown class=class aria_label=label trigger_class=trigger_class open=open disabled=Signal::derive(move || disabled.is_some_and(|disabled| disabled.get())) on_open=on_open above=above ready=Signal::derive(move || ready.is_none_or(|ready| ready.get()))
            label=move || view! { <span>{move || options.with(|options| options.iter().find(|option| option.value == value.get()).map_or_else(|| value.get(), |option| option.label.clone()))}</span> }>
            <For each=move || options.get() key=|option| option.value.clone() children=move |option| {
                let selected = option.value.clone(); let click = option.value.clone();
                let row_value = option.value.clone();
                let row = Signal::derive(move || options.with(|options| options.iter().find(|option| option.value == row_value).cloned()));
                view! { <button type="button" class="ui-dropdown-item recent-item" role="menuitemradio" data-value=option.value aria-checked=move || (value.get() == selected).to_string() disabled=move || row.with(|option| option.as_ref().is_none_or(|option| option.disabled)) on:click=move |_| { on_change.run(click.clone()); open.set(false); }>{move || row.with(|option| option.as_ref().map(|option| option.label.clone()).unwrap_or_default())}</button> }
            } />
        </Dropdown>
    }
}

/// Secondary actions share the same menu, keyboard navigation and theme as selectors.
#[component]
pub fn ActionMenu(
    aria_label: &'static str,
    #[prop(default = IconName::Ellipsis)] icon: IconName,
    #[prop(default = false)] context_only: bool,
    children: ChildrenFn,
) -> impl IntoView {
    let open = RwSignal::new(false);
    let anchor = RwSignal::new(None::<(f64, f64)>);
    let root = NodeRef::<leptos::html::Span>::new();
    super::context_menu::context_menu_target(
        move || {
            let root = root.get()?;
            root.closest("[data-context-menu]")
                .ok()
                .flatten()
                .or_else(|| root.parent_element())
                .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
        },
        Callback::new(move |point| {
            anchor.set(point);
            open.set(true);
        }),
    );
    // Handlers and any dialogs they create must outlive the temporary menu surface.
    let owner = Owner::current().expect("Action menu has an owner");
    let children = std::sync::Arc::new(children);
    view! {
        <span class="ui-action-menu-context" node_ref=root>
        <Dropdown aria_label=aria_label class="ui-action-menu" trigger_class=if context_only { "sr-only" } else { "icon-btn ui-icon" } hide_caret=true open=open pointer_anchor=anchor.into()
            on_open=Callback::new(move |()| anchor.set(None)) label=move || view! { <Icon name=icon /> }>
            <div class="ui-action-items" on:click=move |event| {
                if event.target().and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                    .is_some_and(|target| target.closest("button").ok().flatten().is_some()) { open.set(false); }
            }>{owner.with(|| children())}</div>
        </Dropdown>
        </span>
    }
}
