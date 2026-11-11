//! The small durable identity needed before queuing a filesystem operation.
//! This is not a write lease; match it with fresh metadata and the full record
//! after acquiring the physical folder and snapshot capacity.
use super::{WorkspaceTransactionState, valid_id};
use crate::editor::{EditorRecovery, EditorRecoveryRoot};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTransactionScope {
    pub id: String,
    pub revision: u64,
    /// Database-generated identity of this record, independent of a reusable ID.
    pub generation: String,
    pub root_identity: String,
    pub root: EditorRecoveryRoot,
    /// Last native worker identity, retained after release to fence delayed claims.
    pub worker: Option<super::WorkspaceTransactionWorkerState>,
    /// Exact immutable LanguageReview accepted by a native worker. Absence
    /// never authorizes a first mutation; accepted recovery needs no live grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_language_review_digest: Option<String>,
}
impl WorkspaceTransactionScope {
    pub fn matches_state(&self, state: &WorkspaceTransactionState) -> bool {
        self.id == state.journal.id
            && self.generation == state.journal.generation
            && self.revision == state.journal.revision
            && self.root_identity == state.journal.plan.root
            && self.root == state.editor.root
            && self
                .accepted_language_review_digest
                .as_ref()
                .is_none_or(|digest| {
                    state.language_review.as_ref().is_some_and(|review| {
                        review.fingerprint().is_ok_and(|actual| &actual == digest)
                    })
                })
    }
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id)
            || !valid_id(&self.generation)
            || self.revision == 0
            || self.root_identity.is_empty()
            || self.root_identity.len() > 4096
            || self.root_identity.chars().any(char::is_control)
            || self
                .worker
                .as_ref()
                .is_some_and(|worker| !valid_id(&worker.id))
            || self
                .accepted_language_review_digest
                .as_deref()
                .is_some_and(|digest| {
                    !super::companion::fingerprint(digest) || self.worker.is_none()
                })
        {
            return Err("Invalid workspace recovery scope".into());
        }
        EditorRecovery {
            root: Some(self.root.clone()),
            ..EditorRecovery::default()
        }
        .validate()
    }
}
