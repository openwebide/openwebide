//! File selection for complete project-source observations. It uses published
//! associations and declared inputs, never language or package identities.
use super::{
    LanguageDefinition, ProjectInputPatterns, ProjectInputs, intelligence, validate_definitions,
};
use crate::plugins::PreparedPlugin;
use regex::{RegexSet, RegexSetBuilder};
use std::collections::BTreeSet;

#[derive(Clone, Debug)]
pub struct ProjectFiles {
    extensions: BTreeSet<String>,
    names: BTreeSet<String>,
    patterns: RegexSet,
    inputs: Option<ProjectInputPatterns>,
    all_files: bool,
    signature: String,
}
impl ProjectFiles {
    pub fn for_server(prepared: &PreparedPlugin, language: &str) -> Result<Self, String> {
        let selected = intelligence::declared_server(prepared, language)?.shared_configuration();
        let definitions = prepared
            .manifest
            .contributions
            .languages
            .iter()
            .filter(|definition| {
                definition
                    .features
                    .language_server
                    .as_ref()
                    .is_some_and(|server| server.shared_configuration() == selected)
            })
            .cloned()
            .collect::<Vec<_>>();
        Self::new(&definitions, selected.project_inputs.as_ref())
    }

    pub fn new(
        definitions: &[LanguageDefinition],
        inputs: Option<&ProjectInputs>,
    ) -> Result<Self, String> {
        validate_definitions(definitions)?;
        if definitions.is_empty() {
            return Err("A project-source observation requires language associations".into());
        }
        let extensions = definitions
            .iter()
            .flat_map(|definition| &definition.extensions)
            .map(|value| value.to_ascii_lowercase())
            .collect();
        let names = definitions
            .iter()
            .flat_map(|definition| &definition.filenames)
            .map(|value| value.to_ascii_lowercase())
            .collect();
        let patterns = definitions
            .iter()
            .flat_map(|definition| &definition.filename_patterns)
            .map(|pattern| {
                let body = pattern
                    .chars()
                    .map(|ch| match ch {
                        '*' => ".*".into(),
                        '?' => ".".into(),
                        _ => regex::escape(&ch.to_string()),
                    })
                    .collect::<String>();
                format!("^{body}$")
            })
            .collect::<Vec<_>>();
        let patterns = RegexSetBuilder::new(patterns)
            .case_insensitive(true)
            .dot_matches_new_line(true)
            .size_limit(1024 * 1024)
            .dfa_size_limit(1024 * 1024)
            .build()
            .map_err(|_| "Project-source filename patterns exceed their limit")?;
        // A content association or manual selection can recognize any filename.
        // Include that superset rather than silently missing a dependency.
        let all_files = definitions.iter().any(|definition| {
            definition.first_line.is_some()
                || (definition.extensions.is_empty()
                    && definition.filenames.is_empty()
                    && definition.filename_patterns.is_empty())
        });
        let signature =
            serde_json::to_string(&(definitions, inputs)).map_err(|error| error.to_string())?;
        Ok(Self {
            extensions,
            names,
            patterns,
            inputs: inputs.map(ProjectInputPatterns::new).transpose()?,
            all_files,
            signature,
        })
    }

    pub fn signature(&self) -> &str {
        &self.signature
    }

    pub fn includes_directory(&self, path: &str) -> bool {
        !crate::vfs::private_workspace_path(path)
            && !path
                .split('/')
                .any(|part| part.eq_ignore_ascii_case(".git"))
    }

    pub fn includes_file(&self, path: &str) -> bool {
        if !self.includes_directory(path) {
            return false;
        }
        let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
        self.all_files
            || self.names.contains(&name)
            || self.patterns.is_match(&name)
            || self.extensions.iter().any(|extension| {
                name.strip_suffix(extension)
                    .is_some_and(|base| base.ends_with('.'))
            })
            || self
                .inputs
                .as_ref()
                .is_some_and(|inputs| inputs.includes_file(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn definition(value: serde_json::Value) -> LanguageDefinition {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn published_associations_and_inputs_define_one_conservative_source_set() {
        let files=ProjectFiles::new(&[
            definition(serde_json::json!({"id":"script","name":"Script","extensions":["ts","jsx"],"filenames":["Scriptfile"],"filenamePatterns":["*.script.inc"]})),
            definition(serde_json::json!({"id":"other","name":"Other","extensions":["other"]})),
        ],Some(&ProjectInputs { files:vec!["config/*.json".into()],exclude:vec!["config/private.json".into()]})).unwrap();
        for path in [
            "src/file.TS",
            "component.jsx",
            "x.other",
            "dir/SCRIPTFILE",
            "lib/example.script.inc",
            "config/tool.json",
        ] {
            assert!(files.includes_file(path), "{path}");
        }
        for path in [
            "file.ts.old",
            "filetxts",
            "calls",
            "config/private.json",
            ".git/config",
            ".spin/state",
        ] {
            assert!(!files.includes_file(path), "{path}");
        }
        assert!(files.includes_directory("nested/generated"));
        assert!(!files.includes_directory("nested/.GIT/objects"));
    }

    #[test]
    fn content_recognition_does_not_omit_extensionless_sources() {
        let files = ProjectFiles::new(
            &[definition(
                serde_json::json!({"id":"script","name":"Script","firstLine":"^#!.*script"}),
            )],
            None,
        )
        .unwrap();
        assert!(files.includes_file("bin/extensionless"));
        assert!(!files.includes_file(".spin/private"));
    }

    #[test]
    fn manually_selected_languages_without_associations_include_unknown_sources() {
        let files = ProjectFiles::new(
            &[
                definition(serde_json::json!({"id":"manual","name":"Manual"})),
                definition(serde_json::json!({"id":"named","name":"Named","extensions":["named"]})),
            ],
            None,
        )
        .unwrap();
        assert!(files.includes_file("nested/extensionless"));
        assert!(files.includes_file("dependency.unknown"));
        assert!(!files.includes_file(".git/config"));
        assert!(!files.includes_file(".spin/private"));
    }

    #[test]
    fn related_dialects_select_the_same_source_inventory_and_other_servers_stay_separate() {
        let mut prepared = crate::plugins::testing::receipt();
        prepared.manifest.compatibility.plugin_api = 9;
        prepared.manifest.contributions.languages = vec![
            definition(
                serde_json::json!({"id":"script","name":"Script","extensions":["js"],"features":{"languageServer":{"command":"shared-lsp","languageId":"javascript"}}}),
            ),
            definition(
                serde_json::json!({"id":"typed","name":"Typed","extensions":["ts"],"features":{"languageServer":{"command":"shared-lsp","languageId":"typescript"}}}),
            ),
            definition(
                serde_json::json!({"id":"other","name":"Other","extensions":["other"],"features":{"languageServer":{"command":"different-lsp"}}}),
            ),
        ];
        let script = ProjectFiles::for_server(&prepared, "script").unwrap();
        let typed = ProjectFiles::for_server(&prepared, "typed").unwrap();
        assert_eq!(script.signature(), typed.signature());
        assert!(script.includes_file("dependency.ts"));
        assert!(typed.includes_file("dependency.js"));
        assert!(!script.includes_file("file.other"));
        let other = ProjectFiles::for_server(&prepared, "other").unwrap();
        assert!(other.includes_file("file.other"));
        assert!(!other.includes_file("file.ts"));
    }
}
