//! Real gopls contract through the shared native language-service facade.
use anyhow::{Context, Result, ensure};
use openwebide_bridge::languages::host::{Host, Scope};
use openwebide_core::plugins::languages::{
    intelligence::{self as protocol, Position, Query, Response, results},
    source_digest,
};
use serde_json::json;
use std::{collections::BTreeMap, path::Path, time::Duration};

#[path = "support/cache.rs"]
mod cache;

const LIBRARY: &str = "package lib\r\n\r\nfunc Answer() int { return 42 }\r\n";
const UNSAVED: &str = "package lib\r\n\r\nfunc Answer() string { return \"unsaved\" }\r\n";
const MAIN: &str = "package app\r\n\r\nimport \"example.test/lib\"\r\n\r\nvar Note = \"文😀\"; var Value = lib.Answer()\r\n";

struct Fixture {
    host: Host,
    scope: Scope,
    sequence: u64,
    client: &'static str,
    generations: BTreeMap<String, u64>,
    storage: tempfile::TempDir,
}

impl Fixture {
    fn new(root: &Path) -> Result<Self> {
        let storage = tempfile::tempdir()?;
        let mut host = Host::default();
        host.set_cache_directory(storage.path().into())
            .map_err(anyhow::Error::msg)?;
        Ok(Self {
            host,
            scope: Scope {
                owner: "fixture-owner".into(),
                project: 1,
                root: root.into(),
                provider: "community/go".into(),
                digest: "a".repeat(64),
                server: serde_json::from_value(json!({
                    "command":"gopls", "args":["serve"],
                    "cache":{"versionArgs":["version"],"environment":{"GOPLSCACHE":"${cache}/gopls","GOCACHE":"${cache}/build","GOMODCACHE":"${cache}/modules"}},
                    "settings":{"gopls":{"env":{
                        "GOENV":"off", "GOTOOLCHAIN":"local", "GOWORK":"", "GOFLAGS":"", "GOPROXY":"off", "GOSUMDB":"off",
                        "GOCACHE":"${cache}/build",
                        "GOMODCACHE":"${cache}/modules"
                    }}}
                }))?,
            },
            sequence: 0,
            client: "editor-a",
            generations: BTreeMap::new(),
            storage,
        })
    }

    async fn query(
        &mut self,
        path: &str,
        source: &str,
        revision: u64,
        query: Query,
    ) -> Result<Response> {
        self.sequence += 1;
        let input = protocol::Request {
            version: protocol::VERSION,
            client: self.client.into(),
            document: format!("{path}:{}", self.generations.get(path).unwrap_or(&0)),
            request: self.sequence,
            revision,
            language: "go".into(),
            path: path.into(),
            source_digest: source_digest(source),
            source: source.into(),
            query,
        };
        let response = self
            .host
            .query(self.scope.clone(), &input)
            .await
            .map_err(anyhow::Error::msg)?;
        response.validate(&input).map_err(anyhow::Error::msg)?;
        // Reopening creates a new document lifetime. Late requests keep their
        // old identity and must remain stale after a successful close.
        if matches!(input.query, Query::Close) {
            *self.generations.entry(path.into()).or_default() += 1;
        }
        Ok(response)
    }

    async fn hover(
        &mut self,
        path: &str,
        source: &str,
        token: &str,
        expected: &str,
    ) -> Result<Response> {
        let position = point(source, source.rfind(token).context("Missing hover token")?)?;
        tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                let response = self
                    .query(path, source, 1, Query::Hover { position })
                    .await?;
                if response.result.to_string().contains(expected) {
                    return Ok(response);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await?
    }

    async fn diagnostics(&mut self, source: &str, revision: u64, code: Option<&str>) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                let response = self
                    .query("app/main.go", source, revision, Query::Synchronize)
                    .await?;
                if response.diagnostics_ready
                    && code.map_or_else(
                        || response.diagnostics.is_empty(),
                        |code| {
                            response
                                .diagnostics
                                .iter()
                                .any(|d| d.message.contains(code))
                        },
                    )
                {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await?
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.host.shutdown();
    }
}

fn point(source: &str, byte: usize) -> Result<Position> {
    protocol::position(source, byte).map_err(anyhow::Error::msg)
}

#[tokio::main]
async fn main() -> Result<()> {
    let single = tempfile::tempdir()?;
    std::fs::write(
        single.path().join("go.mod"),
        "module example.test/single\n\ngo 1.27.0\n",
    )?;
    let standalone = "package single\n\nfunc Answer() bool { return true }\n";
    std::fs::write(single.path().join("main.go"), standalone)?;
    {
        let mut fixture = Fixture::new(single.path())?;
        let started = std::time::Instant::now();
        fixture
            .hover("main.go", standalone, "Answer", "bool")
            .await?;
        let cold = started.elapsed();
        let before = cache::wait_for(fixture.storage.path(), |path| {
            path.extension()
                .is_some_and(|suffix| suffix == "kind" || suffix == "gob")
                || path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with("-d"))
        })
        .await?;
        let namespaces = cache::namespaces(fixture.storage.path())?;
        fixture.host.shutdown();
        openwebide_bridge::exec::proc::wait_for_cleanup().await;
        let started = std::time::Instant::now();
        fixture
            .hover("main.go", standalone, "Answer", "bool")
            .await?;
        let restart = started.elapsed();
        let after = cache::snapshot(fixture.storage.path())?;
        ensure!(
            cache::namespaces(fixture.storage.path())? == namespaces,
            "Go allocated a new cache namespace instead of reopening the existing one"
        );
        cache::report("gopls", &before, &after, cold, restart)?;
        ensure!(
            !single.path().join(".go-cache").exists()
                && !single.path().join(".go-modules").exists()
        );
        fixture.host.shutdown();
        openwebide_bridge::exec::proc::wait_for_cleanup().await;
    }
    let root = tempfile::tempdir()?;
    std::fs::create_dir(root.path().join("lib"))?;
    std::fs::create_dir(root.path().join("app"))?;
    let files = [
        ("go.work", "go 1.27.0\n\nuse (\n ./lib\n ./app\n)\n"),
        ("lib/go.mod", "module example.test/lib\n\ngo 1.27.0\n"),
        (
            "app/go.mod",
            "module example.test/app\n\ngo 1.27.0\n\nrequire example.test/lib v0.0.0\n",
        ),
        ("lib/answer.go", LIBRARY),
        ("app/main.go", MAIN),
    ];
    for (path, source) in files {
        std::fs::write(root.path().join(path), source)?;
    }
    let mut fixture = Fixture::new(root.path())?;
    let first = fixture
        .query("lib/answer.go", LIBRARY, 1, Query::Synchronize)
        .await?;
    fixture.hover("app/main.go", MAIN, "Answer", "int").await?;
    fixture.diagnostics(MAIN, 1, None).await?;
    fixture
        .query("lib/answer.go", UNSAVED, 2, Query::Synchronize)
        .await?;
    let warm = fixture
        .hover("app/main.go", MAIN, "Answer", "string")
        .await?;
    ensure!(
        warm.server == first.server,
        "Cross-module edits replaced the warm service"
    );
    let position = point(MAIN, MAIN.rfind("Answer").unwrap())?;
    let definition = fixture
        .query("app/main.go", MAIN, 1, Query::Definition { position })
        .await?;
    let targets: Vec<results::Target> = serde_json::from_value(definition.result)?;
    ensure!(
        targets
            .iter()
            .any(|target| target.path.as_deref() == Some("lib/answer.go")
                && target.source_digest.as_deref() == Some(&source_digest(UNSAVED))),
        "Definition lost its unsaved cross-module source binding"
    );
    let references = fixture
        .query("app/main.go", MAIN, 1, Query::References { position })
        .await?;
    let targets: Vec<results::Target> = serde_json::from_value(references.result)?;
    ensure!(
        targets
            .iter()
            .any(|target| target.path.as_deref() == Some("app/main.go")),
        "Cross-module references are missing"
    );
    let symbols = fixture
        .query("lib/answer.go", UNSAVED, 2, Query::Symbols)
        .await?;
    ensure!(symbols.result.to_string().contains("Answer"));
    let rename = fixture
        .query(
            "lib/answer.go",
            UNSAVED,
            2,
            Query::Rename {
                position: point(UNSAVED, UNSAVED.find("Answer").unwrap())?,
                new_name: "RenamedAnswer".into(),
            },
        )
        .await?;
    let edits: results::WorkspaceEdit = serde_json::from_value(rename.result)?;
    for (path, source) in [("lib/answer.go", UNSAVED), ("app/main.go", MAIN)] {
        let file = edits
            .files
            .iter()
            .find(|file| file.path == path)
            .with_context(|| format!("Missing renamed file {path}"))?;
        ensure!(file.source_digest == source_digest(source));
        let after = results::apply_edits(source, &file.edits).map_err(anyhow::Error::msg)?;
        ensure!(
            after == source.replace("Answer", "RenamedAnswer"),
            "Rename changed unrelated source: {after}"
        );
    }
    let broken = MAIN.replace("var Value =", "var Value int =");
    fixture.diagnostics(&broken, 2, Some("cannot use")).await?;
    let fixed = MAIN.replace("var Value =", "var Value string =");
    fixture.diagnostics(&fixed, 3, None).await?;
    ensure!(
        fixture
            .query("app/main.go", "old", 2, Query::Synchronize)
            .await
            .unwrap_err()
            .to_string()
            .contains("stale")
    );

    let completion_source = "package app\r\n\r\nfunc Render() { fmt.Spri }\r\n";
    let position = point(
        completion_source,
        completion_source.find("fmt.Spri").unwrap() + 8,
    )?;
    let completion = fixture
        .query(
            "app/main.go",
            completion_source,
            4,
            Query::Completion { position },
        )
        .await?;
    let choice = completion
        .result
        .as_array()
        .context("Invalid completion response")?
        .iter()
        .find(|item| {
            item["label"]
                .as_str()
                .is_some_and(|label| label == "Sprint" || label.starts_with("Sprint("))
        })
        .with_context(|| {
            format!(
                "Missing unimported fmt.Sprint completion: {}",
                completion.result
            )
        })?;
    let resolved = fixture
        .query(
            "app/main.go",
            completion_source,
            4,
            Query::ResolveCompletion {
                id: choice["id"]
                    .as_str()
                    .context("Missing completion ID")?
                    .into(),
            },
        )
        .await?;
    let edits = results::completion_edits(completion_source, position, &resolved.result)
        .map_err(anyhow::Error::msg)?;
    let after = results::apply_edits(completion_source, &edits).map_err(anyhow::Error::msg)?;
    ensure!(
        after.contains("fmt.Sprint") && after.contains("\"fmt\""),
        "Completion lost import or insertion: {after}"
    );
    let unformatted = "package app\r\n\r\nvar Formatted=1\r\n";
    let formatted = fixture
        .query(
            "app/main.go",
            unformatted,
            5,
            Query::Formatting {
                tab_width: 4,
                insert_spaces: false,
            },
        )
        .await?;
    let edits: Vec<results::TextEdit> = serde_json::from_value(formatted.result)?;
    ensure!(
        results::apply_edits(unformatted, &edits)
            .map_err(anyhow::Error::msg)?
            .contains("var Formatted = 1"),
        "Go document formatting returned no usable edits"
    );

    let standard = "package app\r\n\r\nimport \"fmt\"\r\n\r\nvar Standard = fmt.Sprint(42)\r\n";
    let definition = fixture
        .query(
            "app/main.go",
            standard,
            6,
            Query::Definition {
                position: point(standard, standard.find("Sprint").unwrap())?,
            },
        )
        .await?;
    let id = definition
        .result
        .as_array()
        .context("Invalid standard-library definition")?
        .iter()
        .find_map(|target| target["resource"].as_str())
        .context("Missing read-only Go standard-library resource")?
        .to_string();
    let resource = fixture
        .query("app/main.go", standard, 6, Query::ReadResource { id })
        .await?;
    ensure!(
        resource.result["readOnly"] == true
            && resource.result["source"]
                .as_str()
                .is_some_and(|source| source.contains("func Sprint(")),
        "Standard library must be read-only"
    );
    fixture
        .query("app/main.go", standard, 6, Query::Close)
        .await?;
    let retained = fixture
        .query("lib/answer.go", UNSAVED, 2, Query::Symbols)
        .await?;
    ensure!(
        retained.server == first.server,
        "Closing a tab discarded the warm service"
    );
    fixture.client = "editor-b";
    let other = "package lib\r\n\r\nfunc Answer() bool { return true }\r\n";
    let isolated = fixture
        .query("lib/answer.go", other, 1, Query::Synchronize)
        .await?;
    ensure!(isolated.server != first.server);
    fixture.hover("app/main.go", MAIN, "Answer", "bool").await?;
    fixture.client = "editor-a";
    fixture
        .hover("app/main.go", MAIN, "Answer", "string")
        .await?;
    fixture.scope.owner = "other-owner".into();
    let owner = fixture.hover("app/main.go", MAIN, "Answer", "int").await?;
    ensure!(owner.server != first.server);
    fixture.scope.owner = "fixture-owner".into();
    fixture.scope.project = 2;
    let project = fixture.hover("app/main.go", MAIN, "Answer", "int").await?;
    ensure!(project.server != first.server && project.server != owner.server);
    fixture.scope.project = 1;
    let before_revoke = fixture
        .hover("app/main.go", MAIN, "Answer", "string")
        .await?;
    fixture
        .host
        .revoke("fixture-owner", Some(1), Some("community/go"));
    let restarted = fixture
        .query("lib/answer.go", UNSAVED, 2, Query::Synchronize)
        .await?;
    ensure!(restarted.server != before_revoke.server);
    fixture
        .hover("app/main.go", MAIN, "Answer", "string")
        .await?;
    for (path, source) in files {
        ensure!(
            std::fs::read(root.path().join(path))? == source.as_bytes(),
            "Language service wrote {path}"
        );
    }
    fixture.host.shutdown();
    openwebide_bridge::exec::proc::wait_for_cleanup().await;
    println!(
        "{}",
        json!({"go_mod_single_module":true,"go_work_multiple_modules":true,
        "warm_service":true,"unsaved_cross_module_types":true,"source_bound_definition":true,
        "standard_library_read_only":true,"references":true,"symbols":true,"reviewable_rename":true,
        "diagnostics_clear":true,"resolved_auto_import_completion":true,"formatting":true,
        "close_retains_service":true,"client_owner_project_isolation":true,"revocation_restart":true,
        "source_files_unchanged":true})
    );
    Ok(())
}
