use super::*;
use openwebide_core::plugins::languages::source_digest;

#[tokio::test]
async fn opened_and_restored_drafts_replace_previously_indexed_disk_sources() {
    for sync in [
        json!(1),
        json!({"openClose":true,"change":2}),
        json!({"openClose":true,"change":0}),
        Value::Null,
    ] {
        check_drafts(sync).await;
    }
}

async fn check_drafts(sync: Value) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("main.notes"), "disk").unwrap();
    let script = root.path().join("server.py");
    std::fs::write(&script, r#"import json,os,sys
from pathlib import Path
from urllib.parse import unquote,urlparse
parsed={};opened={};versions={}
sync=json.loads(sys.argv[1])
changes=sync if isinstance(sync,int) else (sync or {}).get('change',0)
while True:
    headers={}
    while True:
        line=sys.stdin.buffer.readline()
        if not line:sys.exit(0)
        if line==b'\r\n':break
        name,value=line.decode().split(':',1);headers[name.lower()]=value.strip()
    message=json.loads(sys.stdin.buffer.read(int(headers['content-length'])))
    method=message.get('method');params=message.get('params') or {};result=None
    with open('calls','a') as log:log.write(json.dumps(message)+'\n')
    document=params.get('textDocument',{});uri=document.get('uri','')
    if method=='initialize':result={'capabilities':{'textDocumentSync':sync,'hoverProvider':True}}
    elif method=='textDocument/didOpen':
        path=Path(unquote(urlparse(uri).path))
        if changes:parsed.setdefault(uri,path.read_text() if path.exists() else '')
        else:parsed[uri]=document['text']
        opened[uri]=document['text'];versions[uri]=document['version']
    elif method=='textDocument/didChange':
        assert changes in (1,2),'undeclared change notifications'
        assert document['version']>versions[uri]
        versions[uri]=document['version']
        parsed[uri]=opened[uri]=params['contentChanges'][0]['text']
    elif method=='textDocument/didClose':opened.pop(uri);versions.pop(uri)
    elif method=='textDocument/hover':
        result={'contents':json.dumps({'source':parsed[uri],'pid':os.getpid()})}
    if 'id' in message:
        body=json.dumps({'jsonrpc':'2.0','id':message['id'],'result':result}).encode()
        sys.stdout.buffer.write(('Content-Length: %d\r\n\r\n'%len(body)).encode()+body);sys.stdout.buffer.flush()
"#).unwrap();
    let scope = Scope {
        owner: "owner".into(),
        project: 1,
        root: root.path().into(),
        provider: "community/notes".into(),
        digest: "a".repeat(64),
        server: serde_json::from_value(
            json!({"command":"python3","args":[script,sync.to_string()]}),
        )
        .unwrap(),
    };
    let host = Host::default();
    let mut request = Request {
        version: protocol::VERSION,
        client: "editor-a".into(),
        document: "main".into(),
        request: 1,
        revision: 1,
        language: "notes".into(),
        path: "main.notes".into(),
        source: "draft 文😀\r\n".into(),
        source_digest: source_digest("draft 文😀\r\n"),
        query: Query::Hover {
            position: protocol::Position::default(),
        },
    };
    let first = host.query(scope.clone(), &request).await;
    if !sync
        .as_i64()
        .or_else(|| sync.get("change").and_then(Value::as_i64))
        .is_some_and(|kind| matches!(kind, 1 | 2))
    {
        assert!(
            first
                .unwrap_err()
                .contains("does not support live document synchronization")
        );
        let calls = std::fs::read_to_string(root.path().join("calls")).unwrap();
        assert!(!calls.contains("textDocument/didOpen"));
        assert!(!calls.contains("textDocument/didChange"));
        host.shutdown();
        return;
    }
    let first = first.unwrap();
    let first_value: Value =
        serde_json::from_str(first.result["contents"].as_str().unwrap()).unwrap();
    assert_eq!(first_value["source"], request.source);
    request.request += 1;
    let warm = host.query(scope.clone(), &request).await.unwrap();
    assert_eq!(warm.server, first.server);
    request.client = "editor-b".into();
    request.source = "another draft\r\n".into();
    request.source_digest = source_digest(&request.source);
    request.request += 1;
    let other = host.query(scope.clone(), &request).await.unwrap();
    let other_value: Value =
        serde_json::from_str(other.result["contents"].as_str().unwrap()).unwrap();
    assert_eq!(other_value["source"], request.source);
    assert_eq!(other_value["pid"], first_value["pid"]);
    assert_ne!(other.server, first.server);
    request.client = "editor-a".into();
    request.source = "draft 文😀\r\n".into();
    request.source_digest = source_digest(&request.source);
    request.request += 1;
    let restored = host.query(scope.clone(), &request).await.unwrap();
    restored.validate(&request).unwrap();
    let restored: Value =
        serde_json::from_str(restored.result["contents"].as_str().unwrap()).unwrap();
    assert_eq!(restored, first_value);
    // A later-opened, initially nonexistent file belongs to the same live
    // source inventory without having to materialize its draft on disk.
    request.document = "new".into();
    request.path = "new.notes".into();
    request.request += 1;
    let added = host.query(scope, &request).await.unwrap();
    let added: Value = serde_json::from_str(added.result["contents"].as_str().unwrap()).unwrap();
    assert_eq!(added, first_value);
    assert!(!root.path().join("new.notes").exists());
    assert_eq!(
        std::fs::read_to_string(root.path().join("main.notes")).unwrap(),
        "disk"
    );
    let calls = std::fs::read_to_string(root.path().join("calls")).unwrap();
    assert!(!calls.contains("textDocument/didSave"));
    assert_eq!(calls.matches("\"method\": \"initialize\"").count(), 1);
    host.shutdown();
}
