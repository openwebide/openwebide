//! Shared code-intelligence workflow. Native adapters supply authenticated RPC
//! and a project filesystem root, never provider-selection or source policy.
use super::*;
use openwebide_core::plugins::languages::intelligence::{self as protocol, Call, Query, Response};
mod analysis;
mod diagnostics;
mod synchronization;
pub use analysis::LanguageAnalysis;
pub use diagnostics::{ProviderDiagnostics, merge_diagnostics};
pub use synchronization::IntelligenceDocuments;

/// Advertise only capabilities implemented by the shared host/client contract.
/// Servers return edits for review/application through the editor; unsolicited
/// workspace mutation is not advertised. Snippets use the shared Rust editor.
pub fn initialization_params(
    root_uri: &str,
    options: serde_json::Value,
    process_id: u32,
) -> serde_json::Value {
    serde_json::json!({
        "processId": process_id,
        "clientInfo": {"name":"Open WebIDE","version":env!("CARGO_PKG_VERSION")},
        "rootUri": root_uri,
        "workspaceFolders": [{"uri":root_uri,"name":"Project"}],
        "initializationOptions": options,
        "capabilities": {
            "general":{"positionEncodings":["utf-16"]},
            "workspace":{"configuration":true,"workspaceFolders":true,"applyEdit":false,"workspaceEdit":{"documentChanges":true,"resourceOperations":["create","rename","delete"]},"symbol":{"resolveSupport":{"properties":["location.range"]}}},
            "window":{"workDoneProgress":true},
            "textDocument": {
                "synchronization":{"didSave":true},
                "publishDiagnostics":{"versionSupport":true},
                "diagnostic":{"dynamicRegistration":false,"relatedDocumentSupport":false},
                "hover":{"contentFormat":["markdown","plaintext"]},
                "completion":{"completionItem":{"snippetSupport":true,"documentationFormat":["markdown","plaintext"],"resolveSupport":{"properties":["documentation","detail","additionalTextEdits"]}}},
                "definition":{"linkSupport":true},
                "typeDefinition":{"linkSupport":true},
                "implementation":{"linkSupport":true},
                "documentSymbol":{"hierarchicalDocumentSymbolSupport":true},
                "codeAction":{
                    "dataSupport":true,"disabledSupport":true,
                    "resolveSupport":{"properties":["edit"]},
                    "codeActionLiteralSupport":{"codeActionKind":{"valueSet":["","quickfix","refactor","refactor.extract","refactor.inline","refactor.rewrite","source","source.organizeImports"]}}
                }
            }
        }
    })
}

impl<H: PluginActionHost> LanguageService<H> {
    /// Revalidate an unchanged, retained result on its original native service.
    /// Callers retain the complete editor source snapshot used for analysis;
    /// this query neither synchronizes new overlays nor starts a replacement.
    pub async fn validate_analysis(
        &self,
        client: &str,
        document: LanguageDocumentRequest,
        context: &protocol::AnalysisContext,
        result: &serde_json::Value,
    ) -> Result<(), String> {
        context.validate_result(result)?;
        self.query_selected_intelligence(
            client,
            document,
            Query::ValidateAnalysis {
                context: context.clone(),
            },
        )
        .await?;
        Ok(())
    }
    /// Low-priority diagnostics/symbol warming through the same authority,
    /// source validation and transports as interactive queries. The host may
    /// decline it; callers must not treat optional failure as an editor error.
    pub async fn prefetch_intelligence(
        &self,
        client: &str,
        document: LanguageDocumentRequest,
        kind: protocol::PrefetchKind,
    ) -> Result<Response, String> {
        self.query_selected_intelligence(client, document, Query::Prefetch { kind })
            .await
    }
    /// Retain shared transport, authority checks and request ordering while
    /// synchronizing another dialect served by this same installed server.
    pub fn for_server_language(&self, provider: RegisteredLanguage) -> Result<Self, String> {
        if !self.provider.shares_server(&provider) {
            return Err("The language belongs to a different server".into());
        }
        select_language_provider(&self.bindings, &provider)?;
        Ok(self.for_provider(provider))
    }

    /// Resolve feature-specific routing before synchronizing buffers, so an
    /// independently selected formatter receives the same unsaved dependencies.
    pub fn service_for_query(&self, query: &Query) -> Result<Self, String> {
        if !matches!(
            query,
            Query::Formatting { .. } | Query::RangeFormatting { .. }
        ) {
            return Ok(self.clone());
        }
        let registry = LanguageRegistry::from_bindings(&self.bindings);
        match registry.formatter(&self.provider.definition.id, &self.preferences) {
            LanguageSelection::Selected(provider) => Ok(self.for_provider(provider.clone())),
            LanguageSelection::Plain => {
                Err("Formatting is disabled or no formatter is installed".into())
            }
            LanguageSelection::Ambiguous(_) => {
                Err("Choose a formatter before formatting this language".into())
            }
            LanguageSelection::Unavailable(_) => {
                Err("The preferred formatter is unavailable".into())
            }
        }
    }

    pub async fn query_intelligence(
        &self,
        client: &str,
        document: LanguageDocumentRequest,
        query: Query,
    ) -> Result<Response, String> {
        self.service_for_query(&query)?
            .query_selected_intelligence(client, document, query)
            .await
    }

    async fn query_selected_intelligence(
        &self,
        client: &str,
        document: LanguageDocumentRequest,
        query: Query,
    ) -> Result<Response, String> {
        if !self.host.is_current() {
            return Err("Language service context changed".into());
        }
        let server = protocol::declared_server(
            select_language_provider(&self.bindings, &self.provider)?,
            &self.provider.definition.id,
        )?;
        if !server.role.is_language() {
            if self.preferences.diagnostics.get(&self.provider.namespace) == Some(&false)
                && !matches!(query, Query::Close)
            {
                return Err("This diagnostics provider is disabled".into());
            }
            if !matches!(
                query,
                Query::Synchronize
                    | Query::Prefetch {
                        kind: protocol::PrefetchKind::Diagnostics
                    }
                    | Query::SynchronizeAction { .. }
                    | Query::PrepareAction { .. }
                    | Query::ReleaseAction { .. }
                    | Query::Saved
                    | Query::Close
                    | Query::CodeActions { .. }
                    | Query::ResolveAction { .. }
                    | Query::ReviewAction { .. }
                    | Query::ValidateAnalysis { .. }
            ) {
                return Err("This provider supplies diagnostics and fixes, not primary language intelligence".into());
            }
        }
        let selected = select_language_provider(&self.bindings, &self.provider)?;
        protocol::declared_server(selected, &self.provider.definition.id)?;
        let input = protocol::Request {
            version: protocol::VERSION,
            client: client.into(),
            document: document.document,
            request: self
                .sequence
                .fetch_add(1, Ordering::Relaxed)
                .checked_add(1)
                .ok_or("Language request sequence exhausted")?,
            revision: document.revision,
            language: self.provider.definition.id.clone(),
            path: document.path,
            source_digest: source_digest(&document.source),
            source: document.source.as_ref().clone(),
            query,
        };
        input.validate()?;
        let prepared = self.host.prepare(selected).await?;
        super::super::execution::validate_prepared_selection(selected, &prepared)?;
        let grant = self.host.grant(&prepared).await?;
        if !self.host.is_current() || grant.is_empty() {
            return Err("Language server authority is no longer available".into());
        }
        let call = Call {
            grant,
            session_id: None,
            prepared,
            input,
        };
        let response = self.host.language_server(&call).await?;
        if !self.host.is_current() {
            return Err("Language service context changed".into());
        }
        response.validate(&call.input)?;
        Ok(response)
    }
}
