//! Raw run delivery policy above RPC, clock and runner adapters.
use openwebide_core::{RunEvent, RunRejectCode, RunStep, plugins::runs::*};
use std::{future::Future, time::Duration};
pub trait RunHost: Clone + Send + Sync + 'static {
    type Run: Send + Sync;
    fn request(
        &self,
        command: RunServiceRequest,
    ) -> impl Future<Output = Result<RunServiceResponse, String>> + Send;
    fn start(
        &self,
        delivery: &RunDelivery,
    ) -> impl Future<Output = Result<Self::Run, (RunRejectCode, String)>> + Send;
    fn snapshot(&self, run: &Self::Run) -> (Option<RunEvent>, Option<RunStep>);
    fn cancel(&self, delivery: &RunDelivery);
    fn now(&self) -> i64;
    fn wait(&self, duration: Duration) -> impl Future<Output = ()> + Send;
    fn report(&self, error: &str);
}
struct Guard<'a, H: RunHost> {
    host: &'a H,
    delivery: &'a RunDelivery,
    armed: bool,
}
impl<H: RunHost> Drop for Guard<'_, H> {
    fn drop(&mut self) {
        if self.armed {
            self.host.cancel(self.delivery);
        }
    }
}
fn bounded(value: &str) -> String {
    let mut end = value.len().min(4096);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].into()
}
pub fn status(finished: Option<RunEvent>, permission: Option<RunStep>) -> RunReport {
    let (state, detail, permission_id) = match finished {
        Some(RunEvent::Done { message }) => (RunState::Completed, message.content, None),
        Some(RunEvent::Cancelled) => (RunState::Cancelled, "Run cancelled".into(), None),
        Some(RunEvent::Error { message }) => (RunState::Failed, message, None),
        _ => match permission {
            Some(step) => (
                RunState::Blocked,
                step.note.unwrap_or(step.summary),
                Some(step.id),
            ),
            None => (RunState::Running, String::new(), None),
        },
    };
    RunReport {
        state,
        detail: bounded(&detail),
        permission_id,
    }
}
async fn renew<H: RunHost>(host: &H, delivery: &RunDelivery) -> Result<bool, String> {
    let RunServiceResponse::Renewed(status) = host
        .request(RunServiceRequest::Renew {
            delivery: delivery.lease.clone(),
        })
        .await?
    else {
        return Err("Unexpected run lease response".into());
    };
    if status.expires_at <= host.now() {
        return Err("Plugin run lease expired".into());
    }
    if status.cancel_requested {
        host.cancel(delivery);
    }
    Ok(status.cancel_requested)
}
async fn acknowledge<H: RunHost>(
    host: &H,
    delivery: &RunDelivery,
    report: RunReport,
) -> Result<(), String> {
    loop {
        match host
            .request(RunServiceRequest::Report {
                delivery: delivery.lease.clone(),
                report: report.clone(),
            })
            .await
        {
            Ok(RunServiceResponse::Reported) => return Ok(()),
            Ok(_) => return Err("Unexpected run report response".into()),
            Err(error) => {
                host.report(&error);
                host.wait(Duration::from_secs(5)).await;
            }
        }
    }
}
pub async fn deliver<H: RunHost>(host: &H, delivery: RunDelivery) -> Result<(), String> {
    if delivery.run.id != delivery.lease.id
        || delivery.user_id <= 0
        || delivery.run.session_id != Some(delivery.prompt.session_id)
        || delivery.prompt.plugin_run != Some(delivery.run.id)
    {
        return Err("Invalid plugin run delivery".into());
    }
    let mut guard = Guard {
        host,
        delivery: &delivery,
        armed: true,
    };
    let cancelled = renew(host, &delivery).await?;
    let execute = async {
        if cancelled {
            acknowledge(
                host,
                &delivery,
                RunReport {
                    state: RunState::Cancelled,
                    detail: "Run cancelled before execution".into(),
                    permission_id: None,
                },
            )
            .await?;
            return Ok(());
        }
        let run = match host.start(&delivery).await {
            Ok(run) => run,
            Err((RunRejectCode::Busy, _)) => {
                let RunServiceResponse::Released = host
                    .request(RunServiceRequest::Release {
                        delivery: delivery.lease.clone(),
                    })
                    .await?
                else {
                    return Err("Unexpected run release response".into());
                };
                return Ok(());
            }
            Err((_, detail)) => {
                acknowledge(
                    host,
                    &delivery,
                    RunReport {
                        state: RunState::Failed,
                        detail: bounded(&detail),
                        permission_id: None,
                    },
                )
                .await?;
                return Ok(());
            }
        };
        loop {
            let (finished, permission) = host.snapshot(&run);
            let report = status(finished, permission);
            let done = report.state.terminal();
            acknowledge(host, &delivery, report).await?;
            if done {
                return Ok(());
            }
            host.wait(Duration::from_secs(5)).await;
        }
    };
    let heartbeat = async {
        loop {
            host.wait(Duration::from_secs(30)).await;
            renew(host, &delivery).await?;
        }
    };
    let result = match futures::future::select(Box::pin(execute), Box::pin(heartbeat)).await {
        futures::future::Either::Left((result, _)) => result,
        futures::future::Either::Right((result, remaining)) => {
            drop(remaining);
            result
        }
    };
    if result.is_ok() {
        guard.armed = false;
    }
    result
}
#[derive(Clone)]
struct Worker<H>(H);
impl<H: RunHost> super::worker::Worker for Worker<H> {
    type Delivery = RunDelivery;
    async fn poll(
        &self,
        host: &str,
        after: i64,
    ) -> Result<super::worker::Page<RunDelivery>, String> {
        let RunServiceResponse::Claimed(page) = self
            .0
            .request(RunServiceRequest::Claim {
                host_id: host.into(),
                after,
            })
            .await?
        else {
            return Err("Unexpected run claim response".into());
        };
        Ok(super::worker::Page {
            items: page.runs,
            next_after: page.next_after,
        })
    }
    async fn execute(&self, delivery: RunDelivery) -> Result<(), String> {
        deliver(&self.0, delivery).await
    }
    fn belongs_to(&self, delivery: &RunDelivery, host: &str) -> bool {
        delivery.lease.host_id == host
    }
    async fn wait(&self, duration: Duration) {
        self.0.wait(duration).await;
    }
    fn report(&self, error: &str) {
        self.0.report(error);
    }
}
pub async fn serve<H: RunHost>(host: H, hosts: Vec<String>) {
    super::worker::serve(Worker(host), hosts).await;
}

#[cfg(test)]
mod tests;
