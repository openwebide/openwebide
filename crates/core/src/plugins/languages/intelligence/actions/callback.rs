//! Credential-free routing claims around authenticated private recovery bodies.
use super::*;
use crate::workspace_transaction::{
    RECOVERY_SCOPE_BODY_LIMIT, RecoveryAuthority, WorkspaceTransactionScope,
};
pub const COMMAND_RECOVERY_REQUEST_DOMAIN: &str = "command-recovery-request-v1";
pub const COMMAND_RECOVERY_REPLY_DOMAIN: &str = "command-recovery-reply-v1";
pub const COMMAND_RECOVERY_PLAINTEXT_LIMIT: usize = MAX_COMMAND_BYTES + RECOVERY_SCOPE_BODY_LIMIT;
pub const COMMAND_RECOVERY_MESSAGE_LIMIT: usize = 2 * COMMAND_RECOVERY_PLAINTEXT_LIMIT + 8192;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRecoveryMessage {
    pub format: u32,
    pub user: i64,
    pub project: i64,
    pub scope: WorkspaceTransactionScope,
    pub nonce: String,
    pub ciphertext: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRecoveryReply {
    pub nonce: String,
    pub ciphertext: String,
}
impl CommandRecoveryMessage {
    pub fn for_authority(authority: &RecoveryAuthority) -> Result<Self, String> {
        authority.validate()?;
        Ok(Self {
            format: 1,
            user: authority.user,
            project: authority.project,
            scope: WorkspaceTransactionScope {
                id: authority.transaction.clone(),
                generation: authority.generation.clone(),
                root: authority.root.clone(),
                root_identity: authority.root_identity.clone(),
                revision: 1,
                worker: None,
                accepted_language_review_digest: None,
            },
            nonce: String::new(),
            ciphertext: String::new(),
        })
    }
    /// Only immutable public identity is routed outside the private message.
    pub fn context(&self) -> Result<Vec<u8>, String> {
        self.scope.validate()?;
        if self.format != 1
            || self.user <= 0
            || self.project <= 0
            || self.scope.revision != 1
            || self.scope.worker.is_some()
            || self.scope.accepted_language_review_digest.is_some()
        {
            return Err("Invalid original command message identity".into());
        }
        serde_json::to_vec(&(self.format, self.user, self.project, &self.scope))
            .map_err(|_| "Invalid original command message context".into())
    }
    /// A response to another request, including an earlier identical operation,
    /// cannot satisfy this request's private reply authentication.
    pub fn reply_context(&self) -> Result<Vec<u8>, String> {
        let context = self.context()?;
        if self.nonce.len() != 32
            || self.ciphertext.len() > COMMAND_RECOVERY_MESSAGE_LIMIT
            || self.ciphertext.is_empty()
        {
            return Err("Invalid original command message".into());
        }
        serde_json::to_vec(&(
            context,
            &self.nonce,
            crate::plugins::content_digest(self.ciphertext.as_bytes()),
        ))
        .map_err(|_| "Invalid original command reply context".into())
    }
    pub fn matches_authority(&self, authority: &RecoveryAuthority) -> bool {
        self.context().is_ok() && authority.matches_scope(self.user, self.project, &self.scope)
    }
}

/// A candidate address is a hint, never a replacement recovery credential.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandCallbackRequest {
    #[serde(default)]
    pub user: Option<i64>,
    pub project: i64,
    pub callback: String,
}
impl CommandCallbackRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.project <= 0 || self.user.is_some_and(|user| user <= 0) {
            return Err("Invalid command recovery account or project".into());
        }
        Self::address(&self.callback)?;
        Ok(())
    }
    pub fn address(callback: &str) -> Result<String, String> {
        if callback.len() > 4096 {
            return Err("Invalid command recovery address".into());
        }
        let url = url::Url::parse(callback).map_err(|_| "Invalid command recovery address")?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.has_host()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("Invalid command recovery address".into());
        }
        let value = url.to_string().trim_end_matches('/').to_owned();
        if value.len() > 4096 {
            return Err("Invalid command recovery address".into());
        }
        Ok(value)
    }
}
