use super::*;
use futures::FutureExt;
use openwebide_core::{
    Vfs,
    vfs::VfsEntryKind,
    workspace_entries::WorkspaceEntries,
    workspace_transaction::{
        ResourceLocation, TransactionFiles,
        companion::{CompanionFiles, FolderTransport},
    },
};
use std::sync::atomic::AtomicUsize;

mod authority;
mod cleanup;
mod coordination;
mod inspection;
mod journal;

struct Client {
    host: Host,
    root: PathBuf,
    calls: Arc<AtomicUsize>,
    lose_transfer: Arc<AtomicBool>,
    lose_advance: Arc<AtomicBool>,
    requests: Arc<std::sync::Mutex<Vec<FolderRequest>>>,
}
impl FolderTransport for Client {
    async fn request(&self, request: &FolderRequest) -> Result<FolderReply, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.requests.lock().unwrap().push(request.clone());
        // Exercise the production envelopes, including retained locations and
        // base64 binary data, rather than bypassing serialization in this client.
        let request: FolderRequest =
            serde_json::from_slice(&serde_json::to_vec(request).unwrap()).unwrap();
        let transfer = matches!(
            &request.action,
            FolderAction::File {
                action: FileAction::Transfer { .. },
                ..
            }
        );
        let advance = matches!(request.action, FolderAction::Advance { .. });
        let reply = self
            .host
            .request(self.root.clone(), "paired/user:1".into(), request)
            .await?;
        if transfer && self.lose_transfer.swap(false, Ordering::Relaxed) {
            return Err("Native transfer acknowledgement lost".into());
        }
        if advance && self.lose_advance.swap(false, Ordering::Relaxed) {
            return Err("Native cursor acknowledgement lost".into());
        }
        Ok(serde_json::from_slice(&serde_json::to_vec(&reply).unwrap()).unwrap())
    }
}
struct Case {
    directory: tempfile::TempDir,
    host: Host,
    authorities: std::sync::Mutex<Vec<authority::Authority>>,
    lose_advance: Arc<AtomicBool>,
    requests: Arc<std::sync::Mutex<Vec<FolderRequest>>>,
}
impl Case {
    fn new() -> Self {
        Self {
            directory: tempfile::tempdir().unwrap(),
            host: Host::default(),
            authorities: Default::default(),
            lose_advance: Arc::new(AtomicBool::new(false)),
            requests: Default::default(),
        }
    }
    fn root(&self) -> PathBuf {
        self.directory.path().canonicalize().unwrap()
    }
    async fn request(&self, action: FolderAction) -> Result<FolderReply, String> {
        self.host
            .request(
                self.root(),
                "paired/user:1".into(),
                FolderRequest {
                    user: 1,
                    project: 1,
                    action,
                },
            )
            .await
    }
    async fn open(&self) -> (String, String, String) {
        match self
            .request(FolderAction::Open {
                path: "".into(),
                expected: None,
            })
            .await
            .unwrap()
        {
            FolderReply::Opened {
                session,
                challenge,
                identity,
            } => (session, challenge, identity),
            _ => panic!("Missing folder challenge"),
        }
    }
    async fn prove(&self, id: &str, name: &str) -> String {
        let proof = std::fs::read_to_string(self.root().join(name)).unwrap();
        assert!(matches!(
            self.request(FolderAction::Prove {
                session: id.into(),
                proof: proof.clone()
            })
            .await
            .unwrap(),
            FolderReply::Ready { .. }
        ));
        assert!(!self.root().join(name).exists());
        proof
    }
    async fn files(&self) -> CompanionFiles<Client> {
        self.files_at("", Arc::new(AtomicBool::new(false))).await
    }
    async fn files_at(&self, path: &str, lose_transfer: Arc<AtomicBool>) -> CompanionFiles<Client> {
        self.files_on(&self.host, path, lose_transfer).await
    }
    async fn files_on(
        &self,
        host: &Host,
        path: &str,
        lose_transfer: Arc<AtomicBool>,
    ) -> CompanionFiles<Client> {
        let FolderReply::Opened {
            session: id,
            challenge: name,
            identity,
        } = host
            .request(
                self.root(),
                "paired/user:1".into(),
                FolderRequest {
                    user: 1,
                    project: 1,
                    action: FolderAction::Open {
                        path: path.into(),
                        expected: None,
                    },
                },
            )
            .await
            .unwrap()
        else {
            panic!()
        };
        let proof = std::fs::read_to_string(self.root().join(path).join(name)).unwrap();
        let FolderReply::Ready { worker, .. } = host
            .request(
                self.root(),
                "paired/user:1".into(),
                FolderRequest {
                    user: 1,
                    project: 1,
                    action: FolderAction::Prove {
                        session: id.clone(),
                        proof,
                    },
                },
            )
            .await
            .unwrap()
        else {
            panic!("Missing native worker commitment")
        };
        CompanionFiles::new(
            Client {
                host: host.clone(),
                root: self.root(),
                calls: Arc::new(AtomicUsize::new(0)),
                lose_transfer,
                lose_advance: self.lose_advance.clone(),
                requests: self.requests.clone(),
            },
            1,
            1,
            id,
            identity,
            worker,
        )
        .unwrap()
    }
    async fn call(&self, id: &str, sequence: u64, action: FileAction) -> Result<FileValue, String> {
        match self
            .request(FolderAction::File {
                session: id.into(),
                sequence,
                action,
            })
            .await?
        {
            FolderReply::File { outcome } => outcome,
            _ => panic!("Missing filesystem acknowledgement"),
        }
    }
}
fn scope() -> ResourceScope {
    ResourceScope {
        transaction: "a".repeat(32),
        fingerprint: "b".repeat(64),
    }
}
async fn exists(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_browser_must_read_the_physical_challenge_before_native_access() {
    let case = Case::new();
    std::fs::create_dir(case.root().join(".git")).unwrap();
    std::fs::write(case.root().join("source"), [0, 255, 128, 7]).unwrap();
    let (id, name, identity) = case.open().await;
    let secret = std::fs::read_to_string(case.root().join(&name)).unwrap();
    let opened = serde_json::to_string(&FolderReply::Opened {
        session: id.clone(),
        challenge: name.clone(),
        identity: identity.clone(),
    })
    .unwrap();
    assert!(!opened.contains(&secret));
    assert!(
        case.call(
            &id,
            1,
            FileAction::Read {
                path: "source".into()
            }
        )
        .await
        .is_err()
    );
    assert!(
        case.request(FolderAction::Prove {
            session: id.clone(),
            proof: "0".repeat(64)
        })
        .await
        .is_err()
    );
    let request = FolderRequest {
        user: 2,
        project: 1,
        action: FolderAction::Prove {
            session: id.clone(),
            proof: secret.clone(),
        },
    };
    assert!(
        case.host
            .request(case.root(), "paired/user:2".into(), request)
            .await
            .is_err()
    );
    assert!(
        crate::runs::native_vfs::NativeFsVfs { root: case.root() }
            .read(&name)
            .await
            .is_err()
    );
    case.prove(&id, &name).await;
    assert!(
        matches!(case.call(&id, 1, FileAction::Read { path: "source".into() }).await.unwrap(), FileValue::Bytes(bytes) if bytes==[0,255,128,7])
    );
    let FileValue::Entries(entries) = case
        .call(&id, 2, FileAction::List { path: "".into() })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(entries.iter().any(|entry| entry.name == ".git"));
    assert!(
        case.call(
            &id,
            3,
            FileAction::Canonicalize {
                path: ".git".into()
            }
        )
        .await
        .is_err()
    );
    assert!(
        case.call(
            &id,
            3,
            FileAction::Create {
                ordinal: 0,
                path: "unapproved".into(),
                is_dir: false
            }
        )
        .await
        .is_err()
    );
    assert!(!case.root().join("unapproved").exists());
}

#[tokio::test]
async fn duplicate_mutations_acknowledge_once_and_new_proofs_fence_delayed_calls() {
    let case = Case::new();
    std::fs::write(case.root().join("source"), "original").unwrap();
    std::fs::hard_link(case.root().join("source"), case.root().join("witness")).unwrap();
    let (id, name, _) = case.open().await;
    let old_proof = case.prove(&id, &name).await;
    case.call(&id, 1, FileAction::Bind { scope: scope() })
        .await
        .unwrap();
    case.admit_session(&id, &scope()).await;
    let transfer = FileAction::Transfer {
        from: ResourceLocation::Workspace("source".into()),
        to: ResourceLocation::Workspace("moved".into()),
        is_dir: false,
    };
    case.call(&id, 2, transfer.clone()).await.unwrap();
    case.call(&id, 2, transfer).await.unwrap();
    assert!(
        same_file::is_same_file(case.root().join("moved"), case.root().join("witness")).unwrap()
    );
    assert!(!case.root().join("source").exists());
    assert!(
        case.call(
            &id,
            2,
            FileAction::RemoveCreated {
                ordinal: 0,
                path: "moved".into(),
                is_dir: false
            }
        )
        .await
        .is_err()
    );
    assert!(case.call(&id, 4, FileAction::Finish).await.is_err());
    let (next, name, _) = case.open().await;
    // An unproven request must not evict the active owner.
    case.call(
        &id,
        3,
        FileAction::Read {
            path: "moved".into(),
        },
    )
    .await
    .unwrap();
    case.prove(&next, &name).await;
    assert!(case.call(&id, 4, FileAction::Finish).await.is_err());
    assert!(
        case.request(FolderAction::Prove {
            session: id.clone(),
            proof: old_proof
        })
        .await
        .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(case.root().join("moved")).unwrap(),
        "original"
    );
    case.call(
        &next,
        1,
        FileAction::Read {
            path: "moved".into(),
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn cancelling_the_browser_observer_cannot_reorder_a_native_mutation_and_recovery() {
    let case = Case::new();
    let mut files = case.files().await;
    let scope = scope();
    case.bind(&mut files, scope.clone()).await.unwrap();
    let gate = case.host.state.lock().await;
    // Polling sends the actual owned worker, then dropping the pending observer
    // leaves the session poisoned. No second primitive may overtake that worker.
    assert!(
        files
            .create_resource(&scope, 0, "created", VfsEntryKind::File)
            .now_or_never()
            .is_none()
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(1), files.read_bytes("created"))
            .await
            .unwrap()
            .is_err()
    );
    drop(gate);
    exists(&case.root().join("created")).await;
    let mut recovered = case.files().await;
    case.bind(&mut recovered, scope.clone()).await.unwrap();
    assert!(
        recovered
            .created_resource_matches(&scope, 0, "created", VfsEntryKind::File)
            .await
            .unwrap()
    );
    recovered
        .remove_created_resource(&scope, 0, "created", VfsEntryKind::File)
        .await
        .unwrap();
    recovered.finish_retained(&scope).await.unwrap();
    assert!(!case.root().join("created").exists());
    assert!(
        !case
            .root()
            .join(format!(".openwebide/recovery-{}", scope.transaction))
            .exists()
    );
}

#[tokio::test]
async fn retained_transfers_use_the_shared_adapter_and_preserve_the_original_object() {
    let case = Case::new();
    std::fs::create_dir(case.root().join("folder")).unwrap();
    std::fs::write(case.root().join("folder/source"), "binary\0original").unwrap();
    std::fs::hard_link(
        case.root().join("folder/source"),
        case.root().join("witness"),
    )
    .unwrap();
    let mut files = case.files().await;
    let scope = scope();
    case.bind(&mut files, scope.clone()).await.unwrap();
    files
        .transfer_resource(
            &scope,
            &ResourceLocation::Workspace("folder".into()),
            &ResourceLocation::Retained(0),
            VfsEntryKind::Directory,
        )
        .await
        .unwrap();
    assert_eq!(
        files.retained_read_bytes(&scope, "0/source").await.unwrap(),
        b"binary\0original"
    );
    assert_eq!(files.retained_list(&scope, "0").await.unwrap().len(), 1);
    assert_eq!(
        files
            .retained_canonicalize(&scope, "0/source")
            .await
            .unwrap(),
        "0/source"
    );
    files
        .transfer_resource(
            &scope,
            &ResourceLocation::Retained(0),
            &ResourceLocation::Workspace("restored".into()),
            VfsEntryKind::Directory,
        )
        .await
        .unwrap();
    assert!(
        same_file::is_same_file(
            case.root().join("restored/source"),
            case.root().join("witness")
        )
        .unwrap()
    );
    files.finish_retained(&scope).await.unwrap();
    files.close().await.unwrap();
    assert!(files.read_bytes("restored/source").await.is_err());
}

#[tokio::test]
async fn native_transactions_exclude_old_companion_calls_and_replaced_roots_never_redirect_them() {
    let case = Case::new();
    std::fs::create_dir(case.root().join("project")).unwrap();
    std::fs::write(case.root().join("project/source"), "old").unwrap();
    let request = FolderRequest {
        user: 1,
        project: 1,
        action: FolderAction::Open {
            path: "project".into(),
            expected: None,
        },
    };
    let FolderReply::Opened {
        session,
        challenge,
        identity,
    } = case
        .host
        .request(case.root(), "paired/user:1".into(), request)
        .await
        .unwrap()
    else {
        panic!()
    };
    let proof = std::fs::read_to_string(case.root().join("project").join(challenge)).unwrap();
    case.request(FolderAction::Prove {
        session: session.clone(),
        proof,
    })
    .await
    .unwrap();
    let captured = Transactions::capture_root(&case.root(), "project", 1, 1).unwrap();
    let fence = case.host.exclude(&captured).await.unwrap();
    drop(fence);
    assert!(
        case.call(
            &session,
            1,
            FileAction::Read {
                path: "source".into()
            }
        )
        .await
        .is_err()
    );
    std::fs::rename(case.root().join("project"), case.root().join("old-project")).unwrap();
    std::fs::create_dir(case.root().join("project")).unwrap();
    std::fs::write(case.root().join("project/source"), "new").unwrap();
    assert!(
        case.request(FolderAction::Open {
            path: "project".into(),
            expected: Some(identity)
        })
        .await
        .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(case.root().join("project/source")).unwrap(),
        "new"
    );
    assert_eq!(
        std::fs::read_to_string(case.root().join("old-project/source")).unwrap(),
        "old"
    );
}

#[tokio::test]
async fn expiring_proofs_preserve_replacements_and_later_contents() {
    for replacement in [false, true] {
        let case = Case::new();
        let (id, name, _) = case.open().await;
        let path = case.root().join(&name);
        if replacement {
            std::fs::rename(&path, case.root().join("original-proof")).unwrap();
        }
        std::fs::write(&path, "later user data").unwrap();
        let mut state = case.host.state.lock().await;
        state.sessions.get_mut(&id).unwrap().touched = Instant::now() - PROOF_TTL;
        state.sweep();
        assert!(state.sessions.is_empty());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "later user data");
    }
    let case = Case::new();
    let (id, name, _) = case.open().await;
    let mut state = case.host.state.lock().await;
    state.sessions.get_mut(&id).unwrap().touched = Instant::now() - PROOF_TTL;
    state.sweep();
    drop(state);
    tokio::time::timeout(Duration::from_secs(3), async {
        while case.root().join(&name).exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn large_read_responses_are_not_retained_as_unbounded_session_state() {
    let case = Case::new();
    std::fs::write(case.root().join("large"), vec![42; CACHE_LIMIT * 2]).unwrap();
    let (id, name, _) = case.open().await;
    case.prove(&id, &name).await;
    case.call(
        &id,
        1,
        FileAction::Read {
            path: "large".into(),
        },
    )
    .await
    .unwrap();
    assert!(
        case.host.state.lock().await.sessions[&id]
            .last
            .as_ref()
            .unwrap()
            .1
            .is_none()
    );
    assert!(
        case.call(
            &id,
            1,
            FileAction::Read {
                path: "large".into()
            }
        )
        .await
        .is_err()
    );
    case.call(
        &id,
        2,
        FileAction::Read {
            path: "large".into(),
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn acknowledged_native_failures_allow_followup_calls_without_replaying_the_mutation() {
    let case = Case::new();
    std::fs::write(case.root().join("source"), "original").unwrap();
    std::fs::write(case.root().join("occupied"), "later data").unwrap();
    let original = same_file::Handle::from_path(case.root().join("source")).unwrap();
    let scope = scope();
    let mut files = case.files().await;
    case.bind(&mut files, scope.clone()).await.unwrap();
    assert!(
        files
            .transfer_resource(
                &scope,
                &ResourceLocation::Workspace("source".into()),
                &ResourceLocation::Workspace("occupied".into()),
                VfsEntryKind::File
            )
            .await
            .is_err()
    );
    assert_eq!(files.read_bytes("source").await.unwrap(), b"original");
    assert_eq!(files.read_bytes("occupied").await.unwrap(), b"later data");
    files
        .transfer_resource(
            &scope,
            &ResourceLocation::Workspace("source".into()),
            &ResourceLocation::Workspace("available".into()),
            VfsEntryKind::File,
        )
        .await
        .unwrap();
    assert_eq!(
        same_file::Handle::from_path(case.root().join("available")).unwrap(),
        original
    );
    files.finish_retained(&scope).await.unwrap();
}

#[tokio::test]
async fn a_foreign_recovery_area_rejects_binding_before_any_workspace_move() {
    let case = Case::new();
    let scope = scope();
    let mut original = case.files().await;
    case.bind(&mut original, scope.clone()).await.unwrap();
    original
        .create_resource(&scope, 0, "created", VfsEntryKind::File)
        .await
        .unwrap();
    let mut other = case.files().await;
    let wrong = ResourceScope {
        transaction: scope.transaction.clone(),
        fingerprint: "c".repeat(64),
    };
    assert!(case.bind(&mut other, wrong.clone()).await.is_err());
    assert!(!other.supports_resource_moves());
    assert!(
        other
            .transfer_resource(
                &wrong,
                &ResourceLocation::Workspace("created".into()),
                &ResourceLocation::Workspace("moved".into()),
                VfsEntryKind::File
            )
            .await
            .is_err()
    );
    assert!(case.root().join("created").exists());
    assert!(!case.root().join("moved").exists());
    let mut recovered = case.files().await;
    case.bind(&mut recovered, scope.clone()).await.unwrap();
    recovered
        .remove_created_resource(&scope, 0, "created", VfsEntryKind::File)
        .await
        .unwrap();
    recovered.finish_retained(&scope).await.unwrap();
}

#[tokio::test]
async fn timed_out_observation_does_not_cancel_native_work_or_let_recovery_overtake_it() {
    let case = Case::new();
    let mut files = case.files().await;
    let scope = scope();
    case.bind(&mut files, scope.clone()).await.unwrap();
    let gate = case.host.state.lock().await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(10),
            files.create_resource(&scope, 0, "created", VfsEntryKind::File)
        )
        .await
        .is_err()
    );
    assert!(
        files
            .read_bytes("created")
            .await
            .unwrap_err()
            .contains("acknowledgement was lost")
    );
    // Queue fresh proof while the accepted native worker is still waiting.
    // The original worker must finish before this new session can open/prove.
    let mut recovery = Box::pin(case.files());
    assert!(recovery.as_mut().now_or_never().is_none());
    tokio::task::yield_now().await;
    assert!(!case.root().join("created").exists());
    drop(gate);
    let mut recovered = recovery.await;
    assert!(case.root().join("created").is_file());
    case.bind(&mut recovered, scope.clone()).await.unwrap();
    assert!(
        recovered
            .created_resource_matches(&scope, 0, "created", VfsEntryKind::File)
            .await
            .unwrap()
    );
    recovered
        .remove_created_resource(&scope, 0, "created", VfsEntryKind::File)
        .await
        .unwrap();
    recovered.finish_retained(&scope).await.unwrap();
    assert!(!case.root().join("created").exists());
}
