use super::*;
use crate::native_recovery::{DATABASE, DIRECTORY, Namespace};

struct ReplaceDuringClaim {
    backend: Arc<Backend>,
    home: PathBuf,
    once: AtomicBool,
}
impl JournalBackend for ReplaceDuringClaim {
    async fn request(
        &self,
        request: &WorkspaceJournalRequest,
    ) -> Result<WorkspaceJournalResponse, String> {
        let response = self.backend.request(request).await;
        if matches!(
            request.operation,
            WorkspaceJournalOperation::ClaimWorker { .. }
        ) && response.is_ok()
            && self.once.swap(false, Ordering::SeqCst)
        {
            let database = self.home.join(DIRECTORY).join(DATABASE);
            std::fs::rename(&database, self.home.join("original.sqlite3")).unwrap();
            std::fs::copy(self.home.join("original.sqlite3"), database).unwrap();
        }
        response
    }
}

#[tokio::test]
async fn namespace_replacement_during_claim_rejects_files_until_the_original_object_is_restored() {
    let fixture = fixture_version(2).await;
    let home = tempfile::Builder::new()
        .prefix("openwebide-recovery-claim-case-")
        .tempdir()
        .unwrap()
        .keep();
    drop(Namespace::open(&home).unwrap());
    let mut host = Host::new();
    host.recovery_home = Arc::new(home.clone());
    let root = fixture.directory.path().join("project");
    let before = std::fs::read(root.join("before.rs")).unwrap();
    let after = std::fs::read(root.join("after.rs")).unwrap();
    let original_before = same_file::Handle::from_path(root.join("before.rs")).unwrap();
    let original_after = same_file::Handle::from_path(root.join("after.rs")).unwrap();
    let backend = Arc::new(ReplaceDuringClaim {
        backend: fixture.backend.clone(),
        home: home.clone(),
        once: AtomicBool::new(true),
    });
    let result = host
        .execute(
            fixture.directory.path().into(),
            fixture.command.clone(),
            backend,
        )
        .await;
    assert!(result.unwrap_err().contains("Native recovery namespace"));
    assert!(fixture.backend.last_worker.lock().unwrap().is_some());
    let scope = fixture
        .backend
        .store
        .workspace_transaction_scope(fixture.user, fixture.project, &fixture.command.request.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!scope.worker.unwrap().active);
    assert_eq!(std::fs::read(root.join("before.rs")).unwrap(), before);
    assert_eq!(std::fs::read(root.join("after.rs")).unwrap(), after);
    assert_eq!(
        same_file::Handle::from_path(root.join("before.rs")).unwrap(),
        original_before
    );
    assert_eq!(
        same_file::Handle::from_path(root.join("after.rs")).unwrap(),
        original_after
    );
    assert!(Namespace::open(&home).is_err());
    // Explicit fixture restoration moves the same original object back. The
    // application never recreates, clears or accepts the replacement database.
    let database = home.join(DIRECTORY).join(DATABASE);
    std::fs::rename(&database, home.join("rejected-copy.sqlite3")).unwrap();
    std::fs::rename(home.join("original.sqlite3"), database).unwrap();
    drop(Namespace::open(&home).unwrap());
    let result = host
        .execute(
            fixture.directory.path().into(),
            fixture.command,
            fixture.backend,
        )
        .await
        .unwrap();
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(result.phase, Some(TransactionPhase::Committed));
    assert!(!root.join("before.rs").exists());
    assert_eq!(
        same_file::Handle::from_path(root.join("after.rs")).unwrap(),
        original_before
    );
    println!("retained_claim_namespace_fixture={}", home.display());
}
