//! Shared durable delivery policy. Adapters provide RPC, time and plugin transport.
use super::execution::{
    PluginServices, PluginTransport, invoke_plugin, validate_prepared_selection,
};
use openwebide_core::plugins::{
    PreparedPlugin,
    execution::{InvokePlugin, PluginHostRequest, PluginOperation},
    jobs::{JobDelivery, JobLease, JobServiceRequest, JobServiceResponse},
};
use std::{future::Future, time::Duration};

pub trait JobHost: Send + Sync {
    fn request(
        &self,
        command: JobServiceRequest,
    ) -> impl Future<Output = Result<JobServiceResponse, String>> + Send;
    fn now(&self) -> i64;
    fn wait(&self, duration: Duration) -> impl Future<Output = ()> + Send;
}

async fn renew<H: JobHost>(host: &H, lease: &JobLease) -> Result<(), String> {
    match host
        .request(JobServiceRequest::Renew {
            delivery: lease.clone(),
        })
        .await?
    {
        JobServiceResponse::Renewed(expiry) if expiry > host.now() => Ok(()),
        _ => Err("Plugin job lease is unavailable".into()),
    }
}

/// Keep compilation and execution leased; losing authority drops the actor future.
/// Delivery is at least once: plugin keys and revision checks own effect idempotency.
pub async fn deliver<H: JobHost, T: PluginTransport>(
    host: &H,
    transport: &T,
    delivery: JobDelivery,
) -> Result<(), String> {
    let lease = JobLease {
        host_id: delivery.prepared.host_id.clone(),
        id: delivery.job.id,
        lease: delivery.lease.clone(),
    };
    renew(host, &lease).await?;
    let execution = execute(host, transport, &delivery, &lease);
    let heartbeat = async {
        loop {
            host.wait(Duration::from_secs(30)).await;
            renew(host, &lease).await?;
        }
    };
    let result = match futures::future::select(Box::pin(execution), Box::pin(heartbeat)).await {
        futures::future::Either::Left((result, _)) => result,
        futures::future::Either::Right((result, execution)) => {
            drop(execution);
            transport.flush_cancellations().await;
            return result;
        }
    };
    let (success, detail) = match result {
        Ok(outcome) => (outcome.ok, outcome.summary),
        Err(error) => (false, error),
    };
    let mut end = detail.len().min(4096);
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    match host
        .request(JobServiceRequest::Finish {
            delivery: lease,
            success,
            detail: detail[..end].into(),
        })
        .await?
    {
        JobServiceResponse::Finished => Ok(()),
        _ => Err("Unexpected plugin job acknowledgement".into()),
    }
}

async fn execute<H: JobHost, T: PluginTransport>(
    host: &H,
    transport: &T,
    delivery: &JobDelivery,
    lease: &JobLease,
) -> Result<crate::ToolOutcome, String> {
    let prepared = transport.ensure_prepared(&delivery.prepared).await?;
    validate_prepared_selection(&delivery.prepared, &prepared)?;
    let JobServiceResponse::Authorized {
        prepared: authorized,
        context,
        grant,
    } = host
        .request(JobServiceRequest::Grant {
            user_id: delivery.user_id,
            delivery: lease.clone(),
        })
        .await?
    else {
        return Err("Unexpected plugin job authority response".into());
    };
    validate_prepared_selection(&delivery.prepared, &authorized)?;
    if context.project_id != delivery.context.project_id
        || context.primary != delivery.context.primary
        || context.session_id.is_some()
        || context.user_action
        || grant.is_empty()
    {
        return Err("Plugin job execution context changed".into());
    }
    let services = JobServices {
        host,
        user_id: delivery.user_id,
        grant,
    };
    // Preparation above runs under the lease before issuing a callback grant.
    // Reuse its receipt rather than compile again inside the common invocation.
    let transport = PreparedTransport {
        transport,
        prepared: &prepared,
    };
    invoke_plugin(
        &transport,
        &services,
        InvokePlugin {
            prepared: prepared.clone(),
            operation: PluginOperation::Event,
            name: String::new(),
            arguments: serde_json::to_string(&openwebide_core::plugins::execution::EventInput {
                name: delivery.job.event.clone(),
                payload: delivery.job.payload.clone(),
            })
            .map_err(|error| error.to_string())?,
        },
    )
    .await
}
struct JobServices<'a, H> {
    host: &'a H,
    user_id: i64,
    grant: String,
}
impl<H: JobHost> PluginServices for JobServices<'_, H> {
    async fn request(
        &self,
        _: &PreparedPlugin,
        capability: &str,
        payload: &str,
    ) -> Result<String, String> {
        match self
            .host
            .request(JobServiceRequest::Callback {
                user_id: self.user_id,
                request: PluginHostRequest {
                    grant: self.grant.clone(),
                    capability: capability.into(),
                    payload: payload.into(),
                },
            })
            .await?
        {
            JobServiceResponse::Callback(result) => Ok(result),
            _ => Err("Unexpected plugin job callback response".into()),
        }
    }
}
struct PreparedTransport<'a, T> {
    transport: &'a T,
    prepared: &'a PreparedPlugin,
}
impl<T> Clone for PreparedTransport<'_, T> {
    fn clone(&self) -> Self {
        Self {
            transport: self.transport,
            prepared: self.prepared,
        }
    }
}
impl<T: PluginTransport> PluginTransport for PreparedTransport<'_, T> {
    async fn ensure_prepared(&self, expected: &PreparedPlugin) -> Result<PreparedPlugin, String> {
        validate_prepared_selection(expected, self.prepared)?;
        Ok(self.prepared.clone())
    }
    async fn start(
        &self,
        call: InvokePlugin,
    ) -> Result<openwebide_core::plugins::execution::PluginInvocation, String> {
        self.transport.start(call).await
    }
    async fn resume(
        &self,
        call: openwebide_core::plugins::execution::ContinuePlugin,
    ) -> Result<openwebide_core::plugins::execution::PluginInvocation, String> {
        self.transport.resume(call).await
    }
    fn cancel(&self, id: String) {
        self.transport.cancel(id);
    }
    async fn flush_cancellations(&self) {
        self.transport.flush_cancellations().await;
    }
}

#[cfg(test)]
mod tests;

/// Process/clock adapters select execution hosts and supply per-account transports.
pub trait JobWorkerHost: JobHost + Clone + Send + Sync + 'static {
    type Transport: PluginTransport + 'static;
    fn transport(&self, user_id: i64) -> Self::Transport;
    fn report(&self, error: &str);
}

#[derive(Clone)]
struct Worker<H>(H);
impl<H: JobWorkerHost> super::worker::Worker for Worker<H> {
    type Delivery = JobDelivery;
    async fn poll(
        &self,
        host: &str,
        after: i64,
    ) -> Result<super::worker::Page<JobDelivery>, String> {
        let JobServiceResponse::Claimed(page) = self
            .0
            .request(JobServiceRequest::Claim {
                host_id: host.into(),
                after,
            })
            .await?
        else {
            return Err("Unexpected plugin job claim response".into());
        };
        Ok(super::worker::Page {
            items: page.jobs,
            next_after: page.next_after,
        })
    }
    async fn execute(&self, delivery: JobDelivery) -> Result<(), String> {
        let transport = self.0.transport(delivery.user_id);
        deliver(&self.0, &transport, delivery).await
    }
    fn belongs_to(&self, delivery: &JobDelivery, host: &str) -> bool {
        delivery.prepared.host_id == host
    }
    async fn wait(&self, duration: Duration) {
        self.0.wait(duration).await;
    }
    fn report(&self, error: &str) {
        self.0.report(error);
    }
}
pub async fn serve<H: JobWorkerHost>(host: H, hosts: Vec<String>) {
    super::worker::serve(Worker(host), hosts).await;
}
