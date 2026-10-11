//! One account-scoped discovery worker for server and paired host adapters.
use openwebide_core::plugins::updates::{
    CatalogHost, UpdateServiceRequest, UpdateServiceResponse, refresh_catalogs,
};
use std::{future::Future, time::Duration};

pub trait UpdateHost: CatalogHost {
    fn request(
        &self,
        request: UpdateServiceRequest,
    ) -> impl Future<Output = Result<UpdateServiceResponse, String>> + Send;
    fn wait(&self, duration: Duration) -> impl Future<Output = ()> + Send;
    fn report(&self, error: &str);
}
pub async fn check_page(
    host: &impl UpdateHost,
    host_id: &str,
    server: bool,
    after: i64,
) -> Result<Option<i64>, String> {
    let UpdateServiceResponse::Claimed { claims, next } = host
        .request(UpdateServiceRequest::Claim {
            host_id: host_id.into(),
            server,
            after,
        })
        .await?
    else {
        return Err("Unexpected plugin update claims".into());
    };
    for mut claim in claims {
        if claim.host_id != host_id {
            return Err("Plugin check belongs to another execution host".into());
        }
        let refresh = refresh_catalogs(host, claim.settings.clone()).await;
        // The acknowledgement needs the captured revision/sources, not a second
        // copy of up to sixteen cached catalogs.
        claim.settings.catalogs.clear();
        claim.settings.check = None;
        if !matches!(
            host.request(UpdateServiceRequest::Finish {
                claim: Box::new(claim),
                refresh: Box::new(refresh)
            })
            .await?,
            UpdateServiceResponse::Finished
        ) {
            return Err("Unexpected plugin update acknowledgement".into());
        }
    }
    Ok(next)
}
pub async fn serve(host: impl UpdateHost, host_id: String, server: bool) {
    let mut after = 0;
    loop {
        match check_page(&host, &host_id, server, after).await {
            Ok(next) => after = next.unwrap_or(0),
            Err(error) => {
                host.report(&error);
                after = 0;
            }
        }
        host.wait(Duration::from_secs(15)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use openwebide_core::plugins::{marketplace::*, testing::catalog, updates::UpdateCheckClaim};
    use std::sync::Mutex;

    struct Host {
        host_id: String,
        requests: Mutex<Vec<UpdateServiceRequest>>,
    }
    impl CatalogHost for Host {
        async fn catalog(&self, source: &MarketplaceSource) -> Result<CachedMarketplace, String> {
            if *source == MarketplaceSource::official() {
                return Err("offline".into());
            }
            Ok(catalog())
        }
    }
    impl UpdateHost for Host {
        async fn request(
            &self,
            request: UpdateServiceRequest,
        ) -> Result<UpdateServiceResponse, String> {
            let response = match &request {
                UpdateServiceRequest::Claim { .. } => UpdateServiceResponse::Claimed {
                    claims: vec![UpdateCheckClaim {
                        user_id: 42,
                        host_id: self.host_id.clone(),
                        lease: "a".repeat(32),
                        settings: MarketplaceSettings {
                            check: None,
                            revision: 9,
                            sources: vec![MarketplaceSource::official(), catalog().source],
                            catalogs: vec![],
                        },
                    }],
                    next: Some(42),
                },
                UpdateServiceRequest::Finish { .. } => UpdateServiceResponse::Finished,
            };
            self.requests.lock().unwrap().push(request);
            Ok(response)
        }
        async fn wait(&self, duration: Duration) {
            assert_eq!(duration, Duration::from_secs(15));
            futures::future::pending::<()>().await;
        }
        fn report(&self, _: &str) {}
    }
    #[test]
    fn plugin_update_worker_uses_same_discovery_contract_for_server_and_paired_hosts() {
        block_on(async {
            for (host_id, server) in [("server", true), ("paired-host", false)] {
                let host = Host {
                    host_id: host_id.into(),
                    requests: Mutex::default(),
                };
                assert_eq!(
                    check_page(&host, host_id, server, 0).await.unwrap(),
                    Some(42)
                );
                let requests = host.requests.lock().unwrap();
                assert!(
                    matches!(&requests[0], UpdateServiceRequest::Claim { server: actual, .. } if *actual == server)
                );
                let UpdateServiceRequest::Finish { claim, refresh } = &requests[1] else {
                    panic!("expected acknowledgement");
                };
                assert_eq!(claim.user_id, 42);
                assert_eq!(claim.settings.revision, 9);
                assert!(claim.settings.catalogs.is_empty());
                assert_eq!(refresh.settings.catalogs, vec![catalog()]);
                assert_eq!(refresh.failures.len(), 1);
            }
        });
    }
    #[test]
    fn plugin_update_worker_starts_without_a_client_and_waits_between_pages() {
        use std::{
            future::Future,
            task::{Context, Poll},
        };
        for server in [true, false] {
            let host = Host {
                host_id: "execution-host".into(),
                requests: Mutex::default(),
            };
            // Borrowed adapter lets the test inspect the real worker after its
            // first check, with no browser or sign-in callback driving it.
            struct Borrowed<'a>(&'a Host);
            impl CatalogHost for Borrowed<'_> {
                async fn catalog(
                    &self,
                    source: &MarketplaceSource,
                ) -> Result<CachedMarketplace, String> {
                    self.0.catalog(source).await
                }
            }
            impl UpdateHost for Borrowed<'_> {
                async fn request(
                    &self,
                    request: UpdateServiceRequest,
                ) -> Result<UpdateServiceResponse, String> {
                    self.0.request(request).await
                }
                async fn wait(&self, duration: Duration) {
                    self.0.wait(duration).await;
                }
                fn report(&self, error: &str) {
                    self.0.report(error);
                }
            }
            let mut worker = Box::pin(serve(Borrowed(&host), "execution-host".into(), server));
            let waker = futures::task::noop_waker();
            assert!(matches!(
                worker.as_mut().poll(&mut Context::from_waker(&waker)),
                Poll::Pending
            ));
            assert_eq!(host.requests.lock().unwrap().len(), 2);
        }
    }

    #[test]
    fn plugin_update_worker_rejects_wrong_host_before_fetching_or_finishing() {
        block_on(async {
            let host = Host {
                host_id: "another-host".into(),
                requests: Mutex::default(),
            };
            assert!(check_page(&host, "paired-host", false, 0).await.is_err());
            assert_eq!(host.requests.lock().unwrap().len(), 1);
        });
    }
}
