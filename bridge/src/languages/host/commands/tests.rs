use super::*;
use protocol::{Position, Range, actions::origin_fingerprint, results::apply_edits};

const SOURCE: &str = "text 文😀\r\n";
const SERVER: &str = include_str!("../../../../../tools/fixtures/commands_server.py");
mod coordination;

struct Fixture {
    root: tempfile::TempDir,
    host: Host,
    scope: Scope,
    input: Request,
}
impl Fixture {
    async fn new(mode: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("main.notes"), SOURCE).unwrap();
        let script = root.path().join("peer.py");
        std::fs::write(&script, SERVER).unwrap();
        let scope = Scope {
            owner: "owner".into(),
            project: 1,
            root: root.path().into(),
            provider: "community/custom".into(),
            digest: "a".repeat(64),
            server: serde_json::from_value(json!({"command":"python3","args":[script,mode]}))
                .unwrap(),
        };
        let input = Request {
            version: protocol::VERSION,
            client: "client".into(),
            document: "document".into(),
            request: 1,
            revision: 1,
            language: "notes".into(),
            path: "main.notes".into(),
            source: SOURCE.into(),
            source_digest: source_digest(SOURCE),
            query: Query::CodeActions {
                range: Range {
                    start: Position::default(),
                    end: Position {
                        line: 0,
                        character: 4,
                    },
                },
            },
        };
        Self {
            root,
            host: Host::default(),
            scope,
            input,
        }
    }
    async fn review(&mut self) -> (ActionReview, CommandInvocation) {
        let actions = self
            .host
            .query(self.scope.clone(), &self.input)
            .await
            .unwrap();
        let id = actions.result[0]["id"].as_str().unwrap().to_string();
        let unsupported = actions.result[1]["id"].as_str().unwrap().to_string();
        self.input.request += 1;
        self.input.query = Query::ReviewAction { id: unsupported };
        assert!(
            self.host
                .query(self.scope.clone(), &self.input)
                .await
                .unwrap_err()
                .contains("did not advertise")
        );
        self.input.request += 1;
        self.input.query = Query::ReviewAction { id: id.clone() };
        let response = self
            .host
            .query(self.scope.clone(), &self.input)
            .await
            .unwrap();
        let review: ActionReview = serde_json::from_value(response.result).unwrap();
        let command = review.command.as_ref().unwrap();
        assert_eq!(command.server, response.server);
        assert_eq!(command.name, "fixture.apply");
        assert_eq!(self.executions(), 0);
        let invocation = CommandInvocation {
            id: "b".repeat(32),
            client: self.input.client.clone(),
            command: id,
            server: response.server,
            origin: origin_fingerprint(&self.input).unwrap(),
            review: review.fingerprint().unwrap(),
            approval: "c".repeat(64),
        };
        (review, invocation)
    }
    async fn apply_overlay(&mut self, review: &ActionReview) {
        self.input.source = apply_edits(&self.input.source, &review.edit.files[0].edits).unwrap();
        self.input.source_digest = source_digest(&self.input.source);
        self.input.revision += 1;
        self.input.request += 1;
        self.input.query = Query::Synchronize;
        self.host
            .query(self.scope.clone(), &self.input)
            .await
            .unwrap();
    }
    fn executions(&self) -> usize {
        std::fs::read_to_string(self.root.path().join("executions"))
            .map_or(0, |text| text.lines().count())
    }
    async fn finished(&self, invocation: &CommandInvocation) -> CommandOutcome {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let CommandProgress::Finished { outcome } = self
                    .host
                    .command_progress(self.scope.clone(), invocation)
                    .await
                    .unwrap()
                {
                    return outcome;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.host.shutdown();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn executable_replacement_rejects_an_approved_command_without_dispatch() {
    use std::os::unix::fs::PermissionsExt;
    let mut fixture = Fixture::new("success").await;
    let peer = fixture.root.path().join("peer.py");
    std::fs::write(&peer, format!("#!/usr/bin/env python3\n{SERVER}")).unwrap();
    std::fs::set_permissions(&peer, std::fs::Permissions::from_mode(0o700)).unwrap();
    fixture.scope.server.command = peer.to_string_lossy().into();
    fixture.scope.server.args = vec!["success".into()];
    let (review, invocation) = fixture.review().await;
    fixture.apply_overlay(&review).await;
    let replacement = fixture.root.path().join("replacement");
    std::fs::copy(&peer, &replacement).unwrap();
    std::fs::rename(replacement, &peer).unwrap();
    fixture
        .host
        .start_command(fixture.scope.clone(), invocation.clone())
        .await
        .unwrap();
    let outcome = fixture.finished(&invocation).await;
    assert!(
        matches!(outcome, CommandOutcome::Rejected {message} if message.contains("executable changed"))
    );
    assert!(
        fixture
            .host
            .inner
            .entries
            .lock()
            .unwrap()
            .values()
            .all(|entry| *entry.stopped.borrow())
    );
    assert_eq!(fixture.executions(), 0);
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("main.notes")).unwrap(),
        SOURCE
    );
}

#[cfg(unix)]
#[tokio::test]
async fn changed_context_after_source_verification_rejects_the_mutating_rpc() {
    use std::os::unix::fs::PermissionsExt;
    for replace_program in [false, true] {
        let mut fixture = Fixture::new("success").await;
        let peer = fixture.root.path().join("peer.py");
        std::fs::write(&peer, format!("#!/usr/bin/env python3\n{SERVER}")).unwrap();
        std::fs::set_permissions(&peer, std::fs::Permissions::from_mode(0o700)).unwrap();
        fixture.scope.server.command = peer.to_string_lossy().into();
        fixture.scope.server.args = vec!["success".into()];
        let (review, invocation) = fixture.review().await;
        fixture.apply_overlay(&review).await;
        let entry = fixture
            .host
            .inner
            .entries
            .lock()
            .unwrap()
            .values()
            .find(|entry| {
                entry
                    .session
                    .get()
                    .is_some_and(|session| session.id == invocation.server)
            })
            .unwrap()
            .clone();
        let session = entry.session.get().unwrap();
        let data = session.commands.lock().unwrap().slots[&invocation.command]
            .data
            .clone();
        let documents = session.documents.lock().await;
        let input_revision = session.inputs.revision().await;
        entry.check_context(session).await.unwrap();
        verify_sources(session, &documents, &data, input_revision)
            .await
            .unwrap();
        // The workspace was valid when its file verification completed. Change
        // native context at that boundary without relying on scheduler timing.
        if replace_program {
            let replacement = fixture.root.path().join("replacement");
            std::fs::copy(&peer, &replacement).unwrap();
            std::fs::rename(replacement, &peer).unwrap();
        } else {
            let status = std::process::Command::new("git")
                .args(["init", "-q", "-b", "main"])
                .current_dir(fixture.root.path())
                .status()
                .unwrap();
            assert!(status.success());
        }
        let outcome = dispatch(&entry, session, &data, input_revision).await;
        let expected = if replace_program {
            "executable changed"
        } else {
            "Git revision changed"
        };
        assert!(
            matches!(outcome, CommandOutcome::Rejected {ref message} if message.contains(expected)),
            "{outcome:?}"
        );
        assert!(*entry.stopped.borrow());
        assert_eq!(fixture.executions(), 0);
        assert_eq!(
            std::fs::read_to_string(fixture.root.path().join("main.notes")).unwrap(),
            SOURCE
        );
    }
}

#[tokio::test]
async fn command_reviews_do_not_execute_and_reject_incomplete_or_changed_edits() {
    let mut fixture = Fixture::new("success").await;
    let (review, invocation) = fixture.review().await;
    let mut forged = invocation.clone();
    forged.review = "d".repeat(64);
    assert!(
        fixture
            .host
            .start_command(fixture.scope.clone(), forged)
            .await
            .is_err()
    );
    fixture
        .host
        .start_command(fixture.scope.clone(), invocation.clone())
        .await
        .unwrap();
    assert!(matches!(
        fixture.finished(&invocation).await,
        CommandOutcome::Rejected { .. }
    ));
    assert_eq!(fixture.executions(), 0);
    fixture.apply_overlay(&review).await;
    assert!(matches!(
        fixture
            .host
            .start_command(fixture.scope.clone(), invocation.clone())
            .await
            .unwrap(),
        CommandProgress::Finished {
            outcome: CommandOutcome::Rejected { .. }
        }
    ));
    let mut replay = invocation;
    replay.id = "e".repeat(32);
    assert!(
        fixture
            .host
            .start_command(fixture.scope.clone(), replay)
            .await
            .is_err()
    );
    assert_eq!(fixture.executions(), 0);
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("main.notes")).unwrap(),
        SOURCE
    );
}

#[tokio::test]
async fn approved_commands_execute_once_survive_lost_dispatch_replies_and_never_use_read_retry() {
    for mode in ["success", "cancel"] {
        let mut fixture = Fixture::new(mode).await;
        let (review, invocation) = fixture.review().await;
        fixture.apply_overlay(&review).await;
        let mut other = fixture.scope.clone();
        other.owner = "other".into();
        assert!(
            fixture
                .host
                .start_command(other, invocation.clone())
                .await
                .is_err()
        );
        let mut other = fixture.scope.clone();
        other.project += 1;
        assert!(
            fixture
                .host
                .start_command(other, invocation.clone())
                .await
                .is_err()
        );
        assert_eq!(fixture.host.inner.entries.lock().unwrap().len(), 1);
        // Ignore the first dispatch result, as when an HTTP caller disconnects.
        drop(
            fixture
                .host
                .start_command(fixture.scope.clone(), invocation.clone())
                .await
                .unwrap(),
        );
        let outcome = fixture.finished(&invocation).await;
        if mode == "success" {
            assert!(matches!(&outcome,CommandOutcome::Completed {result} if result["marker"]==17));
        } else {
            assert!(matches!(&outcome, CommandOutcome::Uncertain { .. }));
        }
        assert_eq!(fixture.executions(), 1);
        assert_eq!(
            fixture
                .host
                .start_command(fixture.scope.clone(), invocation.clone())
                .await
                .unwrap(),
            CommandProgress::Finished { outcome }
        );
        assert_eq!(fixture.executions(), 1);
        assert_eq!(
            std::fs::read_to_string(fixture.root.path().join("main.notes")).unwrap(),
            SOURCE
        );
    }
}

#[tokio::test]
async fn stopping_a_running_command_does_not_restart_the_service_or_replay_its_effects() {
    let mut fixture = Fixture::new("block").await;
    let (review, invocation) = fixture.review().await;
    fixture.apply_overlay(&review).await;
    fixture
        .host
        .start_command(fixture.scope.clone(), invocation.clone())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.executions() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    fixture.host.shutdown();
    let outcome = fixture.finished(&invocation).await;
    assert!(matches!(&outcome, CommandOutcome::Uncertain { .. }));
    assert_eq!(
        fixture
            .host
            .start_command(fixture.scope.clone(), invocation.clone())
            .await
            .unwrap(),
        CommandProgress::Finished { outcome }
    );
    assert!(matches!(
        fixture
            .host
            .command_progress(fixture.scope.clone(), &invocation)
            .await
            .unwrap(),
        CommandProgress::Finished {
            outcome: CommandOutcome::Uncertain { .. }
        }
    ));
    assert_eq!(fixture.host.inner.entries.lock().unwrap().len(), 0);
    assert_eq!(fixture.executions(), 1);
}

#[tokio::test]
async fn known_outcomes_survive_service_removal_and_replacement_without_retaining_buffers() {
    for mode in ["success", "cancel"] {
        let mut fixture = Fixture::new(mode).await;
        let (review, invocation) = fixture.review().await;
        fixture.apply_overlay(&review).await;
        let original = Arc::downgrade(
            fixture
                .host
                .inner
                .entries
                .lock()
                .unwrap()
                .values()
                .next()
                .unwrap(),
        );
        fixture
            .host
            .start_command(fixture.scope.clone(), invocation.clone())
            .await
            .unwrap();
        let outcome = fixture.finished(&invocation).await;
        fixture
            .host
            .revoke(&fixture.scope.owner, Some(1), Some(&fixture.scope.provider));
        tokio::time::timeout(Duration::from_secs(5), async {
            while original.upgrade().is_some() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let expected = CommandProgress::Finished { outcome };
        let mut unavailable_root = fixture.scope.clone();
        unavailable_root.root = fixture.root.path().join("missing");
        assert_eq!(
            fixture
                .host
                .command_progress(unavailable_root.clone(), &invocation)
                .await
                .unwrap(),
            expected
        );
        assert_eq!(
            fixture
                .host
                .stop_command(unavailable_root, &invocation)
                .await
                .unwrap(),
            expected
        );
        assert_eq!(
            fixture
                .host
                .start_command(fixture.scope.clone(), invocation.clone())
                .await
                .unwrap(),
            expected
        );
        assert!(fixture.host.inner.entries.lock().unwrap().is_empty());
        fixture.input.request += 1;
        let fresh = fixture
            .host
            .query(fixture.scope.clone(), &fixture.input)
            .await
            .unwrap();
        assert_ne!(fresh.server, invocation.server);
        assert_eq!(
            fixture
                .host
                .command_progress(fixture.scope.clone(), &invocation)
                .await
                .unwrap(),
            expected
        );
        assert_eq!(
            fixture
                .host
                .start_command(fixture.scope.clone(), invocation.clone())
                .await
                .unwrap(),
            expected
        );
        assert_eq!(
            fixture
                .host
                .stop_command(fixture.scope.clone(), &invocation)
                .await
                .unwrap(),
            expected
        );
        fixture.input.request += 1;
        assert_eq!(
            fixture
                .host
                .query(fixture.scope.clone(), &fixture.input)
                .await
                .unwrap()
                .server,
            fresh.server
        );
        assert_eq!(fixture.executions(), 1);
    }
}

#[tokio::test]
async fn revoked_running_workers_and_stopped_before_dispatch_receipts_remain_addressable() {
    for running in [false, true] {
        let mut fixture = Fixture::new("block").await;
        let (review, invocation) = fixture.review().await;
        fixture.apply_overlay(&review).await;
        if running {
            fixture
                .host
                .start_command(fixture.scope.clone(), invocation.clone())
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while fixture.executions() == 0 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        } else {
            fixture
                .host
                .stop_command(fixture.scope.clone(), &invocation)
                .await
                .unwrap();
        }
        fixture
            .host
            .revoke(&fixture.scope.owner, Some(1), Some(&fixture.scope.provider));
        let outcome = fixture.finished(&invocation).await;
        assert_eq!(matches!(outcome, CommandOutcome::Uncertain { .. }), running);
        let expected = CommandProgress::Finished { outcome };
        assert_eq!(
            fixture
                .host
                .stop_command(fixture.scope.clone(), &invocation)
                .await
                .unwrap(),
            expected
        );
        assert_eq!(
            fixture
                .host
                .start_command(fixture.scope.clone(), invocation)
                .await
                .unwrap(),
            expected
        );
        assert!(fixture.host.inner.entries.lock().unwrap().is_empty());
        assert_eq!(fixture.executions(), usize::from(running));
    }
}

#[tokio::test]
async fn full_receipt_capacity_rejects_new_execution_before_native_dispatch() {
    let mut fixture = Fixture::new("success").await;
    let (review, invocation) = fixture.review().await;
    fixture.apply_overlay(&review).await;
    let entry = fixture
        .host
        .inner
        .entries
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    let mut senders = vec![];
    for index in 0..128 {
        let mut other = invocation.clone();
        other.command = format!("{index:032x}");
        let context = CommandScope::new(&fixture.scope, &other).unwrap();
        let (progress, _) = watch::channel(CommandProgress::Running);
        if matches!(
            fixture
                .host
                .inner
                .outcomes
                .lock()
                .unwrap()
                .record(&context, &other, &entry, &progress)
                .unwrap(),
            outcomes::Admission::Full
        ) {
            break;
        }
        senders.push(progress);
    }
    assert!(!senders.is_empty());
    let rejected = fixture
        .host
        .start_command(fixture.scope.clone(), invocation.clone())
        .await
        .unwrap();
    assert!(
        matches!(&rejected, CommandProgress::Finished { outcome:CommandOutcome::Rejected { message } } if message.contains("capacity"))
    );
    assert_eq!(
        fixture
            .host
            .start_command(fixture.scope.clone(), invocation.clone())
            .await
            .unwrap(),
        rejected
    );
    assert_eq!(
        fixture
            .host
            .command_progress(fixture.scope.clone(), &invocation)
            .await
            .unwrap(),
        rejected
    );
    assert_eq!(fixture.executions(), 0);
}

#[tokio::test]
async fn declared_input_edits_prepare_before_application_and_sync_the_exact_reviewed_context() {
    use protocol::actions::ActionSynchronization;
    use protocol::results::WorkspaceChange;
    for (changed_before_preflight, nested) in [(false, false), (false, true), (true, false)] {
        let mut fixture = Fixture::new("inputs").await;
        let folder = if nested {
            let folder = fixture.root.path().join("nested");
            std::fs::create_dir(&folder).unwrap();
            std::fs::rename(
                fixture.root.path().join("main.notes"),
                folder.join("main.notes"),
            )
            .unwrap();
            fixture.input.path = "nested/main.notes".into();
            fixture.scope.server.root_markers = vec!["settings-old.json".into()];
            folder
        } else {
            fixture.root.path().to_path_buf()
        };
        std::fs::write(folder.join("settings-old.json"), "{\"value\":1}").unwrap();
        fixture.scope.server.project_inputs =
            Some(serde_json::from_value(json!({"files":["**/settings*.json"]})).unwrap());
        let (review, invocation) = fixture.review().await;
        let action = ActionSynchronization {
            command: invocation.command.clone(),
            server: invocation.server.clone(),
            origin: invocation.origin.clone(),
            review: invocation.review.clone(),
        };
        fixture.input.request += 1;
        fixture.input.query = Query::PrepareAction {
            action: action.clone(),
        };
        if changed_before_preflight {
            std::fs::write(folder.join("settings-old.json"), "{\"value\":2}").unwrap();
            let error = fixture
                .host
                .query(fixture.scope.clone(), &fixture.input)
                .await
                .unwrap_err();
            assert!(error.contains("changed before application"), "{error}");
            assert!(folder.join("settings-old.json").exists());
            assert!(!folder.join("settings-new.json").exists());
            assert_eq!(fixture.executions(), 0);
            continue;
        }
        fixture
            .host
            .query(fixture.scope.clone(), &fixture.input)
            .await
            .unwrap();
        // A journaled move can pass through both inventories before removal.
        std::fs::copy(
            folder.join("settings-old.json"),
            folder.join("settings-new.json"),
        )
        .unwrap();
        fixture.input.request += 1;
        fixture.input.query = Query::Synchronize;
        assert!(
            fixture
                .host
                .query(fixture.scope.clone(), &fixture.input)
                .await
                .unwrap_err()
                .contains("matching reviewed command")
        );
        std::fs::remove_file(folder.join("settings-old.json")).unwrap();
        fixture.input.request += 1;
        fixture.input.query = Query::Hover {
            position: Position::default(),
        };
        assert!(
            fixture
                .host
                .query(fixture.scope.clone(), &fixture.input)
                .await
                .unwrap_err()
                .contains("matching reviewed command")
        );
        assert_eq!(
            fixture.host.inner.entries.lock().unwrap().len(),
            1,
            "A moved marker must not discover a replacement during action application"
        );
        let file = review
            .edit
            .clone()
            .ordered()
            .unwrap()
            .into_iter()
            .find_map(|change| match change {
                WorkspaceChange::Text { file } => Some(file),
                WorkspaceChange::File { .. } => None,
            })
            .unwrap();
        fixture.input.source = apply_edits(SOURCE, &file.edits).unwrap();
        fixture.input.source_digest = source_digest(&fixture.input.source);
        fixture.input.revision += 1;
        fixture.input.request += 1;
        fixture.input.query = Query::SynchronizeAction {
            action: action.clone(),
            close: false,
        };
        let mut forged = fixture.input.clone();
        if let Query::SynchronizeAction { action, .. } = &mut forged.query {
            action.review = "f".repeat(64);
        }
        assert!(
            fixture
                .host
                .query(fixture.scope.clone(), &forged)
                .await
                .is_err()
        );
        fixture.input.request += 1;
        fixture
            .host
            .query(fixture.scope.clone(), &fixture.input)
            .await
            .unwrap();
        fixture
            .host
            .start_command(fixture.scope.clone(), invocation.clone())
            .await
            .unwrap();
        assert!(matches!(
            fixture.finished(&invocation).await,
            CommandOutcome::Completed { .. }
        ));
        assert_eq!(
            std::fs::read_to_string(folder.join("executions"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        fixture.input.request += 1;
        fixture.input.query = Query::Synchronize;
        let analysis = fixture
            .host
            .query(fixture.scope.clone(), &fixture.input)
            .await;
        if nested {
            assert_ne!(analysis.unwrap().server, invocation.server);
        } else {
            assert!(
                analysis
                    .unwrap_err()
                    .contains("Declared project inputs changed")
            );
        }
        assert!(matches!(
            fixture
                .host
                .command_progress(fixture.scope.clone(), &invocation)
                .await
                .unwrap(),
            CommandProgress::Finished {
                outcome: CommandOutcome::Completed { .. }
            }
        ));
    }
}

#[tokio::test]
async fn discarded_input_changing_actions_release_pins_without_sending_a_command_or_replacing_drafts()
 {
    use protocol::actions::ActionSynchronization;
    let mut fixture = Fixture::new("inputs").await;
    std::fs::write(
        fixture.root.path().join("settings-old.json"),
        "configuration",
    )
    .unwrap();
    fixture.scope.server.project_inputs =
        Some(serde_json::from_value(json!({"files":["settings*.json"]})).unwrap());
    let (_, invocation) = fixture.review().await;
    let action = ActionSynchronization {
        command: invocation.command.clone(),
        server: invocation.server.clone(),
        origin: invocation.origin.clone(),
        review: invocation.review.clone(),
    };
    fixture.input.request += 1;
    fixture.input.query = Query::PrepareAction {
        action: action.clone(),
    };
    fixture
        .host
        .query(fixture.scope.clone(), &fixture.input)
        .await
        .unwrap();
    std::fs::rename(
        fixture.root.path().join("settings-old.json"),
        fixture.root.path().join("settings-new.json"),
    )
    .unwrap();
    let mut release = fixture.input.clone();
    release.document = "release:review".into();
    release.path = "settings-old.json".into(); // The old origin may have been moved/deleted.
    release.revision = 0;
    release.request = 1;
    release.source.clear();
    release.source_digest = source_digest("");
    let mut forged = action.clone();
    forged.review = "f".repeat(64);
    release.query = Query::ReleaseAction { action: forged };
    assert!(
        fixture
            .host
            .query(fixture.scope.clone(), &release)
            .await
            .is_err()
    );
    fixture.input.request += 1;
    fixture.input.query = Query::Synchronize;
    assert!(
        fixture
            .host
            .query(fixture.scope.clone(), &fixture.input)
            .await
            .unwrap_err()
            .contains("matching reviewed command")
    );
    release.request += 1;
    release.query = Query::ReleaseAction { action };
    let response = fixture
        .host
        .query(fixture.scope.clone(), &release)
        .await
        .unwrap();
    response.validate(&release).unwrap();
    // Releasing never mutates the original draft or calls executeCommand.
    assert_eq!(fixture.executions(), 0);
    let entry = fixture
        .host
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
        Some(SOURCE)
    );
    assert!(
        fixture
            .host
            .start_command(fixture.scope.clone(), invocation)
            .await
            .is_err()
    );
    fixture.input.request += 1;
    assert!(
        fixture
            .host
            .query(fixture.scope.clone(), &fixture.input)
            .await
            .unwrap_err()
            .contains("Declared project inputs changed")
    );
}

#[tokio::test]
async fn prepared_actions_fence_only_their_service_and_preserve_independent_drafts_after_release() {
    use protocol::actions::ActionSynchronization;
    for boundary in ["account", "project", "client"] {
        let mut fixture = Fixture::new("success").await;
        // Drive maintenance explicitly so release and later idle reuse are
        // observed independently of the background timer.
        fixture
            .host
            .inner
            .reaping
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let (_, invocation) = fixture.review().await;
        let action = ActionSynchronization {
            command: invocation.command,
            server: invocation.server.clone(),
            origin: invocation.origin,
            review: invocation.review,
        };
        fixture.input.request += 1;
        fixture.input.query = Query::PrepareAction {
            action: action.clone(),
        };
        fixture
            .host
            .query(fixture.scope.clone(), &fixture.input)
            .await
            .unwrap();
        let mut scope = fixture.scope.clone();
        let mut independent = fixture.input.clone();
        independent.query = Query::Synchronize;
        independent.source = "independent 文😀\r\n".into();
        independent.source_digest = source_digest(&independent.source);
        match boundary {
            "account" => scope.owner = "other-owner".into(),
            "project" => scope.project += 1,
            "client" => independent.client = "other-client".into(),
            _ => unreachable!(),
        }
        let other = fixture
            .host
            .query(scope.clone(), &independent)
            .await
            .unwrap();
        assert_ne!(other.server, invocation.server);
        fixture.input.request += 1;
        fixture.input.query = Query::Synchronize;
        assert!(
            fixture
                .host
                .query(fixture.scope.clone(), &fixture.input)
                .await
                .unwrap_err()
                .contains("matching reviewed command")
        );
        let mut release = fixture.input.clone();
        release.document = "release-control".into();
        release.revision = 0;
        release.source.clear();
        release.source_digest = source_digest("");
        release.query = Query::ReleaseAction { action };
        fixture
            .host
            .query(fixture.scope.clone(), &release)
            .await
            .unwrap();
        // Releasing an action does not itself replace another client's service.
        {
            let entries = fixture.host.inner.entries.lock().unwrap();
            let entry = entries
                .iter()
                .find(|(key, _)| {
                    key.owner == scope.owner
                        && key.project == scope.project
                        && key.client == independent.client
                })
                .unwrap()
                .1;
            assert_eq!(entry.session.get().unwrap().id, other.server);
        }
        fixture.input.request += 1;
        let resumed = fixture
            .host
            .query(fixture.scope.clone(), &fixture.input)
            .await
            .unwrap();
        assert_eq!(resumed.server, invocation.server);
        independent.request += 1;
        let returned = fixture
            .host
            .query(scope.clone(), &independent)
            .await
            .unwrap();
        if boundary == "client" {
            // Once the command pin is released, ordinary queries may consolidate
            // compatible idle services and issue a fresh identity on handoff.
            assert_ne!(returned.server, other.server);
        } else {
            assert_eq!(returned.server, other.server);
        }
        assert_eq!(returned.source_digest, independent.source_digest);
        let path = fixture
            .root
            .path()
            .canonicalize()
            .unwrap()
            .join(&fixture.input.path);
        let entries = fixture.host.inner.entries.lock().unwrap();
        for (owner, project, input) in [
            (&fixture.scope.owner, fixture.scope.project, &fixture.input),
            (&scope.owner, scope.project, &independent),
        ] {
            let entry = entries
                .iter()
                .find(|(key, _)| {
                    key.owner == *owner && key.project == project && key.client == input.client
                })
                .unwrap()
                .1;
            let documents = reuse::snapshot(entry).unwrap();
            assert_eq!(documents.overlay_source(&path), Some(input.source.as_str()));
        }
        assert_eq!(fixture.executions(), 0);
    }
}

#[tokio::test]
async fn a_replaced_physical_root_does_not_inherit_a_prepared_server_command() {
    let mut fixture = Fixture::new("success").await;
    let (review, invocation) = fixture.review().await;
    fixture.apply_overlay(&review).await;
    let original = fixture.root.path().with_extension("original");
    std::fs::rename(fixture.root.path(), &original).unwrap();
    std::fs::create_dir(fixture.root.path()).unwrap();
    std::fs::write(fixture.root.path().join("main.notes"), SOURCE).unwrap();
    fixture
        .host
        .start_command(fixture.scope.clone(), invocation.clone())
        .await
        .unwrap();
    let outcome = fixture.finished(&invocation).await;
    assert!(
        matches!(outcome, CommandOutcome::Rejected {message} if message.contains("physical project folder"))
    );
    assert!(!original.join("executions").exists());
    std::fs::remove_dir_all(fixture.root.path()).unwrap();
    std::fs::rename(original, fixture.root.path()).unwrap();
}

#[tokio::test]
async fn a_reviewed_command_cannot_execute_after_the_repository_revision_changes() {
    let mut fixture = Fixture::new("success").await;
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.org",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(fixture.root.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let (review, invocation) = fixture.review().await;
    fixture.apply_overlay(&review).await;
    let output = std::process::Command::new("git")
        .args(["checkout", "-q", "-b", "topic"])
        .current_dir(fixture.root.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    fixture
        .host
        .start_command(fixture.scope.clone(), invocation.clone())
        .await
        .unwrap();
    let outcome = fixture.finished(&invocation).await;
    assert!(
        matches!(outcome, CommandOutcome::Rejected { message } if message.contains("Git revision changed"))
    );
    assert_eq!(fixture.executions(), 0);
    assert!(matches!(
        fixture
            .host
            .command_progress(fixture.scope.clone(), &invocation)
            .await
            .unwrap(),
        CommandProgress::Finished {
            outcome: CommandOutcome::Rejected { .. }
        }
    ));
    fixture.host.shutdown();
}

#[tokio::test]
async fn stopping_an_unstarted_claim_tombstones_a_delayed_dispatch() {
    let mut fixture = Fixture::new("success").await;
    let (review, invocation) = fixture.review().await;
    fixture.apply_overlay(&review).await;
    assert_eq!(
        fixture
            .host
            .command_progress(fixture.scope.clone(), &invocation)
            .await
            .unwrap(),
        CommandProgress::Ready
    );
    let mut forged = invocation.clone();
    forged.review = "e".repeat(64);
    assert!(
        fixture
            .host
            .stop_command(fixture.scope.clone(), &forged)
            .await
            .is_err()
    );
    let stopped = fixture
        .host
        .stop_command(fixture.scope.clone(), &invocation)
        .await
        .unwrap();
    assert!(matches!(
        stopped,
        CommandProgress::Finished {
            outcome: CommandOutcome::Rejected { .. }
        }
    ));
    assert_eq!(
        fixture
            .host
            .start_command(fixture.scope.clone(), invocation.clone())
            .await
            .unwrap(),
        stopped
    );
    assert_eq!(
        fixture
            .host
            .stop_command(fixture.scope.clone(), &invocation)
            .await
            .unwrap(),
        stopped
    );
    assert_eq!(fixture.executions(), 0);
}

#[tokio::test]
async fn stopping_an_inflight_claim_preserves_uncertainty_and_prevents_replay() {
    let mut fixture = Fixture::new("block").await;
    let (review, invocation) = fixture.review().await;
    fixture.apply_overlay(&review).await;
    fixture
        .host
        .start_command(fixture.scope.clone(), invocation.clone())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.executions() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    fixture
        .host
        .stop_command(fixture.scope.clone(), &invocation)
        .await
        .unwrap();
    let outcome = fixture.finished(&invocation).await;
    assert!(matches!(outcome, CommandOutcome::Uncertain { .. }));
    assert_eq!(
        fixture
            .host
            .start_command(fixture.scope.clone(), invocation)
            .await
            .unwrap(),
        CommandProgress::Finished { outcome }
    );
    assert_eq!(fixture.executions(), 1);
}

#[tokio::test]
async fn stopping_an_uncertain_outcome_also_stops_the_original_service() {
    let mut fixture = Fixture::new("cancel").await;
    let (review, invocation) = fixture.review().await;
    fixture.apply_overlay(&review).await;
    fixture
        .host
        .start_command(fixture.scope.clone(), invocation.clone())
        .await
        .unwrap();
    assert!(matches!(
        fixture.finished(&invocation).await,
        CommandOutcome::Uncertain { .. }
    ));
    let entry = fixture
        .host
        .inner
        .entries
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    assert!(entry.session.get().unwrap().client.is_closed());
    assert!(matches!(
        fixture
            .host
            .stop_command(fixture.scope.clone(), &invocation)
            .await
            .unwrap(),
        CommandProgress::Finished {
            outcome: CommandOutcome::Uncertain { .. }
        }
    ));
    assert!(entry.session.get().unwrap().client.is_closed());
    fixture
        .host
        .start_command(fixture.scope.clone(), invocation)
        .await
        .unwrap();
    assert_eq!(fixture.executions(), 1);
}

#[tokio::test]
async fn a_replaced_nested_language_workspace_rejects_a_command_before_the_wire() {
    let mut fixture = Fixture::new("success").await;
    std::fs::create_dir(fixture.root.path().join("nested")).unwrap();
    std::fs::rename(
        fixture.root.path().join("main.notes"),
        fixture.root.path().join("nested/main.notes"),
    )
    .unwrap();
    std::fs::write(fixture.root.path().join("nested/custom.config"), "").unwrap();
    fixture.scope.server.root_markers = vec!["custom.config".into()];
    fixture.input.path = "nested/main.notes".into();
    let (review, invocation) = fixture.review().await;
    fixture.apply_overlay(&review).await;
    std::fs::rename(
        fixture.root.path().join("nested"),
        fixture.root.path().join("original"),
    )
    .unwrap();
    std::fs::create_dir(fixture.root.path().join("nested")).unwrap();
    std::fs::write(fixture.root.path().join("nested/main.notes"), SOURCE).unwrap();
    fixture
        .host
        .start_command(fixture.scope.clone(), invocation.clone())
        .await
        .unwrap();
    assert!(
        matches!(fixture.finished(&invocation).await,CommandOutcome::Rejected {message} if message.contains("physical language workspace"))
    );
    assert!(!fixture.root.path().join("original/executions").exists());
}

#[tokio::test]
async fn losing_folder_access_does_not_prevent_stopping_a_delayed_command() {
    let mut fixture = Fixture::new("success").await;
    let (review, invocation) = fixture.review().await;
    fixture.apply_overlay(&review).await;
    let original = fixture.root.path().with_extension("command-access");
    std::fs::rename(fixture.root.path(), &original).unwrap();
    let stopped = fixture
        .host
        .stop_command(fixture.scope.clone(), &invocation)
        .await
        .unwrap();
    assert!(matches!(
        stopped,
        CommandProgress::Finished {
            outcome: CommandOutcome::Rejected { .. }
        }
    ));
    std::fs::rename(original, fixture.root.path()).unwrap();
    assert_eq!(
        fixture
            .host
            .start_command(fixture.scope.clone(), invocation)
            .await
            .unwrap(),
        stopped
    );
    assert_eq!(fixture.executions(), 0);
}

#[tokio::test]
async fn moving_the_server_workspace_rejects_the_entire_action_during_review() {
    let mut fixture = Fixture::new("move-workspace").await;
    std::fs::create_dir(fixture.root.path().join("nested")).unwrap();
    std::fs::rename(
        fixture.root.path().join("main.notes"),
        fixture.root.path().join("nested/main.notes"),
    )
    .unwrap();
    std::fs::write(fixture.root.path().join("nested/custom.config"), "").unwrap();
    fixture.scope.server.root_markers = vec!["custom.config".into()];
    fixture.input.path = "nested/main.notes".into();
    let actions = fixture
        .host
        .query(fixture.scope.clone(), &fixture.input)
        .await
        .unwrap();
    fixture.input.request += 1;
    fixture.input.query = Query::ReviewAction {
        id: actions.result[2]["id"].as_str().unwrap().into(),
    };
    assert!(
        fixture
            .host
            .query(fixture.scope.clone(), &fixture.input)
            .await
            .unwrap_err()
            .contains("no part of the action was applied")
    );
    assert!(fixture.root.path().join("nested/main.notes").exists());
    assert!(!fixture.root.path().join("nested-moved").exists());
    assert_eq!(fixture.executions(), 0);
}
