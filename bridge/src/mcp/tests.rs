use super::*;
use openwebide_core::ToolCall;
use std::{path::PathBuf, sync::Arc};

const SERVER: &str = r#"
import sys,json,os,time
for line in sys.stdin:
    request=json.loads(line)
    if 'id' not in request: continue
    method=request['method']
    if method=='server/discover':
        print(json.dumps({'jsonrpc':'2.0','id':request['id'],'error':{'code':-32601,'message':'Method not found'}}),flush=True)
        continue
    if method=='initialize': result={'protocolVersion':'2025-11-25','capabilities':{'tools':{}}}
    elif method=='tools/list': result={'tools':[{'name':'echo','description':'Echo input','inputSchema':{'type':'object','properties':{'text':{'type':'string'}}}}]}
    elif method=='tools/call':
        value=request['params']['arguments'].get('text','')
        if value=='hang': time.sleep(300)
        if value=='exit': sys.exit(2)
        result={'content':[{'type':'text','text':value+':'+os.environ.get('MCP_TEST_VALUE','')+':'+os.getcwd()}],'isError':value=='error'}
    else: result={}
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
"#;
pub(crate) fn fixture() -> (tempfile::TempDir, host::Host) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".openwebide")).unwrap();
    let script = dir.path().join("server.py");
    std::fs::write(&script, SERVER).unwrap();
    let command = if cfg!(windows) { "python" } else { "python3" };
    std::fs::write(dir.path().join(".openwebide/mcp.json"),json!({"mcpServers":{"fixture":{"command":command,"args":[script],"env":{"MCP_TEST_VALUE":"configured"}}}}).to_string()).unwrap();
    (dir, host::Host::new(None))
}
fn call(catalog: &openwebide_core::mcp::Catalog, text: &str) -> ToolCall {
    ToolCall {
        id: "call-1".into(),
        name: catalog.tools[0].definition.name.clone(),
        arguments: json!({"text":text}).to_string(),
    }
}
#[tokio::test]
async fn native_mcp_discovery_call_failures_restart_and_owner_isolation() {
    let (dir, host) = fixture();
    for owner in ["user:7", "paired"] {
        let catalog = host
            .open(owner.into(), dir.path().to_path_buf(), true)
            .await
            .unwrap();
        assert!(catalog.failures.is_empty(), "{:?}", catalog.failures);
        assert_eq!(catalog.tools[0].definition.name, "mcp__fixture__echo");
        let success = host
            .invoke(owner, &catalog, &call(&catalog, "hello"))
            .await
            .unwrap();
        assert!(success.ok);
        assert!(success.content.contains("hello:configured:"));
        assert!(
            success
                .content
                .contains(&dir.path().to_string_lossy().to_string())
        );
        assert!(
            !host
                .invoke(owner, &catalog, &call(&catalog, "error"))
                .await
                .unwrap()
                .ok
        );
        assert!(
            host.invoke("other-user", &catalog, &call(&catalog, "hello"))
                .await
                .is_err()
        );
        let mut changed = catalog.clone();
        changed.tools[0].name = "retarget".into();
        assert!(
            host.invoke(owner, &changed, &call(&catalog, "hello"))
                .await
                .is_err()
        );
        assert!(
            host.invoke(owner, &catalog, &call(&catalog, "exit"))
                .await
                .is_err()
        );
        assert!(
            host.invoke(owner, &catalog, &call(&catalog, "after-restart"))
                .await
                .unwrap()
                .ok
        );
        host.close(owner, &catalog.id).await.unwrap();
        assert!(
            host.invoke(owner, &catalog, &call(&catalog, "hello"))
                .await
                .is_err()
        );
    }
}
#[tokio::test]
async fn closing_a_catalog_cancels_an_active_call_and_dropping_transport_closes_it() {
    let (dir, host) = fixture();
    let catalog = host
        .open("paired".into(), dir.path().to_path_buf(), true)
        .await
        .unwrap();
    let h = host.clone();
    let c = catalog.clone();
    let running = tokio::spawn(async move { h.invoke("paired", &c, &call(&c, "hang")).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    host.close("paired", &catalog.id).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), running)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    let catalog = host
        .open("paired".into(), dir.path().to_path_buf(), true)
        .await
        .unwrap();
    let transport = host.transport("paired".into(), catalog.id.clone());
    drop(transport);
    tokio::task::yield_now().await;
    assert!(
        host.invoke("paired", &catalog, &call(&catalog, "hello"))
            .await
            .is_err()
    );
}
#[tokio::test]
async fn empty_configuration_does_not_spawn_and_missing_executable_keeps_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let host = host::Host::new(None);
    assert!(
        host.open("paired".into(), dir.path().into(), true)
            .await
            .unwrap()
            .tools
            .is_empty()
    );
    std::fs::create_dir(dir.path().join(".openwebide")).unwrap();
    std::fs::write(
        dir.path().join(".openwebide/mcp.json"),
        json!({"mcpServers":{"missing":{"command":PathBuf::from("/does-not-exist/mcp")}}})
            .to_string(),
    )
    .unwrap();
    let result = host
        .open("paired".into(), dir.path().into(), true)
        .await
        .unwrap();
    assert!(result.tools.is_empty());
    assert_eq!(result.failures.len(), 1);
}

#[tokio::test]
async fn modern_mcp_metadata_and_no_legacy_handshake() {
    let (dir, host) = fixture();
    let script=SERVER.replace("if method=='server/discover':\n        print(json.dumps({'jsonrpc':'2.0','id':request['id'],'error':{'code':-32601,'message':'Method not found'}}),flush=True)\n        continue", "if method=='server/discover':\n        result={'supportedVersions':['2026-07-28'],'capabilities':{'tools':{}},'resultType':'complete'}\n        print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)\n        continue").replace("    if method=='initialize': result=", "    assert request['params']['_meta']['io.modelcontextprotocol/protocolVersion']=='2026-07-28'\n    assert request['params']['_meta']['io.modelcontextprotocol/clientCapabilities']=={}\n    assert method!='initialize'\n    if method=='initialize': result=");
    std::fs::write(dir.path().join("server.py"), script).unwrap();
    let catalog = host
        .open("paired".into(), dir.path().into(), true)
        .await
        .unwrap();
    assert!(catalog.failures.is_empty(), "{:?}", catalog.failures);
    assert!(
        host.invoke("paired", &catalog, &call(&catalog, "modern"))
            .await
            .unwrap()
            .ok
    );
    host.close("paired", &catalog.id).await.unwrap();
}

#[tokio::test]
async fn mcp_http_contract_on_server_and_paired_hosts() {
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    for paired in [false, true] {
        let (dir, mcp) = fixture();
        let script = SERVER.replace(
            "Echo input",
            &format!("Echo input {}", "details ".repeat(1500)),
        );
        std::fs::write(dir.path().join("server.py"), script).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let secret: Arc<str> = "this_is_a_very_long_dummy_secret_32_bytes_min".into();
        let mut config = crate::ServerConfig::new(
            dir.path().into(),
            secret.clone(),
            paired.then(|| "paired-token".into()),
        );
        config.mcp = mcp.clone();
        config.backend_url = "http://127.0.0.1:9/api".into();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(crate::run_server_until(listener, config, async {
            let _ = stopped.await;
        }));
        let rpc = |operation: &str, body: Value, valid: bool| {
            let uri = format!("http://127.0.0.1:{port}/mcp/{operation}");
            let token = if !valid {
                "invalid".to_owned()
            } else if paired {
                "paired-token".to_owned()
            } else {
                secret.to_string()
            };
            async move {
                let mut request = hyper::Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("Content-Type", "application/json")
                    .header("Authorization", format!("Bearer {token}"));
                if paired {
                    request = request.header("Origin", "http://localhost:3000");
                }
                let response = crate::runs::http_client::ReqwestHttpClient::default()
                    .send(
                        request
                            .body(Full::new(Bytes::from(body.to_string())))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = response.status().as_u16();
                let bytes = response.into_body().collect().await.unwrap().to_bytes();
                (status, serde_json::from_slice::<Value>(&bytes).unwrap())
            }
        };
        assert_eq!(rpc("open", json!({"user":7,"cwd":"."}), false).await.0, 401);
        let (status, global) = rpc("open", json!({"user":7}), true).await;
        assert_eq!(status, 200);
        assert!(global["tools"].as_array().unwrap().is_empty());
        let (status, value) = rpc("open", json!({"user":7,"cwd":"."}), true).await;
        assert_eq!(status, 200, "{value}");
        let catalog: openwebide_core::mcp::Catalog = serde_json::from_value(value).unwrap();
        assert!(catalog.failures.is_empty(), "{:?}", catalog.failures);
        let transport = mcp.transport(
            if paired {
                "paired".into()
            } else {
                "user:7".into()
            },
            catalog.id.clone(),
        );
        crate::runs::assert_deferred_mcp_run(transport.clone(), catalog.clone()).await;
        let result = rpc(
            "call",
            json!({"user":7,"catalog":catalog,"call":call(&catalog,"transport")}),
            true,
        )
        .await;
        assert_eq!(result.0, 200, "{:?}", result.1);
        assert_eq!(result.1["ok"], true);
        assert!(
            result.1["content"]
                .as_str()
                .unwrap()
                .contains("transport:configured:")
        );
        if !paired {
            assert_ne!(
                rpc(
                    "call",
                    json!({"user":8,"catalog":catalog,"call":call(&catalog,"other")}),
                    true
                )
                .await
                .0,
                200
            );
        }
        assert_eq!(
            rpc("close", json!({"user":7,"id":catalog.id}), true)
                .await
                .0,
            200
        );
        assert_ne!(
            rpc(
                "call",
                json!({"user":7,"catalog":catalog,"call":call(&catalog,"after-close")}),
                true
            )
            .await
            .0,
            200
        );
        stop.send(()).unwrap();
        server.await.unwrap();
    }
}

#[tokio::test]
async fn restarting_a_server_rejects_changed_tool_definitions_without_replaying() {
    let (dir, host) = fixture();
    let catalog = host
        .open("paired".into(), dir.path().into(), true)
        .await
        .unwrap();
    assert!(
        host.invoke("paired", &catalog, &call(&catalog, "exit"))
            .await
            .is_err()
    );
    std::fs::write(
        dir.path().join("server.py"),
        SERVER.replace("'name':'echo'", "'name':'changed'"),
    )
    .unwrap();
    let error = host
        .invoke("paired", &catalog, &call(&catalog, "must-not-execute"))
        .await
        .unwrap_err();
    assert_eq!(error, "MCP tools changed after restart; start a new run");
    host.close("paired", &catalog.id).await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn closing_a_catalog_terminates_descendants_that_ignore_sigterm() {
    let (dir, host) = fixture();
    let server = SERVER.replace("import sys,json,os,time", "import sys,json,os,time,subprocess\nsubprocess.Popen([sys.executable, '-c', \"import os,signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);open('child.pid','w').write(str(os.getpid()));time.sleep(300)\"])");
    std::fs::write(dir.path().join("server.py"), server).unwrap();
    let catalog = host
        .open("paired".into(), dir.path().into(), true)
        .await
        .unwrap();
    assert!(!catalog.tools.is_empty());
    let pid = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(value) = std::fs::read_to_string(dir.path().join("child.pid"))
                && let Ok(pid) = value.parse::<i32>()
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    host.close("paired", &catalog.id).await.unwrap();
    let exited = tokio::time::timeout(Duration::from_secs(5), async {
        // SAFETY: signal zero only checks this fixture-owned process identity.
        while unsafe { libc::kill(pid, 0) } == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok();
    if !exited {
        // SAFETY: kill only the child PID recorded by this task-owned fixture.
        let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    assert!(exited, "MCP descendant survived catalog cleanup");
}

#[tokio::test]
async fn project_configuration_requires_an_explicit_host_folder() {
    let (dir, host) = fixture();
    let global_only = host
        .open("paired".into(), dir.path().into(), false)
        .await
        .unwrap();
    assert!(global_only.tools.is_empty());
    let host = host::Host::new(Some(dir.path().join(".openwebide/mcp.json")));
    let catalog = host
        .open("paired".into(), dir.path().into(), false)
        .await
        .unwrap();
    assert_eq!(catalog.tools[0].name, "echo");
    host.close("paired", &catalog.id).await.unwrap();
}
