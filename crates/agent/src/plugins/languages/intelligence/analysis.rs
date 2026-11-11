//! An immutable result retains its original service across a later user decision.
use super::*;
use futures::future::BoxFuture;

#[derive(Clone)]
pub struct LanguageAnalysis {
    data: Arc<AnalysisData>,
    validate: Arc<dyn Fn() -> BoxFuture<'static, Result<(), String>> + Send + Sync>,
}

struct AnalysisData {
    provider: RegisteredLanguage,
    prepared: openwebide_core::plugins::PreparedPlugin,
    client: String,
    document: LanguageDocumentRequest,
    request: u64,
    query: Query,
    context: protocol::AnalysisContext,
    result: serde_json::Value,
}

impl AnalysisData {
    fn origin(&self) -> protocol::Request {
        origin(
            &self.provider,
            &self.client,
            &self.document,
            self.request,
            &self.query,
        )
    }
}

fn origin(
    provider: &RegisteredLanguage,
    client: &str,
    document: &LanguageDocumentRequest,
    request: u64,
    query: &Query,
) -> protocol::Request {
    protocol::Request {
        version: protocol::VERSION,
        client: client.into(),
        document: document.document.clone(),
        request,
        revision: document.revision,
        source_digest: source_digest(&document.source),
        language: provider.definition.id.clone(),
        path: document.path.clone(),
        source: document.source.as_ref().clone(),
        query: query.clone(),
    }
}

impl LanguageAnalysis {
    pub fn provider(&self) -> &RegisteredLanguage {
        &self.data.provider
    }

    pub fn query(&self) -> &Query {
        &self.data.query
    }

    pub fn document(&self) -> &LanguageDocumentRequest {
        &self.data.document
    }

    pub fn result(&self) -> &serde_json::Value {
        &self.data.result
    }

    pub fn context(&self) -> &protocol::AnalysisContext {
        &self.data.context
    }

    pub fn origin(&self) -> protocol::Request {
        self.data.origin()
    }

    /// Capture the exact retained result for durable resource review. This
    /// makes no transport request and supplies no execution authority.
    pub fn language_review(
        &self,
        plan: &openwebide_core::workspace_transaction::WorkspacePlan,
        editor: &openwebide_core::workspace_transaction::EditorRecoveryPatch,
    ) -> Result<openwebide_core::workspace_transaction::LanguageReview, String> {
        use openwebide_core::workspace_transaction::LanguageReview;
        let review = LanguageReview {
            prepared: self.data.prepared.clone(),
            binding_revision: self.data.provider.installation_revision,
            input: self.data.origin(),
            context: self.data.context.clone(),
            result: self.data.result.clone(),
            transaction_digest: LanguageReview::transaction_digest(plan, editor)?,
        };
        review.validate_transaction(plan, editor)?;
        Ok(review)
    }

    /// Validate on the original service without synchronizing overlays or
    /// starting a replacement. The same facade rechecks authority in both modes.
    pub async fn validate(&self) -> Result<(), String> {
        (self.validate)().await
    }
}

impl<H: PluginActionHost + 'static> LanguageService<H> {
    /// Retain a matching reply for a later review/application. Capturing makes
    /// no transport request and grants no mutation authority. Unproved read-only
    /// replies remain usable without becoming retained, verified analysis.
    pub fn retain_analysis(
        &self,
        client: &str,
        document: LanguageDocumentRequest,
        query: Query,
        response: &Response,
    ) -> Result<Option<LanguageAnalysis>, String> {
        if !self.host.is_current() || !query.produces_analysis() {
            return Err("The original language analysis is unavailable".into());
        }
        let prepared = select_language_provider(&self.bindings, &self.provider)?.clone();
        let origin = origin(&self.provider, client, &document, response.request, &query);
        origin.validate()?;
        response.validate(&origin)?;
        let data = Arc::new(AnalysisData {
            provider: self.provider.as_ref().clone(),
            prepared,
            client: client.into(),
            document,
            request: response.request,
            query,
            context: match &response.context {
                Some(context) => context.clone(),
                None => return Ok(None),
            },
            result: response.result.clone(),
        });
        let service = self.clone();
        let captured = data.clone();
        Ok(Some(LanguageAnalysis {
            data,
            validate: Arc::new(move || {
                let service = service.clone();
                let captured = captured.clone();
                Box::pin(async move {
                    service
                        .validate_analysis(
                            &captured.client,
                            captured.document.clone(),
                            &captured.context,
                            &captured.result,
                        )
                        .await
                })
            }),
        }))
    }
}
