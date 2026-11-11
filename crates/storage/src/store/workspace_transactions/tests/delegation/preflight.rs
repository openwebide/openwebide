use super::*;

#[test]
fn private_bootstrap_reads_original_scope_and_state_without_accepting_a_worker() {
    block_on(async {
        let store = Store::new(RusqliteDb::open_in_memory().unwrap());
        let (fixture, scope) = setup(&store, WorkspaceMode::Local).await;
        let actor = worker(1);
        let delegation = grant(&store, &fixture, &scope, &actor, 100).await;
        let mut request = call(&delegation, &actor, Operation::Inspect);
        let WorkspaceJournalResponse::Scope {
            scope: Some(inspected),
        } = store
            .delegated_workspace_journal(&request, 101)
            .await
            .unwrap()
        else {
            panic!("Missing private preflight scope")
        };
        assert_eq!(inspected, scope);
        assert!(serde_json::to_vec(&inspected).unwrap().len() < 1024);
        request.operation = Operation::Load;
        let WorkspaceJournalResponse::Loaded { state: Some(state) } = store
            .delegated_workspace_journal(&request, 101)
            .await
            .unwrap()
        else {
            panic!("Missing private preflight state")
        };
        assert!(scope.matches_state(&state));
        assert_eq!(state.journal.plan, fixture.plan);
        assert_eq!(
            store
                .workspace_transaction_scope(fixture.user, fixture.project.id, &scope.id)
                .await
                .unwrap(),
            Some(scope.clone())
        );
        request.operation = Operation::Scope;
        assert!(
            store
                .delegated_workspace_journal(&request, 101)
                .await
                .is_err()
        );
        for operation in [Operation::Inspect, Operation::Load] {
            request.operation = operation;
            let mut forged = request.clone();
            forged.worker.secret = "f".repeat(32);
            assert!(
                store
                    .delegated_workspace_journal(&forged, 101)
                    .await
                    .is_err()
            );
            let mut other = request.clone();
            other.delegation.user = fixture.other.get();
            assert!(
                store
                    .delegated_workspace_journal(&other, 101)
                    .await
                    .is_err()
            );
            assert!(
                store
                    .delegated_workspace_journal(&request, 146)
                    .await
                    .is_err()
            );
        }
        store
            .db
            .execute(
                "UPDATE workspace_transactions SET plan='corrupt large snapshot'",
                &[],
            )
            .await
            .unwrap();
        request.operation = Operation::Inspect;
        assert!(
            store
                .delegated_workspace_journal(&request, 102)
                .await
                .is_ok()
        );
        request.operation = Operation::Load;
        assert!(
            store
                .delegated_workspace_journal(&request, 102)
                .await
                .is_err()
        );
        store
            .advance_workspace_transaction(
                fixture.user,
                fixture.project.id,
                &AdvanceWorkspaceTransaction {
                    id: scope.id.clone(),
                    generation: scope.generation,
                    revision: scope.revision,
                    progress: TransactionProgress::prepared_cancellation(),
                },
                103,
            )
            .await
            .unwrap();
        for operation in [Operation::Inspect, Operation::Load] {
            request.operation = operation;
            assert!(
                store
                    .delegated_workspace_journal(&request, 104)
                    .await
                    .is_err()
            );
        }
    });
}
