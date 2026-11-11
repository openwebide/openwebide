//! Real C# server contract through the shared native service, including .slnx.
use anyhow::{Context, Result, ensure};
use openwebide_bridge::languages::host::{Host, Scope};
use openwebide_core::plugins::languages::{
    intelligence::{self as protocol, Position, Query, Response, results},
    source_digest,
};
use serde_json::json;
use std::{path::Path, process::Command, time::Duration};

const LIB_PATH: &str = "Library/Answers.cs";
const CLIENT_PATH: &str = "Client/Consumer.cs";
const LIBRARY: &str =
    "namespace Library; public static class Answers { public static int GetAnswer() => 42; }\r\n";
const UNSAVED: &str = "namespace Library; public static class Answers { public static string GetAnswer() => \"unsaved\"; }\r\n";
const CLIENT: &str = "// 文😀\r\nnamespace Client; public class Consumer { public string Label = \"😀\"; public int Value => Library.Answers.GetAnswer(); }\r\n";
const FIXED: &str = "// 文😀\r\nnamespace Client; public class Consumer { public string Label = \"😀\"; public string Value => Library.Answers.GetAnswer(); }\r\n";

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
                provider: "community/dotnet".into(),
                digest: "a".repeat(64),
                server: serde_json::from_value(json!({
                    "command":"csharp-ls", "settings":{"csharp":{"useMetadataUris":true}},
                    "virtualDocuments":[{"scheme":"csharp", "method":"csharp/metadata", "parameters":"text_document", "textPointer":"/source"}]
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
            language: "csharp".into(),
            path: path.into(),
            source: source.into(),
            source_digest: source_digest(source),
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
        revision: u64,
        expected: &str,
    ) -> Result<Response> {
        let position = point(
            source,
            source.rfind("GetAnswer").context("Missing hover name")?,
        )?;
        tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                let response = self
                    .query(path, source, revision, Query::Hover { position })
                    .await?;
                if response.result.to_string().contains(expected) {
                    return Ok::<_, anyhow::Error>(response);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await?
    }
    async fn diagnostics(
        &mut self,
        path: &str,
        source: &str,
        revision: u64,
        code: Option<&str>,
    ) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                let response = self
                    .query(path, source, revision, Query::Synchronize)
                    .await?;
                if response.diagnostics_ready
                    && code.map_or_else(
                        || response.diagnostics.is_empty(),
                        |code| {
                            response
                                .diagnostics
                                .iter()
                                .any(|diagnostic| diagnostic.code.as_deref() == Some(code))
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
fn restore(root: &Path, project: &str) -> Result<()> {
    let versions: serde_json::Value = serde_json::from_str(include_str!(
        "../../tools/csharp-language-server/versions.json"
    ))?;
    std::fs::write(
        root.join("global.json"),
        json!({"sdk":{"version":versions["sdk"],"rollForward":"latestPatch"}}).to_string(),
    )?;
    std::fs::write(
        root.join("NuGet.Config"),
        "<configuration><packageSources><clear/></packageSources></configuration>",
    )?;
    let output = Command::new("dotnet")
        .args([
            "restore",
            project,
            "--configfile",
            "NuGet.Config",
            "--disable-build-servers",
        ])
        .current_dir(root)
        .env("DOTNET_CLI_HOME", root.join(".cli"))
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .env("DOTNET_GENERATE_ASPNET_CERTIFICATE", "false")
        .env("DOTNET_NOLOGO", "1")
        .output()
        .context("Install the pinned .NET 10 SDK")?;
    ensure!(
        output.status.success(),
        "Could not restore the isolated C# fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
fn project(root: &Path, name: &str, reference: bool) -> Result<()> {
    std::fs::create_dir_all(root.join(name))?;
    let reference = if reference {
        "<ItemGroup><ProjectReference Include=\"../Library/Library.csproj\"/></ItemGroup>"
    } else {
        ""
    };
    std::fs::write(
        root.join(format!("{name}/{name}.csproj")),
        format!(
            "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup>{reference}</Project>"
        ),
    )?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    {
        let root = tempfile::tempdir()?;
        project(root.path(), "Library", false)?;
        project(root.path(), "Client", true)?;
        std::fs::write(
            root.path().join("Sample.slnx"),
            "<Solution><Project Path=\"Library/Library.csproj\"/><Project Path=\"Client/Client.csproj\"/></Solution>",
        )?;
        std::fs::write(root.path().join(LIB_PATH), LIBRARY)?;
        std::fs::write(root.path().join(CLIENT_PATH), CLIENT)?;
        restore(root.path(), "Sample.slnx")?;
        let mut fixture = Fixture::new(root.path())?;
        fixture
            .query(LIB_PATH, LIBRARY, 1, Query::Synchronize)
            .await?;
        let first = fixture.hover(CLIENT_PATH, CLIENT, 1, "int").await?;
        fixture
            .query(LIB_PATH, UNSAVED, 2, Query::Synchronize)
            .await?;
        let changed = fixture.hover(CLIENT_PATH, CLIENT, 1, "string").await?;
        ensure!(
            first.server == changed.server,
            "A project reference replaced the warm service"
        );
        fixture
            .diagnostics(CLIENT_PATH, CLIENT, 1, Some("CS0029"))
            .await?;
        fixture.diagnostics(CLIENT_PATH, FIXED, 2, None).await?;
        let position = point(FIXED, FIXED.rfind("GetAnswer").unwrap())?;
        let definition = fixture
            .query(CLIENT_PATH, FIXED, 2, Query::Definition { position })
            .await?;
        let targets: Vec<results::Target> = serde_json::from_value(definition.result)?;
        ensure!(
            targets
                .iter()
                .any(|target| target.path.as_deref() == Some(LIB_PATH)
                    && target.source_digest.as_deref() == Some(&source_digest(UNSAVED))),
            "Definition lost its unsaved cross-project source"
        );
        let external = fixture
            .query(
                CLIENT_PATH,
                FIXED,
                2,
                Query::Definition {
                    position: point(FIXED, FIXED.find("string Label").unwrap())?,
                },
            )
            .await?;
        let targets: Vec<results::Target> = serde_json::from_value(external.result)?;
        let resource = targets
            .iter()
            .find_map(|target| target.resource.clone())
            .with_context(|| format!("Missing read-only framework definition: {targets:?}"))?;
        let external = fixture
            .query(CLIENT_PATH, FIXED, 2, Query::ReadResource { id: resource })
            .await?;
        ensure!(external.result["readOnly"] == true);
        ensure!(
            external.result["source"]
                .as_str()
                .is_some_and(|source| source.contains("String")),
            "Framework definition did not provide decompiled source"
        );
        let references = fixture
            .query(CLIENT_PATH, FIXED, 2, Query::References { position })
            .await?;
        let targets: Vec<results::Target> = serde_json::from_value(references.result)?;
        ensure!(
            targets
                .iter()
                .any(|target| target.path.as_deref() == Some(LIB_PATH))
                && targets
                    .iter()
                    .any(|target| target.path.as_deref() == Some(CLIENT_PATH)),
            "References omitted a project"
        );
        let symbols = fixture.query(CLIENT_PATH, FIXED, 2, Query::Symbols).await?;
        ensure!(
            !symbols
                .result
                .as_array()
                .context("Invalid C# symbols")?
                .is_empty()
        );
        let rename = fixture
            .query(
                LIB_PATH,
                UNSAVED,
                2,
                Query::Rename {
                    position: point(UNSAVED, UNSAVED.find("GetAnswer").unwrap())?,
                    new_name: "RenamedAnswer".into(),
                },
            )
            .await?;
        let edits: results::WorkspaceEdit = serde_json::from_value(rename.result)?;
        for (path, source) in [(LIB_PATH, UNSAVED), (CLIENT_PATH, FIXED)] {
            let file = edits
                .files
                .iter()
                .find(|file| file.path == path)
                .with_context(|| format!("Rename omitted {path}"))?;
            ensure!(file.source_digest == source_digest(source));
            let after = results::apply_edits(source, &file.edits).map_err(anyhow::Error::msg)?;
            ensure!(
                after.contains("RenamedAnswer"),
                "Rename did not update {path}"
            );
        }
        let completion_source = FIXED.replace("GetAnswer", "GetAn");
        let position = point(
            &completion_source,
            completion_source.rfind("GetAn").unwrap() + "GetAn".len(),
        )?;
        let choices = fixture
            .query(
                CLIENT_PATH,
                &completion_source,
                3,
                Query::Completion { position },
            )
            .await?;
        let choice = choices
            .result
            .as_array()
            .context("Invalid C# completions")?
            .iter()
            .find(|choice| choice["label"] == "GetAnswer")
            .context("Missing C# member completion")?;
        let resolved = fixture
            .query(
                CLIENT_PATH,
                &completion_source,
                3,
                Query::ResolveCompletion {
                    id: choice["id"]
                        .as_str()
                        .context("Missing completion ID")?
                        .into(),
                },
            )
            .await?;
        let edits = results::completion_edits(&completion_source, position, &resolved.result)
            .map_err(anyhow::Error::msg)?;
        let completed =
            results::apply_edits(&completion_source, &edits).map_err(anyhow::Error::msg)?;
        ensure!(
            completed.contains("GetAnswer()"),
            "Member completion did not replace the current identifier"
        );
        // csharp-ls deliberately omits unimported completion items. Its standard
        // compiler code action supplies a reviewed using directive instead.
        let action_source = FIXED.replace(
            "public string Value => Library.Answers.GetAnswer();",
            "public StringBuilder Value => new StringBuilder();",
        );
        fixture
            .diagnostics(CLIENT_PATH, &action_source, 4, Some("CS0246"))
            .await?;
        let start = action_source.find("StringBuilder").unwrap();
        let actions = fixture
            .query(
                CLIENT_PATH,
                &action_source,
                4,
                Query::CodeActions {
                    range: protocol::Range {
                        start: point(&action_source, start)?,
                        end: point(&action_source, start + "StringBuilder".len())?,
                    },
                },
            )
            .await?;
        let action = actions
            .result
            .as_array()
            .context("Invalid C# code actions")?
            .iter()
            .find(|action| action["title"] == "using System.Text;")
            .context("Missing using-directive code action")?;
        let resolved = fixture
            .query(
                CLIENT_PATH,
                &action_source,
                4,
                Query::ResolveAction {
                    id: action["id"]
                        .as_str()
                        .context("Missing code-action ID")?
                        .into(),
                },
            )
            .await?;
        let edits: results::WorkspaceEdit = serde_json::from_value(resolved.result)?;
        let file = edits
            .files
            .iter()
            .find(|file| file.path == CLIENT_PATH)
            .context("Using directive omitted the caller")?;
        ensure!(file.source_digest == source_digest(&action_source));
        let imported =
            results::apply_edits(&action_source, &file.edits).map_err(anyhow::Error::msg)?;
        ensure!(imported.contains("using System.Text;"));
        fixture.diagnostics(CLIENT_PATH, &imported, 5, None).await?;
        let unformatted = "// 文😀\r\nnamespace Client;public class Consumer{public string Value=>Library.Answers.GetAnswer();}\r\n";
        let formatted = fixture
            .query(
                CLIENT_PATH,
                unformatted,
                6,
                Query::Formatting {
                    tab_width: 4,
                    insert_spaces: true,
                },
            )
            .await?;
        let edits: Vec<results::TextEdit> = serde_json::from_value(formatted.result)?;
        ensure!(
            results::apply_edits(unformatted, &edits).map_err(anyhow::Error::msg)? != unformatted,
            "Formatting returned no change"
        );
        fixture
            .query(CLIENT_PATH, FIXED, 7, Query::Synchronize)
            .await?;
        ensure!(
            fixture
                .query(CLIENT_PATH, "old", 6, Query::Synchronize)
                .await
                .unwrap_err()
                .to_string()
                .contains("stale")
        );
        fixture.client = "editor-b";
        fixture
            .query(LIB_PATH, LIBRARY, 1, Query::Synchronize)
            .await?;
        let other_client = fixture.hover(CLIENT_PATH, CLIENT, 1, "int").await?;
        ensure!(
            other_client.server != first.server,
            "Clients shared unsaved C# state"
        );
        fixture.client = "editor-a";
        fixture.scope.owner = "other-owner".into();
        fixture.scope.project = 2;
        fixture
            .query(LIB_PATH, LIBRARY, 1, Query::Synchronize)
            .await?;
        let other_project = fixture.hover(CLIENT_PATH, CLIENT, 1, "int").await?;
        ensure!(
            other_project.server != first.server,
            "Projects shared a C# service"
        );
        fixture.scope.owner = "fixture-owner".into();
        fixture.scope.project = 1;
        let before_close = fixture.hover(LIB_PATH, UNSAVED, 2, "string").await?;
        fixture.query(CLIENT_PATH, "", 8, Query::Close).await?;
        let retained = fixture.hover(LIB_PATH, UNSAVED, 2, "string").await?;
        ensure!(
            retained.server == before_close.server,
            "Closing one file replaced the warm solution"
        );
        fixture
            .host
            .revoke("fixture-owner", Some(1), Some("community/dotnet"));
        fixture
            .query(LIB_PATH, UNSAVED, 2, Query::Synchronize)
            .await?;
        let restarted = fixture.hover(CLIENT_PATH, FIXED, 9, "string").await?;
        ensure!(
            restarted.server != retained.server,
            "Revocation reused the old service"
        );
        for (path, original) in [(LIB_PATH, LIBRARY), (CLIENT_PATH, CLIENT)] {
            ensure!(
                std::fs::read_to_string(root.path().join(path))? == original,
                "Code intelligence wrote {path} behind Save"
            );
        }
    }
    {
        let root = tempfile::tempdir()?;
        std::fs::write(
            root.path().join("Standalone.csproj"),
            "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>",
        )?;
        std::fs::write(root.path().join("Answers.cs"), LIBRARY)?;
        restore(root.path(), "Standalone.csproj")?;
        let mut fixture = Fixture::new(root.path())?;
        fixture.hover("Answers.cs", LIBRARY, 1, "int").await?;
        fixture.diagnostics("Answers.cs", LIBRARY, 1, None).await?;
    }
    println!(
        "{}",
        json!({"real_csharp_lsp":true,"multi_project_slnx":true,"standalone_csproj":true,"unsaved_cross_project_hover":true,"current_diagnostics_and_clear":true,"source_bound_navigation":true,"read_only_framework_definition":true,"references":true,"symbols":true,"reviewable_cross_project_rename":true,"member_completion":true,"deferred_import_code_action":true,"formatting":true,"client_project_isolation":true,"close_preserves_solution":true,"revocation_restart":true,"source_files_unchanged":true})
    );
    Ok(())
}
