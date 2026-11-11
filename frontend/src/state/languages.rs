//! Language configuration and host readiness, scoped separately from chat tools.
use leptos::prelude::*;
pub use openwebide_agent::plugins::languages::IntelligenceDocuments;
use openwebide_core::plugins::languages::{LanguageSettings, RegisteredLanguage};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LanguageStatus {
    #[default]
    Plain,
    Preparing,
    Ready,
    Unavailable(String),
    Conflicting(Vec<RegisteredLanguage>),
    TooLarge,
}

#[derive(Clone, Copy)]
pub struct LanguagesState {
    pub settings: RwSignal<LanguageSettings>,
    pub settings_loaded: RwSignal<bool>,
    pub settings_error: RwSignal<Option<String>>,
    pub settings_saving: RwSignal<bool>,
    pub selected: RwSignal<Option<RegisteredLanguage>>,
    pub status: RwSignal<LanguageStatus>,
    /// Changes on explicit retry, including reconnects with the same URL.
    pub retry: RwSignal<u64>,
    pub generation: RwSignal<u64>,
    pub intelligence_client: StoredValue<Option<(u64, String)>>,
    pub intelligence_sequence: StoredValue<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    pub intelligence_documents: StoredValue<IntelligenceDocuments>,
}
impl Default for LanguagesState {
    fn default() -> Self {
        Self {
            settings: RwSignal::new(LanguageSettings::default()),
            settings_loaded: RwSignal::new(false),
            settings_error: RwSignal::new(None),
            settings_saving: RwSignal::new(false),
            selected: RwSignal::new(None),
            status: RwSignal::new(LanguageStatus::Plain),
            retry: RwSignal::new(0),
            generation: RwSignal::new(0),
            intelligence_client: StoredValue::new(None),
            intelligence_sequence: StoredValue::new(Default::default()),
            intelligence_documents: StoredValue::new(Default::default()),
        }
    }
}
