use super::*;
use crate::workspace_transaction::{
    DelegatedWorkspaceJournalOperation, WorkspaceTransactionScope, language_review_admission,
};

#[test]
fn original_compact_admission_envelopes_remain_readable_and_grants_stay_out_of_recovery_scope() {
    let admit: DelegatedWorkspaceJournalOperation =
        serde_json::from_value(json!({"command":"admit"})).unwrap();
    assert!(matches!(
        admit,
        DelegatedWorkspaceJournalOperation::Admit {
            review_digest: None,
            language_grant: None
        }
    ));
    assert_eq!(
        serde_json::to_value(admit).unwrap(),
        json!({"command":"admit"})
    );
    let (_, plan, editor) = fixture();
    let old = json!({"id":"a".repeat(32), "generation":"b".repeat(32), "revision":1,
        "root_identity": plan.root, "root":editor.root, "worker":null});
    let scope: WorkspaceTransactionScope = serde_json::from_value(old.clone()).unwrap();
    scope.validate().unwrap();
    assert!(scope.accepted_language_review_digest.is_none());
    assert_eq!(serde_json::to_value(scope).unwrap(), old);
    assert!(language_review_admission(None, None).unwrap().is_none());
    let digest = "c".repeat(64);
    let token = "d".repeat(32);
    for (digest, grant) in [
        (Some(digest.as_str()), None),
        (None, Some(token.as_str())),
        (Some("bad"), Some(token.as_str())),
        (Some(digest.as_str()), Some("bad")),
    ] {
        assert!(language_review_admission(digest, grant).is_err());
    }
}

#[test]
fn accepted_scope_binds_the_full_review_and_cannot_exist_without_a_worker_identity() {
    let (review, plan, editor) = fixture();
    let state = crate::workspace_transaction::WorkspaceTransactionState {
        journal: crate::workspace_transaction::JournalRecord {
            id: "a".repeat(32),
            generation: "b".repeat(32),
            revision: 1,
            plan,
            progress: Default::default(),
        },
        editor,
        editor_revision: 1,
        language_review: Some(Box::new(review.clone())),
    };
    let mut scope = WorkspaceTransactionScope {
        id: state.journal.id.clone(),
        generation: state.journal.generation.clone(),
        revision: 1,
        root_identity: state.journal.plan.root.clone(),
        root: state.editor.root.clone(),
        worker: None,
        accepted_language_review_digest: Some(review.fingerprint().unwrap()),
    };
    assert!(scope.validate().is_err());
    scope.worker = Some(
        crate::workspace_transaction::WorkspaceTransactionWorkerState {
            id: "c".repeat(32),
            active: false,
        },
    );
    scope.validate().unwrap();
    assert!(scope.matches_state(&state));
    let mut changed = state;
    changed.language_review.as_mut().unwrap().input.request += 1;
    assert!(!scope.matches_state(&changed));
    changed.language_review = None;
    assert!(!scope.matches_state(&changed));
}
