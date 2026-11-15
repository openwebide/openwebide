//! Compact recovery policy for ordinary workspace mutations. This observation
//! is not a lease: the owning adapter must serialize it with accepted writes.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceActivity {
    pub file_recovery: bool,
    pub language_command: bool,
}
impl WorkspaceActivity {
    pub fn ensure_idle(self) -> Result<(), &'static str> {
        if self.file_recovery {
            Err("Recover the pending workspace operation before changing files")
        } else if self.language_command {
            Err("Review the pending language command before changing files")
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_incomplete_or_unknown_observation_cannot_grant_write_access() {
        for invalid in [
            "{}",
            r#"{"file_recovery":false}"#,
            r#"{"language_command":false}"#,
            r#"{"file_recovery":false,"language_command":false,"lease":true}"#,
            r#"{"file_recovery":null,"language_command":false}"#,
        ] {
            assert!(serde_json::from_str::<WorkspaceActivity>(invalid).is_err());
        }
        for file_recovery in [false, true] {
            for language_command in [false, true] {
                let status = WorkspaceActivity {
                    file_recovery,
                    language_command,
                };
                assert_eq!(
                    status.ensure_idle().is_ok(),
                    !file_recovery && !language_command
                );
            }
        }
    }
}

#[cfg(test)]
mod recovery_response_tests {
    use super::*;
    use crate::{
        WorkspaceMode,
        editor::EditorRecoveryRoot,
        workspace_transaction::{
            RecoveryAuthority, WorkspaceJournalResponse, WorkspaceTransactionScope,
        },
    };
    #[test]
    fn private_scope_requires_complete_compound_activity_and_redacts_its_credential() {
        let scope = WorkspaceTransactionScope {
            id: "1".repeat(32),
            revision: 1,
            generation: "2".repeat(32),
            root_identity: "original".into(),
            root: EditorRecoveryRoot {
                mode: WorkspaceMode::Local,
                path: None,
            },
            worker: None,
            accepted_language_review_digest: None,
        };
        let authority = RecoveryAuthority::for_scope(1, 2, &scope, "A".repeat(43)).unwrap();
        let secret = authority.secret.clone();
        let response = WorkspaceJournalResponse::RecoveryScope {
            scope,
            authority: Box::new(authority),
            activity: WorkspaceActivity {
                file_recovery: false,
                language_command: true,
            },
        };
        assert!(!format!("{response:?}").contains(&secret));
        let mut value = serde_json::to_value(response).unwrap();
        let WorkspaceJournalResponse::RecoveryScope { activity, .. } =
            serde_json::from_value(value.clone()).unwrap()
        else {
            unreachable!()
        };
        assert!(activity.ensure_idle().is_err());
        for invalid in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!({"file_recovery":false}),
        ] {
            value["activity"] = invalid;
            assert!(serde_json::from_value::<WorkspaceJournalResponse>(value.clone()).is_err());
        }
        value.as_object_mut().unwrap().remove("activity");
        assert!(serde_json::from_value::<WorkspaceJournalResponse>(value).is_err());
    }
}
