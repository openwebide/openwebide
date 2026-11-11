//! Real C/C++ contract through the shared native language-service facade.
use anyhow::{Context, Result, ensure};
use openwebide_bridge::languages::host::{Host, Scope};
use openwebide_core::plugins::languages::{
    intelligence::{self as protocol, Position, Query, Response, results},
    source_digest,
};
use serde_json::json;
use std::{collections::BTreeMap, path::Path, time::Duration};

const HEADER: &str = "#pragma once\r\n#ifndef BUILD_VALUE\r\n#error Missing project flags\r\n#endif\r\ninline int answer() { return BUILD_VALUE; }\r\n";
const C_HEADER: &str = "#pragma once\r\n#ifndef BUILD_VALUE\r\n#error Missing project flags\r\n#endif\r\nstatic inline int answer(void) { return BUILD_VALUE; }\r\n";
const CPP: &str = "#include \"api.hpp\" // IWYU pragma: keep\r\nconst char *note = \"文😀\"; auto value = answer();\r\n";
const C: &str = "#include \"c_api.h\" // IWYU pragma: keep\r\nconst char *note = \"文😀\"; long c_value(void) { return answer(); }\r\n";

struct Fixture {
    host: Host,
    scope: Scope,
    sequence: u64,
    client: &'static str,
    generations: BTreeMap<String, u64>,
}

impl Fixture {
    fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            host: Host::default(),
            scope: Scope {
                owner: "fixture-owner".into(),
                project: 1,
                root: root.into(),
                provider: "community/native-family".into(),
                digest: "a".repeat(64),
                server: serde_json::from_value(json!({
                    "command":"clangd", "args":["--background-index", "--use-dirty-headers", "--pch-storage=memory", "--log=error"]
                }))?,
            },
            sequence: 0,
            client: "editor-a",
            generations: BTreeMap::new(),
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
            document: format!("{path}:{}", self.generations.get(path).unwrap_or(&0)),
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
        .await
        .with_context(|| format!("Hover {path} expected {expected}"))?
    }

    async fn diagnostics(&mut self, source: &str, revision: u64, broken: bool) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                let response = self
                    .query("main.cpp", "cpp", source, revision, Query::Synchronize)
                    .await?;
                if response.diagnostics_ready && response.diagnostics.is_empty() != broken {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .with_context(|| format!("Diagnostics revision {revision} broken {broken}"))?
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
    std::fs::create_dir(root.path().join("include"))?;
    let files = [
        ("include/api.hpp", HEADER),
        ("include/c_api.h", C_HEADER),
        ("main.cpp", CPP),
        ("main.c", C),
    ];
    for (path, source) in files {
        std::fs::write(root.path().join(path), source)?;
    }
    let commands: Vec<_> = [
        ("main.c", "clang", "-std=c17"),
        ("main.cpp", "clang++", "-std=c++20"),
    ]
    .into_iter()
    .map(|(file, compiler, standard)| {
        json!({
            "directory":std::fs::canonicalize(root.path()).unwrap(), "file":file,
            "arguments":[compiler,standard,"-Iinclude","-DBUILD_VALUE=42","-c",file]
        })
    })
    .collect();
    std::fs::write(
        root.path().join("compile_commands.json"),
        serde_json::to_vec(&commands)?,
    )?;
    let mut fixture = Fixture::new(root.path())?;
    eprintln!("clangd: starting compilation-database C/C++ contract");
    let first = fixture
        .query("include/api.hpp", "cpp", HEADER, 1, Query::Synchronize)
        .await?;
    fixture
        .query("include/c_api.h", "c", C_HEADER, 1, Query::Synchronize)
        .await?;
    for (path, language, source) in [("main.c", "c", C), ("main.cpp", "cpp", CPP)] {
        let response = fixture.hover(path, language, source, "int").await?;
        ensure!(
            response.server == first.server,
            "C and C++ must share one service"
        );
    }
    fixture.diagnostics(CPP, 1, false).await?;
    eprintln!("clangd: checking unsaved header overlays");
    let unsaved = HEADER.replace("inline int answer", "inline long answer");
    fixture
        .query("include/api.hpp", "cpp", &unsaved, 2, Query::Synchronize)
        .await?;
    fixture
        .query(
            "include/c_api.h",
            "c",
            &C_HEADER.replace("inline int answer", "inline long answer"),
            2,
            Query::Synchronize,
        )
        .await?;
    for (path, language, source) in [("main.c", "c", C), ("main.cpp", "cpp", CPP)] {
        fixture.hover(path, language, source, "long").await?;
    }
    eprintln!("clangd: checking navigation and reviewed rename");
    let position = point(CPP, CPP.rfind("answer").unwrap())?;
    let response = fixture
        .query("main.cpp", "cpp", CPP, 1, Query::Definition { position })
        .await?;
    let targets: Vec<results::Target> = serde_json::from_value(response.result)?;
    ensure!(
        targets
            .iter()
            .any(|t| t.path.as_deref() == Some("include/api.hpp")
                && t.source_digest.as_deref() == Some(&source_digest(&unsaved))),
        "Definition lost unsaved header binding"
    );
    let symbols = fixture
        .query("include/api.hpp", "cpp", &unsaved, 2, Query::Symbols)
        .await?;
    ensure!(symbols.result.to_string().contains("answer"));
    let references = fixture
        .query("main.cpp", "cpp", CPP, 1, Query::References { position })
        .await?;
    let targets: Vec<results::Target> = serde_json::from_value(references.result)?;
    ensure!(
        targets
            .iter()
            .any(|t| t.path.as_deref() == Some("main.cpp")),
        "Missing cross-file references"
    );
    let rename = fixture
        .query(
            "main.cpp",
            "cpp",
            CPP,
            1,
            Query::Rename {
                position,
                new_name: "renamed_answer".into(),
            },
        )
        .await?;
    let edits: results::WorkspaceEdit = serde_json::from_value(rename.result)?;
    for (path, source) in [("include/api.hpp", unsaved.as_str()), ("main.cpp", CPP)] {
        let file = edits
            .files
            .iter()
            .find(|f| f.path == path)
            .with_context(|| format!("Missing renamed file {path}"))?;
        ensure!(file.source_digest == source_digest(source));
        let after = results::apply_edits(source, &file.edits).map_err(anyhow::Error::msg)?;
        ensure!(after.contains("renamed_answer") && !after.contains("= answer()"));
    }
    let broken = format!("{CPP}\r\nint broken = \"bad\";\r\n");
    fixture.diagnostics(&broken, 2, true).await?;
    fixture.diagnostics(CPP, 3, false).await?;
    ensure!(
        fixture
            .query("main.cpp", "cpp", "old", 2, Query::Synchronize)
            .await
            .unwrap_err()
            .to_string()
            .contains("stale")
    );
    let completion_source =
        "struct Item { int member; };\r\nint demo() { Item item; return item.mem; }\r\n";
    let position = point(
        completion_source,
        completion_source.find("item.mem").unwrap() + 8,
    )?;
    let completion = fixture
        .query(
            "main.cpp",
            "cpp",
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
        .find(|x| x["label"].as_str().is_some_and(|s| s.contains("member")))
        .with_context(|| format!("Missing member completion: {}", completion.result))?;
    let resolved = fixture
        .query(
            "main.cpp",
            "cpp",
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
    ensure!(
        results::apply_edits(completion_source, &edits)
            .map_err(anyhow::Error::msg)?
            .contains("item.member")
    );
    let unformatted = "int formatted(){return 1;}\r\n";
    let response = fixture
        .query(
            "main.cpp",
            "cpp",
            unformatted,
            5,
            Query::Formatting {
                tab_width: 4,
                insert_spaces: true,
            },
        )
        .await?;
    let edits: Vec<results::TextEdit> = serde_json::from_value(response.result)?;
    ensure!(
        !edits.is_empty()
            && results::apply_edits(unformatted, &edits)
                .map_err(anyhow::Error::msg)?
                .contains("return 1;")
    );
    fixture
        .query("main.cpp", "cpp", unformatted, 5, Query::Close)
        .await?;
    let retained = fixture
        .query("include/api.hpp", "cpp", &unsaved, 2, Query::Symbols)
        .await?;
    ensure!(retained.server == first.server);
    fixture.client = "editor-b";
    let isolated = fixture.hover("main.cpp", "cpp", CPP, "int").await?;
    ensure!(isolated.server != first.server);
    fixture.client = "editor-a";
    fixture.hover("main.cpp", "cpp", CPP, "long").await?;
    fixture.scope.owner = "other-owner".into();
    let owner = fixture.hover("main.cpp", "cpp", CPP, "int").await?;
    ensure!(owner.server != first.server);
    fixture.scope.owner = "fixture-owner".into();
    fixture.scope.project = 2;
    let project = fixture.hover("main.cpp", "cpp", CPP, "int").await?;
    ensure!(project.server != first.server && project.server != owner.server);
    fixture.scope.project = 1;
    let before_revoke = fixture.hover("main.cpp", "cpp", CPP, "long").await?;
    fixture
        .host
        .revoke("fixture-owner", Some(1), Some("community/native-family"));
    let restarted = fixture
        .query("include/api.hpp", "cpp", &unsaved, 2, Query::Synchronize)
        .await?;
    ensure!(restarted.server != before_revoke.server);
    fixture.hover("main.cpp", "cpp", CPP, "long").await?;
    for (path, source) in files {
        ensure!(
            std::fs::read(root.path().join(path))? == source.as_bytes(),
            "Server changed source {path}"
        );
    }
    let fallback = tempfile::tempdir()?;
    std::fs::write(
        fallback.path().join(".clangd"),
        "CompileFlags:\n  Add: [-std=c++20, -DFROM_CONFIG=42]\n",
    )?;
    let source = "int answer() { return FROM_CONFIG; }\n";
    std::fs::write(fallback.path().join("main.cpp"), source)?;
    let mut folder = Fixture::new(fallback.path())?;
    folder.hover("main.cpp", "cpp", source, "int").await?;
    folder.diagnostics(source, 1, false).await?;
    println!(
        "{}",
        json!({"compilation_database_flags":true,"fallback_project_configuration":true,"c_cpp_one_service":true,"unsaved_header_overlays":true,"utf16_crlf_navigation":true,"source_bound_definition":true,"references_and_symbols":true,"reviewable_rename":true,"diagnostics_clear":true,"resolved_member_completion":true,"formatting":true,"close_retains_service":true,"client_owner_project_isolation":true,"revocation_restart":true,"source_files_unchanged":true})
    );
    Ok(())
}
