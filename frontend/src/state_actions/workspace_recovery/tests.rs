use super::*;
use crate::{
    backend::Backend,
    state::{chat::ChatState, editor_recovery::EditorRecoveryState, settings::Theme},
    testing::fake_backend::FakeBackend,
};
use wasm_bindgen::{JsCast, prelude::wasm_bindgen};

#[wasm_bindgen(
    inline_js = "export async function recoveryTestRoot() { return navigator.storage.getDirectory(); }"
)]
extern "C" {
    async fn recoveryTestRoot() -> wasm_bindgen::JsValue;
}
use openwebide_core::{
    User, UserId, UserRole, WorkspaceMode,
    plugins::languages::intelligence::actions::{
        ActionPhase, ActionRecovery, ActionRecoveryRequest, CommandInvocation, CommandOutcome,
        CommandRecoveryScope,
    },
    workspace_transaction::WorkspaceTransactionWorkerState,
};

fn command(phase: ActionPhase, pending: bool) -> ActionRecovery {
    let outcome = match phase {
        ActionPhase::Completed => Some(CommandOutcome::Completed {
            result: serde_json::Value::Null,
        }),
        ActionPhase::Acknowledged => Some(CommandOutcome::Uncertain {
            message: "Lost response".into(),
        }),
        ActionPhase::Cancelled => None,
        _ => unreachable!(),
    };
    let result = ActionRecovery {
        command: CommandRecoveryScope {
            invocation: CommandInvocation {
                id: "a".repeat(32),
                client: "original-window".into(),
                command: "b".repeat(32),
                server: "old-service".into(),
                origin: "c".repeat(64),
                review: "d".repeat(64),
                approval: "e".repeat(64),
            },
            revision: 4,
            phase,
            outcome_digest: outcome.as_ref().map(|value| {
                openwebide_core::plugins::content_digest(&serde_json::to_vec(value).unwrap())
            }),
            worker: Some(WorkspaceTransactionWorkerState {
                id: "f".repeat(32),
                active: pending,
            }),
            native_settlement_pending: pending,
            command_cleanup_confirmed: true,
        },
        outcome,
        acknowledgement: (phase == ActionPhase::Acknowledged)
            .then(|| "Reviewed original effects".into()),
    };
    result.validate().unwrap();
    result
}

fn fixture(mode: WorkspaceMode) -> (Owner, Context, Rc<FakeBackend>) {
    crate::testing::initialize_runtime();
    let owner = Owner::new();
    let (context, backend) = owner.with(|| {
        let backend = Rc::new(FakeBackend::default());
        let api: Api = StoredValue::new_local(backend.clone() as Rc<dyn Backend>);
        let auth = AuthState::new();
        auth.user.set(Some(User {
            id: UserId::new(1),
            username: "owner".into(),
            role: UserRole::Admin,
            created_at: 0,
        }));
        let projects = ProjectsState::new();
        projects.projects.set(vec![Project {
            id: 1,
            name: "fixture".into(),
            mode,
            path: Some("test".into()),
            user_id: Some(UserId::new(1)),
            created_at: 0,
        }]);
        projects.projects_loaded.set(true);
        projects.active_project.set(Some(1));
        let workspace = WorkspaceState::with_active_project(projects.active_project);
        let settings = SettingsState::new(Theme::Dark, "http://localhost:3001".into());
        let ui = UiState::new();
        let read_only = RwSignal::new(false);
        provide_context(api);
        provide_context(auth);
        provide_context(ChatState::new());
        provide_context(projects);
        provide_context(workspace);
        provide_context(settings);
        provide_context(ui);
        let files = WorkspaceActions::new(
            api,
            projects,
            workspace,
            ui,
            read_only,
            Callback::new(|()| ()),
        );
        provide_context(files);
        let recovery = RecoveryActions::new(
            api,
            auth,
            projects,
            workspace,
            EditorRecoveryState::new(),
            read_only,
            Callback::new(|_: String| ()),
        );
        let context = Context {
            api,
            auth,
            projects,
            workspace,
            settings,
            recovery,
            files,
            tickets: StoredValue::new(HashMap::new()),
            counter: StoredValue::new(0),
            settlement_checks: StoredValue::new(HashMap::new()),
            original_folders: StoredValue::new(send_wrapper::SendWrapper::new(HashMap::new())),
            folder_access: RwSignal::new(HashSet::new()),
            missing_folders: RwSignal::new(HashSet::new()),
            ui,
            host: None,
        };
        (context, backend)
    });
    (owner, context, backend)
}

async fn wait(label: &str, condition: impl Fn() -> bool) {
    let deadline = js_sys::Date::now() + 4000.0;
    while !condition() {
        assert!(js_sys::Date::now() < deadline, "{label}");
        crate::util::sleep_ms(10).await;
    }
}
fn pending(context: Context) -> bool {
    context
        .workspace
        .workspace_recovery
        .with_untracked(|states| {
            matches!(
                states.get(&1),
                Some(WorkspaceRecoveryStatus::LanguageAction {
                    native_settlement_pending: true,
                    ..
                })
            )
        })
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn asynchronous_native_settlement_refreshes_recovery_without_replaying_commands_in_both_modes()
 {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for phase in [
            ActionPhase::Completed,
            ActionPhase::Acknowledged,
            ActionPhase::Cancelled,
        ] {
            let (owner, context, backend) = fixture(mode);
            backend
                .language_recoveries
                .borrow_mut()
                .insert(1, command(phase, true));
            context.check(1);
            wait("Original native settlement blocks saves", || {
                pending(context)
            })
            .await;
            assert!(context.workspace.workspace_saves_blocked(1));
            backend
                .language_recoveries
                .borrow_mut()
                .insert(1, command(phase, false));
            wait("Native acknowledgement refreshes the editor", || {
                context
                    .workspace
                    .workspace_recovery
                    .with_untracked(|states| {
                        states.get(&1) == Some(&WorkspaceRecoveryStatus::Ready)
                    })
            })
            .await;
            assert!(!context.workspace.workspace_saves_blocked(1));
            assert_eq!(
                backend.workspace_transactions.latest_reads.get(),
                2,
                "Settlement observation repeatedly fetched the full file journal"
            );
            let requests = backend.language_recovery_requests.borrow();
            assert!(requests.iter().any(|(project, request)| *project == 1 && matches!(request, ActionRecoveryRequest::Load { id: Some(id) } if *id == "a".repeat(32))));
            assert!(
                requests
                    .iter()
                    .all(|(_, request)| matches!(request, ActionRecoveryRequest::Load { .. })),
                "Observation attempted a command mutation"
            );
            drop(requests);
            owner.cleanup();
        }
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn late_native_settlement_cannot_refresh_another_account_or_project_root_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for account in [false, true] {
            let (owner, context, backend) = fixture(mode);
            backend
                .language_recoveries
                .borrow_mut()
                .insert(1, command(ActionPhase::Completed, true));
            context.check(1);
            wait("Original settlement observation", || pending(context)).await;
            let (complete, response) = futures::channel::oneshot::channel();
            backend
                .language_recovery_results
                .borrow_mut()
                .push_back(response);
            wait("Compact settlement read started", || {
                backend
                    .language_recovery_requests
                    .borrow()
                    .iter()
                    .any(|(_, request)| {
                        matches!(request, ActionRecoveryRequest::Load { id: Some(_) })
                    })
            })
            .await;
            if account {
                context
                    .auth
                    .generation
                    .update(|generation| *generation += 1);
            } else if mode == WorkspaceMode::Local {
                let handle = recoveryTestRoot().await.unchecked_into();
                context.projects.local_handles.update(|handles| {
                    handles.insert(1, handle);
                });
            } else {
                context
                    .projects
                    .projects
                    .update(|projects| projects[0].path = Some("replacement".into()));
            }
            // Keep the original pending state so only the account/root guard,
            // rather than an unrelated status change, rejects the late reply.
            let original = context
                .workspace
                .workspace_recovery
                .with_untracked(|states| states.get(&1).cloned());
            assert!(pending(context));
            let reads = backend.workspace_transactions.latest_reads.get();
            let _ = complete.send(Ok(Some(command(ActionPhase::Completed, false))));
            crate::util::sleep_ms(50).await;
            assert_eq!(
                context
                    .workspace
                    .workspace_recovery
                    .with_untracked(|states| states.get(&1).cloned()),
                original
            );
            assert_eq!(
                backend.workspace_transactions.latest_reads.get(),
                reads,
                "A stale response started a fresh recovery check"
            );
            owner.cleanup();
        }
    }
}
