#[cfg(target_arch = "wasm32")]
use futures::{
    FutureExt,
    future::{LocalBoxFuture, Shared},
};
use leptos::prelude::*;
use openwebide_core::{Connection, ProviderKind, SystemPrompt};
#[cfg(target_arch = "wasm32")]
use std::collections::BTreeMap;

#[cfg(target_arch = "wasm32")]
type SettingsResult = Result<BTreeMap<String, String>, String>;
#[cfg(target_arch = "wasm32")]
type SettingsRequest = Shared<LocalBoxFuture<'static, SettingsResult>>;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Theme {
    Dark,
    Light,
    #[default]
    System,
}

impl Theme {
    #[cfg(target_arch = "wasm32")]
    pub fn from_root() -> Self {
        web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.document_element())
            .and_then(|root| root.get_attribute("data-theme"))
            .map_or(Self::System, |value| Self::parse(&value))
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::System => "system",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "dark" => Self::Dark,
            "light" => Self::Light,
            "system" => Self::System,
            _ => Self::Dark,
        }
    }
}

/// Configuration destinations opened from the app and account menus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigurationSection {
    Servers,
    SystemPrompts,
}

/// User preferences and editable connection and prompt configuration.
#[derive(Clone, Copy)]
pub struct SettingsState {
    #[cfg(target_arch = "wasm32")]
    startup_request: StoredValue<Option<(u64, SettingsRequest)>, LocalStorage>,
    pub show_settings: RwSignal<bool>,
    /// Initial category requested when opening the settings dialog.
    pub requested_tab: RwSignal<usize>,
    pub configuration: RwSignal<Option<ConfigurationSection>>,
    pub model_setup: RwSignal<openwebide_core::ModelSetup>,
    pub model_defaults_saving: RwSignal<Option<u64>>,
    pub model_defaults_error: RwSignal<Option<(u64, String)>>,
    pub model_defaults_revision: RwSignal<u64>,
    pub(crate) pending_model_defaults: StoredValue<Option<(u64, openwebide_core::ModelDefaults)>>,
    pub theme: RwSignal<Theme>,
    pub editor_preferences: RwSignal<openwebide_core::editor::EditorPreferences>,
    pub editor_preference_revision: RwSignal<u64>,
    pub browser_notifications: RwSignal<bool>,
    pub default_connection: RwSignal<Option<i64>>,
    pub default_prompt: RwSignal<Option<i64>>,
    pub bridge_url: RwSignal<String>,
    pub connections: RwSignal<Vec<Connection>>,
    pub system_prompts: RwSignal<Vec<SystemPrompt>>,
    pub show_prompt_form: RwSignal<bool>,
    pub prompt_edit_id: RwSignal<Option<i64>>,
    pub prompt_name: RwSignal<String>,
    pub prompt_content: RwSignal<String>,
    pub show_conn_form: RwSignal<bool>,
    pub conn_edit_id: RwSignal<Option<i64>>,
    pub conn_kind: RwSignal<ProviderKind>,
    pub conn_base_url: RwSignal<String>,
}

impl SettingsState {
    /// Share the login snapshot across feature loaders. Old-account readers keep
    /// their own future; their callers still check ownership before publishing.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn load_values(self, api: crate::backend::Api, account: u64) -> SettingsRequest {
        self.startup_request.update_value(|current| {
            if current
                .as_ref()
                .is_none_or(|(generation, _)| *generation != account)
            {
                let backend = api.with_value(Clone::clone);
                let request = async move { backend.get_settings().await }
                    .boxed_local()
                    .shared();
                *current = Some((account, request));
            }
        });
        self.startup_request
            .with_value(|current| current.as_ref().unwrap().1.clone())
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn refresh_values(self) {
        self.startup_request.set_value(None);
    }

    pub fn begin_model_setup(self, server: Option<i64>) {
        let connection = self.connections.with_untracked(|connections| {
            connections
                .iter()
                .find(|connection| Some(connection.id) == server)
                .cloned()
        });
        self.conn_edit_id
            .set(connection.as_ref().map(|connection| connection.id));
        self.conn_kind.set(
            connection
                .as_ref()
                .map_or(ProviderKind::Ollama, |connection| connection.kind),
        );
        self.conn_base_url
            .set(connection.map_or_else(String::new, |connection| connection.base_url));
        self.show_conn_form.set(true);
    }

    pub fn new(theme: Theme, bridge_url: String) -> Self {
        Self {
            #[cfg(target_arch = "wasm32")]
            startup_request: StoredValue::new_local(None),
            show_settings: RwSignal::new(false),
            requested_tab: RwSignal::new(0),
            configuration: RwSignal::new(None),
            model_setup: RwSignal::new(Default::default()),
            model_defaults_saving: RwSignal::new(None),
            model_defaults_error: RwSignal::new(None),
            model_defaults_revision: RwSignal::new(0),
            pending_model_defaults: StoredValue::new(None),
            theme: RwSignal::new(theme),
            editor_preferences: RwSignal::new(Default::default()),
            editor_preference_revision: RwSignal::new(0),
            browser_notifications: RwSignal::new(false),
            default_connection: RwSignal::new(None),
            default_prompt: RwSignal::new(None),
            bridge_url: RwSignal::new(bridge_url),
            connections: RwSignal::new(Vec::new()),
            system_prompts: RwSignal::new(Vec::new()),
            show_prompt_form: RwSignal::new(false),
            prompt_edit_id: RwSignal::new(None),
            prompt_name: RwSignal::new(String::new()),
            prompt_content: RwSignal::new(String::new()),
            show_conn_form: RwSignal::new(false),
            conn_edit_id: RwSignal::new(None),
            conn_kind: RwSignal::new(ProviderKind::Ollama),
            conn_base_url: RwSignal::new(String::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_values_round_trip_and_unknown_values_default_to_dark() {
        Owner::new().with(|| {
            for theme in [Theme::Dark, Theme::Light, Theme::System] {
                assert_eq!(Theme::parse(theme.as_str()), theme);
            }
            assert_eq!(Theme::parse("unknown"), Theme::Dark);
        });
    }
}
