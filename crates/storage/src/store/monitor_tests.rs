use super::*;
use crate::rusqlite_db::RusqliteDb;
use openwebide_core::scheduled::{
    DispatchResult, ExecutionHost, HostBinding, MonitorCommand, TaskCommand,
};

struct Fixture {
    store: Store<RusqliteDb>,
    user: UserId,
    other: UserId,
    project: Option<i64>,
    session: i64,
    sibling: i64,
    host: ExecutionHost,
}
#[test]
fn monitor_reads_are_owned_scope_projections_without_expiry_side_effects() {
    futures::executor::block_on(async {
        for mode in [
            None,
            Some(WorkspaceMode::Local),
            Some(WorkspaceMode::Remote),
        ] {
            let f = Fixture::new(mode).await;
            f.authorize().await;
            let original = f
                .command(
                    MonitorCommand::Start {
                        prompt: "Check".into(),
                        delay_seconds: 5,
                        interval_seconds: 10,
                        max_checks: 1,
                    },
                    0,
                )
                .await;
            let monitor = original[0].clone();
            assert_eq!(
                f.store
                    .scheduled_monitors(f.user, f.session, 100000)
                    .await
                    .unwrap()[0]
                    .id,
                monitor.id
            );
            assert_eq!(
                f.store
                    .scheduled_monitors(f.user, f.session, 100000)
                    .await
                    .unwrap()[0]
                    .revision,
                monitor.revision
            );
            assert!(
                f.store
                    .scheduled_monitors(f.user, f.sibling, 100000)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(
                f.store
                    .scheduled_monitors(f.other, f.session, 100000)
                    .await
                    .is_err()
            );
            if mode == Some(WorkspaceMode::Local) {
                let binding = HostBinding {
                    host_id: "paired-new".into(),
                    path: "/project".into(),
                };
                assert!(
                    f.store
                        .bind_background_host(f.other, f.project.unwrap(), &binding)
                        .await
                        .is_err()
                );
                f.store
                    .bind_background_host(f.user, f.project.unwrap(), &binding)
                    .await
                    .unwrap();
                assert!(
                    f.store
                        .bind_background_host(
                            f.user,
                            f.project.unwrap(),
                            &HostBinding {
                                path: String::new(),
                                ..binding
                            }
                        )
                        .await
                        .is_err()
                );
                assert_eq!(
                    f.store
                        .scheduled_monitors(f.user, f.session, 100000)
                        .await
                        .unwrap()[0]
                        .revision,
                    monitor.revision
                );
            }
        }
    });
}
impl Fixture {
    async fn new(mode: Option<WorkspaceMode>) -> Self {
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
        let project = if let Some(mode) = mode {
            Some(
                store
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
                    .id,
            )
        } else {
            None
        };
        let session = store
            .create_session("Session", None, None, project, user, 0)
            .await
            .unwrap()
            .id;
        let sibling = store
            .create_session("Sibling", None, None, project, user, 0)
            .await
            .unwrap()
            .id;
        let host = ExecutionHost {
            id: if mode == Some(WorkspaceMode::Local) {
                "paired"
            } else {
                "server"
            }
            .into(),
            name: "Host".into(),
            last_seen: 0,
        };
        Self {
            store,
            user,
            other,
            project,
            session,
            sibling,
            host,
        }
    }
    async fn authorize(&self) {
        self.store
            .set_user_setting(
                self.user,
                &format!("scheduled_host_{}", self.project.unwrap_or(0)),
                &serde_json::to_string(&HostBinding {
                    host_id: self.host.id.clone(),
                    path: "/authorized/project".into(),
                })
                .unwrap(),
            )
            .await
            .unwrap();
    }
    async fn command(
        &self,
        command: MonitorCommand,
        now: i64,
    ) -> Vec<openwebide_core::scheduled::ScheduledTask> {
        self.store
            .scheduled_session_command(
                self.user,
                self.session,
                &TaskCommand::Monitor {
                    session_id: 0,
                    command,
                },
                now,
            )
            .await
            .unwrap()
    }
    async fn start(&self, checks: i64) -> i64 {
        self.command(
            MonitorCommand::Start {
                prompt: "Check the build; report its state.".into(),
                delay_seconds: 10,
                interval_seconds: 10,
                max_checks: checks,
            },
            0,
        )
        .await[0]
            .id
    }
    async fn result(&self, run: i64, status: &str, now: i64) {
        self.store
            .scheduled_result(
                &self.host.id,
                &DispatchResult {
                    run_id: run,
                    status: status.into(),
                    detail: "build checked".into(),
                    permission_id: None,
                },
                now,
            )
            .await
            .unwrap();
    }
    async fn inject(&self, delivery: &openwebide_core::scheduled::TaskDelivery, now: i64) {
        self.store
            .session_run_lease(
                self.user,
                self.session,
                &format!("scheduled-{}", delivery.run_id),
                false,
                now,
            )
            .await
            .unwrap();
        self.store
            .consume_queued_prompt(
                self.user,
                self.session,
                delivery.prompt.key(),
                &delivery.prompt.content,
                now,
            )
            .await
            .unwrap();
    }
}
#[test]
fn monitor_hosts_share_delivery_repeats_ownership_and_cleanup() {
    futures::executor::block_on(async {
        for mode in [
            None,
            Some(WorkspaceMode::Remote),
            Some(WorkspaceMode::Local),
        ] {
            let f = Fixture::new(mode).await;
            if mode == Some(WorkspaceMode::Local) {
                assert!(
                    f.store
                        .scheduled_session_command(
                            f.user,
                            f.session,
                            &TaskCommand::Monitor {
                                session_id: 0,
                                command: MonitorCommand::Start {
                                    prompt: "check".into(),
                                    delay_seconds: 10,
                                    interval_seconds: 10,
                                    max_checks: 2
                                }
                            },
                            0
                        )
                        .await
                        .is_err()
                );
                f.authorize().await;
            }
            let id = f.start(2).await;
            assert!(
                f.store
                    .scheduled_tasks(f.user, f.project, 0)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(
                f.store
                    .scheduled_session_command(
                        f.other,
                        f.session,
                        &TaskCommand::Monitor {
                            session_id: 0,
                            command: MonitorCommand::List {}
                        },
                        0
                    )
                    .await
                    .is_err()
            );
            assert!(
                f.store
                    .scheduled_session_command(
                        f.user,
                        f.sibling,
                        &TaskCommand::Monitor {
                            session_id: f.session,
                            command: MonitorCommand::List {}
                        },
                        0
                    )
                    .await
                    .is_err()
            );
            assert!(
                f.store
                    .scheduled_session_command(
                        f.user,
                        f.sibling,
                        &TaskCommand::Monitor {
                            session_id: 0,
                            command: MonitorCommand::Cancel { id, revision: 1 }
                        },
                        0
                    )
                    .await
                    .is_err()
            );
            assert!(
                f.store
                    .due_scheduled(
                        &ExecutionHost {
                            id: "wrong".into(),
                            name: "Other".into(),
                            last_seen: 0
                        },
                        10
                    )
                    .await
                    .unwrap()
                    .is_empty()
            );
            let d = f.store.due_scheduled(&f.host, 10).await.unwrap().remove(0);
            assert!(d.prompt.content.contains("[Monitor #"));
            assert_eq!(
                d.binding.as_ref().map(|b| b.path.as_str()),
                if mode == Some(WorkspaceMode::Local) {
                    Some("/authorized/project")
                } else {
                    None
                }
            );
            f.inject(&d, 11).await;
            assert!(
                f.store
                    .scheduled_session_command(
                        f.user,
                        f.session,
                        &TaskCommand::Monitor {
                            session_id: 0,
                            command: MonitorCommand::Start {
                                prompt: "extend".into(),
                                delay_seconds: 10,
                                interval_seconds: 10,
                                max_checks: 1
                            }
                        },
                        12
                    )
                    .await
                    .is_err()
            );
            f.result(d.run_id, "complete", 12).await;
            // A repeated completion callback must not shift the next check.
            f.result(d.run_id, "complete", 18).await;
            assert_eq!(
                f.command(MonitorCommand::List {}, 18).await[0].next_run,
                Some(22)
            );
            f.store
                .session_run_lease(
                    f.user,
                    f.session,
                    &format!("scheduled-{}", d.run_id),
                    true,
                    18,
                )
                .await
                .unwrap();
            assert!(f.store.due_scheduled(&f.host, 21).await.unwrap().is_empty());
            let second = f.store.due_scheduled(&f.host, 22).await.unwrap().remove(0);
            f.inject(&second, 23).await;
            f.result(second.run_id, "complete", 24).await;
            assert!(f.command(MonitorCommand::List {}, 24).await.is_empty());
            assert_eq!(
                f.store.list_messages(f.session).await.unwrap().len(),
                2,
                "injected conversation is preserved"
            );
            assert!(
                f.store
                    .db
                    .execute("PRAGMA foreign_key_check", &[])
                    .await
                    .unwrap()
                    .rows
                    .is_empty()
            );
        }
    });
}
#[test]
fn only_the_user_can_authorize_a_monitor_host_and_invalid_limits_leave_no_jobs() {
    futures::executor::block_on(async {
        let f = Fixture::new(Some(WorkspaceMode::Local)).await;
        let list = TaskCommand::Monitor {
            session_id: f.session,
            command: MonitorCommand::List {},
        };
        let binding = HostBinding {
            host_id: f.host.id.clone(),
            path: "/authorized/project".into(),
        };
        assert!(
            f.store
                .scheduled_command(f.user, f.project, &list, Some(&binding), true, 0)
                .await
                .is_err()
        );
        f.store
            .scheduled_command(f.user, f.project, &list, Some(&binding), false, 0)
            .await
            .unwrap();
        f.start(1).await;
        for (delay_seconds, interval_seconds, max_checks) in
            [(0, 10, 1), (10, 0, 2), (10, 10, 25), (86400, 10, 2)]
        {
            let command = TaskCommand::Monitor {
                session_id: 0,
                command: MonitorCommand::Start {
                    prompt: "check".into(),
                    delay_seconds,
                    interval_seconds,
                    max_checks,
                },
            };
            assert!(
                f.store
                    .scheduled_session_command(f.user, f.session, &command, 0)
                    .await
                    .is_err()
            );
        }
        assert_eq!(f.command(MonitorCommand::List {}, 0).await.len(), 1);
        for _ in 1..20 {
            f.start(1).await;
        }
        assert!(
            f.store
                .scheduled_session_command(
                    f.user,
                    f.session,
                    &TaskCommand::Monitor {
                        session_id: 0,
                        command: MonitorCommand::Start {
                            prompt: "check".into(),
                            delay_seconds: 10,
                            interval_seconds: 10,
                            max_checks: 1
                        }
                    },
                    0
                )
                .await
                .is_err()
        );
    });
}

#[test]
fn monitor_claim_recovery_never_replays_an_injected_prompt() {
    futures::executor::block_on(async {
        for mode in [
            None,
            Some(WorkspaceMode::Remote),
            Some(WorkspaceMode::Local),
        ] {
            let f = Fixture::new(mode).await;
            f.authorize().await;
            f.start(2).await;
            let first = f.store.due_scheduled(&f.host, 10).await.unwrap().remove(0);
            let recovered = f.store.due_scheduled(&f.host, 131).await.unwrap().remove(0);
            assert_eq!(first.run_id, recovered.run_id);
            assert_eq!(first.prompt.key(), recovered.prompt.key());
            f.inject(&recovered, 132).await;
            f.result(recovered.run_id, "blocked", 133).await;
            assert_eq!(
                f.command(MonitorCommand::List {}, 134).await[0]
                    .last_run
                    .as_ref()
                    .unwrap()
                    .status,
                "blocked"
            );
            assert!(
                f.store
                    .due_scheduled(&f.host, 254)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(f.command(MonitorCommand::List {}, 255).await.is_empty());
            let messages = f.store.list_messages(f.session).await.unwrap();
            assert_eq!(messages.len(), 2);
            assert!(messages[1].content.contains("interrupted"));
        }
    });
}
#[test]
fn monitor_cancel_expiry_and_failure_preserve_results_without_saved_tasks() {
    futures::executor::block_on(async {
        for mode in [
            None,
            Some(WorkspaceMode::Remote),
            Some(WorkspaceMode::Local),
        ] {
            let f = Fixture::new(mode).await;
            f.authorize().await;
            let id = f.start(2).await;
            assert!(
                f.command(MonitorCommand::Cancel { id, revision: 1 }, 5)
                    .await
                    .is_empty()
            );
            assert!(f.store.due_scheduled(&f.host, 10).await.unwrap().is_empty());
            f.start(2).await;
            let d = f.store.due_scheduled(&f.host, 10).await.unwrap().remove(0);
            f.result(d.run_id, "failed", 11).await;
            assert!(f.command(MonitorCommand::List {}, 12).await.is_empty());
            f.start(1).await;
            assert!(
                f.store
                    .due_scheduled(&f.host, 86400)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(f.command(MonitorCommand::List {}, 86400).await.is_empty());
            let messages = f.store.list_messages(f.session).await.unwrap();
            assert_eq!(messages.len(), 3);
            for (message, status) in messages.iter().zip(["cancelled", "failed", "expired"]) {
                assert!(message.content.contains(status));
            }
        }
    });
}
