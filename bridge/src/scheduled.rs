//! Native clock and process adapter for the shared saved-prompt dispatcher.
use crate::{ServerConfig, runs::backend_client::BackendClient};
use openwebide_core::scheduled::{DispatchResult, ExecutionHost};
use std::{sync::Arc, time::Duration};
pub fn host(config: &ServerConfig) -> ExecutionHost {
    let name = crate::server::get_system_hostname().unwrap_or_else(|| "Execution host".into());
    ExecutionHost {
        id: std::env::var("OPENWEBIDE_SCHEDULER_HOST")
            .unwrap_or_else(|_| format!("{name}@{}", config.workspace_root.display())),
        name,
        last_seen: 0,
    }
}
pub async fn serve(config: ServerConfig) {
    let mut hosts = vec![host(&config)];
    if config.pairing_token.is_none() {
        hosts.push(ExecutionHost {
            id: "server".into(),
            name: hosts[0].name.clone(),
            last_seen: 0,
        });
    }
    let http = crate::runs::http_client::ReqwestHttpClient::default();
    let backend = Arc::new(BackendClient::new(
        config.backend_url.clone(),
        config.secret.clone(),
        http.clone(),
    ));
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut running = tokio::task::JoinSet::new();
    loop {
        interval.tick().await;
        while running.try_join_next().is_some() {}
        if running.len() >= 10 {
            continue;
        }
        for host in &hosts {
            if running.len() >= 10 {
                break;
            }
            let deliveries = match backend.due_tasks(host).await {
                Ok(deliveries) => deliveries,
                Err(error) => {
                    tracing::debug!(%error,"scheduled task poll unavailable");
                    continue;
                }
            };
            for delivery in deliveries.into_iter().take(10 - running.len()) {
                let backend = backend.clone();
                let config = config.clone();
                let host = host.clone();
                let http = http.clone();
                running.spawn(async move {
                    let principal = crate::auth::Principal::User {
                        user_id: delivery.user_id,
                    };
                    let start = crate::runs::StartRun {
                        run_id: format!("scheduled-{}", delivery.run_id),
                        session_id: delivery.session_id,
                        content: delivery.prompt.content.clone(),
                        model: None,
                        editor_context: None,
                        browser_preferences: None,
                        queued_prompt: Some(delivery.prompt.key()),
                        host_path: delivery.binding.map(|binding| binding.path),
                    };
                    let run =
                        async {
                            let run = config.runs.reserve(&principal, &start)?;
                            config.runs.prepare(
                            run, start, &config.workspace_root, backend.clone(),
                            |plan| {
                                openwebide_llm::registry::Provider::for_connection_with_memo(
                                    &plan.connection,
                                    http.with_transport(plan.transport.clone()),
                                    config.tool_stream_memos.get_or_insert(&plan.connection),
                                )
                            },
                            config.run_host(),
                        ).await
                        }
                        .await;
                    match run {
                        Err((code, detail)) => {
                            let status = if code == openwebide_core::RunRejectCode::Busy {
                                "queued"
                            } else {
                                "failed"
                            };
                            let _ = backend
                                .task_result(
                                    &host.id,
                                    &DispatchResult {
                                        run_id: delivery.run_id,
                                        status: status.into(),
                                        detail: DispatchResult::bounded_detail(&detail),
                                        permission_id: None,
                                    },
                                )
                                .await;
                        }
                        Ok(run) => loop {
                            let (finished, permission) = run.scheduled_status();
                            let (status, detail) = match finished.as_ref() {
                                Some(openwebide_core::RunEvent::Done { message }) => (
                                    "complete",
                                    openwebide_core::assistance::completion_excerpt(
                                        &message.content,
                                    ),
                                ),
                                Some(openwebide_core::RunEvent::Cancelled) => {
                                    ("cancelled", "Run cancelled".into())
                                }
                                Some(openwebide_core::RunEvent::Error { message }) => {
                                    ("failed", message.clone())
                                }
                                _ if permission.is_some() => (
                                    "blocked",
                                    permission.as_ref().map_or_else(String::new, |step| {
                                        step.note.clone().unwrap_or_else(|| step.summary.clone())
                                    }),
                                ),
                                _ => ("running", String::new()),
                            };
                            let delivered = backend
                                .task_result(
                                    &host.id,
                                    &DispatchResult {
                                        run_id: delivery.run_id,
                                        status: status.into(),
                                        detail: DispatchResult::bounded_detail(&detail),
                                        permission_id: permission.map(|step| step.id),
                                    },
                                )
                                .await;
                            if finished.is_some() && delivered.is_ok() {
                                break;
                            }
                            tokio::time::sleep(Duration::from_secs(5)).await;
                        },
                    }
                });
            }
        }
    }
}
