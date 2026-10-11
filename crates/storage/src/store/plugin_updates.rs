//! Durable update-check leases and account-scoped results; no release activation.
use super::*;
use openwebide_core::plugins::{PluginUpdatePolicy, marketplace::*, updates::*};

impl<D: Db> Store<D> {
    pub async fn plugin_update_status(
        &self,
        user: UserId,
    ) -> Result<Option<UpdateCheckStatus>, StorageError> {
        let result = self.db.execute("SELECT checked_at,next_due,host_id,failures FROM plugin_update_checks WHERE user_id=?", &[DbValue::Int(user.get())]).await?;
        result
            .rows
            .first()
            .map(|row| {
                Ok(UpdateCheckStatus {
                    checked_at: row.get_int_opt(0),
                    next_check_at: row.get_int(1)?,
                    host_id: row.get_text(2)?.into(),
                    failures: serde_json::from_str(row.get_text(3)?)
                        .map_err(|error| StorageError::Db(error.to_string()))?,
                })
            })
            .transpose()
    }
    pub async fn claim_plugin_update_checks(
        &self,
        host_id: &str,
        server: bool,
        after: i64,
        lease: &str,
        now: i64,
    ) -> Result<(Vec<UpdateCheckClaim>, Option<i64>), StorageError> {
        if host_id.trim().is_empty() || host_id.len() > 1024 || after < 0 || lease.len() != 32 {
            return Err(StorageError::InvalidRequest(
                "Invalid plugin update host claim".into(),
            ));
        }
        self.db.transaction(|tx| async move {
            let store = Store::new(tx);
            let rows = store.db.execute("SELECT user_id FROM user_settings WHERE key='plugin_installations' AND user_id>? ORDER BY user_id LIMIT 16", &[DbValue::Int(after)]).await?.rows;
            let next = if rows.len() == 16 { Some(rows.last().expect("nonempty page").get_int(0)?) } else { None };
            let mut claims = Vec::new();
            for row in rows {
                let user = UserId::new(row.get_int(0)?);
                let installations = store.plugin_installations(user).await?;
                if !installations.iter().any(|entry| entry.update_policy != PluginUpdatePolicy::Off && (server || entry.hosts.iter().any(|host| host == host_id) || entry.prepared.host_id == host_id)) { continue; }
                store.db.execute("INSERT OR IGNORE INTO plugin_update_checks(user_id) VALUES(?)", &[DbValue::Int(user.get())]).await?;
                let changed = store.db.execute("UPDATE plugin_update_checks SET lease=?,lease_expires_at=?,host_id=? WHERE user_id=? AND next_due<=? AND lease_expires_at<=?", &[
                    DbValue::Text(lease.into()), DbValue::Int(now.saturating_add(CHECK_LEASE)), DbValue::Text(host_id.into()), DbValue::Int(user.get()), DbValue::Int(now), DbValue::Int(now),
                ]).await?;
                if changed.changes == 0 { continue; }
                claims.push(UpdateCheckClaim { user_id: user.get(), host_id: host_id.into(), lease: lease.into(), settings: store.plugin_marketplaces(user).await? });
                // Process at most one account per page; unprocessed accounts are
                // visited on the next bounded wakeup rather than leasing a backlog.
                return Ok((claims, Some(user.get())));
            }
            Ok((claims, next))
        }).await
    }
    pub async fn finish_plugin_update_check(
        &self,
        claim: &UpdateCheckClaim,
        refresh: &MarketplaceRefresh,
        now: i64,
    ) -> Result<(), StorageError> {
        self.db.transaction(|tx| async move {
            let store = Store::new(tx);
            let user = UserId::new(claim.user_id);
            let row = store.db.execute("SELECT failures_count FROM plugin_update_checks WHERE user_id=? AND host_id=? AND lease=? AND lease_expires_at>?", &[
                DbValue::Int(user.get()), DbValue::Text(claim.host_id.clone()), DbValue::Text(claim.lease.clone()), DbValue::Int(now),
            ]).await?.rows;
            let Some(row) = row.first() else { return Err(StorageError::Conflict("Plugin update check lease expired".into())); };
            let current = store.plugin_marketplaces(user).await?;
            // A manual refresh may advance the catalog revision while Git runs.
            // Retain its result and prior check status, then retry shortly.
            if current.revision != claim.settings.revision || current.sources != claim.settings.sources {
                store.db.execute("UPDATE plugin_update_checks SET next_due=?,lease='',lease_expires_at=0 WHERE user_id=?", &[DbValue::Int(now.saturating_add(retry_delay(1))), DbValue::Int(user.get())]).await?;
                return Ok(());
            }
            if refresh.settings.revision != claim.settings.revision || refresh.settings.sources != claim.settings.sources {
                return Err(StorageError::InvalidRequest("Plugin update sources changed".into()));
            }
            let failures = &refresh.failures;
            if failures.len() > current.sources.len() {
                return Err(StorageError::InvalidRequest("Too many marketplace failures".into()));
            }
            for failure in failures {
                if !current.sources.contains(&failure.source) || failure.message.len() > 1024 { return Err(StorageError::InvalidRequest("Invalid marketplace failure".into())); }
            }
            let mut settings = cache_catalogs(current, claim.settings.revision, refresh.settings.catalogs.clone()).map_err(super::plugins::plugin_error)?;
            settings.check = None;
            store.set_user_setting(user, "plugin_marketplaces", &serde_json::to_string(&settings).map_err(|error| StorageError::Db(error.to_string()))?).await?;
            let count = if failures.is_empty() { 0 } else { u32::try_from(row.get_int(0)?).unwrap_or(u32::MAX).saturating_add(1) };
            store.db.execute("UPDATE plugin_update_checks SET checked_at=?,next_due=?,lease='',lease_expires_at=0,failures=?,failures_count=? WHERE user_id=?", &[
                DbValue::Int(now), DbValue::Int(now.saturating_add(retry_delay(count))), DbValue::Text(serde_json::to_string(&failures).map_err(|error| StorageError::Db(error.to_string()))?), DbValue::Int(i64::from(count)), DbValue::Int(user.get()),
            ]).await?;
            Ok(())
        }).await
    }
}

#[cfg(test)]
mod tests;
