use super::*;
use crate::rusqlite_db::RusqliteDb;
use futures::executor::block_on;
use openwebide_core::plugins::{
    RecordPlugin, available_updates,
    testing::{catalog, receipt},
};

async fn fixture(
    db: RusqliteDb,
    host: &str,
    policy: PluginUpdatePolicy,
) -> (Store<RusqliteDb>, UserId) {
    let store = Store::new(db);
    store.migrate().await.unwrap();
    let user = store
        .insert_user("owner", "hash", UserRole::Admin, 0)
        .await
        .unwrap()
        .id;
    let mut prepared = receipt();
    prepared.host_id = host.into();
    store
        .record_plugin(
            user,
            &RecordPlugin {
                prepared,
                revision: None,
                package: None,
                update_policy: Some(policy),
                approved_capabilities: vec![],
            },
            0,
        )
        .await
        .unwrap();
    let cache = catalog();
    store
        .save_plugin_marketplaces(
            user,
            &SaveMarketplaces {
                revision: 0,
                sources: vec![MarketplaceSource::official(), cache.source],
            },
        )
        .await
        .unwrap();
    (store, user)
}
fn refreshed(claim: &UpdateCheckClaim) -> MarketplaceRefresh {
    let mut cache = catalog();
    cache.catalog.plugins[0].releases.push(CatalogRelease {
        version: "0.1.1".into(),
        source: CatalogReleaseSource {
            commit: "b".repeat(40),
            path: "plugins/pr-review".into(),
        },
    });
    let mut settings = claim.settings.clone();
    settings.catalogs = vec![cache];
    MarketplaceRefresh {
        settings,
        failures: vec![],
    }
}

#[test]
fn plugin_update_checks_discover_without_changing_pins_for_both_hosts() {
    block_on(async {
        for (host, server) in [("server", true), ("paired-host", false)] {
            let (store, user) = fixture(
                RusqliteDb::open_in_memory().unwrap(),
                host,
                PluginUpdatePolicy::Notify,
            )
            .await;
            let before = store.plugin_installations(user).await.unwrap();
            let (claims, _) = store
                .claim_plugin_update_checks(host, server, 0, &"a".repeat(32), 1)
                .await
                .unwrap();
            assert_eq!(claims.len(), 1);
            store
                .finish_plugin_update_check(&claims[0], &refreshed(&claims[0]), 2)
                .await
                .unwrap();
            let settings = store.plugin_marketplaces(user).await.unwrap();
            assert_eq!(available_updates(&before, &settings).len(), 1);
            assert_eq!(settings.check.as_ref().unwrap().checked_at, Some(2));
            assert_eq!(settings.check.as_ref().unwrap().next_check_at, 902);
            assert_eq!(store.plugin_installations(user).await.unwrap(), before);
            assert!(
                store
                    .claim_plugin_update_checks(host, server, 0, &"b".repeat(32), 901)
                    .await
                    .unwrap()
                    .0
                    .is_empty()
            );
            assert_eq!(
                store
                    .claim_plugin_update_checks(host, server, 0, &"b".repeat(32), 902)
                    .await
                    .unwrap()
                    .0
                    .len(),
                1
            );
        }
    });
}

#[test]
fn plugin_update_checks_recover_leases_after_restart_and_reject_old_results() {
    block_on(async {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("db.sqlite");
        let (store, user) = fixture(
            RusqliteDb::open(&path).unwrap(),
            "paired-host",
            PluginUpdatePolicy::Notify,
        )
        .await;
        let claim = store
            .claim_plugin_update_checks("paired-host", false, 0, &"a".repeat(32), 1)
            .await
            .unwrap()
            .0
            .remove(0);
        drop(store);
        let store = Store::new(RusqliteDb::open(&path).unwrap());
        store.migrate().await.unwrap();
        assert!(
            store
                .claim_plugin_update_checks("server", true, 0, &"b".repeat(32), 600)
                .await
                .unwrap()
                .0
                .is_empty()
        );
        let next = store
            .claim_plugin_update_checks("server", true, 0, &"b".repeat(32), 601)
            .await
            .unwrap()
            .0
            .remove(0);
        assert!(matches!(
            store
                .finish_plugin_update_check(&claim, &refreshed(&claim), 602)
                .await,
            Err(StorageError::Conflict(_))
        ));
        store
            .finish_plugin_update_check(&next, &refreshed(&next), 602)
            .await
            .unwrap();
        assert_eq!(
            store
                .plugin_marketplaces(user)
                .await
                .unwrap()
                .check
                .unwrap()
                .host_id,
            "server"
        );
    });
}

#[test]
fn plugin_update_checks_preserve_cache_on_outage_and_retry_with_bounded_backoff() {
    block_on(async {
        let (store, user) = fixture(
            RusqliteDb::open_in_memory().unwrap(),
            "server",
            PluginUpdatePolicy::Notify,
        )
        .await;
        let cache = catalog();
        store
            .cache_plugin_marketplaces(user, 1, vec![cache.clone()])
            .await
            .unwrap();
        let mut now = 1;
        for delay in [60, 120, 240, 480, 900, 900] {
            let claim = store
                .claim_plugin_update_checks("server", true, 0, &"a".repeat(32), now)
                .await
                .unwrap()
                .0
                .remove(0);
            let result = MarketplaceRefresh {
                settings: claim.settings.clone(),
                failures: vec![MarketplaceFailure {
                    source: cache.source.clone(),
                    message: "Host offline".into(),
                }],
            };
            store
                .finish_plugin_update_check(&claim, &result, now)
                .await
                .unwrap();
            let settings = store.plugin_marketplaces(user).await.unwrap();
            assert_eq!(settings.catalogs, vec![cache.clone()]);
            assert_eq!(settings.check.as_ref().unwrap().failures, result.failures);
            assert_eq!(settings.check.as_ref().unwrap().next_check_at, now + delay);
            now += delay;
        }
    });
}

#[test]
fn plugin_update_checks_respect_off_host_ownership_and_source_changes() {
    block_on(async {
        let (store, user) = fixture(
            RusqliteDb::open_in_memory().unwrap(),
            "paired-host",
            PluginUpdatePolicy::Off,
        )
        .await;
        assert!(
            store
                .claim_plugin_update_checks("server", true, 0, &"a".repeat(32), 1)
                .await
                .unwrap()
                .0
                .is_empty()
        );
        let entry = store.plugin_installations(user).await.unwrap().remove(0);
        store
            .record_plugin(
                user,
                &RecordPlugin {
                    prepared: entry.prepared,
                    revision: Some(entry.revision),
                    package: None,
                    update_policy: Some(PluginUpdatePolicy::Automatic),
                    approved_capabilities: vec![],
                },
                1,
            )
            .await
            .unwrap();
        assert!(
            store
                .claim_plugin_update_checks("wrong-host", false, 0, &"a".repeat(32), 2)
                .await
                .unwrap()
                .0
                .is_empty()
        );
        let claim = store
            .claim_plugin_update_checks("paired-host", false, 0, &"a".repeat(32), 2)
            .await
            .unwrap()
            .0
            .remove(0);
        let revision = store.plugin_marketplaces(user).await.unwrap().revision;
        store
            .save_plugin_marketplaces(
                user,
                &SaveMarketplaces {
                    revision,
                    sources: vec![MarketplaceSource::official()],
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .finish_plugin_update_check(&claim, &refreshed(&claim), 3)
                .await,
            Err(StorageError::Conflict(_))
        ));
        assert!(
            store
                .plugin_marketplaces(user)
                .await
                .unwrap()
                .catalogs
                .is_empty()
        );
        assert_eq!(
            store
                .claim_plugin_update_checks("paired-host", false, 0, &"b".repeat(32), 3)
                .await
                .unwrap()
                .0
                .len(),
            1
        );
        let other = store
            .insert_user("other", "hash", UserRole::User, 0)
            .await
            .unwrap()
            .id;
        assert!(
            store
                .plugin_marketplaces(other)
                .await
                .unwrap()
                .check
                .is_none()
        );
    });
}

#[test]
fn plugin_update_checks_reject_cross_account_results_and_keep_newer_manual_refresh() {
    block_on(async {
        let (store, user) = fixture(
            RusqliteDb::open_in_memory().unwrap(),
            "server",
            PluginUpdatePolicy::Notify,
        )
        .await;
        let claim = store
            .claim_plugin_update_checks("server", true, 0, &"a".repeat(32), 1)
            .await
            .unwrap()
            .0
            .remove(0);
        let other = store
            .insert_user("other", "hash", UserRole::User, 0)
            .await
            .unwrap()
            .id;
        let mut forged = claim.clone();
        forged.user_id = other.get();
        assert!(matches!(
            store
                .finish_plugin_update_check(&forged, &refreshed(&forged), 2)
                .await,
            Err(StorageError::Conflict(_))
        ));
        let mut manual = catalog();
        manual.fetched_at = 99;
        store
            .cache_plugin_marketplaces(user, claim.settings.revision, vec![manual.clone()])
            .await
            .unwrap();
        store
            .finish_plugin_update_check(&claim, &refreshed(&claim), 3)
            .await
            .unwrap();
        let settings = store.plugin_marketplaces(user).await.unwrap();
        assert_eq!(settings.catalogs, vec![manual]);
        assert_eq!(settings.check.as_ref().unwrap().checked_at, None);
        assert_eq!(settings.check.as_ref().unwrap().next_check_at, 63);
        assert!(
            store
                .plugin_marketplaces(other)
                .await
                .unwrap()
                .catalogs
                .is_empty()
        );
    });
}
