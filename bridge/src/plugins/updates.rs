//! Native Git, RPC and timer primitives for the shared update-check worker.
use crate::{ServerConfig, runs::backend_client::BackendClient};
use openwebide_agent::plugins::updates::UpdateHost;
use openwebide_core::plugins::{
    marketplace::{CachedMarketplace, MarketplaceSource},
    updates::{CatalogHost, UpdateServiceRequest, UpdateServiceResponse},
};
use std::{sync::Arc, time::Duration};

struct Host {
    backend: Arc<BackendClient>,
    installer: super::NativePluginInstaller,
    owner: String,
}
impl CatalogHost for Host {
    async fn catalog(&self, source: &MarketplaceSource) -> Result<CachedMarketplace, String> {
        let now = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        )
        .unwrap_or(i64::MAX);
        tokio::time::timeout(
            Duration::from_secs(30),
            self.installer.catalog(&self.owner, source, now),
        )
        .await
        .map_err(|_| "Marketplace check timed out".to_string())?
        .map_err(|error| error.to_string())
    }
}
impl UpdateHost for Host {
    async fn request(
        &self,
        request: UpdateServiceRequest,
    ) -> Result<UpdateServiceResponse, String> {
        tokio::time::timeout(
            Duration::from_secs(15),
            self.backend.plugin_update_service(&request),
        )
        .await
        .map_err(|_| "Plugin update service request timed out".to_string())?
    }
    async fn wait(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
    fn report(&self, error: &str) {
        tracing::debug!(%error, "plugin update check unavailable");
    }
}
pub async fn serve(config: ServerConfig) {
    let server = config.pairing_token.is_none();
    let host_id = if server {
        "server".into()
    } else {
        crate::scheduled::host(&config).id
    };
    let host = Host {
        backend: Arc::new(BackendClient::new(
            config.backend_url,
            config.secret,
            crate::runs::http_client::ReqwestHttpClient::default(),
        )),
        installer: config.plugins,
        // Catalog contents are public; this separate namespace never loads
        // package snapshots or borrows another account's execution authority.
        owner: if server {
            "update-catalogs".into()
        } else {
            "paired".into()
        },
    };
    openwebide_agent::plugins::updates::serve(host, host_id, server).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwebide_core::plugins::marketplace::{CatalogRelease, CatalogReleaseSource};
    use std::path::Path;
    use tokio::process::Command;

    async fn git(path: &Path, arguments: &[&str]) -> String {
        let result = Command::new("git")
            .current_dir(path)
            .args(arguments)
            .output()
            .await
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap().trim().into()
    }
    #[tokio::test]
    async fn plugin_update_native_catalog_adapters_fetch_new_releases_and_report_outages() {
        let repository = tempfile::tempdir().unwrap();
        let directory = repository.path().join("plugins.git");
        std::fs::create_dir(&directory).unwrap();
        git(&directory, &["init", "-q"]).await;
        let mut catalog = openwebide_core::plugins::testing::catalog().catalog;
        std::fs::write(
            directory.join("marketplace.json"),
            serde_json::to_vec(&catalog).unwrap(),
        )
        .unwrap();
        git(&directory, &["add", "."]).await;
        git(
            &directory,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.org",
                "commit",
                "-qm",
                "first",
            ],
        )
        .await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut daemon = Command::new("git")
            .args([
                "daemon",
                "--reuseaddr",
                "--export-all",
                "--listen=127.0.0.1",
                &format!("--port={port}"),
                &format!("--base-path={}", repository.path().display()),
            ])
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut ready = false;
        for _ in 0..100 {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(ready);
        let source = MarketplaceSource {
            repository: format!("git://127.0.0.1:{port}/plugins.git"),
            reference: String::new(),
            path: "marketplace.json".into(),
        };
        let mut adapters = Vec::new();
        // These are the concrete server and paired cache adapters selected by serve.
        for owner in ["update-catalogs", "paired"] {
            let cache = tempfile::tempdir().unwrap();
            let host = Host {
                backend: Arc::new(BackendClient::new(
                    "http://127.0.0.1:1/api".into(),
                    "secret".into(),
                    crate::runs::http_client::ReqwestHttpClient::default(),
                )),
                installer: super::super::NativePluginInstaller::new(Some(cache.path().into())),
                owner: owner.into(),
            };
            assert_eq!(
                host.catalog(&source).await.unwrap().catalog.plugins[0]
                    .releases
                    .len(),
                1
            );
            adapters.push((cache, host));
        }
        catalog.plugins[0].releases.push(CatalogRelease {
            version: "0.1.1".into(),
            source: CatalogReleaseSource {
                commit: "b".repeat(40),
                path: "plugins/pr-review".into(),
            },
        });
        std::fs::write(
            directory.join("marketplace.json"),
            serde_json::to_vec(&catalog).unwrap(),
        )
        .unwrap();
        git(&directory, &["add", "."]).await;
        git(
            &directory,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.org",
                "commit",
                "-qm",
                "update",
            ],
        )
        .await;
        for (_, host) in &adapters {
            assert_eq!(
                host.catalog(&source).await.unwrap().catalog.plugins[0]
                    .releases
                    .len(),
                2
            );
        }
        daemon.kill().await.unwrap();
        daemon.wait().await.unwrap();
        // A daemon child can outlive its listener briefly. Remove this URL
        // deterministically while retaining both adapter caches.
        std::fs::rename(&directory, repository.path().join("unavailable.git")).unwrap();
        for (_, host) in &adapters {
            assert!(host.catalog(&source).await.is_err());
        }
    }
}
