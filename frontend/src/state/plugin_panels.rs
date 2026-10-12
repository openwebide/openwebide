//! Installed contributions feed the same registry in every workspace mode.
use super::{plugins::PluginsState, projects::ProjectsState};
use leptos::prelude::*;
use openwebide_core::plugins::{default_bindings, panels::registry::PanelRegistry};

#[derive(Clone, Copy)]
pub struct PluginPanelsState {
    pub registry: Memo<PanelRegistry>,
}
impl PluginPanelsState {
    pub fn new(
        plugins: PluginsState,
        projects: ProjectsState,
        layout: super::layout::LayoutState,
    ) -> Self {
        let registry = Memo::new(move |_| {
            let bindings = if projects.active_project.get().is_some() {
                plugins.project_plugins.get()
            } else {
                default_bindings(&plugins.installations.get())
            };
            PanelRegistry::from_bindings(&bindings)
        });
        Effect::new(move |_| {
            layout.plugin_panels.set(
                registry
                    .get()
                    .panels
                    .into_iter()
                    .map(|panel| panel.namespace)
                    .collect(),
            );
            if let Some(namespace) = layout.plugin_sheet.get_untracked()
                && !layout
                    .plugin_panels
                    .with_untracked(|panels| panels.contains(&namespace))
            {
                layout.plugin_sheet.set(None);
            }
            layout.fit(layout.viewport_width.get_untracked());
        });
        Self { registry }
    }
}
