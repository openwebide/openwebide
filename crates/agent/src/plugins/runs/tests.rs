use super::*;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    renewals: usize,
    starts: usize,
    cancels: usize,
    releases: usize,
    reports: Vec<RunReport>,
    errors: Vec<String>,
    fail_report: bool,
}
#[derive(Clone)]
struct Host {
    delivery: RunDelivery,
    state: Arc<Mutex<State>>,
    reject: Option<RunRejectCode>,
    held: bool,
    lost: bool,
    cancel_requested: bool,
}
fn host(mode: &str) -> Host {
    Host {
        delivery: RunDelivery {
            user_id: 17,
            run: PluginRun {
                id: 5,
                revision: 2,
                key: "check:1".into(),
                session_id: Some(9),
                state: RunState::Leased,
                created_at: 0,
                detail: String::new(),
                message_id: None,
                permission_id: None,
            },
            prompt: openwebide_core::QueuedPrompt {
                scheduled_task: None,
                plugin_run: Some(5),
                id: 7,
                session_id: 9,
                revision: 2,
                content: "Check the build".into(),
                created_at: 0,
                guidance: false,
            },
            host_path: (mode != "server").then(|| "/home/project".into()),
            lease: RunLease {
                host_id: mode.into(),
                id: 5,
                lease: "opaque-nonce".into(),
            },
            lease_expires_at: 120,
        },
        state: Arc::default(),
        reject: None,
        held: false,
        lost: false,
        cancel_requested: false,
    }
}
impl RunHost for Host {
    type Run = ();
    async fn request(&self, command: RunServiceRequest) -> Result<RunServiceResponse, String> {
        let mut state = self.state.lock().unwrap();
        match command {
            RunServiceRequest::Renew { delivery } => {
                assert_eq!(delivery.host_id, self.delivery.lease.host_id);
                assert_eq!(delivery.lease, self.delivery.lease.lease);
                state.renewals += 1;
                if self.lost && state.renewals > 1 {
                    return Err("Lease lost".into());
                }
                Ok(RunServiceResponse::Renewed(RunLeaseStatus {
                    expires_at: 120,
                    cancel_requested: self.cancel_requested,
                }))
            }
            RunServiceRequest::Report { delivery, report } => {
                assert_eq!(delivery.id, 5);
                report.validate().unwrap();
                if state.fail_report {
                    state.fail_report = false;
                    return Err("Temporary report failure".into());
                }
                state.reports.push(report);
                Ok(RunServiceResponse::Reported)
            }
            RunServiceRequest::Release { .. } => {
                state.releases += 1;
                Ok(RunServiceResponse::Released)
            }
            RunServiceRequest::Claim { .. } => unreachable!(),
        }
    }
    async fn start(&self, delivery: &RunDelivery) -> Result<(), (RunRejectCode, String)> {
        assert_eq!(delivery.run_id(), "plugin-run-5-opaque-nonce");
        self.state.lock().unwrap().starts += 1;
        if self.held {
            futures::future::pending::<()>().await;
        }
        match &self.reject {
            Some(code) => Err((code.clone(), "é".repeat(3000))),
            None => Ok(()),
        }
    }
    fn snapshot(&self, (): &()) -> (Option<RunEvent>, Option<RunStep>) {
        (Some(RunEvent::Cancelled), None)
    }
    fn cancel(&self, delivery: &RunDelivery) {
        assert_eq!(delivery.lease.lease, "opaque-nonce");
        self.state.lock().unwrap().cancels += 1;
    }
    fn now(&self) -> i64 {
        0
    }
    async fn wait(&self, duration: Duration) {
        if duration.as_secs() == 30 && !self.lost {
            futures::future::pending::<()>().await;
        }
    }
    fn report(&self, error: &str) {
        self.state.lock().unwrap().errors.push(error.into());
    }
}

#[test]
fn delivery_reports_terminal_state_and_retries_transient_reporting_in_both_modes() {
    for mode in ["server", "paired-host"] {
        let host = host(mode);
        host.state.lock().unwrap().fail_report = true;
        futures::executor::block_on(deliver(&host, host.delivery.clone())).unwrap();
        let state = host.state.lock().unwrap();
        assert_eq!(state.starts, 1);
        assert_eq!(state.renewals, 1);
        assert_eq!(state.cancels, 0);
        assert_eq!(state.errors, ["Temporary report failure"]);
        assert_eq!(state.reports.len(), 1);
        assert_eq!(state.reports[0].state, RunState::Cancelled);
    }
}

#[test]
fn busy_runs_release_claim_and_preflight_failures_are_bounded() {
    for mode in ["server", "paired-host"] {
        for reject in [RunRejectCode::Busy, RunRejectCode::PlanFailed] {
            let mut host = host(mode);
            host.reject = Some(reject.clone());
            futures::executor::block_on(deliver(&host, host.delivery.clone())).unwrap();
            let state = host.state.lock().unwrap();
            assert_eq!(state.cancels, 0);
            if reject == RunRejectCode::Busy {
                assert_eq!(state.releases, 1);
                assert!(state.reports.is_empty());
            } else {
                assert_eq!(state.releases, 0);
                assert_eq!(state.reports[0].state, RunState::Failed);
                assert_eq!(state.reports[0].detail.len(), 4096);
            }
        }
    }
}

#[test]
fn losing_a_lease_drops_preparation_and_cancels_only_owned_run() {
    for mode in ["server", "paired-host"] {
        let mut host = host(mode);
        host.held = true;
        host.lost = true;
        assert!(futures::executor::block_on(deliver(&host, host.delivery.clone())).is_err());
        let state = host.state.lock().unwrap();
        assert_eq!(state.starts, 1);
        assert_eq!(state.renewals, 2);
        assert_eq!(state.cancels, 1);
        assert!(state.reports.is_empty());
    }
}

#[test]
fn shutdown_cancels_held_preparation_without_reporting_completion() {
    use std::task::{Context, Poll};
    for mode in ["server", "paired-host"] {
        let mut host = host(mode);
        host.held = true;
        let mut delivery = Box::pin(deliver(&host, host.delivery.clone()));
        let waker = futures::task::noop_waker();
        assert!(matches!(
            delivery.as_mut().poll(&mut Context::from_waker(&waker)),
            Poll::Pending
        ));
        drop(delivery);
        let state = host.state.lock().unwrap();
        assert_eq!(state.cancels, 1);
        assert!(state.reports.is_empty());
    }
}

#[test]
fn cancellation_is_forwarded_and_invalid_delivery_never_starts() {
    for mode in ["server", "paired-host"] {
        let mut host = host(mode);
        host.cancel_requested = true;
        futures::executor::block_on(deliver(&host, host.delivery.clone())).unwrap();
        assert_eq!(host.state.lock().unwrap().cancels, 1);
        let mut invalid = host.delivery.clone();
        invalid.prompt.session_id = 10;
        assert!(futures::executor::block_on(deliver(&host, invalid)).is_err());
        assert_eq!(host.state.lock().unwrap().starts, 0);
    }
}

#[test]
fn permission_reports_preserve_approval_and_terminal_events_take_precedence() {
    let step = RunStep {
        timing: None,
        id: "permission-1".into(),
        name: "write".into(),
        summary: "Write file".into(),
        awaiting_permission: true,
        diff: None,
        note: Some("Needs approval".into()),
        result: None,
    };
    let blocked = status(None, Some(step.clone()));
    assert_eq!(blocked.state, RunState::Blocked);
    assert_eq!(blocked.permission_id.as_deref(), Some("permission-1"));
    assert_eq!(blocked.detail, "Needs approval");
    let failed = status(
        Some(RunEvent::Error {
            message: "Failed".into(),
        }),
        Some(step),
    );
    assert_eq!(failed.state, RunState::Failed);
    assert!(failed.permission_id.is_none());
}
