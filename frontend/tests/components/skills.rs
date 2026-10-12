use super::support::{Mounted, chat_view, mount_test, settle, wait_until};
use leptos::prelude::*;
use openwebide_core::{
    ChatCompletion, ChatResponse, ProjectSkill, ProjectSkills, SkillCommand, SkillDraft,
    StopReason, ToolCall, WorkspaceMode,
};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::*;
fn fixture(mode: WorkspaceMode, chat: bool) -> Mounted {
    mount_test(move |state| {
        state.seed_project();
        state.seed_connection();
        state.seed_session();
        if chat {
            let prepared = state.seed_skill_plugin();
            if mode == WorkspaceMode::Local {
                state
                    .settings
                    .bridge_url
                    .set("ws://bridge.test:3001".into());
                *state.fake.bridge_credential.borrow_mut() =
                    Some(("skills-test-token".into(), i64::MAX));
                let mock = super::local_bridge::fake_bridge_http();
                super::local_bridge::bridge_found(&mock, false);
                super::local_bridge::set_bridge_plugin(
                    &mock,
                    &serde_json::to_string(&prepared).unwrap(),
                );
                let mock = send_wrapper::SendWrapper::new(mock);
                on_cleanup(move || super::local_bridge::restore_bridge_http(&mock));
            }
        }
        state
            .projects
            .projects
            .update(|projects| projects[0].mode = mode);
        if mode == WorkspaceMode::Local {
            state.projects.local_handles.update(|handles| {
                handles.insert(1, super::local_bridge::probe_folder().unchecked_into());
            });
        }
        view! {<style>{include_str!("../../styles.css")}</style><openwebide_frontend::components::skills::Skills/>{chat.then(||chat_view(state))}}
    })
}
fn draft(name: &str) -> SkillDraft {
    SkillDraft {
        name: name.into(),
        description: "Check builds when reviewing changes".into(),
        instructions: "Run approved checks 🦀".into(),
        enabled: true,
        resources: Vec::new(),
        metadata: Default::default(),
    }
}
fn input(mounted: &Mounted, selector: &str, value: &str) {
    let element = mounted.element(selector);
    if let Some(input) = element.dyn_ref::<web_sys::HtmlInputElement>() {
        input.set_value(value);
    } else {
        element
            .dyn_ref::<web_sys::HtmlTextAreaElement>()
            .unwrap()
            .set_value(value);
    }
    element
        .dispatch_event(&web_sys::Event::new("input").unwrap())
        .unwrap();
}
#[wasm_bindgen_test]
async fn skills_ui_crud_resources_disabling_and_conflicts_work_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mounted = fixture(mode, false);
        settle().await;
        mounted.click("[aria-label='New skill']");
        settle().await;
        input(&mounted, "[aria-label='Skill name']", "build-check");
        input(
            &mounted,
            "[aria-label='Skill description']",
            "Check builds when reviewing changes",
        );
        input(
            &mounted,
            "[aria-label='Skill instructions']",
            "Run approved checks 🦀",
        );
        mounted.click_text("Add resource");
        settle().await;
        input(
            &mounted,
            "[aria-label='Skill resource name']",
            "references/check.md",
        );
        input(
            &mounted,
            "[aria-label='Skill resource content']",
            "cargo test",
        );
        mounted.click_text("Save skill");
        settle().await;
        assert_eq!(mounted.state.fake.skills.borrow()[&1].entries.len(), 1);
        mounted.click(".memory-entry .ui-disclosure-toggle");
        mounted.click_text("Export ZIP");
        assert!(mounted.state.skills.error.get_untracked().is_none());
        mounted.click_text("Edit");
        settle().await;
        input(
            &mounted,
            "[aria-label='Skill instructions']",
            "new instructions",
        );
        mounted
            .state
            .fake
            .skills
            .borrow_mut()
            .get_mut(&1)
            .unwrap()
            .entries[0]
            .revision += 1;
        mounted.click_text("Save skill");
        settle().await;
        assert!(
            mounted
                .element("[role=alert]")
                .text_content()
                .unwrap()
                .contains("changed")
        );
        assert!(mounted.state.skills.editing.get_untracked());
        assert_eq!(
            mounted.state.skills.draft.get_untracked().instructions,
            "new instructions"
        );
        mounted.click_text("Cancel");
        mounted.state.skill_actions.refresh.run(());
        settle().await;
        mounted.click_text("Edit");
        settle().await;
        mounted.click(".memory-editor .ui-check input");
        mounted.click_text("Save skill");
        settle().await;
        assert!(
            !mounted.state.fake.skills.borrow()[&1].entries[0]
                .draft
                .enabled
        );
        mounted.click_text("Disabled");
        settle().await;
        assert!(!mounted.state.fake.skills.borrow()[&1].enabled);
        assert_eq!(mounted.state.fake.skills.borrow()[&1].entries.len(), 1);
        mounted.click(".memory-entry .ui-disclosure-toggle");
        mounted.click_text("Delete");
        settle().await;
        assert!(mounted.state.fake.skills.borrow()[&1].entries.is_empty());
    }
}
#[wasm_bindgen_test]
async fn skills_stale_load_and_mutation_responses_cannot_cross_projects_accounts_or_sessions() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mounted = fixture(mode, false);
        settle().await;
        let (send, receive) = futures::channel::oneshot::channel();
        mounted
            .state
            .fake
            .skill_load_results
            .borrow_mut()
            .push_back(receive);
        mounted.state.skill_actions.refresh.run(());
        settle().await;
        mounted.state.projects.active_project.set(Some(2));
        settle().await;
        send.send(Ok(ProjectSkills {
            enabled: true,
            entries: vec![ProjectSkill {
                plugin: None,
                id: 1,
                revision: 1,
                updated_at: 0,
                draft: draft("old-project"),
            }],
        }))
        .unwrap();
        settle().await;
        assert!(!mounted.root.text_content().unwrap().contains("old-project"));
        mounted.state.projects.active_project.set(Some(1));
        settle().await;
        for account in [true, false] {
            mounted.state.skill_actions.edit.run(None);
            mounted.state.skills.draft.set(draft("stale-edit"));
            let (send, receive) = futures::channel::oneshot::channel();
            mounted
                .state
                .fake
                .skill_command_results
                .borrow_mut()
                .push_back(receive);
            mounted.state.skill_actions.save.run(());
            settle().await;
            if account {
                mounted
                    .state
                    .auth
                    .generation
                    .update(|generation| *generation += 1);
            } else {
                mounted.state.chat.active_session.set(Some(2));
            }
            settle().await;
            send.send(Ok(ProjectSkills {
                enabled: true,
                entries: vec![ProjectSkill {
                    plugin: None,
                    id: 1,
                    revision: 1,
                    updated_at: 0,
                    draft: draft("stale-edit"),
                }],
            }))
            .unwrap();
            settle().await;
            assert!(!mounted.root.text_content().unwrap().contains("stale-edit"));
            assert!(!mounted.state.skills.editing.get_untracked());
            assert!(!mounted.state.skills.busy.get_untracked());
        }
    }
}
#[wasm_bindgen_test]
async fn skills_file_folder_and_archive_imports_open_review_without_saving_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mounted = fixture(mode, false);
        settle().await;
        let mut skill = draft("imported-check");
        skill.resources.push(openwebide_core::SkillResource {
            name: "references/check.md".into(),
            content: "cargo test 🦀".into(),
            binary: false,
        });
        let zip = openwebide_core::skills::archive::export_zip(&skill).unwrap();
        let manifest = openwebide_core::skills::archive::markdown(&skill).unwrap();
        for (name, bytes) in [
            ("SKILL.md", manifest.as_bytes()),
            ("skill.zip", zip.as_slice()),
        ] {
            let parts = js_sys::Array::new();
            parts.push(&js_sys::Uint8Array::from(bytes));
            let file = web_sys::File::new_with_u8_array_sequence(&parts, name).unwrap();
            let transfer = web_sys::DataTransfer::new().unwrap();
            transfer.items().add_with_file(&file).unwrap();
            let picker = mounted
                .element("[aria-label='Import skill file or archive']")
                .unchecked_into::<web_sys::HtmlInputElement>();
            picker.set_files(transfer.files().as_ref());
            picker
                .dispatch_event(&web_sys::Event::new("change").unwrap())
                .unwrap();
            wait_until("skill import finished", || {
                !mounted.state.skills.busy.get_untracked()
            })
            .await;
            settle().await;
            assert!(
                mounted.state.skills.editing.get_untracked(),
                "{:?}",
                mounted.state.skills.error.get_untracked()
            );
            assert_eq!(
                mounted.state.skills.draft.get_untracked().name,
                "imported-check"
            );
            assert!(
                mounted
                    .state
                    .fake
                    .skills
                    .borrow()
                    .get(&1)
                    .is_none_or(|data| data.entries.is_empty())
            );
            if name.ends_with("zip") {
                assert_eq!(
                    mounted.state.skills.draft.get_untracked().resources,
                    skill.resources
                );
            }
            mounted.click_text("Cancel");
        }
        let transfer = web_sys::DataTransfer::new().unwrap();
        for (name, bytes) in [
            ("selected/SKILL.md", manifest.as_bytes()),
            ("selected/references/check.md", "cargo test 🦀".as_bytes()),
        ] {
            let parts = js_sys::Array::new();
            parts.push(&js_sys::Uint8Array::from(bytes));
            let file =
                web_sys::File::new_with_u8_array_sequence(&parts, name.rsplit('/').next().unwrap())
                    .unwrap();
            let descriptor = js_sys::Object::new();
            js_sys::Reflect::set(&descriptor, &"value".into(), &name.into()).unwrap();
            js_sys::Object::define_property(
                file.unchecked_ref::<js_sys::Object>(),
                &"webkitRelativePath".into(),
                &descriptor,
            );
            transfer.items().add_with_file(&file).unwrap();
        }
        let picker = mounted
            .element("[aria-label='Import skill folder']")
            .unchecked_into::<web_sys::HtmlInputElement>();
        assert!(picker.has_attribute("webkitdirectory"));
        picker.set_files(transfer.files().as_ref());
        picker
            .dispatch_event(&web_sys::Event::new("change").unwrap())
            .unwrap();
        wait_until("skill folder import finished", || {
            !mounted.state.skills.busy.get_untracked()
        })
        .await;
        settle().await;
        assert_eq!(mounted.state.skills.draft.get_untracked(), skill);
        mounted.click_text("Save skill");
        settle().await;
        assert_eq!(
            mounted.state.fake.skills.borrow()[&1].entries[0].draft,
            skill
        );
    }
}
#[wasm_bindgen_test]
async fn browser_skill_tools_persist_and_toggle_removes_context_and_tools_from_new_runs() {
    let mounted = fixture(WorkspaceMode::Local, true);
    settle().await;
    mounted
        .state
        .chat
        .set_approval_mode(1, openwebide_core::ApprovalMode::Yolo);
    mounted.state.fake.settings.borrow_mut().insert(
        openwebide_core::ApprovalMode::setting_key(1),
        "\"yolo\"".into(),
    );
    let reply = ChatCompletion {
        response: ChatResponse::Text("done".into()),
        preamble: String::new(),
        reasoning: String::new(),
        usage: None,
        stop_reason: StopReason::Complete,
    };
    // Optional tools are discovered before use on small-context models.
    let discover = |name: &str| ChatCompletion {
        response: ChatResponse::ToolCalls(vec![ToolCall {
            id: format!("discover-{name}"),
            name: "tool_search".into(),
            arguments: serde_json::json!({"query": format!("select: {name}")}).to_string(),
        }]),
        ..reply.clone()
    };
    let create = ChatCompletion {
        response: ChatResponse::ToolCalls(vec![ToolCall {
            id: "remember".into(),
            name: "skill_create".into(),
            arguments: r#"{"draft":{"name":"build-check","description":"Run build checks","instructions":"PRIVATE INSTRUCTIONS"}}"#.into(),
        }]),
        ..reply.clone()
    };
    mounted
        .state
        .fake
        .scripted_completions
        .borrow_mut()
        .extend([discover("skill_create"), create, reply.clone()]);
    mounted.input("remember our build command");
    mounted.key("Enter", "Enter", false);
    wait_until("memory run finished", || {
        !mounted.state.chat.streaming.get_untracked()
    })
    .await;
    settle().await;
    assert!(
        mounted.state.chat.error.get_untracked().is_none(),
        "{:?}",
        mounted.state.chat.error.get_untracked()
    );
    assert_eq!(mounted.state.fake.skills.borrow()[&1].entries.len(), 1);
    assert!(mounted.root.text_content().unwrap().contains("build-check"));
    mounted
        .state
        .fake
        .scripted_completions
        .borrow_mut()
        .extend([discover("skill_read"), reply.clone()]);
    let previous_requests = mounted.state.fake.completion_requests.borrow().len();
    mounted.input("what is our build command");
    mounted.key("Enter", "Enter", false);
    wait_until("skill context request started", || {
        mounted.state.fake.completion_requests.borrow().len() > previous_requests
            || mounted.state.chat.error.get_untracked().is_some()
    })
    .await;
    assert!(
        mounted.state.chat.error.get_untracked().is_none(),
        "{:?}",
        mounted.state.chat.error.get_untracked()
    );
    wait_until("skill context run finished", || {
        !mounted.state.chat.streaming.get_untracked()
    })
    .await;
    settle().await;
    let request = mounted
        .state
        .fake
        .completion_requests
        .borrow()
        .last()
        .unwrap()
        .clone();
    assert!(request.tools.iter().any(|tool| tool.name == "skill_read"));
    assert!(
        request
            .system_prompt
            .as_ref()
            .unwrap()
            .contains("build-check")
    );
    mounted.click_text("Disabled");
    settle().await;
    mounted
        .state
        .fake
        .scripted_completions
        .borrow_mut()
        .push_back(reply);
    let previous_requests = mounted.state.fake.completion_requests.borrow().len();
    mounted.input("try without skills");
    mounted.key("Enter", "Enter", false);
    wait_until("disabled skill request started", || {
        mounted.state.fake.completion_requests.borrow().len() > previous_requests
            || mounted.state.chat.error.get_untracked().is_some()
    })
    .await;
    assert!(
        mounted.state.chat.error.get_untracked().is_none(),
        "{:?}",
        mounted.state.chat.error.get_untracked()
    );
    wait_until("disabled skill run finished", || {
        !mounted.state.chat.streaming.get_untracked()
    })
    .await;
    settle().await;
    let requests = mounted.state.fake.completion_requests.borrow();
    let request = requests.last().unwrap();
    assert!(
        !request
            .tools
            .iter()
            .any(|tool| tool.name.starts_with("skill_"))
    );
    assert!(
        !request
            .system_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.contains("Available project skills"))
    );
    assert_eq!(
        mounted
            .state
            .fake
            .skill_commands
            .borrow()
            .iter()
            .filter(
                |(_, command, session)| *session && matches!(command, SkillCommand::Create { .. })
            )
            .count(),
        0
    );
}

#[wasm_bindgen_test]
async fn skills_import_cannot_restore_an_old_project_draft_after_navigation() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mounted = fixture(mode, false);
        settle().await;
        let manifest = openwebide_core::skills::archive::markdown(&draft("old-import")).unwrap();
        let parts = js_sys::Array::new();
        parts.push(&manifest.into());
        let file = web_sys::File::new_with_str_sequence(&parts, "SKILL.md").unwrap();
        let transfer = web_sys::DataTransfer::new().unwrap();
        transfer.items().add_with_file(&file).unwrap();
        mounted
            .state
            .skill_actions
            .import
            .run(transfer.files().unwrap());
        mounted.state.projects.active_project.set(Some(2));
        settle().await;
        openwebide_frontend::util::sleep_ms(30).await;
        settle().await;
        assert!(!mounted.state.skills.editing.get_untracked());
        assert_ne!(
            mounted.state.skills.draft.get_untracked().name,
            "old-import"
        );
        assert!(!mounted.state.skills.busy.get_untracked());
    }
}

#[wasm_bindgen_test]
async fn plugin_skills_show_provenance_and_offer_export_without_direct_edits() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let mounted = fixture(mode, false);
        settle().await;
        let package = openwebide_core::plugins::testing::package();
        mounted.state.skills.data.set(Some(ProjectSkills {
            enabled: true,
            entries: vec![ProjectSkill {
                id: 1,
                revision: 1,
                updated_at: 0,
                plugin: Some(openwebide_core::plugins::PluginSkillOrigin {
                    publisher: package.prepared.manifest.publisher,
                    name: package.prepared.manifest.name,
                    version: package.prepared.manifest.version,
                    commit: package.prepared.source.commit,
                }),
                draft: package.skills[0].clone(),
            }],
        }));
        settle().await;
        mounted.click(".memory-entry .ui-disclosure-toggle");
        settle().await;
        let text = mounted.root.text_content().unwrap();
        assert!(text.contains("From openwebide/pr-review 0.1.0"));
        let entry = mounted.element(".memory-entry");
        let buttons = entry.query_selector_all("button").unwrap();
        let labels = (0..buttons.length())
            .map(|index| buttons.item(index).unwrap().text_content().unwrap())
            .collect::<Vec<_>>();
        assert!(labels.iter().any(|label| label == "Export ZIP"));
        assert!(
            !labels
                .iter()
                .any(|label| label == "Edit" || label == "Delete")
        );
        mounted.click_text("Export ZIP");
        assert!(mounted.state.skills.error.get_untracked().is_none());
    }
}
