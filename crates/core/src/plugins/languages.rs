//! Language contribution validation and shared provider policy. Transport and
//! component execution remain host adapters; no provider is selected by identity.
pub use openwebide_plugin_sdk::languages::*;
use std::{
    collections::BTreeSet,
    sync::{LazyLock, Mutex},
};
mod composition;
pub use composition::LanguageComposition;
pub mod intelligence;
mod registry;
pub use registry::*;
mod service;
pub use service::*;

pub const MAX_LANGUAGES: usize = 64;
pub const MAX_DEFINITION_BYTES: usize = 256 * 1024;
const MAX_VALIDATED_EXPRESSIONS: usize = 256;
static VALID_EXPRESSIONS: LazyLock<Mutex<BTreeSet<String>>> =
    LazyLock::new(|| Mutex::new(BTreeSet::new()));

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, b'-' | b'_'))
}
fn text(value: &str, limit: usize) -> bool {
    !value.trim().is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}
fn expression(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 1024 {
        return Err("Language expressions require 1–1,024 bytes".into());
    }
    if VALID_EXPRESSIONS.lock().unwrap().contains(value) {
        return Ok(());
    }
    regex::RegexBuilder::new(value)
        .size_limit(1024 * 1024)
        .dfa_size_limit(1024 * 1024)
        .build()
        .map(|_| ())
        .map_err(|_| "Invalid or oversized language expression".to_string())?;
    // Exact patterns have the same validity under these fixed compiler limits.
    // Retain only bounded strings, not compiled engines or provider authority.
    let mut validated = VALID_EXPRESSIONS.lock().unwrap();
    validated.insert(value.to_owned());
    while validated.len() > MAX_VALIDATED_EXPRESSIONS {
        let _ = validated.pop_first();
    }
    Ok(())
}
fn optional_expression(value: &Option<String>) -> Result<(), String> {
    if let Some(value) = value {
        expression(value)?;
    }
    Ok(())
}
fn marker(value: &str) -> bool {
    !value.is_empty() && value.len() <= 32 && !value.chars().any(char::is_control)
}
fn character(value: char) -> bool {
    !value.is_control() && !value.is_whitespace()
}
fn filename(value: &str) -> bool {
    text(value, 256) && !matches!(value, "." | "..") && !value.contains(['/', '\\'])
}
fn handler(value: &LanguageHandler) -> bool {
    identifier(&value.id) && text(&value.title, 128)
}

pub fn validate_definitions(definitions: &[LanguageDefinition]) -> Result<(), String> {
    validate_definition_limits(definitions, MAX_LANGUAGES, MAX_DEFINITION_BYTES)
}
fn validate_definition_limits(
    definitions: &[LanguageDefinition],
    limit: usize,
    bytes: usize,
) -> Result<(), String> {
    if definitions.len() > limit
        || serde_json::to_vec(definitions)
            .map_err(|error| error.to_string())?
            .len()
            > bytes
    {
        return Err("Language contributions exceed their metadata limit".into());
    }
    let mut ids = BTreeSet::new();
    for definition in definitions {
        if !identifier(&definition.id)
            || !ids.insert(&definition.id)
            || !text(&definition.name, 128)
            || definition.aliases.len() > 32
            || definition.aliases.iter().any(|alias| !text(alias, 64))
            || definition.extensions.len() > 128
            || definition.extensions.iter().any(|extension| {
                extension.is_empty()
                    || extension.len() > 64
                    || extension.starts_with('.')
                    || !extension
                        .bytes()
                        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'.' | b'_' | b'-'))
            })
            || definition.filenames.len() > 128
            || definition.filenames.iter().any(|name| !filename(name))
            || definition.filename_patterns.len() > 128
            || definition
                .filename_patterns
                .iter()
                .any(|pattern| !filename(pattern))
        {
            return Err("Invalid or duplicate language identity or file association".into());
        }
        optional_expression(&definition.first_line)?;
        let config = &definition.configuration;
        if (config.uniform_line_comments
            && config.line_comment.is_none()
            && config.block_comment.is_none())
            || config
                .line_comment
                .as_deref()
                .is_some_and(|value| !marker(value))
            || config.block_comment.as_ref().is_some_and(|value| {
                !marker(&value.open)
                    || !marker(&value.close)
                    || value.nested && value.open == value.close
            })
            || config.brackets.len() > 16
            || config
                .brackets
                .iter()
                .any(|pair| !character(pair.open) || !character(pair.close))
            || config.auto_closing_pairs.len() > 32
            || config.surrounding_pairs.len() > 32
            || config
                .surrounding_pairs
                .iter()
                .any(|pair| !character(pair.open) || !character(pair.close))
            || config.auto_closing_pairs.iter().any(|pair| {
                !character(pair.open) || !character(pair.close) || pair.not_in.len() > 6
            })
            || config.indentation.on_enter.len() > 32
        {
            return Err("Invalid language editing configuration".into());
        }
        optional_expression(&config.word_pattern)?;
        optional_expression(&config.indentation.increase)?;
        optional_expression(&config.indentation.decrease)?;
        optional_expression(&config.indentation.indent_next_line)?;
        for rule in &config.indentation.on_enter {
            expression(&rule.before)?;
            optional_expression(&rule.after)?;
        }
        for pair in &config.auto_closing_pairs {
            optional_expression(&pair.before)?;
            optional_expression(&pair.after)?;
        }
        let features = &definition.features;
        if features.snippets.len() > 128
            || features.snippets.iter().any(|snippet| {
                !text(&snippet.name, 128)
                    || !text(&snippet.prefix, 128)
                    || snippet.body.len() > 16 * 1024
                    || snippet.body.contains('\0')
            })
            || features
                .formatting
                .as_ref()
                .is_some_and(|value| !handler(value))
            || features.diagnostics.len() > 16
            || features.diagnostics.iter().any(|value| !handler(value))
        {
            return Err("Invalid language feature declaration".into());
        }
        if let Some(server) = &features.language_server
            && (!text(&server.command, 1024)
                || server.args.len() > 64
                || server
                    .args
                    .iter()
                    .any(|arg| arg.len() > 4096 || arg.contains(['\0', '\n', '\r']))
                || server.root_markers.len() > 64
                || server.root_markers.iter().any(|name| !filename(name)))
        {
            return Err("Invalid language server configuration".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod service_tests;
#[cfg(test)]
mod tests;
