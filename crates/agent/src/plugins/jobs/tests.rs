use super::*;
use openwebide_core::plugins::{
    RustPlugin,
    execution::{ContinuePlugin, PluginExecutionContext, PluginInvocation, PluginStep},
    jobs::{Job, JobState},
};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Host {
    delivery: JobDelivery,
    log: Arc<Mutex<Vec<String>>>,
    lost: bool,
    changed: bool,
    blocked: bool,
    changed_context: u8,
    slow_claim: bool,
    fail: bool,
    renewals: Arc<Mutex<usize>>,
    ticks: Arc<Mutex<usize>>,
}
fn host(mode: &str) -> Host {
    let mut prepared = openwebide_core::plugins::testing::receipt();
    prepared.host_id = mode.into();
    prepared.manifest.compatibility.plugin_api = 3;
    prepared.manifest.contributions.skills.clear();
    prepared.manifest.contributions.events = vec!["tick".into()];
    prepared.manifest.executable = Some(RustPlugin {
        manifest: "Cargo.toml".into(),
        library: "community_timer".into(),
        sdk_version: "0.1.0".into(),
        capabilities: vec!["records".into()],
    });
    Host {
        delivery: JobDelivery {
            user_id: 17,
            prepared,
            context: PluginExecutionContext {
                project_id: Some(3),
                session_id: Some(9),
                ..Default::default()
            },
            lease: "opaque-lease".into(),
            lease_expires_at: 120,
            job: Job {
                id: 5,
                revision: 2,
                key: "timer:1".into(),
                due_at: 0,
                expires_at: None,
                event: "tick".into(),
                payload: serde_json::json!({"key":"1"}),
                state: JobState::Leased,
                attempts: 1,
                detail: None,
            },
        },
        log: Arc::default(),
        renewals: Arc::default(),
        ticks: Arc::default(),
        lost: false,
        changed: false,
        blocked: false,
        changed_context: 0,
        slow_claim: false,
        fail: false,
    }
}
impl Host {
    fn record(&self, message: impl Into<String>) {
        self.log.lock().unwrap().push(message.into());
    }
}
impl JobHost for Host {
    async fn request(&self, command: JobServiceRequest) -> Result<JobServiceResponse, String> {
        match command {
            JobServiceRequest::Renew { delivery } => {
                assert_eq!(delivery.host_id, self.delivery.prepared.host_id);
                assert_eq!(delivery.id, 5);
                assert_eq!(delivery.lease, "opaque-lease");
                self.record("renew");
                let mut count = self.renewals.lock().unwrap();
                *count += 1;
                if self.lost && *count > 1 {
                    return Err("lease lost".into());
                }
                Ok(JobServiceResponse::Renewed(120))
            }
            JobServiceRequest::Grant { user_id, .. } => {
                assert_eq!(user_id, 17);
                self.record("grant");
                let mut prepared = self.delivery.prepared.clone();
                if self.changed {
                    prepared.digest = "c".repeat(64);
                }
                let mut context = self.delivery.context.clone();
                context.session_id = None;
                match self.changed_context {
                    1 => context.project_id = Some(999),
                    2 => context.session_id = Some(9),
                    3 => context.user_action = true,
                    _ => (),
                }
                Ok(JobServiceResponse::Authorized {
                    prepared: Box::new(prepared),
                    context,
                    grant: "opaque-grant".into(),
                })
            }
            JobServiceRequest::Callback { user_id, request } => {
                assert_eq!(user_id, 17);
                assert_eq!(request.grant, "opaque-grant");
                assert_eq!(request.capability, "records");
                self.record("callback");
                Ok(JobServiceResponse::Callback("stored".into()))
            }
            JobServiceRequest::Finish {
                success, detail, ..
            } => {
                assert!(detail.len() <= 4096);
                self.record(format!("finish:{success}"));
                Ok(JobServiceResponse::Finished)
            }
            JobServiceRequest::Claim { .. } => {
                self.record("claim");
                if self.slow_claim {
                    futures::future::pending::<()>().await;
                }
                Ok(JobServiceResponse::Claimed(
                    openwebide_core::plugins::jobs::JobDeliveryPage {
                        jobs: vec![self.delivery.clone()],
                        next_after: None,
                    },
                ))
            }
        }
    }
    fn now(&self) -> i64 {
        1
    }
    async fn wait(&self, duration: Duration) {
        if duration == Duration::from_secs(5) {
            let first = {
                let mut ticks = self.ticks.lock().unwrap();
                *ticks += 1;
                *ticks == 1
            };
            if first {
                return;
            }
        }
        if !self.lost {
            futures::future::pending::<()>().await;
        }
    }
}
impl PluginTransport for Host {
    async fn ensure_prepared(&self, expected: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        self.record("prepare");
        Ok(expected.clone())
    }
    async fn start(&self, call: InvokePlugin) -> Result<PluginInvocation, String> {
        call.validate_event().unwrap();
        assert!(matches!(call.operation, PluginOperation::Event));
        assert_eq!(call.prepared, self.delivery.prepared);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&call.arguments).unwrap(),
            serde_json::json!({"name":"tick", "payload":{"key":"1"}})
        );
        assert!(!call.arguments.contains("opaque"));
        self.record("start");
        Ok(PluginInvocation {
            id: "actor".into(),
            step: PluginStep::Ready,
        })
    }
    async fn resume(&self, response: ContinuePlugin) -> Result<PluginInvocation, String> {
        if self.lost || self.blocked {
            futures::future::pending::<()>().await;
        }
        let step = if response.sequence == 0 {
            PluginStep::HostCall {
                sequence: 1,
                capability: "records".into(),
                payload: "{}".into(),
            }
        } else {
            assert_eq!(response.response, Ok("stored".into()));
            PluginStep::Complete {
                ok: !self.fail,
                content: String::new(),
                summary: "界".repeat(2000),
            }
        };
        Ok(PluginInvocation {
            id: "actor".into(),
            step,
        })
    }
    fn cancel(&self, _: String) {
        self.record("cancel");
    }
    async fn flush_cancellations(&self) {
        self.record("flush");
    }
}
#[test]
fn both_hosts_deliver_events_with_sessionless_authority_and_bounded_acknowledgements() {
    for mode in ["server", "paired"] {
        for fail in [false, true] {
            let mut host = host(mode);
            host.fail = fail;
            futures::executor::block_on(deliver(&host, &host, host.delivery.clone())).unwrap();
            assert_eq!(
                *host.log.lock().unwrap(),
                [
                    "renew",
                    "prepare",
                    "grant",
                    "start",
                    "callback",
                    "flush",
                    if fail { "finish:false" } else { "finish:true" }
                ]
            );
        }
    }
}
#[test]
fn lease_loss_drops_the_actor_before_flushing_and_never_acknowledges() {
    for mode in ["server", "paired"] {
        let mut host = host(mode);
        host.lost = true;
        assert_eq!(
            futures::executor::block_on(deliver(&host, &host, host.delivery.clone())),
            Err("lease lost".into())
        );
        assert_eq!(
            *host.log.lock().unwrap(),
            [
                "renew", "prepare", "grant", "start", "renew", "cancel", "flush"
            ]
        );
    }
}
#[test]
fn mismatched_authority_never_starts_plugin_code() {
    for mode in ["server", "paired"] {
        let mut host = host(mode);
        host.changed = true;
        futures::executor::block_on(deliver(&host, &host, host.delivery.clone())).unwrap();
        assert_eq!(
            *host.log.lock().unwrap(),
            ["renew", "prepare", "grant", "finish:false"]
        );
    }
}

impl JobWorkerHost for Host {
    type Transport = Self;
    fn transport(&self, _: i64) -> Self {
        self.clone()
    }
    fn report(&self, error: &str) {
        self.record(format!("report:{error}"));
    }
}
#[test]
fn automatic_worker_claims_and_delivers_for_both_host_adapters() {
    use std::future::Future;
    for mode in ["server", "paired"] {
        let host = host(mode);
        let mut worker = Box::pin(serve(host.clone(), vec![mode.into()]));
        let mut context = std::task::Context::from_waker(futures::task::noop_waker_ref());
        assert!(worker.as_mut().poll(&mut context).is_pending());
        assert_eq!(
            *host.log.lock().unwrap(),
            [
                "claim",
                "renew",
                "prepare",
                "grant",
                "start",
                "callback",
                "flush",
                "finish:true"
            ]
        );
        drop(worker);
    }
}

#[test]
fn changed_job_scope_cannot_consume_chat_or_manual_authority() {
    for mode in ["server", "paired"] {
        for changed in 1..=3 {
            let mut host = host(mode);
            host.changed_context = changed;
            futures::executor::block_on(deliver(&host, &host, host.delivery.clone())).unwrap();
            assert_eq!(
                *host.log.lock().unwrap(),
                ["renew", "prepare", "grant", "finish:false"]
            );
        }
    }
}
#[test]
fn dropping_the_worker_cancels_inflight_actors_without_acknowledging() {
    use std::future::Future;
    for mode in ["server", "paired"] {
        let mut host = host(mode);
        host.blocked = true;
        let mut worker = Box::pin(serve(host.clone(), vec![mode.into()]));
        let mut context = std::task::Context::from_waker(futures::task::noop_waker_ref());
        assert!(worker.as_mut().poll(&mut context).is_pending());
        drop(worker);
        assert_eq!(
            *host.log.lock().unwrap(),
            ["claim", "renew", "prepare", "grant", "start", "cancel"]
        );
    }
}

#[test]
fn a_slow_claim_does_not_suspend_existing_delivery_futures() {
    use std::future::Future;
    let mut host = host("server");
    host.slow_claim = true;
    let mut running = super::super::worker::Deliveries::new();
    let active = host.clone();
    running.push(Box::pin(async move {
        active.record("active-delivery");
        Ok(())
    }));
    let worker = Worker(host.clone());
    let mut claim = Box::pin(super::super::worker::poll(
        &worker,
        "server",
        0,
        &mut running,
    ));
    let mut context = std::task::Context::from_waker(futures::task::noop_waker_ref());
    assert!(claim.as_mut().poll(&mut context).is_pending());
    assert_eq!(*host.log.lock().unwrap(), ["claim", "active-delivery"]);
}
