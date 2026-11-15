use super::{SOURCE, command_journal, git, send_json};
use openwebide_bridge::{
    ServerConfig,
    languages::host::{Host, Scope},
    native_vfs::NativeFsVfs,
    run_server_until,
    workspace_transactions::Host as Transactions,
};
use openwebide_core::{
    NewProject, UserRole, WorkspaceMode,
    editor::{
        Document, EditorRecovery, EditorRecoveryFile, EditorRecoveryRecord, EditorRecoveryRoot,
        RecoveryScroll,
    },
    plugins::{
        PluginPackage, RecordPlugin,
        execution::PluginExecutionContext,
        languages::{
            intelligence::{
                self, Query,
                actions::{
                    ActionApproval, ActionReview, ActionSynchronization, CommandAuthority,
                    CommandDelegationRequest, CommandDispatch, CommandOperation,
                    CommandPreparation, HostCommandRequest, PairedCommandJournalRequest,
                },
                results::WorkspaceChange,
                workspace_edit::{WorkspaceDocument, WorkspaceDraft},
            },
            source_digest,
        },
    },
    workspace_transaction::{
        BeginWorkspaceTransaction, DelegateWorkspaceTransaction, ExecuteWorkspaceTransaction,
        LanguageReview, NativeWorkspaceTransaction, ResourceScope, VfsTransactionFiles,
        WorkspaceTransactionAction,
        companion::{FileAction, FolderAction, FolderReply, FolderRequest},
    },
};
use serde::de::DeserializeOwned;
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

pub struct Case {
    workspace: tempfile::TempDir,
    recovery_home: tempfile::TempDir,
    journal_server: command_journal::Server,
    pub root: PathBuf,
    pub host: Host,
    pub backend: Arc<command_journal::Backend>,
    pub port: u16,
    pub request: HostCommandRequest,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    server: Option<tokio::task::JoinHandle<()>>,
    process: Option<tokio::process::Child>,
}
pub struct Peer {
    process: tokio::process::Child,
    pub port: u16,
    pub pid: u32,
}
impl Peer {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub async fn kill(mut self) {
        self.process.start_kill().unwrap();
        let status = tokio::time::timeout(Duration::from_secs(5), self.process.wait())
            .await
            .unwrap()
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(libc::SIGKILL));
        }
    }
    pub async fn stop(self) {
        stop_process(self.process).await;
    }
}
async fn stop_process(mut process: tokio::process::Child) {
    #[cfg(unix)]
    {
        let pid = i32::try_from(process.id().unwrap()).unwrap();
        // SAFETY: this is the live child owned by this fixture.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    }
    #[cfg(not(unix))]
    process.start_kill().unwrap();
    let status = tokio::time::timeout(Duration::from_secs(15), process.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "Fixture shutdown failed: {status}");
}
fn decode<T: DeserializeOwned>(reply: super::HttpResponse) -> T {
    assert_eq!(reply.status, 200, "{}", reply.body);
    serde_json::from_str(&reply.body).unwrap()
}
impl Case {
    pub async fn new(paired: bool, repository: bool) -> Self {
        Self::build(paired, repository, None, true, None, None).await
    }
    pub async fn cancelled(paired: bool, unadmitted: bool) -> Self {
        Self::build(paired, false, Some(unadmitted), true, None, None).await
    }
    pub async fn unclaimed(paired: bool) -> Self {
        Self::build(paired, false, None, false, None, None).await
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub async fn dispatched(paired: bool, directory: &std::path::Path) -> Self {
        Self::build(paired, false, None, true, Some(directory), None).await
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub async fn completing(paired: bool, directory: &std::path::Path) -> Self {
        Self::build(paired, false, None, true, Some(directory), Some("success")).await
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub async fn retained(paired: bool, directory: &std::path::Path, uncertain: bool) -> Self {
        Self::build(
            paired,
            false,
            None,
            true,
            Some(directory),
            Some(if uncertain {
                "retained-error"
            } else {
                "retained-result"
            }),
        )
        .await
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn recovery_home(&self) -> &std::path::Path {
        self.recovery_home.path()
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn backend_url(&self) -> &str {
        &self.journal_server.url
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub async fn move_callback(&mut self) {
        self.journal_server = command_journal::serve(self.backend.clone()).await;
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub async fn start_native_process_with_callback(&mut self, callback: &str) -> u32 {
        assert!(self.server.is_none() && self.process.is_none());
        let peer =
            spawn_native_peer(self.workspace.path(), self.recovery_home.path(), callback).await;
        self.port = peer.port;
        self.process = Some(peer.process);
        peer.pid
    }
    async fn build(
        paired: bool,
        repository: bool,
        cancellation: Option<bool>,
        claim: bool,
        dispatched: Option<&std::path::Path>,
        reply: Option<&'static str>,
    ) -> Self {
        let workspace = dispatched
            .map_or_else(tempfile::tempdir, tempfile::tempdir_in)
            .unwrap();
        let home = dispatched
            .map_or_else(tempfile::tempdir, tempfile::tempdir_in)
            .unwrap();
        let root = workspace.path().join("project");
        std::fs::create_dir(&root).unwrap();
        if repository {
            git(&root, &["init", "-q", "-b", "main"]);
            git(&root, &["commit", "--allow-empty", "-qm", "initial"]);
        }
        std::fs::write(root.join("main.notes"), SOURCE).unwrap();
        let script = root.join("peer.py");
        std::fs::write(
            &script,
            include_str!("../../../tools/fixtures/commands_server.py"),
        )
        .unwrap();
        let backend = if let Some(directory) = dispatched {
            command_journal::Backend::persisted(&directory.join("journal.sqlite")).await
        } else {
            command_journal::Backend::new().await
        };
        backend
            .original_unavailable
            .store(cancellation.is_some(), Ordering::SeqCst);
        backend
            .reject_file_admission
            .store(cancellation == Some(true), Ordering::SeqCst);
        let journal_server = command_journal::serve(backend.clone()).await;
        let mut config = ServerConfig::new_for_test(
            workspace.path().into(),
            "dummy_secret".into(),
            Some("paired-token".into()),
            home.path().into(),
        )
        .unwrap();
        config.backend_url = journal_server.url.clone();
        let host = config.language_servers.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(run_server_until(listener, config, async {
            let _ = stopped.await;
        }));
        let folder = if paired {
            let opened: FolderReply = decode(
                send_json(
                    port,
                    true,
                    "/workspace/files",
                    &FolderRequest {
                        user: 1,
                        project: 1,
                        action: FolderAction::Open {
                            path: "project".into(),
                            expected: None,
                        },
                    },
                )
                .await,
            );
            let FolderReply::Opened {
                session,
                challenge,
                identity,
            } = opened
            else {
                panic!("Missing original folder proof")
            };
            let proof = std::fs::read_to_string(root.join(challenge)).unwrap();
            let ready: FolderReply = decode(
                send_json(
                    port,
                    true,
                    "/workspace/files",
                    &FolderRequest {
                        user: 1,
                        project: 1,
                        action: FolderAction::Prove {
                            session: session.clone(),
                            proof,
                        },
                    },
                )
                .await,
            );
            let FolderReply::Ready { worker, .. } = ready else {
                panic!("Original folder was not proven")
            };
            Some((session, worker, identity))
        } else {
            None
        };
        let mut prepared = openwebide_core::plugins::testing::receipt();
        prepared.manifest.compatibility.plugin_api = 9;
        prepared.manifest.executable = None;
        prepared.manifest.contributions.tools.clear();
        prepared.manifest.contributions.skills.clear();
        let execution = if let Some(reply) = reply {
            reply
        } else if dispatched.is_some() {
            "fork-block"
        } else {
            "success"
        };
        prepared.manifest.contributions.languages=serde_json::from_value(json!([{"id":"notes","name":"Notes","extensions":["notes"],"features":{"languageServer":{"command":"python3","args":[script,execution]}}}])).unwrap();
        let mode = if paired {
            WorkspaceMode::Local
        } else {
            WorkspaceMode::Remote
        };
        let user = backend
            .store
            .insert_user("owner", "hash", UserRole::Admin, 100)
            .await
            .unwrap()
            .id;
        assert_eq!(user.get(), 1);
        let project = backend
            .store
            .create_project(
                &NewProject {
                    name: "reviewed".into(),
                    mode,
                    path: (!paired).then(|| "project".into()),
                },
                user,
                100,
            )
            .await
            .unwrap();
        assert_eq!(project.id, 1);
        backend
            .store
            .record_plugin(
                user,
                &RecordPlugin {
                    prepared: prepared.clone(),
                    package: Some(Box::new(PluginPackage {
                        prepared: prepared.clone(),
                        skills: vec![],
                    })),
                    approved_capabilities: vec![],
                    revision: None,
                    update_policy: None,
                },
                100,
            )
            .await
            .unwrap();
        let binding = backend
            .store
            .project_plugins(user, 1)
            .await
            .unwrap()
            .into_iter()
            .find(|p| p.prepared.source == prepared.source)
            .unwrap();
        let scope = Scope {
            owner: if paired { "paired/user:1" } else { "user:1" }.into(),
            project: 1,
            root: root.clone(),
            provider: prepared.storage_namespace(),
            digest: prepared.digest.clone(),
            server: intelligence::declared_server(&prepared, "notes")
                .unwrap()
                .clone(),
        };
        let mut input = intelligence::Request {
            version: intelligence::VERSION,
            client: "client".into(),
            document: "document".into(),
            request: 1,
            revision: 1,
            language: "notes".into(),
            path: "main.notes".into(),
            source: SOURCE.into(),
            source_digest: source_digest(SOURCE),
            query: Query::CodeActions {
                range: Default::default(),
            },
        };
        let actions = host
            .query_package(scope.clone(), &input, &prepared)
            .await
            .unwrap();
        input.request += 1;
        input.query = Query::ReviewAction {
            id: actions.result[0]["id"].as_str().unwrap().into(),
        };
        let reviewed = host
            .query_package(scope.clone(), &input, &prepared)
            .await
            .unwrap();
        let review: ActionReview = serde_json::from_value(reviewed.result.clone()).unwrap();
        let before = EditorRecoveryRecord {
            revision: 1,
            state: EditorRecovery {
                format: 1,
                root: Some(EditorRecoveryRoot::for_project(&project)),
                files: vec![EditorRecoveryFile {
                    path: "main.notes".into(),
                    document: Some(Document::new(SOURCE).recovery()),
                    scroll: RecoveryScroll::default(),
                    read_only: false,
                }],
                selected: Some("main.notes".into()),
            },
        };
        assert_eq!(
            backend
                .store
                .save_editor_recovery(user, 1, 0, &before.state)
                .await
                .unwrap(),
            1
        );
        let identity = Transactions::root_identity(workspace.path(), "project", 1, 1).unwrap();
        if let Some((_, _, proven)) = &folder {
            assert_eq!(*proven, identity);
        }
        let native = NativeFsVfs { root: root.clone() };
        let files = VfsTransactionFiles {
            vfs: &native,
            identity: &identity,
        };
        let changes = review.edit.clone().ordered().unwrap();
        let operations = changes
            .iter()
            .filter_map(|change| {
                if let WorkspaceChange::File { operation } = change {
                    Some(operation.clone())
                } else {
                    None
                }
            })
            .collect();
        let documents = [(
            "main.notes".into(),
            WorkspaceDocument::new("main.notes", Document::new(SOURCE)),
        )]
        .into();
        let mut draft = WorkspaceDraft::prepare_native(
            &files,
            identity.clone(),
            operations,
            documents,
            vec!["main.notes".into()],
            Some("main.notes".into()),
            || true,
        )
        .await
        .unwrap();
        for change in changes {
            match change {
                WorkspaceChange::File { operation } => draft.resource(operation).unwrap(),
                WorkspaceChange::Text { file } => {
                    draft.document(&files, &file.path, || true).await.unwrap();
                    draft.text(file).unwrap();
                }
            }
        }
        let after = draft.recovery(&before.state).unwrap();
        let editor = draft.recovery_patch(&before, &after).unwrap();
        let retained = LanguageReview {
            prepared: prepared.clone(),
            binding_revision: binding.revision,
            input: input.clone(),
            context: reviewed.context.unwrap(),
            result: reviewed.result,
            transaction_digest: LanguageReview::transaction_digest(&draft.plan, &editor).unwrap(),
        };
        let action = ActionApproval {
            id: "b".repeat(32),
            prepared: prepared.clone(),
            binding_revision: binding.revision,
            input: input.clone(),
            review,
        };
        let grant = "d".repeat(32);
        backend
            .store
            .issue_plugin_context_grant(
                user,
                &PluginExecutionContext {
                    project_id: Some(1),
                    user_action: true,
                    ..Default::default()
                },
                &prepared,
                &grant,
                100,
            )
            .await
            .unwrap();
        let begin = BeginWorkspaceTransaction {
            id: action.id.clone(),
            plan: draft.plan,
            editor,
            language_review: Some(Box::new(retained)),
        };
        let accepted = backend
            .store
            .begin_workspace_transaction(user, 1, &begin, 100)
            .await
            .unwrap();
        let call = intelligence::Call {
            grant: grant.clone(),
            session_id: None,
            prepared: prepared.clone(),
            input: input.clone(),
        };
        backend
            .store
            .approve_language_action(user, 1, &call, &action, 100)
            .await
            .unwrap();
        let mut prepare = input.clone();
        prepare.request += 1;
        prepare.query = Query::PrepareAction {
            action: ActionSynchronization::from_approval(&action).unwrap(),
        };
        host.query_package(scope.clone(), &prepare, &prepared)
            .await
            .unwrap();
        let native = NativeWorkspaceTransaction {
            user: 1,
            project: 1,
            request: ExecuteWorkspaceTransaction {
                id: action.id.clone(),
                generation: accepted.journal.generation.clone(),
                action: if cancellation.is_some() {
                    WorkspaceTransactionAction::Recover
                } else {
                    WorkspaceTransactionAction::Apply
                },
                language_grant: Some(grant.clone()),
            },
        };
        let outcome = if let Some((session, worker, _)) = folder {
            let bound: FolderReply = decode(
                send_json(
                    port,
                    true,
                    "/workspace/files",
                    &FolderRequest {
                        user: 1,
                        project: 1,
                        action: FolderAction::File {
                            session: session.clone(),
                            sequence: 1,
                            action: FileAction::Bind {
                                scope: ResourceScope {
                                    transaction: action.id.clone(),
                                    fingerprint: begin.plan.fingerprint().unwrap(),
                                },
                            },
                        },
                    },
                )
                .await,
            );
            assert!(matches!(bound, FolderReply::File { outcome: Ok(_) }));
            let delegation = backend
                .store
                .delegate_workspace_transaction(
                    user,
                    1,
                    &action.id,
                    &DelegateWorkspaceTransaction {
                        generation: accepted.journal.generation,
                        revision: accepted.journal.revision,
                        worker,
                    },
                    101,
                )
                .await
                .unwrap();
            let response = send_json(
                port,
                true,
                "/workspace/files",
                &FolderRequest {
                    user: 1,
                    project: 1,
                    action: FolderAction::Execute {
                        session,
                        api_url: journal_server.url.clone(),
                        delegation,
                        request: native.request,
                    },
                },
            )
            .await;
            if cancellation == Some(true) {
                assert_eq!(response.status, 400, "{}", response.body);
                None
            } else {
                let reply: FolderReply = decode(response);
                let FolderReply::Executed { outcome } = reply else {
                    panic!("Missing whole file outcome")
                };
                Some(outcome)
            }
        } else {
            let response = send_json(port, false, "/workspace/execute", &native).await;
            if cancellation == Some(true) {
                assert_eq!(response.status, 400, "{}", response.body);
                None
            } else {
                Some(decode(response))
            }
        };
        if let Some(unadmitted) = cancellation {
            use openwebide_core::workspace_transaction::{
                AdvanceWorkspaceTransaction, TransactionPhase, TransactionProgress,
            };
            if unadmitted {
                let scope = backend
                    .store
                    .workspace_transaction_scope(user, 1, &action.id)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(scope.worker.is_none());
                backend
                    .store
                    .advance_workspace_transaction(
                        user,
                        1,
                        &AdvanceWorkspaceTransaction {
                            id: action.id.clone(),
                            generation: scope.generation,
                            revision: scope.revision,
                            progress: TransactionProgress::prepared_cancellation(),
                        },
                        102,
                    )
                    .await
                    .unwrap();
            } else {
                let outcome = outcome.unwrap();
                assert!(outcome.error.is_none(), "{outcome:?}");
                assert_eq!(outcome.phase, Some(TransactionPhase::Restored));
            }
            let cancelled = backend
                .store
                .cancel_language_action(user, 1, &action.id, 1, 103)
                .await
                .unwrap();
            assert!(cancelled.outcome.is_none());
            let command = backend
                .store
                .authorize_language_command(
                    user,
                    1,
                    &CommandDispatch {
                        id: action.id,
                        operation: CommandOperation::Cancel,
                        call: None,
                    },
                    103,
                )
                .await
                .unwrap();
            let request = HostCommandRequest {
                user: (!paired).then_some(1),
                root: String::new(),
                command,
                authority: None,
                journal: None,
            };
            return Self {
                workspace,
                recovery_home: home,
                journal_server,
                root,
                host,
                backend,
                port,
                request,
                stop: Some(stop),
                server: Some(server),
                process: None,
            };
        }
        let outcome = outcome.unwrap();
        assert!(outcome.error.is_none(), "{outcome:?}");
        assert_eq!(
            outcome.phase,
            Some(openwebide_core::workspace_transaction::TransactionPhase::Committed)
        );
        if !claim {
            let command = backend
                .store
                .authorize_language_command(
                    user,
                    1,
                    &CommandDispatch {
                        id: action.id,
                        operation: CommandOperation::Status,
                        call: None,
                    },
                    102,
                )
                .await
                .unwrap();
            let request = HostCommandRequest {
                user: (!paired).then_some(1),
                root: String::new(),
                command,
                authority: None,
                journal: None,
            };
            return Self {
                workspace,
                recovery_home: home,
                journal_server,
                root,
                host,
                backend,
                port,
                request,
                stop: Some(stop),
                server: Some(server),
                process: None,
            };
        }
        let mut sync = prepare;
        sync.request += 1;
        sync.revision += 1;
        sync.source = after
            .files
            .iter()
            .find(|f| f.path == "main.notes")
            .unwrap()
            .document
            .as_ref()
            .unwrap()
            .restore()
            .unwrap()
            .text()
            .into();
        sync.source_digest = source_digest(&sync.source);
        sync.query = Query::SynchronizeAction {
            action: ActionSynchronization::from_approval(&action).unwrap(),
            close: false,
        };
        host.query_package(scope, &sync, &prepared).await.unwrap();
        backend
            .store
            .claim_language_action(user, 1, &action.id, 1, &call, 102)
            .await
            .unwrap();
        backend.now.store(102, Ordering::SeqCst);
        let command = backend
            .store
            .authorize_language_command(
                user,
                1,
                &CommandDispatch {
                    id: action.id.clone(),
                    operation: CommandOperation::Start,
                    call: Some(Box::new(call)),
                },
                102,
            )
            .await
            .unwrap();
        let mut request = HostCommandRequest {
            user: (!paired).then_some(1),
            root: "project".into(),
            command,
            authority: Some(CommandAuthority {
                grant,
                session_id: None,
            }),
            journal: None,
        };
        let preparation: CommandPreparation =
            decode(send_json(port, paired, "/plugins/languages/command/prepare", &request).await);
        let CommandPreparation::Worker { worker } = preparation else {
            panic!("A new command already has an outcome")
        };
        if paired {
            let delegation = backend
                .store
                .delegate_language_command(
                    user,
                    1,
                    &CommandDelegationRequest {
                        id: action.id,
                        worker,
                    },
                    102,
                )
                .await
                .unwrap();
            request.journal = Some(PairedCommandJournalRequest {
                api_url: journal_server.url.clone(),
                delegation,
            });
        }
        Self {
            workspace,
            recovery_home: home,
            journal_server,
            root,
            host,
            backend,
            port,
            request,
            stop: Some(stop),
            server: Some(server),
            process: None,
        }
    }
    pub async fn stop_native(&mut self) {
        if let Some(process) = self.process.take() {
            stop_process(process).await;
            return;
        }
        let _ = self.stop.take().unwrap().send(());
        tokio::time::timeout(Duration::from_secs(15), self.server.take().unwrap())
            .await
            .unwrap()
            .unwrap();
    }
    pub async fn restart_native(&mut self) {
        self.stop_native().await;
        self.start_native().await;
    }
    pub async fn start_native(&mut self) {
        let mut config = ServerConfig::new_for_test(
            self.workspace.path().into(),
            "dummy_secret".into(),
            Some("paired-token".into()),
            self.recovery_home.path().into(),
        )
        .unwrap();
        config.backend_url = self.journal_server.url.clone();
        self.host = config.language_servers.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        self.port = listener.local_addr().unwrap().port();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        self.stop = Some(stop);
        self.server = Some(tokio::spawn(run_server_until(listener, config, async {
            let _ = stopped.await;
        })));
    }
    pub async fn start_native_process(&mut self) -> u32 {
        assert!(self.server.is_none() && self.process.is_none());
        let peer = self.spawn_native_peer().await;
        self.port = peer.port;
        self.process = Some(peer.process);
        peer.pid
    }
    pub async fn spawn_native_peer(&self) -> Peer {
        spawn_native_peer(
            self.workspace.path(),
            self.recovery_home.path(),
            &self.journal_server.url,
        )
        .await
    }
    pub async fn kill_native_process(&mut self) {
        let mut process = self.process.take().unwrap();
        process.start_kill().unwrap();
        let status = tokio::time::timeout(Duration::from_secs(5), process.wait())
            .await
            .unwrap()
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(libc::SIGKILL));
        }
        assert!(!status.success());
    }
    pub async fn close(mut self) {
        self.stop_native().await;
    }
}
pub async fn spawn_native_peer(
    workspace: &std::path::Path,
    recovery_home: &std::path::Path,
    callback: &str,
) -> Peer {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let current = std::env::current_exe().unwrap();
    let executable = current
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/check_recorded_command_host");
    assert!(
        executable.is_file(),
        "Build the test-support check_recorded_command_host example before this contract"
    );
    let mut process = tokio::process::Command::new(executable)
        .arg(workspace)
        .arg(recovery_home)
        .arg(callback)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pid = process.id().unwrap();
    let mut ready = String::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        BufReader::new(process.stdout.take().unwrap()).read_line(&mut ready),
    )
    .await
    .unwrap()
    .unwrap();
    Peer {
        process,
        port: ready.trim().parse().unwrap(),
        pid,
    }
}
