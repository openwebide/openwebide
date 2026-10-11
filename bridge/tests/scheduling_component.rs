//! Cross-repository component contract; installation/deployment is verified separately.
use futures::executor::block_on;
use openwebide_core::{
    NewConnection, NewProject, ProviderKind, UserId, UserRole, WorkspaceMode,
    plugins::{
        self, PluginPackage, RecordPlugin,
        execution::{PluginExecutionContext, PluginHostRequest},
    },
};
use openwebide_plugin_runtime::{HostServices, Runtime, sdk::EventInput};
use openwebide_storage::{Store, rusqlite_db::RusqliteDb};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
#[derive(Clone)]
struct Services {
    store: Arc<Store<RusqliteDb>>,
    user: UserId,
    grant: String,
    clock: Arc<AtomicI64>,
}
impl HostServices for Services {
    fn request(&mut self, capability: &str, payload: &str) -> Result<String, String> {
        match capability {
            "clock" => Ok(json!({"unix_seconds":self.clock.load(Ordering::Relaxed)}).to_string()),
            "completion" => Err("No model completion in this data-contract fixture".into()),
            "collections" | "records" | "jobs" | "runs" => {
                block_on(self.store.plugin_context_host_request(
                    self.user,
                    &PluginHostRequest {
                        grant: self.grant.clone(),
                        capability: capability.into(),
                        payload: payload.into(),
                    },
                    self.clock.load(Ordering::Relaxed),
                ))
                .map_err(|error| error.to_string())
            }
            _ => panic!("Scheduling tried to use a non-declared feature primitive: {capability}"),
        }
    }
}
#[test]
#[ignore = "Requires the separately built Scheduling component and source manifest paths"]
fn scheduling_component_uses_shared_records_events_and_raw_runs_in_both_modes() {
    let bytes =
        std::fs::read(std::env::var("OPENWEBIDE_SCHEDULING_COMPONENT").expect("component path"))
            .unwrap();
    let manifest: plugins::PluginManifest = serde_json::from_slice(
        &std::fs::read(std::env::var("OPENWEBIDE_SCHEDULING_MANIFEST").expect("manifest path"))
            .unwrap(),
    )
    .unwrap();
    let runtime = Runtime::new().unwrap();
    runtime.validate(&bytes).unwrap();
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let store = Arc::new(Store::new(RusqliteDb::open_in_memory().unwrap()));
        let clock = Arc::new(AtomicI64::new(100));
        let (user, project, session, prepared) = block_on(async {
            store.migrate().await.unwrap();
            let user = store
                .insert_user("owner", "hash", UserRole::Admin, 0)
                .await
                .unwrap()
                .id;
            let project = store
                .create_project(
                    &NewProject {
                        name: "project".into(),
                        mode,
                        path: Some("project".into()),
                    },
                    user,
                    0,
                )
                .await
                .unwrap()
                .id;
            let session = store
                .create_session("conversation", None, None, Some(project), user, 0)
                .await
                .unwrap()
                .id;
            let server = store
                .insert_connection(&NewConnection {
                    name: "model".into(),
                    kind: ProviderKind::Ollama,
                    base_url: "http://model.test".into(),
                    model: Some("configured model".into()),
                    context_limit: None,
                })
                .await
                .unwrap();
            store
                .set_user_setting(
                    user,
                    "model_defaults",
                    &json!({"primary":{"server_id":server.id,"model":"current default"}})
                        .to_string(),
                )
                .await
                .unwrap();
            let mut prepared = plugins::testing::receipt();
            prepared.manifest = manifest.clone();
            prepared.host_id = "host".into();
            store
                .record_plugin(
                    user,
                    &RecordPlugin {
                        prepared: prepared.clone(),
                        revision: None,
                        approved_capabilities: vec![],
                        update_policy: None,
                        package: Some(Box::new(PluginPackage {
                            prepared: prepared.clone(),
                            skills: vec![],
                        })),
                    },
                    0,
                )
                .await
                .unwrap();
            store
                .issue_plugin_context_grant(
                    user,
                    &PluginExecutionContext {
                        user_action: true,
                        project_id: Some(project),
                        session_id: None,
                        primary: None,
                    },
                    &prepared,
                    &"a".repeat(32),
                    1,
                )
                .await
                .unwrap();
            (user, project, session, prepared)
        });
        let mut services = Services {
            store: store.clone(),
            user,
            grant: "a".repeat(32),
            clock: clock.clone(),
        };
        let capabilities = &manifest.executable.as_ref().unwrap().capabilities;
        let tools = runtime.tools(&bytes, services.clone(), &[]).unwrap();
        assert_eq!(
            serde_json::to_value(tools).unwrap(),
            serde_json::to_value(&manifest.contributions.tools).unwrap()
        );
        assert_eq!(
            runtime.events(&bytes).unwrap(),
            manifest.contributions.events
        );
        let created=runtime.execute(&bytes,services.clone(),capabilities,"schedule_create",&json!({"draft":{
            "prompt":"Check build","session_id":session,"session_target":"existing","enabled":true,"schedule":{"kind":"once","at":105}
        }}).to_string()).unwrap();
        assert!(created.ok, "{}", created.content);
        let record: Value = serde_json::from_str(&created.content).unwrap();
        let task = record[0]["id"].as_i64().unwrap();
        assert_eq!(
            block_on(store.scheduled_tasks(user, Some(project), 100)).unwrap()[0].id,
            task
        );
        let original_title = record[0]["title"].clone();
        for (revision, enabled) in [(1, false), (2, true)] {
            let call = openwebide_core::scheduled::TaskCommand::SetEnabled {
                id: task,
                revision,
                enabled,
            }
            .plugin_call()
            .unwrap();
            let outcome = runtime
                .execute(
                    &bytes,
                    services.clone(),
                    capabilities,
                    &call.name,
                    &call.arguments,
                )
                .unwrap();
            assert!(outcome.ok, "{}", outcome.content);
            let value: Value = serde_json::from_str(&outcome.content).unwrap();
            assert_eq!(value[0]["title"], original_title);
            assert_eq!(value[0]["enabled"], enabled);
            assert_eq!(
                block_on(store.scheduled_tasks(user, Some(project), 100)).unwrap()[0]
                    .draft
                    .enabled,
                enabled
            );
        }
        // Simulate a dropped actor after saving a task but before its timer is durable.
        let mut repair = services.clone();
        let jobs: Value = serde_json::from_str(
            &repair
                .request("jobs", &json!({"action":"list","after":0}).to_string())
                .unwrap(),
        )
        .unwrap();
        let timer = jobs["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|job| job["event"] == "task_due" && job["state"] == "pending")
            .unwrap();
        let cancelled: Value = serde_json::from_str(
            &repair
                .request(
                    "jobs",
                    &json!({"action":"cancel","id":timer["id"],"revision":timer["revision"]})
                        .to_string(),
                )
                .unwrap(),
        )
        .unwrap();
        repair.request("jobs", &json!({"action":"delete","id":timer["id"],"revision":cancelled["jobs"][0]["revision"]}).to_string()).unwrap();
        // Execute the host-declared project reconciliation event through its real scoped grant.
        let ticks = block_on(store.claim_plugin_jobs("host", 0, &"f".repeat(32), 100)).unwrap();
        let tick = ticks
            .jobs
            .iter()
            .find(|job| job.context.project_id == Some(project))
            .unwrap();
        assert_eq!(tick.job.event, "reconcile");
        block_on(store.issue_plugin_job_grant(
            user,
            "host",
            tick.job.id,
            &tick.lease,
            &"9".repeat(32),
            100,
        ))
        .unwrap();
        let reconciled = runtime
            .event(
                &bytes,
                Services {
                    grant: "9".repeat(32),
                    ..services.clone()
                },
                capabilities,
                EventInput {
                    name: tick.job.event.clone(),
                    payload: tick.job.payload.clone(),
                },
            )
            .unwrap();
        assert!(reconciled.ok, "{}", reconciled.content);
        block_on(store.finish_plugin_job(
            "host",
            tick.job.id,
            &tick.lease,
            true,
            "Reconciled",
            100,
        ))
        .unwrap();
        clock.store(105, Ordering::Relaxed);
        let due = block_on(store.claim_plugin_jobs("host", 0, &"b".repeat(32), 105))
            .unwrap()
            .jobs
            .remove(0);
        assert_eq!(due.job.event, "task_due");
        block_on(store.issue_plugin_job_grant(
            user,
            "host",
            due.job.id,
            &due.lease,
            &"c".repeat(32),
            105,
        ))
        .unwrap();
        let event_services = Services {
            grant: "c".repeat(32),
            ..services.clone()
        };
        let queued = runtime
            .event(
                &bytes,
                event_services,
                capabilities,
                EventInput {
                    name: due.job.event,
                    payload: due.job.payload,
                },
            )
            .unwrap();
        assert!(queued.ok, "{}", queued.content);
        let queued: Value = serde_json::from_str(&queued.content).unwrap();
        let run = &queued["runs"][0];
        assert_eq!(run["session_id"], session);
        let progress = block_on(store.scheduled_tasks(user, Some(project), 105)).unwrap();
        let history_id = progress[0].last_run.as_ref().unwrap().id;
        assert_eq!(progress[0].last_run.as_ref().unwrap().status, "queued");
        assert_eq!(
            progress[0].last_run.as_ref().unwrap().session_id,
            Some(session)
        );
        assert_eq!(
            block_on(store.list_queued_prompts(user, session))
                .unwrap()
                .len(),
            1
        );
        let mut cancellation = services.clone();
        let cancelled = cancellation
            .request(
                "runs",
                &json!({"action":"cancel","id":run["id"],"revision":run["revision"]}).to_string(),
            )
            .unwrap();
        let cancelled: Value = serde_json::from_str(&cancelled).unwrap();
        cancellation.request("runs",&json!({"action":"delete","id":run["id"],"revision":cancelled["runs"][0]["revision"]}).to_string()).unwrap();
        let retained = block_on(store.scheduled_tasks(user, Some(project), 105)).unwrap();
        assert_eq!(retained[0].last_run.as_ref().unwrap().id, history_id);
        assert_eq!(retained[0].last_run.as_ref().unwrap().status, "cancelled");
        let callback = block_on(store.claim_plugin_jobs("host", 0, &"d".repeat(32), 105))
            .unwrap()
            .jobs
            .remove(0);
        assert_eq!(callback.job.event, "task_completed");
        // A failed callback must remain recoverable after the raw run has already been deleted.
        block_on(store.finish_plugin_job(
            "host",
            callback.job.id,
            &callback.lease,
            false,
            "Actor failed before applying its completion",
            105,
        ))
        .unwrap();
        clock.store(160, Ordering::Relaxed);
        let recovery = block_on(store.claim_plugin_jobs("host", 0, &"7".repeat(32), 160)).unwrap();
        let recovery = recovery
            .jobs
            .iter()
            .find(|job| job.job.event == "reconcile" && job.context.project_id == Some(project))
            .unwrap();
        block_on(store.issue_plugin_job_grant(
            user,
            "host",
            recovery.job.id,
            &recovery.lease,
            &"8".repeat(32),
            160,
        ))
        .unwrap();
        let result = runtime
            .event(
                &bytes,
                Services {
                    grant: "8".repeat(32),
                    ..services.clone()
                },
                capabilities,
                EventInput {
                    name: recovery.job.event.clone(),
                    payload: recovery.job.payload.clone(),
                },
            )
            .unwrap();
        assert!(result.ok, "{}", result.content);
        let mut readback = Services {
            grant: "8".repeat(32),
            ..services.clone()
        };
        let remaining: Value = serde_json::from_str(
            &readback
                .request("jobs", &json!({"action":"list","after":0}).to_string())
                .unwrap(),
        )
        .unwrap();
        assert!(
            remaining["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .all(|job| job["id"] != callback.job.id)
        );
        block_on(store.finish_plugin_job(
            "host",
            recovery.job.id,
            &recovery.lease,
            true,
            "Recovered failed completion",
            160,
        ))
        .unwrap();
        block_on(store.issue_plugin_context_grant(
            user,
            &PluginExecutionContext {
                user_action: true,
                project_id: Some(project),
                session_id: None,
                primary: None,
            },
            &prepared,
            &"6".repeat(32),
            160,
        ))
        .unwrap();
        services.grant = "6".repeat(32);
        assert_eq!(
            block_on(store.scheduled_tasks(user, Some(project), 105)).unwrap()[0]
                .last_run
                .as_ref()
                .unwrap()
                .id,
            history_id
        );
        let data: Value = serde_json::from_str(
            &services
                .clone()
                .request(
                    "collections",
                    &json!({"collection":"tasks","operation":{"action":"read","id":task}})
                        .to_string(),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            data["records"][0]["value"]["state"]["last_result"]["state"],
            "cancelled"
        );
        let revision = data["records"][0]["revision"].clone();
        let removed = runtime
            .execute(
                &bytes,
                services.clone(),
                capabilities,
                "schedule_delete",
                &json!({"id":task,"revision":revision}).to_string(),
            )
            .unwrap();
        assert!(removed.ok, "{}", removed.content);
        assert!(
            block_on(store.scheduled_tasks(user, Some(project), 105))
                .unwrap()
                .is_empty()
        );
        let call = openwebide_core::scheduled::TaskCommand::Monitor {
            session_id: session,
            command: openwebide_core::scheduled::MonitorCommand::Start {
                prompt: "Check deployment".into(),
                delay_seconds: 5,
                interval_seconds: 10,
                max_checks: 1,
            },
        }
        .plugin_call()
        .unwrap();
        let started = runtime
            .execute(
                &bytes,
                services.clone(),
                capabilities,
                &call.name,
                &call.arguments,
            )
            .unwrap();
        assert!(started.ok, "{}", started.content);
        let monitors = block_on(store.scheduled_monitors(user, session, 105)).unwrap();
        assert_eq!(monitors.len(), 1);
        let call = openwebide_core::scheduled::TaskCommand::Monitor {
            session_id: session,
            command: openwebide_core::scheduled::MonitorCommand::Cancel {
                id: monitors[0].id,
                revision: monitors[0].revision,
            },
        }
        .plugin_call()
        .unwrap();
        let cancelled = runtime
            .execute(
                &bytes,
                services.clone(),
                capabilities,
                &call.name,
                &call.arguments,
            )
            .unwrap();
        assert!(cancelled.ok, "{}", cancelled.content);
        assert!(
            block_on(store.scheduled_monitors(user, session, 105))
                .unwrap()
                .is_empty()
        );
        let failed = runtime
            .execute(
                &bytes,
                services.clone(),
                capabilities,
                "schedule_create",
                &json!({"draft":{"title":"Missing model","auto_title":false,"prompt":"Check build",
                "session_target":"new","enabled":true,"schedule":{"kind":"once","at":165}}})
                .to_string(),
            )
            .unwrap();
        assert!(failed.ok, "{}", failed.content);
        let created: Value = serde_json::from_str(&failed.content).unwrap();
        let failed_task = created[0]["id"].as_i64().unwrap();
        for server in block_on(store.list_connections()).unwrap() {
            block_on(store.delete_connection(server.id)).unwrap();
        }
        clock.store(165, Ordering::Relaxed);
        let delivery = block_on(store.claim_plugin_jobs("host", 0, &"4".repeat(32), 165)).unwrap();
        let delivery = delivery
            .jobs
            .iter()
            .find(|job| job.job.event == "task_due" && job.job.payload["task_id"] == failed_task)
            .unwrap();
        block_on(store.issue_plugin_job_grant(
            user,
            "host",
            delivery.job.id,
            &delivery.lease,
            &"5".repeat(32),
            165,
        ))
        .unwrap();
        let failure_services = Services {
            grant: "5".repeat(32),
            ..services.clone()
        };
        let failed = runtime
            .event(
                &bytes,
                failure_services.clone(),
                capabilities,
                EventInput {
                    name: delivery.job.event.clone(),
                    payload: delivery.job.payload.clone(),
                },
            )
            .unwrap();
        assert!(failed.ok, "{}", failed.content);
        let mut readback = failure_services;
        let history: Value = serde_json::from_str(
            &readback
                .request(
                    "collections",
                    &json!({"collection":"task_runs","operation":{"action":"list"}}).to_string(),
                )
                .unwrap(),
        )
        .unwrap();
        let history = history["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["value"]["task_id"] == failed_task)
            .unwrap();
        assert_eq!(history["value"]["snapshot"]["status"], "failed");
        assert!(history["value"]["run_id"].is_null());
        let tasks = block_on(store.scheduled_tasks(user, Some(project), 165)).unwrap();
        let failed_task = tasks.iter().find(|task| task.id == failed_task).unwrap();
        assert!(failed_task.next_run.is_none());
        assert!(failed_task.last_run.is_some());
        block_on(store.finish_plugin_job(
            "host",
            delivery.job.id,
            &delivery.lease,
            true,
            "Source recorded preflight failure",
            165,
        ))
        .unwrap();
        // Seed a completed retention scan, then let the real background actor prune
        // the two newly completed entries while preserving the task's latest failure.
        let mut seed = services.clone();
        let raw: Value = serde_json::from_str(&seed.request("runs",
            &json!({"action":"submit","key":format!("task:{}:1:100", failed_task.id),
                "prompt":"Old accepted occurrence","target":{"kind":"new","title":"Old occurrence"}}).to_string()).unwrap()).unwrap();
        let raw = &raw["runs"][0];
        let raw_id = raw["id"].as_i64().unwrap();
        seed.request(
            "runs",
            &json!({"action":"cancel","id":raw_id,"revision":raw["revision"]}).to_string(),
        )
        .unwrap();
        let mut retained = Vec::new();
        for index in 0..130 {
            let value: Value = serde_json::from_str(&seed.request("collections",
                &json!({"collection":"task_runs","operation":{"action":"create","value":{
                    "key":format!("retention:{index}"),"task_id":failed_task.id,"due_at":100,
                    "snapshot":{"status":"complete","detail":"Old completed occurrence"}
                }}}).to_string()).unwrap()).unwrap();
            retained.push(value["records"][0]["id"].as_i64().unwrap());
        }
        let recovery: Value = serde_json::from_str(
            &seed
                .request(
                    "records",
                    &json!({"collection":"recovery","operation":{"action":"list"}}).to_string(),
                )
                .unwrap(),
        )
        .unwrap();
        let recovery = &recovery["records"][0];
        seed.request(
            "records",
            &json!({"collection":"recovery","operation":{"action":"update",
                "id":recovery["id"],"revision":recovery["revision"],
                "value":{"after":0,"history":{"after":retained[127],"recent":retained[..128]}}
            }})
            .to_string(),
        )
        .unwrap();
        clock.store(220, Ordering::Relaxed);
        let delivery = block_on(store.claim_plugin_jobs("host", 0, &"9".repeat(32), 220)).unwrap();
        let delivery = delivery
            .jobs
            .iter()
            .find(|job| job.job.event == "reconcile" && job.context.project_id == Some(project))
            .unwrap();
        block_on(store.issue_plugin_job_grant(
            user,
            "host",
            delivery.job.id,
            &delivery.lease,
            &"0".repeat(32),
            220,
        ))
        .unwrap();
        let retention_services = Services {
            grant: "0".repeat(32),
            ..services.clone()
        };
        let result = runtime
            .event(
                &bytes,
                retention_services.clone(),
                capabilities,
                EventInput {
                    name: delivery.job.event.clone(),
                    payload: delivery.job.payload.clone(),
                },
            )
            .unwrap();
        assert!(result.ok, "{}", result.content);
        let mut readback = retention_services;
        let remaining: Value = serde_json::from_str(
            &readback
                .request(
                    "collections",
                    &json!({"collection":"task_runs","operation":{"action":"list"}}).to_string(),
                )
                .unwrap(),
        )
        .unwrap();
        assert!(
            remaining["records"]
                .as_array()
                .unwrap()
                .iter()
                .all(|record| record["id"] != retained[0] && record["id"] != retained[1])
        );
        assert!(
            remaining["records"]
                .as_array()
                .unwrap()
                .iter()
                .any(|record| record["id"] == history["id"])
        );
        let newest: Value = serde_json::from_str(&readback.request("collections",
            &json!({"collection":"task_runs","operation":{"action":"read","id":retained[129]}}).to_string()).unwrap()).unwrap();
        assert_eq!(newest["records"][0]["value"]["key"], "retention:129");
        let runs: Value = serde_json::from_str(
            &readback
                .request("runs", &json!({"action":"list"}).to_string())
                .unwrap(),
        )
        .unwrap();
        assert!(
            runs["runs"]
                .as_array()
                .unwrap()
                .iter()
                .all(|run| run["id"] != raw_id)
        );
        let saved: Value = serde_json::from_str(&readback.request("collections",
            &json!({"collection":"task_runs","operation":{"action":"list","after":retained[129]}}).to_string()).unwrap()).unwrap();
        let saved = saved["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["value"]["run_id"] == raw_id)
            .unwrap();
        assert_eq!(saved["value"]["snapshot"]["status"], "cancelled");
        let jobs: Value = serde_json::from_str(
            &readback
                .request("jobs", &json!({"action":"list"}).to_string())
                .unwrap(),
        )
        .unwrap();
        assert!(jobs["jobs"].as_array().unwrap().iter().all(|job| {
            job["key"]
                .as_str()
                .is_none_or(|key| !key.starts_with(&format!("task:{task}:")))
        }));
        block_on(store.finish_plugin_job(
            "host",
            delivery.job.id,
            &delivery.lease,
            true,
            "Pruned old history through public SDK",
            220,
        ))
        .unwrap();
        assert_eq!(prepared.manifest.compatibility.plugin_api, 3);
    }
}
