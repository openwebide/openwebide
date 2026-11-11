//! Signed analysis contexts through both authenticated public-package routes.
use super::*;
use openwebide_core::plugins::languages::{
    intelligence::{self, HostRequest, Query, Request, Response, Selection},
    source_digest,
};
use serde_json::{Value, json};

async fn post(port: u16, paired: bool, request: &HostRequest) -> HttpResponse {
    preparation_request(
        port,
        paired,
        "/plugins/languages/query",
        serde_json::to_value(request).unwrap(),
    )
    .await
}

pub(super) async fn check(port: u16, paired: bool, prepared: &PreparedPlugin, workspace: &Path) {
    let root = workspace.join(if paired {
        "analysis-paired"
    } else {
        "analysis-remote"
    });
    std::fs::create_dir(&root).unwrap();
    for path in ["a.reuse-notes", "b.reuse-notes"] {
        std::fs::write(root.join(path), "disk").unwrap();
    }
    let input = Request {
        version: intelligence::VERSION,
        client: "analysis-client".into(),
        document: "document".into(),
        request: 1,
        revision: 1,
        language: "reuse-notes".into(),
        path: "a.reuse-notes".into(),
        source: "unsaved 文😀".into(),
        source_digest: source_digest("unsaved 文😀"),
        query: Query::Hover {
            position: Default::default(),
        },
    };
    let mut request = HostRequest {
        user: Some(42),
        root: root.to_string_lossy().into(),
        selection: Selection {
            user: 42,
            project: 101,
            prepared: prepared.clone(),
            input,
        },
    };
    let response = post(port, paired, &request).await;
    assert_eq!(response.status, 200, "{}", response.body);
    let result: Response = serde_json::from_str(&response.body).unwrap();
    result.validate(&request.selection.input).unwrap();
    let context = result.context.unwrap();
    context.validate_result(&result.result).unwrap();
    let calls = std::fs::read(root.join("calls")).unwrap();
    request.selection.input.request += 1;
    request.selection.input.query = Query::ValidateAnalysis {
        context: context.clone(),
    };
    let response = post(port, paired, &request).await;
    assert_eq!(response.status, 200, "{}", response.body);
    let ack: Response = serde_json::from_str(&response.body).unwrap();
    ack.validate(&request.selection.input).unwrap();
    assert!(ack.result.is_null());
    assert_eq!(std::fs::read(root.join("calls")).unwrap(), calls);
    let mut invalid = context.clone();
    invalid.result_digest = "a".repeat(64);
    request.selection.input.request += 1;
    request.selection.input.query = Query::ValidateAnalysis { context: invalid };
    assert_eq!(post(port, paired, &request).await.status, 400);
    std::fs::write(root.join("b.reuse-notes"), "external dependency change").unwrap();
    request.selection.input.request += 1;
    request.selection.input.query = Query::ValidateAnalysis { context };
    assert_eq!(post(port, paired, &request).await.status, 400);
    let calls = std::fs::read_to_string(root.join("calls")).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(
                |line| serde_json::from_str::<Value>(line).unwrap()["method"]
                    == json!("initialize")
            )
            .count(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(root.join("a.reuse-notes")).unwrap(),
        "disk"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("b.reuse-notes")).unwrap(),
        "external dependency change"
    );
}
