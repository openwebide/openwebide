//! Bounded polling and concurrency above runtime-specific RPC and clock adapters.
use futures::{StreamExt, future::BoxFuture, stream::FuturesUnordered};
use std::{future::Future, time::Duration};
pub struct Page<D> {
    pub items: Vec<D>,
    pub next_after: Option<i64>,
}
pub trait Worker: Clone + Send + Sync + 'static {
    type Delivery: Send + 'static;
    fn poll(
        &self,
        host: &str,
        after: i64,
    ) -> impl Future<Output = Result<Page<Self::Delivery>, String>> + Send;
    fn execute(&self, delivery: Self::Delivery) -> impl Future<Output = Result<(), String>> + Send;
    fn belongs_to(&self, delivery: &Self::Delivery, host: &str) -> bool;
    fn wait(&self, duration: Duration) -> impl Future<Output = ()> + Send;
    fn report(&self, error: &str);
}
pub(super) type Deliveries = FuturesUnordered<BoxFuture<'static, Result<(), String>>>;
pub(super) async fn poll<W: Worker>(
    worker: &W,
    host: &str,
    after: i64,
    running: &mut Deliveries,
) -> Result<Page<W::Delivery>, String> {
    let mut request = Box::pin(worker.poll(host, after));
    loop {
        let complete = async {
            if running.is_empty() {
                futures::future::pending::<()>().await;
            }
            running.next().await
        };
        match futures::future::select(request, Box::pin(complete)).await {
            futures::future::Either::Left((result, remaining)) => {
                drop(remaining);
                return result;
            }
            futures::future::Either::Right((result, remaining)) => {
                request = remaining;
                if let Some(Err(error)) = result {
                    worker.report(&error);
                }
            }
        }
    }
}
pub async fn serve<W: Worker>(worker: W, hosts: Vec<String>) {
    if hosts.is_empty() {
        return;
    }
    let mut running = Deliveries::new();
    let mut cursors = vec![0; hosts.len()];
    let mut selected = 0;
    loop {
        let complete = async {
            if running.is_empty() {
                futures::future::pending::<()>().await;
            }
            running.next().await
        };
        match futures::future::select(
            Box::pin(worker.wait(Duration::from_secs(5))),
            Box::pin(complete),
        )
        .await
        {
            futures::future::Either::Right((result, _)) => {
                if let Some(Err(error)) = result {
                    worker.report(&error);
                }
                continue;
            }
            futures::future::Either::Left(((), remaining)) => drop(remaining),
        }
        if running.len() > 8 {
            continue;
        }
        let index = selected;
        selected = (selected + 1) % hosts.len();
        let page = match poll(&worker, &hosts[index], cursors[index], &mut running).await {
            Ok(page) => page,
            Err(error) => {
                worker.report(&error);
                continue;
            }
        };
        if page.items.len() > 8
            || page
                .items
                .iter()
                .any(|item| !worker.belongs_to(item, &hosts[index]))
            || page.next_after.is_some_and(|next| next <= cursors[index])
        {
            worker.report("Invalid plugin delivery page");
            continue;
        }
        cursors[index] = page.next_after.unwrap_or(0);
        for item in page.items {
            let worker = worker.clone();
            running.push(Box::pin(async move { worker.execute(item).await }));
        }
    }
}
