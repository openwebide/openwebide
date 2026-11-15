//! HTTP primitives for an original Local command worker. File-worker grants
//! remain independent and cannot be reused after the file phase releases them.
use super::JournalBackend;
use crate::runs::http_client::ReqwestHttpClient;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::Request;
use openwebide_core::{
    WorkspaceMode,
    plugins::languages::intelligence::actions::{
        CommandJournalOperation, DelegatedCommandRequest, MAX_COMMAND_BYTES,
    },
    workspace_transaction::{
        RECOVERY_SCOPE_BODY_LIMIT, WorkspaceJournalDelegation, WorkspaceJournalOperation,
        WorkspaceJournalRequest, WorkspaceJournalResponse, WorkspaceTransactionWorker,
    },
};

#[derive(Clone)]
pub struct PairedCommandJournal {
    url: String,
    delegation: WorkspaceJournalDelegation,
    worker: WorkspaceTransactionWorker,
    http: ReqwestHttpClient,
}
impl PairedCommandJournal {
    pub fn new(
        api_url: String,
        root_identity: &str,
        delegation: WorkspaceJournalDelegation,
        worker: WorkspaceTransactionWorker,
    ) -> Result<Self, String> {
        delegation.validate()?;
        worker.validate()?;
        let uri = api_url
            .parse::<hyper::Uri>()
            .map_err(|_| "Invalid command journal address")?;
        if !matches!(uri.scheme_str(), Some("http" | "https"))
            || uri.authority().is_none()
            || uri.query().is_some()
            || delegation.scope.root.mode != WorkspaceMode::Local
            || delegation.scope.root_identity != root_identity
        {
            return Err("The command journal does not match the original physical folder".into());
        }
        Ok(Self {
            url: format!(
                "{}/language-commands/delegated",
                api_url.trim_end_matches('/')
            ),
            delegation,
            worker,
            http: ReqwestHttpClient::default(),
        })
    }
    async fn send(
        &self,
        operation: CommandJournalOperation,
    ) -> Result<WorkspaceJournalResponse, String> {
        let input = DelegatedCommandRequest {
            delegation: self.delegation.clone(),
            worker: self.worker.clone(),
            operation,
        };
        input.validate()?;
        let body = serde_json::to_vec(&input).map_err(|_| "Invalid paired command request")?;
        if body.len() > MAX_COMMAND_BYTES + RECOVERY_SCOPE_BODY_LIMIT {
            return Err("Paired command request exceeds its limit".into());
        }
        let response = self
            .http
            .send(
                Request::builder()
                    .method("POST")
                    .uri(&self.url)
                    .header("content-type", "application/json")
                    .body(Full::new(Bytes::from(body)))
                    .map_err(|_| "Invalid paired command request")?,
            )
            .await
            .map_err(|_| "Cannot reach the original command journal")?;
        let status = response.status();
        let response_limit = if matches!(input.operation, CommandJournalOperation::Outcome { .. }) {
            openwebide_core::plugins::languages::intelligence::actions::MAX_COMMAND_BYTES
                + RECOVERY_SCOPE_BODY_LIMIT
        } else {
            RECOVERY_SCOPE_BODY_LIMIT
        };
        let body = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            Limited::new(response.into_body(), response_limit).collect(),
        )
        .await
        .map_err(|_| "Paired command response timed out")?
        .map_err(|_| "Paired command response failed or exceeded its limit")?
        .to_bytes();
        if !status.is_success() {
            // Original private credentials and server diagnostics must not be
            // reflected into a browser-facing command failure.
            return Err(format!("Original paired command journal returned {status}"));
        }
        let reply: WorkspaceJournalResponse =
            serde_json::from_slice(&body).map_err(|_| "Invalid paired command response")?;
        let scope = match &input.operation {
            CommandJournalOperation::AcknowledgeSettlement { .. } => {
                return Err(
                    "Settlement acknowledgement requires the original native authority".into(),
                );
            }
            CommandJournalOperation::Outcome { expected } => {
                reply.command_outcome(expected)?;
                let WorkspaceJournalResponse::CommandOutcome { recovery, .. } = &reply else {
                    unreachable!()
                };
                &recovery.scope
            }
            CommandJournalOperation::Scope => {
                let (recovery, _) = reply.command_recovery(
                    self.delegation.user,
                    self.delegation.project,
                    &self.delegation.scope.id,
                )?;
                &recovery.scope
            }
            CommandJournalOperation::Worker { operation } => {
                &reply.command_worker(operation)?.scope
            }
        };
        if scope.generation != self.delegation.scope.generation
            || scope.root_identity != self.delegation.scope.root_identity
            || scope.root != self.delegation.scope.root
        {
            return Err("The original paired command journal changed".into());
        }
        Ok(reply)
    }
}
impl JournalBackend for PairedCommandJournal {
    async fn request(
        &self,
        input: &WorkspaceJournalRequest,
    ) -> Result<WorkspaceJournalResponse, String> {
        if input.user != self.delegation.user
            || input.project != self.delegation.project
            || input.id != self.delegation.scope.id
        {
            return Err("The paired command belongs to another account or transaction".into());
        }
        match &input.operation {
            WorkspaceJournalOperation::CommandRecoveryScope => {
                self.send(CommandJournalOperation::Scope).await
            }
            WorkspaceJournalOperation::Command { operation }
                if operation.worker().id == self.worker.id
                    && operation.worker().secret == self.worker.secret =>
            {
                self.send(CommandJournalOperation::Worker {
                    operation: operation.clone(),
                })
                .await
            }
            _ => Err("The paired command requires its original native worker credential".into()),
        }
    }
}
#[cfg(test)]
mod tests;
