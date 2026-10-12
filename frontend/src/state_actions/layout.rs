//! Shared panel actions and user-scoped persistence; independent of workspace mode.
use crate::{
    backend::Api,
    state::{
        auth::AuthState,
        layout::{ActiveResizer, LayoutState, PANEL_VISIBILITY_KEY, Panel, PanelVisibility},
        responsive::{FilesView, LAYOUT_PREFERENCES_KEY, LayoutMode, LayoutPreferences, PanelSide},
        ui::UiState,
    },
};
use leptos::{prelude::*, task::spawn_local};

#[derive(Clone, Copy)]
pub struct LayoutActions {
    pub save_width: Callback<ActiveResizer>,
    pub save_size: Callback<(String, f64)>,
    pub toggle: Callback<Panel>,
    pub plugin_panel: Callback<(String, bool)>,
    pub move_plugin_panel: Callback<(String, bool)>,
    pub select_files_view: Callback<FilesView>,
    pub set_tree_preferences: Callback<bool>,
    pub show: Callback<Panel>,
    pub set_mode: Callback<LayoutMode>,
    pub pin: Callback<(Panel, PanelSide)>,
    pub move_panel: Callback<(Panel, bool)>,
}
impl LayoutActions {
    pub fn new(api: Api, layout: LayoutState, auth: AuthState, ui: UiState) -> Self {
        let pending = StoredValue::new(None::<PanelVisibility>);
        let saving = StoredValue::new(None::<u64>);
        Effect::new(move |_| {
            auth.generation.track();
            layout.history_tree_width.set(260.0);
            layout.history_tree_revision.set(0);
            layout.panels.set(PanelVisibility::default());
            layout.panel_revision.set(0);
            layout.width_revision.set(0);
            layout.preferences.set(LayoutPreferences::default());
            layout.preference_revision.set(0);
            layout.sheet.set(None);
            layout.plugin_sheet.set(None);
            layout
                .sidebar_width
                .set(crate::state::layout::ActiveResizer::Sidebar.default());
            layout
                .tree_width
                .set(crate::state::layout::ActiveResizer::Tree.default());
            layout
                .chat_width
                .set(crate::state::layout::ActiveResizer::Chat.default());
            layout
                .terminal_height
                .set(crate::state::layout::ActiveResizer::Terminal.default());
            layout.history_width.set(ActiveResizer::History.default());
            layout.active_resizer.set(Default::default());
            pending.set_value(None);
        });
        Effect::new(move |_| {
            layout.active_project.track();
            layout.sheet.set(None);
            layout.plugin_sheet.set(None);
            layout.active_resizer.set(Default::default());
            let viewport = web_sys::window()
                .and_then(|window| window.inner_width().ok())
                .and_then(|value| value.as_f64())
                .unwrap_or(1200.0);
            layout.fit(viewport);
        });
        let change = Callback::new(move |(panel, visible): (Panel, bool)| {
            if !layout.available(panel) {
                return;
            }
            if layout.phone.get_untracked() && panel != Panel::Terminal {
                layout.plugin_sheet.set(None);
                layout.sheet.set(if visible && panel != Panel::Chat {
                    Some(panel)
                } else {
                    None
                });
                return;
            }
            let mut panels = layout.panels.get_untracked();
            if panels.visible(panel) == visible {
                return;
            }
            panels.set(panel, visible);
            layout.panels.set(panels);
            layout.panel_revision.update(|value| *value += 1);
            layout.active_resizer.set(Default::default());
            let viewport = web_sys::window()
                .and_then(|window| window.inner_width().ok())
                .and_then(|value| value.as_f64())
                .unwrap_or(1200.0);
            layout.fit(viewport);
            if auth.user.get_untracked().is_none() {
                return;
            }
            pending.set_value(Some(panels));
            let epoch = auth.generation.get_untracked();
            if saving.get_value() == Some(epoch) {
                return;
            }
            saving.set_value(Some(epoch));
            spawn_local(async move {
                while auth.generation.try_get_untracked() == Some(epoch) {
                    let Some(panels) = pending.try_update_value(Option::take).flatten() else {
                        break;
                    };
                    let value =
                        serde_json::to_string(&panels).expect("panel visibility serializes");
                    let result = api
                        .with_value(Clone::clone)
                        .set_setting(PANEL_VISIBILITY_KEY, &value)
                        .await;
                    if auth.generation.try_get_untracked() != Some(epoch) {
                        return;
                    }
                    if let Err(message) = result {
                        ui.notify(format!("Could not save panel layout: {message}"));
                    }
                }
                if saving.try_get_value() == Some(Some(epoch)) {
                    saving.set_value(None);
                }
            });
        });
        let preference_pending = StoredValue::new(None::<LayoutPreferences>);
        let preference_saving = StoredValue::new(None::<u64>);
        Effect::new(move |_| {
            auth.generation.track();
            preference_pending.set_value(None);
        });
        let save_preferences = Callback::new(move |preferences: LayoutPreferences| {
            layout.preferences.set(preferences.clone());
            layout.preference_revision.update(|revision| *revision += 1);
            layout.sheet.set(None);
            layout.plugin_sheet.set(None);
            layout.fit(layout.viewport_width.get_untracked());
            if auth.user.get_untracked().is_none() {
                return;
            }
            preference_pending.set_value(Some(preferences));
            let epoch = auth.generation.get_untracked();
            if preference_saving.get_value() == Some(epoch) {
                return;
            }
            preference_saving.set_value(Some(epoch));
            let backend = api.with_value(Clone::clone);
            spawn_local(async move {
                while auth.generation.try_get_untracked() == Some(epoch) {
                    let Some(preferences) =
                        preference_pending.try_update_value(Option::take).flatten()
                    else {
                        break;
                    };
                    let value =
                        serde_json::to_string(&preferences).expect("layout preferences serialize");
                    let result = backend.set_setting(LAYOUT_PREFERENCES_KEY, &value).await;
                    if auth.generation.try_get_untracked() != Some(epoch) {
                        return;
                    }
                    if let Err(message) = result {
                        ui.notify(format!("Could not save panel layout: {message}"));
                    }
                }
                if preference_saving.try_get_value() == Some(Some(epoch)) {
                    preference_saving.set_value(None);
                }
            });
        });
        let select_files_view = Callback::new(move |view| {
            if !layout.available(Panel::Files) {
                return;
            }
            let mut preferences = layout.preferences.get_untracked();
            preferences.files_view = view;
            save_preferences.run(preferences);
            change.run((Panel::Files, true));
        });
        let open = Callback::new(move |(panel, toggle): (Panel, bool)| match panel {
            Panel::Git => select_files_view.run(FilesView::Changes),
            Panel::Search => select_files_view.run(FilesView::Search),
            _ => change.run((
                panel,
                !toggle || !layout.visible_panels.get_untracked().visible(panel),
            )),
        });
        let width_pending = StoredValue::new(std::collections::BTreeMap::<String, String>::new());
        let width_saving = StoredValue::new(None::<u64>);
        Effect::new(move |_| {
            auth.generation.track();
            width_pending.update_value(std::collections::BTreeMap::clear);
        });
        let save_size = Callback::new(move |(key, value): (String, f64)| {
            if !value.is_finite() || auth.user.get_untracked().is_none() {
                return;
            }
            width_pending.update_value(|values| {
                values.insert(key, value.to_string());
            });
            let epoch = auth.generation.get_untracked();
            if width_saving.get_value() == Some(epoch) {
                return;
            }
            width_saving.set_value(Some(epoch));
            spawn_local(async move {
                while auth.generation.try_get_untracked() == Some(epoch) {
                    let Some(entry) = width_pending
                        .try_update_value(std::collections::BTreeMap::pop_first)
                        .flatten()
                    else {
                        break;
                    };
                    let result = api
                        .with_value(Clone::clone)
                        .set_setting(&entry.0, &entry.1)
                        .await;
                    if auth.generation.try_get_untracked() != Some(epoch) {
                        return;
                    }
                    if let Err(message) = result {
                        ui.notify(format!("Could not save panel size: {message}"));
                    }
                }
                if width_saving.try_get_value() == Some(Some(epoch)) {
                    width_saving.set_value(None);
                }
            });
        });
        let save_width = Callback::new(move |kind: ActiveResizer| {
            if kind != ActiveResizer::None {
                save_size.run((
                    kind.setting_key().into(),
                    layout.width(kind).get_untracked(),
                ));
            }
        });
        Self {
            save_width,
            save_size,
            select_files_view,
            plugin_panel: Callback::new(move |(namespace, visible): (String, bool)| {
                if !layout
                    .plugin_panels
                    .with_untracked(|panels| panels.contains(&namespace))
                {
                    return;
                }
                let mut preferences = layout.preferences.get_untracked();
                preferences
                    .plugin_panels
                    .entry(namespace.clone())
                    .or_default()
                    .visible = visible;
                save_preferences.run(preferences);
                if layout.phone.get_untracked() {
                    layout.sheet.set(None);
                    layout.plugin_sheet.set(visible.then_some(namespace));
                }
            }),
            move_plugin_panel: Callback::new(move |(namespace, right): (String, bool)| {
                let mut preferences = layout.preferences.get_untracked();
                let visibility = layout.visible_panels.get_untracked();
                let mut visible = [
                    Panel::Sessions,
                    Panel::Files,
                    Panel::History,
                    Panel::Editor,
                    Panel::Terminal,
                    Panel::Chat,
                ]
                .into_iter()
                .filter(|panel| visibility.visible(*panel))
                .map(|panel| panel.id().to_owned())
                .collect::<Vec<_>>();
                visible.extend(
                    preferences
                        .plugin_panels
                        .iter()
                        .filter(|(id, panel)| {
                            panel.visible
                                && layout
                                    .plugin_panels
                                    .with_untracked(|panels| panels.contains(*id))
                        })
                        .map(|(id, _)| id.clone()),
                );
                let ids = visible.iter().map(String::as_str).collect::<Vec<_>>();
                if preferences.move_panel(&namespace, right, &ids) {
                    save_preferences.run(preferences);
                }
            }),
            move_panel: Callback::new(move |(panel, right): (Panel, bool)| {
                let mut preferences = layout.preferences.get_untracked();
                let visibility = layout.visible_panels.get_untracked();
                let mut visible = [
                    Panel::Sessions,
                    Panel::Files,
                    Panel::History,
                    Panel::Editor,
                    Panel::Terminal,
                    Panel::Chat,
                ]
                .into_iter()
                .filter(|panel| visibility.visible(*panel))
                .map(|panel| panel.id().to_owned())
                .collect::<Vec<_>>();
                visible.extend(
                    preferences
                        .plugin_panels
                        .iter()
                        .filter(|(id, panel)| {
                            panel.visible
                                && layout
                                    .plugin_panels
                                    .with_untracked(|panels| panels.contains(*id))
                        })
                        .map(|(id, _)| id.clone()),
                );
                let ids = visible.iter().map(String::as_str).collect::<Vec<_>>();
                if preferences.move_panel(panel.id(), right, &ids) {
                    save_preferences.run(preferences);
                }
            }),
            set_tree_preferences: Callback::new(move |include_hidden| {
                let mut preferences = layout.preferences.get_untracked();
                preferences.include_hidden = include_hidden;
                save_preferences.run(preferences);
                change.run((Panel::Files, true));
            }),
            set_mode: Callback::new(move |mode| {
                let mut preferences = layout.preferences.get_untracked();
                preferences.mode = mode;
                save_preferences.run(preferences);
            }),
            pin: Callback::new(move |(panel, side): (Panel, PanelSide)| {
                let mut preferences = layout.preferences.get_untracked();
                if preferences.pin(panel.id(), side) {
                    save_preferences.run(preferences);
                }
            }),
            toggle: Callback::new(move |panel| {
                open.run((panel, true));
            }),
            show: Callback::new(move |panel| open.run((panel, false))),
        }
    }
}
