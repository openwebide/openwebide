//! Real HTML/CSS/SCSS/JSON/JSONC services through the shared native host.
use anyhow::{Context, Result, ensure};
use openwebide_bridge::languages::host::{Host, Scope};
use openwebide_core::plugins::languages::{
    intelligence::{self as protocol, Query, Response, results},
    source_digest,
};
use serde_json::json;
use std::{path::Path, time::Duration};

struct Fixture {
    host: Host,
    scope: Scope,
    sequence: u64,
}
impl Fixture {
    fn new(root: &Path, family: &str) -> Result<Self> {
        let command = format!("vscode-{family}-language-server");
        Ok(Self {
            host: Host::default(),
            scope: Scope {
                owner: "fixture-owner".into(),
                project: 1,
                root: root.into(),
                provider: format!("community/{family}"),
                digest: "a".repeat(64),
                server: serde_json::from_value(json!({
                    "command":command,"args":["--stdio"],
                    "initializationOptions":{"provideFormatter":true},
                    "settings":{"json":{"validate":{"enable":true},"schemas":[{"fileMatch":["*.json","*.jsonc"],"schema":{
                        "type":"object","properties":{"count":{"type":"integer","description":"Count setting"}},
                        "required":["count"],"additionalProperties":false
                    }}]}}
                }))?,
            },
            sequence: 0,
        })
    }
    async fn query(
        &mut self,
        client: &str,
        path: &str,
        language: &str,
        source: &str,
        revision: u64,
        query: Query,
    ) -> Result<Response> {
        self.sequence += 1;
        let input = protocol::Request {
            version: protocol::VERSION,
            client: client.into(),
            document: path.into(),
            request: self.sequence,
            revision,
            language: language.into(),
            path: path.into(),
            source_digest: source_digest(source),
            source: source.into(),
            query,
        };
        let mut scope = self.scope.clone();
        scope.server.language_id = Some(language.into());
        let response = self
            .host
            .query(scope, &input)
            .await
            .map_err(anyhow::Error::msg)?;
        response.validate(&input).map_err(anyhow::Error::msg)?;
        Ok(response)
    }
    async fn diagnostics(
        &mut self,
        client: &str,
        path: &str,
        language: &str,
        source: &str,
    ) -> Result<Response> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let response = self
                .query(client, path, language, source, 1, Query::Synchronize)
                .await?;
            if response.diagnostics_ready {
                return Ok(response);
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "{language}: diagnostics did not settle"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    for (family, dialects) in [
        ("html", vec!["html"]),
        ("css", vec!["css", "scss"]),
        ("json", vec!["json", "jsonc"]),
    ] {
        let root = tempfile::tempdir()?;
        let mut fixture = Fixture::new(root.path(), family)?;
        let mut server = None;
        for language in dialects {
            let path = format!("fixture.{language}");
            let source = match language {
                "html" => "<div class=\"card\"><span>文😀</span></div>\r\n",
                "css" => ".card { color:red; padding: 2px; }\r\n",
                "scss" => "$gap: 2px; .card { padding:$gap; color:red; }\r\n",
                "json" => "{\"count\":1,\"other\":true}\r\n",
                "jsonc" => "// 文😀\r\n{\"count\":1,\"other\":true}\r\n",
                _ => unreachable!(),
            };
            std::fs::write(root.path().join(&path), source)?;
            let sync = fixture
                .query("editor-a", &path, language, source, 1, Query::Synchronize)
                .await?;
            if let Some(previous) = &server {
                ensure!(
                    previous == &sync.server,
                    "{family}: dialects started separate services"
                );
            } else {
                server = Some(sync.server.clone());
            }
            let symbols = fixture
                .query("editor-a", &path, language, source, 1, Query::Symbols)
                .await?;
            ensure!(
                !serde_json::from_value::<Vec<results::Symbol>>(symbols.result)?.is_empty(),
                "{language}: missing symbols"
            );
            let range = protocol::Range {
                start: protocol::Position::default(),
                end: protocol::position(source, source.len()).map_err(anyhow::Error::msg)?,
            };
            for query in [
                Query::Formatting {
                    tab_width: 2,
                    insert_spaces: true,
                },
                Query::RangeFormatting {
                    range,
                    tab_width: 2,
                    insert_spaces: true,
                },
            ] {
                let response = fixture
                    .query("editor-a", &path, language, source, 1, query)
                    .await?;
                let edits = serde_json::from_value::<Vec<results::TextEdit>>(response.result)?;
                let formatted = results::apply_edits(source, &edits).map_err(anyhow::Error::msg)?;
                ensure!(
                    formatted.contains(if family == "html" {
                        "文😀"
                    } else if family == "css" {
                        "red"
                    } else {
                        "count"
                    }),
                    "{language}: formatter lost content"
                );
            }
            let (completion_source, needle, wanted) = match language {
                "html" => ("<di", "<di", "div"),
                "css" => (".card { col }", "col", "color"),
                "scss" => ("$gap: 2px; .card { col }", "col", "color"),
                "json" => ("{\"cou\"}", "cou", "count"),
                "jsonc" => ("// comment\r\n{\"cou\"}", "cou", "count"),
                _ => unreachable!(),
            };
            let at = completion_source.find(needle).expect("completion fixture") + needle.len();
            let response = fixture
                .query(
                    "editor-a",
                    &path,
                    language,
                    completion_source,
                    2,
                    Query::Completion {
                        position: protocol::position(completion_source, at)
                            .map_err(anyhow::Error::msg)?,
                    },
                )
                .await?;
            let items = response
                .result
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("{language}: invalid completion list"))?;
            let item = items
                .iter()
                .find(|item| {
                    item["label"]
                        .as_str()
                        .is_some_and(|label| label.contains(wanted))
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("{language}: missing {wanted} completion: {items:?}")
                })?;
            let resolved = fixture
                .query(
                    "editor-a",
                    &path,
                    language,
                    completion_source,
                    2,
                    Query::ResolveCompletion {
                        id: item["id"].as_str().unwrap().into(),
                    },
                )
                .await?;
            let completion = results::completion(
                completion_source,
                protocol::position(completion_source, at).map_err(anyhow::Error::msg)?,
                &resolved.result,
                &std::collections::BTreeMap::new(),
            )
            .map_err(anyhow::Error::msg)
            .with_context(|| {
                format!(
                    "{language} completion format {:?}, command {:?}",
                    resolved.result["insertTextFormat"], resolved.result["command"]["command"]
                )
            })?;
            let inserted = results::apply_edits(completion_source, &completion.edits)
                .map_err(anyhow::Error::msg)?;
            ensure!(
                inserted.contains(wanted),
                "{language}: completion did not insert the chosen item"
            );
            ensure!(
                std::fs::read_to_string(root.path().join(&path))? == source,
                "{language}: queries saved an unsaved overlay"
            );
            println!(
                "{}",
                json!({"language":language,"symbols":true,"document_and_selection_formatting":true,"resolved_completion":true,"source_files_unchanged":true})
            );
        }
        if family == "json" {
            let invalid = fixture
                .diagnostics("schema-a", "schema.json", "json", "{\"count\":\"wrong\"}")
                .await?;
            ensure!(
                invalid
                    .diagnostics
                    .iter()
                    .any(|item| item.message.contains("integer")),
                "JSON schema diagnostics missing: {:?}",
                invalid.diagnostics
            );
            let valid = fixture
                .diagnostics("schema-b", "schema.json", "json", "{\"count\":2}")
                .await?;
            ensure!(
                valid.diagnostics.is_empty(),
                "Independent editing clients leaked schema overlays"
            );
            println!(
                "{}",
                json!({"json_schema_diagnostics":true,"independent_unsaved_clients":true})
            );
        }
        fixture.host.shutdown();
    }
    Ok(())
}
