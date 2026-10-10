//! Host-owned source preparation, separate from recording an installation.
use super::{PluginError, PluginSource, PreparedPlugin};
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};

pub type PreparationFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Adapters supply transport, monotonic time and lifecycle primitives only.
pub trait PreparationClientHost: Send + Sync {
    fn request<'a>(
        &'a self,
        command: &'a PreparationCommand,
    ) -> PreparationFuture<'a, Result<PluginPreparation, String>>;
    fn sleep(&self, milliseconds: u32) -> PreparationFuture<'_, ()>;
    fn now_millis(&self) -> f64;
    fn active(&self) -> bool {
        true
    }
    fn cancelled(&self) -> bool {
        false
    }
    fn progress(&self, _state: PreparationState) {}
    fn abandon(&self, id: String);
}

struct PreparationGuard<'a, H: PreparationClientHost> {
    host: &'a H,
    id: Option<String>,
}
impl<H: PreparationClientHost> Drop for PreparationGuard<'_, H> {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            self.host.abandon(id);
        }
    }
}

/// Short bounded requests and one preparation policy for every client runtime.
pub async fn preparation_request(
    host: &impl PreparationClientHost,
    command: &PreparationCommand,
) -> Result<PluginPreparation, String> {
    command.validate().map_err(|error| error.to_string())?;
    match futures::future::select(host.request(command), host.sleep(30_000)).await {
        futures::future::Either::Left((result, _)) => result,
        futures::future::Either::Right(_) => Err("Plugin host request timed out.".into()),
    }
}

pub async fn prepare_on_host(
    host: &impl PreparationClientHost,
    source: &PluginSource,
) -> Result<PreparedPlugin, String> {
    source.validate().map_err(|error| error.to_string())?;
    if !host.active() || host.cancelled() {
        return Err("Plugin preparation was cancelled.".into());
    }
    let mut guard = PreparationGuard { host, id: None };
    let started = host.now_millis();
    host.progress(PreparationState::Queued);
    // Keep the first acknowledgement so invalidation can cancel its host operation.
    let mut status = preparation_request(
        host,
        &PreparationCommand::Start {
            source: source.clone(),
        },
    )
    .await?;
    status.validate().map_err(|error| error.to_string())?;
    guard.id = Some(status.id.clone());
    loop {
        if host.now_millis() - started >= 900_000.0 {
            return Err("Plugin preparation exceeded its time limit.".into());
        }
        if !host.active() || host.cancelled() {
            return Err("Plugin preparation was cancelled.".into());
        }
        host.progress(status.state);
        match status.state {
            PreparationState::Ready => {
                let prepared = status.prepared.ok_or("Missing prepared plugin receipt")?;
                prepared
                    .manifest
                    .validate_activation()
                    .map_err(|error| error.to_string())?;
                if prepared.source != *source {
                    return Err("Plugin host returned a different source.".into());
                }
                guard.id = None;
                return Ok(prepared);
            }
            PreparationState::Failed => {
                return Err(status
                    .error
                    .unwrap_or_else(|| "Plugin preparation failed.".into()));
            }
            PreparationState::Cancelled => return Err("Plugin preparation was cancelled.".into()),
            PreparationState::Queued | PreparationState::Preparing => {}
        }
        host.sleep(250).await;
        if !host.active() || host.cancelled() {
            return Err("Plugin preparation was cancelled.".into());
        }
        let id = guard
            .id
            .clone()
            .ok_or("Plugin preparation is unavailable")?;
        let command = PreparationCommand::Status { id: id.clone() };
        let request = preparation_request(host, &command);
        let invalidation = async {
            loop {
                if !host.active() || host.cancelled() {
                    return Err("Plugin preparation was cancelled.".to_string());
                }
                host.sleep(50).await;
            }
        };
        futures::pin_mut!(request, invalidation);
        status = match futures::future::select(request, invalidation).await {
            futures::future::Either::Left((result, _)) => result?,
            futures::future::Either::Right((result, _)) => return result,
        };
        status.validate().map_err(|error| error.to_string())?;
        if status.id != id {
            return Err("Plugin host returned a different preparation.".into());
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparationState {
    Queued,
    Preparing,
    Ready,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginPreparation {
    pub id: String,
    pub state: PreparationState,
    pub prepared: Option<PreparedPlugin>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PreparationCommand {
    Start { source: PluginSource },
    Status { id: String },
    Cancel { id: String },
}

impl PreparationCommand {
    pub fn validate(&self) -> Result<(), PluginError> {
        match self {
            Self::Start { source } => source.validate(),
            Self::Status { id } | Self::Cancel { id } => validate_id(id),
        }
    }

    pub fn operation(&self) -> &'static str {
        match self {
            Self::Start { .. } => "start",
            Self::Status { .. } => "status",
            Self::Cancel { .. } => "cancel",
        }
    }

    pub fn host_payload(&self) -> serde_json::Value {
        match self {
            Self::Start { source } => serde_json::json!({"source":source}),
            Self::Status { id } | Self::Cancel { id } => serde_json::json!({"id":id}),
        }
    }
}

fn validate_id(id: &str) -> Result<(), PluginError> {
    if id.len() != 32
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(PluginError::Invalid(
            "Invalid plugin preparation id.".into(),
        ));
    }
    Ok(())
}

impl PluginPreparation {
    pub fn validate(&self) -> Result<(), PluginError> {
        validate_id(&self.id)?;
        match self.state {
            PreparationState::Ready if self.error.is_none() => self
                .prepared
                .as_ref()
                .ok_or_else(|| PluginError::Invalid("Missing prepared plugin receipt.".into()))?
                .validate(),
            PreparationState::Failed
                if self.prepared.is_none()
                    && self
                        .error
                        .as_ref()
                        .is_some_and(|error| !error.is_empty() && error.len() <= 256 * 1024) =>
            {
                Ok(())
            }
            PreparationState::Queued
            | PreparationState::Preparing
            | PreparationState::Cancelled
                if self.prepared.is_none() && self.error.is_none() =>
            {
                Ok(())
            }
            _ => Err(PluginError::Invalid(
                "Invalid plugin preparation response.".into(),
            )),
        }
    }
}

#[cfg(test)]
mod client_tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };

    struct Host {
        clock: AtomicU64,
        wait_forever: bool,
        active: AtomicBool,
        abandoned: Mutex<Vec<String>>,
        ready: Option<PreparedPlugin>,
    }
    impl PreparationClientHost for Host {
        fn request<'a>(
            &'a self,
            command: &'a PreparationCommand,
        ) -> PreparationFuture<'a, Result<PluginPreparation, String>> {
            Box::pin(async move {
                if self.wait_forever && matches!(command, PreparationCommand::Status { .. }) {
                    return futures::future::pending().await;
                }
                Ok(PluginPreparation {
                    id: "a".repeat(32),
                    state: if self.ready.is_some() {
                        PreparationState::Ready
                    } else {
                        PreparationState::Preparing
                    },
                    prepared: self.ready.clone(),
                    error: None,
                })
            })
        }
        fn sleep(&self, milliseconds: u32) -> PreparationFuture<'_, ()> {
            Box::pin(async move {
                // Advance only a polled timer; dropped RPC deadlines consume no time.
                self.clock.fetch_add(
                    if milliseconds == 250 {
                        60_000
                    } else {
                        u64::from(milliseconds)
                    },
                    Ordering::Relaxed,
                );
            })
        }
        fn now_millis(&self) -> f64 {
            // This test clock advances in whole bounded seconds.
            f64::from(u32::try_from(self.clock.load(Ordering::Relaxed)).unwrap())
        }
        fn active(&self) -> bool {
            self.active.load(Ordering::Relaxed)
        }
        fn abandon(&self, id: String) {
            self.abandoned.lock().unwrap().push(id);
        }
    }

    #[test]
    fn stalled_rpc_and_endless_preparation_both_release_the_host_operation() {
        for wait_forever in [false, true] {
            let host = Host {
                clock: AtomicU64::new(0),
                wait_forever,
                active: AtomicBool::new(true),
                abandoned: Mutex::new(Vec::new()),
                ready: None,
            };
            let error = futures::executor::block_on(prepare_on_host(
                &host,
                &super::super::testing::source(),
            ))
            .unwrap_err();
            assert_eq!(
                error,
                if wait_forever {
                    "Plugin host request timed out."
                } else {
                    "Plugin preparation exceeded its time limit."
                }
            );
            assert_eq!(*host.abandoned.lock().unwrap(), vec!["a".repeat(32)]);
        }
    }
    #[test]
    fn retired_receipts_from_older_hosts_cannot_complete_client_preparation() {
        for host_id in ["server", "paired"] {
            let mut prepared = super::super::testing::receipt();
            prepared.host_id = host_id.into();
            prepared.manifest.compatibility.plugin_api = 2;
            prepared.manifest.contributions.tool_groups =
                vec![super::super::PluginToolGroup::Memory];
            let host = Host {
                clock: AtomicU64::new(0),
                wait_forever: false,
                active: AtomicBool::new(true),
                abandoned: Mutex::default(),
                ready: Some(prepared.clone()),
            };
            let error =
                futures::executor::block_on(prepare_on_host(&host, &prepared.source)).unwrap_err();
            assert!(error.contains("Update required"));
            assert_eq!(*host.abandoned.lock().unwrap(), vec!["a".repeat(32)]);
        }
    }
}
