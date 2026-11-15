//! Recovery-only transport pinned to the original native owner's callback.
use super::*;
use crate::runs::http_client::ReqwestHttpClient;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use openwebide_core::plugins::languages::intelligence::actions::{
    COMMAND_RECOVERY_REPLY_DOMAIN, COMMAND_RECOVERY_REQUEST_DOMAIN, CommandJournalOperation,
    CommandRecoveryMessage, CommandRecoveryReply, MAX_COMMAND_BYTES, OriginalCommandRequest,
};

#[derive(Clone)]
pub(crate) struct OriginalCommandJournal {
    origin: String,
    authority: transaction::RecoveryAuthority,
    worker: WorkspaceTransactionWorker,
    namespace: Option<std::sync::Arc<crate::native_recovery::Namespace>>,
}
impl OriginalCommandJournal {
    pub(crate) fn new(
        origin: String,
        authority: transaction::RecoveryAuthority,
        worker: WorkspaceTransactionWorker,
    ) -> Result<Self, String> {
        let uri = origin
            .parse::<hyper::Uri>()
            .map_err(|_| "Invalid original command callback")?;
        if !matches!(uri.scheme_str(), Some("http" | "https"))
            || uri.authority().is_none()
            || uri.query().is_some()
        {
            return Err("Invalid original command callback".into());
        }
        authority.validate()?;
        worker.validate()?;
        Ok(Self {
            origin,
            authority,
            worker,
            namespace: None,
        })
    }
    pub(crate) fn with_namespace(
        mut self,
        namespace: std::sync::Arc<crate::native_recovery::Namespace>,
    ) -> Self {
        self.namespace = Some(namespace);
        self
    }
    pub(crate) async fn scope(&self) -> Result<transaction::WorkspaceCommandRecovery, String> {
        let reply = self.send(CommandJournalOperation::Scope).await?;
        Ok(reply
            .command_recovery(
                self.authority.user,
                self.authority.project,
                &self.authority.transaction,
            )?
            .0
            .clone())
    }
    async fn send(
        &self,
        operation: CommandJournalOperation,
    ) -> Result<WorkspaceJournalResponse, String> {
        let input = OriginalCommandRequest {
            authority: self.authority.clone(),
            worker: self.worker.clone(),
            operation,
        };
        let origin = if let Some(namespace) = &self.namespace {
            namespace
                .command_callback(&self.authority, &self.worker)?
                .ok_or("Original command owner has retired")?
        } else {
            self.origin.clone()
        };
        input.validate()?;
        let response_limit = if matches!(input.operation, CommandJournalOperation::Outcome { .. }) {
            MAX_COMMAND_BYTES + transaction::RECOVERY_SCOPE_BODY_LIMIT
        } else {
            transaction::RECOVERY_SCOPE_BODY_LIMIT
        };
        let body = serde_json::to_vec(&input).map_err(|_| "Invalid original command cleanup")?;
        if body.len() > MAX_COMMAND_BYTES + transaction::RECOVERY_SCOPE_BODY_LIMIT {
            return Err("Original command cleanup exceeds its bound".into());
        }
        let mut message = CommandRecoveryMessage::for_authority(&self.authority)?;
        let (nonce, ciphertext) = openwebide_auth::private_messages::seal(
            self.authority.secret.as_bytes(),
            COMMAND_RECOVERY_REQUEST_DOMAIN,
            &message.context()?,
            &body,
        )?;
        message.nonce = nonce;
        message.ciphertext = ciphertext;
        let body = serde_json::to_vec(&message).map_err(|_| "Invalid private original command")?;
        let callback_changed = async {
            if let Some(namespace) = &self.namespace {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    match namespace.command_callback(&self.authority, &self.worker) {
                        Ok(Some(current)) if current == origin => {}
                        Ok(_) => return,
                        Err(_) => {}
                    }
                }
            } else {
                std::future::pending::<()>().await;
            }
        };
        let response = tokio::time::timeout(std::time::Duration::from_secs(60), async {
            let response = ReqwestHttpClient::default()
                .send(
                    hyper::Request::builder()
                        .method("POST")
                        .uri(format!(
                            "{}/language-commands/original-private",
                            origin.trim_end_matches('/')
                        ))
                        .header("content-type", "application/json")
                        .body(Full::new(Bytes::from(body)))
                        .map_err(|_| "Invalid original command callback")?,
                )
                .await
                .map_err(|_| "Original command callback is unavailable")?;
            let status = response.status();
            let bytes = Limited::new(response.into_body(), 2 * response_limit + 8192)
                .collect()
                .await
                .map_err(|_| "Original command reply exceeds its bound")?
                .to_bytes();
            if !status.is_success() {
                return Err("Original command cleanup was not acknowledged");
            }
            Ok(bytes)
        });
        let response = tokio::select! {
            result=response=>result.map_err(|_| "Original command cleanup timed out")??,
            ()=callback_changed=>return Err("Original command callback changed; retry its retained owner".into()),
        };
        let private: CommandRecoveryReply = serde_json::from_slice(&response)
            .map_err(|_| "Invalid private original command reply")?;
        let response = openwebide_auth::private_messages::open(
            self.authority.secret.as_bytes(),
            COMMAND_RECOVERY_REPLY_DOMAIN,
            &message.reply_context()?,
            &private.nonce,
            &private.ciphertext,
            response_limit,
        )?;
        let reply: WorkspaceJournalResponse =
            serde_json::from_slice(&response).map_err(|_| "Invalid original command reply")?;
        match &input.operation {
            CommandJournalOperation::AcknowledgeSettlement { expected } => {
                reply.command_settlement_acknowledged(expected)?;
            }
            CommandJournalOperation::Outcome { expected } => {
                reply.command_outcome(expected)?;
            }
            CommandJournalOperation::Scope => {
                let (_, authority) = reply.command_recovery(
                    self.authority.user,
                    self.authority.project,
                    &self.authority.transaction,
                )?;
                if authority != &self.authority {
                    return Err("Original command issuer changed".into());
                }
            }
            CommandJournalOperation::Worker { operation } => {
                reply.command_worker(operation)?;
            }
        }
        Ok(reply)
    }
    pub(crate) async fn acknowledge_settlement(
        &self,
        expected: &transaction::WorkspaceCommandRecovery,
    ) -> Result<(), String> {
        self.send(CommandJournalOperation::AcknowledgeSettlement {
            expected: Box::new(expected.clone()),
        })
        .await?
        .command_settlement_acknowledged(expected)
    }
}
impl JournalBackend for OriginalCommandJournal {
    async fn command_outcome(
        &self,
        expected: &transaction::WorkspaceCommandRecovery,
    ) -> Result<openwebide_core::plugins::languages::intelligence::actions::CommandOutcome, String>
    {
        let reply = self
            .send(CommandJournalOperation::Outcome {
                expected: Box::new(expected.clone()),
            })
            .await?;
        Ok(reply.command_outcome(expected)?.clone())
    }
    fn recovery_origin(&self) -> Option<&str> {
        Some(&self.origin)
    }
    async fn request(
        &self,
        request: &WorkspaceJournalRequest,
    ) -> Result<WorkspaceJournalResponse, String> {
        if request.user != self.authority.user
            || request.project != self.authority.project
            || request.id != self.authority.transaction
        {
            return Err("Original command cleanup changed scope".into());
        }
        match &request.operation {
            WorkspaceJournalOperation::CommandRecoveryScope => {
                self.send(CommandJournalOperation::Scope).await
            }
            WorkspaceJournalOperation::Command { operation } => {
                self.send(CommandJournalOperation::Worker {
                    operation: operation.clone(),
                })
                .await
            }
            _ => Err("Original command cleanup cannot admit workspace work".into()),
        }
    }
}
