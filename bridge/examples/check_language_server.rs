//! Required real-server probe, run explicitly where rust-analyzer is installed.
//! Production browser/host checks exercise the facade separately.
use openwebide_bridge::languages::process::Process;
use openwebide_core::plugins::languages::{LanguageServer, intelligence};
use serde_json::{Value, json};
use std::time::Duration;

#[path = "support/cache.rs"]
mod cache;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    std::fs::create_dir(root.path().join("src"))?;
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname = \"rust_lsp_fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    )?;
    std::fs::write(
        root.path().join("rust-toolchain.toml"),
        include_str!("../../rust-toolchain.toml"),
    )?;
    let library = "pub fn answer() -> u32 { 42 }\n";
    let main = "fn main() { let value = rust_lsp_fixture::answer(); println!(\"{value}\"); }\n";
    std::fs::write(root.path().join("src/lib.rs"), library)?;
    std::fs::write(root.path().join("src/main.rs"), main)?;
    std::fs::write(
        root.path().join("build.rs"),
        r#"fn main() {
        let path = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("build-count");
        let count = std::fs::read_to_string(&path).ok().and_then(|text| text.parse::<u32>().ok()).unwrap_or(0);
        std::fs::write(path, (count + 1).to_string()).unwrap();
        println!("cargo:rerun-if-changed=build.rs");
    }"#,
    )?;
    let root_uri = url::Url::from_directory_path(root.path())
        .map_err(|()| anyhow::anyhow!("Invalid fixture root"))?;
    let server: LanguageServer = serde_json::from_value(json!({
        "command":std::env::args().nth(1).unwrap_or_else(||"rust-analyzer".into()),
        "initializationOptions":{"cargo":{"buildScripts":{"enable":false}},"procMacro":{"enable":false},"checkOnSave":false}
    }))?;
    let (mut process, mut events) =
        Process::spawn(&server, root.path()).map_err(anyhow::Error::msg)?;
    let client = process.client();
    let answering = client.clone();
    let folder = root_uri.to_string();
    let pump = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            let Some(id) = event.id else {
                continue;
            };
            let result = match event.method.as_str() {
                "workspace/configuration" => Some(Value::Array(
                    event.params["items"]
                        .as_array()
                        .map_or_else(Vec::new, |items| items.iter().map(|_| json!({})).collect()),
                )),
                "workspace/workspaceFolders" => Some(json!([{"uri":folder,"name":"Project"}])),
                "window/workDoneProgress/create" => Some(Value::Null),
                _ => None,
            };
            let reply = result.map_or_else(
                || json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Unsupported client request"}}),
                |result| json!({"jsonrpc":"2.0","id":id,"result":result}),
            );
            if answering.send(reply).await.is_err() {
                break;
            }
        }
    });
    let initialized = client
        .request(
            "initialize",
            openwebide_agent::plugins::languages::initialization_params(
                root_uri.as_str(),
                server.initialization_options.clone(),
                std::process::id(),
            ),
            Duration::from_secs(30),
        )
        .await?;
    anyhow::ensure!(
        initialized["capabilities"]["hoverProvider"] == true,
        "Server did not advertise hover"
    );
    client.notify("initialized", json!({})).await?;
    let library_uri = root_uri.join("src/lib.rs")?;
    let main_uri = root_uri.join("src/main.rs")?;
    for (uri, text) in [(&library_uri, library), (&main_uri, main)] {
        client.notify("textDocument/didOpen", json!({"textDocument":{"uri":uri.as_str(),"languageId":"rust","version":1,"text":text}})).await?;
    }
    let position = intelligence::position(library, library.find("answer").unwrap())
        .map_err(anyhow::Error::msg)?;
    let hover = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let result = client
                .request(
                    "textDocument/hover",
                    json!({"textDocument":{"uri":library_uri.as_str()},"position":position}),
                    Duration::from_secs(10),
                )
                .await?;
            if !result.is_null() {
                break Ok::<_, openwebide_bridge::languages::rpc::Error>(result);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await??;
    anyhow::ensure!(
        hover.to_string().contains("u32"),
        "Hover did not resolve the Rust return type: {hover}"
    );
    let position =
        intelligence::position(main, main.find("answer").unwrap()).map_err(anyhow::Error::msg)?;
    let definition = client
        .request(
            "textDocument/definition",
            json!({"textDocument":{"uri":main_uri.as_str()},"position":position}),
            Duration::from_secs(10),
        )
        .await?;
    anyhow::ensure!(
        definition.to_string().contains("src/lib.rs"),
        "Cross-file definition did not resolve: {definition}"
    );
    println!(
        "{}",
        json!({"real_server":initialized["serverInfo"],"hover":hover,"cross_file_definition":definition})
    );
    process.shutdown().await;
    pump.abort();
    check_project_service(root.path(), server).await?;
    Ok(())
}

fn service_request(
    client: &str,
    path: &str,
    source: &str,
    request: u64,
    revision: u64,
    query: intelligence::Query,
) -> intelligence::Request {
    intelligence::Request {
        version: intelligence::VERSION,
        client: client.into(),
        document: path.into(),
        request,
        revision,
        language: "rust".into(),
        path: path.into(),
        source_digest: openwebide_core::plugins::languages::source_digest(source),
        source: source.into(),
        query,
    }
}

async fn check_project_service(
    root: &std::path::Path,
    mut server: LanguageServer,
) -> anyhow::Result<()> {
    use intelligence::{Position, Query};
    use openwebide_bridge::languages::host::{Host, Scope};
    let storage = tempfile::tempdir()?;
    let mut host = Host::default();
    host.set_cache_directory(storage.path().into())
        .map_err(anyhow::Error::msg)?;
    server.cache = Some(serde_json::from_value(
        json!({"versionArgs":["--version"],"environment":{"CARGO_TARGET_DIR":"${cache}/cargo"}}),
    )?);
    server.initialization_options["cargo"]["buildScripts"]["enable"] = json!(true);
    let cold_started = std::time::Instant::now();
    let scope = Scope {
        owner: "owner-a".into(),
        project: 1,
        root: root.into(),
        provider: "community/rust".into(),
        digest: "a".repeat(64),
        server,
    };
    let source = "pub fn answer() -> u32 { 42 }\n";
    let mut input = service_request(
        "client-a",
        "src/lib.rs",
        source,
        1,
        1,
        Query::Hover {
            position: Position {
                line: 0,
                character: 7,
            },
        },
    );
    let first = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let result = host
                .query(scope.clone(), &input)
                .await
                .map_err(anyhow::Error::msg)?;
            input.request += 1;
            if !result.result.is_null() {
                break Ok::<_, anyhow::Error>(result);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    anyhow::ensure!(
        first.result.to_string().contains("u32"),
        "Native project hover did not resolve: {}",
        first.result
    );
    let cold = cold_started.elapsed();
    let cached = cache::wait_for(storage.path(), |path| {
        path.file_name().is_some_and(|name| name == "build-count")
    })
    .await?;
    let counts: Vec<_> = cached
        .keys()
        .filter(|path| path.file_name().is_some_and(|name| name == "build-count"))
        .map(|path| Ok((path.clone(), std::fs::read_to_string(path)?)))
        .collect::<anyhow::Result<_>>()?;
    anyhow::ensure!(
        !root.join("target").exists(),
        "Rust wrote build output inside the project"
    );
    let warm = host
        .query(scope.clone(), &input)
        .await
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        first.server == warm.server,
        "Identical project query restarted its language server"
    );
    input.request += 1;
    input.query = Query::Symbols;
    let symbols = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let response = host
                .query(scope.clone(), &input)
                .await
                .map_err(anyhow::Error::msg)?;
            input.request += 1;
            if response.result.to_string().contains("answer") {
                break Ok::<_, anyhow::Error>(response);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    anyhow::ensure!(
        symbols.result.to_string().contains("answer"),
        "Document symbols missing answer"
    );
    let main = "fn main() { let value = rust_lsp_fixture::answer(); println!(\"{value}\"); }\n";
    let mut definition_request = service_request(
        "client-a",
        "src/main.rs",
        main,
        1,
        1,
        Query::Definition {
            position: intelligence::position(main, main.find("answer").unwrap())
                .map_err(anyhow::Error::msg)?,
        },
    );
    let definition = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let response = host
                .query(scope.clone(), &definition_request)
                .await
                .map_err(anyhow::Error::msg)?;
            definition_request.request += 1;
            if response
                .result
                .as_array()
                .is_some_and(|locations| !locations.is_empty())
            {
                break Ok::<_, anyhow::Error>(response);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    anyhow::ensure!(
        definition.result.to_string().contains("src/lib.rs"),
        "Project service did not resolve cross-file definition: {}",
        definition.result
    );
    input.request += 1;
    input.query = Query::References {
        position: Position {
            line: 0,
            character: 7,
        },
    };
    let references = host
        .query(scope.clone(), &input)
        .await
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        references.result.to_string().contains("src/main.rs"),
        "Cross-file references missing call site"
    );
    input.request += 1;
    input.query = Query::Rename {
        position: Position {
            line: 0,
            character: 7,
        },
        new_name: "renamed_answer".into(),
    };
    let rename = host
        .query(scope.clone(), &input)
        .await
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        rename.result.to_string().contains("renamed_answer"),
        "Rename did not return workspace edits"
    );
    let unformatted = "fn main(){let value=rust_lsp_fixture::answer();println!(\"{value}\");}\n";
    let format = host
        .query(
            scope.clone(),
            &service_request(
                "client-a",
                "src/main.rs",
                unformatted,
                definition_request.request,
                2,
                Query::Formatting {
                    tab_width: 4,
                    insert_spaces: true,
                },
            ),
        )
        .await
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        format
            .result
            .as_array()
            .is_some_and(|edits| !edits.is_empty()),
        "Rust formatting returned no edits"
    );
    let completion_source = "fn main() { let _ = rust_lsp_fixture::ans }\n";
    let completion = host
        .query(
            scope.clone(),
            &service_request(
                "client-a",
                "src/main.rs",
                completion_source,
                definition_request.request + 1,
                3,
                Query::Completion {
                    position: intelligence::position(
                        completion_source,
                        completion_source.find("ans").unwrap() + 3,
                    )
                    .map_err(anyhow::Error::msg)?,
                },
            ),
        )
        .await
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        completion.result.to_string().contains("answer"),
        "Rust completion omitted answer"
    );

    let choice = completion
        .result
        .as_array()
        .and_then(|items| {
            items.iter().find(|item| {
                item["label"]
                    .as_str()
                    .is_some_and(|label| label.starts_with("answer"))
            })
        })
        .and_then(|item| item["id"].as_str())
        .ok_or_else(|| anyhow::anyhow!("Completion has no source-bound choice"))?;
    let resolved_completion = host
        .query(
            scope.clone(),
            &service_request(
                "client-a",
                "src/main.rs",
                completion_source,
                definition_request.request + 2,
                3,
                Query::ResolveCompletion { id: choice.into() },
            ),
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let edits = intelligence::results::completion_edits(
        completion_source,
        intelligence::position(
            completion_source,
            completion_source.find("ans").unwrap() + 3,
        )
        .map_err(anyhow::Error::msg)?,
        &resolved_completion.result,
    )
    .map_err(anyhow::Error::msg)?;
    let inserted = intelligence::results::apply_edits(completion_source, &edits)
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        inserted.contains("rust_lsp_fixture::answer"),
        "Resolved completion did not insert the selected identifier: {inserted}"
    );
    let external_source = "fn main() { let _ = String::new(); }\n";
    let mut external = service_request(
        "client-a",
        "src/main.rs",
        external_source,
        definition_request.request + 3,
        4,
        Query::Definition {
            position: intelligence::position(
                external_source,
                external_source.find("String").unwrap(),
            )
            .map_err(anyhow::Error::msg)?,
        },
    );
    let target = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let response = host
                .query(scope.clone(), &external)
                .await
                .map_err(anyhow::Error::msg)?;
            external.request += 1;
            if let Some(id) = response
                .result
                .as_array()
                .and_then(|targets| {
                    targets
                        .iter()
                        .find_map(|target| target["resource"].as_str())
                })
                .map(str::to_string)
            {
                break Ok::<_, anyhow::Error>(id);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    external.query = Query::ReadResource { id: target };
    let resource = host
        .query(scope.clone(), &external)
        .await
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        resource.result["readOnly"] == true
            && resource.result["source"]
                .as_str()
                .is_some_and(|source| source.contains("String")),
        "External definition was not a read-only source resource"
    );
    external.query = Query::Close;
    external.request -= 1;
    anyhow::ensure!(
        host.query(scope.clone(), &external).await.is_err(),
        "Stale close removed a current document"
    );
    external.request += 2;
    host.query(scope.clone(), &external)
        .await
        .map_err(anyhow::Error::msg)?;

    input.request += 1;
    let literal =
        intelligence::position(source, source.find("42").unwrap()).map_err(anyhow::Error::msg)?;
    input.query = Query::CodeActions {
        range: intelligence::Range {
            start: literal,
            end: literal,
        },
    };
    let actions = host
        .query(scope.clone(), &input)
        .await
        .map_err(anyhow::Error::msg)?;
    let actions = actions
        .result
        .as_array()
        .filter(|actions| !actions.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Rust code actions were empty"))?;
    let mut resolved = false;
    for action in actions.iter().filter(|action| action["disabled"].is_null()) {
        let Some(id) = action["id"].as_str() else {
            continue;
        };
        input.request += 1;
        input.query = Query::ResolveAction { id: id.into() };
        let result = host
            .query(scope.clone(), &input)
            .await
            .map_err(anyhow::Error::msg)?;
        if result
            .result
            .get("files")
            .and_then(Value::as_array)
            .is_some_and(|files| !files.is_empty())
        {
            resolved = true;
            break;
        }
    }
    anyhow::ensure!(resolved, "No Rust code action resolved to workspace edits");
    let invalid = "pub fn answer( { }";
    input.request += 1;
    input.revision = 2;
    input.source = invalid.into();
    input.source_digest = openwebide_core::plugins::languages::source_digest(invalid);
    input.query = Query::Synchronize;
    let diagnostics = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let result = host
                .query(scope.clone(), &input)
                .await
                .map_err(anyhow::Error::msg)?;
            input.request += 1;
            if result.diagnostics_ready && !result.diagnostics.is_empty() {
                break Ok::<_, anyhow::Error>(result);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    input.revision = 3;
    input.source = source.into();
    input.source_digest = openwebide_core::plugins::languages::source_digest(source);
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let result = host
                .query(scope.clone(), &input)
                .await
                .map_err(anyhow::Error::msg)?;
            input.request += 1;
            if result.diagnostics_ready {
                anyhow::ensure!(
                    result.diagnostics.is_empty(),
                    "Old diagnostics survived a source change"
                );
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;

    let other = "pub fn answer() -> &'static str { \"client-b\" }\n";
    let mut isolated_request = service_request(
        "client-b",
        "src/lib.rs",
        other,
        1,
        1,
        Query::Hover {
            position: Position {
                line: 0,
                character: 7,
            },
        },
    );
    let isolated = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let response = host
                .query(scope.clone(), &isolated_request)
                .await
                .map_err(anyhow::Error::msg)?;
            isolated_request.request += 1;
            if !response.result.is_null() {
                break Ok::<_, anyhow::Error>(response);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    anyhow::ensure!(
        isolated.server != first.server && isolated.result.to_string().contains("str"),
        "Unsaved clients shared a server or source: {}",
        isolated.result
    );
    input.query = Query::Hover {
        position: Position {
            line: 0,
            character: 7,
        },
    };
    let original = host
        .query(scope.clone(), &input)
        .await
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        original.server != isolated.server && original.result.to_string().contains("u32"),
        "Another client's source replaced the original overlay: {}",
        original.result
    );
    let removed_path = root.join("src/deleted/old.rs");
    std::fs::create_dir_all(removed_path.parent().unwrap())?;
    std::fs::write(&removed_path, "fn old() {}\n")?;
    let mut removed = service_request(
        "client-a",
        "src/deleted/old.rs",
        "fn old() {}\n",
        1,
        1,
        Query::Synchronize,
    );
    host.query(scope.clone(), &removed)
        .await
        .map_err(anyhow::Error::msg)?;
    std::fs::remove_dir_all(removed_path.parent().unwrap())?;
    removed.request += 1;
    removed.query = Query::Close;
    host.query(scope.clone(), &removed)
        .await
        .map_err(anyhow::Error::msg)?;
    host.revoke("owner-a", Some(1), Some("community/rust"));
    input.request += 1;
    let restarted = host
        .query(scope.clone(), &input)
        .await
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        restarted.server != original.server,
        "Revoked native server was reused"
    );
    host.shutdown();
    openwebide_bridge::exec::proc::wait_for_cleanup().await;
    let before = cache::snapshot(storage.path())?;
    let namespaces = cache::namespaces(storage.path())?;
    let mut restored = Host::default();
    restored
        .set_cache_directory(storage.path().into())
        .map_err(anyhow::Error::msg)?;
    let restart_started = std::time::Instant::now();
    input.request += 1;
    input.query = Query::Hover {
        position: Position {
            line: 0,
            character: 7,
        },
    };
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let response = restored
                .query(scope.clone(), &input)
                .await
                .map_err(anyhow::Error::msg)?;
            input.request += 1;
            if response.result.to_string().contains("u32") {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    let restart = restart_started.elapsed();
    // A single-file hover can resolve before Cargo workspace loading finishes.
    // Require the restored project graph before inspecting build-script reuse.
    definition_request.request += 10;
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let response = restored
                .query(scope.clone(), &definition_request)
                .await
                .map_err(anyhow::Error::msg)?;
            definition_request.request += 1;
            if response.result.to_string().contains("src/lib.rs") {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    let after = cache::snapshot(storage.path())?;
    anyhow::ensure!(
        cache::namespaces(storage.path())? == namespaces,
        "Rust allocated a new cache namespace instead of reopening the existing one"
    );
    for (path, count) in counts {
        anyhow::ensure!(
            std::fs::read_to_string(path)? == count,
            "Cargo reran an unchanged build script instead of reusing its cache"
        );
    }
    anyhow::ensure!(!root.join("target").exists());
    cache::report("rust-analyzer", &before, &after, cold, restart)?;
    restored.shutdown();
    openwebide_bridge::exec::proc::wait_for_cleanup().await;
    println!(
        "{}",
        json!({"native_project_service":true,"warm_reuse":true,"symbols":true,"definition":true,"references":true,"rename_edits":true,"formatting_edits":true,"completion":true,"completion_insertion":true,"external_definition_read_only":true,"stale_close_rejected":true,"deleted_document_closed":true,"code_action_edits":true,"versioned_diagnostics":diagnostics.diagnostics.len(),"unsaved_clients_isolated":true,"revocation_restart":true})
    );
    Ok(())
}
