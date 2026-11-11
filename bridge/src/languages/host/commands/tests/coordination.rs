use super::*;
use crate::folder_coordination::Folder;
#[cfg(unix)]
use futures::FutureExt;

#[tokio::test]
async fn uncertain_receipts_survive_capacity_eviction_without_pinning_a_stopped_service() {
    let mut fixture = Fixture::new("cancel").await;
    let (review, invocation) = fixture.review().await;
    fixture.apply_overlay(&review).await;
    fixture
        .host
        .start_command(fixture.scope.clone(), invocation.clone())
        .await
        .unwrap();
    assert!(matches!(
        fixture.finished(&invocation).await,
        CommandOutcome::Uncertain { .. }
    ));
    for client in ["another-client", "third-client"] {
        let mut other = fixture.input.clone();
        other.client = client.into();
        other.request = 1;
        other.revision = 1;
        other.query = Query::Synchronize;
        fixture
            .host
            .query(fixture.scope.clone(), &other)
            .await
            .unwrap();
    }
    let retained = fixture
        .host
        .inner
        .entries
        .lock()
        .unwrap()
        .keys()
        .any(|key| key.client == fixture.input.client);
    assert!(!retained);
    assert!(matches!(
        fixture
            .host
            .command_progress(fixture.scope.clone(), &invocation)
            .await
            .unwrap(),
        CommandProgress::Finished {
            outcome: CommandOutcome::Uncertain { .. }
        }
    ));
    fixture
        .host
        .stop_command(fixture.scope.clone(), &invocation)
        .await
        .unwrap();
    assert_eq!(fixture.executions(), 1);
}

#[tokio::test]
async fn idle_reviewed_commands_keep_the_original_process_across_another_clients_arrival() {
    let mut fixture = Fixture::new("success").await;
    let (review, invocation) = fixture.review().await;
    let mut other = fixture.input.clone();
    other.client = "another-client".into();
    other.request = 1;
    other.revision = 1;
    other.source = "another draft".into();
    other.source_digest = source_digest(&other.source);
    other.query = Query::Synchronize;
    fixture
        .host
        .query(fixture.scope.clone(), &other)
        .await
        .unwrap();
    reuse::consolidate_idle(&mut fixture.host.inner.entries.lock().unwrap());
    // The next client reaches owner capacity. The unclaimed review still
    // keeps its exact process; only the other idle client may be replaced.
    other.client = "third-client".into();
    fixture
        .host
        .query(fixture.scope.clone(), &other)
        .await
        .unwrap();
    reuse::consolidate_idle(&mut fixture.host.inner.entries.lock().unwrap());
    {
        let entries = fixture.host.inner.entries.lock().unwrap();
        let pids = entries
            .values()
            .map(|entry| entry.session.get().unwrap().process.lock().unwrap().id())
            .collect::<Vec<_>>();
        assert_eq!(pids.len(), 2);
        assert_ne!(pids[0], pids[1]);
    }
    fixture.apply_overlay(&review).await;
    fixture
        .host
        .start_command(fixture.scope.clone(), invocation.clone())
        .await
        .unwrap();
    assert!(matches!(
        fixture.finished(&invocation).await,
        CommandOutcome::Completed { .. }
    ));
    assert_eq!(fixture.executions(), 1);
}

fn configured(fixture: &mut Fixture) -> crate::ServerConfig {
    let mut config =
        crate::ServerConfig::new(fixture.root.path().into(), "test-secret".into(), None);
    // Exercise the same limit-configuration path as the production binary.
    config
        .set_language_memory_limits(MemoryLimits::default())
        .unwrap();
    fixture.host = config.language_servers.clone();
    config
}
async fn queued(config: &crate::ServerConfig) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while config.workspace_transactions.coordinator.waiting() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn approved_commands_wait_for_the_shared_folder_and_recheck_changes_before_rpc() {
    for change in ["unchanged", "changed", "restored"] {
        let mut fixture = Fixture::new("success").await;
        std::fs::write(fixture.root.path().join("settings.json"), "original").unwrap();
        fixture.scope.server.project_inputs =
            Some(serde_json::from_value(json!({"files":["settings.json"]})).unwrap());
        let config = configured(&mut fixture);
        let (review, invocation) = fixture.review().await;
        fixture.apply_overlay(&review).await;
        let lease = config
            .workspace_transactions
            .coordinator
            .acquire(Folder::capture(fixture.root.path()).unwrap())
            .await
            .unwrap();
        fixture
            .host
            .start_command(fixture.scope.clone(), invocation.clone())
            .await
            .unwrap();
        queued(&config).await;
        assert_eq!(fixture.executions(), 0);
        if change != "unchanged" {
            // The older approval must be checked after its wait, not against
            // the file inventory observed before another owner held the lease.
            std::fs::write(fixture.root.path().join("settings.json"), "external change").unwrap();
            if change == "restored" {
                std::fs::write(fixture.root.path().join("settings.json"), "original").unwrap();
            }
        }
        drop(lease);
        let outcome = fixture.finished(&invocation).await;
        assert_eq!(
            matches!(outcome, CommandOutcome::Completed { .. }),
            change == "unchanged",
            "A {change} input crossed the queued command's original observation: {outcome:?}"
        );
        assert_eq!(fixture.executions(), usize::from(change == "unchanged"));
    }
}

#[tokio::test]
async fn queued_commands_can_be_stopped_without_waiting_for_the_busy_folder_or_replaying() {
    let mut fixture = Fixture::new("success").await;
    let config = configured(&mut fixture);
    let (review, invocation) = fixture.review().await;
    fixture.apply_overlay(&review).await;
    let lease = config
        .workspace_transactions
        .coordinator
        .acquire(Folder::capture(fixture.root.path()).unwrap())
        .await
        .unwrap();
    fixture
        .host
        .start_command(fixture.scope.clone(), invocation.clone())
        .await
        .unwrap();
    queued(&config).await;
    fixture
        .host
        .stop_command(fixture.scope.clone(), &invocation)
        .await
        .unwrap();
    assert!(matches!(
        fixture.finished(&invocation).await,
        CommandOutcome::Rejected { .. }
    ));
    assert_eq!(fixture.executions(), 0);
    drop(lease);
    fixture
        .host
        .start_command(fixture.scope.clone(), invocation)
        .await
        .unwrap();
    assert_eq!(fixture.executions(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn command_uncertainty_keeps_the_folder_until_the_original_process_group_is_fenced() {
    let mut fixture = Fixture::new("fork-block").await;
    let config = configured(&mut fixture);
    let (review, invocation) = fixture.review().await;
    fixture.apply_overlay(&review).await;
    fixture
        .host
        .start_command(fixture.scope.clone(), invocation.clone())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !fixture.root.path().join("child-ready").exists()
            || !fixture.root.path().join("heartbeat").exists()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut recovery = Box::pin(
        config
            .workspace_transactions
            .coordinator
            .acquire(Folder::capture(fixture.root.path()).unwrap()),
    );
    assert!(recovery.as_mut().now_or_never().is_none());
    let mut stop = Box::pin(
        fixture
            .host
            .stop_command(fixture.scope.clone(), &invocation),
    );
    // This poll initiates stop and reaches its cleanup wait before the 2s
    // process-group grace can expire. A lease released at signal delivery fails.
    assert!(stop.as_mut().now_or_never().is_none());
    assert!(recovery.as_mut().now_or_never().is_none());
    tokio::time::timeout(Duration::from_secs(5), stop)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        fixture.finished(&invocation).await,
        CommandOutcome::Uncertain { .. }
    ));
    let _recovery = tokio::time::timeout(Duration::from_secs(5), recovery)
        .await
        .unwrap()
        .unwrap();
    let final_bytes = std::fs::read(fixture.root.path().join("heartbeat")).unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(
        std::fs::read(fixture.root.path().join("heartbeat")).unwrap(),
        final_bytes
    );
    assert_eq!(fixture.executions(), 1);
}
