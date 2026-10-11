//! Shared catalog checks; discovery never activates an installed release.
use super::marketplace::{
    CachedMarketplace, MarketplaceFailure, MarketplaceRefresh, MarketplaceSettings,
    MarketplaceSource,
};
use serde::{Deserialize, Serialize};
use std::future::Future;

pub const CHECK_INTERVAL: i64 = 15 * 60;
pub const CHECK_LEASE: i64 = 10 * 60;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateCheckStatus {
    pub checked_at: Option<i64>,
    pub next_check_at: i64,
    pub host_id: String,
    pub failures: Vec<MarketplaceFailure>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpdateCheckClaim {
    pub user_id: i64,
    pub host_id: String,
    pub lease: String,
    pub settings: MarketplaceSettings,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdateServiceRequest {
    Claim {
        host_id: String,
        server: bool,
        after: i64,
    },
    Finish {
        claim: Box<UpdateCheckClaim>,
        refresh: Box<MarketplaceRefresh>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdateServiceResponse {
    Claimed {
        claims: Vec<UpdateCheckClaim>,
        next: Option<i64>,
    },
    Finished,
}
pub trait CatalogHost {
    fn catalog(
        &self,
        source: &MarketplaceSource,
    ) -> impl Future<Output = Result<CachedMarketplace, String>> + Send;
}
/// Partial outages retain successful and previously cached sources. Persistence
/// applies the captured revision, so edits cannot resurrect removed sources.
pub async fn refresh_catalogs(
    host: &impl CatalogHost,
    settings: MarketplaceSettings,
) -> MarketplaceRefresh {
    let mut catalogs = Vec::new();
    let mut failures = Vec::new();
    for source in &settings.sources {
        let result = host.catalog(source).await.and_then(|catalog| {
            catalog.validate().map_err(|error| error.to_string())?;
            if catalog.source != *source {
                return Err("Catalog host returned a different source.".into());
            }
            Ok(catalog)
        });
        match result {
            Ok(catalog) => catalogs.push(catalog),
            Err(message) => failures.push(MarketplaceFailure {
                source: source.clone(),
                message: bounded_message(&message),
            }),
        }
    }
    let mut next = settings;
    for catalog in catalogs {
        next.catalogs
            .retain(|previous| previous.source != catalog.source);
        next.catalogs.push(catalog);
    }
    MarketplaceRefresh {
        settings: next,
        failures,
    }
}
pub fn bounded_message(message: &str) -> String {
    let mut end = message.len().min(1024);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message[..end].into()
}
pub fn retry_delay(failures: u32) -> i64 {
    if failures == 0 {
        CHECK_INTERVAL
    } else {
        (60_i64 << failures.saturating_sub(1).min(4)).min(CHECK_INTERVAL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::testing::catalog;
    use futures::executor::block_on;

    struct Host {
        wrong_source: bool,
    }
    impl CatalogHost for Host {
        async fn catalog(&self, source: &MarketplaceSource) -> Result<CachedMarketplace, String> {
            let mut cache = catalog();
            if *source == MarketplaceSource::official() {
                return Err("offline".into());
            }
            if !self.wrong_source {
                cache.source = source.clone();
            } else {
                cache.source = MarketplaceSource::official();
            }
            cache.fetched_at = 12;
            Ok(cache)
        }
    }
    #[test]
    fn plugin_update_catalog_checks_keep_partial_success_and_failed_source_cache() {
        block_on(async {
            let old = catalog();
            let mut official = old.clone();
            official.source = MarketplaceSource::official();
            let settings = MarketplaceSettings {
                check: None,
                revision: 7,
                sources: vec![official.source.clone(), old.source.clone()],
                catalogs: vec![official.clone(), old.clone()],
            };
            let result = refresh_catalogs(
                &Host {
                    wrong_source: false,
                },
                settings.clone(),
            )
            .await;
            assert_eq!(result.settings.catalogs[0], official);
            assert_eq!(result.settings.catalogs[1].fetched_at, 12);
            assert_eq!(result.settings.revision, 7);
            assert_eq!(result.failures.len(), 1);
            let failed = refresh_catalogs(&Host { wrong_source: true }, settings.clone()).await;
            assert_eq!(failed.settings, settings);
            assert_eq!(failed.failures.len(), 2);
        });
    }
    #[test]
    fn plugin_update_failure_details_and_backoff_are_bounded() {
        assert_eq!(bounded_message(&"é".repeat(1000)).len(), 1024);
        assert_eq!(retry_delay(0), 900);
        assert_eq!(retry_delay(1), 60);
        assert_eq!(retry_delay(u32::MAX), 900);
    }
}
