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
