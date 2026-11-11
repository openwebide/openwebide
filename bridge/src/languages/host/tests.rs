use super::super::{framing, rpc};
use super::*;
use tokio::io::BufReader;
mod lifecycle;
mod synchronization;

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
async fn workspace_symbol_resolution_and_extended_navigation_reject_other_clients_and_stale_results()
 {
    let root = tempfile::tempdir().unwrap();
    let source = "文😀 Item\n";
    std::fs::write(root.path().join("main.notes"), source).unwrap();
    let script = root.path().join("server.py");
    std::fs::write(&script,r#"import json,sys,pathlib
def send(value):
    body=json.dumps({'jsonrpc':'2.0',**value}).encode()
    sys.stdout.buffer.write(('Content-Length: %d\r\n\r\n'%len(body)).encode()+body);sys.stdout.buffer.flush()
uri=pathlib.Path('main.notes').resolve().as_uri()
where={'uri':uri,'range':{'start':{'line':0,'character':4},'end':{'line':0,'character':8}}}
while True:
    headers={}
    while True:
        line=sys.stdin.buffer.readline()
        if not line:sys.exit(0)
        if line==b'\r\n':break
        name,value=line.decode().split(':',1);headers[name.lower()]=value.strip()
    message=json.loads(sys.stdin.buffer.read(int(headers['content-length'])))
    method=message.get('method');params=message.get('params') or {};result=None
    if method=='initialize':result={'capabilities':{'textDocumentSync':1,'workspaceSymbolProvider':{'resolveProvider':True},'typeDefinitionProvider':True,'implementationProvider':True}}
    elif method=='workspace/symbol':result=[{'name':'Item','kind':5,'location':where},{'name':'Later','kind':5,'containerName':'Example','location':{'uri':uri},'data':{'selected':True}}]
    elif method=='workspaceSymbol/resolve':
        assert params['data']['selected'];result={**params,'location':where}
    elif method in ('textDocument/typeDefinition','textDocument/implementation'):result=[{'targetUri':uri,'targetSelectionRange':where['range'],'targetRange':where['range']}]
    if 'id' in message:send({'id':message['id'],'result':result})
"#).unwrap();
    let host = Host::default();
    let scope = Scope {
        owner: "owner".into(),
        project: 1,
        root: root.path().into(),
        provider: "community/notes".into(),
        digest: "a".repeat(64),
        server: serde_json::from_value(json!({"command":"python3","args":[script]})).unwrap(),
    };
    let mut input = Request {
        version: protocol::VERSION,
        client: "client-a".into(),
        document: "document".into(),
        request: 1,
        revision: 1,
        language: "notes".into(),
        path: "main.notes".into(),
        source_digest: openwebide_core::plugins::languages::source_digest(source),
        source: source.into(),
        query: Query::WorkspaceSymbols {
            query: "Item".into(),
        },
    };
    let response = host.query(scope.clone(), &input).await.unwrap();
    let symbols: Vec<protocol::results::WorkspaceSymbol> =
        serde_json::from_value(response.result).unwrap();
    assert_eq!(symbols[0].target.as_ref().unwrap().range.start.character, 4);
    let id = symbols[1].id.clone().unwrap();
    assert_eq!(id.len(), 32);
    assert!(symbols[1].target.is_none());
    input.request += 1;
    input.query = Query::ResolveSymbol { id: id.clone() };
    let resolved = host.query(scope.clone(), &input).await.unwrap();
    let targets: Vec<protocol::results::Target> = serde_json::from_value(resolved.result).unwrap();
    assert_eq!(targets[0].range.offsets(source).unwrap(), 8..12);
    let mut other = input.clone();
    other.client = "client-b".into();
    assert!(
        host.query(scope.clone(), &other)
            .await
            .unwrap_err()
            .contains("stale or belongs")
    );
    input.request += 1;
    input.revision += 1;
    input.source.push('\n');
    input.source_digest = openwebide_core::plugins::languages::source_digest(&input.source);
    assert!(
        host.query(scope.clone(), &input)
            .await
            .unwrap_err()
            .contains("stale or belongs")
    );
    for query in [
        Query::TypeDefinition {
            position: protocol::Position {
                line: 0,
                character: 4,
            },
        },
        Query::Implementation {
            position: protocol::Position {
                line: 0,
                character: 4,
            },
        },
    ] {
        input.request += 1;
        input.query = query;
        let response = host.query(scope.clone(), &input).await.unwrap();
        let targets: Vec<protocol::results::Target> =
            serde_json::from_value(response.result).unwrap();
        assert_eq!(
            targets[0].source_digest.as_deref(),
            Some(input.source_digest.as_str())
        );
    }
    host.shutdown();
    assert_eq!(
        std::fs::read_to_string(root.path().join("main.notes")).unwrap(),
        source
    );
}

#[tokio::test]
async fn dependent_documents_refresh_once_after_overlay_changes_and_close_without_saving() {
    use openwebide_core::plugins::languages::source_digest;
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("header.notes"), "disk").unwrap();
    std::fs::write(root.path().join("main.notes"), "use header").unwrap();
    let script = root.path().join("server.py");
    std::fs::write(&script, r#"import json,sys
opened={};parsed={}
def send(value):
    body=json.dumps({'jsonrpc':'2.0',**value}).encode()
    sys.stdout.buffer.write(('Content-Length: %d\r\n\r\n'%len(body)).encode()+body);sys.stdout.buffer.flush()
while True:
    headers={}
    while True:
        line=sys.stdin.buffer.readline()
        if not line:sys.exit(0)
        if line==b'\r\n':break
        name,value=line.decode().split(':',1);headers[name.lower()]=value.strip()
    message=json.loads(sys.stdin.buffer.read(int(headers['content-length'])))
    method=message.get('method');params=message.get('params') or {};result=None
    document=params.get('textDocument',{});uri=document.get('uri','')
    with open('calls','a') as log:log.write(method+' '+uri+'\n')
    if method=='initialize':result={'capabilities':{'textDocumentSync':1,'hoverProvider':True}}
    elif method=='textDocument/didOpen':
        opened[uri]=document['text']
        header=uri.rsplit('/',1)[0]+'/header.notes'
        parsed[uri]=opened.get(header,'disk')
    elif method=='textDocument/didChange':opened[uri]=params['contentChanges'][0]['text']
    elif method=='textDocument/didClose':opened.pop(uri,None);parsed.pop(uri,None)
    elif method=='textDocument/hover':result={'contents':parsed[uri]}
    if 'id' in message:send({'id':message['id'],'result':result})
"#).unwrap();
    let host = Host::default();
    let scope = Scope {
        owner: "owner".into(),
        project: 1,
        root: root.path().into(),
        provider: "community/notes".into(),
        digest: "a".repeat(64),
        server: serde_json::from_value(json!({"command":"python3","args":[script]})).unwrap(),
    };
    let mut request = Request {
        version: protocol::VERSION,
        client: "client".into(),
        document: "header".into(),
        request: 1,
        revision: 1,
        language: "notes".into(),
        path: "header.notes".into(),
        source: "first".into(),
        source_digest: source_digest("first"),
        query: Query::Synchronize,
    };
    let first = host.query(scope.clone(), &request).await.unwrap();
    let mut main = request.clone();
    main.document = "main".into();
    main.path = "main.notes".into();
    main.source = "use header".into();
    main.source_digest = source_digest(&main.source);
    main.query = Query::Hover {
        position: protocol::Position::default(),
    };
    let response = host.query(scope.clone(), &main).await.unwrap();
    assert_eq!(response.result["contents"], "first");
    request.request += 1;
    request.revision += 1;
    request.source = "second".into();
    request.source_digest = source_digest(&request.source);
    host.query(scope.clone(), &request).await.unwrap();
    main.request += 1;
    let updated = host.query(scope.clone(), &main).await.unwrap();
    updated.validate(&main).unwrap();
    assert_eq!(updated.result["contents"], "second");
    assert_eq!(updated.server, first.server);
    let calls = std::fs::read_to_string(root.path().join("calls")).unwrap();
    assert_eq!(calls.matches("textDocument/didClose").count(), 1);
    main.request += 1;
    host.query(scope.clone(), &main).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(root.path().join("calls")).unwrap(),
        calls
            + "textDocument/hover "
            + url::Url::from_file_path(
                std::fs::canonicalize(root.path().join("main.notes")).unwrap()
            )
            .unwrap()
            .as_str()
            + "\n"
    );
    request.request += 1;
    request.query = Query::Close;
    host.query(scope.clone(), &request).await.unwrap();
    let mut late = request.clone();
    late.request -= 1;
    late.query = Query::Synchronize;
    assert!(
        host.query(scope.clone(), &late)
            .await
            .unwrap_err()
            .contains("stale")
    );
    main.request += 1;
    let closed = host.query(scope.clone(), &main).await.unwrap();
    assert_eq!(closed.result["contents"], "disk");
    assert_eq!(closed.server, first.server);
    assert_eq!(
        std::fs::read_to_string(root.path().join("header.notes")).unwrap(),
        "disk"
    );
    assert!(
        !std::fs::read_to_string(root.path().join("calls"))
            .unwrap()
            .contains("textDocument/didSave")
    );
    host.shutdown();
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
    let input = lifecycle::request();
    let (first_request, _) = host.acquire(&key("owner-a", 1), &input).unwrap();
    let first = first_request.entry.clone();
    let (second_request, _) = host.acquire(&key("owner-a", 2), &input).unwrap();
    let second = second_request.entry.clone();
    drop(second_request);
    *first.used.lock().unwrap() = Instant::now() - Duration::from_secs(2);
    *second.used.lock().unwrap() = Instant::now() - Duration::from_secs(1);
    let (third_request, _) = host.acquire(&key("owner-a", 3), &input).unwrap();
    let third = third_request.entry.clone();
    assert!(!*first.stopped.borrow());
    assert!(*second.stopped.borrow());
    assert!(host.acquire(&key("owner-a", 4), &input).is_err());
    let (other_request, _) = host.acquire(&key("owner-b", 1), &input).unwrap();
    let other = other_request.entry.clone();
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

#[tokio::test]
async fn ordered_workspace_edits_keep_logical_sources_and_reject_partial_commands() {
    use openwebide_core::plugins::languages::{
        intelligence::{
            Position, Range,
            results::{WorkspaceChange, WorkspaceEdit},
        },
        source_digest,
    };
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("main.notes"), "disk original").unwrap();
    std::fs::write(root.path().join("doomed.notes"), [0, 255]).unwrap();
    let script = root.path().join("ordered.py");
    std::fs::write(&script, r#"import json,sys,pathlib
root=pathlib.Path.cwd();uri=(root/'main.notes').as_uri();moved=(root/'moved.notes').as_uri();new=(root/'new'/'new.notes').as_uri();final=(root/'final.notes').as_uri()
def text(uri,end,value,version=None):return {'textDocument':{'uri':uri,'version':version},'edits':[{'range':{'start':{'line':0,'character':0},'end':{'line':0,'character':end}},'newText':value}]}
def send(id,result):
    data=json.dumps({'jsonrpc':'2.0','id':id,'result':result}).encode();sys.stdout.buffer.write(('Content-Length: %d\r\n\r\n'%len(data)).encode()+data);sys.stdout.buffer.flush()
while True:
    headers={}
    while True:
        line=sys.stdin.buffer.readline()
        if not line:sys.exit(0)
        if line==b'\r\n':break
        key,value=line.decode().split(':',1);headers[key.lower()]=value.strip()
    message=json.loads(sys.stdin.buffer.read(int(headers['content-length'])))
    method=message.get('method');params=message.get('params') or {};result=None
    if method=='initialize':result={'capabilities':{'textDocumentSync':1,'renameProvider':True,'codeActionProvider':True}}
    elif method=='textDocument/rename':
        changes=[text(uri,9,'first'),{'kind':'rename','oldUri':uri,'newUri':moved},text(moved,5,'second 文😀'),{'kind':'create','uri':new},text(new,0,'created'),{'kind':'rename','oldUri':new,'newUri':final},{'kind':'delete','uri':(root/'doomed.notes').as_uri()}]
        if params['newName']=='outside':changes[1]['newUri']=(root.parent/'outside.notes').as_uri()
        if params['newName']=='stale':changes[0]['textDocument']['version']=999
        if params['newName']=='options':changes[1]['options']={'overwrite':'yes'}
        result={'documentChanges':changes}
    elif method=='textDocument/codeAction':result=[{'title':'Edit and command','kind':'quickfix','edit':{'changes':{uri:text(uri,9,'changed')['edits']}},'command':{'title':'Run','command':'must-not-execute'}}]
    elif method=='codeAction/resolve':result=params
    elif method=='workspace/executeCommand':raise Exception('Unapproved server command executed')
    if 'id' in message:send(message['id'],result)
"#).unwrap();
    let host = Host::default();
    let scope = Scope {
        owner: "owner".into(),
        project: 1,
        root: root.path().into(),
        provider: "community/notes".into(),
        digest: "a".repeat(64),
        server: serde_json::from_value(json!({"command":"python3","args":[script]})).unwrap(),
    };
    let mut input = Request {
        version: protocol::VERSION,
        client: "client".into(),
        document: "document".into(),
        request: 1,
        revision: 1,
        language: "notes".into(),
        path: "main.notes".into(),
        source: "draft old".into(),
        source_digest: source_digest("draft old"),
        query: Query::Rename {
            position: Position::default(),
            new_name: "move".into(),
        },
    };
    let response = host.query(scope.clone(), &input).await.unwrap();
    let edit: WorkspaceEdit = serde_json::from_value(response.result).unwrap();
    assert!(edit.files.is_empty());
    assert_eq!(edit.changes.len(), 7);
    let sources = edit
        .changes
        .iter()
        .filter_map(|change| match change {
            WorkspaceChange::Text { file } => {
                Some((file.path.as_str(), file.source_digest.clone()))
            }
            WorkspaceChange::File { .. } => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sources,
        [
            ("main.notes", source_digest("draft old")),
            ("moved.notes", source_digest("first")),
            ("new/new.notes", source_digest(""))
        ]
    );
    for (name, message) in [
        ("outside", "outside"),
        ("stale", "stale"),
        ("options", "options"),
    ] {
        input.request += 1;
        input.query = Query::Rename {
            position: Position::default(),
            new_name: name.into(),
        };
        assert!(
            host.query(scope.clone(), &input)
                .await
                .unwrap_err()
                .contains(message)
        );
    }
    input.request += 1;
    input.query = Query::CodeActions {
        range: Range::default(),
    };
    let response = host.query(scope.clone(), &input).await.unwrap();
    let id = response.result[0]["id"].as_str().unwrap().to_string();
    input.request += 1;
    input.query = Query::ResolveAction { id };
    assert!(
        host.query(scope, &input)
            .await
            .unwrap_err()
            .contains("command approval")
    );
    host.shutdown();
    assert_eq!(
        std::fs::read_to_string(root.path().join("main.notes")).unwrap(),
        "disk original"
    );
    assert!(!root.path().join("moved.notes").exists());
    assert!(!root.path().join("new").exists());
    assert_eq!(
        std::fs::read(root.path().join("doomed.notes")).unwrap(),
        [0, 255]
    );
}
