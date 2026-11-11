//! Browser consumers validate the original native result before editor history.
use super::binding;
use crate::{
    local_bridge::{
        HttpGuard, fake_bridge_http, probe_folder, set_bridge_native_language_handler,
        set_bridge_plugin,
    },
    support::{Mounted, mount_test_with_backend, settle, wait_until},
};
use leptos::prelude::*;
use openwebide_core::{
    User, UserId, UserRole, WorkspaceMode,
    editor::{Document, Indentation, Selection},
    plugins::languages::intelligence::{Query, Request, Response, results::TextEdit},
};
use openwebide_frontend::{
    state::{plugins::PluginsState, workspace::EditorBuffer},
    state_actions::editor::{
        EditorActions, EditorCommand, EditorIntelligence, IntelligenceCommand,
    },
    testing::fake_backend::FakeBackend,
};
use std::{cell::Cell, rc::Rc};
use wasm_bindgen::{JsCast, prelude::*};
use wasm_bindgen_test::*;

struct Fixture {
    mounted: Mounted,
    actions: EditorIntelligence,
    editor: EditorActions,
    http: HttpGuard,
    native_reply: Closure<dyn FnMut(String) -> js_sys::Promise>,
    previous_token: Option<String>,
}

impl Fixture {
    async fn new(mode: WorkspaceMode, result: serde_json::Value) -> Self {
        let previous_token = openwebide_frontend::idb::get_bridge_pairing_token()
            .await
            .unwrap();
        openwebide_frontend::idb::set_bridge_pairing_token("editor-analysis-token")
            .await
            .unwrap();
        let http = HttpGuard(fake_bridge_http());
        let mut plugin = binding();
        plugin.prepared.manifest.compatibility.plugin_api = 9;
        plugin.prepared.manifest.executable = None;
        plugin.prepared.manifest.contributions.languages = serde_json::from_value(serde_json::json!([
            {"id":"notes","name":"Notes","extensions":["notes"],"features":{"languageServer":{"command":"notes-lsp"}}}
        ])).unwrap();
        plugin.prepared.manifest.validate().unwrap();
        let paired = plugin.prepared.clone();
        set_bridge_plugin(&http.0, &serde_json::to_string(&paired).unwrap());
        let fake = Rc::new(FakeBackend::default());
        fake.project_plugin_entries
            .borrow_mut()
            .insert(1, vec![plugin]);
        *fake.native_language_result.borrow_mut() = Some(result);
        let bridge = fake.clone();
        let native_reply =
            Closure::<dyn FnMut(String) -> js_sys::Promise>::new(move |wire: String| {
                let bridge = bridge.clone();
                wasm_bindgen_futures::future_to_promise(async move {
                    let input: Request = serde_json::from_str(&wire).unwrap();
                    let response = bridge
                        .native_language_reply(&input)
                        .await
                        .map_err(|error| JsValue::from(js_sys::Error::new(&error)))?;
                    Ok(JsValue::from_str(
                        &serde_json::to_string(&response).unwrap(),
                    ))
                })
            });
        set_bridge_native_language_handler(&http.0, native_reply.as_ref().unchecked_ref());
        let slot = Rc::new(Cell::new(None::<(EditorActions, EditorIntelligence)>));
        let captured = slot.clone();
        let folder = probe_folder();
        let mounted = mount_test_with_backend(fake, move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.fake.projects.borrow_mut()[0].mode = mode;
            state.projects.local_handles.update(|handles| {
                handles.insert(1, folder.unchecked_into());
            });
            state
                .settings
                .bridge_url
                .set("ws://bridge.test:3001".into());
            state.auth.set_user(User {
                id: UserId::new(1),
                username: "owner".into(),
                role: UserRole::User,
                created_at: 0,
            });
            state.workspace.open_file.set(Some("draft.notes".into()));
            state.workspace.content.set("alpha".into());
            state.workspace.dirty.set(true);
            state.workspace.editor_tabs.update(|tabs| {
                tabs.insert(1, vec!["draft.notes".into(), "dependency.notes".into()]);
            });
            state.workspace.editor_documents.update(|documents| {
                let mut draft = Document::new("saved");
                draft
                    .apply(
                        vec![openwebide_core::editor::Edit::replace(0..5, "alpha")],
                        vec![Selection::caret(5)],
                        None,
                    )
                    .unwrap();
                documents.insert((1, "draft.notes".into()), draft);
                documents.insert((1, "dependency.notes".into()), Document::new("dependency"));
            });
            provide_context(
                openwebide_frontend::state_actions::workspace::WorkspaceActions::new(
                    state.api,
                    state.projects,
                    state.workspace,
                    state.ui,
                    RwSignal::new(false),
                    Callback::new(|()| ()),
                ),
            );
            let editor = EditorActions::new(state.workspace);
            editor.record_selection(Selection::caret(5)).unwrap();
            captured.set(Some((
                editor,
                editor
                    .install_intelligence(Signal::derive(|| false))
                    .unwrap(),
            )));
            view! { <div/> }
        });
        let (editor, actions) = slot.get().unwrap();
        wait_until("editor language service is ready", || {
            actions.available.get_untracked()
        })
        .await;
        settle().await;
        Self {
            mounted,
            actions,
            editor,
            http,
            native_reply,
            previous_token,
        }
    }

    async fn query(&self, command: IntelligenceCommand) -> Response {
        self.actions.request.run(command);
        wait_until("editor analysis result", || {
            !self.actions.busy.get_untracked()
                && (self.actions.result.with_untracked(Option::is_some)
                    || self.actions.error.with_untracked(Option::is_some))
        })
        .await;
        assert_eq!(self.actions.error.get_untracked(), None);
        self.actions.result.get_untracked().unwrap().1
    }

    fn validations(&self) -> usize {
        self.mounted
            .state
            .fake
            .native_analysis_inputs
            .borrow()
            .iter()
            .filter(|input| matches!(input.query, Query::ValidateAnalysis { .. }))
            .count()
    }

    fn hold_validation(&self) -> futures::channel::oneshot::Sender<Result<(), String>> {
        let (release, response) = futures::channel::oneshot::channel();
        self.mounted
            .state
            .fake
            .native_analysis_results
            .borrow_mut()
            .push_back(response);
        release
    }

    fn content(&self) -> String {
        self.mounted
            .state
            .workspace
            .content
            .get_untracked()
            .to_string()
    }

    async fn finish(self) {
        drop(self.mounted);
        settle().await;
        drop(self.http);
        drop(self.native_reply);
        if let Some(previous) = self.previous_token {
            openwebide_frontend::idb::set_bridge_pairing_token(&previous)
                .await
                .unwrap();
        } else {
            openwebide_frontend::idb::delete_bridge_pairing_token()
                .await
                .unwrap();
        }
    }
}

fn formatting(text: &str) -> serde_json::Value {
    serde_json::json!([{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":5}},"newText":text}])
}

fn workspace_edit(
    text: &str,
) -> openwebide_core::plugins::languages::intelligence::results::WorkspaceEdit {
    use openwebide_core::plugins::languages::{
        intelligence::results::{FileEdit, WorkspaceEdit},
        source_digest,
    };
    WorkspaceEdit {
        changes: vec![],
        files: [("draft.notes", "alpha"), ("dependency.notes", "dependency")]
            .into_iter()
            .map(|(path, source)| FileEdit {
                path: path.into(), source_digest: source_digest(source), revision: None,
                edits: serde_json::from_value(serde_json::json!([{
                    "range":{"start":{"line":0,"character":0},"end":{"line":0,"character":source.len()}},
                    "newText":text
                }])).unwrap(),
            }).collect(),
    }
}

async fn prepare_workspace(
    fixture: &Fixture,
    edit: openwebide_core::plugins::languages::intelligence::results::WorkspaceEdit,
) {
    fixture.actions.prepare_workspace(edit);
    wait_until("workspace review is ready", || {
        !fixture.actions.busy.get_untracked()
    })
    .await;
    assert_eq!(fixture.actions.error.get_untracked(), None);
    assert_eq!(
        fixture.actions.review.get_untracked().unwrap().files.len(),
        2
    );
}

#[wasm_bindgen_test]
async fn workspace_text_reviews_validate_before_atomic_publication_and_keep_undo_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let edit = workspace_edit("beta");
        let fixture = Fixture::new(mode, serde_json::to_value(&edit).unwrap()).await;
        fixture
            .query(IntelligenceCommand::Rename("beta".into()))
            .await;
        let mut tampered = edit.clone();
        tampered.files[1].edits[0].new_text = "unreviewed".into();
        fixture.actions.prepare_workspace(tampered);
        assert!(fixture.actions.review.get_untracked().is_none());
        assert!(
            fixture
                .actions
                .error
                .get_untracked()
                .unwrap()
                .contains("workspace edit changed")
        );
        assert_eq!(fixture.validations(), 0);
        fixture.actions.error.set(None);
        prepare_workspace(&fixture, edit).await;
        let approved = fixture.actions.review.get_untracked().unwrap();
        let release = fixture.hold_validation();
        fixture
            .actions
            .apply_workspace(approved.clone(), Signal::derive(|| false));
        wait_until("workspace validation is pending", || {
            fixture.validations() == 1
        })
        .await;
        assert_eq!(fixture.content(), "alpha");
        fixture
            .mounted
            .state
            .workspace
            .editor_documents
            .with_untracked(|documents| {
                assert_eq!(
                    documents[&(1, "dependency.notes".into())].text(),
                    "dependency"
                );
            });
        release.send(Err("dependency changed".into())).unwrap();
        wait_until("failed workspace validation settles", || {
            !fixture.actions.busy.get_untracked()
        })
        .await;
        assert_eq!(fixture.content(), "alpha");
        assert!(
            fixture
                .actions
                .error
                .get_untracked()
                .unwrap()
                .contains("dependency changed")
        );
        assert!(std::sync::Arc::ptr_eq(
            &approved,
            &fixture.actions.review.get_untracked().unwrap()
        ));
        fixture
            .actions
            .apply_workspace(approved, Signal::derive(|| false));
        wait_until("all reviewed drafts publish", || {
            fixture.content() == "beta"
        })
        .await;
        assert_eq!(fixture.validations(), 2);
        assert_eq!(fixture.actions.error.get_untracked(), None);
        fixture
            .mounted
            .state
            .workspace
            .editor_documents
            .update(|documents| {
                let dependency = documents.get_mut(&(1, "dependency.notes".into())).unwrap();
                assert_eq!(dependency.text(), "beta");
                assert!(dependency.is_dirty());
                assert!(dependency.undo());
                assert_eq!(dependency.text(), "dependency");
            });
        fixture
            .editor
            .command(
                EditorCommand::Undo,
                fixture.editor.current_selection().unwrap(),
                Indentation::default(),
            )
            .unwrap();
        assert_eq!(fixture.content(), "alpha");
        assert!(fixture.mounted.state.workspace.dirty.get_untracked());
        assert!(
            !fixture
                .mounted
                .state
                .fake
                .calls
                .borrow()
                .iter()
                .any(|call| matches!(
                    call,
                    openwebide_frontend::testing::fake_backend::Call::WriteFile { .. }
                ))
        );
        fixture.finish().await;
    }
}

#[wasm_bindgen_test]
async fn workspace_text_review_identity_and_late_draft_changes_fence_publication_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let edit = workspace_edit("beta");
        let fixture = Fixture::new(mode, serde_json::to_value(&edit).unwrap()).await;
        fixture
            .query(IntelligenceCommand::Rename("beta".into()))
            .await;
        prepare_workspace(&fixture, edit).await;
        let obsolete = fixture.actions.review.get_untracked().unwrap();
        let replacement = workspace_edit("gamma");
        *fixture
            .mounted
            .state
            .fake
            .native_language_result
            .borrow_mut() = Some(serde_json::to_value(&replacement).unwrap());
        fixture
            .query(IntelligenceCommand::Rename("gamma".into()))
            .await;
        prepare_workspace(&fixture, replacement).await;
        fixture
            .actions
            .apply_workspace(obsolete, Signal::derive(|| false));
        assert_eq!(fixture.validations(), 0);
        assert_eq!(fixture.content(), "alpha");
        let approved = fixture.actions.review.get_untracked().unwrap();
        fixture
            .actions
            .apply_workspace(approved.clone(), Signal::derive(|| true));
        assert_eq!(fixture.validations(), 0);
        let release = fixture.hold_validation();
        fixture
            .actions
            .apply_workspace(approved, Signal::derive(|| false));
        wait_until("replacement review validation", || {
            fixture.validations() == 1
        })
        .await;
        fixture
            .mounted
            .state
            .workspace
            .editor_buffers
            .update(|buffers| {
                buffers.insert(
                    (1, "dependency.notes".into()),
                    EditorBuffer {
                        content: "new dependency".into(),
                        dirty: true,
                        read_only: false,
                    },
                );
            });
        settle().await;
        let _ = release.send(Ok(()));
        wait_until("changed workspace review settles", || {
            !fixture.actions.busy.get_untracked()
        })
        .await;
        assert_eq!(fixture.content(), "alpha");
        fixture
            .mounted
            .state
            .workspace
            .editor_buffers
            .with_untracked(|buffers| {
                assert_eq!(
                    buffers[&(1, "dependency.notes".into())].content.as_str(),
                    "new dependency"
                );
            });
        fixture.finish().await;
    }
}

#[wasm_bindgen_test]
async fn formatting_validates_the_retained_body_and_native_context_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let fixture = Fixture::new(mode, formatting("beta")).await;
        let response = fixture.query(IntelligenceCommand::Formatting).await;
        assert!(response.context.is_some());
        let edits: Vec<TextEdit> = serde_json::from_value(response.result).unwrap();
        let mut altered = edits.clone();
        altered[0].new_text = "intruder".into();
        fixture.actions.apply_text.run(altered);
        settle().await;
        assert_eq!(
            fixture.validations(),
            0,
            "Changed edits reject before native authority"
        );
        assert_eq!(fixture.content(), "alpha");
        *fixture
            .mounted
            .state
            .fake
            .native_analysis_error
            .borrow_mut() = Some("dependency changed".into());
        fixture.actions.apply_text.run(edits.clone());
        wait_until("native freshness rejection", || {
            !fixture.actions.busy.get_untracked()
        })
        .await;
        assert_eq!(fixture.validations(), 1);
        assert_eq!(fixture.content(), "alpha");
        assert!(
            fixture
                .actions
                .error
                .get_untracked()
                .unwrap()
                .contains("dependency changed")
        );
        *fixture
            .mounted
            .state
            .fake
            .native_analysis_error
            .borrow_mut() = None;
        fixture.actions.apply_text.run(edits);
        wait_until("validated formatting publication", || {
            fixture.content() == "beta"
        })
        .await;
        assert_eq!(fixture.validations(), 2);
        fixture
            .editor
            .command(
                EditorCommand::Undo,
                fixture.editor.current_selection().unwrap(),
                Indentation::default(),
            )
            .unwrap();
        assert_eq!(fixture.content(), "alpha");
        assert!(fixture.mounted.state.workspace.dirty.get_untracked());
        fixture.finish().await;

        let fixture = Fixture::new(mode, formatting("beta")).await;
        *fixture
            .mounted
            .state
            .fake
            .native_analysis_unproved
            .borrow_mut() = true;
        let response = fixture.query(IntelligenceCommand::Formatting).await;
        assert!(response.context.is_none());
        fixture
            .actions
            .apply_text
            .run(serde_json::from_value(response.result).unwrap());
        settle().await;
        assert_eq!(fixture.validations(), 0);
        assert_eq!(fixture.content(), "alpha");
        assert!(fixture.actions.error.get_untracked().is_some());
        fixture.finish().await;
    }
}

#[wasm_bindgen_test]
async fn completion_waits_for_context_validation_and_keeps_snippet_history_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let item =
            serde_json::json!({"label":"beta", "insertText":"${1:beta}($0)", "insertTextFormat":2});
        let fixture = Fixture::new(mode, item.clone()).await;
        fixture
            .query(IntelligenceCommand::Query(Query::ResolveCompletion {
                id: "completion-1".into(),
            }))
            .await;
        let mut altered = item.clone();
        altered["label"] = "intruder".into();
        fixture.actions.apply_completion(&altered);
        settle().await;
        assert_eq!(fixture.validations(), 0);
        let release = fixture.hold_validation();
        fixture.actions.apply_completion(&item);
        wait_until("completion validation dispatched", || {
            fixture.validations() == 1
        })
        .await;
        assert_eq!(fixture.content(), "alpha");
        assert!(fixture.actions.busy.get_untracked());
        release.send(Ok(())).unwrap();
        wait_until("validated snippet publication", || {
            fixture.content() == "beta()"
        })
        .await;
        assert_eq!(fixture.editor.current_selection().unwrap().range(), 0..4);
        fixture
            .editor
            .command(
                EditorCommand::Undo,
                fixture.editor.current_selection().unwrap(),
                Indentation::default(),
            )
            .unwrap();
        assert_eq!(fixture.content(), "alpha");
        fixture.finish().await;
    }
}

#[derive(Clone, Copy, Debug)]
enum Change {
    Dependency,
    Selection,
    Dismiss,
    Provider,
    Account,
    Replacement,
}

#[wasm_bindgen_test]
async fn late_validation_cannot_publish_after_editor_scope_or_result_changes_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for change in [
            Change::Dependency,
            Change::Selection,
            Change::Dismiss,
            Change::Provider,
            Change::Account,
            Change::Replacement,
        ] {
            let fixture = Fixture::new(mode, formatting("beta")).await;
            let response = fixture.query(IntelligenceCommand::Formatting).await;
            let release = fixture.hold_validation();
            fixture
                .actions
                .apply_text
                .run(serde_json::from_value(response.result).unwrap());
            wait_until("original validation is pending", || {
                fixture.validations() == 1
            })
            .await;
            match change {
                Change::Dependency => {
                    fixture
                        .mounted
                        .state
                        .workspace
                        .editor_buffers
                        .update(|buffers| {
                            buffers.insert(
                                (1, "dependency.notes".into()),
                                EditorBuffer {
                                    content: "new dependency".into(),
                                    dirty: true,
                                    read_only: false,
                                },
                            );
                        });
                }
                Change::Selection => fixture
                    .editor
                    .record_selection(Selection::caret(1))
                    .unwrap(),
                Change::Dismiss => fixture.actions.dismiss.run(()),
                Change::Provider => fixture
                    .mounted
                    .state
                    .owner
                    .with(expect_context::<PluginsState>)
                    .project_plugins
                    .update(|plugins| plugins[0].enabled = false),
                Change::Account => fixture
                    .mounted
                    .state
                    .auth
                    .generation
                    .update(|generation| *generation = generation.wrapping_add(1)),
                Change::Replacement => {
                    *fixture
                        .mounted
                        .state
                        .fake
                        .native_language_result
                        .borrow_mut() = Some(formatting("gamma"));
                    let replacement = fixture.query(IntelligenceCommand::Formatting).await;
                    assert_eq!(replacement.result, formatting("gamma"));
                }
            }
            settle().await;
            let _ = release.send(Ok(()));
            wait_until("stale validation releases the editor", || {
                !fixture.actions.busy.get_untracked()
            })
            .await;
            settle().await;
            assert_eq!(fixture.content(), "alpha", "{mode:?}/{change:?}");
            assert!(fixture.mounted.state.workspace.dirty.get_untracked());
            if matches!(change, Change::Dependency) {
                fixture
                    .mounted
                    .state
                    .workspace
                    .editor_buffers
                    .with_untracked(|buffers| {
                        let dependency = &buffers[&(1, "dependency.notes".into())];
                        assert_eq!(dependency.content.as_str(), "new dependency");
                        assert!(dependency.dirty);
                    });
            }
            if matches!(change, Change::Replacement) {
                fixture
                    .actions
                    .apply_text
                    .run(serde_json::from_value(formatting("gamma")).unwrap());
                wait_until("fresh replacement can still apply", || {
                    fixture.content() == "gamma"
                })
                .await;
            }
            fixture.finish().await;
        }
    }
}

#[wasm_bindgen_test]
async fn dismissed_popup_preserves_diagnostics_polling_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let fixture = Fixture::new(
            mode,
            serde_json::json!([{"id":"completion-1","label":"beta"}]),
        )
        .await;
        let inputs = &fixture.mounted.state.fake.native_analysis_inputs;
        let synchronizations = || {
            inputs
                .borrow()
                .iter()
                .filter(|input| {
                    input.path == "draft.notes" && matches!(input.query, Query::Synchronize)
                })
                .count()
        };
        wait_until("initial diagnostics request", || synchronizations() > 0).await;
        fixture.actions.request.run(IntelligenceCommand::Suggest);
        wait_until("completion popup before dismissal", || {
            fixture.actions.popup.with_untracked(Option::is_some)
        })
        .await;
        let before = synchronizations();
        fixture.actions.dismiss_popup.run(());
        crate::support::wait_until_with_timeout(
            "diagnostics polling after popup dismissal",
            6500,
            || synchronizations() > before,
        )
        .await;
        assert!(fixture.actions.popup.get_untracked().is_none());
        assert_eq!(fixture.content(), "alpha");
        fixture.finish().await;
    }
}

#[wasm_bindgen_test]
async fn dismissed_completion_preserves_latest_source_diagnostics_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let fixture = Fixture::new(
            mode,
            serde_json::json!([{"id":"completion-1","label":"beta"}]),
        )
        .await;
        let inputs = &fixture.mounted.state.fake.native_analysis_inputs;
        wait_until("initial source synchronization", || {
            inputs
                .borrow()
                .iter()
                .any(|input| matches!(input.query, Query::Synchronize))
        })
        .await;
        let (release, response) = futures::channel::oneshot::channel();
        fixture
            .mounted
            .state
            .fake
            .native_completion_results
            .borrow_mut()
            .push_back(response);
        fixture.actions.request.run(IntelligenceCommand::Suggest);
        wait_until("automatic completion is held", || {
            inputs
                .borrow()
                .iter()
                .any(|input| matches!(input.query, Query::Completion { .. }))
        })
        .await;
        fixture.editor.insert_native_text(".", 1.0).unwrap();
        settle().await;
        // A subsequent suggestion coalesces behind the held request. Dismissing
        // it must revoke that popup without cancelling new-source diagnostics.
        fixture.actions.request.run(IntelligenceCommand::Suggest);
        fixture.actions.dismiss_popup.run(());
        openwebide_frontend::util::sleep_ms(450).await;
        assert_eq!(
            inputs
                .borrow()
                .iter()
                .filter(|input| matches!(input.query, Query::Completion { .. }))
                .count(),
            1,
            "Only one automatic transport may run, {mode:?}"
        );
        assert!(!inputs.borrow().iter().any(|input| {
            matches!(input.query, Query::Synchronize) && input.source == "alpha."
        }));
        release.send(Ok(())).unwrap();
        wait_until(
            "latest source diagnostics after dismissed completion",
            || {
                inputs.borrow().iter().any(|input| {
                    matches!(input.query, Query::Synchronize) && input.source == "alpha."
                })
            },
        )
        .await;
        settle().await;
        assert!(fixture.actions.popup.get_untracked().is_none());
        assert_eq!(fixture.content(), "alpha.");
        fixture.finish().await;
    }
}

#[wasm_bindgen_test]
async fn automatic_completion_keeps_its_popup_until_validated_publication_in_both_modes() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for reject in [false, true] {
            let fixture = Fixture::new(
                mode,
                serde_json::json!([{"id":"completion-1","label":"beta"}]),
            )
            .await;
            fixture.actions.request.run(IntelligenceCommand::Suggest);
            wait_until("completion suggestions", || {
                fixture.actions.popup.with_untracked(Option::is_some)
            })
            .await;
            *fixture
                .mounted
                .state
                .fake
                .native_language_result
                .borrow_mut() = Some(serde_json::json!({"label":"beta","insertText":"beta"}));
            let release = fixture.hold_validation();
            fixture
                .actions
                .request
                .run(IntelligenceCommand::InsertSuggestion("completion-1".into()));
            wait_until("automatic insertion validation", || {
                fixture.validations() == 1
            })
            .await;
            assert!(fixture.actions.popup.with_untracked(Option::is_some));
            assert!(fixture.actions.popup_busy.get_untracked());
            assert_eq!(fixture.content(), "alpha");
            if reject {
                *fixture
                    .mounted
                    .state
                    .fake
                    .native_analysis_error
                    .borrow_mut() = Some("dependency changed".into());
            }
            release.send(Ok(())).unwrap();
            wait_until("automatic validation settles", || {
                !fixture.actions.popup_busy.get_untracked()
            })
            .await;
            if reject {
                assert_eq!(fixture.content(), "alpha");
                assert!(fixture.actions.popup.with_untracked(Option::is_some));
                assert!(
                    fixture
                        .actions
                        .popup_error
                        .get_untracked()
                        .unwrap()
                        .contains("dependency changed")
                );
                *fixture
                    .mounted
                    .state
                    .fake
                    .native_analysis_error
                    .borrow_mut() = None;
                fixture
                    .actions
                    .request
                    .run(IntelligenceCommand::InsertSuggestion("completion-1".into()));
            }
            wait_until("current completion publication", || {
                fixture.content() == "beta"
            })
            .await;
            assert!(fixture.actions.popup.get_untracked().is_none());
            assert_eq!(fixture.actions.popup_error.get_untracked(), None);
            fixture.finish().await;
        }
    }
}
