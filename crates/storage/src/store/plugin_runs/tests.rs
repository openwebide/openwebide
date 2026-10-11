use super::*;
use crate::rusqlite_db::RusqliteDb;
use futures::executor::block_on;
use openwebide_core::plugins::{
    PluginPackage, RecordPlugin, RustPlugin, execution::PluginHostRequest,
};
use serde_json::json;

struct Fixture {
    store: Store<RusqliteDb>,
    user: UserId,
    other: UserId,
    project: i64,
    session: i64,
    plugin: PreparedPlugin,
}
impl Fixture {
    fn completion_submission() -> RunRequest {
        RunRequest::Submit {
            prerequisites: Vec::new(),
            completion: Some(RunCompletion {
                event: "due".into(),
                payload: json!({"task":"my-task"}),
            }),
            key: "completion".into(),
            prompt: "Plugin-owned prompt".into(),
            target: RunTarget::Origin,
            model: None,
        }
    }
    async fn jobs(&self) -> openwebide_core::plugins::jobs::JobResult {
        let value = self
            .store
            .plugin_host_request(
                self.user,
                self.session,
                &PluginHostRequest {
                    grant: "a".repeat(32),
                    capability: "jobs".into(),
                    payload: json!({"action":"list"}).to_string(),
                },
                2,
            )
            .await
            .unwrap();
        serde_json::from_str(&value).unwrap()
    }
    async fn bind_host(&self, mode: WorkspaceMode) {
        if mode == WorkspaceMode::Local {
            self.store
                .set_user_setting(
                    self.user,
                    &format!("scheduled_host_{}", self.project),
                    &encode(&openwebide_core::scheduled::HostBinding {
                        host_id: self.plugin.host_id.clone(),
                        path: "repos/local".into(),
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
        }
    }
    async fn new(mode: WorkspaceMode) -> Self {
        let store = Store::new(RusqliteDb::open_in_memory().unwrap());
        store.migrate().await.unwrap();
        let user = store
            .insert_user("owner", "hash", UserRole::Admin, 0)
            .await
            .unwrap()
            .id;
        let other = store
            .insert_user("other", "hash", UserRole::User, 0)
            .await
            .unwrap()
            .id;
        let project = store
            .create_project(
                &NewProject {
                    name: "project".into(),
                    mode,
                    path: Some("p".into()),
                },
                user,
                0,
            )
            .await
            .unwrap()
            .id;
        let session = store
            .create_session("origin", None, None, Some(project), user, 0)
            .await
            .unwrap()
            .id;
        let mut plugin = openwebide_core::plugins::testing::receipt();
        plugin.manifest.compatibility.plugin_api = 3;
        plugin.manifest.contributions.skills.clear();
        plugin.manifest.contributions.events = vec!["due".into()];
        plugin.manifest.executable = Some(RustPlugin {
            manifest: "Cargo.toml".into(),
            library: "community_runs".into(),
            sdk_version: "0.1.0".into(),
            capabilities: vec!["runs".into(), "jobs".into(), "collections".into()],
        });
        store
            .record_plugin(
                user,
                &RecordPlugin {
                    prepared: plugin.clone(),
                    revision: None,
                    approved_capabilities: vec![],
                    update_policy: None,
                    package: Some(Box::new(PluginPackage {
                        prepared: plugin.clone(),
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
                    project_id: Some(project),
                    session_id: Some(session),
                    ..Default::default()
                },
                &plugin,
                &"a".repeat(32),
                1,
            )
            .await
            .unwrap();
        Self {
            store,
            user,
            other,
            project,
            session,
            plugin,
        }
    }
    async fn request(&self, command: &RunRequest) -> Result<RunResult, StorageError> {
        let result = self
            .store
            .plugin_host_request(
                self.user,
                self.session,
                &PluginHostRequest {
                    grant: "a".repeat(32),
                    capability: "runs".into(),
                    payload: encode(command).unwrap(),
                },
                2,
            )
            .await?;
        serde_json::from_str(&result).map_err(|error| StorageError::Db(error.to_string()))
    }
    fn submission(key: &str) -> RunRequest {
        RunRequest::Submit {
            prerequisites: Vec::new(),
            key: key.into(),
            prompt: "Plugin-owned prompt".into(),
            target: RunTarget::Origin,
            model: None,
            completion: None,
        }
    }
}

#[test]
fn completion_events_are_reserved_atomic_pinned_and_delivered_once_after_terminal_status() {
    use openwebide_core::plugins::jobs::JobState;
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let f = Fixture::new(mode).await;
            f.bind_host(mode).await;
            let submitted = f
                .request(&Fixture::completion_submission())
                .await
                .unwrap()
                .runs
                .remove(0);
            assert_eq!(
                f.request(&Fixture::completion_submission())
                    .await
                    .unwrap()
                    .runs[0]
                    .id,
                submitted.id
            );
            let reserved = f.jobs().await;
            assert_eq!(reserved.jobs.len(), 1);
            assert_eq!(reserved.jobs[0].state, JobState::Waiting);
            // Install another version before completion; the event retains its original code.
            let mut newer = f.plugin.clone();
            newer.source.commit = "c".repeat(40);
            newer.digest = "d".repeat(64);
            newer.manifest.version = "0.2.0".into();
            let revision = f.store.plugin_installations(f.user).await.unwrap()[0].revision;
            f.store
                .record_plugin(
                    f.user,
                    &RecordPlugin {
                        prepared: newer.clone(),
                        revision: Some(revision),
                        approved_capabilities: vec![],
                        update_policy: None,
                        package: Some(Box::new(PluginPackage {
                            prepared: newer,
                            skills: vec![],
                        })),
                    },
                    3,
                )
                .await
                .unwrap();
            assert!(
                f.store
                    .claim_plugin_jobs(&f.plugin.host_id, 0, &"c".repeat(32), 3)
                    .await
                    .unwrap()
                    .jobs
                    .is_empty()
            );
            let run = f
                .store
                .claim_plugin_runs(&f.plugin.host_id, 0, &"b".repeat(32), 3)
                .await
                .unwrap()
                .runs
                .remove(0);
            f.store
                .session_run_lease(f.user, f.session, &run.run_id(), false, 4)
                .await
                .unwrap();
            f.store
                .consume_queued_prompt(f.user, f.session, run.prompt.key(), &run.prompt.content, 4)
                .await
                .unwrap();
            let report = RunReport {
                state: RunState::Completed,
                detail: "raw assistant output".into(),
                permission_id: None,
            };
            f.store
                .report_plugin_run(&run.lease, &report, 5)
                .await
                .unwrap();
            f.store
                .report_plugin_run(&run.lease, &report, 6)
                .await
                .unwrap();
            let ready = f.jobs().await;
            assert_eq!(ready.jobs.len(), 1);
            assert_eq!(ready.jobs[0].state, JobState::Pending);
            assert_eq!(ready.jobs[0].revision, 2);
            assert_eq!(ready.jobs[0].payload["data"], json!({"task":"my-task"}));
            let result: PluginRun =
                serde_json::from_value(ready.jobs[0].payload["run"].clone()).unwrap();
            assert_eq!(result.id, submitted.id);
            assert_eq!(result.state, RunState::Completed);
            assert_eq!(result.detail, "raw assistant output");
            assert!(result.message_id.is_some());
            // History cleanup cannot discard an already released callback.
            f.request(&RunRequest::Delete {
                id: result.id,
                revision: result.revision,
            })
            .await
            .unwrap();
            let event = f
                .store
                .claim_plugin_jobs(&f.plugin.host_id, 0, &"c".repeat(32), 7)
                .await
                .unwrap()
                .jobs
                .remove(0);
            assert_eq!(event.prepared.source, f.plugin.source);
            assert_eq!(event.prepared.digest, f.plugin.digest);
            assert_eq!(event.context.session_id, Some(f.session));
            assert!(
                f.store
                    .claim_plugin_jobs(&f.plugin.host_id, 0, &"d".repeat(32), 8)
                    .await
                    .unwrap()
                    .jobs
                    .is_empty()
            );
            let (prepared, context) = f
                .store
                .issue_plugin_job_grant(
                    f.user,
                    &f.plugin.host_id,
                    event.job.id,
                    &event.lease,
                    &"e".repeat(32),
                    8,
                )
                .await
                .unwrap();
            assert_eq!(prepared.source, f.plugin.source);
            assert_eq!(context.session_id, None);
            assert!(!context.user_action);
        }
    });
}

#[test]
fn completion_migration_preserves_prior_run_history_and_replays_idempotently() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let f = Fixture::new(mode).await;
            let original = f
                .request(&Fixture::submission("old-schema"))
                .await
                .unwrap()
                .runs
                .remove(0);
            f.store
                .db
                .execute("DROP TRIGGER complete_plugin_run_event", &[])
                .await
                .unwrap();
            f.store
                .db
                .execute("DROP TRIGGER delete_plugin_run_event", &[])
                .await
                .unwrap();
            f.store
                .db
                .execute("ALTER TABLE plugin_runs DROP COLUMN completion_job", &[])
                .await
                .unwrap();
            f.store
                .db
                .execute("PRAGMA user_version=51", &[])
                .await
                .unwrap();
            f.store.migrate().await.unwrap();
            let preserved = f
                .request(&RunRequest::Read { id: original.id })
                .await
                .unwrap();
            assert_eq!(preserved.runs[0].key, "old-schema");
            assert_eq!(preserved.runs[0].state, RunState::Pending);
            assert_eq!(
                f.store
                    .list_queued_prompts(f.user, f.session)
                    .await
                    .unwrap()
                    .len(),
                1
            );
            f.store
                .db
                .execute("PRAGMA user_version=51", &[])
                .await
                .unwrap();
            f.store.migrate().await.unwrap();
            f.request(&Fixture::completion_submission()).await.unwrap();
            assert_eq!(f.jobs().await.jobs.len(), 1);
        }
    });
}

#[test]
fn completion_events_cover_queue_cancellation_failure_and_interrupted_recovery() {
    use openwebide_core::plugins::jobs::JobState;
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            for outcome in [
                "queue-cancel",
                "failure",
                "interrupted",
                "conversation-removed",
            ] {
                let f = Fixture::new(mode).await;
                f.bind_host(mode).await;
                f.request(&Fixture::completion_submission()).await.unwrap();
                let expected = match outcome {
                    "queue-cancel" => {
                        let queued = f
                            .store
                            .list_queued_prompts(f.user, f.session)
                            .await
                            .unwrap()
                            .remove(0);
                        f.store
                            .remove_queued_prompt(f.user, f.session, queued.key())
                            .await
                            .unwrap();
                        RunState::Cancelled
                    }
                    "conversation-removed" => {
                        f.store.delete_session(f.session, f.user).await.unwrap();
                        RunState::Cancelled
                    }
                    _ => {
                        let run = f
                            .store
                            .claim_plugin_runs(&f.plugin.host_id, 0, &"b".repeat(32), 3)
                            .await
                            .unwrap()
                            .runs
                            .remove(0);
                        if outcome == "failure" {
                            f.store
                                .report_plugin_run(
                                    &run.lease,
                                    &RunReport {
                                        state: RunState::Failed,
                                        detail: "Preflight failed".into(),
                                        permission_id: None,
                                    },
                                    4,
                                )
                                .await
                                .unwrap();
                            RunState::Failed
                        } else {
                            f.store
                                .session_run_lease(f.user, f.session, &run.run_id(), false, 4)
                                .await
                                .unwrap();
                            f.store
                                .consume_queued_prompt(
                                    f.user,
                                    f.session,
                                    run.prompt.key(),
                                    &run.prompt.content,
                                    4,
                                )
                                .await
                                .unwrap();
                            assert!(
                                f.store
                                    .claim_plugin_runs(&f.plugin.host_id, 0, &"c".repeat(32), 124)
                                    .await
                                    .unwrap()
                                    .runs
                                    .is_empty()
                            );
                            RunState::Interrupted
                        }
                    }
                };
                let rows = f
                    .store
                    .db
                    .execute("SELECT state,payload FROM plugin_jobs", &[])
                    .await
                    .unwrap();
                assert_eq!(rows.rows.len(), 1);
                let state: JobState =
                    serde_json::from_value(json!(rows.rows[0].get_text(0).unwrap())).unwrap();
                assert_eq!(state, JobState::Pending);
                let payload: serde_json::Value =
                    serde_json::from_str(rows.rows[0].get_text(1).unwrap()).unwrap();
                let result: PluginRun = serde_json::from_value(payload["run"].clone()).unwrap();
                assert_eq!(result.state, expected);
                if outcome == "conversation-removed" {
                    assert!(
                        f.store
                            .claim_plugin_jobs(&f.plugin.host_id, 0, &"d".repeat(32), 125)
                            .await
                            .unwrap()
                            .jobs
                            .is_empty()
                    );
                }
            }
        }
    });
}

#[test]
fn completion_capacity_and_event_validation_roll_back_submission_and_cancel_can_suppress_callback()
{
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let f = Fixture::new(mode).await;
            let mut invalid = Fixture::completion_submission();
            if let RunRequest::Submit {
                completion: Some(completion),
                ..
            } = &mut invalid
            {
                completion.event = "undeclared".into();
            }
            assert!(f.request(&invalid).await.is_err());
            assert!(
                f.store
                    .list_queued_prompts(f.user, f.session)
                    .await
                    .unwrap()
                    .is_empty()
            );
            f.request(&Fixture::completion_submission()).await.unwrap();
            let job = f.jobs().await.jobs.remove(0);
            f.store
                .plugin_host_request(
                    f.user,
                    f.session,
                    &PluginHostRequest {
                        grant: "a".repeat(32),
                        capability: "jobs".into(),
                        payload: json!({"action":"cancel","id":job.id,"revision":job.revision})
                            .to_string(),
                    },
                    3,
                )
                .await
                .unwrap();
            let queued = f
                .store
                .list_queued_prompts(f.user, f.session)
                .await
                .unwrap()
                .remove(0);
            f.store
                .remove_queued_prompt(f.user, f.session, queued.key())
                .await
                .unwrap();
            assert_eq!(
                f.jobs().await.jobs[0].state,
                openwebide_core::plugins::jobs::JobState::Cancelled
            );
            let f = Fixture::new(mode).await;
            f.store.db.execute("WITH RECURSIVE entries(id) AS (SELECT 1 UNION ALL SELECT id+1 FROM entries WHERE id<1000) INSERT INTO plugin_jobs(user_id,project_scope,plugin,job_key,due_at,event,payload,prepared,context,host_id) SELECT ?,?,?,CAST(id AS TEXT),0,'due','{}',?,?,? FROM entries", &[
                DbValue::Int(f.user.get()),DbValue::Int(f.project),DbValue::Text(f.plugin.storage_namespace()),
                DbValue::Text(encode(&f.plugin).unwrap()),DbValue::Text(encode(&PluginExecutionContext {project_id:Some(f.project),session_id:Some(f.session),..Default::default()}).unwrap()),DbValue::Text(f.plugin.host_id.clone()),
            ]).await.unwrap();
            assert!(f.request(&Fixture::completion_submission()).await.is_err());
            assert!(
                f.store
                    .list_queued_prompts(f.user, f.session)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(
                f.request(&RunRequest::List { after: 0 })
                    .await
                    .unwrap()
                    .runs
                    .is_empty()
            );
        }
    });
}
#[test]
fn raw_submissions_are_owned_idempotent_cancellable_and_queue_atomic_in_both_modes() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let f = Fixture::new(mode).await;
            let first = f
                .request(&Fixture::submission("one"))
                .await
                .unwrap()
                .runs
                .remove(0);
            assert_eq!(first.session_id, Some(f.session));
            assert_eq!(first.state, RunState::Pending);
            assert_eq!(
                f.request(&Fixture::submission("one")).await.unwrap().runs[0].id,
                first.id
            );
            let mut changed = Fixture::submission("one");
            if let RunRequest::Submit { prompt, .. } = &mut changed {
                *prompt = "changed".into();
            }
            assert!(f.request(&changed).await.is_err());
            let queue = f
                .store
                .list_queued_prompts(f.user, f.session)
                .await
                .unwrap();
            assert_eq!(queue.len(), 1);
            assert!(
                f.store
                    .update_queued_prompt(f.user, f.session, queue[0].key(), "changed")
                    .await
                    .is_err()
            );
            assert!(
                f.store
                    .consume_queued_prompt(f.user, f.session, queue[0].key(), &queue[0].content, 3)
                    .await
                    .is_err()
            );
            assert!(
                f.request(&RunRequest::Delete {
                    id: first.id,
                    revision: first.revision
                })
                .await
                .is_err()
            );
            assert!(
                f.request(&RunRequest::Cancel {
                    id: first.id,
                    revision: 99
                })
                .await
                .is_err()
            );
            let cancelled = f
                .request(&RunRequest::Cancel {
                    id: first.id,
                    revision: first.revision,
                })
                .await
                .unwrap()
                .runs
                .remove(0);
            assert_eq!(cancelled.state, RunState::Cancelled);
            assert_eq!(cancelled.revision, 2);
            assert!(
                f.store
                    .list_queued_prompts(f.user, f.session)
                    .await
                    .unwrap()
                    .is_empty()
            );
            f.request(&RunRequest::Delete {
                id: first.id,
                revision: cancelled.revision,
            })
            .await
            .unwrap();
            assert!(f.request(&RunRequest::Read { id: first.id }).await.is_err());
            let owned = f
                .store
                .create_session("other", None, None, None, f.other, 0)
                .await
                .unwrap()
                .id;
            let other_project = f
                .store
                .create_project(
                    &NewProject {
                        name: "sibling".into(),
                        mode,
                        path: Some("sibling".into()),
                    },
                    f.user,
                    0,
                )
                .await
                .unwrap()
                .id;
            let sibling = f
                .store
                .create_session("sibling", None, None, Some(other_project), f.user, 0)
                .await
                .unwrap()
                .id;
            for session in [owned, sibling] {
                let request = RunRequest::Submit {
                    prerequisites: Vec::new(),
                    key: format!("cross:{session}"),
                    prompt: "x".into(),
                    target: RunTarget::Session { id: session },
                    model: None,
                    completion: None,
                };
                assert!(f.request(&request).await.is_err());
            }
            assert!(
                f.request(&RunRequest::List { after: 0 })
                    .await
                    .unwrap()
                    .runs
                    .is_empty()
            );
        }
    });
}
#[test]
fn queue_limits_roll_back_new_conversations_and_manual_removal_cancels_submissions() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let f = Fixture::new(mode).await;
            for index in 0..8 {
                f.store
                    .enqueue_prompt(f.user, f.session, &format!("manual:{index}"), 0)
                    .await
                    .unwrap();
            }
            assert!(f.request(&Fixture::submission("full")).await.is_err());
            assert!(
                f.request(&RunRequest::List { after: 0 })
                    .await
                    .unwrap()
                    .runs
                    .is_empty()
            );
            let new = RunRequest::Submit {
                prerequisites: Vec::new(),
                key: "new".into(),
                prompt: "new conversation".into(),
                target: RunTarget::New {
                    title: "Plugin supplied title".into(),
                },
                model: None,
                completion: None,
            };
            let run = f.request(&new).await.unwrap().runs.remove(0);
            let session = run.session_id.unwrap();
            assert_eq!(
                f.store.get_session(session, f.user).await.unwrap().name,
                "Plugin supplied title"
            );
            assert_eq!(
                f.request(&new).await.unwrap().runs[0].session_id,
                Some(session)
            );
            let queued = f
                .store
                .list_queued_prompts(f.user, session)
                .await
                .unwrap()
                .remove(0);
            f.store
                .remove_queued_prompt(f.user, session, queued.key())
                .await
                .unwrap();
            assert_eq!(
                f.request(&RunRequest::Read { id: run.id })
                    .await
                    .unwrap()
                    .runs[0]
                    .state,
                RunState::Cancelled
            );
            let second = f
                .request(&RunRequest::Submit {
                    prerequisites: Vec::new(),
                    key: "deleted".into(),
                    prompt: "x".into(),
                    target: RunTarget::New {
                        title: "temporary".into(),
                    },
                    model: None,
                    completion: None,
                })
                .await
                .unwrap()
                .runs
                .remove(0);
            f.store
                .delete_session(second.session_id.unwrap(), f.user)
                .await
                .unwrap();
            let state = f
                .request(&RunRequest::Read { id: second.id })
                .await
                .unwrap()
                .runs
                .remove(0);
            assert_eq!(state.state, RunState::Cancelled);
            assert!(state.session_id.is_none());
        }
    });
}
#[test]
fn consumed_submissions_are_never_reinjected_and_cancel_only_their_own_active_run() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let f = Fixture::new(mode).await;
            let first = f
                .request(&Fixture::submission("one"))
                .await
                .unwrap()
                .runs
                .remove(0);
            let queued = f
                .store
                .list_queued_prompts(f.user, f.session)
                .await
                .unwrap()
                .remove(0);
            f.store.db.execute("UPDATE plugin_runs SET state='leased',lease='opaque',lease_expires_at=120 WHERE id=?", &[DbValue::Int(first.id)]).await.unwrap();
            f.store
                .session_run_lease(
                    f.user,
                    f.session,
                    &format!("plugin-run-{}-opaque", first.id),
                    false,
                    0,
                )
                .await
                .unwrap();
            let message = f
                .store
                .consume_queued_prompt(f.user, f.session, queued.key(), &queued.content, 3)
                .await
                .unwrap();
            let running = f
                .request(&RunRequest::Read { id: first.id })
                .await
                .unwrap()
                .runs
                .remove(0);
            assert_eq!(running.state, RunState::Running);
            assert_eq!(running.message_id, Some(message.id));
            assert!(
                f.store
                    .consume_queued_prompt(f.user, f.session, queued.key(), &queued.content, 4)
                    .await
                    .is_err()
            );
            assert_eq!(
                f.request(&Fixture::submission("one")).await.unwrap().runs[0].id,
                first.id
            );
            assert!(
                f.store
                    .list_queued_prompts(f.user, f.session)
                    .await
                    .unwrap()
                    .is_empty()
            );
            let cancelled = f
                .request(&RunRequest::Cancel {
                    id: first.id,
                    revision: running.revision,
                })
                .await
                .unwrap()
                .runs
                .remove(0);
            assert_eq!(cancelled.state, RunState::Cancelling);
            assert!(f.store.cancel_requested_since(f.session, 0).await.unwrap());
        }
    });
}
#[test]
fn sessionless_event_grants_retain_origin_for_raw_submissions() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let f = Fixture::new(mode).await;
            let response=f.store.plugin_host_request(f.user,f.session,&PluginHostRequest{grant:"a".repeat(32),capability:"jobs".into(),payload:json!({"action":"schedule","key":"job","due_at":20,"event":"due","payload":null}).to_string()},2).await.unwrap();
            assert!(!response.is_empty());
            let delivery = f
                .store
                .claim_plugin_jobs(&f.plugin.host_id, 0, &"b".repeat(32), 20)
                .await
                .unwrap()
                .jobs
                .remove(0);
            let (plugin, context) = f
                .store
                .issue_plugin_job_grant(
                    f.user,
                    &f.plugin.host_id,
                    delivery.job.id,
                    &delivery.lease,
                    &"c".repeat(32),
                    21,
                )
                .await
                .unwrap();
            assert_eq!(plugin, f.plugin);
            assert!(context.session_id.is_none());
            let request = PluginHostRequest {
                grant: "c".repeat(32),
                capability: "runs".into(),
                payload: encode(&Fixture::submission("background")).unwrap(),
            };
            let result = f
                .store
                .plugin_context_host_request(f.user, &request, 21)
                .await
                .unwrap();
            let result: RunResult = serde_json::from_str(&result).unwrap();
            assert_eq!(result.runs[0].session_id, Some(f.session));
            f.store
                .finish_plugin_job(
                    &f.plugin.host_id,
                    delivery.job.id,
                    &delivery.lease,
                    true,
                    "done",
                    22,
                )
                .await
                .unwrap();
            assert!(
                f.store
                    .plugin_context_host_request(f.user, &request, 23)
                    .await
                    .is_err()
            );
        }
    });
}

#[test]
fn run_models_are_pinned_without_mutating_conversation_and_project_deletion_cleans_queue() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let f = Fixture::new(mode).await;
            let connection = f
                .store
                .insert_connection(&NewConnection {
                    name: "primary".into(),
                    kind: ProviderKind::Ollama,
                    base_url: "http://model.test".into(),
                    model: Some("default".into()),
                    context_limit: None,
                })
                .await
                .unwrap();
            let selected = ModelSelection {
                server_id: connection.id,
                model: "override".into(),
            };
            let command = RunRequest::Submit {
                prerequisites: Vec::new(),
                key: "pinned".into(),
                prompt: "x".into(),
                target: RunTarget::Origin,
                model: Some(selected.clone()),
                completion: Some(RunCompletion {
                    event: "due".into(),
                    payload: json!({}),
                }),
            };
            let run = f.request(&command).await.unwrap().runs.remove(0);
            let callback = f
                .store
                .db
                .execute("SELECT context FROM plugin_jobs", &[])
                .await
                .unwrap();
            let callback: PluginExecutionContext =
                serde_json::from_str(callback.rows[0].get_text(0).unwrap()).unwrap();
            assert_eq!(callback.primary, Some(selected.clone()));
            assert_eq!(callback.session_id, Some(f.session));
            let queued = f
                .store
                .list_queued_prompts(f.user, f.session)
                .await
                .unwrap()
                .remove(0);
            assert_eq!(queued.plugin_run, Some(run.id));
            assert!(queued.is_host_delivered());
            assert_eq!(
                f.store
                    .plugin_prompt_model(f.user, f.session, queued.key())
                    .await
                    .unwrap(),
                Some(selected)
            );
            assert!(
                f.store
                    .plugin_prompt_model(f.other, f.session, queued.key())
                    .await
                    .is_err()
            );
            assert!(
                f.store
                    .get_session(f.session, f.user)
                    .await
                    .unwrap()
                    .connection_id
                    .is_none()
            );
            assert_eq!(
                f.store
                    .get_session(f.session, f.user)
                    .await
                    .unwrap()
                    .project_id,
                Some(f.project)
            );
            f.store.delete_project(f.project, f.user).await.unwrap();
            assert!(
                f.store
                    .db
                    .execute("SELECT 1 FROM plugin_runs", &[])
                    .await
                    .unwrap()
                    .rows
                    .is_empty()
            );
            assert!(
                f.store
                    .db
                    .execute("SELECT 1 FROM queued_prompts", &[])
                    .await
                    .unwrap()
                    .rows
                    .is_empty()
            );
        }
    });
}
#[test]
fn run_history_is_paged_bounded_replay_safe_and_released_by_terminal_deletion() {
    block_on(async {
        let f = Fixture::new(WorkspaceMode::Remote).await;
        let first = f
            .request(&Fixture::submission("one"))
            .await
            .unwrap()
            .runs
            .remove(0);
        f.request(&RunRequest::Cancel {
            id: first.id,
            revision: first.revision,
        })
        .await
        .unwrap();
        f.store.db.execute("WITH RECURSIVE sequence(value) AS (SELECT 1 UNION ALL SELECT value+1 FROM sequence WHERE value<999) INSERT INTO plugin_runs(user_id,project_scope,plugin,run_key,request,prepared,context,host_id,session_id,state,created_at) SELECT r.user_id,r.project_scope,r.plugin,'history:'||sequence.value,r.request,r.prepared,r.context,r.host_id,r.session_id,'completed',r.created_at FROM plugin_runs r,sequence WHERE r.id=?", &[DbValue::Int(first.id)]).await.unwrap();
        assert!(f.request(&Fixture::submission("full")).await.is_err());
        assert_eq!(
            f.request(&Fixture::submission("one")).await.unwrap().runs[0].id,
            first.id
        );
        let mut after = 0;
        let mut count = 0;
        loop {
            let page = f.request(&RunRequest::List { after }).await.unwrap();
            assert!(page.runs.len() <= 16);
            count += page.runs.len();
            if let Some(next) = page.next_after {
                assert!(next > after);
                after = next;
            } else {
                break;
            }
        }
        assert_eq!(count, 1000);
        f.request(&RunRequest::Delete {
            id: first.id,
            revision: 2,
        })
        .await
        .unwrap();
        assert!(f.request(&Fixture::submission("new")).await.is_ok());
        f.store
            .db
            .execute("PRAGMA user_version=50", &[])
            .await
            .unwrap();
        f.store.migrate().await.unwrap();
        assert_eq!(
            f.store
                .db
                .execute("SELECT count(*) FROM plugin_runs", &[])
                .await
                .unwrap()
                .rows[0]
                .get_int(0)
                .unwrap(),
            1000
        );
    });
}

#[test]
fn projectless_runs_keep_a_separate_owned_namespace() {
    block_on(async {
        let f = Fixture::new(WorkspaceMode::Remote).await;
        let session = f
            .store
            .create_session("global", None, None, None, f.user, 0)
            .await
            .unwrap()
            .id;
        f.store
            .issue_plugin_context_grant(
                f.user,
                &PluginExecutionContext {
                    session_id: Some(session),
                    ..Default::default()
                },
                &f.plugin,
                &"d".repeat(32),
                1,
            )
            .await
            .unwrap();
        let request = PluginHostRequest {
            grant: "d".repeat(32),
            capability: "runs".into(),
            payload: encode(&Fixture::submission("global")).unwrap(),
        };
        let result = f
            .store
            .plugin_host_request(f.user, session, &request, 2)
            .await
            .unwrap();
        let result: RunResult = serde_json::from_str(&result).unwrap();
        assert_eq!(result.runs[0].session_id, Some(session));
        assert!(
            f.request(&RunRequest::Read {
                id: result.runs[0].id
            })
            .await
            .is_err()
        );
        assert!(
            f.store
                .plugin_host_request(f.other, session, &request, 2)
                .await
                .is_err()
        );
        assert!(
            f.store
                .plugin_context_host_request(f.user, &request, 2)
                .await
                .is_err()
        );
    });
}

#[test]
fn run_claims_are_host_bound_and_recover_only_unconsumed_prompts_in_both_modes() {
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let f = Fixture::new(mode).await;
            if mode == WorkspaceMode::Local {
                f.store
                    .bind_background_host(
                        f.user,
                        f.project,
                        &openwebide_core::scheduled::HostBinding::new(
                            f.plugin.host_id.clone(),
                            String::new(),
                        ),
                    )
                    .await
                    .unwrap();
            }
            let submitted = f
                .request(&Fixture::submission("one"))
                .await
                .unwrap()
                .runs
                .remove(0);
            let original = f
                .store
                .list_queued_prompts(f.user, f.session)
                .await
                .unwrap()
                .remove(0);
            assert!(
                f.store
                    .claim_plugin_runs("other-host", 0, &"b".repeat(32), 3)
                    .await
                    .unwrap()
                    .runs
                    .is_empty()
            );
            let first = f
                .store
                .claim_plugin_runs(&f.plugin.host_id, 0, &"b".repeat(32), 3)
                .await
                .unwrap()
                .runs
                .remove(0);
            assert_eq!(first.run.id, submitted.id);
            assert_eq!(first.prompt.revision, original.revision + 1);
            assert_eq!(
                first.host_path.as_deref(),
                (mode == WorkspaceMode::Local).then_some("."),
            );
            assert!(
                f.store
                    .claim_plugin_runs(&f.plugin.host_id, 0, &"c".repeat(32), 4)
                    .await
                    .unwrap()
                    .runs
                    .is_empty()
            );
            f.store.renew_plugin_run(&first.lease, 4).await.unwrap();
            let recovered = f
                .store
                .claim_plugin_runs(&f.plugin.host_id, 0, &"d".repeat(32), 124)
                .await
                .unwrap()
                .runs
                .remove(0);
            assert_ne!(first.run_id(), recovered.run_id());
            assert_eq!(recovered.prompt.revision, first.prompt.revision + 1);
            assert!(f.store.renew_plugin_run(&first.lease, 125).await.is_err());
            f.store
                .session_run_lease(f.user, f.session, &recovered.run_id(), false, 125)
                .await
                .unwrap();
            assert!(
                f.store
                    .consume_queued_prompt(
                        f.user,
                        f.session,
                        first.prompt.key(),
                        &first.prompt.content,
                        125
                    )
                    .await
                    .is_err()
            );
            f.store
                .consume_queued_prompt(
                    f.user,
                    f.session,
                    recovered.prompt.key(),
                    &recovered.prompt.content,
                    125,
                )
                .await
                .unwrap();
            let blocked = RunReport {
                state: RunState::Blocked,
                detail: "Awaiting approval".into(),
                permission_id: Some("permission".into()),
            };
            f.store
                .report_plugin_run(&recovered.lease, &blocked, 126)
                .await
                .unwrap();
            assert!(
                f.store
                    .report_plugin_run(&first.lease, &blocked, 126)
                    .await
                    .is_err()
            );
            assert!(
                f.store
                    .release_plugin_run(&recovered.lease, 127)
                    .await
                    .is_err()
            );
            assert!(
                f.store
                    .claim_plugin_runs(&f.plugin.host_id, 0, &"e".repeat(32), 246)
                    .await
                    .unwrap()
                    .runs
                    .is_empty()
            );
            let current = f
                .request(&RunRequest::Read { id: submitted.id })
                .await
                .unwrap()
                .runs
                .remove(0);
            assert_eq!(current.state, RunState::Interrupted);
            assert!(current.permission_id.is_none());
            assert!(
                f.store
                    .list_queued_prompts(f.user, f.session)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    });
}
#[test]
fn run_status_acknowledgements_release_busy_claims_and_reject_wrong_identity() {
    block_on(async {
        let f = Fixture::new(WorkspaceMode::Remote).await;
        let submitted = f
            .request(&Fixture::submission("one"))
            .await
            .unwrap()
            .runs
            .remove(0);
        let first = f
            .store
            .claim_plugin_runs(&f.plugin.host_id, 0, &"b".repeat(32), 3)
            .await
            .unwrap()
            .runs
            .remove(0);
        f.store.release_plugin_run(&first.lease, 4).await.unwrap();
        assert!(f.store.release_plugin_run(&first.lease, 4).await.is_err());
        let delivery = f
            .store
            .claim_plugin_runs(&f.plugin.host_id, 0, &"c".repeat(32), 5)
            .await
            .unwrap()
            .runs
            .remove(0);
        let mut wrong = delivery.lease.clone();
        wrong.host_id = "foreign".into();
        assert!(f.store.renew_plugin_run(&wrong, 6).await.is_err());
        let done = RunReport {
            state: RunState::Completed,
            detail: "Source interpretation belongs in the plugin".into(),
            permission_id: None,
        };
        assert!(
            f.store
                .report_plugin_run(&delivery.lease, &done, 6)
                .await
                .is_err()
        );
        f.store
            .session_run_lease(f.user, f.session, &delivery.run_id(), false, 6)
            .await
            .unwrap();
        f.store
            .consume_queued_prompt(
                f.user,
                f.session,
                delivery.prompt.key(),
                &delivery.prompt.content,
                6,
            )
            .await
            .unwrap();
        f.store
            .report_plugin_run(&delivery.lease, &done, 7)
            .await
            .unwrap();
        f.store
            .report_plugin_run(&delivery.lease, &done, 8)
            .await
            .unwrap();
        let changed = RunReport {
            detail: "changed".into(),
            ..done
        };
        assert!(
            f.store
                .report_plugin_run(&delivery.lease, &changed, 8)
                .await
                .is_err()
        );
        assert_eq!(
            f.request(&RunRequest::Read { id: submitted.id })
                .await
                .unwrap()
                .runs[0]
                .state,
            RunState::Completed
        );
    });
}

#[test]
fn conversation_discovery_is_owned_bounded_read_only_and_reports_raw_activity_and_model_metadata() {
    use openwebide_core::plugins::records::CollectionResult;
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let f = Fixture::new(mode).await;
            let request = |operation| PluginHostRequest {
                grant: "a".repeat(32),
                capability: "collections".into(),
                payload: json!({"collection":"conversations","operation":operation}).to_string(),
            };
            let other_project = f
                .store
                .create_project(
                    &NewProject {
                        name: "sibling".into(),
                        mode,
                        path: Some("sibling".into()),
                    },
                    f.user,
                    0,
                )
                .await
                .unwrap()
                .id;
            let sibling = f
                .store
                .create_session("Sibling", None, None, Some(other_project), f.user, 0)
                .await
                .unwrap()
                .id;
            let foreign = f
                .store
                .create_session("Foreign", None, None, None, f.other, 0)
                .await
                .unwrap()
                .id;
            let global = f
                .store
                .create_session("Global", None, None, None, f.user, 0)
                .await
                .unwrap()
                .id;
            let first: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(f.user, f.session, &request(json!({"action":"list"})), 2)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(first.records.len(), 1);
            assert_eq!(first.records[0].id, f.session);
            assert_eq!(first.records[0].value["origin"], true);
            assert_eq!(first.records[0].value["name"], "origin");
            let revision = first.records[0].revision;
            assert!(revision > 0);
            for id in [sibling, foreign, global] {
                assert!(
                    f.store
                        .plugin_host_request(
                            f.user,
                            f.session,
                            &request(json!({"action":"read","id":id})),
                            2
                        )
                        .await
                        .is_err()
                );
            }
            for operation in [
                json!({"action":"create","value":{}}),
                json!({"action":"update","id":f.session,"revision":revision,"value":{}}),
                json!({"action":"delete","id":f.session,"revision":revision}),
            ] {
                assert!(
                    f.store
                        .plugin_host_request(f.user, f.session, &request(operation), 2)
                        .await
                        .is_err()
                );
            }
            let server = f
                .store
                .insert_connection(&NewConnection {
                    name: "Primary".into(),
                    kind: ProviderKind::Ollama,
                    base_url: "http://private-model.test".into(),
                    model: Some("server-default".into()),
                    context_limit: None,
                })
                .await
                .unwrap();
            f.store
                .set_session_connection(f.session, server.id, f.user)
                .await
                .unwrap();
            f.store
                .insert_message(f.session, Role::User, "Private conversation text", 10)
                .await
                .unwrap();
            f.store
                .set_user_setting(
                    f.user,
                    &format!("session_model_{}", f.session),
                    &json!({"connection_id":server.id,"model":"raw-override"}).to_string(),
                )
                .await
                .unwrap();
            let read = f
                .store
                .plugin_host_request(
                    f.user,
                    f.session,
                    &request(json!({"action":"read","id":f.session})),
                    11,
                )
                .await
                .unwrap();
            assert!(!read.contains("Private conversation text"));
            assert!(!read.contains("private-model.test"));
            let read: CollectionResult = serde_json::from_str(&read).unwrap();
            assert_eq!(read.records[0].updated_at, 10);
            assert_eq!(read.records[0].value["connection"]["id"], server.id);
            assert_eq!(
                read.records[0].value["connection"]["model"],
                "server-default"
            );
            assert_eq!(read.records[0].value["connection"]["enabled"], true);
            assert_eq!(read.records[0].value["last_activity"], 10);
            assert_eq!(
                read.records[0].value["model_override"]["model"],
                "raw-override"
            );
            assert_ne!(read.records[0].revision, revision);
            let again: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request(json!({"action":"read","id":f.session})),
                        11,
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(again.records, read.records);
            for i in 0..35 {
                f.store
                    .create_session(
                        &format!("Conversation {i}"),
                        None,
                        None,
                        Some(f.project),
                        f.user,
                        i,
                    )
                    .await
                    .unwrap();
            }
            let first: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(f.user, f.session, &request(json!({"action":"list"})), 12)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(first.records.len(), 32);
            let next = first.next.unwrap();
            assert_eq!(next, first.records.last().unwrap().id);
            let second: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request(json!({"action":"list","after":next})),
                        12,
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(second.records.len(), 4);
            assert!(second.next.is_none());
            assert!(second.records.iter().all(|record| record.id > next));
            let padding = json!({"padding":"x".repeat(61*1024)}).to_string();
            for record in first.records.iter().chain(&second.records) {
                f.store
                    .set_user_setting(f.user, &format!("session_model_{}", record.id), &padding)
                    .await
                    .unwrap();
            }
            let mut after = 0;
            let mut seen = Vec::new();
            loop {
                let encoded = f
                    .store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request(json!({"action":"list","after":after})),
                        12,
                    )
                    .await
                    .unwrap();
                assert!(encoded.len() <= 1024 * 1024);
                let page: CollectionResult = serde_json::from_str(&encoded).unwrap();
                assert!(page.records.len() < 32);
                seen.extend(page.records.iter().map(|record| record.id));
                match page.next {
                    Some(next) => {
                        assert!(next > after);
                        after = next;
                    }
                    None => break,
                }
            }
            assert_eq!(seen.len(), 36);
            f.store
                .set_user_setting(
                    f.user,
                    &format!("session_model_{}", f.session),
                    &json!({"padding":"x".repeat(64*1024)}).to_string(),
                )
                .await
                .unwrap();
            assert!(
                f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request(json!({"action":"read","id":f.session})),
                        12
                    )
                    .await
                    .is_err()
            );
            // A sessionless scope sees only global conversations in this account.
            f.store
                .issue_plugin_context_grant(
                    f.user,
                    &PluginExecutionContext::default(),
                    &f.plugin,
                    &"f".repeat(32),
                    1,
                )
                .await
                .unwrap();
            let global: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_context_host_request(
                        f.user,
                        &PluginHostRequest {
                            grant: "f".repeat(32),
                            capability: "collections".into(),
                            payload:
                                json!({"collection":"conversations","operation":{"action":"list"}})
                                    .to_string(),
                        },
                        2,
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(global.records.len(), 1);
            assert_eq!(global.records[0].value["name"], "Global");
            assert_eq!(global.records[0].value["origin"], false);
        }
    });
}

#[test]
fn task_collection_preserves_ids_history_and_cas_while_separating_plugin_policy_from_legacy_dispatch()
 {
    use openwebide_core::{
        plugins::records::CollectionResult,
        scheduled::{ExecutionHost, Schedule, SessionTarget, TaskCommand, TaskDraft},
    };
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let f = Fixture::new(mode).await;
            f.bind_host(mode).await;
            let request = |operation| PluginHostRequest {
                grant: "a".repeat(32),
                capability: "collections".into(),
                payload: json!({"collection":"tasks","operation":operation}).to_string(),
            };
            let draft = TaskDraft {
                session_target: SessionTarget::Existing,
                auto_title: false,
                title: "Legacy task".into(),
                prompt: "Check build".into(),
                session_id: f.session,
                model: None,
                schedule: Schedule::Once { at: 100 },
                enabled: true,
            };
            let legacy = f
                .store
                .scheduled_session_command(
                    f.user,
                    f.session,
                    &TaskCommand::Create {
                        draft: draft.clone(),
                    },
                    1,
                )
                .await
                .unwrap()
                .remove(0);
            f.store.db.execute("INSERT INTO scheduled_runs(task_id,due_at,status,detail,session_id) VALUES(?,50,'complete','Historic result',?)",&[DbValue::Int(legacy.id),DbValue::Int(f.session)]).await.unwrap();
            let before = f
                .store
                .plugin_host_request(
                    f.user,
                    f.session,
                    &request(json!({"action":"read","id":legacy.id})),
                    2,
                )
                .await
                .unwrap();
            let before: CollectionResult = serde_json::from_str(&before).unwrap();
            assert!(before.records[0].value["owner"].is_null());
            let value = json!({"draft":draft,"next_run":120,"state":{"job":17},"monitor":null});
            let claimed=f.store.plugin_host_request(f.user,f.session,&request(json!({"action":"update","id":legacy.id,"revision":legacy.revision,"value":value})),3).await.unwrap();
            let claimed: CollectionResult = serde_json::from_str(&claimed).unwrap();
            assert_eq!(claimed.records[0].id, legacy.id);
            assert_eq!(claimed.records[0].revision, legacy.revision + 1);
            assert_eq!(
                claimed.records[0].value["owner"],
                f.plugin.storage_namespace()
            );
            assert_eq!(claimed.records[0].value["state"]["job"], 17);
            assert!(f.store.plugin_host_request(f.user,f.session,&request(json!({"action":"update","id":legacy.id,"revision":legacy.revision,"value":value})),4).await.is_err());
            let visible = f
                .store
                .scheduled_tasks(f.user, Some(f.project), 4)
                .await
                .unwrap();
            assert_eq!(visible[0].id, legacy.id);
            assert_eq!(
                visible[0].last_run.as_ref().unwrap().detail,
                "Historic result"
            );
            assert_eq!(visible[0].next_run, Some(120));
            assert!(
                f.store
                    .scheduled_session_command(
                        f.user,
                        f.session,
                        &TaskCommand::SetEnabled {
                            id: legacy.id,
                            revision: claimed.records[0].revision,
                            enabled: false
                        },
                        4
                    )
                    .await
                    .is_err()
            );
            let host = ExecutionHost {
                id: f.plugin.host_id.clone(),
                name: "Host".into(),
                last_seen: 200,
            };
            assert!(f.store.due_scheduled(&host, 200).await.unwrap().is_empty());
            let visible = f
                .store
                .scheduled_tasks(f.user, Some(f.project), 200)
                .await
                .unwrap();
            assert_eq!(visible[0].next_run, Some(120));
            let mut monitor = value.clone();
            monitor["draft"]["title"] = json!("Monitor");
            monitor["next_run"] = json!(15);
            monitor["monitor"] =
                json!({"session_id":f.session,"interval_seconds":10,"remaining":1,"expires_at":20});
            // Expiry and repetition are source decisions; the host only validates record shape.
            monitor["draft"]["schedule"] =
                json!({"kind":"cron","expression":"not cron","timezone":"Mars/Base"});
            let created = f
                .store
                .plugin_host_request(
                    f.user,
                    f.session,
                    &request(json!({"action":"create","value":monitor})),
                    5,
                )
                .await
                .unwrap();
            let created: CollectionResult = serde_json::from_str(&created).unwrap();
            let id = created.records[0].id;
            assert!(f.store.due_scheduled(&host, 200).await.unwrap().is_empty());
            let unchanged = f
                .store
                .plugin_host_request(
                    f.user,
                    f.session,
                    &request(json!({"action":"read","id":id})),
                    6,
                )
                .await
                .unwrap();
            let unchanged: CollectionResult = serde_json::from_str(&unchanged).unwrap();
            assert_eq!(unchanged.records[0].value["monitor"]["remaining"], 1);
            assert_eq!(unchanged.records[0].value["next_run"], 15);
            f.store
                .plugin_host_request(
                    f.user,
                    f.session,
                    &request(
                        json!({"action":"delete","id":id,"revision":created.records[0].revision}),
                    ),
                    7,
                )
                .await
                .unwrap();
            assert!(
                f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request(json!({"action":"read","id":id})),
                        7
                    )
                    .await
                    .is_err()
            );
        }
    });
}

#[test]
fn task_collection_enforces_scope_plugin_ownership_and_inflight_handoff() {
    use openwebide_core::{
        plugins::records::CollectionResult,
        scheduled::{Schedule, SessionTarget, TaskCommand, TaskDraft},
    };
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let f = Fixture::new(mode).await;
            f.bind_host(mode).await;
            let request = |grant: &str, operation| PluginHostRequest {
                grant: grant.repeat(32),
                capability: "collections".into(),
                payload: json!({"collection":"tasks","operation":operation}).to_string(),
            };
            let draft = TaskDraft {
                session_target: SessionTarget::Existing,
                auto_title: false,
                title: "Task".into(),
                prompt: "Check".into(),
                session_id: f.session,
                model: None,
                schedule: Schedule::Once { at: 100 },
                enabled: true,
            };
            let value = json!({"draft":draft,"next_run":100,"state":{},"monitor":null});
            let created = f
                .store
                .plugin_host_request(
                    f.user,
                    f.session,
                    &request("a", json!({"action":"create","value":value})),
                    2,
                )
                .await
                .unwrap();
            let created: CollectionResult = serde_json::from_str(&created).unwrap();
            let record = &created.records[0];
            let other_session = f
                .store
                .create_session("Other", None, None, None, f.other, 0)
                .await
                .unwrap()
                .id;
            let mut invalid = value.clone();
            invalid["draft"]["session_id"] = json!(other_session);
            assert!(
                f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request("a", json!({"action":"create","value":invalid})),
                        2
                    )
                    .await
                    .is_err()
            );
            let mut other = f.plugin.clone();
            other.source.path = "plugins/other".into();
            other.manifest.name = "other".into();
            f.store
                .record_plugin(
                    f.user,
                    &RecordPlugin {
                        prepared: other.clone(),
                        revision: None,
                        approved_capabilities: vec![],
                        update_policy: None,
                        package: Some(Box::new(PluginPackage {
                            prepared: other.clone(),
                            skills: vec![],
                        })),
                    },
                    0,
                )
                .await
                .unwrap();
            f.store
                .issue_plugin_context_grant(
                    f.user,
                    &PluginExecutionContext {
                        project_id: Some(f.project),
                        session_id: Some(f.session),
                        ..Default::default()
                    },
                    &other,
                    &"b".repeat(32),
                    1,
                )
                .await
                .unwrap();
            for operation in [
                json!({"action":"update","id":record.id,"revision":record.revision,"value":value}),
                json!({"action":"delete","id":record.id,"revision":record.revision}),
            ] {
                assert!(
                    f.store
                        .plugin_host_request(f.user, f.session, &request("b", operation), 3)
                        .await
                        .is_err()
                );
            }
            let legacy = f
                .store
                .scheduled_session_command(f.user, f.session, &TaskCommand::Create { draft }, 1)
                .await
                .unwrap()
                .into_iter()
                .find(|task| task.id != record.id)
                .unwrap();
            f.store
                .db
                .execute(
                    "INSERT INTO scheduled_runs(task_id,due_at,status) VALUES(?,50,'running')",
                    &[DbValue::Int(legacy.id)],
                )
                .await
                .unwrap();
            assert!(f.store.plugin_host_request(f.user,f.session,&request("a",json!({"action":"update","id":legacy.id,"revision":legacy.revision,"value":value})),3).await.is_err());
            f.store
                .db
                .execute(
                    "UPDATE scheduled_runs SET status='complete' WHERE task_id=?",
                    &[DbValue::Int(legacy.id)],
                )
                .await
                .unwrap();
            f.store.plugin_host_request(f.user,f.session,&request("a",json!({"action":"update","id":legacy.id,"revision":legacy.revision,"value":value})),4).await.unwrap();
            f.store
                .db
                .execute(
                    "INSERT INTO goal_workers(session_id,task_id,revision) VALUES(?,?,1)",
                    &[DbValue::Int(f.session), DbValue::Int(record.id)],
                )
                .await
                .unwrap();
            assert!(
                f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request("a", json!({"action":"read","id":record.id})),
                        4
                    )
                    .await
                    .is_err()
            );
        }
    });
}

#[test]
fn run_prerequisites_reject_stale_deleted_or_ungranted_records_without_side_effects() {
    use openwebide_core::plugins::records::{CollectionResult, RecordOperation, RecordRequest};
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let f = Fixture::new(mode).await;
            let collection = |operation| PluginHostRequest {
                grant: "a".repeat(32),
                capability: "collections".into(),
                payload: encode(&RecordRequest {
                    collection: "tasks".into(),
                    operation,
                })
                .unwrap(),
            };
            let value = json!({"draft":{"session_target":"existing","session_id":f.session,
                "title":"Saved task","prompt":"Check","schedule":{"kind":"once","at":100},
                "enabled":true},"next_run":100,"state":{}});
            let record: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &collection(RecordOperation::Create {
                            value: value.clone(),
                        }),
                        2,
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
            let record = &record.records[0];
            let command = |key: &str, revision: i64| RunRequest::Submit {
                key: key.into(),
                prompt: "Check".into(),
                target: RunTarget::New {
                    title: "New work".into(),
                },
                model: None,
                completion: None,
                prerequisites: vec![RunPrerequisite {
                    capability: "collections".into(),
                    collection: "tasks".into(),
                    id: record.id,
                    revision,
                }],
            };
            let before = f
                .store
                .db
                .execute("SELECT count(*) FROM sessions", &[])
                .await
                .unwrap()
                .rows[0]
                .get_int(0)
                .unwrap();
            assert!(
                f.request(&command("stale", record.revision + 1))
                    .await
                    .is_err()
            );
            assert!(
                f.request(&RunRequest::List { after: 0 })
                    .await
                    .unwrap()
                    .runs
                    .is_empty()
            );
            let current = command("current", record.revision);
            let run = f.request(&current).await.unwrap().runs.remove(0);
            let mut disabled = value.clone();
            disabled["draft"]["enabled"] = json!(false);
            disabled["next_run"] = json!(null);
            let changed: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &collection(RecordOperation::Update {
                            id: record.id,
                            revision: record.revision,
                            value: disabled,
                        }),
                        3,
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
            // Durable retry acknowledges prior work; a late new timer cannot create work.
            assert_eq!(f.request(&current).await.unwrap().runs[0].id, run.id);
            assert!(f.request(&command("late", record.revision)).await.is_err());
            let after = f
                .store
                .db
                .execute("SELECT count(*) FROM sessions", &[])
                .await
                .unwrap()
                .rows[0]
                .get_int(0)
                .unwrap();
            assert_eq!(after, before + 1);
            f.store
                .plugin_host_request(
                    f.user,
                    f.session,
                    &collection(RecordOperation::Delete {
                        id: record.id,
                        revision: changed.records[0].revision,
                    }),
                    4,
                )
                .await
                .unwrap();
            assert!(
                f.request(&command("deleted", changed.records[0].revision))
                    .await
                    .is_err()
            );
            let mut ungranted = Fixture::submission("ungranted");
            if let RunRequest::Submit { prerequisites, .. } = &mut ungranted {
                prerequisites.push(RunPrerequisite {
                    capability: "records".into(),
                    collection: "notes".into(),
                    id: 1,
                    revision: 1,
                });
            }
            assert!(matches!(
                f.request(&ungranted).await,
                Err(StorageError::InvalidRequest(_))
            ));
            assert_eq!(
                f.request(&RunRequest::List { after: 0 })
                    .await
                    .unwrap()
                    .runs
                    .len(),
                1
            );
            assert_eq!(
                f.store
                    .db
                    .execute("SELECT count(*) FROM queued_prompts", &[])
                    .await
                    .unwrap()
                    .rows[0]
                    .get_int(0)
                    .unwrap(),
                1
            );
        }
    });
}

#[test]
fn task_collection_pages_are_bounded_and_schema_upgrade_preserves_legacy_data() {
    use openwebide_core::{plugins::records::*, scheduled::*};
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let f = Fixture::new(mode).await;
            f.bind_host(mode).await;
            let draft = TaskDraft {
                session_target: SessionTarget::Existing,
                session_id: f.session,
                title: "Original".into(),
                prompt: "Keep me".into(),
                auto_title: false,
                model: None,
                schedule: Schedule::Once { at: 100 },
                enabled: true,
            };
            let original = f
                .store
                .scheduled_session_command(
                    f.user,
                    f.session,
                    &TaskCommand::Create {
                        draft: draft.clone(),
                    },
                    1,
                )
                .await
                .unwrap()
                .remove(0);
            f.store.db.execute("INSERT INTO scheduled_runs(task_id,due_at,status,detail) VALUES(?,10,'complete','Retained history')",&[DbValue::Int(original.id)]).await.unwrap();
            for column in ["plugin_owner", "plugin_state", "plugin_updated_at"] {
                f.store
                    .db
                    .execute(
                        &format!("ALTER TABLE scheduled_tasks DROP COLUMN {column}"),
                        &[],
                    )
                    .await
                    .unwrap();
            }
            f.store
                .db
                .execute("PRAGMA user_version=53", &[])
                .await
                .unwrap();
            f.store.migrate().await.unwrap();
            let read = |operation| PluginHostRequest {
                grant: "a".repeat(32),
                capability: "collections".into(),
                payload: encode(&RecordRequest {
                    collection: "tasks".into(),
                    operation,
                })
                .unwrap(),
            };
            let value: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &read(RecordOperation::Read { id: original.id }),
                        2,
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(value.records[0].value["owner"], json!(null));
            assert_eq!(value.records[0].value["state"], json!({}));
            assert_eq!(value.records[0].revision, original.revision);
            let visible = f
                .store
                .scheduled_tasks(f.user, Some(f.project), 2)
                .await
                .unwrap();
            assert_eq!(
                visible[0].last_run.as_ref().unwrap().detail,
                "Retained history"
            );
            // Replay must leave existing revisions, owner and history unchanged.
            f.store
                .db
                .execute("PRAGMA user_version=53", &[])
                .await
                .unwrap();
            f.store.migrate().await.unwrap();
            let mut large = draft;
            large.prompt = "x".repeat(32 * 1024);
            for _ in 0..33 {
                f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &read(RecordOperation::Create {
                            value: json!({"draft":large,"next_run":100,"state":{}}),
                        }),
                        2,
                    )
                    .await
                    .unwrap();
            }
            let mut after = 0;
            let mut ids = Vec::new();
            loop {
                let serialized = f
                    .store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &read(RecordOperation::List { after }),
                        2,
                    )
                    .await
                    .unwrap();
                assert!(serialized.len() <= 1024 * 1024);
                let page: CollectionResult = serde_json::from_str(&serialized).unwrap();
                assert!(page.records.len() <= 32);
                ids.extend(page.records.iter().map(|record| record.id));
                let Some(next) = page.next else { break };
                assert!(next > after);
                after = next;
            }
            assert_eq!(ids.len(), 34);
            assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
        }
    });
}

#[test]
fn configuration_metadata_is_read_only_account_scoped_current_and_credential_free() {
    use openwebide_core::plugins::records::CollectionResult;
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let f = Fixture::new(mode).await;
            let server = f
                .store
                .insert_connection(&NewConnection {
                    name: "private server name".into(),
                    kind: ProviderKind::Ollama,
                    base_url: "http://private-server.test:1234".into(),
                    model: Some("server model".into()),
                    context_limit: None,
                })
                .await
                .unwrap();
            f.store.set_user_setting(f.user,"model_defaults",&json!({"primary":{"server_id":server.id,"model":"my default"},"fast":{"server_id":server.id,"model":"my fast"},"unrelated":"Do not export"}).to_string()).await.unwrap();
            f.store
                .set_user_setting(
                    f.other,
                    "model_defaults",
                    &json!({"primary":{"server_id":server.id,"model":"other account"}}).to_string(),
                )
                .await
                .unwrap();
            f.store
                .set_user_setting(f.user, "default_connection", &server.id.to_string())
                .await
                .unwrap();
            let request = |token: &str, operation| PluginHostRequest {
                grant: token.repeat(32),
                capability: "collections".into(),
                payload: json!({"collection":"configuration","operation":operation}).to_string(),
            };
            let read = request("a", json!({"action":"read","id":1}));
            let body = f
                .store
                .plugin_host_request(f.user, f.session, &read, 2)
                .await
                .unwrap();
            let page: CollectionResult = serde_json::from_str(&body).unwrap();
            assert_eq!(page.records[0].value["primary"]["model"], "my default");
            assert_eq!(page.records[0].value["fast"]["model"], "my fast");
            assert_eq!(page.records[0].value["default_connection"], server.id);
            assert_eq!(
                page.records[0].value["servers"],
                json!([{"id":server.id,"model":"server model","enabled":true}])
            );
            assert!(!body.contains("private-server"));
            assert!(!body.contains("private server name"));
            assert!(!body.contains("other account"));
            assert!(!body.contains("unrelated"));
            assert!(
                f.store
                    .plugin_host_request(f.other, f.session, &read, 2)
                    .await
                    .is_err()
            );
            for operation in [
                json!({"action":"read","id":2}),
                json!({"action":"create","value":{}}),
                json!({"action":"update","id":1,"revision":page.records[0].revision,"value":{}}),
                json!({"action":"delete","id":1,"revision":page.records[0].revision}),
            ] {
                assert!(
                    f.store
                        .plugin_host_request(f.user, f.session, &request("a", operation), 2)
                        .await
                        .is_err()
                );
            }
            let empty: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request("a", json!({"action":"list","after":1})),
                        2,
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert!(empty.records.is_empty());
            f.store
                .issue_plugin_context_grant(
                    f.user,
                    &PluginExecutionContext::default(),
                    &f.plugin,
                    &"b".repeat(32),
                    1,
                )
                .await
                .unwrap();
            let global: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_context_host_request(
                        f.user,
                        &request("b", json!({"action":"read","id":1})),
                        2,
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(global, page);
            f.store
                .set_user_setting(
                    f.user,
                    "model_defaults",
                    &json!({"primary":{"server_id":server.id,"model":"new default"}}).to_string(),
                )
                .await
                .unwrap();
            let changed: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(f.user, f.session, &read, 3)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(changed.records[0].value["primary"]["model"], "new default");
            assert_ne!(changed.records[0].revision, page.records[0].revision);
            f.store
                .db
                .execute(
                    "UPDATE connections SET model=? WHERE id=?",
                    &[
                        DbValue::Text("x".repeat(64 * 1024)),
                        DbValue::Int(server.id),
                    ],
                )
                .await
                .unwrap();
            assert!(matches!(
                f.store
                    .plugin_host_request(f.user, f.session, &read, 3)
                    .await,
                Err(StorageError::InvalidValue(_))
            ));
        }
    });
}

#[test]
fn task_history_projects_progress_preserves_legacy_ids_and_survives_raw_history_removal() {
    use openwebide_core::plugins::records::CollectionResult;
    use openwebide_core::scheduled::*;
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let f = Fixture::new(mode).await;
            f.bind_host(mode).await;
            let draft = TaskDraft {
                session_target: SessionTarget::Existing,
                session_id: f.session,
                title: "Task".into(),
                prompt: "Check".into(),
                auto_title: false,
                model: None,
                schedule: Schedule::Once { at: 100 },
                enabled: true,
            };
            let task = f
                .store
                .scheduled_session_command(
                    f.user,
                    f.session,
                    &TaskCommand::Create {
                        draft: draft.clone(),
                    },
                    1,
                )
                .await
                .unwrap()
                .remove(0);
            let legacy=f.store.db.execute("INSERT INTO scheduled_runs(task_id,due_at,status,detail) VALUES(?,10,'complete','Original history')",&[DbValue::Int(task.id)]).await.unwrap().last_insert_rowid;
            let request = |collection: &str, operation| PluginHostRequest {
                grant: "a".repeat(32),
                capability: "collections".into(),
                payload: json!({"collection":collection,"operation":operation}).to_string(),
            };
            f.store.plugin_host_request(f.user,f.session,&request("tasks",json!({"action":"update","id":task.id,"revision":task.revision,"value":{"draft":draft,"next_run":100,"state":{}}})),2).await.unwrap();
            let raw = f
                .request(&Fixture::submission("task:1:1:100"))
                .await
                .unwrap()
                .runs
                .remove(0);
            let create = request(
                "task_runs",
                json!({"action":"create","value":{"key":"first","task_id":task.id,"due_at":100,"run_id":raw.id}}),
            );
            let history: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(f.user, f.session, &create, 2)
                    .await
                    .unwrap(),
            )
            .unwrap();
            let id = history.records[0].id;
            assert!(id > 1_i64 << 52);
            assert_eq!(history.records[0].value["snapshot"]["status"], "queued");
            let visible = f
                .store
                .scheduled_tasks(f.user, Some(f.project), 2)
                .await
                .unwrap();
            assert_eq!(visible[0].last_run.as_ref().unwrap().id, id);
            assert_eq!(
                visible[0].last_run.as_ref().unwrap().session_id,
                Some(f.session)
            );
            assert_eq!(visible[0].last_run.as_ref().unwrap().status, "queued");
            let repeated: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(f.user, f.session, &create, 3)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(history, repeated);
            let read = request("task_runs", json!({"action":"read","id":id}));
            f.store.db.execute("UPDATE plugin_runs SET state='blocked',detail='Waiting for approval',permission_id='approval' WHERE id=?",&[DbValue::Int(raw.id)]).await.unwrap();
            let blocked: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(f.user, f.session, &read, 3)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert!(blocked.records[0].revision > history.records[0].revision);
            assert_eq!(blocked.records[0].value["snapshot"]["status"], "blocked");
            assert_eq!(
                f.store
                    .scheduled_tasks(f.user, Some(f.project), 3)
                    .await
                    .unwrap()[0]
                    .last_run
                    .as_ref()
                    .unwrap()
                    .permission_id,
                Some("approval".into())
            );
            assert!(f.store.plugin_host_request(f.user,f.session,&request("task_runs",json!({"action":"delete","id":id,"revision":blocked.records[0].revision})),3).await.is_err());
            // SDK cancellation updates the projection without any Scheduling feature call.
            let current = f
                .request(&RunRequest::Read { id: raw.id })
                .await
                .unwrap()
                .runs
                .remove(0);
            let cancelled = f
                .request(&RunRequest::Cancel {
                    id: raw.id,
                    revision: current.revision,
                })
                .await
                .unwrap()
                .runs
                .remove(0);
            assert_eq!(cancelled.state, RunState::Cancelled);
            let terminal: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(f.user, f.session, &read, 3)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(terminal.records[0].value["snapshot"]["status"], "cancelled");
            f.request(&RunRequest::Delete {
                id: raw.id,
                revision: cancelled.revision,
            })
            .await
            .unwrap();
            let retained: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(f.user, f.session, &create, 3)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(retained.records[0].id, id);
            assert_eq!(retained.records[0].value["snapshot"]["status"], "cancelled");
            assert_eq!(retained.records[0].value["run_id"], raw.id);
            assert_eq!(
                f.store
                    .scheduled_tasks(f.user, Some(f.project), 3)
                    .await
                    .unwrap()[0]
                    .last_run
                    .as_ref()
                    .unwrap()
                    .id,
                id
            );
            let old: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request("task_runs", json!({"action":"read","id":legacy})),
                        3,
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(old.records[0].id, legacy);
            assert_eq!(
                old.records[0].value["snapshot"]["detail"],
                "Original history"
            );
            assert_eq!(old.records[0].value["editable"], false);
            assert!(f.store.plugin_host_request(f.user,f.session,&request("task_runs",json!({"action":"delete","id":id,"revision":history.records[0].revision})),3).await.is_err());
            f.store
                .plugin_host_request(
                    f.user,
                    f.session,
                    &request(
                        "task_runs",
                        json!({"action":"delete","id":id,"revision":retained.records[0].revision}),
                    ),
                    3,
                )
                .await
                .unwrap();
            assert_eq!(
                f.store
                    .scheduled_tasks(f.user, Some(f.project), 3)
                    .await
                    .unwrap()[0]
                    .last_run
                    .as_ref()
                    .unwrap()
                    .id,
                legacy
            );
        }
    });
}

#[test]
fn task_bookkeeping_preserves_existing_deleted_models_without_accepting_new_ones() {
    use openwebide_core::{NewConnection, ProviderKind, plugins::records::CollectionResult};
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let f = Fixture::new(mode).await;
            let model = f
                .store
                .insert_connection(&NewConnection {
                    name: "Deleted model".into(),
                    kind: ProviderKind::Ollama,
                    base_url: "http://model.test".into(),
                    model: Some("model".into()),
                    context_limit: None,
                })
                .await
                .unwrap()
                .id;
            let request = |operation| PluginHostRequest {
                grant: "a".repeat(32),
                capability: "collections".into(),
                payload: json!({"collection":"tasks","operation":operation}).to_string(),
            };
            let mut value = json!({"draft":{"title":"Saved task","prompt":"Check",
                "session_target":"existing","session_id":f.session,"enabled":true,
                "model":{"server_id":model,"model":"model"},
                "schedule":{"kind":"once","at":100}},"next_run":100,"state":{}});
            let created: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request(json!({"action":"create","value":value})),
                        2,
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
            let record = &created.records[0];
            f.store.delete_connection(model).await.unwrap();
            value["next_run"] = json!(null);
            value["state"] =
                json!({"last_result":{"state":"failed","detail":"Model no longer exists"}});
            let saved: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request(json!({"action":"update","id":record.id,
                    "revision":record.revision,"value":value})),
                        3,
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(saved.records[0].value["draft"]["model"]["server_id"], model);
            assert!(saved.records[0].value["next_run"].is_null());
            assert!(
                f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request(json!({"action":"create","value":value})),
                        4
                    )
                    .await
                    .is_err()
            );
            value["draft"]["model"]["server_id"] = json!(model + 1000);
            assert!(
                f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request(json!({
                "action":"update","id":record.id,"revision":saved.records[0].revision,
                "value":value})),
                        4
                    )
                    .await
                    .is_err()
            );
        }
    });
}

#[test]
fn task_history_requires_owned_tasks_scoped_snapshots_and_immutable_keys() {
    use openwebide_core::plugins::records::CollectionResult;
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let f = Fixture::new(mode).await;
            let request = |collection: &str, operation| PluginHostRequest {
                grant: "a".repeat(32),
                capability: "collections".into(),
                payload: json!({"collection":collection,"operation":operation}).to_string(),
            };
            let value = json!({"draft":{"title":"Task","prompt":"Check","session_target":"existing","session_id":f.session,"schedule":{"kind":"once","at":100},"enabled":true},"next_run":100,"state":{}});
            let created: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request("tasks", json!({"action":"create","value":value})),
                        2,
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
            let task = created.records[0].id;
            let mut snapshot = json!({"status":"failed","detail":"Source-owned preflight failure","session_id":f.session});
            let operation = |key: &str, task_id: i64, snapshot: serde_json::Value| json!({"action":"create","value":{"key":key,"task_id":task_id,"due_at":100,"snapshot":snapshot}});
            let created: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request("task_runs", operation("failure", task, snapshot.clone())),
                        2,
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(
                created.records[0].value["snapshot"]["detail"],
                "Source-owned preflight failure"
            );
            let foreign = f
                .store
                .create_session("Foreign", None, None, None, f.other, 0)
                .await
                .unwrap()
                .id;
            snapshot["session_id"] = json!(foreign);
            assert!(
                f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request("task_runs", operation("foreign", task, snapshot.clone())),
                        2
                    )
                    .await
                    .is_err()
            );
            snapshot["session_id"] = json!(f.session);
            snapshot["status"] = json!("running");
            assert!(
                f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request(
                            "task_runs",
                            operation("fake-running", task, snapshot.clone())
                        ),
                        2
                    )
                    .await
                    .is_err()
            );
            snapshot["status"] = json!("complete");
            assert!(
                f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request(
                            "task_runs",
                            operation("missing-task", task + 1, snapshot.clone())
                        ),
                        2
                    )
                    .await
                    .is_err()
            );
            let mut wrong = operation("failure", task, snapshot.clone());
            wrong["value"]["due_at"] = json!(101);
            assert!(
                f.store
                    .plugin_host_request(f.user, f.session, &request("task_runs", wrong), 2)
                    .await
                    .is_err()
            );
            let id = created.records[0].id;
            assert!(
                f.store
                    .plugin_host_request(
                        f.other,
                        f.session,
                        &request("task_runs", json!({"action":"read","id":id})),
                        2
                    )
                    .await
                    .is_err()
            );
            assert!(
                f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request(
                            "task_runs",
                            json!({"action":"update","id":id,"revision":1,"value":{}})
                        ),
                        2
                    )
                    .await
                    .is_err()
            );
            f.store
                .db
                .execute(
                    "UPDATE scheduled_tasks SET plugin_owner='different' WHERE id=?",
                    &[DbValue::Int(task)],
                )
                .await
                .unwrap();
            assert!(
                f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request("task_runs", json!({"action":"read","id":id})),
                        2
                    )
                    .await
                    .is_err()
            );
        }
    });
}

#[test]
fn task_history_replay_and_deleted_run_fallback_preserve_snapshots_and_foreign_links_fail() {
    use openwebide_core::plugins::records::CollectionResult;
    block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let f = Fixture::new(mode).await;
            let request = |collection: &str, operation| PluginHostRequest {
                grant: "a".repeat(32),
                capability: "collections".into(),
                payload: json!({"collection":collection,"operation":operation}).to_string(),
            };
            let value = json!({"draft":{"title":"Task","prompt":"Check","session_target":"existing","session_id":f.session,"schedule":{"kind":"once","at":100},"enabled":true},"next_run":100,"state":{}});
            let created: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request("tasks", json!({"action":"create","value":value})),
                        2,
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
            let task = created.records[0].id;
            let raw = f
                .request(&Fixture::submission("fallback"))
                .await
                .unwrap()
                .runs
                .remove(0);
            let operation = json!({"action":"create","value":{"key":"fallback","task_id":task,"due_at":100,"run_id":raw.id,"snapshot":{"status":"cancelled","detail":"Retained event result","session_id":f.session}}});
            f.store
                .db
                .execute(
                    "UPDATE plugin_runs SET project_scope=0 WHERE id=?",
                    &[DbValue::Int(raw.id)],
                )
                .await
                .unwrap();
            assert!(
                f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request("task_runs", operation.clone()),
                        2
                    )
                    .await
                    .is_err()
            );
            f.store
                .db
                .execute(
                    "UPDATE plugin_runs SET project_scope=? WHERE id=?",
                    &[DbValue::Int(f.project), DbValue::Int(raw.id)],
                )
                .await
                .unwrap();
            let cancelled = f
                .request(&RunRequest::Cancel {
                    id: raw.id,
                    revision: raw.revision,
                })
                .await
                .unwrap()
                .runs
                .remove(0);
            f.request(&RunRequest::Delete {
                id: raw.id,
                revision: cancelled.revision,
            })
            .await
            .unwrap();
            let created: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request("task_runs", operation.clone()),
                        2,
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(
                created.records[0].value["snapshot"]["detail"],
                "Retained event result"
            );
            f.store
                .db
                .execute("PRAGMA user_version=54", &[])
                .await
                .unwrap();
            f.store.migrate().await.unwrap();
            let repeated: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(f.user, f.session, &request("task_runs", operation), 2)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(created, repeated);
            let live = f
                .request(&Fixture::submission("linked-after-replay"))
                .await
                .unwrap()
                .runs
                .remove(0);
            let linked:CollectionResult=serde_json::from_str(&f.store.plugin_host_request(f.user,f.session,&request("task_runs",json!({"action":"create","value":{"key":"live","task_id":task,"due_at":100,"run_id":live.id}})),2).await.unwrap()).unwrap();
            let cancelled = f
                .request(&RunRequest::Cancel {
                    id: live.id,
                    revision: live.revision,
                })
                .await
                .unwrap()
                .runs
                .remove(0);
            let read: CollectionResult = serde_json::from_str(
                &f.store
                    .plugin_host_request(
                        f.user,
                        f.session,
                        &request(
                            "task_runs",
                            json!({"action":"read","id":linked.records[0].id}),
                        ),
                        2,
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(read.records[0].value["snapshot"]["status"], "cancelled");
            assert_eq!(cancelled.state, RunState::Cancelled);
        }
    });
}
