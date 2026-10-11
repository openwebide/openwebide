use leptos::prelude::*;
use openwebide_core::plugins::{
    PluginInstallation, ProjectPlugin, RecordPlugin,
    marketplace::{MarketplaceFailure, MarketplaceSettings},
};

type ReviewScope = std::sync::Arc<dyn Fn() -> bool + Send + Sync>;

#[derive(Clone, Copy)]
pub struct PluginsState {
    pub review_current: RwSignal<Option<ReviewScope>>,
    pub pending_updates: RwSignal<Vec<RecordPlugin>>,
    pub installations: RwSignal<Vec<PluginInstallation>>,
    pub project_plugins: RwSignal<Vec<ProjectPlugin>>,
    pub marketplaces: RwSignal<MarketplaceSettings>,
    pub failures: RwSignal<Vec<MarketplaceFailure>>,
    pub marketplace_repository: RwSignal<String>,
    pub marketplace_reference: RwSignal<String>,
    pub marketplace_path: RwSignal<String>,
    pub search: RwSignal<String>,
    pub repository: RwSignal<String>,
    pub commit: RwSignal<String>,
    pub path: RwSignal<String>,
    pub busy: RwSignal<bool>,
    pub preparation: RwSignal<Option<openwebide_core::plugins::preparation::PreparationState>>,
    pub cancel_preparation: RwSignal<bool>,
    pub loaded: RwSignal<bool>,
    pub error: RwSignal<Option<String>>,
}
impl PluginsState {
    pub fn updates(self) -> Vec<openwebide_core::plugins::PluginUpdate> {
        openwebide_core::plugins::available_updates(
            &self.installations.get(),
            &self.marketplaces.get(),
        )
    }
}
impl Default for PluginsState {
    fn default() -> Self {
        Self {
            review_current: RwSignal::new(None),
            pending_updates: RwSignal::new(Vec::new()),
            installations: RwSignal::new(Vec::new()),
            project_plugins: RwSignal::new(Vec::new()),
            marketplaces: RwSignal::new(MarketplaceSettings::default()),
            failures: RwSignal::new(Vec::new()),
            marketplace_repository: RwSignal::new(String::new()),
            marketplace_reference: RwSignal::new(String::new()),
            marketplace_path: RwSignal::new("marketplace.json".into()),
            search: RwSignal::new(String::new()),
            repository: RwSignal::new(String::new()),
            commit: RwSignal::new(String::new()),
            path: RwSignal::new(".".into()),
            busy: RwSignal::new(false),
            preparation: RwSignal::new(None),
            cancel_preparation: RwSignal::new(false),
            loaded: RwSignal::new(false),
            error: RwSignal::new(None),
        }
    }
}
