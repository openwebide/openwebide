//! The same idle-work contract through ordinary backend and paired HTTP adapters.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

fn inputs(fixture: &Fixture) -> Vec<Request> {
    fixture
        .mounted
        .state
        .fake
        .native_analysis_inputs
        .borrow()
        .iter()
        .filter(|input| input.query.is_prefetch())
        .cloned()
        .collect()
}

fn hold(fixture: &Fixture) -> futures::channel::oneshot::Sender<Result<(), String>> {
    let (send, receive) = futures::channel::oneshot::channel();
    fixture
        .mounted
        .state
        .fake
        .native_prefetch_results
        .borrow_mut()
        .push_back(receive);
    send
}

#[wasm_bindgen_test]
async fn idle_prefetch_defaults_off_and_disabling_it_cancels_held_work_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let fixture = Fixture::new(mode, serde_json::json!([])).await;
        let state = fixture.mounted.state.languages;
        let actions =
            fixture.mounted.state.owner.with(
                expect_context::<openwebide_frontend::project_languages::ProjectLanguageActions>,
            );
        wait_until("ordinary synchronization warms the service", || {
            fixture
                .mounted
                .state
                .fake
                .native_analysis_inputs
                .borrow()
                .iter()
                .any(|input| matches!(input.query, Query::Synchronize))
        })
        .await;
        openwebide_frontend::util::sleep_ms(1000).await;
        assert!(!state.prefetch_enabled.get_untracked());
        assert!(inputs(&fixture).is_empty());
        let release = hold(&fixture);
        actions.set_prefetch.run(true);
        wait_until("opted-in optional work is held", || {
            inputs(&fixture).len() == 1
        })
        .await;
        actions.set_prefetch.run(false);
        assert!(!state.prefetch_enabled.get_untracked());
        let _ = release.send(Ok(()));
        wait_until("disabled preference is persisted", || {
            !state.prefetch_saving.get_untracked()
                && fixture
                    .mounted
                    .state
                    .fake
                    .settings
                    .borrow()
                    .get("language_prefetch")
                    .is_some_and(|value| value == "false")
        })
        .await;
        openwebide_frontend::util::sleep_ms(1000).await;
        assert_eq!(
            inputs(&fixture).len(),
            1,
            "Disabled work must not continue its batch"
        );
        assert!(fixture.actions.result.get_untracked().is_none());
        assert_eq!(fixture.actions.error.get_untracked(), None);
        fixture.query(IntelligenceCommand::Symbols).await;
        assert_eq!(fixture.content(), "alpha");
        fixture.finish().await;
    }
}

#[wasm_bindgen_test]
async fn idle_prefetch_uses_bounded_current_buffers_and_rewarms_after_dependency_edits_in_both_modes()
 {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let fixture = Fixture::with_prefetch(mode, serde_json::json!([]), true).await;
        let workspace = fixture.mounted.state.workspace;
        for index in 0..8 {
            let path = format!("extra-{index}.notes");
            workspace.register_editor_tab(1, path.clone());
            workspace.editor_buffers.update(|buffers| {
                buffers.insert(
                    (1, path.clone()),
                    EditorBuffer {
                        content: "x".repeat(200 * 1024).into(),
                        dirty: true,
                        read_only: false,
                    },
                );
            });
        }
        workspace.register_editor_tab(1, "oversized.notes".into());
        workspace.editor_buffers.update(|buffers| {
            buffers.insert(
                (1, "oversized.notes".into()),
                EditorBuffer {
                    content: "x".repeat(300 * 1024).into(),
                    dirty: true,
                    read_only: false,
                },
            );
            buffers.insert(
                (1, "dependency.notes".into()),
                EditorBuffer {
                    content: "effective unsaved dependency".into(),
                    dirty: true,
                    read_only: false,
                },
            );
        });
        wait_until("idle prefetch completes its bounded batch", || {
            inputs(&fixture).len() >= 8
        })
        .await;
        let observed = inputs(&fixture);
        assert_eq!(observed.len(), 8, "{mode:?}");
        let sources = observed
            .iter()
            .map(|input| (input.path.clone(), input.source.clone()))
            .collect::<BTreeMap<_, _>>();
        assert!(sources.len() <= 4);
        assert!(sources.values().map(String::len).sum::<usize>() <= 512 * 1024);
        assert!(!sources.contains_key("oversized.notes"));
        assert_eq!(
            sources.get("dependency.notes").map(String::as_str),
            Some("effective unsaved dependency")
        );
        assert_eq!(
            observed
                .iter()
                .map(|input| (
                    input.path.clone(),
                    serde_json::to_string(&input.query).unwrap()
                ))
                .collect::<BTreeSet<_>>()
                .len(),
            observed.len()
        );
        fixture
            .actions
            .request
            .run(IntelligenceCommand::Query(Query::Synchronize));
        openwebide_frontend::util::sleep_ms(1100).await;
        assert_eq!(
            inputs(&fixture).len(),
            observed.len(),
            "Unchanged buffers must not cause repeated speculation"
        );
        workspace.editor_buffers.update(|buffers| {
            buffers
                .get_mut(&(1, "dependency.notes".into()))
                .unwrap()
                .content = "new unsaved dependency 文😀".into();
        });
        wait_until(
            "new dependency revision receives fresh optional work",
            || {
                inputs(&fixture).iter().any(|input| {
                    input.path == "dependency.notes"
                        && input.source == "new unsaved dependency 文😀"
                })
            },
        )
        .await;
        assert_eq!(fixture.content(), "alpha");
        assert!(workspace.dirty.get_untracked());
        assert!(
            fixture
                .mounted
                .state
                .fake
                .calls
                .borrow()
                .iter()
                .all(|call| !matches!(
                    call,
                    openwebide_frontend::testing::fake_backend::Call::WriteFile { .. }
                ))
        );
        assert!(fixture.actions.result.get_untracked().is_none());
        assert_eq!(fixture.actions.error.get_untracked(), None);
        fixture.finish().await;
    }
}

#[wasm_bindgen_test]
async fn known_dependency_hints_survive_edits_and_use_current_open_sources_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let fixture = Fixture::with_prefetch(mode, serde_json::json!([]), true).await;
        let workspace = fixture.mounted.state.workspace;
        for path in std::iter::once("a-related.notes".to_string())
            .chain((0..8).map(|index| format!("z-other-{index}.notes")))
        {
            workspace.register_editor_tab(1, path.clone());
            workspace.editor_buffers.update(|buffers| {
                buffers.insert(
                    (1, path.clone()),
                    EditorBuffer {
                        content: "current dependency 文😀".into(),
                        dirty: true,
                        read_only: false,
                    },
                );
            });
        }
        settle().await;
        *fixture
            .mounted
            .state
            .fake
            .native_language_result
            .borrow_mut() = Some(serde_json::json!([{
            "path":"a-related.notes", "resource":null,
            "source_digest":openwebide_core::plugins::languages::source_digest("older dependency"),
            "label":"Dependency", "range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}}
        }]));
        fixture.query(IntelligenceCommand::Definition).await;
        let before = inputs(&fixture).len();
        *fixture
            .mounted
            .state
            .fake
            .native_language_result
            .borrow_mut() = Some(serde_json::json!([]));
        fixture.actions.dismiss.run(());
        fixture.editor.insert_native_text("!", 1.0).unwrap();
        wait_until(
            "known dependency receives current-source optional work",
            || {
                inputs(&fixture).iter().skip(before).any(|input| {
                    input.path == "a-related.notes" && input.source == "current dependency 文😀"
                })
            },
        )
        .await;
        assert_eq!(fixture.content(), "alpha!");
        assert!(fixture.actions.result.get_untracked().is_none());
        assert_eq!(fixture.actions.error.get_untracked(), None);
        fixture.finish().await;
    }
}

#[wasm_bindgen_test]
async fn foreground_requests_preempt_held_prefetch_and_late_replies_never_replace_the_result_in_both_modes()
 {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let fixture = Fixture::with_prefetch(mode, serde_json::json!([]), true).await;
        let release = hold(&fixture);
        wait_until("optional request is held", || inputs(&fixture).len() == 1).await;
        let response = fixture.query(IntelligenceCommand::Symbols).await;
        assert!(response.result.as_array().unwrap().is_empty());
        assert!(matches!(
            fixture.actions.result.get_untracked().unwrap().0,
            Query::Symbols
        ));
        let _ = release.send(Ok(()));
        settle().await;
        assert!(matches!(
            fixture.actions.result.get_untracked().unwrap().0,
            Query::Symbols
        ));
        assert_eq!(fixture.content(), "alpha");
        assert_eq!(fixture.actions.error.get_untracked(), None);
        fixture.finish().await;
    }
}

#[derive(Clone, Copy, Debug)]
enum ContextChange {
    Source,
    Provider,
    Account,
    Project,
    Composition,
    Recovery,
    Unmount,
}

#[wasm_bindgen_test]
async fn prefetch_is_cancelled_for_source_provider_account_project_and_unmount_changes_in_both_modes()
 {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for change in [
            ContextChange::Source,
            ContextChange::Provider,
            ContextChange::Account,
            ContextChange::Project,
            ContextChange::Composition,
            ContextChange::Recovery,
            ContextChange::Unmount,
        ] {
            let fixture = Fixture::with_prefetch(mode, serde_json::json!([]), true).await;
            let release = hold(&fixture);
            wait_until("optional work is held before context replacement", || {
                inputs(&fixture).len() == 1
            })
            .await;
            let old = inputs(&fixture).pop().unwrap();
            let fake = fixture.mounted.state.fake.clone();
            match change {
                ContextChange::Source => {
                    fixture.editor.insert_native_text("!", 1.0).unwrap();
                }
                ContextChange::Provider => fixture
                    .mounted
                    .state
                    .owner
                    .with(expect_context::<PluginsState>)
                    .project_plugins
                    .update(|plugins| plugins[0].enabled = false),
                ContextChange::Account => fixture
                    .mounted
                    .state
                    .auth
                    .generation
                    .update(|generation| *generation = generation.wrapping_add(1)),
                ContextChange::Project => {
                    fixture.mounted.state.projects.active_project.set(Some(2));
                }
                ContextChange::Composition => {
                    fixture.editor.begin_composition();
                    assert!(fixture.editor.is_composing());
                }
                ContextChange::Recovery => fixture.mounted.state.workspace.workspace_recovery.update(|states| {
                    states.insert(1, openwebide_frontend::state::workspace::WorkspaceRecoveryStatus::Checking);
                }),
                ContextChange::Unmount => {
                    fixture.finish().await;
                    let _ = release.send(Ok(()));
                    openwebide_frontend::util::sleep_ms(1000).await;
                    assert_eq!(
                        fake.native_analysis_inputs
                            .borrow()
                            .iter()
                            .filter(|input| input.query.is_prefetch())
                            .count(),
                        1
                    );
                    continue;
                }
            }
            settle().await;
            let _ = release.send(Ok(()));
            openwebide_frontend::util::sleep_ms(1000).await;
            assert!(
                inputs(&fixture)
                    .iter()
                    .skip(1)
                    .all(|input| input.client != old.client || input.revision != old.revision),
                "Stale queue survived {mode:?}/{change:?}"
            );
            assert!(fixture.actions.result.get_untracked().is_none());
            assert_eq!(
                fixture.content(),
                if matches!(change, ContextChange::Source) {
                    "alpha!"
                } else {
                    "alpha"
                }
            );
            if matches!(change, ContextChange::Composition) {
                assert!(fixture.editor.is_composing());
                fixture.editor.cancel_composition();
            }
            fixture.finish().await;
        }
    }
}

#[wasm_bindgen_test]
async fn unavailable_language_services_do_not_start_optional_work_or_block_editing_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let fixture = Fixture::with_prefetch(mode, serde_json::json!([]), true).await;
        *fixture
            .mounted
            .state
            .fake
            .native_language_result
            .borrow_mut() = None;
        openwebide_frontend::util::sleep_ms(1500).await;
        assert!(inputs(&fixture).is_empty());
        fixture.editor.insert_native_text("!", 1.0).unwrap();
        assert_eq!(fixture.content(), "alpha!");
        openwebide_frontend::util::sleep_ms(1500).await;
        assert!(inputs(&fixture).is_empty());
        assert!(fixture.actions.available.get_untracked());
        fixture.finish().await;
    }
}

#[wasm_bindgen_test]
async fn stalled_prefetch_has_a_client_deadline_without_publishing_an_editor_error_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let fixture = Fixture::with_prefetch(mode, serde_json::json!([]), true).await;
        let release = hold(&fixture);
        wait_until("first optional request is held", || {
            inputs(&fixture).len() == 1
        })
        .await;
        crate::support::wait_until_with_timeout(
            "client deadline releases speculative progress",
            4000,
            || inputs(&fixture).len() > 1,
        )
        .await;
        assert_eq!(fixture.actions.error.get_untracked(), None);
        let _ = fixture.query(IntelligenceCommand::Symbols).await;
        let _ = release.send(Ok(()));
        settle().await;
        assert!(matches!(
            fixture.actions.result.get_untracked().unwrap().0,
            Query::Symbols
        ));
        assert_eq!(fixture.content(), "alpha");
        fixture.finish().await;
    }
}
