//! Private original-transaction credentials, independent of replaceable workers.
use super::{WorkspaceTransactionScope, scope::validate_original_scope};
use crate::editor::EditorRecoveryRoot;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

pub const RECOVERY_AUTHORITY_DOMAIN: &str = "workspace-recovery-v1";

/// Structural credential data. Issuance and original-owner verification belong
/// to trusted runtimes; this type never grants filesystem mutation on its own.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryAuthority {
    pub format: u32,
    pub user: i64,
    pub project: i64,
    pub transaction: String,
    pub generation: String,
    pub root_identity: String,
    pub root: EditorRecoveryRoot,
    pub secret: String,
}
impl std::fmt::Debug for RecoveryAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RecoveryAuthority")
            .field("format", &self.format)
            .field("user", &self.user)
            .field("project", &self.project)
            .field("transaction", &self.transaction)
            .field("generation", &self.generation)
            .field("root_identity", &self.root_identity)
            .finish_non_exhaustive()
    }
}
impl RecoveryAuthority {
    pub fn for_scope(
        user: i64,
        project: i64,
        scope: &WorkspaceTransactionScope,
        secret: String,
    ) -> Result<Self, String> {
        scope.validate()?;
        let authority = Self {
            format: 1,
            user,
            project,
            transaction: scope.id.clone(),
            generation: scope.generation.clone(),
            root_identity: scope.root_identity.clone(),
            root: scope.root.clone(),
            secret,
        };
        authority.validate()?;
        Ok(authority)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.format != 1
            || self.user <= 0
            || self.project <= 0
            || self.secret.len() != 43
            || !URL_SAFE_NO_PAD
                .decode(&self.secret)
                .is_ok_and(|bytes| bytes.len() == 32)
        {
            return Err("Invalid original workspace recovery authority".into());
        }
        validate_original_scope(
            &self.transaction,
            &self.generation,
            &self.root_identity,
            &self.root,
        )
    }

    /// Canonical immutable claims for the trusted issuer. Revision, worker and
    /// accepted-provider state deliberately do not rotate an original owner.
    pub fn binding_for_scope(
        user: i64,
        project: i64,
        scope: &WorkspaceTransactionScope,
    ) -> Result<Vec<u8>, String> {
        scope.validate()?;
        if user <= 0 || project <= 0 {
            return Err("Invalid original workspace recovery account".into());
        }
        serde_json::to_vec(&(
            RECOVERY_AUTHORITY_DOMAIN,
            1u32,
            user,
            project,
            &scope.id,
            &scope.generation,
            &scope.root_identity,
            &scope.root,
        ))
        .map_err(|error| error.to_string())
    }

    /// Compare original identity only. A caller must separately verify the
    /// private credential and current mutation/recovery phase.
    pub fn matches_scope(
        &self,
        user: i64,
        project: i64,
        scope: &WorkspaceTransactionScope,
    ) -> bool {
        self.validate().is_ok()
            && scope.validate().is_ok()
            && self.user == user
            && self.project == project
            && self.transaction == scope.id
            && self.generation == scope.generation
            && self.root_identity == scope.root_identity
            && self.root == scope.root
    }

    pub fn secret_digest(&self) -> Result<String, String> {
        self.validate()?;
        Ok(crate::plugins::content_digest(self.secret.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WorkspaceMode, workspace_transaction::WorkspaceTransactionWorkerState};

    fn scope() -> WorkspaceTransactionScope {
        WorkspaceTransactionScope {
            id: "1".repeat(32),
            generation: "2".repeat(32),
            revision: 1,
            root_identity: "original-root".into(),
            root: EditorRecoveryRoot {
                mode: WorkspaceMode::Local,
                path: None,
            },
            worker: None,
            accepted_language_review_digest: None,
        }
    }

    fn authority(scope: &WorkspaceTransactionScope) -> RecoveryAuthority {
        RecoveryAuthority::for_scope(7, 11, scope, URL_SAFE_NO_PAD.encode([7u8; 32])).unwrap()
    }

    #[test]
    fn original_binding_survives_worker_release_and_provider_acceptance() {
        let original = scope();
        let mut progressed = original.clone();
        progressed.revision = 99;
        progressed.worker = Some(WorkspaceTransactionWorkerState {
            id: "3".repeat(32),
            active: false,
        });
        progressed.accepted_language_review_digest = Some("ab".repeat(32));
        assert_eq!(
            RecoveryAuthority::binding_for_scope(7, 11, &original).unwrap(),
            RecoveryAuthority::binding_for_scope(7, 11, &progressed).unwrap(),
        );
        assert_eq!(authority(&original), authority(&progressed));
        assert!(authority(&original).matches_scope(7, 11, &progressed));
    }

    #[test]
    fn account_project_generation_and_original_root_are_separate_bindings() {
        let original = scope();
        let expected = RecoveryAuthority::binding_for_scope(7, 11, &original).unwrap();
        let held = authority(&original);
        for (user, project) in [(8, 11), (7, 12)] {
            assert_ne!(
                expected,
                RecoveryAuthority::binding_for_scope(user, project, &original).unwrap()
            );
            assert!(!held.matches_scope(user, project, &original));
        }
        let mut changed = Vec::new();
        let mut value = original.clone();
        value.id = "3".repeat(32);
        changed.push(value);
        let mut value = original.clone();
        value.generation = "3".repeat(32);
        changed.push(value);
        let mut value = original.clone();
        value.root_identity = "replacement-root".into();
        changed.push(value);
        let mut value = original.clone();
        value.root.mode = WorkspaceMode::Remote;
        changed.push(value);
        let mut value = original.clone();
        value.root.path = Some("another-path".into());
        changed.push(value);
        for changed in changed {
            assert_ne!(
                expected,
                RecoveryAuthority::binding_for_scope(7, 11, &changed).unwrap()
            );
            assert!(!held.matches_scope(7, 11, &changed));
        }
    }

    #[test]
    fn identical_public_claims_do_not_replace_the_private_owner_credential() {
        let scope = scope();
        let first = authority(&scope);
        let other =
            RecoveryAuthority::for_scope(7, 11, &scope, URL_SAFE_NO_PAD.encode([8u8; 32])).unwrap();
        assert!(first.matches_scope(7, 11, &scope));
        assert!(other.matches_scope(7, 11, &scope));
        assert_ne!(
            first.secret_digest().unwrap(),
            other.secret_digest().unwrap()
        );
    }

    #[test]
    fn private_authority_roundtrips_without_debug_disclosure() {
        let authority = authority(&scope());
        let wire = serde_json::to_string(&authority).unwrap();
        assert_eq!(authority, serde_json::from_str(&wire).unwrap());
        assert!(!format!("{authority:?}").contains(&authority.secret));
        let mut unknown = serde_json::to_value(&authority).unwrap();
        unknown["replacement"] = serde_json::json!(true);
        assert!(serde_json::from_value::<RecoveryAuthority>(unknown).is_err());
    }

    #[test]
    fn malformed_authorities_and_roots_fail_without_echoing_private_values() {
        let original = authority(&scope());
        let mut invalid = Vec::new();
        let mut value = original.clone();
        value.format = 2;
        invalid.push(value);
        let mut value = original.clone();
        value.user = 0;
        invalid.push(value);
        let mut value = original.clone();
        value.project = 0;
        invalid.push(value);
        let mut value = original.clone();
        value.transaction = "not-an-id".into();
        invalid.push(value);
        let mut value = original.clone();
        value.generation = "A".repeat(32);
        invalid.push(value);
        let mut value = original.clone();
        value.root_identity = String::new();
        invalid.push(value);
        let mut value = original.clone();
        value.root.path = Some("\0".into());
        invalid.push(value);
        let mut value = original.clone();
        value.root.path = Some("x".repeat(4097));
        invalid.push(value);
        let mut value = original.clone();
        value.secret = "invalid-secret-value".into();
        invalid.push(value);
        for value in invalid {
            let error = value.validate().unwrap_err();
            assert!(!error.contains(&value.secret));
            assert!(value.secret_digest().is_err());
        }
    }
}
