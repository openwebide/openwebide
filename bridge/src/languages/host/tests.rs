use super::super::{framing, rpc};
use super::*;
use tokio::io::BufReader;

fn key(owner: &str, project: i64) -> Key {
    Key {
        owner: owner.into(),
        project,
        root: PathBuf::from("project"),
        workspace: PathBuf::from("project"),
        provider: "provider".into(),
        digest: "a".repeat(64),
        server: "configuration".into(),
        client: "client".into(),
    }
}

#[tokio::test]
async fn virtual_documents_use_declared_requests_and_source_bound_read_only_resources() {
    use openwebide_core::plugins::languages::source_digest;
    for (parameters, pointer, mode) in [
        ("text_document", "/body~1text/~0value", "nested"),
        ("uri", "", "root"),
        ("uri", "/source", "missing"),
        ("uri", "", "wrong-type"),
        ("uri", "", "oversized"),
        ("uri", "", "undeclared"),
        ("uri", "", "oversized-uri"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let source = "// 文😀\r\nreference\r\n";
        std::fs::write(root.path().join("file.notes"), source).unwrap();
        std::fs::write(root.path().join("other.notes"), source).unwrap();
        let script = root.path().join("fixture.py");
        std::fs::write(&script, r#"import sys,json
shape,mode,limit=sys.argv[1],sys.argv[2],int(sys.argv[3])
uri=('unregistered' if mode=='undeclared' else 'notes')+':/external/Library.notes?server-token=opaque'
if mode=='oversized-uri':uri+='x'*4096
text='// 文😀\r\nread only library\r\n'
def send(value):
    body=json.dumps({'jsonrpc':'2.0',**value},ensure_ascii=False).encode()
    sys.stdout.buffer.write(('Content-Length: %d\r\n\r\n'%len(body)).encode()+body)
    sys.stdout.buffer.flush()
while True:
    headers={}
    while True:
        line=sys.stdin.buffer.readline()
        if not line:sys.exit(0)
        if line==b'\r\n':break
        key,value=line.decode().split(':',1);headers[key.lower()]=value.strip()
    message=json.loads(sys.stdin.buffer.read(int(headers['content-length'])))
    method=message.get('method');params=message.get('params') or {};result=None
    with open('calls','a') as calls:calls.write(str(method)+'\n')
    if method=='initialize':result={'capabilities':{'textDocumentSync':1,'definitionProvider':True,'renameProvider':True}}
    elif method=='textDocument/definition':result=[{'uri':uri,'range':{'start':{'line':1,'character':0},'end':{'line':1,'character':4}}}]
    elif method=='notes/source':
        assert params==({'uri':uri} if shape=='uri' else {'textDocument':{'uri':uri}})
        if mode=='root':result=text
        elif mode=='nested':result={'body/text':{'~value':text}}
        elif mode=='missing':result={'different':'missing'}
        elif mode=='wrong-type':result={'source':text}
        elif mode=='oversized':result='x'*(limit+1)
    elif method=='textDocument/rename':result={'changes':{uri:[]}}
    if 'id' in message:send({'id':message['id'],'result':result})
"#).unwrap();
        let host = Host::default();
        let scope = Scope { owner:"owner".into(), project:1, root:root.path().into(), provider:"community/notes".into(), digest:"a".repeat(64), server:serde_json::from_value(json!({
            "command":"python3", "args":[script, parameters, mode, openwebide_core::plugins::languages::MAX_SOURCE_BYTES.to_string()],
            "virtualDocuments":[{"scheme":"notes", "method":"notes/source", "parameters":parameters, "textPointer":pointer}]
        })).unwrap() };
        let mut request = Request {
            version: protocol::VERSION,
            client: "client".into(),
            document: "document".into(),
            request: 1,
            revision: 1,
            language: "notes".into(),
            path: "file.notes".into(),
            source: source.into(),
            source_digest: source_digest(source),
            query: Query::Definition {
                position: protocol::Position::default(),
            },
        };
        let response = host.query(scope.clone(), &request).await;
        if matches!(mode, "undeclared" | "oversized-uri") {
            assert!(response.unwrap_err().contains("not declared"));
            assert!(
                !std::fs::read_to_string(root.path().join("calls"))
                    .unwrap()
                    .contains("notes/source")
            );
            host.shutdown();
            continue;
        }
        let response = response.unwrap();
        let id = response.result[0]["resource"].as_str().unwrap().to_owned();
        assert_eq!(response.result[0]["label"], "Library.notes");
        assert!(response.result[0]["path"].is_null());
        assert!(!response.result.to_string().contains("server-token"));
        request.request += 1;
        request.query = Query::ReadResource { id: id.clone() };
        let response = host.query(scope.clone(), &request).await;
        if !matches!(mode, "root" | "nested") {
            assert!(response.unwrap_err().contains("source text"));
            host.shutdown();
            continue;
        }
        let response = response.unwrap();
        assert_eq!(
            response.result,
            json!({"name":"Library.notes", "source":"// 文😀\r\nread only library\r\n", "readOnly":true})
        );
        for boundary in [
            "document", "client", "owner", "project", "provider", "server",
        ] {
            let mut foreign_scope = scope.clone();
            let mut foreign = request.clone();
            foreign.request += 1;
            match boundary {
                "document" => {
                    foreign.document = "other".into();
                    foreign.path = "other.notes".into();
                }
                "client" => foreign.client = "other".into(),
                "owner" => foreign_scope.owner = "other".into(),
                "project" => foreign_scope.project = 2,
                "provider" => foreign_scope.provider = "other".into(),
                "server" => foreign_scope.server.settings = json!({"different":true}),
                _ => unreachable!(),
            }
            assert!(
                host.query(foreign_scope, &foreign)
                    .await
                    .unwrap_err()
                    .contains("stale"),
                "{boundary}"
            );
            // Foreign session creation may evict the original idle session.
            // Re-establish its resource through navigation, never client URI input.
            request.request += 2;
            request.query = Query::Definition {
                position: protocol::Position::default(),
            };
            let listed = host.query(scope.clone(), &request).await.unwrap();
            request.query = Query::ReadResource {
                id: listed.result[0]["resource"].as_str().unwrap().into(),
            };
        }
        let current_id = match &request.query {
            Query::ReadResource { id } => id.clone(),
            _ => unreachable!(),
        };
        request.request += 1;
        request.query = Query::Rename {
            position: protocol::Position::default(),
            new_name: "Changed".into(),
        };
        assert!(
            host.query(scope.clone(), &request)
                .await
                .unwrap_err()
                .contains("scheme")
        );
        request.request += 1;
        request.revision += 1;
        request.source.push_str("// changed\r\n");
        request.source_digest = source_digest(&request.source);
        request.query = Query::ReadResource { id: current_id };
        assert!(
            host.query(scope.clone(), &request)
                .await
                .unwrap_err()
                .contains("stale")
        );
        request.request += 1;
        request.query = Query::Definition {
            position: protocol::Position::default(),
        };
        let listed = host.query(scope.clone(), &request).await.unwrap();
        let id = listed.result[0]["resource"].as_str().unwrap().into();
        host.revoke("owner", Some(1), Some("community/notes"));
        request.request += 1;
        request.query = Query::ReadResource { id };
        assert!(
            host.query(scope.clone(), &request)
                .await
                .unwrap_err()
                .contains("stale")
        );
        request.request += 1;
        request.query = Query::Definition {
            position: protocol::Position::default(),
        };
        let listed = host.query(scope.clone(), &request).await.unwrap();
        let id = listed.result[0]["resource"].as_str().unwrap().into();
        request.request += 1;
        request.query = Query::Close;
        host.query(scope.clone(), &request).await.unwrap();
        request.request += 1;
        request.query = Query::ReadResource { id };
        assert!(
            host.query(scope, &request)
                .await
                .unwrap_err()
                .contains("stale")
        );
        let calls = std::fs::read_to_string(root.path().join("calls")).unwrap();
        assert!(!calls.contains("workspace/executeCommand"));
        assert!(!calls.contains("workspace/applyEdit"));
        assert_eq!(
            std::fs::read_to_string(root.path().join("file.notes")).unwrap(),
            source
        );
        host.shutdown();
    }
}

#[tokio::test]
async fn capacity_never_evicts_active_requests_and_revocation_is_scoped() {
    let host = Host::default();
    let first = host.entry(&key("owner-a", 1)).unwrap();
    let second = host.entry(&key("owner-a", 2)).unwrap();
    *first.used.lock().unwrap() = Instant::now() - Duration::from_secs(2);
    *second.used.lock().unwrap() = Instant::now() - Duration::from_secs(1);
    first
        .active
        .lock()
        .unwrap()
        .insert("document".into(), (1, watch::channel(false).0));
    let third = host.entry(&key("owner-a", 3)).unwrap();
    assert!(!*first.stopped.borrow());
    assert!(*second.stopped.borrow());
    third
        .active
        .lock()
        .unwrap()
        .insert("document".into(), (1, watch::channel(false).0));
    assert!(host.entry(&key("owner-a", 4)).is_err());
    let other = host.entry(&key("owner-b", 1)).unwrap();
    host.revoke("owner-a", Some(1), Some("provider"));
    assert!(*first.stopped.borrow());
    assert!(!*third.stopped.borrow());
    assert!(!*other.stopped.borrow());
    host.shutdown();
    assert!(*third.stopped.borrow());
    assert!(*other.stopped.borrow());
}

#[tokio::test]
async fn protocol_retries_index_cancellation_but_never_caller_cancellation() {
    for code in [-32801, -32802, -32800] {
        let (local, remote) = tokio::io::duplex(4096);
        let (input, output) = tokio::io::split(local);
        let (mut reader, mut writer) = tokio::io::split(remote);
        let (client, _events) = rpc::Client::connect(output, BufReader::new(input));
        let calling = client.clone();
        let call = tokio::spawn(async move {
            request_current(&calling, "textDocument/hover", json!({})).await
        });
        let first = framing::read(&mut BufReader::new(&mut reader))
            .await
            .unwrap();
        framing::write(
            &mut writer,
            &json!({"jsonrpc":"2.0","id":first["id"],"error":{"code":code,"message":"cancelled"}}),
        )
        .await
        .unwrap();
        if code == -32800 {
            assert!(call.await.unwrap().unwrap_err().contains("-32800"));
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(100),
                    framing::read(&mut BufReader::new(&mut reader))
                )
                .await
                .is_err()
            );
        } else {
            let next = framing::read(&mut BufReader::new(&mut reader))
                .await
                .unwrap();
            assert_ne!(first["id"], next["id"]);
            framing::write(
                &mut writer,
                &json!({"jsonrpc":"2.0","id":next["id"],"result":"current"}),
            )
            .await
            .unwrap();
            assert_eq!(call.await.unwrap().unwrap(), "current");
        }
    }
}

#[tokio::test]
async fn missing_toolchain_has_actionable_bounded_diagnostics_and_startup_backoff() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("file.rs"), "fn main() {}").unwrap();
    let host = Host::default();
    let scope=Scope {owner:"owner".into(),project:1,root:root.path().into(),provider:"provider".into(),digest:"a".repeat(64),server:serde_json::from_value(json!({"command":"rustup","args":["run","openwebide-lsp-missing-toolchain","rust-analyzer"]})).unwrap()};
    let mut request = Request {
        version: protocol::VERSION,
        client: "client".into(),
        document: "file".into(),
        request: 1,
        revision: 1,
        language: "rust".into(),
        path: "file.rs".into(),
        source: "fn main() {}".into(),
        source_digest: openwebide_core::plugins::languages::source_digest("fn main() {}"),
        query: Query::Synchronize,
    };
    let error = host.query(scope.clone(), &request).await.unwrap_err();
    assert!(
        error.contains("Install the server and its toolchain"),
        "{error}"
    );
    assert!(error.contains("not installed"), "{error}");
    assert!(error.len() < 4096);
    request.request += 1;
    assert!(
        host.query(scope, &request)
            .await
            .unwrap_err()
            .contains("waiting to retry")
    );
    assert_eq!(
        host.inner
            .entries
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .failure
            .lock()
            .unwrap()
            .count,
        1
    );
}

#[tokio::test]
async fn project_paths_and_workspace_markers_do_not_escape_the_authorized_root() {
    let root = tempfile::tempdir().unwrap();
    let root = tokio::fs::canonicalize(root.path()).await.unwrap();
    tokio::fs::create_dir_all(root.join("nested/src"))
        .await
        .unwrap();
    tokio::fs::write(root.join("nested/Cargo.toml"), "")
        .await
        .unwrap();
    let document = documents::source_path(&root, "nested/src/new.rs")
        .await
        .unwrap();
    assert_eq!(
        documents::workspace_root(&root, &document, &["Cargo.toml".into()]).await,
        root.join("nested")
    );
    tokio::fs::write(root.join("Cargo.toml"), "").await.unwrap();
    assert_eq!(
        documents::workspace_root(&root, &document, &["Cargo.toml".into()]).await,
        root
    );
    assert!(
        documents::source_path(&root, "../outside.rs")
            .await
            .is_err()
    );
    #[cfg(unix)]
    {
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("escape")).unwrap();
        assert!(
            documents::source_path(&root, "escape/new.rs")
                .await
                .is_err()
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn deferred_code_actions_preserve_data_and_review_edits_with_legacy_capabilities() {
    use openwebide_core::plugins::languages::{intelligence::results, source_digest};

    for (capability, mode, resolves) in [
        (json!(true), "missing-edit", true),
        (json!(true), "null-edit", true),
        (json!({"resolveProvider":true}), "null-edit", true),
        (json!({"resolveProvider":false}), "null-edit", false),
        (json!(true), "command-only", false),
    ] {
        let root = tempfile::tempdir().unwrap();
        let source = "// 文😀\r\nclass Example {}\r\n";
        std::fs::write(root.path().join("file.cs"), source).unwrap();
        let script = root.path().join("fixture.py");
        std::fs::write(
            &script,
            r#"import sys,json
capability=json.loads(sys.argv[1]);mode=sys.argv[2]
def send(value):
    body=json.dumps({'jsonrpc':'2.0',**value}).encode()
    sys.stdout.buffer.write(('Content-Length: %d\r\n\r\n'%len(body)).encode()+body)
    sys.stdout.buffer.flush()
while True:
    headers={}
    while True:
        line=sys.stdin.buffer.readline()
        if not line:sys.exit(0)
        if line==b'\r\n':break
        key,value=line.decode().split(':',1);headers[key.lower()]=value.strip()
    message=json.loads(sys.stdin.buffer.read(int(headers['content-length'])))
    method=message.get('method')
    with open('calls','a') as calls:calls.write(str(method)+'\n')
    params=message.get('params') or {}
    result=None
    if method=='initialize':result={'capabilities':{'textDocumentSync':1,'codeActionProvider':capability}}
    elif method=='textDocument/didOpen':
        document=params['textDocument']
        send({'method':'textDocument/publishDiagnostics','params':{'uri':document['uri'],'version':document['version'],'diagnostics':[]}})
    elif method=='textDocument/codeAction':
        action={'title':'Add using','data':{'uri':params['textDocument']['uri'],'opaque':[1,'keep']}}
        if mode=='null-edit':action['edit']=None
        if mode=='command-only':action={'title':'Run command','command':{'title':'Run','command':'unsupported'},'data':{'opaque':[1,'keep']}}
        result=[action]
    elif method=='codeAction/resolve':
        assert params['data']['opaque']==[1,'keep']
        result={'title':params['title'],'edit':{'changes':{params['data']['uri']:[{'range':{'start':{'line':0,'character':0},'end':{'line':0,'character':0}},'newText':'using System.Text;\r\n'}]}}}
    if 'id' in message:send({'id':message['id'],'result':result})
"#,
        )
        .unwrap();
        let host = Host::default();
        let scope = Scope {
            owner: "owner".into(),
            project: 1,
            root: root.path().into(),
            provider: "community/fixture".into(),
            digest: "a".repeat(64),
            server: serde_json::from_value(json!({
                "command":"python3", "args":[script,capability.to_string(),mode]
            }))
            .unwrap(),
        };
        let mut request = Request {
            version: protocol::VERSION,
            client: "client".into(),
            document: "document".into(),
            request: 1,
            revision: 1,
            language: "fixture".into(),
            path: "file.cs".into(),
            source: source.into(),
            source_digest: source_digest(source),
            query: Query::CodeActions {
                range: protocol::Range {
                    start: protocol::Position {
                        line: 0,
                        character: 0,
                    },
                    end: protocol::Position {
                        line: 0,
                        character: 0,
                    },
                },
            },
        };
        let listed = host.query(scope.clone(), &request).await.unwrap();
        let id = listed.result[0]["id"].as_str().unwrap().to_string();
        request.request += 1;
        request.query = Query::ResolveAction { id: id.clone() };
        let response = host.query(scope.clone(), &request).await;
        if resolves {
            let edit: results::WorkspaceEdit =
                serde_json::from_value(response.unwrap().result).unwrap();
            assert_eq!(edit.files.len(), 1);
            assert_eq!(edit.files[0].path, "file.cs");
            assert_eq!(edit.files[0].source_digest, source_digest(source));
            let staged = results::apply_edits(source, &edit.files[0].edits).unwrap();
            assert!(staged.starts_with("using System.Text;\r\n"));
            // A resolved action remains tied to the original unsaved snapshot.
            request.request += 1;
            request.revision += 1;
            request.source = format!("{source}// newer source\r\n");
            request.source_digest = source_digest(&request.source);
            request.query = Query::Synchronize;
            host.query(scope.clone(), &request).await.unwrap();
            request.request += 1;
            request.query = Query::ResolveAction { id };
            assert!(
                host.query(scope, &request)
                    .await
                    .unwrap_err()
                    .contains("stale")
            );
        } else if let Ok(response) = response {
            let edit: results::WorkspaceEdit = serde_json::from_value(response.result).unwrap();
            assert!(edit.files.is_empty());
        }
        let calls = std::fs::read_to_string(root.path().join("calls")).unwrap();
        assert_eq!(calls.contains("codeAction/resolve"), resolves);
        assert!(!calls.contains("workspace/executeCommand"));
        assert_eq!(
            std::fs::read_to_string(root.path().join("file.cs")).unwrap(),
            source
        );
        host.shutdown();
    }
}
