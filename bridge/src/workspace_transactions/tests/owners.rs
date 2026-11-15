use super::*;
use crate::{native_recovery::Namespace, runs::native_vfs::workspace::WorkspaceFiles};
use openwebide_core::Vfs;

struct ClaimBoundary {
    backend: Arc<Backend>,
    home: PathBuf,
    root: PathBuf,
    reject: AtomicBool,
    foreign: AtomicBool,
    calls: AtomicUsize,
}
impl JournalBackend for ClaimBoundary {
    async fn request(
        &self,
        input: &WorkspaceJournalRequest,
    ) -> Result<WorkspaceJournalResponse, String> {
        if matches!(
            input.operation,
            WorkspaceJournalOperation::ClaimWorker { .. }
        ) {
            let namespace = Namespace::open(&self.home).unwrap();
            namespace.ensure_current().unwrap();
            let folder = Folder::capture(&self.root).unwrap();
            assert!(
                namespace
                    .admit(&folder, None)
                    .unwrap_err()
                    .contains("Recover the original native workspace operation"),
                "Owner intent must be durable BEFORE the backend CAS"
            );
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.reject.swap(false, Ordering::SeqCst) {
                return Err("Injected worker-claim rejection".into());
            }
        }
        let mut response = self.backend.request(input).await?;
        if self.foreign.load(Ordering::SeqCst)
            && let WorkspaceJournalResponse::RecoveryScope { authority, .. } = &mut response
        {
            authority.secret = "Q".repeat(43);
        }
        Ok(response)
    }
}
#[tokio::test]
async fn intent_precedes_cas_and_only_the_original_private_issuer_can_resume_it() {
    let fixture = fixture_version(2).await;
    let root = fixture.directory.path().join("project");
    let home = tempfile::Builder::new()
        .prefix("openwebide-owner-cas-")
        .tempdir()
        .unwrap()
        .keep();
    let backend = Arc::new(ClaimBoundary {
        backend: fixture.backend.clone(),
        home: home.clone(),
        root: root.clone(),
        reject: AtomicBool::new(true),
        foreign: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
    });
    let mut host = Host::new();
    host.recovery_home = Arc::new(home.clone());
    let error = host
        .execute(
            fixture.directory.path().into(),
            fixture.command.clone(),
            backend.clone(),
        )
        .await
        .unwrap_err();
    assert!(error.contains("Injected worker-claim rejection"));
    assert!(fixture.backend.last_worker.lock().unwrap().is_none());
    drop(host);
    let coordinator = Coordinator::default();
    coordinator.set_recovery_home(Arc::new(home.clone()));
    let files = WorkspaceFiles::new(root.clone(), coordinator);
    let denied = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        files.write("unwanted.txt", "no"),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(
        denied
            .to_string()
            .contains("Recover the original native workspace operation")
    );
    assert!(!root.join("unwanted.txt").exists());
    backend.foreign.store(true, Ordering::SeqCst);
    let mut next = Host::new();
    next.recovery_home = Arc::new(home.clone());
    assert!(
        next.execute(
            fixture.directory.path().into(),
            fixture.command.clone(),
            backend.clone()
        )
        .await
        .unwrap_err()
        .contains("Recover the original native workspace operation")
    );
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    assert!(fixture.backend.last_worker.lock().unwrap().is_none());
    backend.foreign.store(false, Ordering::SeqCst);
    let outcome = next
        .execute(
            fixture.directory.path().into(),
            fixture.command.clone(),
            backend.clone(),
        )
        .await
        .unwrap();
    assert!(outcome.error.is_none());
    assert_eq!(outcome.phase, Some(TransactionPhase::Committed));
    assert_eq!(backend.calls.load(Ordering::SeqCst), 2);
    files.write("allowed.txt", "resolved").await.unwrap();
    assert_eq!(
        std::fs::read(root.join("allowed.txt")).unwrap(),
        b"resolved"
    );
    println!(
        "retained_claim_boundary_fixture={} workspace={}",
        home.display(),
        fixture.directory.keep().display()
    );
}
