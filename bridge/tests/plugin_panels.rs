//! Real first-party WASI views/actions over both authenticated bridge transports.
//! HTTP is a controlled GitHub-shaped primitive fixture; records and claims use SQLite.
mod common;
use common::{HttpResponse, http, post};
use openwebide_agent::plugins::{
    actions::PluginActionHost,
    execution::{GrantedHost, PluginTransport},
    panels::{PanelActionHost, invoke_panel},
};
use openwebide_bridge::{ServerConfig, plugins::NativePluginInstaller, run_server};
use openwebide_core::{
    NewProject, UserId, UserRole, WorkspaceMode,
    plugins::{
        self, PluginManifest, PluginPackage, PreparedPlugin, RecordPlugin,
        execution::*,
        panels::{coordination::*, *},
    },
};
use openwebide_plugin_runtime::Runtime;
use openwebide_storage::{Store, rusqlite_db::RusqliteDb};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Clone)]
struct Host {
    port: u16,
    paired: bool,
    user: UserId,
    project: i64,
    store: Arc<Store<RusqliteDb>>,
    grants: Arc<Mutex<String>>,
    sequence: Arc<AtomicU64>,
}
impl Host {
    async fn rpc<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        mut payload: Value,
    ) -> Result<T, String> {
        payload["user"] = json!(self.user.get());
        let body = payload.to_string();
        let response = if self.paired {
            HttpResponse::parse(&http(self.port, &format!("POST {path} HTTP/1.1\r\nHost: localhost:{}\r\nOrigin: http://localhost:3000\r\nAuthorization: Bearer paired-panel-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", self.port, body.len())).await)
        } else {
            post(
                self.port,
                path,
                &body,
                &[("Content-Type", "application/json")],
            )
            .await
        };
        if response.status != 200 {
            return Err(format!("{}: {}", response.status, response.body));
        }
        serde_json::from_str(&response.body).map_err(|error| error.to_string())
    }
}
impl PluginActionHost for Host {
    async fn prepare(&self, plugin: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        // This contract starts from a validated installed artifact. Cold source
        // compilation has a separate installer contract.
        Ok(plugin.clone())
    }
    async fn grant(&self, plugin: &PreparedPlugin) -> Result<String, String> {
        let token = format!("{:032x}", self.sequence.fetch_add(1, Ordering::Relaxed) + 1);
        // The SQLite fixture has synchronous primitives; avoid the compiler
        // RPIT/GAT Send limitation for its opaque transaction future.
        futures::executor::block_on(self.store.issue_plugin_context_grant(
            self.user,
            &PluginExecutionContext {
                user_action: true,
                project_id: Some(self.project),
                session_id: None,
                primary: None,
            },
            plugin,
            &token,
            10,
        ))
        .map_err(|error| error.to_string())?;
        *self.grants.lock().unwrap() = token.clone();
        Ok(token)
    }
    fn is_current(&self) -> bool {
        true
    }
}
impl PluginTransport for Host {
    async fn ensure_prepared(&self, plugin: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        Ok(plugin.clone())
    }
    async fn start(&self, call: InvokePlugin) -> Result<PluginInvocation, String> {
        let grant = self.grants.lock().unwrap().clone();
        self.store
            .authorize_plugin_invocation(
                self.user,
                &PluginStartRequest {
                    grant,
                    session_id: None,
                    call: call.clone(),
                },
                10,
            )
            .await
            .map_err(|error| error.to_string())?;
        self.rpc("/plugins/invoke", json!({"call":call})).await
    }
    async fn resume(&self, continuation: ContinuePlugin) -> Result<PluginInvocation, String> {
        self.rpc("/plugins/continue", json!({"continuation":continuation}))
            .await
    }
    fn cancel(&self, id: String) {
        let host = self.clone();
        tokio::spawn(async move {
            let _ = host.rpc::<Value>("/plugins/cancel", json!({"id":id})).await;
        });
    }
}
impl PanelActionHost for Host {
    async fn begin_panel(
        &self,
        prepared: &PreparedPlugin,
        input: &PanelInput,
        approved: bool,
    ) -> Result<PanelClaim, String> {
        futures::executor::block_on(self.store.begin_plugin_panel(
            self.user,
            &BeginPanel {
                project: Some(self.project),
                prepared: prepared.clone(),
                input: input.clone(),
                approved,
            },
            10,
        ))
        .map_err(|error| error.to_string())
    }
    async fn finish_panel(&self, finish: &FinishPanel) -> Result<(), String> {
        futures::executor::block_on(self.store.finish_plugin_panel(self.user, finish, 10))
            .map_err(|error| error.to_string())
    }
    async fn close_panel(&self, instance: &str) -> Result<(), String> {
        futures::executor::block_on(self.store.close_plugin_panel(self.user, instance, 10))
            .map_err(|error| error.to_string())
    }
}
impl GrantedHost for Host {
    async fn request(&self, request: &PluginHostRequest) -> Result<String, String> {
        self.store
            .authorize_plugin_context(self.user, request, 10)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            request.capability, "records",
            "Native HTTP never delegates to this fixture callback"
        );
        self.store
            .plugin_context_host_request(self.user, request, 10)
            .await
            .map_err(|error| error.to_string())
    }
}
async fn github_fixture(posts: Arc<Mutex<Vec<Value>>>) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let posts = posts.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let end = loop {
                    assert!(socket.read_buf(&mut bytes).await.unwrap() > 0);
                    if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                        break end + 4;
                    }
                    assert!(bytes.len() < 65536);
                };
                let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
                let mut lines = headers.lines();
                let request = lines.next().unwrap().split_whitespace().collect::<Vec<_>>();
                let headers = lines
                    .filter_map(|line| line.split_once(':'))
                    .map(|(key, value)| (key.to_ascii_lowercase(), value.trim().to_owned()))
                    .collect::<std::collections::BTreeMap<_, _>>();
                assert!(request[1].starts_with("/api/repos/owner/repo/"));
                assert_eq!(headers["authorization"], "Bearer fixture-private-token");
                assert_eq!(
                    headers[openwebide_core::plugins::execution::PLUGIN_HTTP_HEADER],
                    "1"
                );
                let length = headers
                    .get("content-length")
                    .map_or(0, |length| length.parse::<usize>().unwrap());
                while bytes.len() < end + length {
                    assert!(socket.read_buf(&mut bytes).await.unwrap() > 0);
                }
                let body = if request[0] == "POST" {
                    posts
                        .lock()
                        .unwrap()
                        .push(serde_json::from_slice(&bytes[end..end + length]).unwrap());
                    "{}".into()
                } else {
                    assert_eq!(request[0], "GET");
                    if headers["accept"] == "application/vnd.github.diff" {
                        "diff --git a/file b/file\n+new\n".into()
                    } else if request[1].ends_with("reviews?per_page=30") {
                        "[]".into()
                    } else if request[1].ends_with("pulls/42") {
                        json!({"number":42,"title":"A change","body":"Review this","state":"open","head":{"sha":"a".repeat(40)},"html_url":"https://github.com/owner/repo/pull/42"}).to_string()
                    } else {
                        json!([{"number":42,"title":"A change","user":{"login":"contributor"}}])
                            .to_string()
                    }
                };
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            });
        }
    });
    (format!("http://127.0.0.1:{port}/api"), server)
}

fn installed_artifact(cache: &Path, owner: &str, prepared: &PreparedPlugin, bytes: &[u8]) {
    let source = &prepared.source;
    let root = cache.join(plugins::content_digest(owner.as_bytes()));
    let snapshot = root
        .join("versions")
        .join(plugins::content_digest(
            format!("{}\n{}", source.repository, source.path).as_bytes(),
        ))
        .join(&source.commit)
        .join(&prepared.digest);
    std::fs::create_dir_all(&snapshot).unwrap();
    std::fs::write(
        snapshot.join("plugin.json"),
        serde_json::to_vec(&prepared.manifest).unwrap(),
    )
    .unwrap();
    let key = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        source.repository,
        source.path,
        source.commit,
        prepared.digest,
        openwebide_plugin_runtime::build::TOOLCHAIN,
        openwebide_plugin_runtime::build::sdk_digest()
    );
    let artifact = root
        .join("artifacts")
        .join(plugins::content_digest(key.as_bytes()));
    std::fs::create_dir_all(&artifact).unwrap();
    std::fs::write(artifact.join("plugin.wasm"), bytes).unwrap();
    std::fs::write(artifact.join("digest"), plugins::content_digest(bytes)).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires the compiled marketplace PR Review component and manifest"]
async fn first_party_panels_publish_once_through_real_paired_and_backend_bridge_adapters() {
    let bytes =
        std::fs::read(std::env::var("OPENWEBIDE_PR_REVIEW_COMPONENT").expect("component path"))
            .unwrap();
    let path = std::path::PathBuf::from(
        std::env::var("OPENWEBIDE_PR_REVIEW_MANIFEST").expect("manifest path"),
    );
    let manifest: PluginManifest = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    Runtime::new()
        .unwrap()
        .validate_exports(&bytes, &manifest)
        .unwrap();
    let skills = manifest
        .contributions
        .skills
        .iter()
        .map(|skill| {
            openwebide_core::skills::archive::import_markdown(
                &std::fs::read_to_string(path.parent().unwrap().join(&skill.path)).unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(RusqliteDb::open_in_memory().unwrap()));
        store.migrate().await.unwrap();
        let user = store
            .insert_user("owner", "hash", UserRole::Admin, 0)
            .await
            .unwrap()
            .id;
        let project = store
            .create_project(
                &NewProject {
                    name: "p".into(),
                    mode,
                    path: Some("p".into()),
                },
                user,
                0,
            )
            .await
            .unwrap()
            .id;
        let mut prepared = plugins::testing::receipt();
        prepared.manifest = manifest.clone();
        prepared.host_id = "panel-contract-host".into();
        let owner = if mode == WorkspaceMode::Local {
            "paired".into()
        } else {
            format!("user:{}", user.get())
        };
        installed_artifact(cache.path(), &owner, &prepared, &bytes);
        store
            .record_plugin(
                user,
                &RecordPlugin {
                    prepared: prepared.clone(),
                    package: Some(Box::new(PluginPackage {
                        prepared: prepared.clone(),
                        skills: skills.clone(),
                    })),
                    revision: None,
                    update_policy: None,
                    approved_capabilities: vec!["http".into(), "records".into()],
                },
                0,
            )
            .await
            .unwrap();
        let bindings = store.project_plugins(user, project).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut config = ServerConfig::new(
            workspace.path().into(),
            "dummy_secret".into(),
            Some("paired-panel-token".into()),
        );
        config.plugins = NativePluginInstaller::new(Some(cache.path().into()));
        let server = tokio::spawn(run_server(listener, config));
        let posts = Arc::new(Mutex::new(Vec::new()));
        let (api, github) = github_fixture(posts.clone()).await;
        let host = Host {
            port,
            paired: mode == WorkspaceMode::Local,
            user,
            project,
            store: store.clone(),
            grants: Arc::default(),
            sequence: Arc::default(),
        };
        let namespace = prepared.storage_namespace();
        let mut input = PanelInput {
            panel: "pull-requests".into(),
            instance: "a".repeat(32),
            request: "b".repeat(32),
            revision: 0,
            handler: "render".into(),
            action: None,
            values: Default::default(),
            state: Value::Null,
        };
        let configuration = invoke_panel(
            &host,
            &bindings,
            &namespace,
            &prepared.digest,
            &input,
            false,
        )
        .await
        .unwrap();
        input.revision = configuration.revision;
        input.state = configuration.state;
        input.request = "c".repeat(32);
        input.handler = "configure".into();
        input.action = Some("configure".into());
        input.values = [
            ("api".into(), json!(api)),
            ("repository".into(), json!("owner/repo")),
            ("token".into(), json!("fixture-private-token")),
        ]
        .into();
        let listing = invoke_panel(
            &host,
            &bindings,
            &namespace,
            &prepared.digest,
            &input,
            false,
        )
        .await
        .unwrap();
        assert!(listing.blocks.iter().any(|block| matches!(block, Block::Actions {buttons} if buttons.iter().any(|button|button.action=="open"))), "Configured listing must offer its declared open action: {listing:?}");
        input.revision = listing.revision;
        input.state = listing.state;
        input.request = "d".repeat(32);
        input.handler = "open".into();
        input.action = Some("open".into());
        input.values = [("number".into(), json!(42))].into();
        let detail = invoke_panel(
            &host,
            &bindings,
            &namespace,
            &prepared.digest,
            &input,
            false,
        )
        .await
        .unwrap();
        let submit = detail
            .blocks
            .iter()
            .find_map(|block| match block {
                Block::Form { id, submit, .. } if id == "review" => Some(submit),
                _ => None,
            })
            .unwrap();
        input.revision = detail.revision;
        input.state = detail.state.clone();
        input.request = "e".repeat(32);
        input.handler = "publish".into();
        input.action = Some("publish".into());
        input.values = submit.values.clone();
        input.values.insert("event".into(), json!("COMMENT"));
        input.values.insert("body".into(), json!("Looks good"));
        assert!(
            invoke_panel(
                &host,
                &bindings,
                &namespace,
                &prepared.digest,
                &input,
                false
            )
            .await
            .is_err()
        );
        assert!(posts.lock().unwrap().is_empty());
        let published = invoke_panel(&host, &bindings, &namespace, &prepared.digest, &input, true)
            .await
            .unwrap();
        assert_eq!(
            invoke_panel(&host, &bindings, &namespace, &prepared.digest, &input, true)
                .await
                .unwrap(),
            published
        );
        assert_eq!(
            posts.lock().unwrap().as_slice(),
            &[json!({"event":"COMMENT","body":"Looks good","commit_id":"a".repeat(40)})]
        );
        assert!(
            !serde_json::to_string(&published)
                .unwrap()
                .contains("fixture-private-token")
        );
        host.close_panel(&input.instance).await.unwrap();
        assert!(
            invoke_panel(&host, &bindings, &namespace, &prepared.digest, &input, true)
                .await
                .is_err()
        );
        server.abort();
        github.abort();
    }
}
