//! Real Pyright contract through the shared native language-service facade.
use anyhow::{Context, Result, ensure};
use openwebide_bridge::languages::host::{Host, Scope};
use openwebide_core::plugins::languages::{
    intelligence::{self as protocol, Position, Query, Response, results},
    source_digest,
};
use serde_json::json;
use std::{path::Path, process::Command, time::Duration};

const LIBRARY: &str = "def answer() -> int:\r\n    return 42\r\n";
const UNSAVED: &str = "def answer() -> str:\r\n    return 'unsaved'\r\n";
const MAIN: &str = "# 文😀\r\nfrom lib import answer\r\nfrom env_only import environment_value\r\nvalue = answer()\r\nenvironment = environment_value()\r\n";

struct Fixture {
    host: Host,
    scope: Scope,
    sequence: u64,
    client: &'static str,
}

impl Fixture {
    fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            host: Host::default(),
            scope: Scope {
                owner: "fixture-owner".into(),
                project: 1,
                root: root.into(),
                provider: "community/python".into(),
                digest: "a".repeat(64),
                server: serde_json::from_value(json!({
                    "command":"pyright-langserver", "args":["--stdio"],
                    "settings":{"python":{"analysis":{"diagnosticMode":"openFilesOnly","autoImportCompletions":true}}}
                }))?,
            },
            sequence: 0,
            client: "editor-a",
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
            document: path.into(),
            request: self.sequence,
            revision,
            language: "python".into(),
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
                    .query("main.py", source, revision, Query::Synchronize)
                    .await?;
                if response.diagnostics_ready
                    && code.map_or_else(
                        || response.diagnostics.is_empty(),
                        |code| {
                            response
                                .diagnostics
                                .iter()
                                .any(|d| d.code.as_deref() == Some(code))
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

fn environment(root: &Path) -> Result<()> {
    let status = Command::new("python3")
        .args(["-m", "venv", "--without-pip", ".venv"])
        .current_dir(root)
        .status()?;
    ensure!(
        status.success(),
        "Could not create isolated Python environment"
    );
    let output = Command::new(root.join(".venv/bin/python"))
        .args([
            "-c",
            "import sysconfig; print(sysconfig.get_path('purelib'))",
        ])
        .output()?;
    ensure!(output.status.success());
    let site = String::from_utf8(output.stdout)?;
    std::fs::write(
        Path::new(site.trim()).join("env_only.py"),
        "def environment_value() -> bytes:\n    return b'venv'\n",
    )?;
    std::fs::write(
        root.join("pyproject.toml"),
        "[tool.pyright]\nvenvPath = '.'\nvenv = '.venv'\ntypeCheckingMode = 'standard'\n",
    )?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let root = tempfile::tempdir()?;
    environment(root.path())?;
    std::fs::write(root.path().join("lib.py"), LIBRARY)?;
    std::fs::write(root.path().join("main.py"), MAIN)?;
    let stub = "def stub_answer() -> str: ...\r\n";
    std::fs::write(root.path().join("stub.pyi"), stub)?;
    let mut fixture = Fixture::new(root.path())?;
    let first = fixture
        .query("lib.py", LIBRARY, 1, Query::Synchronize)
        .await?;
    fixture.hover("main.py", MAIN, "answer", "int").await?;
    fixture
        .hover("main.py", MAIN, "environment_value", "bytes")
        .await?;
    let stub_result = fixture
        .hover("stub.pyi", stub, "stub_answer", "str")
        .await?;
    ensure!(stub_result.server == first.server);
    fixture.diagnostics(MAIN, 1, None).await?;
    fixture
        .query("lib.py", UNSAVED, 2, Query::Synchronize)
        .await?;
    let warm = fixture.hover("main.py", MAIN, "answer", "str").await?;
    ensure!(
        warm.server == first.server,
        "An unsaved edit discarded the warm service"
    );
    let position = point(MAIN, MAIN.rfind("answer").unwrap())?;
    let definition = fixture
        .query("main.py", MAIN, 1, Query::Definition { position })
        .await?;
    let targets: Vec<results::Target> = serde_json::from_value(definition.result)?;
    ensure!(
        targets
            .iter()
            .any(|target| target.path.as_deref() == Some("lib.py")
                && target.source_digest.as_deref() == Some(&source_digest(UNSAVED))),
        "Definition lost its unsaved source binding"
    );
    let references = fixture
        .query("main.py", MAIN, 1, Query::References { position })
        .await?;
    let targets: Vec<results::Target> = serde_json::from_value(references.result)?;
    ensure!(
        targets.iter().any(|t| t.path.as_deref() == Some("lib.py"))
            && targets.iter().any(|t| t.path.as_deref() == Some("main.py")),
        "Cross-file references missing"
    );
    let symbols = fixture.query("lib.py", UNSAVED, 2, Query::Symbols).await?;
    ensure!(symbols.result.to_string().contains("answer"));
    let rename = fixture
        .query(
            "lib.py",
            UNSAVED,
            2,
            Query::Rename {
                position: point(UNSAVED, UNSAVED.find("answer").unwrap())?,
                new_name: "renamed_answer".into(),
            },
        )
        .await?;
    let edits: results::WorkspaceEdit = serde_json::from_value(rename.result)?;
    for (path, source) in [("lib.py", UNSAVED), ("main.py", MAIN)] {
        let file = edits
            .files
            .iter()
            .find(|file| file.path == path)
            .with_context(|| format!("Missing renamed file {path}"))?;
        ensure!(file.source_digest == source_digest(source));
        let after = results::apply_edits(source, &file.edits).map_err(anyhow::Error::msg)?;
        ensure!(after.contains("renamed_answer"));
    }
    let broken = MAIN.replace("value = answer()", "value: int = answer()");
    fixture
        .diagnostics(&broken, 2, Some("reportAssignmentType"))
        .await?;
    let fixed = MAIN.replace("value = answer()", "value: str = answer()");
    fixture.diagnostics(&fixed, 3, None).await?;
    ensure!(
        fixture
            .query("main.py", "old", 2, Query::Synchronize)
            .await
            .unwrap_err()
            .to_string()
            .contains("stale")
    );
    let completion_source = "# 文😀\r\nfrom lib import answer\r\nvalue = answer().upp\r\n";
    let position = point(
        completion_source,
        completion_source.find(".upp").unwrap() + 4,
    )?;
    let completion = fixture
        .query(
            "main.py",
            completion_source,
            4,
            Query::Completion { position },
        )
        .await?;
    let choice = completion
        .result
        .as_array()
        .context("Invalid completions")?
        .iter()
        .find(|item| item["label"] == "upper")
        .context("Missing string member completion")?;
    let resolved = fixture
        .query(
            "main.py",
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
        after.contains("answer().upper"),
        "Completion insertion missing: {after}"
    );
    let standard = "word = 'hello'\r\nvalue = word.upper()\r\n";
    std::fs::write(root.path().join("standard.py"), standard)?;
    let definition = fixture
        .query(
            "standard.py",
            standard,
            1,
            Query::Definition {
                position: point(standard, standard.find("upper").unwrap())?,
            },
        )
        .await?;
    let id = definition
        .result
        .as_array()
        .context("Invalid standard-library definition")?
        .iter()
        .find_map(|target| target["resource"].as_str())
        .context("Missing read-only standard-library resource")?
        .to_string();
    let resource = fixture
        .query("standard.py", standard, 1, Query::ReadResource { id })
        .await?;
    ensure!(
        resource.result["readOnly"] == true
            && resource.result["source"]
                .as_str()
                .is_some_and(|source| source.contains("def upper")),
        "Standard-library definitions must remain read-only"
    );
    ensure!(
        first.capabilities["documentFormattingProvider"] != true,
        "Unexpected formatter capability"
    );
    fixture
        .query("main.py", completion_source, 4, Query::Close)
        .await?;
    let retained = fixture.query("lib.py", UNSAVED, 2, Query::Symbols).await?;
    ensure!(
        retained.server == first.server,
        "Closing one document discarded the warm service"
    );
    fixture.client = "editor-b";
    let other = "def answer() -> bool:\r\n    return True\r\n";
    let isolated = fixture
        .query("lib.py", other, 1, Query::Synchronize)
        .await?;
    ensure!(isolated.server != first.server);
    fixture.hover("main.py", MAIN, "answer", "bool").await?;
    fixture.client = "editor-a";
    fixture.hover("main.py", MAIN, "answer", "str").await?;
    fixture.scope.owner = "other-owner".into();
    let other_owner = fixture.hover("main.py", MAIN, "answer", "int").await?;
    ensure!(other_owner.server != first.server);
    fixture.scope.owner = "fixture-owner".into();
    fixture.scope.project = 2;
    let other_project = fixture.hover("main.py", MAIN, "answer", "int").await?;
    ensure!(other_project.server != first.server && other_project.server != other_owner.server);
    fixture.scope.project = 1;
    fixture
        .host
        .revoke("fixture-owner", Some(1), Some("community/python"));
    let restarted = fixture
        .query("lib.py", UNSAVED, 2, Query::Synchronize)
        .await?;
    ensure!(restarted.server != first.server);
    fixture.hover("main.py", MAIN, "answer", "str").await?;
    // JSON configuration has priority over pyproject.toml. Verify on a fresh
    // service so this contract does not depend on filesystem watcher timing.
    std::fs::write(
        root.path().join("pyrightconfig.json"),
        json!({"venvPath":".","venv":".venv","typeCheckingMode":"standard"}).to_string(),
    )?;
    std::fs::write(
        root.path().join("pyproject.toml"),
        "[tool.pyright]\nvenvPath = '.'\nvenv = 'missing-environment'\n",
    )?;
    fixture
        .host
        .revoke("fixture-owner", Some(1), Some("community/python"));
    fixture
        .hover("main.py", MAIN, "environment_value", "bytes")
        .await?;
    fixture.diagnostics(MAIN, 1, None).await?;
    ensure!(std::fs::read_to_string(root.path().join("lib.py"))? == LIBRARY);
    ensure!(std::fs::read_to_string(root.path().join("main.py"))? == MAIN);
    println!(
        "{}",
        json!({"pyproject_venv_imports":true,"json_config_precedence":true,"stub_language_service":true,"warm_service":true,"unsaved_cross_file_types":true,"source_bound_definition":true,"standard_library_read_only":true,"references":true,"symbols":true,"reviewable_rename":true,"diagnostics_clear":true,"resolved_member_completion":true,"close_retains_service":true,"client_owner_project_isolation":true,"revocation_restart":true,"source_files_unchanged":true})
    );
    Ok(())
}
