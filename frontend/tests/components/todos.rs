use super::support::{Mounted, chat_view, mount_test, settle};
use leptos::prelude::*;
use openwebide_core::{
    ChatCompletion, ChatResponse, Role, RunEvent, StopReason, TodoItem, TodoPlan, TodoStatus,
    TodoUpdate, ToolCall, WorkspaceMode,
};
use openwebide_frontend::{backend::Backend, util::sleep_ms};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::*;

fn plan(status: TodoStatus) -> TodoPlan {
    TodoPlan {
        todos: vec![
            TodoItem {
                id: "inspect".into(),
                content: "Inspect the code".into(),
                status,
            },
            TodoItem {
                id: "verify".into(),
                content: "Verify the result".into(),
                status: TodoStatus::Pending,
            },
        ],
    }
}
fn update(session: i64, anchor: i64, status: TodoStatus) -> TodoUpdate {
    TodoUpdate {
        id: 1,
        session_id: session,
        anchor_message_id: anchor,
        created_at: 0,
        plan: plan(status),
    }
}
fn fixture(mode: Option<WorkspaceMode>) -> Mounted {
    mount_test(move |state| {
        if let Some(mode) = mode {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            if mode == WorkspaceMode::Local {
                state.projects.local_handles.update(|handles| {
                    handles.insert(1, super::local_bridge::probe_folder().unchecked_into());
                });
            }
        }
        state.seed_connection();
        state.seed_session();
        state.fake.connections.borrow_mut()[0].context_limit = Some(32768);
        state
            .settings
            .connections
            .update(|connections| connections[0].context_limit = Some(32768));
        if mode.is_none() {
            state
                .chat
                .sessions
                .update(|sessions| sessions[0].project_id = None);
            state.fake.sessions.borrow_mut()[0].project_id = None;
        }
        view! { {chat_view(state)} <openwebide_frontend::components::ConfirmDialog/> }
    })
}
async fn idle(mounted: &Mounted) {
    for _ in 0..300 {
        sleep_ms(5).await;
        settle().await;
        if !mounted.state.chat.streaming.get_untracked()
            && !mounted.state.chat.todo_loading.get_untracked()
            && !mounted.state.chat.branching.get_untracked()
            && !mounted.state.chat.rewinding.get_untracked()
        {
            return;
        }
    }
    panic!(
        "run/plan loading did not settle: {:?}",
        mounted.state.chat.error.get_untracked()
    );
}
#[wasm_bindgen_test]
async fn local_agent_writes_the_persisted_checklist_and_failed_writes_keep_the_previous_plan() {
    let mounted = fixture(Some(WorkspaceMode::Local));
    settle().await;
    let write = ChatCompletion {
        response: ChatResponse::ToolCalls(vec![ToolCall {
            id: "plan".into(),
            name: "todo_write".into(),
            arguments: serde_json::to_string(&plan(TodoStatus::InProgress)).unwrap(),
        }]),
        preamble: String::new(),
        reasoning: String::new(),
        stop_reason: StopReason::Complete,
        usage: None,
    };
    // The full catalog uses discovery at this context limit. Load the
    // optional checklist schema before asking the scripted model to use it.
    let discover = ChatCompletion {
        response: ChatResponse::ToolCalls(vec![ToolCall {
            id: "discover-plan".into(),
            name: "tool_search".into(),
            arguments: r#"{"query":"select:todo_write"}"#.into(),
        }]),
        ..write.clone()
    };
    let reply = ChatCompletion {
        response: ChatResponse::Text("done".into()),
        preamble: String::new(),
        reasoning: String::new(),
        stop_reason: StopReason::Complete,
        usage: None,
    };
    mounted
        .state
        .fake
        .scripted_completions
        .borrow_mut()
        .extend([discover.clone(), write.clone(), reply.clone()]);
    mounted.input("make a plan");
    mounted.key("Enter", "Enter", false);
    idle(&mounted).await;
    assert!(mounted.state.chat.error.get_untracked().is_none());
    assert_eq!(
        mounted
            .state
            .chat
            .todo_plan
            .get_untracked()
            .unwrap_or_else(|| {
                panic!(
                    "Checklist missing after run: {:?}",
                    mounted
                        .state
                        .chat
                        .messages
                        .snapshot()
                        .into_iter()
                        .filter(|item| matches!(
                            item,
                            openwebide_frontend::conversation::ConversationItem::ToolStep { .. }
                        ))
                        .collect::<Vec<_>>()
                )
            })
            .plan,
        plan(TodoStatus::InProgress)
    );
    assert!(
        mounted
            .root
            .text_content()
            .unwrap()
            .contains("Plan · 0/2 completed")
    );
    assert!(
        mounted
            .element("[data-todo-id='inspect']")
            .class_name()
            .contains("active")
    );
    let saved = mounted.state.chat.todo_plan.get_untracked();
    mounted
        .state
        .fake
        .todo_errors
        .borrow_mut()
        .push_back("database unavailable".into());
    mounted
        .state
        .fake
        .scripted_completions
        .borrow_mut()
        .extend([discover, write, reply]);
    mounted.input("try another plan update");
    mounted.key("Enter", "Enter", false);
    idle(&mounted).await;
    assert_eq!(mounted.state.chat.todo_plan.get_untracked(), saved);
    assert!(
        mounted
            .state
            .fake
            .completion_requests
            .borrow()
            .last()
            .unwrap()
            .messages
            .iter()
            .any(|message| message.role == Role::Tool
                && message.content.contains("Could not update plan"))
    );
}

#[wasm_bindgen_test]
async fn remote_and_projectless_tool_results_refresh_the_same_pinned_checklist() {
    for mode in [Some(WorkspaceMode::Remote), None] {
        let mounted = fixture(mode);
        settle().await;
        let prompt = mounted
            .state
            .fake
            .persist_message(1, Role::User, "original", None, None)
            .await
            .unwrap();
        mounted
            .state
            .fake
            .write_todo_plan(1, prompt.id, &plan(TodoStatus::Completed))
            .await
            .unwrap();
        mounted
            .state
            .fake
            .scripted_events
            .borrow_mut()
            .push_back(vec![RunEvent::ToolResult {
                id: "plan".into(),
                name: "todo_write".into(),
                ok: true,
                summary: "updated".into(),
                diff: None,
            }]);
        mounted.input("show the plan");
        mounted.key("Enter", "Enter", false);
        idle(&mounted).await;
        assert_eq!(
            mounted.state.chat.todo_plan.get_untracked().unwrap().plan,
            plan(TodoStatus::Completed)
        );
        assert!(
            mounted
                .root
                .text_content()
                .unwrap()
                .contains("Plan · 1/2 completed")
        );
    }
}

#[wasm_bindgen_test]
async fn plan_loads_on_session_selection_and_stale_responses_do_not_replace_another_session_or_account()
 {
    for account_change in [false, true] {
        let mounted = fixture(None);
        settle().await;
        mounted
            .state
            .fake
            .todo_updates
            .borrow_mut()
            .insert(1, vec![update(1, 1, TodoStatus::Pending)]);
        mounted.state.chat.active_session.set(None);
        settle().await;
        mounted.state.chat.active_session.set(Some(1));
        idle(&mounted).await;
        assert!(mounted.state.chat.todo_plan.get_untracked().is_some());
        let (sender, receiver) = futures::channel::oneshot::channel();
        mounted
            .state
            .fake
            .todo_load_results
            .borrow_mut()
            .push_back(receiver);
        // Exercise the same facade used by tool-result/snapshot and retry callers.
        mounted.state.chat.todo_error.set(Some("retry".into()));
        settle().await;
        mounted.click("button[aria-label='Retry loading plan']");
        settle().await;
        if account_change {
            mounted.state.fake.todo_updates.borrow_mut().clear();
            mounted.state.auth.generation.update(|value| *value += 1);
        } else {
            mounted.state.chat.active_session.set(Some(2));
        }
        settle().await;
        sender
            .send(Ok(Some(update(1, 1, TodoStatus::Completed))))
            .unwrap();
        settle().await;
        assert!(mounted.state.chat.todo_plan.get_untracked().is_none());
        assert!(
            !mounted
                .root
                .text_content()
                .unwrap()
                .contains("Inspect the code")
        );
    }
}

#[wasm_bindgen_test]
async fn failed_plan_load_keeps_the_displayed_revision_and_retry_recovers() {
    let mounted = fixture(None);
    settle().await;
    mounted
        .state
        .fake
        .todo_updates
        .borrow_mut()
        .insert(1, vec![update(1, 1, TodoStatus::Pending)]);
    mounted.state.chat.active_session.set(None);
    settle().await;
    mounted.state.chat.active_session.set(Some(1));
    idle(&mounted).await;
    let previous = mounted.state.chat.todo_plan.get_untracked();
    let (sender, receiver) = futures::channel::oneshot::channel();
    mounted
        .state
        .fake
        .todo_load_results
        .borrow_mut()
        .push_back(receiver);
    mounted.state.chat.todo_error.set(Some("retry".into()));
    settle().await;
    mounted.click("button[aria-label='Retry loading plan']");
    settle().await;
    sender.send(Err("network unavailable".into())).unwrap();
    settle().await;
    assert_eq!(mounted.state.chat.todo_plan.get_untracked(), previous);
    assert!(
        mounted
            .root
            .text_content()
            .unwrap()
            .contains("network unavailable")
    );
    mounted.click("button[aria-label='Retry loading plan']");
    idle(&mounted).await;
    assert_eq!(mounted.state.chat.todo_plan.get_untracked(), previous);
    assert!(mounted.state.chat.todo_error.get_untracked().is_none());
}

#[wasm_bindgen_test]
async fn fork_and_rewind_restore_the_plan_before_the_selected_prompt() {
    let mounted = fixture(None);
    settle().await;
    let first = mounted
        .state
        .fake
        .persist_message(1, Role::User, "earlier", None, None)
        .await
        .unwrap();
    mounted
        .state
        .fake
        .write_todo_plan(1, first.id, &plan(TodoStatus::Pending))
        .await
        .unwrap();
    mounted
        .state
        .fake
        .persist_message(1, Role::Assistant, "earlier answer", None, None)
        .await
        .unwrap();
    let second = mounted
        .state
        .fake
        .persist_message(1, Role::User, "later", None, None)
        .await
        .unwrap();
    mounted
        .state
        .fake
        .write_todo_plan(1, second.id, &plan(TodoStatus::Completed))
        .await
        .unwrap();
    mounted
        .state
        .fake
        .persist_message(1, Role::Assistant, "later answer", None, None)
        .await
        .unwrap();
    mounted.state.chat.active_session.set(None);
    settle().await;
    mounted.state.chat.active_session.set(Some(1));
    idle(&mounted).await;
    assert_eq!(
        mounted.state.chat.todo_plan.get_untracked().unwrap().plan,
        plan(TodoStatus::Completed)
    );
    super::support::click_action(
        &mounted,
        &format!(".tui-fork-prompt[data-message-id='{}']", second.id),
    )
    .await;
    idle(&mounted).await;
    assert_ne!(mounted.state.chat.active_session.get_untracked(), Some(1));
    assert_eq!(
        mounted.state.chat.todo_plan.get_untracked().unwrap().plan,
        plan(TodoStatus::Pending)
    );
    assert_eq!(mounted.state.fake.todo_updates.borrow()[&1].len(), 2);
    mounted.state.chat.draft.set(String::new());
    mounted.state.chat.active_session.set(Some(1));
    idle(&mounted).await;
    super::support::click_action(
        &mounted,
        &format!(".tui-rewind[data-message-id='{}']", second.id),
    )
    .await;
    settle().await;
    mounted.click(".modal-footer .danger");
    idle(&mounted).await;
    assert_eq!(
        mounted.state.chat.todo_plan.get_untracked().unwrap().plan,
        plan(TodoStatus::Pending)
    );
    assert_eq!(mounted.state.fake.todo_updates.borrow()[&1].len(), 1);
}
