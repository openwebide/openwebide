//! Same native service contract through the installed public package in both
//! authenticated transports. The peer supplies LSP only, never host behavior.
use super::*;
use openwebide_core::plugins::languages::{
    intelligence::{self, HostRequest, Position, Query, Request, Response, Selection},
    source_digest,
};
use serde_json::{Value, json};

fn input(client: &str, path: &str, source: &str, sequence: u64, close: bool) -> Request {
    Request {
        version: intelligence::VERSION,
        client: client.into(),
        document: source_digest(path),
        request: sequence,
        revision: sequence,
        language: "reuse-notes".into(),
        path: path.into(),
        source: source.into(),
        source_digest: source_digest(source),
        query: if close {
            Query::Close
        } else {
            Query::Hover {
                position: Position::default(),
            }
        },
    }
}
async fn query(port: u16, paired: bool, scope: &HostRequest, input: Request) -> (Response, Value) {
    let close = matches!(input.query, Query::Close);
    let mut request = scope.clone();
    request.selection.input = input;
    let response = preparation_request(
        port,
        paired,
        "/plugins/languages/query",
        serde_json::to_value(&request).unwrap(),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body);
    let response: Response = serde_json::from_str(&response.body).unwrap();
    response.validate(&request.selection.input).unwrap();
    let value = if close {
        Value::Null
    } else {
        serde_json::from_str(response.result["contents"].as_str().unwrap()).unwrap()
    };
    (response, value)
}
pub(super) async fn check(port: u16, paired: bool, prepared: &PreparedPlugin, workspace: &Path) {
    let root = workspace.join(if paired {
        "reuse-paired"
    } else {
        "reuse-remote"
    });
    std::fs::create_dir(&root).unwrap();
    for path in ["a.reuse-notes", "b.reuse-notes"] {
        std::fs::write(root.join(path), "disk").unwrap();
    }
    let scope = HostRequest {
        user: Some(42),
        root: root.to_string_lossy().into(),
        selection: Selection {
            user: 42,
            project: 101,
            prepared: prepared.clone(),
            input: Request {
                version: intelligence::VERSION,
                client: "A".into(),
                document: "file".into(),
                request: 1,
                revision: 1,
                language: "reuse-notes".into(),
                path: "a.reuse-notes".into(),
                source: "A".into(),
                source_digest: source_digest("A"),
                query: Query::Synchronize,
            },
        },
    };
    let (first, initial) = query(
        port,
        paired,
        &scope,
        input("A", "a.reuse-notes", "A", 1, false),
    )
    .await;
    query(
        port,
        paired,
        &scope,
        input("A", "b.reuse-notes", "B of A", 2, false),
    )
    .await;
    let (second, other) = query(
        port,
        paired,
        &scope,
        input("B", "a.reuse-notes", "new B", 1, false),
    )
    .await;
    assert_ne!(first.server, second.server);
    assert_eq!(initial["pid"], other["pid"]);
    assert_eq!(other["open"], json!({"a.reuse-notes":"new B"}));
    let (_, resumed) = query(
        port,
        paired,
        &scope,
        input("A", "a.reuse-notes", "A after", 3, false),
    )
    .await;
    assert_eq!(resumed["pid"], initial["pid"]);
    assert_eq!(
        resumed["open"],
        json!({"a.reuse-notes":"A after","b.reuse-notes":"B of A"})
    );
    query(
        port,
        paired,
        &scope,
        input("B", "a.reuse-notes", "new B", 2, true),
    )
    .await;
    let (_, reopened) = query(
        port,
        paired,
        &scope,
        input("B", "b.reuse-notes", "B file", 3, false),
    )
    .await;
    assert_eq!(reopened["pid"], initial["pid"]);
    assert_eq!(reopened["open"], json!({"b.reuse-notes":"B file"}));
    let calls = std::fs::read_to_string(root.join("calls")).unwrap();
    let starts = calls
        .lines()
        .filter(|line| serde_json::from_str::<Value>(line).unwrap()["method"] == "initialize")
        .count();
    assert_eq!(starts, 1);
    for path in ["a.reuse-notes", "b.reuse-notes"] {
        assert_eq!(std::fs::read_to_string(root.join(path)).unwrap(), "disk");
    }
}
