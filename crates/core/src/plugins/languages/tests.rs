use super::*;
use crate::plugins::{PluginManifest, ProjectPlugin};
use serde_json::json;

fn binding(name: &str, host: &str, definition: serde_json::Value) -> ProjectPlugin {
    serde_json::from_value(json!({
        "id":1,"revision":7,"enabled":true,
        "prepared":{
            "source":{"repository":"https://example.test/plugins.git","commit":"a".repeat(40),"path":format!("plugins/{name}")},
            "manifest":{
                "schemaVersion":1,"publisher":"example","name":name,"version":"1.0.0",
                "displayName":name,"description":"Language contribution contract","license":"MIT",
                "compatibility":{"pluginApi":5},"contributions":{"skills":[],"languages":[definition]}
            },
            "digest":"b".repeat(64),"host_id":host
        }
    })).unwrap()
}

fn rust() -> serde_json::Value {
    json!({"id":"rust","name":"Rust","extensions":["rs"],"configuration":{"lineComment":"//"}})
}

#[test]
fn declarations_require_the_versioned_contract_and_analysis_requires_a_component() {
    let mut manifest: PluginManifest = binding("rust", "host", rust()).prepared.manifest;
    manifest.validate().unwrap();
    for api in [1, 2, 3, 4] {
        manifest.compatibility.plugin_api = api;
        assert!(manifest.validate().is_err());
    }
    manifest.compatibility.plugin_api = 5;
    manifest.contributions.languages[0].features.syntax = true;
    assert!(manifest.validate().is_err());
    manifest.executable = Some(crate::plugins::RustPlugin {
        manifest: "Cargo.toml".into(),
        library: "language_fixture".into(),
        sdk_version: "0.1.0".into(),
        capabilities: vec![],
    });
    manifest.validate().unwrap();
    manifest
        .contributions
        .languages
        .push(manifest.contributions.languages[0].clone());
    assert!(manifest.validate().is_err());
}

#[test]
fn recognition_conflicts_and_preferences_are_identical_on_both_hosts() {
    for host in ["server-host", "paired-host"] {
        let mut bindings = vec![
            binding("first", host, rust()),
            binding("second", host, rust()),
        ];
        let registry = LanguageRegistry::from_bindings(&bindings);
        assert!(registry.errors.is_empty());
        assert!(
            matches!(registry.select("src/main.rs","",None,&LanguagePreferences::default()),LanguageSelection::Ambiguous(values) if values.len()==2)
        );
        let mut preferences = LanguagePreferences::default();
        preferences.providers.insert(
            "rust".into(),
            ProviderPreference::Provider {
                namespace: registry.providers[1].namespace.clone(),
            },
        );
        let LanguageSelection::Selected(selected) =
            registry.select("src/main.rs", "", None, &preferences)
        else {
            panic!("explicit provider must win");
        };
        assert_eq!(selected.host_id, host);
        assert_eq!(
            selected.definition.configuration.line_comment.as_deref(),
            Some("//")
        );
        bindings[1].enabled = false;
        let registry = LanguageRegistry::from_bindings(&bindings);
        assert!(matches!(
            registry.select("src/main.rs", "", None, &preferences),
            LanguageSelection::Unavailable(_)
        ));
        preferences
            .providers
            .insert("rust".into(), ProviderPreference::Disabled);
        assert_eq!(
            registry.select("src/main.rs", "", None, &preferences),
            LanguageSelection::Plain
        );
        assert_eq!(
            registry.select("unknown.file", "", None, &LanguagePreferences::default()),
            LanguageSelection::Plain
        );
    }
}

#[test]
fn filenames_compound_extensions_and_bounded_first_lines_have_defined_precedence() {
    let bindings = [
        binding(
            "text",
            "host",
            json!({"id":"text","name":"Text","extensions":["txt"]}),
        ),
        binding(
            "special",
            "host",
            json!({"id":"special","name":"Special","filenames":["special.txt"]}),
        ),
        binding(
            "typescript",
            "host",
            json!({"id":"typescript","name":"TypeScript","aliases":["ts"],"extensions":["ts"]}),
        ),
        binding(
            "declarations",
            "host",
            json!({"id":"declarations","name":"Declarations","extensions":["d.ts"]}),
        ),
        binding(
            "python",
            "host",
            json!({"id":"python","name":"Python","firstLine":"^#!.*python"}),
        ),
    ];
    let registry = LanguageRegistry::from_bindings(&bindings);
    for (path, line, expected) in [
        ("special.txt", "", "special"),
        ("index.d.ts", "", "declarations"),
        ("main.TS", "#!/usr/bin/python", "typescript"),
        ("script", "#!/usr/bin/python3", "python"),
    ] {
        let LanguageSelection::Selected(provider) =
            registry.select(path, line, None, &LanguagePreferences::default())
        else {
            panic!("missing {path}");
        };
        assert_eq!(provider.definition.id, expected);
    }
    assert!(
        matches!(registry.select("","",Some("ts"),&LanguagePreferences::default()),LanguageSelection::Selected(provider) if provider.definition.id=="typescript")
    );
    assert_eq!(
        registry.select(
            "script",
            "no signature\n#!/usr/bin/python",
            None,
            &LanguagePreferences::default()
        ),
        LanguageSelection::Plain
    );
    assert_eq!(
        registry.select("文😀", "", None, &LanguagePreferences::default()),
        LanguageSelection::Plain
    );
}

#[test]
fn replacing_or_disabling_packages_changes_registered_source_ownership() {
    let mut value = binding("rust", "paired", rust());
    let original = LanguageRegistry::from_bindings(&[value.clone()])
        .providers
        .remove(0);
    value.revision += 1;
    value.prepared.digest = "c".repeat(64);
    value.prepared.host_id = "reconnected".into();
    let changed = LanguageRegistry::from_bindings(&[value.clone()])
        .providers
        .remove(0);
    assert_eq!(original.namespace, changed.namespace);
    assert_ne!(original.digest, changed.digest);
    assert_ne!(
        original.installation_revision,
        changed.installation_revision
    );
    assert_ne!(original.host_id, changed.host_id);
    value.enabled = false;
    assert!(
        LanguageRegistry::from_bindings(&[value])
            .providers
            .is_empty()
    );
}

#[test]
fn invalid_language_payloads_cannot_activate_a_provider() {
    for definition in [
        json!({"id":"../rust","name":"Bad"}),
        json!({"id":"rust","name":"Bad","extensions":["../rs"]}),
        json!({"id":"rust","name":"Bad","filenames":["../file.rs"]}),
        json!({"id":"rust","name":"Bad","firstLine":"["}),
        json!({"id":"rust","name":"Bad","configuration":{"lineComment":"//\n"}}),
        json!({"id":"rust","name":"Bad","configuration":{"uniformLineComments":true}}),
        json!({"id":"rust","name":"Bad","configuration":{"wordPattern":"x".repeat(1025)}}),
    ] {
        let binding = binding("invalid", "host", definition);
        assert!(binding.prepared.manifest.validate().is_err());
        assert!(
            LanguageRegistry::from_bindings(&[binding])
                .providers
                .is_empty()
        );
    }
    assert!(
        serde_json::from_value::<LanguageDefinition>(
            json!({"id":"rust","name":"Rust","script":"execute on client"})
        )
        .is_err()
    );
}

#[test]
fn formatter_preference_never_silently_substitutes_a_different_provider() {
    let definitions = [binding(
        "formatter",
        "host",
        json!({"id":"rust","name":"Rust","extensions":["rs"],"features":{"formatting":{"id":"format","title":"Format"}}}),
    )];
    let registry = LanguageRegistry::from_bindings(&definitions);
    assert!(matches!(
        registry.formatter("rust", &LanguagePreferences::default()),
        LanguageSelection::Selected(_)
    ));
    let mut prefs = LanguagePreferences::default();
    prefs.formatters.insert(
        "rust".into(),
        ProviderPreference::Provider {
            namespace: "missing".into(),
        },
    );
    assert_eq!(
        registry.formatter("rust", &prefs),
        LanguageSelection::Unavailable("missing".into())
    );
    prefs
        .formatters
        .insert("rust".into(), ProviderPreference::Disabled);
    assert_eq!(registry.formatter("rust", &prefs), LanguageSelection::Plain);
}

#[test]
fn a_formatter_only_package_does_not_displace_syntax_or_editing_rules() {
    let mut syntax = rust();
    syntax["features"] = json!({"syntax":true});
    let mut parser = binding("syntax", "host", syntax);
    parser.prepared.manifest.executable = Some(crate::plugins::RustPlugin {
        manifest: "Cargo.toml".into(),
        library: "syntax".into(),
        sdk_version: "0.1.0".into(),
        capabilities: vec![],
    });
    let formatter = binding(
        "formatter",
        "host",
        json!({"id":"rust","name":"Rust formatter","extensions":["rs"],"features":{"formatting":{"id":"format","title":"Format Rust"}}}),
    );
    let registry = LanguageRegistry::from_bindings(&[parser, formatter]);
    let preferences = LanguagePreferences::default();
    let LanguageSelection::Selected(provider) = registry.select("main.rs", "", None, &preferences)
    else {
        panic!("syntax must remain selected")
    };
    assert_eq!(provider.plugin_label, "syntax");
    let LanguageSelection::Selected(provider) = registry.formatter("rust", &preferences) else {
        panic!("formatter must route independently")
    };
    assert_eq!(provider.plugin_label, "formatter");
}

#[test]
fn wildcard_only_matches_preserve_specific_providers_and_missing_preferences() {
    for host in ["server-host", "paired-host"] {
        let mut bindings = vec![
            binding(
                "fallback",
                host,
                json!({"id":"generic","name":"Generic","filenamePatterns":["*"],"configuration":{"lineComment":"#"}}),
            ),
            binding("rust", host, rust()),
            binding(
                "special",
                host,
                json!({"id":"special","name":"Special","filenames":["special.rs"]}),
            ),
            binding(
                "pattern",
                host,
                json!({"id":"pattern","name":"Pattern","filenamePatterns":[".notes.*"]}),
            ),
            binding(
                "script",
                host,
                json!({"id":"script","name":"Script","firstLine":"^#!.*python"}),
            ),
        ];
        for reverse in [false, true] {
            if reverse {
                bindings.reverse();
            }
            let registry = LanguageRegistry::from_bindings(&bindings);
            for (path, line, expected) in [
                ("unknown.文😀", "", "generic"),
                ("src/main.rs", "", "rust"),
                ("special.rs", "", "special"),
                (".notes.local", "", "pattern"),
                ("script", "#!/usr/bin/python3", "script"),
            ] {
                let LanguageSelection::Selected(provider) =
                    registry.select(path, line, None, &LanguagePreferences::default())
                else {
                    panic!("{path}: selected provider required")
                };
                assert_eq!(provider.definition.id, expected, "{host}: {path}");
            }
            let mut preferences = LanguagePreferences::default();
            preferences.providers.insert(
                "rust".into(),
                ProviderPreference::Provider {
                    namespace: "plugin:missing/rust".into(),
                },
            );
            assert_eq!(
                registry.select("src/main.rs", "", None, &preferences),
                LanguageSelection::Unavailable("plugin:missing/rust".into())
            );
            preferences
                .providers
                .insert("generic".into(), ProviderPreference::Disabled);
            assert_eq!(
                registry.select("unknown.file", "", None, &preferences),
                LanguageSelection::Plain
            );
        }
        bindings.push(binding("other-fallback", host, json!({"id":"other","name":"Other","filenamePatterns":["?*"],"configuration":{"lineComment":";"}})));
        let registry = LanguageRegistry::from_bindings(&bindings);
        assert!(
            matches!(registry.select("unknown.file", "", None, &LanguagePreferences::default()), LanguageSelection::Ambiguous(values) if values.len()==2)
        );
    }
}
