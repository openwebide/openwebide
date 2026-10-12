//! Agent-facing plugin lifecycle policy shared by every execution host.
use openwebide_core::plugins::{
    PluginPackage, PluginSource, PreparedPlugin, RecordPlugin,
    management::{InstallationPlan, ManagementRequest, ManagementSnapshot, PluginListing, search},
};
use std::future::Future;

/// Thin adapters supply authenticated metadata I/O and selected-host preparation.
/// Project/account identity is captured by the adapter and cannot be changed by tool arguments.
pub trait ManagementHost: Send + Sync {
    fn workspace(&self) -> Option<&dyn openwebide_core::Vfs> {
        None
    }
    fn metadata(
        &self,
        request: ManagementRequest,
    ) -> impl Future<Output = Result<ManagementSnapshot, String>> + Send;
    fn prepare(
        &self,
        source: &PluginSource,
    ) -> impl Future<Output = Result<PreparedPlugin, String>> + Send;
    fn package(
        &self,
        prepared: &PreparedPlugin,
    ) -> impl Future<Output = Result<PluginPackage, String>> + Send;
    fn current(&self) -> bool;
}

fn current(host: &impl ManagementHost) -> Result<(), String> {
    if host.current() {
        Ok(())
    } else {
        Err("Plugin management context changed; no further changes were applied.".into())
    }
}

pub async fn search_plugins(
    host: &impl ManagementHost,
    query: &str,
    installed_only: bool,
    limit: usize,
) -> Result<Vec<PluginListing>, String> {
    current(host)?;
    let snapshot = host.metadata(ManagementRequest::Snapshot).await?;
    current(host)?;
    search(
        &snapshot.marketplaces,
        &snapshot.installations,
        query,
        installed_only,
        limit,
    )
    .map_err(|error| error.to_string())
}

/// Prepare concrete immutable contents for the approval preview. This does not
/// activate the plugin, import its instructions or change account metadata.
pub async fn plan_installation(
    host: &impl ManagementHost,
    source: PluginSource,
    selection: Option<openwebide_core::plugins::management::CatalogSelection>,
) -> Result<InstallationReview, String> {
    current(host)?;
    let snapshot = host.metadata(ManagementRequest::Snapshot).await?;
    current(host)?;
    if let Some(selection) = &selection {
        let resolved = selection
            .resolve(&snapshot.marketplaces)
            .map_err(|error| error.to_string())?;
        if resolved != source {
            return Err("Selected source does not match the marketplace release.".into());
        }
    }
    let plan = InstallationPlan::new(source, selection, &snapshot.installations)
        .map_err(|error| error.to_string())?;
    let prepared = host.prepare(&plan.source).await?;
    current(host)?;
    plan.validate_prepared(&prepared)
        .map_err(|error| error.to_string())?;
    let package = host.package(&prepared).await?;
    current(host)?;
    let additions = plan.capability_additions(&prepared);
    let request = plan
        .request(&prepared, package)
        .map_err(|error| error.to_string())?;
    Ok(InstallationReview { request, additions })
}

/// A host-created review, retained by the tool executor through approval. The
/// model cannot replace its source, revision or capabilities after seeing a preview.
pub struct InstallationReview {
    request: RecordPlugin,
    additions: Vec<String>,
}
impl InstallationReview {
    pub fn revision(&self) -> Option<i64> {
        self.request.revision
    }
    pub fn prepared(&self) -> &PreparedPlugin {
        &self.request.prepared
    }
    pub fn additions(&self) -> &[String] {
        &self.additions
    }
    pub fn into_pending_request(self) -> RecordPlugin {
        RecordPlugin {
            approved_capabilities: self.additions,
            ..self.request
        }
    }
    pub async fn apply(
        mut self,
        host: &impl ManagementHost,
        approved: bool,
    ) -> Result<ManagementSnapshot, String> {
        if !approved {
            return Err("Plugin installation requires approval.".into());
        }
        current(host)?;
        self.request.approved_capabilities = self.additions;
        let snapshot = host
            .metadata(ManagementRequest::Record {
                request: Box::new(self.request),
            })
            .await?;
        current(host)?;
        Ok(snapshot)
    }
}

/// Enablement retains source and revision validation while registering the selected
/// host's receipt. Both the UI and agent use this orchestration.
pub async fn enable_plugin(
    host: &impl ManagementHost,
    identity: &openwebide_core::plugins::management::PluginIdentity,
    revision: i64,
    project_revision: Option<i64>,
) -> Result<ManagementSnapshot, String> {
    current(host)?;
    let snapshot = host.metadata(ManagementRequest::Snapshot).await?;
    current(host)?;
    if snapshot.project_id.is_none() {
        return Err("Open a project before enabling a plugin.".into());
    }
    let installed = snapshot
        .installations
        .iter()
        .find(|entry| identity.matches(&entry.prepared))
        .ok_or("Plugin is not installed.")?;
    if installed.revision != revision {
        return Err("Plugin changed; inspect its current revision before retrying.".into());
    }
    installed
        .prepared
        .manifest
        .validate_activation()
        .map_err(|error| error.to_string())?;
    let package = host.package(&installed.prepared).await?;
    current(host)?;
    openwebide_core::plugins::management::validate_loaded_plugin(&installed.prepared, &package)
        .map_err(|error| error.to_string())?;
    let registered = host
        .metadata(ManagementRequest::Record {
            request: Box::new(RecordPlugin {
                prepared: package.prepared.clone(),
                revision: Some(revision),
                package: None,
                approved_capabilities: Vec::new(),
                update_policy: None,
            }),
        })
        .await?;
    current(host)?;
    let registered = registered
        .installations
        .iter()
        .find(|entry| identity.matches(&entry.prepared))
        .ok_or("Plugin registration disappeared.")?;
    let result = host
        .metadata(ManagementRequest::Project {
            command: openwebide_core::plugins::ProjectPluginCommand::Enable {
                package: Box::new(package),
                installation_revision: registered.revision,
                revision: project_revision,
            },
        })
        .await?;
    current(host)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use openwebide_core::plugins::{marketplace::MarketplaceSettings, testing};
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    struct Host {
        active: AtomicBool,
        stale_after_prepare: bool,
        requests: Mutex<Vec<ManagementRequest>>,
    }
    impl Host {
        fn new(stale_after_prepare: bool) -> Self {
            Self {
                active: AtomicBool::new(true),
                stale_after_prepare,
                requests: Mutex::new(Vec::new()),
            }
        }
    }
    impl ManagementHost for Host {
        async fn metadata(&self, request: ManagementRequest) -> Result<ManagementSnapshot, String> {
            self.requests.lock().unwrap().push(request);
            Ok(ManagementSnapshot {
                project_id: None,
                installations: Vec::new(),
                marketplaces: MarketplaceSettings::default(),
                bindings: Vec::new(),
            })
        }
        async fn prepare(&self, source: &PluginSource) -> Result<PreparedPlugin, String> {
            assert_eq!(source, &testing::source());
            if self.stale_after_prepare {
                self.active.store(false, Ordering::SeqCst);
            }
            Ok(testing::receipt())
        }
        async fn package(&self, prepared: &PreparedPlugin) -> Result<PluginPackage, String> {
            assert_eq!(prepared, &testing::receipt());
            Ok(testing::package())
        }
        fn current(&self) -> bool {
            self.active.load(Ordering::SeqCst)
        }
    }

    #[test]
    fn review_is_inert_until_approved_and_keeps_exact_prepared_contents() {
        block_on(async {
            let host = Host::new(false);
            let review = plan_installation(&host, testing::source(), None)
                .await
                .unwrap();
            assert_eq!(review.prepared(), &testing::receipt());
            assert_eq!(host.requests.lock().unwrap().len(), 1);
            review.apply(&host, true).await.unwrap();
            let requests = host.requests.lock().unwrap();
            let ManagementRequest::Record { request } = &requests[1] else {
                panic!("expected record")
            };
            assert_eq!(request.prepared, testing::receipt());
            assert!(request.package.is_some());
        });
    }

    #[test]
    fn rejected_or_stale_reviews_never_record_installations() {
        block_on(async {
            let host = Host::new(false);
            let review = plan_installation(&host, testing::source(), None)
                .await
                .unwrap();
            assert!(review.apply(&host, false).await.is_err());
            assert_eq!(host.requests.lock().unwrap().len(), 1);
            let review = plan_installation(&host, testing::source(), None)
                .await
                .unwrap();
            host.active.store(false, Ordering::SeqCst);
            assert!(review.apply(&host, true).await.is_err());
            assert_eq!(host.requests.lock().unwrap().len(), 2);
        });
    }

    #[test]
    fn changing_run_context_during_preparation_cannot_reach_recording() {
        block_on(async {
            let host = Host::new(true);
            assert!(
                plan_installation(&host, testing::source(), None)
                    .await
                    .is_err()
            );
            assert_eq!(host.requests.lock().unwrap().len(), 1);
        });
    }
    #[test]
    fn projectless_enablement_never_prepares_or_registers_a_plugin() {
        block_on(async {
            let host = Host::new(false);
            let prepared = testing::receipt();
            let identity = openwebide_core::plugins::management::PluginIdentity {
                repository: prepared.source.repository,
                path: prepared.source.path,
                publisher: prepared.manifest.publisher,
                name: prepared.manifest.name,
            };
            let error = enable_plugin(&host, &identity, 1, None).await.unwrap_err();
            assert!(error.contains("Open a project"));
            assert_eq!(host.requests.lock().unwrap().len(), 1);
        });
    }
}
