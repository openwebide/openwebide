//! An explicit test host for the language packages' public protocol. This module
//! and its engine dependency exist only with test-support; production clients
//! never construct an engine or fall back to one when a host fails.
use super::fake_backend::LanguageHandler;
use openwebide_core::plugins::{
    PackageFile, PackageFileKind, ProjectPlugin, languages::LanguageDefinition, validate_files,
};
use openwebide_language_syntax::{LanguageGrammar, LanguageSyntax, SyntaxLanguage};
use serde::Deserialize;
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};
mod native;
pub use native::NativeLanguageHost;

pub fn agent_tools(
    api: crate::backend::Api,
    host: crate::project_host::ProjectHost,
    session: i64,
    scope: openwebide_core::plugins::languages::tools::LanguageToolScope,
    current: impl Fn() -> bool + Clone + 'static,
) -> Result<std::sync::Arc<dyn openwebide_agent::intelligence::CodeIntelligence>, String> {
    crate::plugin_actions::agent_language_tools(api, host, session, scope, current)
}

#[derive(Deserialize)]
struct Snapshot {
    plugins: Vec<Package>,
}
#[derive(Deserialize)]
struct Package {
    path: String,
    files: Vec<File>,
}
#[derive(Deserialize)]
struct File {
    path: String,
    content: String,
    executable: bool,
}

pub fn standard_binding() -> ProjectPlugin {
    // This synthetic multi-language host keeps existing editor contracts small.
    // Its declarations come from the actual independent defaults; native host
    // checks separately execute each family's real WASI component.
    static DEFINITIONS: std::sync::LazyLock<Vec<LanguageDefinition>> =
        std::sync::LazyLock::new(|| {
            let snapshot: Snapshot =
                serde_json::from_str(include_str!("../../../bridge/bundled/plugins.json")).unwrap();
            let mut definitions = BTreeMap::new();
            for package in snapshot.plugins {
                let files: Vec<_> = package
                    .files
                    .into_iter()
                    .map(|file| PackageFile {
                        path: file.path,
                        content: file.content.into_bytes(),
                        kind: if file.executable {
                            PackageFileKind::Executable
                        } else {
                            PackageFileKind::File
                        },
                    })
                    .collect();
                for mut definition in validate_files(&files).unwrap().contributions.languages {
                    // This API 5 fixture implements pure analysis, not native
                    // processes. Real host checks retain and execute the actual
                    // API 6/7 server declarations and document language IDs.
                    definition.features.language_server = None;
                    assert!(
                        definitions
                            .insert(definition.id.clone(), definition)
                            .is_none(),
                        "duplicate default language in {}",
                        package.path
                    );
                }
            }
            assert_eq!(definitions.len(), 26);
            definitions.into_values().collect()
        });
    let binding: ProjectPlugin = serde_json::from_value(serde_json::json!({
        "id":900,"revision":1,"enabled":true,"prepared":{
            "source":{"repository":"https://example.test/reference-languages.git","commit":"a".repeat(40),"path":"."},
            "digest":"b".repeat(64),"host_id":"fixture-server",
            "manifest":{"schemaVersion":1,"publisher":"example","name":"reference-languages","version":"0.1.0",
                "displayName":"Reference languages","description":"Explicit public language test host","license":"MIT",
                "compatibility":{"pluginApi":5},"executable":{"manifest":"Cargo.toml","library":"reference_languages","sdkVersion":"0.1.0","capabilities":[]},
                "contributions":{"skills":[],"languages":&*DEFINITIONS}}
        }
    })).unwrap();
    binding.prepared.validate().unwrap();
    binding
}

/// Emulates the component behind a test transport using the same public toolkit
/// and the actual pinned package declarations. Native tests separately execute
/// each family's compiled WASI artifact, including the complete fixture matrix.
pub fn standard_handler(binding: &ProjectPlugin) -> LanguageHandler {
    use SyntaxLanguage::*;
    let bindings = [
        ("rust", Rust),
        ("python", Python),
        ("javascript", JavaScript),
        ("jsx", Jsx),
        ("typescript", TypeScript),
        ("tsx", Tsx),
        ("java", Java),
        ("csharp", CSharp),
        ("php", Php),
        ("json", Json),
        ("jsonc", Json),
        ("html", Html),
        ("css", Css),
        ("scss", Css),
        ("markdown", Markdown),
        ("ini", Ini),
        ("xml", Xml),
        ("shell", Shell),
        ("toml", Toml),
        ("yaml", Yaml),
        ("sql", Sql),
        ("c", C),
        ("cpp", Cpp),
        ("go", Go),
        ("markdown-inline", MarkdownInline),
        ("plain", Plain),
    ]
    .into_iter()
    .map(|(id, language)| (id.to_string(), LanguageGrammar::standard(language)))
    .collect::<BTreeMap<_, _>>();
    let engine = Rc::new(RefCell::new(
        LanguageSyntax::new_delegated(
            binding.prepared.manifest.contributions.languages.clone(),
            bindings,
        )
        .unwrap(),
    ));
    Rc::new(move |call| {
        let engine = engine.clone();
        Box::pin(async move {
            let input = serde_json::from_str(&call.arguments).map_err(|error| error.to_string())?;
            engine.borrow_mut().handle(input)
        })
    })
}
