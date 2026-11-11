//! Real TypeScript server contract, including one shared service for dialects.
use anyhow::{Context, Result, ensure};
use openwebide_bridge::languages::host::{Host, Scope};
use openwebide_core::plugins::languages::{
    LanguageServer,
    intelligence::{
        self as protocol, Position, Query, Response,
        actions::{
            ActionReview, CommandInvocation, CommandOutcome, CommandProgress, origin_fingerprint,
        },
        results,
    },
    source_digest,
};
use serde_json::json;
use std::{path::Path, time::Duration};

const LIBRARY: &str = "export function answer(): number { return 42; }\r\n";
const UNSAVED: &str = "export function answer(): string { return 'unsaved'; }\r\n";
const MAIN: &str =
    "// 文😀\r\nimport { answer } from './lib';\r\nexport const current = answer();\r\n";
const JS: &str = "// 文😀\r\nimport { answer } from './lib';\r\nexport const plain = answer();\r\n";
const TSX: &str =
    "import { answer } from './lib';\r\nexport const view = <div>{answer()}</div>;\r\n";
const JSX: &str =
    "import { answer } from './lib';\r\nexport const widget = <div>{answer()}</div>;\r\n";

struct Fixture {
    host: Host,
    scope: Scope,
    sequence: u64,
    client: &'static str,
}

impl Fixture {
    fn new(root: &Path) -> Result<Self> {
        let server: LanguageServer = serde_json::from_value(json!({
            "command":"typescript-language-server", "args":["--stdio"],
            "initializationOptions":{"disableAutomaticTypingAcquisition":true,"tsserver":{"useSyntaxServer":"never"}},
            "settings":{"typescript":{},"javascript":{}}
        }))?;
        Ok(Self {
            host: Host::default(),
            scope: Scope {
                owner: "fixture-owner".into(),
                project: 1,
                root: root.into(),
                provider: "community/script-family".into(),
                digest: "a".repeat(64),
                server,
            },
            sequence: 0,
            client: "editor-a",
        })
    }

    async fn query(
        &mut self,
        path: &str,
        language: &str,
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
            language: language.into(),
            path: path.into(),
            source_digest: source_digest(source),
            source: source.into(),
            query,
        };
        let mut scope = self.scope.clone();
        scope.server.language_id = Some(
            match language {
                "jsx" => "javascriptreact",
                "tsx" => "typescriptreact",
                language => language,
            }
            .into(),
        );
        let response = self
            .host
            .query(scope, &input)
            .await
            .map_err(anyhow::Error::msg)?;
        response.validate(&input).map_err(anyhow::Error::msg)?;
        Ok(response)
    }

    async fn hover(
        &mut self,
        path: &str,
        language: &str,
        source: &str,
        expected: &str,
    ) -> Result<Response> {
        let position = point(
            source,
            source.rfind("answer").context("Missing hover name")?,
        )?;
        tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                let response = self
                    .query(path, language, source, 1, Query::Hover { position })
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
                    .query(
                        "main.ts",
                        "typescript",
                        source,
                        revision,
                        Query::Synchronize,
                    )
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

#[tokio::main]
async fn main() -> Result<()> {
    let root = tempfile::tempdir()?;
    let config = json!({"compilerOptions":{
        "target":"ES2022","module":"ESNext","moduleResolution":"bundler", "jsx":"preserve",
        "allowJs":true,"checkJs":true,"strict":true,"noEmit":true,"types":[]
    },"include":["*.ts","*.tsx","*.js","*.jsx"]});
    std::fs::write(root.path().join("tsconfig.json"), config.to_string())?;
    std::fs::write(root.path().join("package.json"), "{\"type\":\"module\"}")?;
    std::fs::write(
        root.path().join("types.d.ts"),
        "declare namespace JSX { interface IntrinsicElements { div: any; } }\n",
    )?;
    std::fs::write(
        root.path().join("extra.ts"),
        "export function extraAnswer(): number { return 42; }\n",
    )?;
    for (path, source) in [
        ("lib.ts", LIBRARY),
        ("main.ts", MAIN),
        ("plain.js", JS),
        ("view.tsx", TSX),
        ("widget.jsx", JSX),
    ] {
        std::fs::write(root.path().join(path), source)?;
    }
    let mut fixture = Fixture::new(root.path())?;
    let first = fixture
        .query("lib.ts", "typescript", LIBRARY, 1, Query::Synchronize)
        .await?;
    for (path, language, source) in [
        ("main.ts", "typescript", MAIN),
        ("plain.js", "javascript", JS),
        ("view.tsx", "tsx", TSX),
        ("widget.jsx", "jsx", JSX),
    ] {
        let response = fixture.hover(path, language, source, "number").await?;
        ensure!(
            response.server == first.server,
            "A dialect started a different server"
        );
    }
    fixture
        .query("lib.ts", "typescript", UNSAVED, 2, Query::Synchronize)
        .await?;
    for (path, language, source) in [
        ("main.ts", "typescript", MAIN),
        ("plain.js", "javascript", JS),
        ("view.tsx", "tsx", TSX),
        ("widget.jsx", "jsx", JSX),
    ] {
        let response = fixture.hover(path, language, source, "string").await?;
        ensure!(
            response.server == first.server,
            "The warm server was replaced"
        );
    }
    let position = point(MAIN, MAIN.rfind("answer").unwrap())?;
    let definition = fixture
        .query(
            "main.ts",
            "typescript",
            MAIN,
            1,
            Query::Definition { position },
        )
        .await?;
    let targets: Vec<results::Target> = serde_json::from_value(definition.result)?;
    ensure!(
        targets
            .iter()
            .any(|target| target.path.as_deref() == Some("lib.ts")
                && target.source_digest.as_deref() == Some(&source_digest(UNSAVED))),
        "Definition lost its unsaved source binding"
    );
    let references = fixture
        .query(
            "main.ts",
            "typescript",
            MAIN,
            1,
            Query::References { position },
        )
        .await?;
    let targets: Vec<results::Target> = serde_json::from_value(references.result)?;
    ensure!(targets.len() >= 5, "Cross-dialect references were missing");
    let symbols = fixture
        .query("main.ts", "typescript", MAIN, 1, Query::Symbols)
        .await?;
    ensure!(
        !symbols
            .result
            .as_array()
            .context("Invalid symbols")?
            .is_empty()
    );
    let rename = fixture
        .query(
            "lib.ts",
            "typescript",
            UNSAVED,
            2,
            Query::Rename {
                position: point(UNSAVED, UNSAVED.find("answer").unwrap())?,
                new_name: "renamedAnswer".into(),
            },
        )
        .await?;
    let edits: results::WorkspaceEdit = serde_json::from_value(rename.result)?;
    for (path, source) in [
        ("lib.ts", UNSAVED),
        ("main.ts", MAIN),
        ("plain.js", JS),
        ("view.tsx", TSX),
        ("widget.jsx", JSX),
    ] {
        let file = edits
            .files
            .iter()
            .find(|file| file.path == path)
            .with_context(|| {
                format!(
                    "Missing renamed dialect {path}; edits: {:?}",
                    edits
                        .files
                        .iter()
                        .map(|file| &file.path)
                        .collect::<Vec<_>>()
                )
            })?;
        ensure!(file.source_digest == source_digest(source));
        let after = results::apply_edits(source, &file.edits).map_err(anyhow::Error::msg)?;
        ensure!(after.contains("renamedAnswer"), "Rename omitted {path}");
    }
    // TypeScript owns configured/inferred projects inside this app project.
    // A nested tsconfig must not split the dialects into another native service.
    std::fs::create_dir(root.path().join("nested"))?;
    std::fs::write(
        root.path().join("nested/tsconfig.json"),
        r#"{"extends":"../tsconfig.json","include":["*.ts"]}"#,
    )?;
    let nested = "import { answer } from '../lib';\r\nexport const nested = answer();\r\n";
    std::fs::write(root.path().join("nested/main.ts"), nested)?;
    let response = fixture
        .hover("nested/main.ts", "typescript", nested, "string")
        .await?;
    ensure!(
        response.server == first.server,
        "A nested configuration split the project service"
    );
    let unformatted = "// 文😀\r\nexport const current={a:1,b:2};\r\n";
    let formatted = fixture
        .query(
            "main.ts",
            "typescript",
            unformatted,
            2,
            Query::Formatting {
                tab_width: 2,
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
        .diagnostics(
            "// 文😀\r\nexport const broken: number = 'wrong';\r\n",
            3,
            Some("2322"),
        )
        .await?;
    let fixed = "// 文😀\r\nexport const broken: number = 1;\r\n";
    fixture.diagnostics(fixed, 4, None).await?;
    ensure!(
        fixture
            .query("main.ts", "typescript", "old", 3, Query::Synchronize)
            .await
            .unwrap_err()
            .to_string()
            .contains("stale")
    );
    let completion_source = "// 文😀\r\nexport const next = extraAn();\r\n";
    let completion_position = point(
        completion_source,
        completion_source.find("extraAn").unwrap() + "extraAn".len(),
    )?;
    let completion = fixture
        .query(
            "main.ts",
            "typescript",
            completion_source,
            5,
            Query::Completion {
                position: completion_position,
            },
        )
        .await?;
    let choice = completion
        .result
        .as_array()
        .context("Invalid completions")?
        .iter()
        .find(|item| item["label"] == "extraAnswer")
        .context("Missing auto-import completion")?;
    let resolved = fixture
        .query(
            "main.ts",
            "typescript",
            completion_source,
            5,
            Query::ResolveCompletion {
                id: choice["id"]
                    .as_str()
                    .context("Missing completion ID")?
                    .into(),
            },
        )
        .await?;
    let edits = results::completion_edits(completion_source, completion_position, &resolved.result)
        .map_err(anyhow::Error::msg)?;
    let after = results::apply_edits(completion_source, &edits).map_err(anyhow::Error::msg)?;
    ensure!(
        after.contains("import") && after.contains("extraAnswer()"),
        "Completion omitted its import: {after}"
    );
    let action_source = "// 文😀\r\nexport const next = extraAnswer();\r\n";
    fixture.diagnostics(action_source, 6, Some("2304")).await?;
    let start = action_source.find("extraAnswer").unwrap();
    let actions = fixture
        .query(
            "main.ts",
            "typescript",
            action_source,
            6,
            Query::CodeActions {
                range: protocol::Range {
                    start: point(action_source, start)?,
                    end: point(action_source, start + "extraAnswer".len())?,
                },
            },
        )
        .await?;
    let action = actions
        .result
        .as_array()
        .context("Invalid code actions")?
        .iter()
        .find(|item| {
            item["title"]
                .as_str()
                .is_some_and(|title| title.to_lowercase().contains("import"))
        })
        .context("Missing import code action")?;
    let action_id = action["id"]
        .as_str()
        .context("Missing action ID")?
        .to_string();
    let action = fixture
        .query(
            "main.ts",
            "typescript",
            action_source,
            6,
            Query::ReviewAction {
                id: action_id.clone(),
            },
        )
        .await?;
    let review: ActionReview = serde_json::from_value(action.result)?;
    let edit = &review.edit;
    ensure!(
        edit.files.iter().any(|file| file.path == "main.ts"),
        "Code action omitted the current file"
    );
    ensure!(
        edit.changes.is_empty() && edit.files.iter().all(|file| file.path == "main.ts"),
        "Import action unexpectedly changed another resource"
    );
    let after =
        results::apply_edits(action_source, &edit.files[0].edits).map_err(anyhow::Error::msg)?;
    let command = review
        .command
        .as_ref()
        .context("The pinned server's code-action command was discarded")?;
    let original = protocol::Request {
        version: protocol::VERSION,
        client: fixture.client.into(),
        document: "main.ts".into(),
        path: "main.ts".into(),
        language: "typescript".into(),
        request: action.request,
        revision: 6,
        source: action_source.into(),
        source_digest: source_digest(action_source),
        query: Query::ReviewAction { id: action_id },
    };
    // This example verifies the trusted native primitive. Actual manual grants,
    // durable claims and editor recovery are separate storage/UI contracts.
    let invocation = CommandInvocation {
        id: "b".repeat(32),
        client: fixture.client.into(),
        command: command.id.clone(),
        server: command.server.clone(),
        origin: origin_fingerprint(&original).map_err(anyhow::Error::msg)?,
        review: review.fingerprint().map_err(anyhow::Error::msg)?,
        approval: "c".repeat(64),
    };
    fixture
        .query("main.ts", "typescript", &after, 7, Query::Synchronize)
        .await?;
    fixture
        .host
        .start_command(fixture.scope.clone(), invocation.clone())
        .await
        .map_err(anyhow::Error::msg)?;
    let outcome = tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            if let CommandProgress::Finished { outcome } = fixture
                .host
                .command_progress(fixture.scope.clone(), &invocation)
                .await
                .map_err(anyhow::Error::msg)?
            {
                return Ok::<_, anyhow::Error>(outcome);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await??;
    ensure!(
        matches!(&outcome, CommandOutcome::Completed { .. }),
        "Import action command did not complete: {outcome:?}"
    );
    ensure!(
        fixture
            .host
            .start_command(fixture.scope.clone(), invocation)
            .await
            .map_err(anyhow::Error::msg)?
            == CommandProgress::Finished { outcome },
        "Duplicate dispatch did not return the retained outcome"
    );
    fixture.diagnostics(&after, 7, None).await?;
    std::fs::remove_file(root.path().join("widget.jsx"))?;
    fixture
        .query("widget.jsx", "jsx", "", 2, Query::Close)
        .await?;
    let retained = fixture
        .hover("plain.js", "javascript", JS, "string")
        .await?;
    ensure!(
        retained.server == first.server,
        "Closing JSX discarded other dialects"
    );
    fixture.client = "editor-b";
    let other_source = "export function answer(): boolean { return true; }\r\n";
    let other = fixture
        .query("lib.ts", "typescript", other_source, 1, Query::Synchronize)
        .await?;
    ensure!(
        other.server != first.server,
        "Editing clients shared unsaved state"
    );
    fixture
        .hover("plain.js", "javascript", JS, "boolean")
        .await?;
    fixture.client = "editor-a";
    let before_revoke = fixture
        .hover("plain.js", "javascript", JS, "string")
        .await?;
    fixture
        .host
        .revoke("fixture-owner", Some(1), Some("community/script-family"));
    let restarted = fixture
        .query("lib.ts", "typescript", UNSAVED, 2, Query::Synchronize)
        .await?;
    ensure!(
        restarted.server != before_revoke.server,
        "Revocation did not replace the service"
    );
    fixture
        .hover("plain.js", "javascript", JS, "string")
        .await?;
    ensure!(std::fs::read_to_string(root.path().join("lib.ts"))? == LIBRARY);
    ensure!(std::fs::read_to_string(root.path().join("main.ts"))? == MAIN);
    println!(
        "{}",
        json!({"dialects":4,"shared_server":true,"unsaved_cross_dialect_types":true,"source_bound_navigation":true,"references":true,"symbols":true,"rename":true,"formatting":true,"versionless_diagnostics":true,"completion_auto_import":true,"code_actions":true,"closed_dialect_preserves_other_documents":true,"client_isolation":true,"revocation_restart":true,"source_files_unchanged":true})
    );
    Ok(())
}
