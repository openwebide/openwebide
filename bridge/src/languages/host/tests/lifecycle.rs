use super::*;
#[cfg(unix)]
mod programs;

pub(super) fn request() -> Request {
    Request {
        version: protocol::VERSION,
        client: "client".into(),
        document: "document".into(),
        request: 1,
        revision: 1,
        language: "notes".into(),
        path: "main.notes".into(),
        source: "draft".into(),
        source_digest: openwebide_core::plugins::languages::source_digest("draft"),
        query: Query::Synchronize,
    }
}

fn git(root: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.org",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn scope(root: &std::path::Path, command: &str) -> Scope {
    Scope {
        owner: "owner".into(),
        project: 1,
        root: root.into(),
        provider: "community/notes".into(),
        digest: "a".repeat(64),
        server: serde_json::from_value(json!({"command":command})).unwrap(),
    }
}

fn fixture(root: &std::path::Path) -> Scope {
    let script = root.join("peer.py");
    std::fs::write(
        &script,
        include_str!("../../../../../tools/fixtures/lifecycle_server.py"),
    )
    .unwrap();
    let mut scope = scope(root, "python3");
    scope.server.args = vec![script.to_str().unwrap().into()];
    scope
}

#[tokio::test]
async fn cold_close_does_not_allocate_a_service_or_retry_a_failed_start() {
    let root = tempfile::tempdir().unwrap();
    let scope = scope(root.path(), "openwebide-nonexistent-lifecycle-server");
    let host = Host::default();
    let mut input = request();
    input.path = "deleted/parent/main.notes".into();
    input.query = Query::Close;
    let closed = host.query(scope.clone(), &input).await.unwrap();
    closed.validate(&input).unwrap();
    assert_eq!(closed.server, "closed");
    assert!(host.inner.entries.lock().unwrap().is_empty());
    input.path = "main.notes".into();
    input.query = Query::Synchronize;
    assert!(host.query(scope.clone(), &input).await.is_err());
    input.request += 1;
    input.query = Query::Close;
    host.query(scope, &input).await.unwrap();
    let entries = host.inner.entries.lock().unwrap();
    assert_eq!(entries.len(), 1);
    let entry = entries.values().next().unwrap();
    assert!(entry.session.get().is_none());
    assert_eq!(entry.failure.lock().unwrap().count, 1);
    assert!(!entry.busy());
}

#[tokio::test]
async fn close_cancels_startup_without_restarting_and_rejects_late_requests() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("wait"), "").unwrap();
    let scope = fixture(root.path());
    let host = Host::default();
    let input = request();
    let starting_host = host.clone();
    let starting_scope = scope.clone();
    let starting_input = input.clone();
    let pending =
        tokio::spawn(async move { starting_host.query(starting_scope, &starting_input).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.path().join("calls").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut close = input.clone();
    close.request += 1;
    close.query = Query::Close;
    host.query(scope.clone(), &close)
        .await
        .unwrap()
        .validate(&close)
        .unwrap();
    assert!(pending.await.unwrap().unwrap_err().contains("superseded"));
    assert!(
        host.query(scope.clone(), &input)
            .await
            .unwrap_err()
            .contains("stale")
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("calls"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    {
        let entries = host.inner.entries.lock().unwrap();
        let entry = entries.values().next().unwrap();
        assert!(entry.session.get().is_none());
        assert!(entry.starting.lock().unwrap().is_none());
        assert!(!entry.busy());
    }
    std::fs::write(root.path().join("release"), "").unwrap();
    let mut reopened = input;
    reopened.request = 3;
    host.query(scope, &reopened).await.unwrap();
    host.shutdown();
}

#[tokio::test]
async fn close_uses_retained_workspace_after_marker_removal_and_preserves_other_clients() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("nested")).unwrap();
    std::fs::write(root.path().join("nested/project.json"), "{}").unwrap();
    let mut scope = fixture(root.path());
    scope.server.root_markers = vec!["project.json".into()];
    let host = Host::default();
    let mut input = request();
    input.path = "nested/main.notes".into();
    host.query(scope.clone(), &input).await.unwrap();
    let mut other = input.clone();
    other.client = "other-client".into();
    host.query(scope.clone(), &other).await.unwrap();
    input.request += 1;
    input.query = Query::Close;
    for boundary in ["owner", "project", "provider", "digest", "config", "client"] {
        let mut foreign_scope = scope.clone();
        let mut foreign_input = input.clone();
        match boundary {
            "owner" => foreign_scope.owner = "another-owner".into(),
            "project" => foreign_scope.project = 2,
            "provider" => foreign_scope.provider = "community/other".into(),
            "digest" => foreign_scope.digest = "b".repeat(64),
            "config" => foreign_scope.server.settings = json!({"different":true}),
            "client" => foreign_input.client = "unknown-client".into(),
            _ => unreachable!(),
        }
        let response = host.query(foreign_scope, &foreign_input).await.unwrap();
        assert_eq!(response.server, "closed", "{boundary}");
        assert_eq!(host.inner.entries.lock().unwrap().len(), 2, "{boundary}");
    }
    std::fs::remove_dir_all(root.path().join("nested")).unwrap();
    let closed = host.query(scope.clone(), &input).await.unwrap();
    assert_eq!(closed.server, "closed");
    let entries = host
        .inner
        .entries
        .lock()
        .unwrap()
        .iter()
        .map(|(key, entry)| (key.clone(), entry.clone()))
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 2);
    for (key, entry) in entries {
        let documents = crate::languages::host::reuse::snapshot(&entry).unwrap();
        assert_eq!(
            documents
                .workspace_documents(&scope.root.canonicalize().unwrap())
                .len(),
            usize::from(key.client != input.client)
        );
        if let Some(session) = entry.session.get() {
            session
                .client
                .request("test/barrier", json!({}), REQUEST_TIMEOUT)
                .await
                .unwrap();
        }
    }
    let calls = std::fs::read_to_string(root.path().join("calls")).unwrap();
    assert_eq!(calls.matches("\"method\": \"initialize\"").count(), 1);
    assert_eq!(
        calls
            .matches("\"method\": \"textDocument/didClose\"")
            .count(),
        1
    );
    host.shutdown();
}

#[tokio::test]
async fn close_releases_all_retained_workspaces_after_discovery_changes() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("nested")).unwrap();
    let marker = root.path().join("nested/project.json");
    std::fs::write(&marker, "{}").unwrap();
    let mut scope = fixture(root.path());
    scope.server.root_markers = vec!["project.json".into()];
    let host = Host::default();
    let mut input = request();
    input.path = "nested/main.notes".into();
    let first = host.query(scope.clone(), &input).await.unwrap();
    std::fs::remove_file(marker).unwrap();
    input.request += 1;
    let second = host.query(scope.clone(), &input).await.unwrap();
    assert_ne!(first.server, second.server);
    input.request += 1;
    input.query = Query::Close;
    host.query(scope, &input).await.unwrap();
    let entries = host
        .inner
        .entries
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 2);
    for entry in entries {
        let session = entry.session.get().unwrap();
        assert!(
            session
                .documents
                .lock()
                .await
                .workspace_documents(&session.root)
                .is_empty()
        );
        session
            .client
            .request("test/barrier", json!({}), REQUEST_TIMEOUT)
            .await
            .unwrap();
    }
    let calls = std::fs::read_to_string(root.path().join("calls")).unwrap();
    assert_eq!(calls.matches("\"method\": \"initialize\"").count(), 2);
    assert_eq!(
        calls
            .matches("\"method\": \"textDocument/didClose\"")
            .count(),
        2
    );
    host.shutdown();
}

#[tokio::test]
async fn replaced_project_and_nested_workspace_stop_old_queries_before_resynchronizing() {
    for nested in [false, true] {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let mut scope = fixture(&root);
        let mut input = request();
        let replaced = if nested {
            std::fs::create_dir(root.join("nested")).unwrap();
            std::fs::write(root.join("nested/project.json"), "{}").unwrap();
            scope.server.root_markers = vec!["project.json".into()];
            input.path = "nested/main.notes".into();
            root.join("nested")
        } else {
            root.clone()
        };
        let host = Host::default();
        let original = host.query(scope.clone(), &input).await.unwrap();
        let old = host
            .inner
            .entries
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .clone();
        std::fs::rename(&replaced, parent.path().join("replaced")).unwrap();
        std::fs::create_dir(&replaced).unwrap();
        if nested {
            std::fs::write(replaced.join("project.json"), "{}").unwrap();
        } else {
            fixture(&root);
        }
        input.request += 1;
        let error = host.query(scope.clone(), &input).await.unwrap_err();
        assert!(
            error.contains(if nested {
                "physical language workspace"
            } else {
                "physical project folder"
            }),
            "{error}"
        );
        assert!(*old.stopped.borrow());
        input.request += 1;
        let fresh = host.query(scope, &input).await.unwrap();
        assert_ne!(original.server, fresh.server);
        fresh.validate(&input).unwrap();
        host.shutdown();
    }
}

#[tokio::test]
async fn replacement_during_a_request_rejects_the_old_services_reply() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let scope = fixture(&root);
    let host = Host::default();
    let mut input = request();
    host.query(scope.clone(), &input).await.unwrap();
    std::fs::write(root.join("hold-hover"), "").unwrap();
    input.request += 1;
    input.query = Query::Hover {
        position: protocol::Position::default(),
    };
    let pending_host = host.clone();
    let pending_input = input.clone();
    let pending = tokio::spawn(async move { pending_host.query(scope, &pending_input).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.join("hover-started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    std::fs::rename(&root, parent.path().join("replaced")).unwrap();
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("release-hover"), "").unwrap();
    let error = pending.await.unwrap().unwrap_err();
    assert!(error.contains("physical project folder"), "{error}");
    assert!(
        *host
            .inner
            .entries
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .stopped
            .borrow()
    );
    host.shutdown();
}

#[tokio::test]
async fn branch_and_commit_changes_invalidate_services_but_preserve_unsaved_sources() {
    let root = tempfile::tempdir().unwrap();
    let scope = fixture(root.path());
    let host = Host::default();
    let mut input = request();
    std::fs::write(root.path().join("main.notes"), "disk").unwrap();
    git(root.path(), &["init", "-q", "-b", "main"]);
    git(root.path(), &["add", "main.notes"]);
    git(root.path(), &["commit", "-qm", "initial"]);
    let mut previous = host.query(scope.clone(), &input).await.unwrap();
    for args in [
        vec!["checkout", "-q", "-b", "topic"],
        vec!["commit", "--allow-empty", "-qm", "next"],
    ] {
        git(root.path(), &args);
        input.request += 1;
        let error = host.query(scope.clone(), &input).await.unwrap_err();
        assert!(error.contains("Git revision changed"), "{error}");
        input.request += 1;
        let fresh = host.query(scope.clone(), &input).await.unwrap();
        assert_ne!(previous.server, fresh.server);
        previous = fresh;
        let entry = host
            .inner
            .entries
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .clone();
        let session = entry.session.get().unwrap();
        assert_eq!(
            session
                .documents
                .lock()
                .await
                .overlay_source(&session.root.join("main.notes")),
            Some("draft")
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("main.notes")).unwrap(),
            "disk"
        );
    }
    host.shutdown();
}

#[tokio::test]
async fn branch_switch_during_startup_or_query_rejects_old_analysis() {
    for startup in [true, false] {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q", "-b", "main"]);
        git(root.path(), &["commit", "--allow-empty", "-qm", "initial"]);
        let scope = fixture(root.path());
        let host = Host::default();
        let mut input = request();
        let (seen, release) = if startup {
            std::fs::write(root.path().join("wait"), "").unwrap();
            ("calls", "release")
        } else {
            host.query(scope.clone(), &input).await.unwrap();
            input.request += 1;
            input.query = Query::Hover {
                position: protocol::Position::default(),
            };
            std::fs::write(root.path().join("hold-hover"), "").unwrap();
            ("hover-started", "release-hover")
        };
        let pending_host = host.clone();
        let pending = tokio::spawn(async move { pending_host.query(scope, &input).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !root.path().join(seen).exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        git(root.path(), &["checkout", "-q", "-b", "topic"]);
        std::fs::write(root.path().join(release), "").unwrap();
        let error = pending.await.unwrap().unwrap_err();
        assert!(error.contains("Git revision changed"), "{error}");
        host.shutdown();
    }
}

#[tokio::test]
async fn declared_configuration_changes_restart_analysis_and_keep_excluded_writes_warm() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("settings.json"), "one").unwrap();
    let mut scope = fixture(root.path());
    scope.server.project_inputs = Some(
        serde_json::from_value(json!({"files":["settings.json"],"exclude":["ignored/**"]}))
            .unwrap(),
    );
    let host = Host::default();
    let mut input = request();
    input.query = Query::Hover {
        position: protocol::Position::default(),
    };
    let first = host.query(scope.clone(), &input).await.unwrap();
    assert_eq!(first.result["contents"], "one");
    std::fs::create_dir(root.path().join("ignored")).unwrap();
    std::fs::write(root.path().join("ignored/generated.json"), "new").unwrap();
    input.request += 1;
    assert_eq!(
        host.query(scope.clone(), &input).await.unwrap().server,
        first.server
    );
    std::fs::write(root.path().join("settings.json"), "two").unwrap();
    input.request += 1;
    assert!(
        host.query(scope.clone(), &input)
            .await
            .unwrap_err()
            .contains("Declared project inputs changed")
    );
    input.request += 1;
    let fresh = host.query(scope, &input).await.unwrap();
    assert_ne!(first.server, fresh.server);
    assert_eq!(fresh.result["contents"], "two");
    assert_eq!(input.source, "draft");
    host.shutdown();
}

#[tokio::test]
async fn configuration_changes_during_startup_and_a_read_reject_old_results() {
    for (startup, restore) in [(true, false), (false, false), (true, true), (false, true)] {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("settings.json"), "one").unwrap();
        let mut scope = fixture(root.path());
        scope.server.project_inputs =
            Some(serde_json::from_value(json!({"files":["settings.json"]})).unwrap());
        let host = Host::default();
        let mut input = request();
        let (seen, release) = if startup {
            std::fs::write(root.path().join("wait"), "").unwrap();
            ("calls", "release")
        } else {
            host.query(scope.clone(), &input).await.unwrap();
            input.request += 1;
            input.query = Query::Hover {
                position: protocol::Position::default(),
            };
            std::fs::write(root.path().join("hold-hover"), "").unwrap();
            ("hover-started", "release-hover")
        };
        let pending_host = host.clone();
        let pending = tokio::spawn(async move { pending_host.query(scope, &input).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !root.path().join(seen).exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        std::fs::write(root.path().join("settings.json"), "two").unwrap();
        if restore {
            std::fs::write(root.path().join("settings.json"), "one").unwrap();
        }
        std::fs::write(root.path().join(release), "").unwrap();
        let error = pending.await.unwrap().unwrap_err();
        assert!(error.contains("Declared project inputs changed"), "{error}");
        host.shutdown();
    }
}

#[tokio::test]
async fn branch_round_trip_during_startup_or_query_rejects_old_analysis() {
    for startup in [true, false] {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q", "-b", "main"]);
        git(root.path(), &["commit", "--allow-empty", "-qm", "initial"]);
        git(root.path(), &["config", "core.logAllRefUpdates", "false"]);
        let head = root.path().join(".git/HEAD");
        let head_bytes = std::fs::read(&head).unwrap();
        let modified = std::fs::metadata(&head).unwrap().modified().unwrap();
        let before = crate::exec::git::repository_revision(root.path())
            .await
            .unwrap();
        let scope = fixture(root.path());
        let host = Host::default();
        let mut input = request();
        let (seen, release) = if startup {
            std::fs::write(root.path().join("wait"), "").unwrap();
            ("calls", "release")
        } else {
            host.query(scope.clone(), &input).await.unwrap();
            input.request += 1;
            input.query = Query::Hover {
                position: protocol::Position::default(),
            };
            std::fs::write(root.path().join("hold-hover"), "").unwrap();
            ("hover-started", "release-hover")
        };
        let pending_host = host.clone();
        let pending = tokio::spawn(async move { pending_host.query(scope, &input).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !root.path().join(seen).exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        git(root.path(), &["checkout", "-q", "-b", "topic"]);
        git(root.path(), &["checkout", "-q", "main"]);
        std::fs::File::options()
            .write(true)
            .open(&head)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert_eq!(std::fs::read(&head).unwrap(), head_bytes);
        assert_eq!(
            std::fs::metadata(&head).unwrap().modified().unwrap(),
            modified
        );
        assert_eq!(
            crate::exec::git::repository_revision(root.path())
                .await
                .unwrap(),
            before
        );
        std::fs::write(root.path().join(release), "").unwrap();
        let error = pending.await.unwrap().unwrap_err();
        assert!(error.contains("Git revision changed"), "{error}");
        host.shutdown();
    }
}
