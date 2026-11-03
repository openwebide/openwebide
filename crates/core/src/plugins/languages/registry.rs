//! Deterministic recognition and explicit conflict handling above host transports.
use super::{LanguageDefinition, MAX_LANGUAGES, validate_definitions};
use crate::plugins::ProjectPlugin;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_REGISTERED_LANGUAGES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredLanguage {
    pub namespace: String,
    pub plugin_namespace: String,
    pub plugin_label: String,
    pub digest: String,
    pub installation_revision: i64,
    pub host_id: String,
    pub definition: LanguageDefinition,
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
        let mut candidates = Vec::new();
        let mut best = 0;
        let mut unavailable = None;
        for provider in &self.providers {
            let definition = &provider.definition;
            let features = &definition.features;
            // Formatting, diagnostics and server-only contributions route by
            // feature; they do not replace an installed editor language provider.
            if !features.syntax
                && !features.folding
                && !features.structure
                && definition.configuration == super::LanguageConfiguration::default()
                && *features != super::LanguageFeatures::default()
            {
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
                    unavailable = Some(namespace.clone());
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
        match candidates.as_slice() {
            [] => unavailable.map_or(LanguageSelection::Plain, LanguageSelection::Unavailable),
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
                    && provider.definition.features.formatting.is_some()
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
            return Some(4000);
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
    None
}
