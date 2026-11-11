//! Independent lint/analysis policy above both authenticated host adapters.
use super::*;

#[derive(Clone, Debug)]
pub struct ProviderDiagnostics {
    pub provider: RegisteredLanguage,
    pub diagnostics: Vec<protocol::Diagnostic>,
    pub ready: bool,
    pub server: Option<String>,
    pub error: Option<String>,
}

/// Bound the combined view, preserving explicit per-provider failure instead
/// of silently truncating diagnostics or replacing another provider's results.
pub fn merge_diagnostics(
    mut previous: Vec<ProviderDiagnostics>,
    updates: Vec<ProviderDiagnostics>,
) -> Vec<ProviderDiagnostics> {
    for report in updates {
        if let Some(old) = previous
            .iter_mut()
            .find(|old| old.provider.namespace == report.provider.namespace)
        {
            *old = report;
        } else {
            previous.push(report);
        }
    }
    previous.sort_by_key(|report| {
        !report
            .provider
            .definition
            .features
            .language_server
            .as_ref()
            .is_some_and(|server| server.role.is_language())
    });
    let mut count = 0_usize;
    let mut bytes = 0_usize;
    for report in &mut previous {
        let size = serde_json::to_vec(&report.diagnostics).map_or(usize::MAX, |data| data.len());
        if report.diagnostics.len() > protocol::MAX_DIAGNOSTICS.saturating_sub(count)
            || size > protocol::MAX_RESULT_BYTES.saturating_sub(bytes)
        {
            report.diagnostics.clear();
            report.ready = false;
            report.error = Some("Provider diagnostics exceed the combined document limit".into());
        } else {
            count += report.diagnostics.len();
            bytes += size;
        }
    }
    previous
}
impl<H: PluginActionHost> LanguageService<H> {
    pub async fn query_diagnostics(
        &self,
        project: i64,
        client: &str,
        document: LanguageDocumentRequest,
        open: &[LanguageDocumentRequest],
        history: &IntelligenceDocuments,
        saved: bool,
    ) -> Result<Vec<ProviderDiagnostics>, String> {
        let registry = LanguageRegistry::from_bindings(&self.bindings);
        let mut providers = registry
            .diagnostic_providers(&document.path, &document.source, None, &self.preferences)?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        if self
            .provider
            .definition
            .features
            .language_server
            .as_ref()
            .is_some_and(|server| server.role.is_language())
        {
            providers.insert(0, self.provider.as_ref().clone());
        }
        let mut reports = Vec::new();
        // Interactive requests can cancel this future between each bounded
        // provider query. Do not launch an unbounded fan-out of native servers.
        for provider in providers {
            if !self.host.is_current() {
                return Err("Language service context changed".into());
            }
            let service = self.for_provider(provider.clone());
            let queried = async {
                service
                    .synchronize_intelligence(project, client, &document, open, history)
                    .await?;
                service
                    .query_selected_intelligence(
                        client,
                        document.clone(),
                        if saved {
                            Query::Saved
                        } else {
                            Query::Synchronize
                        },
                    )
                    .await
            }
            .await;
            if !self.host.is_current() {
                return Err("Language service context changed".into());
            }
            reports.push(match queried {
                Ok(response) => ProviderDiagnostics {
                    provider,
                    diagnostics: response.diagnostics,
                    ready: response.diagnostics_ready,
                    server: Some(response.server),
                    error: None,
                },
                Err(error) => ProviderDiagnostics {
                    provider,
                    diagnostics: vec![],
                    ready: false,
                    server: None,
                    error: Some(error),
                },
            });
        }
        Ok(merge_diagnostics(vec![], reports))
    }

    pub fn diagnostic_service(
        &self,
        namespace: &str,
        path: &str,
        source: &str,
    ) -> Result<Self, String> {
        let registry = LanguageRegistry::from_bindings(&self.bindings);
        let provider = registry
            .diagnostic_providers(path, source, None, &self.preferences)?
            .into_iter()
            .find(|provider| provider.namespace == namespace)
            .ok_or("The diagnostics provider is unavailable or disabled for this document")?;
        Ok(self.for_provider(provider.clone()))
    }
}
