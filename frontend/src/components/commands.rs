use super::modal::Modal;
use super::ui::{DialogActions, DialogBody};
use crate::{
    commands::{COMMANDS, COMPOSER_SHORTCUTS},
    state::ui::UiState,
    state_actions::omnibar::OmnibarActions,
};
use leptos::prelude::*;

#[component]
pub fn CommandDialogs() -> impl IntoView {
    let ui = expect_context::<UiState>();
    view! {
        <Show when=move || ui.whats_new_open.get()><super::about::WhatsNew /></Show>
        <Show when=move || ui.about_open.get()><super::About /></Show>
        <Show when=move || ui.shortcuts_open.get()><KeyboardShortcuts /></Show>
    }
}
#[component]
pub fn Omnibar() -> impl IntoView {
    let ui = expect_context::<UiState>();
    let actions = expect_context::<OmnibarActions>();
    let opened_scope = StoredValue::new(actions.scope.get_untracked());
    let input = NodeRef::<leptos::html::Input>::new();
    let root = NodeRef::<leptos::html::Div>::new();
    let panel = NodeRef::<leptos::html::Div>::new();
    let placement = RwSignal::new(0u64);
    let opener = StoredValue::new_local(None::<web_sys::HtmlElement>);
    let restore_focus = StoredValue::new(true);
    let query = actions.query;
    let results = actions.entries;
    let selected = RwSignal::new(None::<usize>);
    let previous_query = StoredValue::new(String::new());
    let selected_id = StoredValue::new(None::<String>);
    Effect::new(move |_| {
        let query = query.get();
        results.with(|results| {
            let current = if previous_query.get_value() == query {
                selected_id.get_value().and_then(|id| {
                    results
                        .iter()
                        .position(|entry| entry.id == id && entry.unavailable.is_none())
                })
            } else {
                None
            };
            let current =
                current.or_else(|| results.iter().position(|entry| entry.unavailable.is_none()));
            selected.set(current);
            selected_id.set_value(current.map(|index| results[index].id.clone()));
        });
        previous_query.set_value(query);
    });
    let activate = move |index: usize| {
        if actions.scope.get_untracked() != opened_scope.get_value() {
            return;
        }
        if let Some(entry) = results.with_untracked(|results| results.get(index).cloned())
            && entry.unavailable.is_none()
        {
            actions.activate.run(entry.action);
        }
    };
    let close = Callback::new(move |()| ui.palette_open.set(false));
    Effect::new(move |previous: Option<bool>| {
        let open = ui.palette_open.get();
        if open && previous != Some(true) {
            opened_scope.set_value(actions.scope.get_untracked());
            opener.set_value(None);
            if let Some(active) = document().active_element().and_then(|element| {
                use wasm_bindgen::JsCast;
                element.dyn_into::<web_sys::HtmlElement>().ok()
            }) && root
                .get_untracked()
                .is_none_or(|root| !root.contains(Some(&active)))
            {
                opener.set_value(Some(active));
            }
            restore_focus.set_value(true);
            if let Some(input) = input.get() {
                let _ = input.focus();
            }
        } else if !open
            && previous == Some(true)
            && restore_focus.get_value()
            && !super::modal::modal_is_open()
            && root.get_untracked().is_some_and(|root| {
                document()
                    .active_element()
                    .is_some_and(|active| root.contains(Some(&active)))
            })
            && let Some(opener) = opener.get_value().filter(|opener| opener.is_connected())
        {
            let _ = opener.focus();
        }
        open
    });
    let pointer = window_event_listener(leptos::ev::pointerdown, move |event| {
        use wasm_bindgen::JsCast;
        if ui.palette_open.get_untracked()
            && root.get_untracked().is_some_and(|root| {
                event
                    .target()
                    .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
                    .is_none_or(|target| !root.contains(Some(&target)))
            })
        {
            restore_focus.set_value(false);
            close.run(());
        }
    });
    let focus = window_event_listener(leptos::ev::focusin, move |event| {
        use wasm_bindgen::JsCast;
        if ui.palette_open.get_untracked()
            && root.get_untracked().is_some_and(|root| {
                event
                    .target()
                    .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
                    .is_none_or(|target| !root.contains(Some(&target)))
            })
        {
            restore_focus.set_value(false);
            close.run(());
        }
    });
    on_cleanup(move || {
        pointer.remove();
        focus.remove();
    });
    let resize = window_event_listener(leptos::ev::resize, move |_| {
        placement.update(|revision| *revision = revision.wrapping_add(1));
    });
    on_cleanup(move || resize.remove());
    if let Some(viewport) = window().visual_viewport() {
        use wasm_bindgen::{JsCast, closure::Closure};
        let listener = Closure::wrap(Box::new(move |_: web_sys::Event| {
            placement.update(|revision| *revision = revision.wrapping_add(1));
        }) as Box<dyn FnMut(_)>);
        for event in ["resize", "scroll"] {
            let _ =
                viewport.add_event_listener_with_callback(event, listener.as_ref().unchecked_ref());
        }
        let subscription = StoredValue::new_local((viewport, listener));
        on_cleanup(move || {
            subscription.with_value(|(viewport, listener)| {
                for event in ["resize", "scroll"] {
                    let _ = viewport.remove_event_listener_with_callback(
                        event,
                        listener.as_ref().unchecked_ref(),
                    );
                }
            });
        });
    }
    Effect::new(move |_| {
        placement.track();
        if !ui.palette_open.get() {
            return;
        }
        let (Some(input), Some(panel)) = (input.get(), panel.get()) else {
            return;
        };
        let viewport = window().visual_viewport();
        let width = viewport.as_ref().map_or_else(
            || f64::from(document().document_element().unwrap().client_width()),
            web_sys::VisualViewport::width,
        );
        let height = viewport.as_ref().map_or_else(
            || f64::from(document().document_element().unwrap().client_height()),
            web_sys::VisualViewport::height,
        );
        let offset = viewport
            .as_ref()
            .map_or(0.0, web_sys::VisualViewport::offset_top);
        let rect = input.get_bounding_client_rect();
        let panel_width = 720.0_f64.min((width - 16.0).max(0.0));
        let left = (rect.left() + rect.width() / 2.0 - panel_width / 2.0)
            .clamp(8.0, (width - panel_width - 8.0).max(8.0));
        let top = rect.bottom() + 8.0;
        let style = web_sys::HtmlElement::style(&panel);
        let _ = style.set_property("width", &format!("{panel_width}px"));
        let _ = style.set_property("left", &format!("{left}px"));
        let _ = style.set_property("top", &format!("{top}px"));
        let _ = style.set_property(
            "max-height",
            &format!("{}px", (offset + height - top - 8.0).max(0.0)),
        );
        let _ = style.set_property("visibility", "visible");
    });
    view! {
        <div class="omnibar-trigger omnibar" node_ref=root>
            <super::ui::PanelSearchRow class="omnibar-field">
                <input node_ref=input
                id="command-search" class="form-input command-search" data-loading=move || actions.loading.get().to_string() type="search" autocomplete="off" maxlength="256" placeholder="Search…" aria-label="Search commands, files, projects and sessions" role="combobox"
                    aria-autocomplete="list" aria-expanded=move || ui.palette_open.get().to_string() aria-controls="command-results"
                    on:focus=move |_| { if !ui.palette_open.get_untracked() { ui.palette_open.set(true); } }
                    on:click=move |_| { if !ui.palette_open.get_untracked() { ui.palette_open.set(true); } }
                    aria-activedescendant=move || ui.palette_open.get().then(|| selected.get()).flatten().and_then(|index|results.with(|results|results.get(index).map(|entry|entry.id.clone())))
                    prop:value=move || query.get()
                    on:input=move |event| { query.set(event_target_value(&event)); if !ui.palette_open.get_untracked() { ui.palette_open.set(true); } }
                    on:keydown=move |event: web_sys::KeyboardEvent| {
                        if event.is_composing() { return; }
                        match event.key().as_str() {
                            "ArrowDown" | "ArrowUp" => {
                                event.prevent_default();
                                let choices:Vec<_>=results.with_untracked(|results|results.iter().enumerate().filter(|(_,entry)|entry.unavailable.is_none()).map(|(index,_)|index).collect());
                                if choices.is_empty() { return; }
                                let current=selected.get_untracked().and_then(|index|choices.iter().position(|choice|*choice==index));
                                let next=if event.key()=="ArrowUp" { current.map_or(choices.len()-1,|index|(index+choices.len()-1)%choices.len()) } else { current.map_or(0,|index|(index+1)%choices.len()) };
                                let index=choices[next];selected.set(Some(index));
                                if let Some(id)=results.with_untracked(|results|results.get(index).map(|entry|entry.id.clone())) {
                                    selected_id.set_value(Some(id.clone()));
                                    if let Some(element)=document().get_element_by_id(&id) { element.scroll_into_view_with_bool(false); }
                                }
                            }
                            "Escape" => { event.prevent_default(); event.stop_propagation(); close.run(()); }
                            "Tab" => { restore_focus.set_value(false); close.run(()); }
                            "Enter" => { event.prevent_default();if let Some(index)=selected.get_untracked() { activate(index); } }
                            _=>{}
                        }
                    }
                />
                <kbd>"⌘/Ctrl ⇧ P"</kbd>
            </super::ui::PanelSearchRow>
            <Show when=move || ui.palette_open.get()>
            <div class="omnibar-panel command-palette" node_ref=panel style="visibility:hidden">
                <p id="command-palette-help" class="form-hint">"↑ / ↓ to choose · Enter to open · Escape to close · > commands · / files · # projects · @ sessions"</p>
                <Show when=move || actions.loading.get()><p class="form-hint" role="status">"Loading filenames and sessions…"</p></Show>
                <Show when=move || actions.notice.get().is_some()><p class="form-hint" role="status">{move || actions.notice.get()}</p></Show>
                <div id="command-results" class="command-results" role="listbox" aria-label="Search results">
                    {move || results.get().into_iter().enumerate().map(|(index,entry)| {
                        let reason=entry.unavailable.clone();
                        view! {
                            <button id=entry.id class=move || format!("btn ghost command-option{}",if selected.get()==Some(index){" selected"}else{""}) data-result-kind=entry.kind role="option" aria-selected=move || (selected.get()==Some(index)).to_string() disabled=entry.unavailable.is_some() on:click=move |_|activate(index)>
                                <span class="omnibar-entry"><span class="omnibar-entry-label">{entry.label}</span><span class="form-hint omnibar-entry-detail">{reason.unwrap_or(entry.detail)}</span></span>
                                <span class="omnibar-entry-kind">{entry.kind}</span><kbd>{entry.shortcut}</kbd>
                            </button>
                        }
                    }).collect::<Vec<_>>()}
                    <Show when=move || results.with(Vec::is_empty)><p class="empty" role="status">"No matching results"</p></Show>
                </div>
            </div>
            </Show>
        </div>
    }
}
#[component]
fn KeyboardShortcuts() -> impl IntoView {
    let ui = expect_context::<UiState>();
    let close = Callback::new(move |()| ui.shortcuts_open.set(false));
    let tab = RwSignal::new(0_usize);
    view! {
        <Modal title="Keyboard shortcuts".to_string().into() on_close=close class="modal ui-tabbed-modal" size=super::ui::DialogSize::Wide describedby="shortcut-help">
            <super::ui::DialogTabs label="Shortcut categories" options=vec![
                super::ui::DialogTab::new("Workspace", "shortcuts-tab-workspace", "shortcuts-panel-workspace"),
                super::ui::DialogTab::new("Chat", "shortcuts-tab-chat", "shortcuts-panel-chat"),
                super::ui::DialogTab::new("Search", "shortcuts-tab-search", "shortcuts-panel-search"),
            ] selected=tab.read_only().into() on_change=Callback::new(move |index| tab.set(index)) />
            <DialogBody class="keyboard-shortcuts ui-tabbed-body">
                <p id="shortcut-help" class="form-hint">"Use Ctrl on Windows/Linux or ⌘ on macOS. Global commands pause while a dialog is open."</p>
                <div id="shortcuts-panel-workspace" class="ui-tab-panel" role="tabpanel" aria-labelledby="shortcuts-tab-workspace" tabindex="0" hidden=move || tab.get() != 0>
                    <dl>{COMMANDS.iter().filter(|command| command.shortcut.starts_with("Ctrl")).map(|command| view! { <div class="shortcut-row"><dt><kbd>{command.shortcut}</kbd></dt><dd>{command.label}</dd></div> }).collect::<Vec<_>>()}</dl>
                </div>
                <div id="shortcuts-panel-chat" class="ui-tab-panel" role="tabpanel" aria-labelledby="shortcuts-tab-chat" tabindex="0" hidden=move || tab.get() != 1>
                    <p class="form-hint">"These bindings apply while the prompt is focused."</p>
                    <dl>{COMPOSER_SHORTCUTS.iter().map(|(key, action)| view! { <div class="shortcut-row"><dt><kbd>{*key}</kbd></dt><dd>{*action}</dd></div> }).collect::<Vec<_>>()}</dl>
                </div>
                <div id="shortcuts-panel-search" class="ui-tab-panel" role="tabpanel" aria-labelledby="shortcuts-tab-search" tabindex="0" hidden=move || tab.get() != 2>
                    <p class="form-hint">"Search commands, files, projects and sessions from the top bar."</p>
                    <dl><div class="shortcut-row"><dt><kbd>"↑ / ↓"</kbd></dt><dd>"Choose a result"</dd></div><div class="shortcut-row"><dt><kbd>"Enter"</kbd></dt><dd>"Open the selected result"</dd></div><div class="shortcut-row"><dt><kbd>"Escape"</kbd></dt><dd>"Close search"</dd></div></dl>
                </div>
            </DialogBody>
            <DialogActions><button class="btn" on:click=move |_| close.run(())>"Close"</button></DialogActions>
        </Modal>
    }
}
