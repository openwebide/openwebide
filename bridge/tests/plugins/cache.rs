use super::*;
use openwebide_core::plugins::languages::{
    intelligence::{self, HostRequest, Position, Query, Request, Response, Selection},
    source_digest,
};
use serde_json::{Value, json};

async fn query(port: u16, paired: bool, scope: &HostRequest) -> Value {
    let response = preparation_request(
        port,
        paired,
        "/plugins/languages/query",
        serde_json::to_value(scope).unwrap(),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body);
    let response: Response = serde_json::from_str(&response.body).unwrap();
    response.validate(&scope.selection.input).unwrap();
    serde_json::from_str(response.result["contents"].as_str().unwrap()).unwrap()
}
pub(super) async fn check(port: u16, paired: bool, prepared: &PreparedPlugin, workspace: &Path) {
    let root = workspace.join(if paired {
        "cache-paired"
    } else {
        "cache-remote"
    });
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("main.cache-notes"), "disk source").unwrap();
    let mut scope = HostRequest {
        user: Some(42),
        root: root.to_string_lossy().into(),
        selection: Selection {
            user: 42,
            project: 202,
            prepared: prepared.clone(),
            input: Request {
                version: intelligence::VERSION,
                client: "client".into(),
                document: "file".into(),
                request: 1,
                revision: 1,
                language: "cache-notes".into(),
                path: "main.cache-notes".into(),
                source: "dirty source".into(),
                source_digest: source_digest("dirty source"),
                query: Query::Hover {
                    position: Position::default(),
                },
            },
        },
    };
    let first = query(port, paired, &scope).await;
    assert_eq!(first["hit"], false);
    let path = std::path::PathBuf::from(first["cache"].as_str().unwrap());
    assert!(!path.starts_with(root.canonicalize().unwrap()));
    assert_eq!(
        first["options"]["directory"],
        path.parent().unwrap().join("options").to_str().unwrap()
    );
    assert_eq!(
        first["settings"]["directory"],
        path.parent().unwrap().join("settings").to_str().unwrap()
    );
    let revoked = preparation_request(
        port,
        paired,
        "/plugins/languages/revoke",
        json!({"user":42,"project":202,"provider":prepared.storage_namespace()}),
    )
    .await;
    assert_eq!(revoked.status, 200, "{}", revoked.body);
    scope.selection.input.request += 1;
    let restarted = query(port, paired, &scope).await;
    assert_eq!(restarted["hit"], true);
    assert_eq!(restarted["cache"], first["cache"]);
    assert_eq!(
        std::fs::read_to_string(root.join("main.cache-notes")).unwrap(),
        "disk source"
    );
    assert!(!root.join("build").exists());
}
