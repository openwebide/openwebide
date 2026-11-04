//! Deterministic recognition and explicit conflict handling above host transports.
use super::{LanguageDefinition, MAX_LANGUAGES, validate_definitions};
use crate::plugins::ProjectPlugin;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_REGISTERED_LANGUAGES: usize = 256;

/// An editor composes selected definitions from independent packages. Package
/// manifests retain their smaller per-package metadata and contribution limits.
pub fn validate_registered_definitions(definitions: &[LanguageDefinition]) -> Result<(), String> {
    super::validate_definition_limits(
        definitions,
        MAX_REGISTERED_LANGUAGES,
        4 * super::MAX_DEFINITION_BYTES,
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredLanguage {
    pub namespace: String,
    pub plugin_namespace: String,
    pub plugin_label: String,
    pub digest: String,
    pub installation_revision: i64,
    pub host_id: String,
    pub plugin_api: u32,
    pub definition: LanguageDefinition,
}
impl RegisteredLanguage {
    pub fn supports_editor(&self) -> bool {
        let definition = &self.definition;
        let features = &definition.features;
        features.syntax
            || features.folding
            || features.structure
            || definition.configuration != super::LanguageConfiguration::default()
            || *features == super::LanguageFeatures::default()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LanguageRegistry {
    pub providers: Vec<RegisteredLanguage>,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderPreference {
    #[default]
    Automatic,
    Disabled,
    Provider {
        namespace: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguagePreferences {
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderPreference>,
    #[serde(default)]
    pub formatters: BTreeMap<String, ProviderPreference>,
}
impl LanguagePreferences {
    pub fn validate(&self) -> Result<(), String> {
        for preferences in [&self.providers, &self.formatters] {
            if preferences.len() > MAX_REGISTERED_LANGUAGES || preferences.iter().any(|(language, preference)| {
                !super::identifier(language) || matches!(preference, ProviderPreference::Provider { namespace } if namespace.len() > 256 || !namespace.starts_with("plugin:") || namespace.chars().any(char::is_control))
            }) {
                return Err("Invalid or oversized language provider preferences".into());
            }
        }
        Ok(())
    }
}

/// User settings with explicit project overrides, independent of execution mode.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageSettings {
    #[serde(default)]
    pub defaults: LanguagePreferences,
    #[serde(default)]
    pub projects: BTreeMap<i64, LanguagePreferences>,
}
impl LanguageSettings {
    pub fn validate(&self) -> Result<(), String> {
        self.defaults.validate()?;
        if self.projects.len() > MAX_REGISTERED_LANGUAGES {
            return Err("Too many project language preferences".into());
        }
        for (project, preferences) in &self.projects {
            if *project <= 0 {
                return Err("Language preferences require a valid project".into());
            }
            preferences.validate()?;
        }
        if serde_json::to_vec(self)
            .map_err(|error| error.to_string())?
            .len()
            > 256 * 1024
        {
            return Err("Language preferences exceed the settings limit".into());
        }
        Ok(())
    }

    /// An explicit Automatic project choice clears an inherited provider pin.
    pub fn for_project(&self, project: i64) -> LanguagePreferences {
        let mut preferences = self.defaults.clone();
        if let Some(overrides) = self.projects.get(&project) {
            preferences.providers.extend(overrides.providers.clone());
            preferences.formatters.extend(overrides.formatters.clone());
        }
        preferences
    }
}

#[cfg(test)]
mod settings_tests {
    use super::*;

    #[test]
    fn project_choices_override_user_pins_without_changing_other_projects_or_formatters() {
        let pin = ProviderPreference::Provider {
            namespace: "plugin:custom/language:notes".into(),
        };
        let mut settings = LanguageSettings::default();
        settings
            .defaults
            .providers
            .insert("notes".into(), pin.clone());
        settings
            .defaults
            .formatters
            .insert("notes".into(), ProviderPreference::Disabled);
        settings
            .projects
            .entry(1)
            .or_default()
            .providers
            .insert("notes".into(), ProviderPreference::Automatic);
        settings
            .projects
            .entry(2)
            .or_default()
            .providers
            .insert("notes".into(), ProviderPreference::Disabled);
        assert_eq!(
            settings.for_project(1).providers["notes"],
            ProviderPreference::Automatic
        );
        assert_eq!(
            settings.for_project(2).providers["notes"],
            ProviderPreference::Disabled
        );
        assert_eq!(settings.for_project(3).providers["notes"], pin);
        assert_eq!(
            settings.for_project(1).formatters["notes"],
            ProviderPreference::Disabled
        );
        settings.validate().unwrap();
        let encoded = serde_json::to_string(&settings).unwrap();
        assert_eq!(
            serde_json::from_str::<LanguageSettings>(&encoded).unwrap(),
            settings
        );
    }

    #[test]
    fn invalid_project_choices_and_oversized_settings_are_rejected() {
        let mut settings = LanguageSettings::default();
        settings.projects.insert(0, LanguagePreferences::default());
        assert!(settings.validate().is_err());
        settings.projects.clear();
        settings
            .defaults
            .providers
            .insert("../notes".into(), ProviderPreference::Automatic);
        assert!(settings.validate().is_err());
        settings.defaults.providers.clear();
        for project in 1..=MAX_REGISTERED_LANGUAGES {
            let preferences = settings
                .projects
                .entry(i64::try_from(project).unwrap())
                .or_default();
            for language in 0..32 {
                preferences.providers.insert(
                    format!("language-{language}"),
                    ProviderPreference::Provider {
                        namespace: format!("plugin:{}/language:notes", "a".repeat(200)),
                    },
                );
            }
        }
        assert!(settings.validate().unwrap_err().contains("settings limit"));
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LanguageSelection<'a> {
    Plain,
    Selected(&'a RegisteredLanguage),
    Ambiguous(Vec<&'a RegisteredLanguage>),
    Unavailable(String),
}

impl LanguageRegistry {
    pub fn from_bindings(bindings: &[ProjectPlugin]) -> Self {
        let mut registry = Self::default();
        let mut providers = BTreeMap::new();
        let mut ambiguous = BTreeSet::new();
        for binding in bindings.iter().filter(|binding| binding.enabled) {
            let plugin = &binding.prepared;
            if plugin.manifest.contributions.languages.is_empty() {
                continue;
            }
            if let Err(error) = plugin
                .manifest
                .validate()
                .and_then(|()| plugin.manifest.validate_activation())
            {
                registry
                    .errors
                    .push(format!("{}: {error}", plugin.manifest.display_name));
                continue;
            }
            let definitions = &plugin.manifest.contributions.languages;
            if definitions.len() > MAX_LANGUAGES || validate_definitions(definitions).is_err() {
                registry.errors.push(format!(
                    "{}: invalid language declarations",
                    plugin.manifest.display_name
                ));
                continue;
            }
            let namespace = plugin.storage_namespace();
            for definition in definitions {
                let key = format!("plugin:{namespace}/language:{}", definition.id);
                let provider = RegisteredLanguage {
                    namespace: key.clone(),
                    plugin_namespace: namespace.clone(),
                    plugin_label: plugin.manifest.display_name.clone(),
                    digest: plugin.digest.clone(),
                    installation_revision: binding.revision,
                    host_id: plugin.host_id.clone(),
                    plugin_api: plugin.manifest.compatibility.plugin_api,
                    definition: definition.clone(),
                };
                if providers.insert(key.clone(), provider).is_some() {
                    ambiguous.insert(key);
                }
            }
        }
        for key in ambiguous {
            providers.remove(&key);
            registry
                .errors
                .push(format!("Language provider identity is ambiguous: {key}"));
        }
        if providers.len() > MAX_REGISTERED_LANGUAGES {
            registry
                .errors
                .push("Language providers exceed the registration limit".into());
        }
        registry.providers = providers
            .into_values()
            .take(MAX_REGISTERED_LANGUAGES)
            .collect();
        registry
    }

    pub fn find(&self, namespace: &str) -> Option<&RegisteredLanguage> {
        self.providers
            .iter()
            .find(|provider| provider.namespace == namespace)
    }

    pub fn select<'a>(
        &'a self,
        path: &str,
        first_line: &str,
        language: Option<&str>,
        preferences: &LanguagePreferences,
    ) -> LanguageSelection<'a> {
        self.select_matching(
            path,
            first_line,
            language,
            preferences,
            RegisteredLanguage::supports_editor,
        )
    }

    pub fn select_server<'a>(
        &'a self,
        path: &str,
        first_line: &str,
        language: Option<&str>,
        preferences: &LanguagePreferences,
    ) -> LanguageSelection<'a> {
        self.select_matching(path, first_line, language, preferences, |provider| {
            provider.plugin_api >= super::intelligence::MIN_PLUGIN_API
                && provider.definition.features.language_server.is_some()
        })
    }

    fn select_matching<'a>(
        &'a self,
        path: &str,
        first_line: &str,
        language: Option<&str>,
        preferences: &LanguagePreferences,
        supports: impl Fn(&RegisteredLanguage) -> bool,
    ) -> LanguageSelection<'a> {
        let mut candidates = Vec::new();
        let mut best = 0;
        let mut unavailable = None;
        for provider in &self.providers {
            // Formatting, diagnostics and server-only contributions route by
            // feature; they do not replace an installed editor language provider.
            if !supports(provider) {
                continue;
            }
            let Some(score) = recognition(&provider.definition, path, first_line, language) else {
                continue;
            };
            match preferences.providers.get(&provider.definition.id) {
                Some(ProviderPreference::Disabled) => continue,
                Some(ProviderPreference::Provider { namespace })
                    if namespace != &provider.namespace =>
                {
                    let score = score + 10_000;
                    if unavailable.as_ref().is_none_or(|(best, _)| score > *best) {
                        unavailable = Some((score, namespace.clone()));
                    }
                    continue;
                }
                _ => (),
            }
            let explicit = matches!(
                preferences.providers.get(&provider.definition.id),
                Some(ProviderPreference::Provider { .. })
            );
            let score = score + if explicit { 10_000 } else { 0 };
            if score > best {
                candidates.clear();
                best = score;
            }
            if score == best {
                candidates.push(provider);
            }
        }
        if let Some((score, namespace)) = unavailable
            && score > best
        {
            return LanguageSelection::Unavailable(namespace);
        }
        match candidates.as_slice() {
            [] => LanguageSelection::Plain,
            [provider] => LanguageSelection::Selected(provider),
            _ => LanguageSelection::Ambiguous(candidates),
        }
    }

    pub fn formatter<'a>(
        &'a self,
        language: &str,
        preferences: &LanguagePreferences,
    ) -> LanguageSelection<'a> {
        let candidates = self
            .providers
            .iter()
            .filter(|provider| {
                provider.definition.id == language
                    && (provider.definition.features.formatting.is_some()
                        || (provider.plugin_api >= super::intelligence::MIN_PLUGIN_API
                            && provider.definition.features.language_server.is_some()))
            })
            .collect::<Vec<_>>();
        match preferences.formatters.get(language) {
            Some(ProviderPreference::Disabled) => LanguageSelection::Plain,
            Some(ProviderPreference::Provider { namespace }) => candidates
                .into_iter()
                .find(|provider| &provider.namespace == namespace)
                .map_or_else(
                    || LanguageSelection::Unavailable(namespace.clone()),
                    LanguageSelection::Selected,
                ),
            _ => match candidates.as_slice() {
                [] => LanguageSelection::Plain,
                [provider] => LanguageSelection::Selected(provider),
                _ => LanguageSelection::Ambiguous(candidates),
            },
        }
    }

    /// Native servers are explicitly opted in by API 6. Older declarations
    /// remain inert and cannot shadow an executable provider.
    pub fn server<'a>(
        &'a self,
        language: &str,
        preferences: &LanguagePreferences,
    ) -> LanguageSelection<'a> {
        self.select_server("", "", Some(language), preferences)
    }
}

fn recognition(
    definition: &LanguageDefinition,
    path: &str,
    first_line: &str,
    language: Option<&str>,
) -> Option<usize> {
    if let Some(language) = language {
        return (definition.id.eq_ignore_ascii_case(language)
            || definition
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(language)))
        .then_some(10_000);
    }
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    if definition
        .filenames
        .iter()
        .any(|filename| filename.eq_ignore_ascii_case(name))
    {
        return Some(5000);
    }
    let mut wildcard_match = false;
    for pattern in &definition.filename_patterns {
        let expression = format!(
            "^{}$",
            pattern
                .chars()
                .map(|ch| match ch {
                    '*' => ".*".to_string(),
                    '?' => ".".to_string(),
                    _ => regex::escape(&ch.to_string()),
                })
                .collect::<String>()
        );
        if regex::RegexBuilder::new(&expression)
            .case_insensitive(true)
            .size_limit(1024 * 1024)
            .dfa_size_limit(1024 * 1024)
            .build()
            .is_ok_and(|pattern| pattern.is_match(name))
        {
            if pattern.chars().all(|ch| matches!(ch, '*' | '?')) {
                wildcard_match = true;
            } else {
                return Some(4000);
            }
        }
    }
    if let Some(extension) = definition
        .extensions
        .iter()
        .filter(|extension| {
            name.len() > extension.len()
                && name.as_bytes()[name.len() - extension.len() - 1] == b'.'
                && name[name.len() - extension.len()..].eq_ignore_ascii_case(extension)
        })
        .max_by_key(|extension| extension.len())
    {
        return Some(2000 + extension.len());
    }
    if let Some(pattern) = &definition.first_line {
        let first_line = &first_line[..first_line.floor_char_boundary(first_line.len().min(8192))];
        let first_line = first_line
            .split_once('\n')
            .map_or(first_line, |(line, _)| line);
        if regex::RegexBuilder::new(pattern)
            .size_limit(1024 * 1024)
            .dfa_size_limit(1024 * 1024)
            .build()
            .is_ok_and(|pattern| pattern.is_match(first_line))
        {
            return Some(1000);
        }
    }
    wildcard_match.then_some(1)
}

#[cfg(test)]
mod selected_definition_tests {
    use super::*;
    #[test]
    fn selected_definitions_span_packages_but_keep_shared_identity_and_metadata_limits() {
        let mut definitions: Vec<LanguageDefinition> = (0..MAX_REGISTERED_LANGUAGES).map(|index| serde_json::from_value(serde_json::json!({ "id":format!("language-{index}"), "name":format!("Language {index}") })).unwrap()).collect();
        assert!(validate_definitions(&definitions).is_err());
        validate_registered_definitions(&definitions).unwrap();
        definitions.push(definitions[0].clone());
        assert!(validate_registered_definitions(&definitions).is_err());
        definitions.pop();
        definitions[1] = definitions[0].clone();
        assert!(validate_registered_definitions(&definitions).is_err());
    }
}
