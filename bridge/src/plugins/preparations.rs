//! Bounded asynchronous preparation, shared by server and paired execution hosts.
use super::{NativePluginInstaller, host_error};
use openwebide_core::plugins::{
    PluginError, PluginSource,
    preparation::{PluginPreparation, PreparationState},
};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Notify};

const DEADLINE: Duration = Duration::from_secs(900);
const RETENTION: Duration = Duration::from_secs(600);

pub(super) struct NativePreparationHost<'a> {
    pub installer: &'a NativePluginInstaller,
    pub owner: &'a str,
    pub host_id: String,
    pub clock: Instant,
}
impl openwebide_core::plugins::preparation::PreparationClientHost for NativePreparationHost<'_> {
    fn request<'a>(
        &'a self,
        command: &'a openwebide_core::plugins::preparation::PreparationCommand,
    ) -> openwebide_core::plugins::preparation::PreparationFuture<
        'a,
        Result<PluginPreparation, String>,
    > {
        Box::pin(async move {
            use openwebide_core::plugins::preparation::PreparationCommand;
            let result = match command {
                PreparationCommand::Start { source } => {
                    self.installer
                        .preparations
                        .start(
                            self.installer.clone(),
                            self.owner.into(),
                            self.host_id.clone(),
                            source.clone(),
                        )
                        .await
                }
                PreparationCommand::Status { id } => {
                    self.installer.preparations.status(self.owner, id).await
                }
                PreparationCommand::Cancel { id } => {
                    self.installer.preparations.cancel(self.owner, id).await
                }
            };
            result.map_err(|error| error.to_string())
        })
    }
    fn sleep(
        &self,
        milliseconds: u32,
    ) -> openwebide_core::plugins::preparation::PreparationFuture<'_, ()> {
        Box::pin(tokio::time::sleep(Duration::from_millis(u64::from(
            milliseconds,
        ))))
    }
    fn now_millis(&self) -> f64 {
        self.clock.elapsed().as_secs_f64() * 1000.0
    }
    fn abandon(&self, id: String) {
        let preparations = self.installer.preparations.clone();
        let owner = self.owner.to_owned();
        tokio::spawn(async move {
            let _ = preparations.cancel(&owner, &id).await;
        });
    }
}

#[derive(Clone, Debug, Default)]
pub struct Preparations {
    entries: Arc<Mutex<HashMap<String, Arc<Preparation>>>>,
}

#[derive(Debug)]
struct Preparation {
    owner: String,
    observed: AtomicBool,
    cancelled: Arc<AtomicBool>,
    notify: Notify,
    status: Mutex<PluginPreparation>,
}

struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        // Runtime shutdown drops async tasks while their compiler thread may still run.
        self.0.store(true, Ordering::Release);
    }
}

impl Preparations {
    pub async fn start(
        &self,
        installer: NativePluginInstaller,
        owner: String,
        host_id: String,
        source: PluginSource,
    ) -> Result<PluginPreparation, PluginError> {
        source.validate()?;
        let mut entries = self.entries.lock().await;
        // Terminal results already returned to a client need not block new work.
        // Unobserved results retain their handles until their client polls or TTL expires.
        if entries.len() >= 64
            || entries
                .values()
                .filter(|entry| entry.owner == owner)
                .count()
                >= 16
        {
            entries.retain(|_, entry| !entry.observed.load(Ordering::Acquire));
        }
        if entries.len() >= 64
            || entries
                .values()
                .filter(|entry| entry.owner == owner)
                .count()
                >= 16
        {
            return Err(host_error(
                "Too many plugin preparations. Try again after current work finishes.",
            ));
        }
        let id = format!("{:032x}", rand::random::<u128>());
        let status = PluginPreparation {
            id: id.clone(),
            state: PreparationState::Queued,
            prepared: None,
            error: None,
        };
        let entry = Arc::new(Preparation {
            owner: owner.clone(),
            observed: AtomicBool::new(false),
            cancelled: Arc::new(AtomicBool::new(false)),
            notify: Notify::new(),
            status: Mutex::new(status.clone()),
        });
        entries.insert(id.clone(), entry.clone());
        drop(entries);
        let entries = self.entries.clone();
        tokio::spawn(async move {
            let _cancel_on_drop = CancelOnDrop(entry.cancelled.clone());
            {
                let mut status = entry.status.lock().await;
                if status.state != PreparationState::Cancelled {
                    status.state = PreparationState::Preparing;
                }
            }
            let result = tokio::select! {
                biased;
                () = entry.notify.notified() => Err(host_error("Plugin preparation was cancelled.")),
                result = tokio::time::timeout(DEADLINE, installer.prepare_cancellable(
                    &owner, host_id, &source, entry.cancelled.clone(),
                )) => result.unwrap_or_else(|_| {
                    entry.cancelled.store(true, Ordering::Release);
                    Err(host_error("Plugin preparation exceeded its time limit."))
                }),
            };
            {
                let mut status = entry.status.lock().await;
                if status.state != PreparationState::Cancelled {
                    match result {
                        Ok(prepared) => {
                            status.state = PreparationState::Ready;
                            status.prepared = Some(prepared);
                        }
                        Err(error) => {
                            status.state = PreparationState::Failed;
                            status.error = Some(error.to_string());
                        }
                    }
                }
            }
            tokio::time::sleep(RETENTION).await;
            entries.lock().await.remove(&id);
        });
        Ok(status)
    }

    async fn entry(&self, owner: &str, id: &str) -> Result<Arc<Preparation>, PluginError> {
        self.entries
            .lock()
            .await
            .get(id)
            .filter(|entry| entry.owner == owner)
            .cloned()
            .ok_or_else(|| host_error("Plugin preparation is unavailable."))
    }

    pub async fn status(&self, owner: &str, id: &str) -> Result<PluginPreparation, PluginError> {
        let entry = self.entry(owner, id).await?;
        let status = entry.status.lock().await.clone();
        if !matches!(
            status.state,
            PreparationState::Queued | PreparationState::Preparing
        ) {
            entry.observed.store(true, Ordering::Release);
        }
        Ok(status)
    }

    pub async fn cancel(&self, owner: &str, id: &str) -> Result<PluginPreparation, PluginError> {
        let entry = self.entry(owner, id).await?;
        let mut status = entry.status.lock().await;
        if matches!(
            status.state,
            PreparationState::Queued | PreparationState::Preparing
        ) {
            entry.cancelled.store(true, Ordering::Release);
            status.state = PreparationState::Cancelled;
            // notify_one retains a permit when cancellation precedes the worker poll.
            entry.notify.notify_one();
        }
        entry.observed.store(true, Ordering::Release);
        Ok(status.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwebide_core::plugins::testing::source;

    #[tokio::test]
    async fn cancelled_waiting_preparation_cannot_resume_or_cross_owners_in_either_host_scope() {
        for host in ["remote-host", "paired-host"] {
            let installer = NativePluginInstaller::new(None);
            let held = installer.lock.lock().await;
            let preparations = Preparations::default();
            let queued = preparations
                .start(installer.clone(), "owner".into(), host.into(), source())
                .await
                .unwrap();
            assert!(preparations.status("other", &queued.id).await.is_err());
            assert!(preparations.cancel("other", &queued.id).await.is_err());
            assert_eq!(
                preparations
                    .cancel("owner", &queued.id)
                    .await
                    .unwrap()
                    .state,
                PreparationState::Cancelled
            );
            drop(held);
            tokio::task::yield_now().await;
            let status = preparations.status("owner", &queued.id).await.unwrap();
            assert_eq!(status.state, PreparationState::Cancelled);
            assert!(status.prepared.is_none());
        }
    }

    #[tokio::test]
    async fn host_failures_are_polled_without_recording_an_installation() {
        let preparations = Preparations::default();
        let queued = preparations
            .start(
                NativePluginInstaller::new(None),
                "owner".into(),
                "host".into(),
                source(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let status = preparations.status("owner", &queued.id).await.unwrap();
                if status.state == PreparationState::Failed {
                    assert!(status.prepared.is_none());
                    assert!(status.error.is_some());
                    assert_eq!(
                        preparations
                            .cancel("owner", &queued.id)
                            .await
                            .unwrap()
                            .state,
                        PreparationState::Failed
                    );
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn preparation_capacity_is_bounded_per_owner_without_blocking_other_owners() {
        let installer = NativePluginInstaller::new(None);
        let held = installer.lock.lock().await;
        let preparations = Preparations::default();
        let mut ids = Vec::new();
        for _ in 0..16 {
            ids.push(
                preparations
                    .start(installer.clone(), "owner".into(), "host".into(), source())
                    .await
                    .unwrap()
                    .id,
            );
        }
        assert!(
            preparations
                .start(installer.clone(), "owner".into(), "host".into(), source())
                .await
                .is_err()
        );
        let other = preparations
            .start(installer.clone(), "other".into(), "host".into(), source())
            .await
            .unwrap();
        for id in ids {
            preparations.cancel("owner", &id).await.unwrap();
        }
        preparations.cancel("other", &other.id).await.unwrap();
        drop(held);
    }

    #[test]
    fn runtime_shutdown_cancels_work_retained_by_a_preparation() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let cancelled = runtime.block_on(async {
            let preparations = Preparations::default();
            let installer = NativePluginInstaller::new(None);
            let held = installer.lock.lock().await;
            let queued = preparations
                .start(installer.clone(), "owner".into(), "host".into(), source())
                .await
                .unwrap();
            tokio::task::yield_now().await;
            let entry = preparations.entry("owner", &queued.id).await.unwrap();
            assert!(!entry.cancelled.load(Ordering::Acquire));
            drop(held);
            entry.cancelled.clone()
        });
        drop(runtime);
        assert!(cancelled.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn completed_client_requests_do_not_exhaust_preparation_capacity() {
        let installer = NativePluginInstaller::new(None);
        let mut original = None;
        for _ in 0..20 {
            let error = installer
                .prepare("owner", "host".into(), &source())
                .await
                .unwrap_err()
                .to_string();
            if let Some(original) = &original {
                assert_eq!(&error, original);
            } else {
                original = Some(error);
            }
        }
        assert!(installer.preparations.entries.lock().await.len() <= 16);
    }
}
