//! One bounded open-buffer/close policy for primary and independent services.
use super::*;
use std::collections::BTreeMap;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ServiceIdentity {
    project: i64,
    provider: String,
    host: String,
    client: String,
    digest: String,
    installation_revision: i64,
    server: String,
}
#[derive(Clone, Default)]
pub struct IntelligenceDocuments {
    entries: Arc<Mutex<BTreeMap<ServiceIdentity, Inventory>>>,
    sequence: Arc<AtomicU64>,
}
struct Inventory {
    ticket: u64,
    paths: BTreeMap<String, String>,
}

impl<H: PluginActionHost> LanguageService<H> {
    pub async fn prepare_action_synchronization(
        &self,
        approval: &protocol::actions::ActionApproval,
        tab_width: usize,
    ) -> Result<(), String> {
        let action = protocol::actions::ActionSynchronization::from_approval(approval)?;
        if self.provider.definition.id != approval.input.language
            || self.provider.plugin_namespace != approval.prepared.storage_namespace()
            || self.provider.digest != approval.prepared.digest
            || self.provider.installation_revision != approval.binding_revision
        {
            return Err("The reviewed action belongs to another language service".into());
        }
        self.query_selected_intelligence(
            &approval.input.client,
            LanguageDocumentRequest {
                document: approval.input.document.clone(),
                path: approval.input.path.clone(),
                source: Arc::new(approval.input.source.clone()),
                revision: approval.input.revision,
                tab_width,
            },
            Query::PrepareAction { action },
        )
        .await?;
        Ok(())
    }
    /// Publish the complete post-action overlay before its attached command.
    /// A removed origin closes its old document; remaining dialects still sync
    /// through their normal declared providers and the same client inventory.
    pub async fn synchronize_action(
        &self,
        project: i64,
        approval: &protocol::actions::ActionApproval,
        recovery: &openwebide_core::editor::EditorRecovery,
        revision: u64,
        tab_width: usize,
        history: &IntelligenceDocuments,
    ) -> Result<(), String> {
        approval.validate()?;
        recovery.validate()?;
        if self.provider.definition.id != approval.input.language
            || self.provider.plugin_namespace != approval.prepared.storage_namespace()
            || self.provider.digest != approval.prepared.digest
            || self.provider.installation_revision != approval.binding_revision
        {
            return Err("The approved command belongs to another language service".into());
        }
        let revision = revision.max(
            approval
                .input
                .revision
                .checked_add(1)
                .ok_or("Language revision exhausted")?,
        );
        let open = recovery
            .files
            .iter()
            .filter_map(|file| {
                file.document
                    .as_ref()
                    .map(|document| LanguageDocumentRequest {
                        document: if file.path == approval.input.path {
                            approval.input.document.clone()
                        } else {
                            source_digest(&file.path)
                        },
                        path: file.path.clone(),
                        source: document.text.clone(),
                        revision,
                        tab_width,
                    })
            })
            .collect::<Vec<_>>();
        let active = open
            .iter()
            .find(|document| document.path == approval.input.path)
            .cloned();
        let synchronization = protocol::actions::ActionSynchronization::from_approval(approval)?;
        let query = Query::SynchronizeAction {
            action: synchronization.clone(),
            close: active.is_none(),
        };
        let active = active.unwrap_or_else(|| LanguageDocumentRequest {
            document: approval.input.document.clone(),
            path: approval.input.path.clone(),
            source: Arc::new(String::new()),
            revision,
            tab_width,
        });
        self.synchronize_documents(
            project,
            &approval.input.client,
            &active,
            &open,
            history,
            Some(&synchronization),
        )
        .await?;
        self.query_selected_intelligence(&approval.input.client, active, query)
            .await?;
        Ok(())
    }
    pub async fn synchronize_intelligence(
        &self,
        project: i64,
        client: &str,
        active: &LanguageDocumentRequest,
        open: &[LanguageDocumentRequest],
        history: &IntelligenceDocuments,
    ) -> Result<(), String> {
        self.synchronize_documents(project, client, active, open, history, None)
            .await
    }
    async fn synchronize_documents(
        &self,
        project: i64,
        client: &str,
        active: &LanguageDocumentRequest,
        open: &[LanguageDocumentRequest],
        history: &IntelligenceDocuments,
        synchronization: Option<&protocol::actions::ActionSynchronization>,
    ) -> Result<(), String> {
        if project <= 0
            || !protocol::relative_path(&active.path)
            || active.source.len() > MAX_SOURCE_BYTES
            || open.len() > openwebide_core::editor::MAX_RECOVERY_FILES
        {
            return Err("Open language buffers exceed the project service limit".into());
        }
        if !self.host.is_current() {
            return Err("Language service context changed".into());
        }
        let registry = LanguageRegistry::from_bindings(&self.bindings);
        let server = self
            .provider
            .definition
            .features
            .language_server
            .as_ref()
            .ok_or("The language server is unavailable")?;
        let mut sources = Vec::new();
        let mut paths = BTreeMap::new();
        for source in open {
            if source.path == active.path {
                continue;
            }
            if !protocol::relative_path(&source.path) {
                return Err("Invalid open language document".into());
            }
            let provider = registry.server_document(
                &self.provider,
                &source.path,
                &source.source,
                &self.preferences,
            )?;
            if let Some(provider) = provider {
                if source.source.len() > MAX_SOURCE_BYTES {
                    return Err(
                        "An unsaved dependency exceeds the language-service source limit".into(),
                    );
                }
                if paths
                    .insert(source.path.clone(), provider.definition.id.clone())
                    .is_some()
                {
                    return Err("Repeated open language document".into());
                }
                sources.push((provider.clone(), source.clone()));
            }
        }
        paths.insert(active.path.clone(), self.provider.definition.id.clone());
        if paths.len() > 64
            || sources
                .iter()
                .map(|(_, source)| source.source.len())
                .sum::<usize>()
                .saturating_add(active.source.len())
                > 32 * 1024 * 1024
        {
            return Err("Open language buffers exceed the project service limit".into());
        }
        let key = ServiceIdentity {
            project,
            provider: self.provider.plugin_namespace.clone(),
            host: self.provider.host_id.clone(),
            client: client.into(),
            digest: self.provider.digest.clone(),
            installation_revision: self.provider.installation_revision,
            server: serde_json::to_string(&server.shared_configuration())
                .map_err(|error| error.to_string())?,
        };
        let ticket = history
            .sequence
            .fetch_add(1, Ordering::Relaxed)
            .checked_add(1)
            .ok_or("Language synchronization sequence exhausted")?;
        let closed = {
            let mut entries = history.entries.lock().unwrap();
            entries.retain(|existing, _| {
                existing.project != project
                    || existing.provider != key.provider
                    || existing.host != key.host
                    || existing.client != client
                    || *existing == key
            });
            if entries.len() >= 64 && !entries.contains_key(&key) {
                return Err("Too many retained language service inventories".into());
            }
            let previous = entries
                .get(&key)
                .map_or_else(BTreeMap::new, |inventory| inventory.paths.clone());
            let closed = previous
                .iter()
                .filter(|(path, _)| !paths.contains_key(*path))
                .map(|(path, language)| (path.clone(), language.clone()))
                .collect::<Vec<_>>();
            entries.insert(
                key.clone(),
                Inventory {
                    ticket,
                    paths: previous,
                },
            );
            closed
        };
        let current = || {
            self.host.is_current()
                && history
                    .entries
                    .lock()
                    .unwrap()
                    .get(&key)
                    .is_some_and(|inventory| inventory.ticket == ticket)
        };
        for (path, language) in closed {
            if !current() {
                return Err("Open language buffers were superseded".into());
            }
            let provider = registry
                .providers
                .iter()
                .find(|provider| {
                    provider.definition.id == language && self.provider.shares_server(provider)
                })
                .ok_or("The previous document provider is unavailable")?;
            let closed_path = path.clone();
            self.for_server_language(provider.clone())?
                .query_selected_intelligence(
                    client,
                    LanguageDocumentRequest {
                        document: source_digest(&path),
                        path,
                        source: Arc::new(String::new()),
                        revision: active.revision,
                        tab_width: active.tab_width,
                    },
                    synchronization.map_or(Query::Close, |action| Query::SynchronizeAction {
                        action: action.clone(),
                        close: true,
                    }),
                )
                .await?;
            if let Some(inventory) = history
                .entries
                .lock()
                .unwrap()
                .get_mut(&key)
                .filter(|inventory| inventory.ticket == ticket)
            {
                inventory.paths.remove(&closed_path);
            }
        }
        for (provider, source) in sources {
            if !current() {
                return Err("Open language buffers were superseded".into());
            }
            if let Some(inventory) = history
                .entries
                .lock()
                .unwrap()
                .get_mut(&key)
                .filter(|inventory| inventory.ticket == ticket)
            {
                inventory
                    .paths
                    .insert(source.path.clone(), provider.definition.id.clone());
            }
            self.for_server_language(provider)?
                .query_selected_intelligence(
                    client,
                    source,
                    synchronization.map_or(Query::Synchronize, |action| Query::SynchronizeAction {
                        action: action.clone(),
                        close: false,
                    }),
                )
                .await?;
        }
        if !current() {
            return Err("Open language buffers were superseded".into());
        }
        history
            .entries
            .lock()
            .unwrap()
            .insert(key, Inventory { ticket, paths });
        Ok(())
    }
}
