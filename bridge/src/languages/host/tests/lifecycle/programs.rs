use super::*;
use std::os::unix::fs::PermissionsExt;

fn executable(path: &std::path::Path, contents: &[u8]) {
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}
fn executable_peer(root: &std::path::Path) -> Scope {
    let mut scope = fixture(root);
    let peer = root.join("peer.py");
    let contents = format!(
        "#!/usr/bin/env python3\n{}",
        std::fs::read_to_string(&peer).unwrap()
    );
    executable(&peer, contents.as_bytes());
    scope.server.command = "./peer.py".into();
    scope.server.args.clear();
    scope
}
fn replace(path: &std::path::Path) {
    let next = path.with_extension("replacement");
    std::fs::copy(path, &next).unwrap();
    std::fs::rename(next, path).unwrap();
}
async fn wait_for(root: &std::path::Path, name: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.join(name).exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn replaced_or_rewritten_server_rejects_warm_requests_and_reopens_drafts() {
    for atomic in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let scope = executable_peer(root.path());
        let host = Host::default();
        let mut input = request();
        let first = host.query(scope.clone(), &input).await.unwrap();
        input.request += 1;
        let warm = host.query(scope.clone(), &input).await.unwrap();
        assert_eq!(first.server, warm.server);
        let peer = root.path().join("peer.py");
        if atomic {
            replace(&peer);
        } else {
            let content = std::fs::read_to_string(&peer).unwrap();
            std::fs::write(&peer, format!("{content}\n# Updated server\n")).unwrap();
        }
        input.request += 1;
        let error = host.query(scope.clone(), &input).await.unwrap_err();
        assert!(error.contains("executable changed"), "{error}");
        input.request += 1;
        let fresh = host.query(scope, &input).await.unwrap();
        assert_ne!(fresh.server, first.server);
        let calls = std::fs::read_to_string(root.path().join("calls")).unwrap();
        assert_eq!(calls.matches("\"method\": \"initialize\"").count(), 2);
        let opens = calls
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|message| message["method"] == "textDocument/didOpen")
            .collect::<Vec<_>>();
        assert_eq!(opens.len(), 2);
        assert!(
            opens
                .iter()
                .all(|message| message["params"]["textDocument"]["text"] == "draft")
        );
        host.shutdown();
    }
}

#[tokio::test]
async fn executable_changes_during_startup_or_analysis_reject_the_reply() {
    for startup in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let scope = executable_peer(root.path());
        let host = Host::default();
        let mut input = request();
        let (hold, started, release) = if startup {
            ("wait", "calls", "release")
        } else {
            host.query(scope.clone(), &input).await.unwrap();
            input.request += 1;
            input.query = Query::Hover {
                position: protocol::Position::default(),
            };
            ("hold-hover", "hover-started", "release-hover")
        };
        std::fs::write(root.path().join(hold), "").unwrap();
        let pending_host = host.clone();
        let pending_scope = scope.clone();
        let pending_input = input.clone();
        let pending =
            tokio::spawn(async move { pending_host.query(pending_scope, &pending_input).await });
        wait_for(root.path(), started).await;
        replace(&root.path().join("peer.py"));
        std::fs::write(root.path().join(release), "").unwrap();
        let error = pending.await.unwrap().unwrap_err();
        assert!(error.contains("executable changed"), "{error}");
        if startup {
            let until = host
                .inner
                .entries
                .lock()
                .unwrap()
                .values()
                .next()
                .unwrap()
                .failure
                .lock()
                .unwrap()
                .until
                .unwrap();
            tokio::time::sleep_until(tokio::time::Instant::from_std(until)).await;
        }
        input.request += 1;
        host.query(scope, &input).await.unwrap();
        host.shutdown();
    }
}

#[tokio::test]
async fn launch_alias_is_preserved_and_retargeting_it_invalidates_the_service() {
    let root = tempfile::tempdir().unwrap();
    let mut scope = executable_peer(root.path());
    let peer = root.path().join("peer.py");
    let content = std::fs::read_to_string(&peer).unwrap();
    let (shebang, rest) = content.split_once('\n').unwrap();
    executable(&peer, format!("{shebang}\nimport pathlib,sys\nassert pathlib.Path(sys.argv[0]).name=='alias-lsp'\n{rest}").as_bytes());
    let alias = root.path().join("alias-lsp");
    std::os::unix::fs::symlink("peer.py", &alias).unwrap();
    scope.server.command = "./alias-lsp".into();
    let host = Host::default();
    let mut input = request();
    let before = host.query(scope.clone(), &input).await.unwrap();
    std::fs::copy(&peer, root.path().join("other-peer.py")).unwrap();
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink("other-peer.py", &alias).unwrap();
    input.request += 1;
    assert!(
        host.query(scope.clone(), &input)
            .await
            .unwrap_err()
            .contains("executable changed")
    );
    input.request += 1;
    assert_ne!(
        host.query(scope, &input).await.unwrap().server,
        before.server
    );
    host.shutdown();
}

#[tokio::test]
async fn replaced_launch_links_invalidate_even_when_the_final_executable_is_unchanged() {
    for shape in ["direct", "parent", "intermediate"] {
        let root = tempfile::tempdir().unwrap();
        let mut scope = executable_peer(root.path());
        let (command, alias, target) = match shape {
            "parent" => ("./bin/peer.py", "bin", "."),
            "intermediate" => {
                std::os::unix::fs::symlink("intermediate", root.path().join("launch")).unwrap();
                ("./launch", "intermediate", "peer.py")
            }
            _ => ("./launch", "launch", "peer.py"),
        };
        let alias = root.path().join(alias);
        std::os::unix::fs::symlink(target, &alias).unwrap();
        scope.server.command = command.into();
        let host = Host::default();
        let mut input = request();
        let first = host.query(scope.clone(), &input).await.unwrap();
        // Ordinary directory activity does not replace an executable or any of
        // its launch links and must keep the warm service and unsaved overlay.
        std::fs::write(root.path().join("unrelated"), "new sibling").unwrap();
        input.request += 1;
        assert_eq!(
            host.query(scope.clone(), &input).await.unwrap().server,
            first.server,
            "{shape}"
        );
        let selected = root.path().join(command);
        let before = std::fs::canonicalize(&selected).unwrap();
        let executable = std::fs::read(&selected).unwrap();
        // Publish a distinct link to precisely the same target. Retaining only
        // the final executable's inode, bytes and canonical path misses this.
        let replacement = root.path().join("replacement-link");
        std::os::unix::fs::symlink(target, &replacement).unwrap();
        std::fs::rename(&replacement, &alias).unwrap();
        assert_eq!(std::fs::canonicalize(&selected).unwrap(), before);
        assert_eq!(std::fs::read(&selected).unwrap(), executable);
        input.request += 1;
        let error = host.query(scope.clone(), &input).await.unwrap_err();
        assert!(error.contains("executable changed"), "{shape}: {error}");
        input.request += 1;
        assert_ne!(
            host.query(scope, &input).await.unwrap().server,
            first.server,
            "{shape}"
        );
        host.shutdown();
    }
}

#[tokio::test]
async fn replaced_interpreter_is_detected_behind_an_unchanged_launch_script() {
    let root = tempfile::tempdir().unwrap();
    let mut scope = fixture(root.path());
    let interpreter = root.path().join("interpreter");
    let source = root.path().join("interpreter.c");
    std::fs::write(
        &source,
        include_str!("../../../../../../tools/fixtures/forwarding_interpreter.c"),
    )
    .unwrap();
    let compiled = std::process::Command::new("cc")
        .arg(&source)
        .arg("-o")
        .arg(&interpreter)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let python = which::which("python3").unwrap();
    let wrapper = root.path().join("wrapper");
    let contents = format!(
        "#!{} {}\n{}",
        interpreter.display(),
        python.display(),
        include_str!("../../../../../../tools/fixtures/lifecycle_server.py")
    );
    executable(&wrapper, contents.as_bytes());
    scope.server.command = "./wrapper".into();
    scope.server.args.clear();
    let host = Host::default();
    let mut input = request();
    let first = host.query(scope.clone(), &input).await.unwrap();
    replace(&interpreter);
    assert_eq!(std::fs::read_to_string(wrapper).unwrap(), contents);
    input.request += 1;
    let error = host.query(scope.clone(), &input).await.unwrap_err();
    assert!(error.contains("executable changed"), "{error}");
    input.request += 1;
    assert_ne!(
        host.query(scope, &input).await.unwrap().server,
        first.server
    );
    host.shutdown();
}
