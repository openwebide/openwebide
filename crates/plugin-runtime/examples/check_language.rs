//! Exercise a precompiled community language package using the public runtime.
use anyhow::{Context, Result, ensure};
use openwebide_core::plugins::{PluginManifest, PreparedPlugin, languages::*};
use openwebide_plugin_runtime::{Runtime, languages::LanguageRuntime};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};
#[path = "../../core/src/editor/syntax_contracts.rs"]
mod syntax_contracts;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let component = PathBuf::from(args.next().context("Provide a WASI component path")?);
    let manifest = PathBuf::from(args.next().context("Provide a plugin.json path")?);
    let sample = args
        .next()
        .context("Provide a sample source path, --matrix or --family-matrix")?;
    let source = if matches!(sample.as_str(), "--matrix" | "--family-matrix") {
        String::new()
    } else {
        std::fs::read_to_string(&sample)?
    };
    let bytes = std::fs::read(component)?;
    let manifest: PluginManifest = serde_json::from_slice(&std::fs::read(manifest)?)?;
    manifest.validate().map_err(anyhow::Error::msg)?;
    Runtime::new()?.validate_exports(&bytes, &manifest)?;
    let language = manifest
        .contributions
        .languages
        .iter()
        .find(|definition| {
            definition.features.syntax
                || definition.features.folding
                || definition.features.structure
        })
        .context("Package must declare a language analysis provider")?
        .id
        .clone();
    let prepared: PreparedPlugin = serde_json::from_value(serde_json::json!({
        "source":{"repository":"https://example.test/language-plugin.git","commit":"a".repeat(40),"path":"."},
        "manifest":manifest,"digest":"b".repeat(64),"host_id":"authoring-check"
    }))?;
    if matches!(sample.as_str(), "--matrix" | "--family-matrix") {
        return matrix(&bytes, &prepared, sample == "--family-matrix");
    }
    let mut runtime = LanguageRuntime::new()?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let request = AnalysisRequest {
        version: VERSION,
        document: "authoring/project/client/document".into(),
        request: 1,
        revision: 1,
        language,
        path: "sample.notes".into(),
        source_digest: source_digest(&source),
        source: SourceChange::Full {
            text: source.clone(),
        },
        tab_width: 4,
    };
    let full = LanguageInput::Analyze(request.clone());
    let output = runtime.execute("owner", &prepared, &bytes, &full, cancelled.clone())?;
    let LanguageOutput::Analyzed(response) = output else {
        anyhow::bail!("Provider returned an unexpected operation")
    };
    ensure!(
        matches!(response.status, AnalysisStatus::Ready { .. }),
        "Provider failed to analyze sample"
    );
    let analysis = openwebide_core::editor::SyntaxAnalysis::from_language_data(
        Arc::new(source.clone()),
        &request.language,
        response.analysis.context("Provider returned no analysis")?,
        &prepared.manifest.contributions.languages,
    )
    .map_err(anyhow::Error::msg)?;
    let structure = analysis
        .structure()
        .context("Provider returned no editing context")?;
    let mut document = openwebide_core::editor::Document::new(&source);
    if structure
        .configuration_at(0)
        .is_some_and(|configuration| configuration.line_comment.is_some())
    {
        ensure!(
            document
                .toggle_line_comments_with_context(structure)
                .is_ok_and(|changed| changed),
            "Declared line comments did not edit the document"
        );
        document.undo();
        ensure!(
            document.text() == source,
            "Comment undo changed the original source"
        );
    }
    if let Some(pair) = structure
        .configuration_at(source.len())
        .and_then(|configuration| configuration.auto_closing_pairs.first())
    {
        document
            .set_selections(vec![openwebide_core::editor::Selection::caret(
                source.len(),
            )])
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        document
            .type_character_with_context(pair.open, structure)
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        ensure!(
            document.text() == format!("{source}{}{}", pair.open, pair.close),
            "Declared automatic pair was not applied"
        );
        document.undo();
    }
    let mut request = request;
    request.request += 1;
    request.revision += 1;
    request.source_digest = source_digest(&format!("文{source}"));
    request.source = SourceChange::Replace {
        base_revision: 1,
        start: 0,
        end: 0,
        text: "文".into(),
    };
    let delta = LanguageInput::Analyze(request.clone());
    let output = runtime.execute("owner", &prepared, &bytes, &delta, cancelled.clone())?;
    ensure!(
        matches!(output, LanguageOutput::Analyzed(response) if matches!(response.status, AnalysisStatus::Ready { .. })),
        "Provider lost its admitted delta base"
    );
    let output = runtime.execute("other-owner", &prepared, &bytes, &delta, cancelled.clone())?;
    ensure!(
        matches!(output, LanguageOutput::Analyzed(response) if response.status == AnalysisStatus::NeedsSource),
        "Provider shared a source across owners"
    );
    runtime.execute(
        "owner",
        &prepared,
        &bytes,
        &LanguageInput::Close {
            version: VERSION,
            document: request.document.clone(),
        },
        cancelled.clone(),
    )?;
    request.request += 1;
    request.revision += 1;
    request.source = SourceChange::Replace {
        base_revision: 2,
        start: 0,
        end: 0,
        text: "文".into(),
    };
    let output = runtime.execute(
        "owner",
        &prepared,
        &bytes,
        &LanguageInput::Analyze(request),
        cancelled,
    )?;
    ensure!(
        matches!(output, LanguageOutput::Analyzed(response) if response.status == AnalysisStatus::NeedsSource),
        "Closed source remained available"
    );
    println!(
        "Validated public language exports, full source, Unicode delta, owner isolation and close recovery"
    );
    Ok(())
}

fn matrix(bytes: &[u8], prepared: &PreparedPlugin, family: bool) -> Result<()> {
    use openwebide_core::plugins::ProjectPlugin;
    let registry = LanguageRegistry::from_bindings(&[ProjectPlugin {
        id: 1,
        revision: 1,
        enabled: true,
        prepared: prepared.clone(),
    }]);
    ensure!(
        registry.errors.is_empty(),
        "Language package registration failed: {:?}",
        registry.errors
    );
    let mut runtime = LanguageRuntime::new()?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut count = 0;
    let mut covered = std::collections::BTreeSet::new();
    let large_markdown = (0..1_000)
        .map(|index| format!("Paragraph {index}: **文😀** and `code`.\n\n"))
        .collect::<String>();
    let large_rust = "fn example() { let value = 42; }\n".repeat(10_000);
    let cases = syntax_contracts::LANGUAGE_CASES
        .iter()
        .map(|&(path, source, fold)| (path, source, Some(fold), None))
        .chain(
            syntax_contracts::CONFIG_CASES
                .iter()
                .map(|&(path, source)| (path, source, None, None)),
        )
        .chain(
            syntax_contracts::LITERAL_CASES
                .iter()
                .map(|&(path, source, literal, code)| (path, source, None, Some((literal, code)))),
        )
        .chain([
            (
                "fallback.txt",
                "#region group\n文😀\n#endregion\nafter\n",
                Some(FoldRange {
                    start_line: 0,
                    end_line: 2,
                }),
                None,
            ),
            (
                "unknown.custom-extension",
                "#region group\n文😀\n#endregion\nafter\n",
                Some(FoldRange {
                    start_line: 0,
                    end_line: 2,
                }),
                None,
            ),
        ]);
    let extra = [
        ("query.sql", "SELECT '文😀';\n", None, None),
        ("body.markdown-inline", "**文😀** and `code`", None, None),
        (
            "style.scss",
            ".main {\n    $color: #fff;\n    color: $color;\n}\n",
            Some(FoldRange {
                start_line: 0,
                end_line: 3,
            }),
            None,
        ),
        (
            "embedded.html",
            "<script>const text = \"文😀\";</script>\n<style>a { color: red; }</style>\n",
            None,
            None,
        ),
        ("custom-fences.md", "```notes\n文😀\n```\n", None, None),
        ("large.md", large_markdown.as_str(), None, None),
        ("large.rs", large_rust.as_str(), None, None),
    ];
    for (path, source, fold, literal) in cases.chain(extra.into_iter().filter(|_| family)) {
        let id = if path == "body.markdown-inline" {
            "markdown-inline".into()
        } else {
            format!("{:?}", openwebide_core::highlight::language_from_path(path))
                .to_ascii_lowercase()
        };
        if family
            && !prepared
                .manifest
                .contributions
                .languages
                .iter()
                .any(|definition| definition.id == id)
        {
            continue;
        }
        for source in [source.to_string(), source.replace('\n', "\r\n")] {
            let LanguageSelection::Selected(provider) = registry.select(
                path,
                &source,
                (path == "body.markdown-inline").then_some("markdown-inline"),
                &LanguagePreferences::default(),
            ) else {
                anyhow::bail!("No unambiguous provider for {path}")
            };
            if family {
                ensure!(
                    provider.definition.id == id,
                    "{path}: another language claimed this fixture"
                );
            }
            covered.insert(provider.definition.id.clone());
            let request = AnalysisRequest {
                version: VERSION,
                document: format!("fixture-{count}"),
                request: 1,
                revision: 1,
                language: provider.definition.id.clone(),
                path: path.into(),
                source_digest: source_digest(&source),
                source: SourceChange::Full {
                    text: source.clone(),
                },
                tab_width: 4,
            };
            let output = runtime
                .execute(
                    "matrix",
                    prepared,
                    bytes,
                    &LanguageInput::Analyze(request),
                    cancelled.clone(),
                )
                .with_context(|| format!("{path} component execution"))?;
            let LanguageOutput::Analyzed(response) = output else {
                anyhow::bail!("Unexpected {path} operation")
            };
            ensure!(
                matches!(response.status, AnalysisStatus::Ready { .. }),
                "{path}: {:?}",
                response.status
            );
            let data = response.analysis.context("Missing analysis")?;
            if path == "large.md" {
                ensure!(data.highlights.is_some(), "Missing Markdown highlights");
                ensure!(
                    data.structure
                        .as_ref()
                        .context("Missing Markdown structure")?
                        .scopes
                        .is_empty(),
                    "Inline Markdown must stay inside its family"
                );
            }
            if family && matches!(path, "embedded.html" | "custom-fences.md") {
                let scopes = &data
                    .structure
                    .as_ref()
                    .context("Missing delegated scopes")?
                    .scopes;
                let expected = if path == "embedded.html" {
                    vec!["javascript", "css"]
                } else {
                    vec!["notes"]
                };
                ensure!(
                    scopes
                        .iter()
                        .map(|scope| scope.language.as_str())
                        .collect::<Vec<_>>()
                        == expected,
                    "{path}: embedded providers were not delegated"
                );
            }
            if let Some(fold) = fold {
                ensure!(
                    data.folds.contains(&fold),
                    "{path}: expected fold is absent"
                );
            }
            let analysis = openwebide_core::editor::SyntaxAnalysis::from_language_data(
                Arc::new(source.clone()),
                &provider.definition.id,
                data,
                &prepared.manifest.contributions.languages,
            )
            .map_err(anyhow::Error::msg)?;
            let structure = analysis.structure().context("Missing editing context")?;
            if let Some((literal, code)) = literal {
                ensure!(
                    !structure.is_code(source.find(literal).context("Missing literal fixture")?),
                    "{path}: literal was treated as code"
                );
                if let Some(code) = code {
                    ensure!(
                        structure.is_code(source.find(code).context("Missing code fixture")?),
                        "{path}: embedded code was treated as a literal"
                    );
                }
            }
            if let Some(rows) = analysis.highlights() {
                if path == "large.md" {
                    ensure!(
                        rows.iter()
                            .step_by(2)
                            .take(1_000)
                            .filter(|row| row.iter().any(|token| token.kind
                                == openwebide_core::highlight::TokenKind::String
                                && token.text.contains("code")))
                            .count()
                            == 1_000,
                        "Every Markdown paragraph must receive inline syntax"
                    );
                }
                for (row, source) in rows.iter().zip(source.split('\n')) {
                    ensure!(
                        row.iter()
                            .map(|token| token.text.as_str())
                            .collect::<String>()
                            == source,
                        "{path}: painted source changed"
                    );
                }
            }
            count += 1;
        }
    }
    ensure!(count > 0, "No language fixtures were exercised");
    if family {
        let declared: std::collections::BTreeSet<_> = prepared
            .manifest
            .contributions
            .languages
            .iter()
            .filter(|definition| {
                definition.features.syntax
                    || definition.features.folding
                    || definition.features.structure
            })
            .map(|definition| definition.id.clone())
            .collect();
        ensure!(
            covered == declared,
            "Some language IDs lack real component coverage: declared {declared:?}, covered {covered:?}"
        );
    }
    println!(
        "Validated {count} real component cases: recognition, syntax, folds, embedded regions, Unicode/CRLF and core reconstruction"
    );
    Ok(())
}
