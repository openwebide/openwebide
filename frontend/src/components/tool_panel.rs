use crate::{
    state::layout::{ActiveResizer, LayoutState, Panel},
    state_actions::layout::LayoutActions,
};
use leptos::prelude::*;

/// The same collapse control appears in standard and feature-owned headers.
#[component]
pub(super) fn PanelMinimize(
    panel: Panel,
    #[prop(optional)] on_minimize: Option<Callback<()>>,
) -> impl IntoView {
    let actions = use_context::<LayoutActions>();
    view! { <Show when=move || actions.is_some() || on_minimize.is_some()>
        <button type="button" class="icon-btn ui-icon panel-minimize" aria-label=format!("Minimize {}", panel.label()) title="Minimize panel" on:click=move |_| {
            if let Some(callback) = on_minimize { callback.run(()); }
            else if let Some(actions) = actions { actions.toggle.run(panel); }
        }><super::ui::Icon name=super::ui::IconName::Minus /></button>
    </Show> }
}

/// Collapsing retains the mounted editor, drafts, run subscriptions and terminal.
#[component]
pub fn ToolPanel(panel: Panel, children: Children) -> impl IntoView {
    let layout = expect_context::<LayoutState>();
    let chat = use_context::<crate::state::chat::ChatState>();
    let actions = expect_context::<LayoutActions>();
    let kind = match panel {
        Panel::Sessions => ActiveResizer::Sidebar,
        Panel::Files => ActiveResizer::Tree,
        Panel::History => ActiveResizer::History,
        Panel::Chat => ActiveResizer::Chat,
        Panel::Terminal => ActiveResizer::Terminal,
        _ => ActiveResizer::None,
    };
    view! { <PanelFrame id=panel.id().to_owned() label=panel.label().to_owned()
        visible=Signal::derive(move || layout.visible_panels.get().visible(panel))
        center=Signal::derive(move || layout.flexible_panel()==Some(panel))
        order=Signal::derive(move || if panel == Panel::Terminal {255} else {layout.preferences.with(|prefs| prefs.order(panel.id()))})
        width=Signal::derive(move || if kind == ActiveResizer::None {260.0} else {layout.width(kind).get()})>
        <div class="tool-panel-heading" data-context-menu="" hidden=panel == Panel::Terminal || panel == Panel::Editor || panel == Panel::Files>
            <span>{panel.label()}</span>
            <Show when=move || panel != Panel::Terminal>
            <super::dropdown::ActionMenu aria_label="Panel actions">
                <Show when=move || panel == Panel::Chat && chat.is_some()>
                    <button role="menuitem" type="button" class="ui-dropdown-item recent-item" aria-label="Attach images" disabled=move || chat.is_some_and(|chat| chat.reading_images.get()) on:click=move |_| { if let Some(chat) = chat { chat.request_image_picker(); } }><crate::components::ui::Icon name=crate::components::ui::IconName::Paperclip /><span>"Attach images"</span></button>
                </Show>
                <Show when=move || panel == Panel::Chat><h3 class="ui-menu-heading">"Panel"</h3></Show>
                <button role="menuitem" type="button" class="ui-dropdown-item recent-item icon-btn ui-icon" aria-label=format!("Move {} left", panel.label()) title="Move panel left" on:click=move |_| actions.move_panel.run((panel, false))><crate::components::ui::Icon name=crate::components::ui::IconName::ArrowLeft /><span>"Move panel left"</span></button>
                <button role="menuitem" type="button" class="ui-dropdown-item recent-item icon-btn ui-icon" aria-label=format!("Move {} right", panel.label()) title="Move panel right" on:click=move |_| actions.move_panel.run((panel, true))><crate::components::ui::Icon name=crate::components::ui::IconName::ArrowRight /><span>"Move panel right"</span></button>
            </super::dropdown::ActionMenu>
            </Show>
            <Show when=move || !matches!(panel, Panel::Files | Panel::Editor | Panel::Terminal)><PanelMinimize panel=panel /></Show>
            <button class="btn" title="Return to chat" on:click=move |_| actions.show.run(Panel::Chat)>"Back to chat"</button>
        </div>
        <div class="tool-panel-content">
            {children()}
        </div>
        {(panel == Panel::Terminal).then(|| view! { <super::panel_resizer::PanelResizer kind=ActiveResizer::Terminal /> })}
        {(panel != Panel::Terminal).then(|| view! { <DockBoundary panel=panel /> })}
    </PanelFrame> }
}

/// Shared shell for built-in and namespaced contributed panels.
#[component]
pub fn PanelFrame(
    id: String,
    label: String,
    visible: Signal<bool>,
    center: Signal<bool>,
    order: Signal<u8>,
    width: Signal<f64>,
    children: Children,
) -> impl IntoView {
    let layout = expect_context::<LayoutState>();
    view! { <section id=format!("panel-{id}") class="tool-panel"
    class:tool-panel-center=move ||center.get()
    style=move ||format!("display: {}; order: {}; --files-panel-width: {}px; --tool-window-width: {}px; --tool-window-height: {}px;", if visible.get() {"flex"} else {"none"}, order.get(),layout.tree_width.get(),width.get().clamp(140.0,1200.0),layout.terminal_height.get())
    aria-label=label>{children()}</section> }
}

#[component]
pub fn PanelRail(panels: Vec<Panel>) -> impl IntoView {
    let layout = expect_context::<LayoutState>();
    let actions = expect_context::<LayoutActions>();
    view! { <nav class="panel-rail" aria-label="Workspace panels">
        {panels.into_iter().map(|panel| view! {
            <div class="panel-tab-group">
            <button class="btn panel-tab" class:is-open=move || layout.visible_panels.get().visible(panel)
                aria-current=move || if layout.phone.get() && layout.visible_panels.get().visible(panel) { Some("page") } else { None }
                aria-controls=format!("panel-{}", panel.id()) aria-expanded=move || layout.visible_panels.get().visible(panel).to_string()
                disabled=move || !layout.available(panel)
                title=move || if layout.available(panel) { format!("Show or collapse {}", panel.label()) } else { format!("Open a project to use {}", panel.label()) } on:click=move |_| { if layout.phone.get_untracked() { actions.show.run(panel); } else { actions.toggle.run(panel); } }><super::ui::Icon name=match panel { Panel::Sessions | Panel::Chat => super::ui::IconName::MessageCircle, Panel::Files | Panel::Search => super::ui::IconName::FolderSearch, Panel::Editor | Panel::Git => super::ui::IconName::File, Panel::History => super::ui::IconName::FileClock, Panel::Terminal => super::ui::IconName::Terminal } /><span>{panel.label()}</span></button>

            </div>
        }).collect_view()}
        <super::plugin_panels::PluginPanelRailItems/>
    </nav> }
}

/// Files views stay mounted so switching tabs retains queries, selection and scroll.
#[component]
pub fn FilesPanel(
    on_new_file: Callback<()>,
    on_new_dir: Callback<()>,
    children: Children,
) -> impl IntoView {
    use super::ui::{SegmentOption, SegmentedControl};
    use crate::state::responsive::FilesView;
    let layout = expect_context::<LayoutState>();
    let actions = expect_context::<LayoutActions>();
    view! { <div class="files-panel">
        <super::ui::PanelToolbar class="files-panel-toolbar">
        <SegmentedControl options=vec![SegmentOption::new("File Tree", FilesView::Explorer), SegmentOption::new("Changes", FilesView::Changes)]
            value=Signal::derive(move || layout.preferences.with(|p| if p.files_view == FilesView::Search { FilesView::Explorer } else { p.files_view })) on_change=actions.select_files_view />
            <button type="button" class="icon-btn ui-icon" data-file-search-toggle="" title="Search project files" aria-label="Search project files" aria-controls="project-search" aria-expanded=move || layout.preferences.with(|prefs| prefs.files_view == FilesView::Search).to_string()
                on:click=move |_| actions.select_files_view.run(if layout.preferences.with(|prefs| prefs.files_view == FilesView::Search) { FilesView::Explorer } else { FilesView::Search })><super::ui::Icon name=super::ui::IconName::Search /></button>
            <super::file_tree::FileActions on_new_file=on_new_file on_new_dir=on_new_dir />
            <PanelMinimize panel=Panel::Files />
        </super::ui::PanelToolbar>
        {children()}
    </div> }
}

/// Each seam appears once; the flexible pane absorbs changes on either side.
#[component]
fn DockBoundary(panel: Panel) -> impl IntoView {
    let layout = expect_context::<LayoutState>();
    let boundary = Memo::new(move |_| {
        let visible = layout.visible_panels.get();
        layout.preferences.with(|prefs| {
            let mut panels = [
                Panel::Sessions,
                Panel::Files,
                Panel::History,
                Panel::Editor,
                Panel::Chat,
            ]
            .into_iter()
            .filter(|candidate| visible.visible(*candidate))
            .collect::<Vec<_>>();
            panels.sort_by_key(|candidate| prefs.order(candidate.id()));
            let index = panels.iter().position(|candidate| *candidate == panel)?;
            let next = *panels.get(index + 1)?;
            let flexible = layout.flexible_panel();
            let flexible_index = flexible
                .and_then(|flexible| panels.iter().position(|candidate| *candidate == flexible));
            let invert = flexible_index.is_some_and(|flexible| index == flexible);
            let coupled = flexible_index.is_some_and(|flexible| index > flexible);
            let target = if invert { next } else { panel };
            let kind_for = |target| match target {
                Panel::Sessions => ActiveResizer::Sidebar,
                Panel::Files => ActiveResizer::Tree,
                Panel::History => ActiveResizer::History,
                Panel::Chat => ActiveResizer::Chat,
                _ => ActiveResizer::None,
            };
            let kind = kind_for(target);
            if kind == ActiveResizer::None {
                return None;
            }
            Some((kind, invert, coupled.then(|| kind_for(next))))
        })
    });
    view! { {move || boundary.get().map(|(kind, invert, partner)| view! {
        <super::panel_resizer::PanelResizer kind=kind panel=panel invert=invert partner=partner />
    })} }
}
