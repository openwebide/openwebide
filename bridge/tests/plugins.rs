//! Both plugin transports authenticate into isolated host caches.
#[path = "plugins/analysis.rs"]
mod analysis;
#[path = "plugins/cache.rs"]
mod cache;
mod common;
#[path = "plugins/prefetch.rs"]
mod prefetch;
#[path = "plugins/reuse.rs"]
mod reuse;
use common::{HttpResponse, http, post};
use openwebide_bridge::{ServerConfig, plugins::NativePluginInstaller, run_server};
use openwebide_core::plugins::{
    PreparedPlugin, content_digest,
    preparation::{PluginPreparation, PreparationState},
    testing::{review_files, source},
};
use std::path::Path;

async fn preparation_request(
    port: u16,
    paired: bool,
    path: &str,
    payload: serde_json::Value,
) -> HttpResponse {
    let body = payload.to_string();
    if paired {
        let raw = format!(
            "POST {path} HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:3000\r\nAuthorization: Bearer paired-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len(),
        );
        HttpResponse::parse(&http(port, &raw).await)
    } else {
        post(port, path, &body, &[("Content-Type", "application/json")]).await
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

#[tokio::test]
async fn native_language_routes_check_account_package_and_path_before_launch() {
    use openwebide_core::plugins::languages::{intelligence, source_digest};
    let cache = tempfile::tempdir().unwrap();
    let fixture = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("file.notes"), "notes").unwrap();
    git(workspace.path(), &["init", "-q", "-b", "main"]);
    git(
        workspace.path(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.org",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ],
    );
    let lifecycle_peer = workspace.path().join("lifecycle.py");
    std::fs::write(
        &lifecycle_peer,
        include_str!("../../tools/fixtures/lifecycle_server.py"),
    )
    .unwrap();
    let reuse_peer = workspace.path().join("reuse.py");
    std::fs::write(
        &reuse_peer,
        include_str!("../../tools/fixtures/reuse_server.py"),
    )
    .unwrap();
    let cache_peer = workspace.path().join("cache.py");
    std::fs::write(
        &cache_peer,
        include_str!("../../tools/fixtures/cache_server.py"),
    )
    .unwrap();
    let prefetch_peer = workspace.path().join("prefetch.py");
    std::fs::write(
        &prefetch_peer,
        include_str!("../../tools/fixtures/prefetch_server.py"),
    )
    .unwrap();
    #[cfg(unix)]
    let (live_command, live_args) = {
        use std::os::unix::fs::PermissionsExt;
        let content = std::fs::read_to_string(&lifecycle_peer).unwrap();
        std::fs::write(
            &lifecycle_peer,
            format!("#!/usr/bin/env python3\n{content}"),
        )
        .unwrap();
        std::fs::set_permissions(&lifecycle_peer, std::fs::Permissions::from_mode(0o700)).unwrap();
        (
            lifecycle_peer.to_string_lossy().into_owned(),
            Vec::<String>::new(),
        )
    };
    #[cfg(not(unix))]
    let (live_command, live_args) = (
        "python3".to_string(),
        vec![lifecycle_peer.to_string_lossy().into_owned()],
    );
    for mut file in review_files() {
        if file.path == "plugin.json" {
            let mut manifest: serde_json::Value = serde_json::from_slice(&file.content).unwrap();
            manifest["compatibility"]["pluginApi"] = serde_json::json!(9);
            manifest["contributions"] = serde_json::json!({"skills":[],"languages":[{
                "id":"notes","name":"Notes","extensions":["notes"],"features":{"languageServer":{"command":"openwebide-test-language-server-not-installed"}}
            }, {
                "id":"live-notes","name":"Live Notes","extensions":["live-notes"],"features":{"languageServer":{"command":live_command,"args":live_args,"rootMarkers":["project.json"],"projectInputs":{"files":["settings.json"]}}}
            }, {
                "id":"reuse-notes","name":"Reuse Notes","extensions":["reuse-notes"],"features":{"languageServer":{"command":"python3","args":[reuse_peer]}}
            }, {
                "id":"cache-notes","name":"Cache Notes","extensions":["cache-notes"],"features":{"languageServer":{"command":"python3","args":[cache_peer],"cache":{"versionArgs":[cache_peer,"--version"],"environment":{"FIXTURE_CACHE":"${cache}/build"}},"initializationOptions":{"directory":"${cache}/options"},"settings":{"directory":"${cache}/settings"}}}
            }, {
                "id":"prefetch-notes","name":"Prefetch Notes","extensions":["prefetch-notes"],"features":{"languageServer":{"command":"python3","args":[prefetch_peer]}}
            }]});
            file.content = serde_json::to_vec(&manifest).unwrap();
        }
        let path = fixture.path().join("plugins/pr-review").join(file.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, file.content).unwrap();
    }
    git(fixture.path(), &["init", "-q"]);
    git(fixture.path(), &["add", "."]);
    git(
        fixture.path(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.org",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    let mut source = source();
    source.commit = git(fixture.path(), &["rev-parse", "HEAD"]);
    for owner in ["paired", "user:42"] {
        let repo = cache
            .path()
            .join(content_digest(owner.as_bytes()))
            .join("repositories")
            .join(content_digest(source.repository.as_bytes()));
        std::fs::create_dir_all(repo.parent().unwrap()).unwrap();
        git(
            cache.path(),
            &[
                "clone",
                "--bare",
                fixture.path().to_str().unwrap(),
                repo.to_str().unwrap(),
            ],
        );
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut config = ServerConfig::new(
        workspace.path().into(),
        "dummy_secret".into(),
        Some("paired-token".into()),
    );
    config.plugins = NativePluginInstaller::new(Some(cache.path().into()));
    config
        .language_servers
        .set_cache_directory(cache.path().join("languages"))
        .unwrap();
    let server = tokio::spawn(run_server(listener, config));
    for paired in [false, true] {
        let prepared = preparation_request(
            port,
            paired,
            "/plugins/prepare",
            serde_json::json!({"source":source,"user":42}),
        )
        .await;
        assert_eq!(prepared.status, 200, "{}", prepared.body);
        let prepared: PreparedPlugin = serde_json::from_str(&prepared.body).unwrap();
        assert!(prepared.manifest.executable.is_none());
        reuse::check(port, paired, &prepared, workspace.path()).await;
        analysis::check(port, paired, &prepared, workspace.path()).await;
        cache::check(port, paired, &prepared, workspace.path()).await;
        prefetch::check(port, paired, &prepared, workspace.path()).await;
        let request = intelligence::HostRequest {
            user: Some(42),
            root: workspace.path().to_string_lossy().into(),
            selection: intelligence::Selection {
                user: 42,
                project: 1,
                prepared,
                input: intelligence::Request {
                    version: intelligence::VERSION,
                    client: "client-a".into(),
                    document: "file".into(),
                    request: 1,
                    revision: 1,
                    language: "notes".into(),
                    path: "file.notes".into(),
                    source: "notes".into(),
                    source_digest: source_digest("notes"),
                    query: intelligence::Query::Synchronize,
                },
            },
        };
        let mut closed = request.clone();
        closed.selection.input.query = intelligence::Query::Close;
        closed.selection.input.path = "deleted/parent/file.notes".into();
        let response = preparation_request(
            port,
            paired,
            "/plugins/languages/query",
            serde_json::to_value(&closed).unwrap(),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);
        let response: intelligence::Response = serde_json::from_str(&response.body).unwrap();
        response.validate(&closed.selection.input).unwrap();
        assert_eq!(response.server, "closed");
        let valid = preparation_request(
            port,
            paired,
            "/plugins/languages/query",
            serde_json::to_value(&request).unwrap(),
        )
        .await;
        assert_eq!(valid.status, 400, "{}", valid.body);
        assert!(
            valid.body.contains("Cannot start language server"),
            "{}",
            valid.body
        );
        // A failed startup's backoff also must not prevent closing its state.
        closed.selection.input.request = 2;
        let response = preparation_request(
            port,
            paired,
            "/plugins/languages/query",
            serde_json::to_value(&closed).unwrap(),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);
        let mut forged = request.clone();
        forged.selection.prepared.manifest.contributions.languages[0]
            .features
            .language_server
            .as_mut()
            .unwrap()
            .command = "forged-command-must-not-start".into();
        let rejected = preparation_request(
            port,
            paired,
            "/plugins/languages/query",
            serde_json::to_value(&forged).unwrap(),
        )
        .await;
        assert_eq!(rejected.status, 400, "{}", rejected.body);
        assert!(
            !rejected.body.contains("Cannot start language server"),
            "{}",
            rejected.body
        );
        let mut outside = request.clone();
        outside.selection.input.request = 2;
        outside.selection.input.path = "../outside.notes".into();
        let rejected = preparation_request(
            port,
            paired,
            "/plugins/languages/query",
            serde_json::to_value(&outside).unwrap(),
        )
        .await;
        assert_eq!(rejected.status, 400, "{}", rejected.body);
        assert!(
            !rejected.body.contains("Cannot start language server"),
            "{}",
            rejected.body
        );
        std::fs::create_dir(workspace.path().join("nested")).unwrap();
        std::fs::write(workspace.path().join("nested/project.json"), "{}").unwrap();
        let mut live = request.clone();
        live.selection.input.language = "live-notes".into();
        live.selection.input.path = "nested/file.notes".into();
        let response = preparation_request(
            port,
            paired,
            "/plugins/languages/query",
            serde_json::to_value(&live).unwrap(),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);
        let opened: intelligence::Response = serde_json::from_str(&response.body).unwrap();
        let calls_path = workspace.path().join("calls");
        let calls = std::fs::read_to_string(&calls_path).unwrap();
        let starts = calls.matches("\"method\": \"initialize\"").count();
        let closes = calls
            .matches("\"method\": \"textDocument/didClose\"")
            .count();
        std::fs::remove_dir_all(workspace.path().join("nested")).unwrap();
        live.selection.input.request += 1;
        live.selection.input.query = intelligence::Query::Close;
        let response = preparation_request(
            port,
            paired,
            "/plugins/languages/query",
            serde_json::to_value(&live).unwrap(),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);
        let closed: intelligence::Response = serde_json::from_str(&response.body).unwrap();
        closed.validate(&live.selection.input).unwrap();
        assert_eq!(closed.server, opened.server);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let calls = std::fs::read_to_string(&calls_path).unwrap();
                assert_eq!(calls.matches("\"method\": \"initialize\"").count(), starts);
                if calls
                    .matches("\"method\": \"textDocument/didClose\"")
                    .count()
                    == closes + 1
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        std::fs::create_dir(workspace.path().join("nested")).unwrap();
        std::fs::write(workspace.path().join("nested/project.json"), "{}").unwrap();
        live.selection.input.request += 1;
        live.selection.input.query = intelligence::Query::Synchronize;
        let response = preparation_request(
            port,
            paired,
            "/plugins/languages/query",
            serde_json::to_value(&live).unwrap(),
        )
        .await;
        assert_eq!(response.status, 400, "{}", response.body);
        assert!(
            response.body.contains("physical language workspace"),
            "{}",
            response.body
        );
        live.selection.input.request += 1;
        let response = preparation_request(
            port,
            paired,
            "/plugins/languages/query",
            serde_json::to_value(&live).unwrap(),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);
        let before_branch: intelligence::Response = serde_json::from_str(&response.body).unwrap();
        git(
            workspace.path(),
            &[
                "checkout",
                "-q",
                "-b",
                if paired { "paired" } else { "remote" },
            ],
        );
        live.selection.input.request += 1;
        let response = preparation_request(
            port,
            paired,
            "/plugins/languages/query",
            serde_json::to_value(&live).unwrap(),
        )
        .await;
        assert_eq!(response.status, 400, "{}", response.body);
        assert!(
            response.body.contains("Git revision changed"),
            "{}",
            response.body
        );
        live.selection.input.request += 1;
        let response = preparation_request(
            port,
            paired,
            "/plugins/languages/query",
            serde_json::to_value(&live).unwrap(),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);
        let fresh: intelligence::Response = serde_json::from_str(&response.body).unwrap();
        fresh.validate(&live.selection.input).unwrap();
        assert_ne!(fresh.server, before_branch.server);
        let starts = std::fs::read_to_string(&calls_path)
            .unwrap()
            .matches("\"method\": \"initialize\"")
            .count();
        live.selection.input.client = "client-git-handoff".into();
        live.selection.input.request += 1;
        let response = preparation_request(
            port,
            paired,
            "/plugins/languages/query",
            serde_json::to_value(&live).unwrap(),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);
        let handed_off: intelligence::Response = serde_json::from_str(&response.body).unwrap();
        assert_ne!(handed_off.server, fresh.server);
        assert_eq!(
            std::fs::read_to_string(&calls_path)
                .unwrap()
                .matches("\"method\": \"initialize\"")
                .count(),
            starts
        );
        let head = workspace.path().join(".git/HEAD");
        let modified = std::fs::metadata(&head).unwrap().modified().unwrap();
        git(
            workspace.path(),
            &[
                "checkout",
                "-q",
                "-b",
                if paired {
                    "paired-round-trip"
                } else {
                    "remote-round-trip"
                },
            ],
        );
        git(
            workspace.path(),
            &["checkout", "-q", if paired { "paired" } else { "remote" }],
        );
        std::fs::File::options()
            .write(true)
            .open(&head)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        live.selection.input.request += 1;
        let response = preparation_request(
            port,
            paired,
            "/plugins/languages/query",
            serde_json::to_value(&live).unwrap(),
        )
        .await;
        assert_eq!(response.status, 400, "{}", response.body);
        assert!(
            response.body.contains("Git revision changed"),
            "{}",
            response.body
        );
        live.selection.input.request += 1;
        let response = preparation_request(
            port,
            paired,
            "/plugins/languages/query",
            serde_json::to_value(&live).unwrap(),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);
        let after_round_trip: intelligence::Response =
            serde_json::from_str(&response.body).unwrap();
        after_round_trip.validate(&live.selection.input).unwrap();
        assert_ne!(after_round_trip.server, handed_off.server);
        std::fs::write(
            workspace.path().join("settings.json"),
            "changed configuration",
        )
        .unwrap();
        live.selection.input.request += 1;
        let response = preparation_request(
            port,
            paired,
            "/plugins/languages/query",
            serde_json::to_value(&live).unwrap(),
        )
        .await;
        assert_eq!(response.status, 400, "{}", response.body);
        assert!(
            response.body.contains("Declared project inputs changed"),
            "{}",
            response.body
        );
        live.selection.input.request += 1;
        let response = preparation_request(
            port,
            paired,
            "/plugins/languages/query",
            serde_json::to_value(&live).unwrap(),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);
        let reconfigured: intelligence::Response = serde_json::from_str(&response.body).unwrap();
        reconfigured.validate(&live.selection.input).unwrap();
        assert_ne!(fresh.server, reconfigured.server);
        assert_eq!(live.selection.input.source, "notes");
        #[cfg(unix)]
        {
            let replacement = workspace.path().join("replacement-server");
            std::fs::copy(&lifecycle_peer, &replacement).unwrap();
            std::fs::rename(replacement, &lifecycle_peer).unwrap();
            live.selection.input.request += 1;
            let response = preparation_request(
                port,
                paired,
                "/plugins/languages/query",
                serde_json::to_value(&live).unwrap(),
            )
            .await;
            assert_eq!(response.status, 400, "{}", response.body);
            assert!(
                response.body.contains("executable changed"),
                "{}",
                response.body
            );
            live.selection.input.request += 1;
            let response = preparation_request(
                port,
                paired,
                "/plugins/languages/query",
                serde_json::to_value(&live).unwrap(),
            )
            .await;
            assert_eq!(response.status, 200, "{}", response.body);
            let reopened: intelligence::Response = serde_json::from_str(&response.body).unwrap();
            reopened.validate(&live.selection.input).unwrap();
            assert_ne!(reopened.server, reconfigured.server);
        }
        std::fs::remove_file(workspace.path().join("settings.json")).unwrap();
        std::fs::remove_dir_all(workspace.path().join("nested")).unwrap();
        let revoked=preparation_request(port,paired,"/plugins/languages/revoke",serde_json::json!({"user":42,"project":1,"provider":request.selection.prepared.storage_namespace()})).await;
        assert_eq!(revoked.status, 200, "{}", revoked.body);
        if !paired {
            let mut other = request.clone();
            other.selection.user = 43;
            let rejected = preparation_request(
                port,
                false,
                "/plugins/languages/query",
                serde_json::to_value(other).unwrap(),
            )
            .await;
            assert_eq!(rejected.status, 401);
        }
    }
    for path in [
        "/plugins/languages/query",
        "/plugins/languages/revoke",
        "/plugins/languages/command",
    ] {
        let raw = format!(
            "POST {path} HTTP/1.1\r\nHost: localhost:{port}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
        );
        assert_eq!(HttpResponse::parse(&http(port, &raw).await).status, 401);
    }
    server.abort();
}

#[tokio::test]
async fn local_pairing_and_remote_backend_install_the_same_package_without_workspace_writes() {
    let cache = tempfile::tempdir().unwrap();
    let fixture = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    for file in review_files() {
        let path = fixture.path().join("plugins/pr-review").join(file.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, file.content).unwrap();
    }
    git(fixture.path(), &["init", "-q"]);
    git(fixture.path(), &["add", "."]);
    git(
        fixture.path(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.org",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    let mut source = source();
    source.commit = git(fixture.path(), &["rev-parse", "HEAD"]);
    for owner in ["paired", "user:42"] {
        let repo = cache
            .path()
            .join(content_digest(owner.as_bytes()))
            .join("repositories")
            .join(content_digest(source.repository.as_bytes()));
        std::fs::create_dir_all(repo.parent().unwrap()).unwrap();
        git(
            cache.path(),
            &[
                "clone",
                "--bare",
                fixture.path().to_str().unwrap(),
                repo.to_str().unwrap(),
            ],
        );
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut config = ServerConfig::new(
        workspace.path().into(),
        "dummy_secret".into(),
        Some("paired-token".into()),
    );
    config.plugins = NativePluginInstaller::new(Some(cache.path().into()));
    let server = tokio::spawn(run_server(listener, config));
    let body = serde_json::json!({"source":source,"user":42}).to_string();
    let remote = post(
        port,
        "/plugins/prepare",
        &body,
        &[("Content-Type", "application/json")],
    )
    .await;
    assert_eq!(remote.status, 200, "{}", remote.body);
    let remote: PreparedPlugin = serde_json::from_str(&remote.body).unwrap();
    let signed_body = serde_json::json!({"source":source,"user":999}).to_string();
    let signed = post(
        port,
        "/plugins/prepare",
        &signed_body,
        &[
            ("Origin", "http://localhost:3000"),
            ("Content-Type", "application/json"),
        ],
    )
    .await;
    assert_eq!(signed.status, 200, "{}", signed.body);
    let local_body = serde_json::json!({"source":source,"user":999}).to_string();
    let raw = format!(
        "POST /plugins/prepare HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:3000\r\nAuthorization: Bearer paired-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{local_body}",
        local_body.len()
    );
    let local = HttpResponse::parse(&http(port, &raw).await);
    assert_eq!(local.status, 200, "{}", local.body);
    assert_eq!(
        local.header("Access-Control-Allow-Origin"),
        Some("http://localhost:3000")
    );
    let local: PreparedPlugin = serde_json::from_str(&local.body).unwrap();
    assert_eq!(local, remote);
    let remote_package = post(
        port,
        "/plugins/package",
        &serde_json::json!({"prepared":remote,"user":42}).to_string(),
        &[("Content-Type", "application/json")],
    )
    .await;
    assert_eq!(remote_package.status, 200, "{}", remote_package.body);
    let body = serde_json::json!({"prepared":local,"user":999}).to_string();
    let raw = format!(
        "POST /plugins/package HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:3000\r\nAuthorization: Bearer paired-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let local_package = HttpResponse::parse(&http(port, &raw).await);
    assert_eq!(local_package.status, 200, "{}", local_package.body);
    let remote_package: openwebide_core::plugins::PluginPackage =
        serde_json::from_str(&remote_package.body).unwrap();
    let local_package: openwebide_core::plugins::PluginPackage =
        serde_json::from_str(&local_package.body).unwrap();
    assert_eq!(local_package, remote_package);
    assert!(!local_package.skills[0].resources.is_empty());

    // Source preparation uses short authenticated requests on both transports.
    let mut preparation_ids = Vec::new();
    for paired in [false, true] {
        let started = preparation_request(
            port,
            paired,
            "/plugins/prepare/start",
            serde_json::json!({"source":source,"user":42}),
        )
        .await;
        assert_eq!(started.status, 200, "{}", started.body);
        let started: PluginPreparation = serde_json::from_str(&started.body).unwrap();
        assert_eq!(started.state, PreparationState::Queued);
        let ready = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let response = preparation_request(
                    port,
                    paired,
                    "/plugins/prepare/status",
                    serde_json::json!({"id":started.id,"user":42}),
                )
                .await;
                assert_eq!(response.status, 200, "{}", response.body);
                let status: PluginPreparation = serde_json::from_str(&response.body).unwrap();
                match status.state {
                    PreparationState::Queued | PreparationState::Preparing => {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                    PreparationState::Ready => break status,
                    _ => panic!("Preparation failed: {status:?}"),
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(ready.prepared.as_ref().unwrap(), &remote);
        for path in ["/plugins/prepare/status", "/plugins/prepare/cancel"] {
            let response = preparation_request(
                port,
                !paired,
                path,
                serde_json::json!({"id":started.id,"user":42}),
            )
            .await;
            assert_ne!(
                response.status, 200,
                "A preparation must not cross authenticated owners"
            );
        }
        let complete = preparation_request(
            port,
            paired,
            "/plugins/prepare/cancel",
            serde_json::json!({"id":started.id,"user":42}),
        )
        .await;
        assert_eq!(complete.status, 200);
        let complete: PluginPreparation = serde_json::from_str(&complete.body).unwrap();
        assert_eq!(complete.state, PreparationState::Ready);
        preparation_ids.push(started.id);
    }
    assert_ne!(preparation_ids[0], preparation_ids[1]);
    for path in [
        "/plugins/prepare/start",
        "/plugins/prepare/status",
        "/plugins/prepare/cancel",
    ] {
        let unauth = format!(
            "POST {path} HTTP/1.1\r\nHost: localhost:{port}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        assert_eq!(HttpResponse::parse(&http(port, &unauth).await).status, 401);
    }

    // Browser-supplied user IDs cannot select another cache namespace.
    assert!(!cache.path().join(content_digest(b"user:999")).exists());
    assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
    let missing_user = post(
        port,
        "/plugins/prepare",
        &serde_json::json!({"source":source}).to_string(),
        &[("Content-Type", "application/json")],
    )
    .await;
    assert_eq!(missing_user.status, 400);
    let evil = post(
        port,
        "/plugins/prepare",
        &body,
        &[
            ("Origin", "http://evil.example"),
            ("Content-Type", "application/json"),
        ],
    )
    .await;
    assert_eq!(evil.status, 403);
    let unauth = format!(
        "POST /plugins/prepare HTTP/1.1\r\nHost: localhost:{port}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    assert_eq!(HttpResponse::parse(&http(port, &unauth).await).status, 401);
    server.abort();
}
