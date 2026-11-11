use super::*;

fn scope() -> Scope {
    Scope {
        owner: "owner".into(),
        project: 1,
        root: PathBuf::from("project"),
        provider: "community/notes".into(),
        digest: "a".repeat(64),
        server: serde_json::from_value(json!({"command":"notes-lsp"})).unwrap(),
    }
}
fn invocation(index: u128) -> CommandInvocation {
    CommandInvocation {
        id: "a".repeat(32),
        client: "client".into(),
        command: format!("{index:032x}"),
        server: "original-session".into(),
        origin: "b".repeat(64),
        review: "c".repeat(64),
        approval: "d".repeat(64),
    }
}
#[test]
fn receipts_keep_results_without_retaining_services_and_enforce_every_claim_boundary() {
    let mut store = Outcomes::default();
    let scope = scope();
    let input = invocation(1);
    let context = CommandScope::new(&scope, &input).unwrap();
    let entry = Arc::new(Entry::new());
    let weak = Arc::downgrade(&entry);
    let (progress, _) = watch::channel(CommandProgress::Running);
    assert!(matches!(
        store.record(&context, &input, &entry, &progress).unwrap(),
        Admission::New
    ));
    let outcome = CommandOutcome::Completed {
        result: json!({"marker":17}),
    };
    Completion(progress.clone()).finish(outcome.clone());
    drop(entry);
    assert!(weak.upgrade().is_none());
    assert_eq!(
        store.get(&context, &input, false).unwrap(),
        Some(CommandProgress::Finished { outcome })
    );
    assert!(matches!(
        store
            .record(&context, &input, &Arc::new(Entry::new()), &progress)
            .unwrap(),
        Admission::Existing(CommandProgress::Finished { .. })
    ));
    for boundary in [
        "owner", "project", "provider", "digest", "config", "client", "server", "command", "id",
        "approval", "origin", "review",
    ] {
        let mut other_scope = scope.clone();
        let mut other = input.clone();
        match boundary {
            "owner" => other_scope.owner = "other".into(),
            "project" => other_scope.project += 1,
            "provider" => other_scope.provider = "other".into(),
            "digest" => other_scope.digest = "e".repeat(64),
            "config" => other_scope.server.settings = json!({"other":true}),
            "client" => other.client = "other".into(),
            "server" => other.server = "other".into(),
            "command" => other.command = "f".repeat(32),
            "id" => other.id = "f".repeat(32),
            "approval" => other.approval = "f".repeat(64),
            "origin" => other.origin = "f".repeat(64),
            "review" => other.review = "f".repeat(64),
            _ => unreachable!(),
        }
        let context = CommandScope::new(&other_scope, &other).unwrap();
        for stop in [false, true] {
            assert!(
                !matches!(store.get(&context, &other, stop), Ok(Some(_))),
                "{boundary}"
            );
        }
    }
}
#[test]
fn pending_receipts_pin_reserved_outcome_capacity_and_expiry_releases_terminal_records() {
    let scope = scope();
    let entry = Arc::new(Entry::new());
    let mut store = Outcomes::default();
    let mut senders = Vec::new();
    for index in 0..MAX_OUTCOMES + 1 {
        let input = invocation(index as u128);
        let context = CommandScope::new(&scope, &input).unwrap();
        let (progress, _) = watch::channel(CommandProgress::Running);
        if matches!(
            store.record(&context, &input, &entry, &progress).unwrap(),
            Admission::Full
        ) {
            break;
        }
        senders.push(progress);
    }
    assert!(!senders.is_empty());
    assert!(store.records.len() <= MAX_OUTCOMES);
    assert!(
        store
            .records
            .values()
            .map(|record| record.reserved)
            .sum::<usize>()
            <= MAX_OUTCOME_BYTES
    );
    let pending = store.records.len();
    for record in store.records.values_mut() {
        record.created = Instant::now() - COMMAND_LIFETIME - Duration::from_secs(1);
    }
    store.prune();
    assert_eq!(store.records.len(), pending);
    let input = invocation(999);
    let context = CommandScope::new(&scope, &input).unwrap();
    let (progress, _) = watch::channel(CommandProgress::Running);
    assert!(matches!(
        store.record(&context, &input, &entry, &progress).unwrap(),
        Admission::Full
    ));
    senders[0].send_replace(CommandProgress::Finished {
        outcome: CommandOutcome::Rejected {
            message: "not dispatched".into(),
        },
    });
    store.prune();
    assert_eq!(store.records.len(), pending - 1);
    assert!(matches!(
        store.record(&context, &input, &entry, &progress).unwrap(),
        Admission::New
    ));
}
#[test]
fn unpolled_worker_drop_and_oversized_failures_leave_valid_bounded_receipts() {
    let (progress, result) = watch::channel(CommandProgress::Running);
    let completion = Completion(progress.clone());
    let future = async move {
        std::future::pending::<()>().await;
        completion.finish(CommandOutcome::Completed {
            result: Value::Null,
        });
    };
    drop(future);
    assert!(matches!(
        *result.borrow(),
        CommandProgress::Finished {
            outcome: CommandOutcome::Uncertain { .. }
        }
    ));
    for outcome in [
        CommandOutcome::Rejected {
            message: "x".repeat(16 * 1024 + 1),
        },
        CommandOutcome::Completed {
            result: json!("x".repeat(protocol::actions::MAX_COMMAND_BYTES)),
        },
    ] {
        let rejected = matches!(outcome, CommandOutcome::Rejected { .. });
        progress.send_replace(CommandProgress::Running);
        Completion(progress.clone()).finish(outcome);
        let CommandProgress::Finished { outcome } = result.borrow().clone() else {
            panic!("missing terminal outcome")
        };
        outcome.validate().unwrap();
        assert_eq!(matches!(outcome, CommandOutcome::Rejected { .. }), rejected);
    }
}
