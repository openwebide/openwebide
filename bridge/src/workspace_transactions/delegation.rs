//! Native HTTP adapter for one Local journal. Its caller holds physical folder
//! exclusion around admission and every operation authorized by the returned
//! scope; credentials never include the deployment-wide backend secret.
use crate::runs::http_client::ReqwestHttpClient;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::Request;
use openwebide_core::workspace_transaction::{
    DelegatedWorkspaceJournalOperation as Operation, DelegatedWorkspaceJournalRequest,
    RECOVERY_SCOPE_BODY_LIMIT, TransactionProgress, WorkspaceJournalDelegation,
    WorkspaceJournalResponse, WorkspaceTransactionAdvanced, WorkspaceTransactionScope,
    WorkspaceTransactionWorker,
};

pub struct PairedJournal {
    url: String,
    delegation: WorkspaceJournalDelegation,
    worker: WorkspaceTransactionWorker,
    http: ReqwestHttpClient,
}

#[cfg(test)]
mod tests;
impl PairedJournal {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn identity(&self) -> (&str, &str) {
        (&self.delegation.scope.id, &self.delegation.scope.generation)
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn matches(&self, api_url: &str, delegation: &WorkspaceJournalDelegation) -> bool {
        self.url
            == format!(
                "{}/workspace-transactions/delegated",
                api_url.trim_end_matches('/')
            )
            && self.delegation == *delegation
    }
    pub fn new(
        api_url: String,
        root_identity: &str,
        delegation: WorkspaceJournalDelegation,
        worker: WorkspaceTransactionWorker,
    ) -> Result<Self, String> {
        delegation.validate()?;
        worker.validate()?;
        let uri = api_url.parse::<hyper::Uri>().map_err(|e| e.to_string())?;
        if !matches!(uri.scheme_str(), Some("http" | "https"))
            || uri.authority().is_none()
            || uri.query().is_some()
            || delegation.scope.root.mode != openwebide_core::WorkspaceMode::Local
            || delegation.scope.root_identity != root_identity
        {
            return Err("The delegated journal does not match the proven physical folder".into());
        }
        Ok(Self {
            url: format!(
                "{}/workspace-transactions/delegated",
                api_url.trim_end_matches('/')
            ),
            delegation,
            worker,
            // Never inherit model or deployment authorization headers.
            http: ReqwestHttpClient::default(),
        })
    }
    pub async fn admit(&self) -> Result<WorkspaceTransactionScope, String> {
        self.admit_reviewed(None).await
    }
    pub async fn admit_reviewed(
        &self,
        review: Option<(&str, &str)>,
    ) -> Result<WorkspaceTransactionScope, String> {
        let mut expected = self.delegation.scope.clone();
        expected.worker = Some(self.worker.state());
        if let Some((digest, _)) = review {
            expected.accepted_language_review_digest = Some(digest.into());
        }
        match self
            .request(Operation::Admit {
                review_digest: review.map(|(digest, _)| digest.into()),
                language_grant: review.map(|(_, grant)| grant.into()),
            })
            .await?
        {
            WorkspaceJournalResponse::WorkerClaimed { scope } if scope == expected => Ok(scope),
            _ => Err("Paired worker admission was not acknowledged".into()),
        }
    }
    pub async fn scope(&self) -> Result<WorkspaceTransactionScope, String> {
        self.scope_for(Operation::Scope).await
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) async fn mutation_scope(&self) -> Result<WorkspaceTransactionScope, String> {
        self.scope_for(Operation::MutationScope).await
    }
    async fn scope_for(&self, operation: Operation) -> Result<WorkspaceTransactionScope, String> {
        match self.request(operation).await? {
            WorkspaceJournalResponse::Scope { scope: Some(scope) }
                if scope.id == self.delegation.scope.id
                    && scope.generation == self.delegation.scope.generation
                    && scope.root_identity == self.delegation.scope.root_identity
                    && scope.root == self.delegation.scope.root
                    && scope.worker == Some(self.worker.state()) =>
            {
                scope.validate()?;
                Ok(scope)
            }
            _ => Err("The paired worker no longer owns this journal".into()),
        }
    }
    pub async fn advance(
        &self,
        revision: u64,
        progress: TransactionProgress,
    ) -> Result<WorkspaceTransactionAdvanced, String> {
        match self
            .request(Operation::Advance { revision, progress })
            .await?
        {
            WorkspaceJournalResponse::Advanced { response } if response.editor.is_none() => {
                response.validate(&self.delegation.scope.generation, revision)?;
                Ok(response)
            }
            _ => Err("Invalid paired workspace cursor acknowledgement".into()),
        }
    }
    pub async fn release(&self) -> Result<(), String> {
        match self.request(Operation::Release).await? {
            WorkspaceJournalResponse::WorkerReleased { generation, worker }
                if generation == self.delegation.scope.generation && worker == self.worker.id =>
            {
                Ok(())
            }
            _ => Err("Paired worker release was not acknowledged".into()),
        }
    }
    async fn request(&self, operation: Operation) -> Result<WorkspaceJournalResponse, String> {
        let response_limit = if matches!(operation, Operation::Load) {
            openwebide_core::workspace_transaction::JOURNAL_API_BODY_LIMIT
        } else {
            RECOVERY_SCOPE_BODY_LIMIT
        };
        let request = DelegatedWorkspaceJournalRequest {
            delegation: self.delegation.clone(),
            worker: self.worker.clone(),
            operation,
        };
        request.validate()?;
        let body = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
        if body.len() > 64 * 1024 {
            return Err("Paired workspace request exceeds its limit".into());
        }
        let response = self
            .http
            .send(
                Request::builder()
                    .method("POST")
                    .uri(&self.url)
                    .header("content-type", "application/json")
                    .body(Full::new(Bytes::from(body)))
                    .map_err(|e| e.to_string())?,
            )
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status();
        let body = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            Limited::new(response.into_body(), response_limit).collect(),
        )
        .await
        .map_err(|_| "Paired workspace response timed out")?
        .map_err(|_| "Paired workspace response failed or exceeded its limit")?
        .to_bytes();
        if !status.is_success() {
            return Err(serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_owned))
                .unwrap_or_else(|| format!("Paired workspace service returned {status}")));
        }
        serde_json::from_slice(&body).map_err(|e| e.to_string())
    }
}

// Translate transport primitives only. The same native worker and shared
// transaction interpreter own admission, execution, rollback and release.
impl super::JournalBackend for PairedJournal {
    async fn request(
        &self,
        input: &openwebide_core::workspace_transaction::WorkspaceJournalRequest,
    ) -> Result<WorkspaceJournalResponse, String> {
        use openwebide_core::workspace_transaction::WorkspaceJournalOperation as Native;
        if input.user != self.delegation.user
            || input.project != self.delegation.project
            || input.id != self.delegation.scope.id
        {
            return Err("The paired journal belongs to another account or transaction".into());
        }
        let owns = |worker: &WorkspaceTransactionWorker, generation: &str| {
            worker.id == self.worker.id
                && worker.secret == self.worker.secret
                && generation == self.delegation.scope.generation
        };
        match &input.operation {
            Native::Scope => self.request(Operation::Inspect).await,
            Native::Load => self.request(Operation::Load).await,
            Native::ClaimWorker {
                scope,
                worker,
                review_digest,
                language_grant,
            } => {
                if scope != &self.delegation.scope || !owns(worker, &scope.generation) {
                    return Err("The paired worker admission changed identity".into());
                }
                let review = openwebide_core::workspace_transaction::language_review_admission(
                    review_digest.as_deref(),
                    language_grant.as_deref(),
                )?;
                self.admit_reviewed(review)
                    .await
                    .map(|scope| WorkspaceJournalResponse::WorkerClaimed { scope })
            }
            Native::AdvanceOwned {
                worker,
                generation,
                revision,
                progress,
            } if owns(worker, generation) => self
                .advance(*revision, progress.clone())
                .await
                .map(|response| WorkspaceJournalResponse::Advanced { response }),
            Native::ReleaseWorker { worker, generation } if owns(worker, generation) => {
                self.request(Operation::Release).await
            }
            _ => Err("The paired journal requires its original private worker credential".into()),
        }
    }
}
