//! Shared code-intelligence workflow. Native adapters supply authenticated RPC
//! and a project filesystem root, never provider-selection or source policy.
use super::*;
use openwebide_core::plugins::languages::intelligence::{self as protocol, Call, Query, Response};

/// Advertise only capabilities implemented by the shared host/client contract.
/// Servers return edits for review/application through the editor; unsolicited
/// workspace mutation and snippet-only completion insertion are not advertised.
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
            "workspace":{"configuration":true,"workspaceFolders":true,"applyEdit":false},
            "window":{"workDoneProgress":true},
            "textDocument": {
                "synchronization":{"didSave":true},
                "publishDiagnostics":{"versionSupport":true},
                "diagnostic":{"dynamicRegistration":false,"relatedDocumentSupport":false},
                "hover":{"contentFormat":["markdown","plaintext"]},
                "completion":{"completionItem":{"snippetSupport":false,"documentationFormat":["markdown","plaintext"],"resolveSupport":{"properties":["documentation","detail","additionalTextEdits"]}}},
                "definition":{"linkSupport":true},
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
    /// Retain shared transport, authority checks and request ordering while
    /// synchronizing another dialect served by this same installed server.
    pub fn for_server_language(&self, provider: RegisteredLanguage) -> Result<Self, String> {
        if !self.provider.shares_server(&provider) {
            return Err("The language belongs to a different server".into());
        }
        select_language_provider(&self.bindings, &provider)?;
        Ok(self.for_provider(provider))
    }

    pub async fn query_intelligence(
        &self,
        client: &str,
        document: LanguageDocumentRequest,
        query: Query,
    ) -> Result<Response, String> {
        let service = if matches!(query, Query::Formatting { .. }) {
            let registry = LanguageRegistry::from_bindings(&self.bindings);
            match registry.formatter(&self.provider.definition.id, &self.preferences) {
                LanguageSelection::Selected(provider) => self.for_provider(provider.clone()),
                LanguageSelection::Plain => {
                    return Err("Formatting is disabled or no formatter is installed".into());
                }
                LanguageSelection::Ambiguous(_) => {
                    return Err("Choose a formatter before formatting this language".into());
                }
                LanguageSelection::Unavailable(_) => {
                    return Err("The preferred formatter is unavailable".into());
                }
            }
        } else {
            self.clone()
        };
        service
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
