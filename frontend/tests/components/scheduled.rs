use super::support::{mount_test, settle};
use leptos::prelude::*;
use wasm_bindgen_test::*;
#[wasm_bindgen_test]
async fn calendar_task_ui_creates_pauses_edits_and_keeps_projectless_scope() {
    for projectless in [false, true] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.seed_session();
            state.seed_scheduling_plugin();
            if projectless {
                state.projects.active_project.set(None);
                state
                    .chat
                    .sessions
                    .update(|sessions| sessions[0].project_id = None);
            }
            view! {<style>{include_str!("../../styles.css")}</style><openwebide_frontend::components::scheduled::ScheduledTasks on_select=Callback::new(|_|())/>}
        });
        settle().await;
        mounted.click("[aria-label='New task']");
        settle().await;
        let state = mounted.state.scheduled;
        state.title.set("Review changes".into());
        state
            .prompt
            .set("Review the changes since yesterday".into());
        state.session.set(1);
        state
            .session_target
            .set(openwebide_core::scheduled::SessionTarget::Existing);
        state.time.set("09:30".into());
        state.days.set(vec![1, 3, 5]);
        mounted.click_text("Save task");
        settle().await;
        let task = mounted.state.fake.scheduled.borrow()[0].clone();
        assert_eq!(task.project_id, if projectless { None } else { Some(1) });
        assert!(
            matches!(task.draft.schedule,openwebide_core::scheduled::Schedule::Cron{expression,..} if expression=="30 9 * * 1,3,5")
        );
        assert!(
            mounted
                .root
                .query_selector(".scheduled-tasks .ui-disclosure-toggle")
                .unwrap()
                .is_some(),
            "Scheduled task should be visible in this filter"
        );
        mounted.click(".scheduled-tasks .ui-disclosure-toggle");
        mounted.click("[aria-label='Pause task']");
        settle().await;
        mounted.click_text("Paused");
        settle().await;
        assert!(
            mounted
                .root
                .query_selector(".scheduled-tasks .ui-disclosure-toggle")
                .unwrap()
                .is_some(),
            "Scheduled task should be visible in this filter"
        );
        mounted.click(".scheduled-tasks .ui-disclosure-toggle");
        mounted.click_text("Edit");
        settle().await;
        mounted.click_text("One time");
        settle().await;
        assert_eq!(state.kind.get_untracked(), "once");
        state.date.set("2030-01-01T12:00".into());
        state.enabled.set(true);
        mounted.click_text("Save task");
        settle().await;
        assert!(matches!(
            mounted.state.fake.scheduled.borrow()[0].draft.schedule,
            openwebide_core::scheduled::Schedule::Once { .. }
        ));
        mounted.click_text("Active");
        settle().await;
        assert!(
            mounted
                .root
                .query_selector(".scheduled-tasks .ui-disclosure-toggle")
                .unwrap()
                .is_some(),
            "Scheduled task should be visible in this filter"
        );
        mounted.click(".scheduled-tasks .ui-disclosure-toggle");
        mounted.click_text("Delete");
        settle().await;
        assert!(mounted.state.fake.scheduled.borrow().is_empty());
        assert!(
            mounted.state.fake.scheduled_commands.borrow().is_empty(),
            "Task controls must not call legacy scheduling endpoints"
        );
        assert_eq!(
            mounted
                .state
                .fake
                .plugin_action_calls
                .borrow()
                .iter()
                .map(|(_, call)| call.name.as_str())
                .collect::<Vec<_>>(),
            [
                "schedule_create",
                "schedule_set_enabled",
                "schedule_update",
                "schedule_delete"
            ]
        );
        assert!(
            mounted
                .state
                .fake
                .plugin_action_calls
                .borrow()
                .iter()
                .all(|(scope, _)| *scope == if projectless { None } else { Some(1) })
        );
    }
}
#[wasm_bindgen_test]
async fn task_refresh_discards_stale_projects_and_accounts() {
    let mounted = mount_test(|state| {
        state.seed_project();
        state.seed_session();
        view! {<openwebide_frontend::components::scheduled::ScheduledTasks on_select=Callback::new(|_|())/>}
    });
    settle().await;
    let (send, receive) = futures::channel::oneshot::channel();
    mounted
        .state
        .fake
        .scheduled_load_results
        .borrow_mut()
        .push_back(receive);
    mounted.state.task_actions.refresh.run(());
    settle().await;
    mounted.state.projects.active_project.set(None);
    settle().await;
    send.send(Err("stale project".into())).unwrap();
    settle().await;
    assert_ne!(
        mounted.state.scheduled.error.get_untracked().as_deref(),
        Some("stale project")
    );
    let (send, receive) = futures::channel::oneshot::channel();
    mounted
        .state
        .fake
        .scheduled_load_results
        .borrow_mut()
        .push_back(receive);
    mounted.state.task_actions.refresh.run(());
    settle().await;
    mounted
        .state
        .auth
        .generation
        .update(|generation| *generation += 1);
    settle().await;
    send.send(Err("stale account".into())).unwrap();
    settle().await;
    assert_ne!(
        mounted.state.scheduled.error.get_untracked().as_deref(),
        Some("stale account")
    );
}

#[wasm_bindgen_test]
async fn task_editor_uses_shared_tabs_session_options_and_can_always_be_dismissed() {
    use openwebide_core::scheduled::SessionTarget;
    let mounted = mount_test(|state| {
        state.seed_project();
        state.seed_session();
        view! {<openwebide_frontend::components::scheduled::ScheduledTasks on_select=Callback::new(|_|())/>}
    });
    settle().await;
    mounted.click("[aria-label='New task']");
    settle().await;
    assert!(mounted.root.query_selector("select").unwrap().is_none());
    assert_eq!(
        mounted.state.scheduled.session_target.get_untracked(),
        SessionTarget::Latest
    );
    mounted.click_text("Cron");
    settle().await;
    assert!(
        mounted
            .root
            .query_selector("[aria-label='Cron minute']")
            .unwrap()
            .is_some()
    );
    mounted.click_text("One time");
    settle().await;
    assert!(
        mounted
            .root
            .query_selector("input[type='datetime-local']")
            .unwrap()
            .is_some()
    );
    mounted.click_text("Repeating");
    settle().await;
    assert!(
        mounted
            .root
            .query_selector("input[type='time']")
            .unwrap()
            .is_some()
    );
    mounted.click("[aria-label='Session']");
    settle().await;
    mounted.click_text("New session each run");
    settle().await;
    assert_eq!(
        mounted.state.scheduled.session_target.get_untracked(),
        SessionTarget::New
    );
    mounted.state.scheduled.busy.set(true);
    mounted.click_text("Cancel");
    settle().await;
    assert!(!mounted.state.scheduled.editing.get_untracked());
    assert!(mounted.state.fake.scheduled_commands.borrow().is_empty());
}

#[wasm_bindgen_test]
async fn cron_fields_support_full_paste_and_preserve_empty_segments() {
    use wasm_bindgen::JsCast;
    let mounted = mount_test(|state| {
        state.seed_project();
        state.seed_session();
        view! {<openwebide_frontend::components::scheduled::ScheduledTasks on_select=Callback::new(|_|())/>}
    });
    settle().await;
    mounted.click("[aria-label='New task']");
    settle().await;
    mounted.click_text("Cron");
    settle().await;
    let input = |label: &str| {
        mounted
            .root
            .query_selector(&format!("[aria-label='{label}']"))
            .unwrap()
            .unwrap()
            .unchecked_into::<web_sys::HtmlInputElement>()
    };
    let edit = |label: &str, text: &str| {
        let field = input(label);
        field.set_value(text);
        let init = web_sys::EventInit::new();
        init.set_bubbles(true);
        field
            .dispatch_event(&web_sys::Event::new_with_event_init_dict("input", &init).unwrap())
            .unwrap();
    };
    edit("Cron minute", "*/15 8-17 * JAN,MAR MON-FRI");
    settle().await;
    assert_eq!(input("Cron hour").value(), "8-17");
    assert_eq!(input("Cron month").value(), "JAN,MAR");
    assert_eq!(input("Cron day of week").value(), "MON-FRI");
    edit("Cron hour", "");
    settle().await;
    assert_eq!(input("Cron hour").value(), "");
    assert_eq!(input("Cron day of month").value(), "*");
    assert_eq!(
        mounted.state.scheduled.cron.get_untracked(),
        "*/15  * JAN,MAR MON-FRI"
    );
    edit("Cron hour", "9");
    settle().await;
    assert_eq!(
        mounted.state.scheduled.cron.get_untracked(),
        "*/15 9 * JAN,MAR MON-FRI"
    );
    mounted.state.scheduled.cron.set("0 12 * * 1-5".into());
    settle().await;
    assert_eq!(input("Cron minute").value(), "0");
    assert_eq!(input("Cron hour").value(), "12");
    assert_eq!(input("Cron day of week").value(), "1-5");
}

#[wasm_bindgen_test]
async fn task_model_dropdown_saves_reopens_and_clears_override_in_every_scope() {
    use openwebide_core::{ModelInfo, ModelSelection, WorkspaceMode};
    for mode in [
        None,
        Some(WorkspaceMode::Local),
        Some(WorkspaceMode::Remote),
    ] {
        let mounted = mount_test(move |state| {
            state.seed_project();
            state.seed_scheduling_plugin();
            state.seed_session();
            state.seed_connection();
            if let Some(mode) = mode {
                state
                    .projects
                    .projects
                    .update(|projects| projects[0].mode = mode);
            } else {
                state.projects.active_project.set(None);
                state
                    .chat
                    .sessions
                    .update(|sessions| sessions[0].project_id = None);
            }
            *state.fake.models.borrow_mut() = vec![ModelInfo {
                name: "task-model".into(),
            }];
            view! {<openwebide_frontend::components::scheduled::ScheduledTasks on_select=Callback::new(|_|())/>}
        });
        settle().await;
        mounted.click("[aria-label='New task']");
        settle().await;
        assert!(mounted.root.query_selector("select").unwrap().is_none());
        assert!(
            mounted
                .element("[aria-label='Task model']")
                .text_content()
                .unwrap()
                .contains("Current session model")
        );
        mounted.click("[aria-label='Task model']");
        settle().await;
        mounted.click_text("task-model @ Ollama");
        let selected = ModelSelection {
            server_id: 1,
            model: "task-model".into(),
        };
        assert_eq!(
            mounted.state.scheduled.model.get_untracked(),
            Some(selected.clone())
        );
        mounted.state.scheduled.title.set("Task".into());
        mounted.state.scheduled.prompt.set("Do work".into());
        mounted.click_text("Save task");
        settle().await;
        if mode == Some(WorkspaceMode::Local) {
            // Unpaired local folders retain the editor choice and require a host before saving.
            assert!(mounted.state.fake.scheduled.borrow().is_empty());
            assert!(mounted.state.scheduled.error.get_untracked().is_some());
            assert_eq!(
                mounted.state.scheduled.model.get_untracked(),
                Some(selected)
            );
            continue;
        }
        assert_eq!(
            mounted.state.fake.scheduled.borrow()[0].draft.model,
            Some(selected.clone())
        );
        mounted.click(".scheduled-tasks .ui-disclosure-toggle");
        mounted.click_text("Edit");
        settle().await;
        assert_eq!(
            mounted.state.scheduled.model.get_untracked(),
            Some(selected)
        );
        mounted.click("[aria-label='Task model']");
        settle().await;
        mounted.click_text("Current session model");
        mounted.click_text("Save task");
        settle().await;
        assert!(
            mounted.state.fake.scheduled.borrow()[0]
                .draft
                .model
                .is_none()
        );
    }
}
