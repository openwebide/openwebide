//! The same installed package and cancellation contract on both host routes.
use super::*;
use openwebide_core::plugins::languages::intelligence::{
    self, HostRequest, Position, PrefetchKind, Query, Request, Response, Selection,
};
use openwebide_core::plugins::languages::source_digest;
use serde_json::{Value, json};

async fn call(port: u16, paired: bool, scope: &HostRequest, input: &Request) -> HttpResponse {
    let mut request = scope.clone();
    request.selection.input = input.clone();
    preparation_request(
        port,
        paired,
        "/plugins/languages/query",
        serde_json::to_value(request).unwrap(),
    )
    .await
}

async fn marked(root: &Path, name: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !std::fs::metadata(root.join(name)).is_ok_and(|metadata| metadata.len() > 0) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

pub(super) async fn check(port: u16, paired: bool, prepared: &PreparedPlugin, workspace: &Path) {
    let root = workspace.join(if paired {
        "prefetch-paired"
    } else {
        "prefetch-remote"
    });
    std::fs::create_dir(&root).unwrap();
    for path in ["a.prefetch-notes", "b.prefetch-notes", "c.prefetch-notes"] {
        std::fs::write(root.join(path), "disk").unwrap();
    }
    for path in ["symbols-started", "symbols-cancelled"] {
        std::fs::write(root.join(path), "").unwrap();
    }
    let mut input = Request {
        version: intelligence::VERSION,
        client: "A".into(),
        document: source_digest("a.prefetch-notes"),
        request: 1,
        revision: 1,
        language: "prefetch-notes".into(),
        path: "a.prefetch-notes".into(),
        source: "draft 文😀".into(),
        source_digest: source_digest("draft 文😀"),
        query: Query::Prefetch {
            kind: PrefetchKind::Symbols,
            close: false,
        },
    };
    let scope = HostRequest {
        user: Some(42),
        root: root.to_string_lossy().into(),
        selection: Selection {
            user: 42,
            project: 103,
            prepared: prepared.clone(),
            input: input.clone(),
        },
    };
    let cold = call(port, paired, &scope, &input).await;
    assert_ne!(cold.status, 200);
    assert!(
        cold.body.contains("existing editing-client"),
        "{}",
        cold.body
    );
    assert!(!root.join("calls").exists());
    input.query = Query::Hover {
        position: Position::default(),
    };
    input.request += 1;
    let warm = call(port, paired, &scope, &input).await;
    assert_eq!(warm.status, 200, "{}", warm.body);
    let original: Response = serde_json::from_str(&warm.body).unwrap();
    for kind in [PrefetchKind::Symbols, PrefetchKind::Diagnostics] {
        input.request += 1;
        input.query = Query::Prefetch { kind, close: false };
        let result = call(port, paired, &scope, &input).await;
        assert_eq!(result.status, 200, "{}", result.body);
        let response: Response = serde_json::from_str(&result.body).unwrap();
        response.validate(&input).unwrap();
        assert_eq!(response.server, original.server);
        if kind == PrefetchKind::Diagnostics {
            assert!(response.diagnostics_ready);
        } else {
            assert_eq!(response.result[0]["name"], "Item");
        }
    }
    input.query = Query::Hover {
        position: Position::default(),
    };
    std::fs::write(root.join("hold-symbols"), "").unwrap();
    let pending = Request {
        document: source_digest("b.prefetch-notes"),
        path: "b.prefetch-notes".into(),
        request: 5,
        query: Query::Prefetch {
            kind: PrefetchKind::Symbols,
            close: false,
        },
        ..input.clone()
    };
    let retained = scope.clone();
    let slow = tokio::spawn(async move { call(port, paired, &retained, &pending).await });
    marked(&root, "symbols-started").await;
    let extra = Request {
        document: source_digest("c.prefetch-notes"),
        path: "c.prefetch-notes".into(),
        request: 6,
        query: Query::Prefetch {
            kind: PrefetchKind::Diagnostics,
            close: false,
        },
        ..input.clone()
    };
    let deferred = call(port, paired, &scope, &extra).await;
    assert_ne!(deferred.status, 200);
    assert!(deferred.body.contains("deferred"), "{}", deferred.body);
    input.request = 7;
    let foreground = call(port, paired, &scope, &input).await;
    assert_eq!(foreground.status, 200, "{}", foreground.body);
    let resumed: Response = serde_json::from_str(&foreground.body).unwrap();
    resumed.validate(&input).unwrap();
    assert_eq!(original.server, resumed.server);
    let sources: Value =
        serde_json::from_str(resumed.result["contents"].as_str().unwrap()).unwrap();
    assert_eq!(
        sources,
        json!({"a.prefetch-notes":"draft 文😀","b.prefetch-notes":"draft 文😀"})
    );
    let cancelled = tokio::time::timeout(std::time::Duration::from_secs(5), slow)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(cancelled.status, 200);
    assert!(cancelled.body.contains("superseded"), "{}", cancelled.body);
    marked(&root, "symbols-cancelled").await;
    std::fs::remove_file(root.join("hold-symbols")).unwrap();
    for kind in [PrefetchKind::Symbols, PrefetchKind::Diagnostics] {
        input.request += 1;
        input.query = Query::Prefetch { kind, close: false };
        let result = call(port, paired, &scope, &input).await;
        assert_ne!(result.status, 200);
        assert!(result.body.contains("suspended"), "{}", result.body);
    }
    input.request += 1;
    input.query = Query::Hover {
        position: Position::default(),
    };
    let ordinary = call(port, paired, &scope, &input).await;
    assert_eq!(ordinary.status, 200, "{}", ordinary.body);
    assert_eq!(
        serde_json::from_str::<Response>(&ordinary.body)
            .unwrap()
            .server,
        original.server
    );
    let calls = std::fs::read_to_string(root.join("calls")).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|line| serde_json::from_str::<Value>(line).unwrap()["method"] == "initialize")
            .count(),
        1
    );
    for path in ["a.prefetch-notes", "b.prefetch-notes", "c.prefetch-notes"] {
        assert_eq!(std::fs::read_to_string(root.join(path)).unwrap(), "disk");
    }
    closed_files(port, paired, prepared, workspace).await;
    generations(port, paired, prepared, workspace).await;
}

async fn closed_files(port: u16, paired: bool, prepared: &PreparedPlugin, workspace: &Path) {
    let root = workspace.join(format!("closed-prefetch-{paired}"));
    std::fs::create_dir(&root).unwrap();
    for path in ["a.prefetch-notes", "b.prefetch-notes", "c.prefetch-notes"] {
        std::fs::write(root.join(path), "disk").unwrap();
    }
    for marker in ["symbols-started", "symbols-cancelled"] {
        std::fs::write(root.join(marker), "").unwrap();
    }
    let mut input = Request {
        version: intelligence::VERSION,
        client: "closed-client".into(),
        document: source_digest("a.prefetch-notes"),
        request: 1,
        revision: 1,
        language: "prefetch-notes".into(),
        path: "a.prefetch-notes".into(),
        source: "A unsaved 文😀".into(),
        source_digest: source_digest("A unsaved 文😀"),
        query: Query::Hover {
            position: Position::default(),
        },
    };
    let scope = HostRequest {
        user: Some(42),
        root: root.to_string_lossy().into(),
        selection: Selection {
            user: 42,
            project: 105,
            prepared: prepared.clone(),
            input: input.clone(),
        },
    };
    let cold = Request {
        query: Query::Prefetch {
            kind: PrefetchKind::Symbols,
            close: true,
        },
        ..input.clone()
    };
    let declined = call(port, paired, &scope, &cold).await;
    assert_ne!(declined.status, 200);
    assert!(
        declined.body.contains("existing editing-client"),
        "{}",
        declined.body
    );
    assert!(!root.join("calls").exists());
    let warm = call(port, paired, &scope, &input).await;
    assert_eq!(warm.status, 200, "{}", warm.body);
    let original = serde_json::from_str::<Response>(&warm.body).unwrap().server;
    let mut closed = Request {
        document: source_digest("b.prefetch-notes"),
        path: "b.prefetch-notes".into(),
        source: "fresh closed source 文😀".into(),
        source_digest: source_digest("fresh closed source 文😀"),
        ..input.clone()
    };
    for kind in [PrefetchKind::Symbols, PrefetchKind::Diagnostics] {
        closed.request += 1;
        closed.query = Query::Prefetch { kind, close: true };
        let response = call(port, paired, &scope, &closed).await;
        assert_eq!(response.status, 200, "{}", response.body);
        let response: Response = serde_json::from_str(&response.body).unwrap();
        response.validate(&closed).unwrap();
        assert!(response.context.is_none());
        assert_eq!(response.server, original);
    }
    input.request = 4;
    let response = call(port, paired, &scope, &input).await;
    assert_eq!(response.status, 200, "{}", response.body);
    let response: Response = serde_json::from_str(&response.body).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(response.result["contents"].as_str().unwrap()).unwrap(),
        json!({"a.prefetch-notes":"A unsaved 文😀"})
    );
    std::fs::write(root.join("hold-symbols"), "hold").unwrap();
    closed.request = 5;
    closed.query = Query::Prefetch {
        kind: PrefetchKind::Symbols,
        close: true,
    };
    let (retained, pending) = (scope.clone(), closed.clone());
    let slow = tokio::spawn(async move { call(port, paired, &retained, &pending).await });
    marked(&root, "symbols-started").await;
    closed.request = 6;
    closed.query = Query::Hover {
        position: Position::default(),
    };
    closed.source = "reopened unsaved 文😀".into();
    closed.source_digest = source_digest(&closed.source);
    let reopened = call(port, paired, &scope, &closed).await;
    assert_eq!(reopened.status, 200, "{}", reopened.body);
    let reopened: Response = serde_json::from_str(&reopened.body).unwrap();
    let expected =
        json!({"a.prefetch-notes":"A unsaved 文😀","b.prefetch-notes":"reopened unsaved 文😀"});
    assert_eq!(
        serde_json::from_str::<Value>(reopened.result["contents"].as_str().unwrap()).unwrap(),
        expected
    );
    assert_eq!(reopened.server, original);
    let cancelled = tokio::time::timeout(std::time::Duration::from_secs(5), slow)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(cancelled.status, 200);
    assert!(cancelled.body.contains("superseded"), "{}", cancelled.body);
    input.request = 7;
    let response = call(port, paired, &scope, &input).await;
    assert_eq!(response.status, 200, "{}", response.body);
    let response: Response = serde_json::from_str(&response.body).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(response.result["contents"].as_str().unwrap()).unwrap(),
        expected
    );
    let calls = std::fs::read_to_string(root.join("calls")).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|line| serde_json::from_str::<Value>(line).unwrap()["method"] == "initialize")
            .count(),
        1
    );
    for path in ["a.prefetch-notes", "b.prefetch-notes", "c.prefetch-notes"] {
        assert_eq!(std::fs::read_to_string(root.join(path)).unwrap(), "disk");
    }
}

async fn generations(port: u16, paired: bool, prepared: &PreparedPlugin, workspace: &Path) {
    for (close, changed) in [false, true]
        .into_iter()
        .flat_map(|close| [false, true].map(|changed| (close, changed)))
    {
        let root = workspace.join(format!("generation-{paired}-{close}-{changed}"));
        std::fs::create_dir(&root).unwrap();
        for path in ["a.prefetch-notes", "b.prefetch-notes", "c.prefetch-notes"] {
            std::fs::write(root.join(path), "disk").unwrap();
        }
        for name in ["symbols-started", "symbols-cancelled"] {
            std::fs::write(root.join(name), "").unwrap();
        }
        let mut input = Request {
            version: intelligence::VERSION,
            client: "generation-client".into(),
            document: source_digest("a.prefetch-notes"),
            request: 1,
            revision: 1,
            language: "prefetch-notes".into(),
            path: "a.prefetch-notes".into(),
            source: "draft 文😀".into(),
            source_digest: source_digest("draft 文😀"),
            query: Query::Hover {
                position: Position::default(),
            },
        };
        let scope = HostRequest {
            user: Some(42),
            root: root.to_string_lossy().into(),
            selection: Selection {
                user: 42,
                project: 104,
                prepared: prepared.clone(),
                input: input.clone(),
            },
        };
        let warm = call(port, paired, &scope, &input).await;
        assert_eq!(warm.status, 200, "{}", warm.body);
        let identity = serde_json::from_str::<Response>(&warm.body).unwrap().server;
        std::fs::write(root.join("hold-symbols"), "").unwrap();
        input.document = source_digest("b.prefetch-notes");
        input.path = "b.prefetch-notes".into();
        input.request = 2;
        input.query = Query::Prefetch {
            kind: PrefetchKind::Symbols,
            close,
        };
        let (retained, request) = (scope.clone(), input.clone());
        let pending = tokio::spawn(async move { call(port, paired, &retained, &request).await });
        marked(&root, "symbols-started").await;
        let dependency = root.join("c.prefetch-notes");
        let modified = std::fs::metadata(&dependency).unwrap().modified().unwrap();
        if changed {
            std::fs::write(&dependency, "temporary").unwrap();
            std::fs::write(&dependency, "disk").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&dependency)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(modified))
                .unwrap();
        }
        // Release lives outside the observed project; the changed case must
        // fail because of the dependency, not because of fixture coordination.
        let release = root.parent().unwrap().join(format!(
            "{}.release-symbols",
            root.file_name().unwrap().to_str().unwrap()
        ));
        std::fs::write(&release, "").unwrap();
        let response = tokio::time::timeout(std::time::Duration::from_secs(5), pending)
            .await
            .unwrap()
            .unwrap();
        if changed {
            assert_ne!(response.status, 200);
            assert!(
                response.body.contains("Project files changed"),
                "{}",
                response.body
            );
        } else {
            assert_eq!(response.status, 200, "{}", response.body);
            assert_eq!(
                serde_json::from_str::<Response>(&response.body)
                    .unwrap()
                    .result[0]["name"],
                "Item"
            );
        }
        std::fs::remove_file(release).unwrap();
        std::fs::remove_file(root.join("hold-symbols")).unwrap();
        input.request = 3;
        let retry = call(port, paired, &scope, &input).await;
        assert_eq!(retry.status, 200, "{}", retry.body);
        assert_eq!(
            serde_json::from_str::<Response>(&retry.body)
                .unwrap()
                .server,
            identity
        );
        for path in ["a.prefetch-notes", "b.prefetch-notes", "c.prefetch-notes"] {
            assert_eq!(std::fs::read_to_string(root.join(path)).unwrap(), "disk");
        }
        assert_eq!(
            std::fs::metadata(dependency).unwrap().modified().unwrap(),
            modified
        );
    }
}
