//! Browser consumers use ordinary plugin invocations on both host transports.
//! The test host deliberately returns distinctive colors for a custom language;
//! actual component/grammar execution is covered by the native runtime matrix.
use super::{
    local_bridge::{
        HttpGuard, bridge_calls, fake_bridge_http, probe_folder, set_bridge_language_handler,
        set_bridge_plugin,
    },
    support::{editor_view, mount_test_with_backend, settle, wait_until},
};
use leptos::prelude::*;
use openwebide_core::{
    User, UserId, UserRole, WorkspaceMode,
    plugins::{ProjectPlugin, execution::InvokePlugin, languages::*},
};
use openwebide_frontend::{
    state::{
        git::HeadContent,
        languages::{LanguageStatus, LanguagesState},
        plugins::PluginsState,
    },
    state_actions::editor::{EditorActions, EditorCommand},
    testing::fake_backend::{FakeBackend, LanguageHandler},
};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};
use wasm_bindgen::{JsCast, prelude::*};
use wasm_bindgen_test::*;

fn binding() -> ProjectPlugin {
    serde_json::from_value(serde_json::json!({
        "id":7,"revision":1,"enabled":true,"prepared":{
            "source":{"repository":"https://example.test/notes.git","commit":"a".repeat(40),"path":"."},
            "digest":"b".repeat(64),"host_id":"fixture-host",
            "manifest":{"schemaVersion":1,"publisher":"example","name":"notes","version":"0.1.0",
                "displayName":"Notes","description":"Custom host language","license":"MIT",
                "compatibility":{"pluginApi":5},"executable":{"manifest":"Cargo.toml","library":"notes","sdkVersion":"0.1.0","capabilities":[]},
                "contributions":{"skills":[],"languages":[{"id":"notes","name":"Notes","extensions":["notes"],"features":{"syntax":true},"configuration":{"lineComment":"#"}}]}}
        }
    })).unwrap()
}

type Delayed = Rc<RefCell<Option<futures::channel::oneshot::Receiver<()>>>>;

#[wasm_bindgen_test]
async fn repository_changes_clear_language_results_without_discarding_drafts_in_both_modes() {
    use openwebide_core::{
        GitRepoStatus,
        plugins::languages::intelligence::{self, Query, Response},
    };
    use openwebide_frontend::state_actions::editor::EditorIntelligence;
    let previous = openwebide_frontend::idb::get_bridge_pairing_token()
        .await
        .unwrap();
    openwebide_frontend::idb::set_bridge_pairing_token("revision-language-test-token")
        .await
        .unwrap();
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let http = HttpGuard(fake_bridge_http());
        let mut plugin = binding();
        plugin.prepared.manifest.compatibility.plugin_api = 9;
        plugin.prepared.manifest.executable = None;
        plugin.prepared.manifest.contributions.languages = serde_json::from_value(serde_json::json!([
            {"id":"notes","name":"Notes","extensions":["notes"],"features":{"languageServer":{"command":"notes-lsp"}}}
        ])).unwrap();
        let mut local = plugin.prepared.clone();
        local.host_id = "paired-local-fixture".into();
        set_bridge_plugin(&http.0, &serde_json::to_string(&local).unwrap());
        let fake = Rc::new(FakeBackend::default());
        fake.project_plugin_entries
            .borrow_mut()
            .insert(1, vec![plugin]);
        *fake.native_language_result.borrow_mut() = Some(serde_json::json!([]));
        let slot = Rc::new(Cell::new(None::<EditorIntelligence>));
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
            state.workspace.content.set("unsaved draft".into());
            state.workspace.dirty.set(true);
            captured.set(Some(
                EditorActions::new(state.workspace)
                    .install_intelligence(Signal::derive(|| false))
                    .unwrap(),
            ));
            view! { <div/> }
        });
        let actions = slot.get().unwrap();
        wait_until("language identity is ready", || {
            actions.available.get_untracked()
        })
        .await;
        settle().await;
        mounted
            .state
            .workspace
            .register_editor_tab(1, "dependency.notes".into());
        mounted
            .state
            .workspace
            .editor_documents
            .update(|documents| {
                documents.insert(
                    (1, "dependency.notes".into()),
                    openwebide_core::editor::Document::new("cached dependency"),
                );
            });
        mounted.state.workspace.editor_buffers.update(|buffers| {
            buffers.insert(
                (1, "dependency.notes".into()),
                openwebide_frontend::state::workspace::EditorBuffer {
                    content: "effective unsaved dependency".into(),
                    dirty: true,
                    read_only: false,
                },
            );
        });
        settle().await;
        let response = Response {
            version: intelligence::VERSION,
            client: "client".into(),
            document: "draft".into(),
            request: 1,
            revision: 1,
            source_digest: source_digest("unsaved draft"),
            server: "service".into(),
            context: None,
            capabilities: serde_json::json!({}),
            result: serde_json::json!({"contents":"from previous revision"}),
            diagnostics: vec![],
            diagnostics_ready: true,
        };
        let query = Query::Hover {
            position: Default::default(),
        };
        actions.result.set(Some((query.clone(), response.clone())));
        actions.popup.set(Some((query.clone(), response.clone())));
        let revision = actions.context_revision.get_untracked();
        mounted.state.git.status.set(Some(GitRepoStatus {
            branch: "main".into(),
            commit_hash: "a".repeat(40),
            ..Default::default()
        }));
        settle().await;
        assert!(
            actions.result.get_untracked().is_some(),
            "Initial Git status must not cancel a language result"
        );
        assert!(actions.popup.get_untracked().is_some());
        assert_eq!(actions.context_revision.get_untracked(), revision);
        mounted.state.git.status.set(None);
        settle().await;
        assert!(
            actions.result.get_untracked().is_some(),
            "Unavailable Git status must retain the last known revision"
        );
        mounted.state.git.status.set(Some(GitRepoStatus {
            branch: "main".into(),
            commit_hash: "a".repeat(40),
            ..Default::default()
        }));
        settle().await;
        assert!(actions.result.get_untracked().is_some());
        mounted
            .state
            .workspace
            .dirty
            .set(mounted.state.workspace.dirty.get_untracked());
        mounted
            .state
            .workspace
            .editor_composition
            .set(mounted.state.workspace.editor_composition.get_untracked());
        mounted
            .state
            .workspace
            .open_file
            .set(mounted.state.workspace.open_file.get_untracked());
        mounted
            .state
            .workspace
            .content
            .set(mounted.state.workspace.content.get_untracked());
        mounted
            .state
            .workspace
            .active_session
            .set(mounted.state.workspace.active_session.get_untracked());
        mounted
            .state
            .projects
            .projects
            .set(mounted.state.projects.projects.get_untracked());
        mounted
            .state
            .languages
            .settings
            .set(mounted.state.languages.settings.get_untracked());
        let plugins = mounted.state.owner.with(expect_context::<PluginsState>);
        plugins
            .project_plugins
            .set(plugins.project_plugins.get_untracked());
        settle().await;
        assert!(
            actions.result.get_untracked().is_some(),
            "Unchanged editor notifications must not discard a language result"
        );
        assert!(actions.popup.get_untracked().is_some());
        assert_eq!(actions.context_revision.get_untracked(), revision);
        mounted
            .state
            .workspace
            .editor_documents
            .update(|documents| {
                documents.insert(
                    (1, "dependency.notes".into()),
                    openwebide_core::editor::Document::new("changed underlying cached document"),
                );
            });
        settle().await;
        assert!(
            actions.result.get_untracked().is_some(),
            "A retained buffer owns its effective source over a cached document"
        );
        mounted.state.workspace.editor_buffers.update(|buffers| {
            buffers
                .get_mut(&(1, "dependency.notes".into()))
                .unwrap()
                .content = "changed unsaved dependency".into();
        });
        wait_until("an unsaved dependency invalidates language results", || {
            actions.result.get_untracked().is_none() && actions.popup.get_untracked().is_none()
        })
        .await;
        assert_eq!(
            mounted.state.workspace.content.get_untracked().as_str(),
            "unsaved draft"
        );
        actions.result.set(Some((query.clone(), response.clone())));
        actions.popup.set(Some((query.clone(), response.clone())));
        mounted.state.git.status.update(|status| {
            status.as_mut().unwrap().ahead += 1;
        });
        settle().await;
        assert_eq!(actions.context_revision.get_untracked(), revision);
        assert!(actions.result.get_untracked().is_some());
        assert!(actions.popup.get_untracked().is_some());
        for commit in [false, true] {
            actions.result.set(Some((query.clone(), response.clone())));
            actions.popup.set(Some((query.clone(), response.clone())));
            let mut status = mounted.state.git.status.get_untracked().unwrap();
            if commit {
                status.commit_hash = "b".repeat(40);
            } else {
                mounted.state.git.status.set(None);
                settle().await;
                assert!(actions.result.get_untracked().is_some());
                status.branch = "topic".into();
            }
            mounted.state.git.status.set(Some(status));
            wait_until("repository change clears stale language UI", || {
                actions.result.get_untracked().is_none() && actions.popup.get_untracked().is_none()
            })
            .await;
            assert!(actions.context_revision.get_untracked() > revision);
            assert_eq!(
                mounted.state.workspace.content.get_untracked().as_str(),
                "unsaved draft"
            );
            assert!(mounted.state.workspace.dirty.get_untracked());
        }
        drop(mounted);
    }
    if let Some(previous) = previous {
        openwebide_frontend::idb::set_bridge_pairing_token(&previous)
            .await
            .unwrap();
    } else {
        openwebide_frontend::idb::delete_bridge_pairing_token()
            .await
            .unwrap();
    }
}

fn host_handler(
    fail: Rc<Cell<bool>>,
    inputs: Rc<RefCell<Vec<LanguageInput>>>,
    delay: Delayed,
    held: Rc<Cell<bool>>,
) -> LanguageHandler {
    let documents = Rc::new(RefCell::new(BTreeMap::<String, LanguageDocument>::new()));
    Rc::new(move |call: InvokePlugin| {
        let documents = documents.clone();
        let inputs = inputs.clone();
        let fail = fail.clone();
        let delay = delay.clone();
        let held = held.clone();
        Box::pin(async move {
            if fail.get() {
                return Err("Fixture host disconnected".into());
            }
            let input: LanguageInput = serde_json::from_str(&call.arguments).unwrap();
            inputs.borrow_mut().push(input.clone());
            match input {
                LanguageInput::Close { version, document } => {
                    documents.borrow_mut().remove(&document);
                    Ok(LanguageOutput::Closed { version, document })
                }
                LanguageInput::Analyze(request) => {
                    let previous = documents.borrow().get(&request.document).cloned();
                    let Some(document) = LanguageDocument::apply(previous.as_ref(), &request)?
                    else {
                        return Ok(analysis_response(&request, AnalysisStatus::NeedsSource));
                    };
                    let highlights = document
                        .source
                        .split('\n')
                        .map(|row| {
                            if row.is_empty() {
                                vec![]
                            } else {
                                vec![TokenSpan {
                                    end: row.len(),
                                    kind: TokenKind::String,
                                }]
                            }
                        })
                        .collect();
                    documents
                        .borrow_mut()
                        .insert(request.document.clone(), document);
                    let mut response = analysis_response(
                        &request,
                        AnalysisStatus::Ready {
                            incremental: previous.is_some(),
                        },
                    );
                    let LanguageOutput::Analyzed(response) = &mut response else {
                        unreachable!()
                    };
                    response.analysis = Some(SyntaxAnalysis {
                        highlights: Some(highlights),
                        folds: vec![],
                        structure: None,
                    });
                    let pending = delay.borrow_mut().take();
                    if let Some(pending) = pending {
                        held.set(true);
                        let _ = pending.await;
                        held.set(false);
                    }
                    Ok(LanguageOutput::Analyzed(response.clone()))
                }
            }
        })
    })
}

#[wasm_bindgen_test]
async fn custom_languages_color_editor_and_diff_through_both_hosts_and_recover_from_disablement_and_outages()
 {
    let previous = openwebide_frontend::idb::get_bridge_pairing_token()
        .await
        .unwrap();
    openwebide_frontend::idb::set_bridge_pairing_token("language-contract-token")
        .await
        .unwrap();
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let http = HttpGuard(fake_bridge_http());
        let plugin = binding();
        plugin.prepared.manifest.validate().unwrap();
        let mut host_prepared = plugin.prepared.clone();
        host_prepared.host_id = "paired-local-fixture".into();
        set_bridge_plugin(&http.0, &serde_json::to_string(&host_prepared).unwrap());
        let inputs = Rc::new(RefCell::new(vec![]));
        let fail = Rc::new(Cell::new(false));
        let delay = Rc::new(RefCell::new(None));
        let held = Rc::new(Cell::new(false));
        let handler = host_handler(fail.clone(), inputs.clone(), delay.clone(), held.clone());
        let bridge_handler = handler.clone();
        let callback = Closure::<dyn FnMut(String) -> js_sys::Promise>::new(move |wire: String| {
            let handler = bridge_handler.clone();
            wasm_bindgen_futures::future_to_promise(async move {
                let call = serde_json::from_str(&wire).unwrap();
                let output = handler(call)
                    .await
                    .map_err(|error| JsValue::from(js_sys::Error::new(&error)))?;
                Ok(JsValue::from_str(&serde_json::to_string(&output).unwrap()))
            })
        });
        set_bridge_language_handler(&http.0, callback.as_ref().unchecked_ref());
        let fake = Rc::new(FakeBackend::default());
        *fake.language_handler.borrow_mut() = Some(handler);
        fake.project_plugin_entries
            .borrow_mut()
            .insert(1, vec![plugin]);
        let folder = probe_folder();
        let captured = Rc::new(Cell::new(None));
        let slot = captured.clone();
        let mounted = mount_test_with_backend(fake.clone(), move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state
                .settings
                .bridge_url
                .set("ws://bridge.test:3001".into());
            state.projects.local_handles.update(|handles| {
                handles.insert(1, folder.unchecked_into());
            });
            state.auth.set_user(User {
                id: UserId::new(1),
                username: "owner".into(),
                role: UserRole::User,
                created_at: 0,
            });
            state.workspace.open_file.set(Some("fixture.notes".into()));
            state.workspace.content.set("note new 😀\r\n".into());
            state.git.head_content.set(Some(HeadContent {
                project_id: Some(1),
                path: "fixture.notes".into(),
                content: Ok("note old café\r\n".into()),
            }));
            slot.set(Some((
                expect_context::<PluginsState>(),
                expect_context::<LanguagesState>(),
                EditorActions::new(state.workspace),
            )));
            view! { <style>{include_str!("../../styles.css")}</style><div style="display:flex;width:600px;height:400px">{editor_view(state)}</div> }
        });
        let (plugins, language, actions) = captured.get().unwrap();
        wait_until("custom host language colors", || {
            if let LanguageStatus::Unavailable(error) = language.status.get_untracked()
                && language.settings_loaded.get_untracked()
                && language.selected.get_untracked().is_some()
            {
                panic!("{mode:?} language host failed: {error}");
            }
            language.status.get_untracked() == LanguageStatus::Ready
                && mounted.has_element(".tok-string")
        })
        .await;
        assert!(actions.command_supported(EditorCommand::LineComment));
        assert!(!inputs.borrow().is_empty());
        if mode == WorkspaceMode::Local {
            let calls: Vec<serde_json::Value> =
                serde_json::from_str(&bridge_calls(&http.0)).unwrap();
            assert!(calls.iter().any(|call| call["path"] == "/plugins/invoke"
                && call["body"]["call"]["operation"] == "language"));
            assert!(
                calls
                    .iter()
                    .filter(|call| call["path"] == "/plugins/invoke")
                    .all(|call| call["body"]["call"]["prepared"]["host_id"]
                        == "paired-local-fixture")
            );
            assert!(fake.language_inputs.borrow().is_empty());
        } else {
            assert!(!fake.language_inputs.borrow().is_empty());
        }

        let (release, pending) = futures::channel::oneshot::channel();
        *delay.borrow_mut() = Some(pending);
        mounted
            .state
            .workspace
            .content
            .set("note delayed 文😀\r\n".into());
        wait_until("in-flight host result", || held.get()).await;
        fake.project_plugin_entries
            .borrow_mut()
            .get_mut(&1)
            .unwrap()[0]
            .enabled = false;
        plugins
            .project_plugins
            .update(|plugins| plugins[0].enabled = false);
        wait_until("disabled provider clears editor colors", || {
            !mounted.has_element(".tok-string")
        })
        .await;
        release.send(()).unwrap();
        wait_until("obsolete host result returned", || !held.get()).await;
        settle().await;
        assert!(!mounted.has_element(".tok-string"));
        assert_eq!(language.status.get_untracked(), LanguageStatus::Plain);
        fake.project_plugin_entries
            .borrow_mut()
            .get_mut(&1)
            .unwrap()[0]
            .enabled = true;
        plugins
            .project_plugins
            .update(|plugins| plugins[0].enabled = true);
        wait_until("re-enabled editor provider", || {
            language.status.get_untracked() == LanguageStatus::Ready
                && mounted.has_element(".tok-string")
        })
        .await;

        fail.set(true);
        mounted
            .state
            .workspace
            .content
            .set("note offline 文😀\r\n".into());
        let deadline = js_sys::Date::now() + 3000.0;
        while !matches!(
            language.status.get_untracked(),
            LanguageStatus::Unavailable(_)
        ) || mounted.has_element(".tok-string")
        {
            assert!(
                js_sys::Date::now() < deadline,
                "{mode:?} outage: status {:?}, active {}, colors {}, preparation {:?}",
                language.status.get_untracked(),
                mounted.state.workspace.editor_worker_active.get_untracked(),
                mounted.has_element(".tok-string"),
                mounted.state.workspace.editor_preparation.get_untracked()
            );
            openwebide_frontend::util::sleep_ms(10).await;
            settle().await;
        }
        assert_eq!(
            mounted.state.workspace.content.get_untracked().as_str(),
            "note offline 文😀\r\n"
        );
        fail.set(false);
        language.retry.update(|retry| *retry += 1);
        wait_until("explicit host retry", || {
            language.status.get_untracked() == LanguageStatus::Ready
                && mounted.has_element(".tok-string")
        })
        .await;

        mounted.click_text("Changes");
        wait_until("plugin colors intersect diff word changes", || {
            mounted.has_element(".diff-line.add .tok-string.diff-word-add")
        })
        .await;
        let track = mounted.element(".inline-track");
        track.set_scroll_left(17.0);
        fake.project_plugin_entries
            .borrow_mut()
            .get_mut(&1)
            .unwrap()[0]
            .enabled = false;
        plugins
            .project_plugins
            .update(|plugins| plugins[0].enabled = false);
        assert!(!actions.command_supported(EditorCommand::LineComment));
        settle().await;
        wait_until("disabled preview colors cleared", || {
            !mounted.has_element(".tok-string")
        })
        .await;
        assert!(track.is_same_node(Some(&mounted.element(".inline-track"))));
        assert!(
            mounted
                .element(".inline-track")
                .text_content()
                .unwrap()
                .contains("note offline 文😀")
        );
        fake.project_plugin_entries
            .borrow_mut()
            .get_mut(&1)
            .unwrap()[0]
            .enabled = true;
        plugins
            .project_plugins
            .update(|plugins| plugins[0].enabled = true);
        wait_until("re-enabled preview colors", || {
            mounted.has_element(".diff-line.add .tok-string.diff-word-add")
        })
        .await;
        mounted.click_text("Split");
        wait_until("split preview consumes provider colors", || {
            mounted.has_element(".sbs-cell .tok-string")
        })
        .await;
        assert_eq!(
            mounted.state.workspace.content.get_untracked().as_str(),
            "note offline 文😀\r\n"
        );
        drop(mounted);
        settle().await;
    }
    match previous {
        Some(token) => openwebide_frontend::idb::set_bridge_pairing_token(&token)
            .await
            .unwrap(),
        None => openwebide_frontend::idb::delete_bridge_pairing_token()
            .await
            .unwrap(),
    }
}

#[wasm_bindgen_test]
async fn language_settings_controls_persist_defaults_and_project_overrides_in_both_modes() {
    use super::{language_support, support::choose_dropdown};
    use openwebide_frontend::{
        components::Settings,
        state_actions::settings::{SettingsActionContext, build_settings_actions},
    };

    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let host = language_support::LanguageHost::new().await;
        let mounted = host.mount(move |state| {
            state.seed_project();
            state
                .projects
                .projects
                .update(|projects| projects[0].mode = mode);
            state.workspace.open_file.set(Some("settings.rs".into()));
            state
                .workspace
                .content
                .set("fn main() { let text = \"文😀\"; }\r\n".into());
            state.settings.requested_tab.set(1);
            let actions = build_settings_actions(SettingsActionContext {
                api: state.api,
                settings: state.settings,
                ui: state.ui,
            });
            view! {
                {editor_view(state.clone())}
                <Settings on_set_notifications=actions.on_set_notifications
                    on_set_theme=actions.on_set_theme
                    on_set_editor_preferences=actions.on_set_editor_preferences
                    on_set_default_prompt=actions.on_set_default_prompt
                    on_set_bridge_url=actions.on_set_bridge_url />
            }
        });
        language_support::ready(&mounted.state).await;
        let warming = mounted
            .root
            .query_selector(
                ".ui-section[aria-label='Idle language analysis'] input[type='checkbox']",
            )
            .unwrap()
            .unwrap()
            .dyn_into::<web_sys::HtmlInputElement>()
            .unwrap();
        assert!(!warming.checked(), "Idle work must be opt-in");
        warming.click();
        wait_until("idle language analysis preference persists", || {
            mounted.state.languages.prefetch_enabled.get_untracked()
                && !mounted.state.languages.prefetch_saving.get_untracked()
                && mounted
                    .state
                    .fake
                    .settings
                    .borrow()
                    .get("language_prefetch")
                    .is_some_and(|value| value == "true")
        })
        .await;
        // Find the provider by its visible language label, independently of row order.
        let fields = mounted
            .root
            .query_selector_all(".ui-section[aria-label='Language providers'] .ui-field")
            .unwrap();
        let rust = (0..fields.length())
            .filter_map(|index| fields.item(index))
            .filter_map(|node| node.dyn_into::<web_sys::Element>().ok())
            .find(|field| {
                field
                    .query_selector(".setting-label")
                    .unwrap()
                    .is_some_and(|label| label.text_content().as_deref() == Some("Rust"))
            })
            .unwrap();
        rust.query_selector(".ui-dropdown-trigger")
            .unwrap()
            .unwrap()
            .set_attribute("id", "test-rust-provider")
            .unwrap();
        let source = mounted.state.workspace.content.get_untracked();
        let saved = || -> Option<LanguageSettings> {
            mounted
                .state
                .fake
                .settings
                .borrow()
                .get("language_settings")
                .map(|value| serde_json::from_str(value).unwrap())
        };
        let colored = || {
            mounted
                .root
                .query_selector(".editor-code .tok-keyword")
                .unwrap()
                .is_some()
        };
        choose_dropdown(&mounted, "#test-rust-provider", "disabled").await;
        wait_until("saved default disables syntax", || {
            !colored()
                && saved().is_some_and(|settings| {
                    settings.defaults.providers.get("rust") == Some(&ProviderPreference::Disabled)
                })
        })
        .await;
        choose_dropdown(
            &mounted,
            "button[aria-label='Language settings scope']",
            "project",
        )
        .await;
        choose_dropdown(&mounted, "#test-rust-provider", "automatic").await;
        language_support::ready(&mounted.state).await;
        wait_until("project choice restores host syntax", || {
            colored()
                && saved().is_some_and(|settings| {
                    settings.defaults.providers.get("rust") == Some(&ProviderPreference::Disabled)
                        && settings.for_project(1).providers.get("rust")
                            == Some(&ProviderPreference::Automatic)
                })
        })
        .await;
        let namespace = mounted
            .state
            .languages
            .selected
            .get_untracked()
            .unwrap()
            .namespace;
        choose_dropdown(&mounted, "#test-rust-provider", &namespace).await;
        wait_until("explicit project provider pin persists", || {
            saved().is_some_and(|settings| {
                settings.for_project(1).providers.get("rust")
                    == Some(&ProviderPreference::Provider {
                        namespace: namespace.clone(),
                    })
            })
        })
        .await;
        choose_dropdown(&mounted, "#test-rust-provider", "inherit").await;
        wait_until("inherit restores the disabled account default", || {
            !colored()
                && saved().is_some_and(|settings| {
                    settings
                        .projects
                        .get(&1)
                        .is_none_or(|project| !project.providers.contains_key("rust"))
                        && settings.for_project(1).providers.get("rust")
                            == Some(&ProviderPreference::Disabled)
                })
        })
        .await;
        choose_dropdown(
            &mounted,
            "button[aria-label='Language settings scope']",
            "default",
        )
        .await;
        choose_dropdown(&mounted, "#test-rust-provider", "automatic").await;
        language_support::ready(&mounted.state).await;
        wait_until("account default restores host syntax", || {
            colored()
                && saved().is_some_and(|settings| {
                    settings.defaults.providers.get("rust") == Some(&ProviderPreference::Automatic)
                })
        })
        .await;
        warming.click();
        wait_until(
            "idle analysis can be disabled independently of provider choices",
            || {
                !mounted.state.languages.prefetch_enabled.get_untracked()
                    && !mounted.state.languages.prefetch_saving.get_untracked()
                    && mounted
                        .state
                        .fake
                        .settings
                        .borrow()
                        .get("language_prefetch")
                        .is_some_and(|value| value == "false")
            },
        )
        .await;
        assert_eq!(mounted.state.workspace.content.get_untracked(), source);
        assert!(!mounted.state.workspace.dirty.get_untracked());
        drop(mounted);
        host.close().await;
    }
}

#[path = "language_plugins/application.rs"]
mod application;
