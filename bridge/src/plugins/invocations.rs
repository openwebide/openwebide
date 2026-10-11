//! Bounded component actors; transports supply authenticated ownership.
use super::NativePluginInstaller;
use openwebide_core::plugins::execution::{
    ContinuePlugin, InvokePlugin, PluginInvocation, PluginOperation, PluginStep,
};
use openwebide_plugin_runtime::{HostServices, MAX_MESSAGE_BYTES, Runtime};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, mpsc as async_mpsc};

#[derive(Clone, Debug, Default)]
pub struct Invocations {
    entries: Arc<Mutex<HashMap<String, Arc<Invocation>>>>,
}

#[cfg(test)]
mod expiry_tests {
    use super::*;

    #[tokio::test]
    async fn abandoned_invocations_expire_without_another_install_or_call() {
        let invocations = Invocations::default();
        let (_, events) = async_mpsc::channel(1);
        let entry = Arc::new(Invocation {
            owner: "paired-host".into(),
            started: Instant::now(),
            cancelled: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(tokio::sync::Notify::new()),
            state: Mutex::new(InvocationState {
                deferred: None,
                events,
                pending: None,
            }),
        });
        invocations
            .entries
            .lock()
            .await
            .insert("abandoned".into(), entry.clone());
        let notified = entry.notify.notified();
        invocations.expire_after("abandoned".into(), Duration::from_millis(10));
        tokio::time::timeout(Duration::from_secs(2), notified)
            .await
            .expect("expiry should wake a waiting invocation");
        assert!(entry.cancelled.load(Ordering::Relaxed));
        assert!(invocations.entry("abandoned").await.is_err());
    }
}
#[derive(Debug)]
struct Invocation {
    owner: String,
    started: Instant,
    cancelled: Arc<AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
    state: Mutex<InvocationState>,
}
#[derive(Debug)]
struct InvocationState {
    deferred: Option<DeferredInvocation>,
    events: async_mpsc::Receiver<Event>,
    pending: Option<(u32, mpsc::SyncSender<Result<String, String>>)>,
}
#[derive(Debug)]
struct DeferredInvocation {
    bytes: Vec<u8>,
    call: InvokePlugin,
    grants: Vec<String>,
    sender: async_mpsc::Sender<Event>,
}
impl Drop for Invocation {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.notify.notify_waiters();
    }
}
#[derive(Debug)]
enum Event {
    HostCall {
        sequence: u32,
        capability: String,
        payload: String,
        response: mpsc::SyncSender<Result<String, String>>,
    },
    Finished(PluginStep),
}
struct Services {
    runtime: tokio::runtime::Handle,
    notify: Arc<tokio::sync::Notify>,
    events: async_mpsc::Sender<Event>,
    cancelled: Arc<AtomicBool>,
    sequence: u32,
}
impl HostServices for Services {
    fn request(&mut self, capability: &str, payload: &str) -> Result<String, String> {
        if self.cancelled.load(Ordering::Relaxed) || self.sequence >= 128 {
            return Err("Plugin invocation was cancelled or exceeded its host-call limit.".into());
        }
        self.sequence += 1;
        if capability == "clock" {
            let seconds = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_secs();
            return Ok(serde_json::json!({"unix_seconds":seconds}).to_string());
        }
        if capability == "http" {
            return self.runtime.block_on(async {
                if self.cancelled.load(Ordering::Relaxed) { return Err("Plugin invocation was cancelled.".into()); }
                tokio::select! {
                    () = self.notify.notified() => Err("Plugin invocation was cancelled.".into()),
                    result = tokio::time::timeout(Duration::from_secs(30), super::http::request(payload)) => result.map_err(|_| "Plugin HTTP request timed out.".to_string())?,
                }
            });
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        self.events
            .blocking_send(Event::HostCall {
                sequence: self.sequence,
                capability: capability.into(),
                payload: payload.into(),
                response: sender,
            })
            .map_err(|_| "Plugin invocation was closed.")?;
        match receiver.recv_timeout(Duration::from_secs(60)) {
            Ok(result) => result,
            Err(_) => {
                self.cancelled.store(true, Ordering::Relaxed);
                Err("Plugin host call timed out or was cancelled.".into())
            }
        }
    }
}
impl Invocations {
    fn expire_after(&self, id: String, ttl: Duration) {
        let entries = self.entries.clone();
        tokio::spawn(async move {
            tokio::time::sleep(ttl).await;
            if let Some(entry) = entries.lock().await.remove(&id) {
                entry.cancelled.store(true, Ordering::Relaxed);
                entry.notify.notify_waiters();
            }
        });
    }
    pub async fn start(
        &self,
        installer: &NativePluginInstaller,
        owner: &str,
        call: InvokePlugin,
    ) -> Result<PluginInvocation, String> {
        call.prepared
            .validate()
            .map_err(|error| error.to_string())?;
        call.validate_event()?;
        let rust = call
            .prepared
            .manifest
            .executable
            .as_ref()
            .ok_or("Plugin has no executable")?;
        let declared = match call.operation {
            PluginOperation::Context | PluginOperation::Event => call.name.is_empty(),
            PluginOperation::Tool => call
                .prepared
                .manifest
                .contributions
                .tools
                .iter()
                .any(|tool| tool.name == call.name),
        };
        if !declared || call.arguments.len() > MAX_MESSAGE_BYTES {
            return Err(
                "Tool is not declared by this plugin, or its arguments exceed the limit.".into(),
            );
        }
        let _: serde_json::Value =
            serde_json::from_str(&call.arguments).map_err(|error| error.to_string())?;
        let bytes = installer
            .component(owner, &call.prepared)
            .await
            .map_err(|error| error.to_string())?;
        let id = format!("{:032x}", rand::random::<u128>());
        let cancelled = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = async_mpsc::channel(1);
        let grants = rust.capabilities.clone();
        let entry = Arc::new(Invocation {
            owner: owner.into(),
            started: Instant::now(),
            cancelled,
            notify: Arc::new(tokio::sync::Notify::new()),
            state: Mutex::new(InvocationState {
                events: receiver,
                pending: None,
                deferred: Some(DeferredInvocation {
                    bytes,
                    call,
                    grants,
                    sender,
                }),
            }),
        });
        {
            let mut entries = self.entries.lock().await;
            entries.retain(|_, entry| entry.started.elapsed() < Duration::from_secs(300));
            if entries.len() >= 32
                || entries
                    .values()
                    .filter(|entry| entry.owner == owner)
                    .count()
                    >= 8
            {
                return Err("Too many active plugin invocations.".into());
            }
            entries.insert(id.clone(), entry);
        }
        // Reclaim abandoned request/response invocations even when no later
        // request arrives to trigger the admission-time cleanup.
        self.expire_after(id.clone(), Duration::from_secs(300));
        // No plugin code runs until the caller receives the id and resumes it.
        Ok(PluginInvocation {
            id,
            step: PluginStep::Ready,
        })
    }
    pub async fn resume(
        &self,
        owner: &str,
        response: ContinuePlugin,
    ) -> Result<PluginInvocation, String> {
        if response
            .response
            .as_ref()
            .map_or_else(String::len, String::len)
            > MAX_MESSAGE_BYTES
        {
            return Err("Plugin host response exceeds its limit.".into());
        }
        let entry = self.entry(&response.id).await?;
        if entry.owner != owner {
            return Err("Plugin invocation belongs to another owner.".into());
        }
        {
            let mut state = entry.state.lock().await;
            if let Some(deferred) = state.deferred.take() {
                if response.sequence != 0 || response.response != Ok(String::new()) {
                    state.deferred = Some(deferred);
                    return Err("Invalid initial plugin continuation.".into());
                }
                let DeferredInvocation {
                    bytes,
                    call,
                    grants,
                    sender,
                } = deferred;
                let services = Services {
                    runtime: tokio::runtime::Handle::current(),
                    notify: entry.notify.clone(),
                    events: sender.clone(),
                    cancelled: entry.cancelled.clone(),
                    sequence: 0,
                };
                tokio::task::spawn_blocking(move || {
                    let result = Runtime::new().and_then(|runtime| match call.operation {
                        PluginOperation::Tool => {
                            runtime.execute(&bytes, services, &grants, &call.name, &call.arguments)
                        }
                        PluginOperation::Context => {
                            let input = serde_json::from_str(&call.arguments)?;
                            let context = runtime.context(&bytes, services, &grants, input)?;
                            Ok(openwebide_plugin_runtime::sdk::Outcome {
                                ok: true,
                                content: serde_json::to_string(&context)?,
                                summary: "Plugin context".into(),
                            })
                        }
                        PluginOperation::Event => {
                            let input = serde_json::from_str(&call.arguments)?;
                            runtime.event(&bytes, services, &grants, input)
                        }
                    });
                    let step = match result {
                        Ok(outcome) => PluginStep::Complete {
                            ok: outcome.ok,
                            content: outcome.content,
                            summary: outcome.summary,
                        },
                        Err(error) => PluginStep::Failed {
                            error: format!("Plugin execution failed: {error:#}"),
                        },
                    };
                    let _ = sender.try_send(Event::Finished(step));
                });
            } else {
                let pending = state
                    .pending
                    .as_ref()
                    .ok_or("No plugin host response is pending")?;
                if pending.0 != response.sequence {
                    return Err("Stale plugin host response.".into());
                }
                let (_, sender) = state.pending.take().expect("checked pending call");
                sender
                    .send(response.response)
                    .map_err(|_| "Plugin invocation has ended.")?;
            }
        }
        self.next(owner, &response.id, entry).await
    }
    pub async fn cancel(&self, owner: &str, id: &str) -> Result<(), String> {
        let entry = self.entry(id).await?;
        if entry.owner != owner {
            return Err("Plugin invocation belongs to another owner.".into());
        }
        entry.cancelled.store(true, Ordering::Relaxed);
        entry.notify.notify_waiters();
        if let Ok(mut state) = entry.state.try_lock() {
            state.pending.take();
        }
        self.entries.lock().await.remove(id);
        Ok(())
    }
    async fn entry(&self, id: &str) -> Result<Arc<Invocation>, String> {
        self.entries
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| "Plugin invocation is no longer active.".into())
    }
    async fn next(
        &self,
        owner: &str,
        id: &str,
        entry: Arc<Invocation>,
    ) -> Result<PluginInvocation, String> {
        if entry.owner != owner {
            return Err("Plugin invocation belongs to another owner.".into());
        }
        let mut state = entry.state.lock().await;
        let event = if entry.cancelled.load(Ordering::Relaxed) {
            None
        } else {
            tokio::select! {
                () = entry.notify.notified() => None,
                event = tokio::time::timeout(Duration::from_secs(60), state.events.recv()) => event.ok().flatten(),
            }
        };
        let Some(event) = event else {
            entry.cancelled.store(true, Ordering::Relaxed);
            state.pending.take();
            drop(state);
            self.entries.lock().await.remove(id);
            return Err("Plugin invocation timed out or ended without a result.".into());
        };
        let step = match event {
            Event::HostCall {
                sequence,
                capability,
                payload,
                response,
            } => {
                state.pending = Some((sequence, response));
                PluginStep::HostCall {
                    sequence,
                    capability,
                    payload,
                }
            }
            Event::Finished(step) => {
                drop(state);
                self.entries.lock().await.remove(id);
                step
            }
        };
        Ok(PluginInvocation {
            id: id.into(),
            step,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pending_responses_reject_other_owners_and_stale_sequences_before_cancellation() {
        let invocations = Invocations::default();
        let cancelled = Arc::new(AtomicBool::new(false));
        let (_events, receiver) = async_mpsc::channel(1);
        let (response, replies) = mpsc::sync_channel(1);
        invocations.entries.lock().await.insert(
            "call".into(),
            Arc::new(Invocation {
                owner: "owner".into(),
                started: Instant::now(),
                cancelled: cancelled.clone(),
                notify: Arc::new(tokio::sync::Notify::new()),
                state: Mutex::new(InvocationState {
                    deferred: None,
                    events: receiver,
                    pending: Some((7, response)),
                }),
            }),
        );
        for (owner, sequence) in [("other", 7), ("owner", 6)] {
            assert!(
                invocations
                    .resume(
                        owner,
                        ContinuePlugin {
                            id: "call".into(),
                            sequence,
                            response: Ok("{}".into()),
                        },
                    )
                    .await
                    .is_err()
            );
            assert!(matches!(replies.try_recv(), Err(mpsc::TryRecvError::Empty)));
        }
        assert!(invocations.cancel("other", "call").await.is_err());
        assert!(!cancelled.load(Ordering::Relaxed));
        invocations.cancel("owner", "call").await.unwrap();
        assert!(cancelled.load(Ordering::Relaxed));
        assert!(matches!(
            replies.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        assert!(invocations.entry("call").await.is_err());
    }
}
