//! Source-owned language data and declarative rules; no grammar execution.
use super::{RegionKind, Structure};
use crate::{highlight::Language, plugins::languages as protocol};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Debug)]
pub(super) struct LanguageRules {
    root: protocol::LanguageConfiguration,
    scopes: Vec<protocol::LanguageConfiguration>,
    root_structure: bool,
    scope_structure: Vec<bool>,
}

impl Structure {
    pub(in crate::editor) fn closing_at(&self, position: usize, ch: char) -> Option<char> {
        self.configuration_at(position).map_or_else(
            || super::closing(ch),
            |configuration| {
                configuration
                    .brackets
                    .iter()
                    .find(|pair| pair.open == ch)
                    .map(|pair| pair.close)
            },
        )
    }

    pub(in crate::editor) fn is_closing_at(&self, position: usize, ch: char) -> bool {
        self.configuration_at(position).map_or_else(
            || matches!(ch, ')' | ']' | '}'),
            |configuration| configuration.brackets.iter().any(|pair| pair.close == ch),
        )
    }

    pub(in crate::editor) fn automatic_pair(
        &self,
        position: usize,
        end: usize,
        ch: char,
    ) -> Option<&protocol::AutoClosingPair> {
        let configuration = self.configuration_at(position)?;
        let kind = self
            .region_at(position)
            .filter(|(range, complete, _)| {
                (position > range.start || self.opaque_starts.binary_search(&position).is_ok())
                    && (position < range.end || !complete && position == range.end)
            })
            .map(|(_, _, kind)| match kind {
                RegionKind::String => protocol::RegionKind::String,
                RegionKind::Template => protocol::RegionKind::Template,
                RegionKind::Text => protocol::RegionKind::Text,
                RegionKind::LineComment => protocol::RegionKind::LineComment,
                RegionKind::BlockComment => protocol::RegionKind::BlockComment,
                RegionKind::Regex => protocol::RegionKind::Regex,
            });
        let before = &self.source[..position];
        let before = before.rsplit('\n').next().unwrap_or(before);
        let before = &before[before.ceil_char_boundary(before.len().saturating_sub(8192))..];
        let after = &self.source[end..];
        let after = after.split('\n').next().unwrap_or(after);
        let after = &after[..after.floor_char_boundary(after.len().min(8192))];
        configuration.auto_closing_pairs.iter().find(|pair| {
            pair.open == ch
                && (self.available || pair.not_in.is_empty())
                && kind.is_none_or(|kind| !pair.not_in.contains(&kind))
                && pair
                    .before
                    .as_ref()
                    .is_none_or(|expression| matches_expression(expression, before))
                && pair
                    .after
                    .as_ref()
                    .is_none_or(|expression| matches_expression(expression, after))
        })
    }

    pub(in crate::editor) fn quote_closes_at_character(&self, position: usize, ch: char) -> bool {
        self.protected.iter().any(|(range, complete, kind)| {
            *complete
                && matches!(kind, RegionKind::String | RegionKind::Template)
                && range.end == position + ch.len_utf8()
        })
    }
    pub fn from_language_configuration(
        source: Arc<String>,
        language: &str,
        definitions: &[protocol::LanguageDefinition],
    ) -> Result<Self, String> {
        protocol::validate_definitions(definitions)?;
        let definition = definitions
            .iter()
            .find(|definition| definition.id == language)
            .ok_or("Language has no declared configuration")?;
        let mut result = Self::plain(source);
        result.language_rules = Some(Arc::new(LanguageRules {
            root: definition.configuration.clone(),
            scopes: vec![],
            root_structure: definition.features.structure,
            scope_structure: vec![],
        }));
        result.available = false;
        Ok(result)
    }
    pub fn from_language_data(
        source: Arc<String>,
        data: protocol::Structure,
        definitions: &[protocol::LanguageDefinition],
    ) -> Result<Self, String> {
        protocol::validate_definitions(definitions)?;
        protocol::validate_analysis(
            &protocol::SyntaxAnalysis {
                highlights: None,
                folds: vec![],
                structure: Some(data.clone()),
            },
            &source,
            &data.language,
        )?;
        let configurations: BTreeMap<_, _> = definitions
            .iter()
            .map(|definition| (definition.id.as_str(), &definition.configuration))
            .collect();
        let root = configurations
            .get(data.language.as_str())
            .ok_or("Language structure has no declared root configuration")?;
        if root.uniform_line_comments
            && data.scopes.iter().any(|scope| {
                configurations
                    .get(scope.language.as_str())
                    .is_none_or(|configuration| {
                        configuration.line_comment != root.line_comment
                            || (root.line_comment.is_none()
                                && configuration.block_comment != root.block_comment)
                    })
            })
        {
            return Err("Embedded scopes contradict uniform line-comment rules".into());
        }
        let rules = LanguageRules {
            root: (*root).clone(),
            root_structure: definitions
                .iter()
                .find(|definition| definition.id == data.language)
                .is_some_and(|definition| definition.features.structure),
            scope_structure: data
                .scopes
                .iter()
                .map(|scope| {
                    definitions
                        .iter()
                        .find(|definition| definition.id == scope.language)
                        .is_some_and(|definition| definition.features.structure)
                })
                .collect(),
            scopes: data
                .scopes
                .iter()
                .map(|scope| {
                    configurations
                        .get(scope.language.as_str())
                        .map_or_else(protocol::LanguageConfiguration::default, |rules| {
                            (*rules).clone()
                        })
                })
                .collect(),
        };
        Ok(Self {
            source,
            language: Language::Plain,
            scopes: data
                .scopes
                .into_iter()
                .map(|scope| (scope.range.start..scope.range.end, Language::Plain))
                .collect(),
            selection_ranges: data
                .selections
                .into_iter()
                .map(|range| range.start..range.end)
                .collect(),
            opaque_starts: data.opaque_starts,
            protected: data
                .protected
                .into_iter()
                .map(|region| {
                    (
                        region.range.start..region.range.end,
                        region.complete,
                        match region.kind {
                            protocol::RegionKind::String => RegionKind::String,
                            protocol::RegionKind::Template => RegionKind::Template,
                            protocol::RegionKind::Text => RegionKind::Text,
                            protocol::RegionKind::LineComment => RegionKind::LineComment,
                            protocol::RegionKind::BlockComment => RegionKind::BlockComment,
                            protocol::RegionKind::Regex => RegionKind::Regex,
                        },
                    )
                })
                .collect(),
            brackets: data
                .brackets
                .into_iter()
                .map(|bracket| (bracket.offset, bracket.character, bracket.partner))
                .collect(),
            available: true,
            language_rules: Some(Arc::new(rules)),
        })
    }

    pub fn configuration_at(&self, position: usize) -> Option<&protocol::LanguageConfiguration> {
        let rules = self.language_rules.as_ref()?;
        Some(
            self.language_scope_at(position, true)
                .map_or(&rules.root, |index| &rules.scopes[index]),
        )
    }

    pub(in crate::editor) fn declarative_indentation_at(&self, position: usize) -> bool {
        self.language_rules.as_ref().is_some_and(|rules| {
            !self
                .language_scope_at(position, true)
                .map_or(rules.root_structure, |index| rules.scope_structure[index])
        })
    }

    /// A core plain-text context uses no language recognition or lexical scan.
    pub fn plain(source: Arc<String>) -> Self {
        Self {
            source,
            language: Language::Plain,
            scopes: vec![],
            selection_ranges: vec![],
            opaque_starts: vec![],
            available: true,
            protected: vec![],
            brackets: vec![],
            language_rules: None,
        }
    }
}

pub(in crate::editor) fn matches_expression(expression: &str, text: &str) -> bool {
    regex::RegexBuilder::new(expression)
        .size_limit(1024 * 1024)
        .dfa_size_limit(1024 * 1024)
        .build()
        .is_ok_and(|expression| expression.is_match(text))
}

#[cfg(test)]
mod tests;
