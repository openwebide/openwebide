#[test]
fn settings_body_limit_accepts_maximum_prompt_history() {
    for character in ['😀', '\0'] {
        let history: Vec<String> = (0..200)
            .map(|i| {
                let prefix = format!("{i:03}");
                prefix + &character.to_string().repeat(1997)
            })
            .collect();
        let value = serde_json::to_string(&history).unwrap();
        let body = serde_json::to_vec(&serde_json::json!({
            "key": "prompt_history",
            "value": value,
        }))
        .unwrap();
        assert!(body.len() <= super::SETTINGS_BODY_LIMIT);
    }
}

use super::auth::*;
use super::bridge::*;
use super::files::*;
use super::projects::*;
use super::sessions::*;
use super::*;

#[test]
fn run_plan_rebuilds_tool_history_and_falls_back_across_gaps() {
    futures::executor::block_on(async {
        let store = openwebide_storage::Store::new(crate::state::AppDb::open_in_memory().unwrap());
        store.migrate().await.unwrap();
        let user = store
            .insert_user("u", "hash", openwebide_core::UserRole::Admin, 1)
            .await
            .unwrap();
        let connection = store
            .insert_connection(&NewConnection {
                name: "server".into(),
                kind: openwebide_core::ProviderKind::LlamaCpp,
                base_url: "http://server".into(),
                model: None,
                context_limit: None,
            })
            .await
            .unwrap();
        let state = AppState { store };
        for (row_count, complete_second) in [(2, true), (2, false), (1, true)] {
            let session = state
                .store
                .create_session("s", Some(connection.id), None, None, user.id, 1)
                .await
                .unwrap();
            let calls: Vec<_> = (0..2)
                .map(|i| openwebide_core::ToolCall {
                    id: format!("wire-{i}"),
                    name: "read_file".into(),
                    arguments: format!("{{\"path\":\"{i}\"}}"),
                })
                .collect();
            let interim = state
                .store
                .insert_interim_message(
                    session.id,
                    Role::Assistant,
                    "checking",
                    1,
                    None,
                    Some(&calls),
                )
                .await
                .unwrap();
            for i in 0..row_count {
                let id = format!("step-{i}");
                state
                    .store
                    .upsert_tool_step(session.id, interim.id, &id, "read_file", "read", 1, None)
                    .await
                    .unwrap();
                if i == 0 || complete_second {
                    state
                        .store
                        .complete_tool_step(
                            user.id,
                            session.id,
                            &id,
                            true,
                            &format!("result-{i}"),
                            None,
                        )
                        .await
                        .unwrap();
                }
            }
            let plan = build_run_plan(
                &state,
                user.id,
                session.id,
                SendMessageBody {
                    content: "next".into(),
                    model: None,
                    editor_context: None,
                    browser_preferences: None,
                    queued_prompt: None,
                },
            )
            .await
            .unwrap();
            let history = plan.request.messages;
            assert_eq!(history[0].content, "checking");
            assert_eq!(history.len(), 3);
            assert_eq!(history[0].tool_calls.as_ref(), Some(&calls));
            for i in 0..2 {
                assert_eq!(history[i + 1].role, Role::Tool);
                assert_eq!(
                    history[i + 1].tool_call_id.as_deref(),
                    Some(calls[i].id.as_str())
                );
                assert_eq!(
                    history[i + 1].content,
                    if i >= row_count || (i == 1 && !complete_second) {
                        openwebide_core::chat::UNRECORDED_TOOL_RESULT.into()
                    } else {
                        format!("result-{i}")
                    }
                );
            }
        }
    });
}

#[derive(Clone, Default)]
struct MemoHttp(std::sync::Arc<std::sync::Mutex<Vec<bool>>>);

impl openwebide_llm::HttpClient for MemoHttp {
    async fn get_json(&self, _: &str) -> Result<serde_json::Value, openwebide_llm::ProviderError> {
        unreachable!()
    }
    async fn post_json(
        &self,
        _: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, openwebide_llm::ProviderError> {
        self.0
            .lock()
            .unwrap()
            .push(body["stream"].as_bool().unwrap());
        Ok(json!({"choices":[{"message":{"content":"done"}}]}))
    }
    fn post_stream(
        &self,
        _: &str,
        _: &serde_json::Value,
    ) -> std::pin::Pin<
        Box<
            dyn futures::Stream<Item = Result<Bytes, openwebide_llm::ProviderError>>
                + Send
                + 'static,
        >,
    > {
        self.0.lock().unwrap().push(true);
        Box::pin(futures::stream::empty())
    }
}

#[test]
fn persisted_memo_skips_streaming() {
    futures::executor::block_on(async {
        let connection = openwebide_core::Connection {
            id: 1,
            name: "server".into(),
            kind: openwebide_core::ProviderKind::LlamaCpp,
            base_url: "http://server".into(),
            model: None,
            enabled: true,
            context_limit: None,
            tool_stream_unsupported: true,
            tool_stream_revision: 0,
            tool_selection: Default::default(),
        };
        let http = MemoHttp::default();
        let provider = Provider::for_connection(&connection, http.clone());
        let request = ChatRequest {
            model_settings: Default::default(),
            connection_id: 1,
            system_prompt: None,
            model: Some("model".into()),
            messages: vec![],
            tools: vec![],
        };
        let chunks = provider
            .chat_tools_stream(&request)
            .collect::<Vec<_>>()
            .await;
        assert!(chunks.iter().all(Result::is_ok));
        assert_eq!(*http.0.lock().unwrap(), vec![false]);
    });
}

#[test]
fn run_plan_prepares_chat_and_remote_agent_without_mutations() {
    futures::executor::block_on(async {
        let store = openwebide_storage::Store::new(crate::state::AppDb::open_in_memory().unwrap());
        store.migrate_with(&|_| true).await.unwrap();
        let user = store
            .insert_user("alice", "hash", openwebide_core::UserRole::Admin, 1)
            .await
            .unwrap();
        let other = store
            .insert_user("bob", "hash", openwebide_core::UserRole::User, 1)
            .await
            .unwrap();
        let connection = store
            .insert_connection(&NewConnection {
                name: "local".into(),
                kind: openwebide_core::ProviderKind::Ollama,
                base_url: "http://localhost:11434".into(),
                model: Some("model".into()),
                context_limit: None,
            })
            .await
            .unwrap();
        let prompt = store
            .insert_system_prompt(user.id, "coder", "Be helpful")
            .await
            .unwrap();
        let project = store
            .create_project(
                &NewProject {
                    name: "app".into(),
                    mode: WorkspaceMode::Remote,
                    path: Some("repos/app".into()),
                },
                user.id,
                1,
            )
            .await
            .unwrap();
        let state = AppState { store };
        for project_id in [None, Some(project.id)] {
            let session = state
                .store
                .create_session(
                    "s",
                    Some(connection.id),
                    Some(prompt.id),
                    project_id,
                    user.id,
                    1,
                )
                .await
                .unwrap();
            let history = state
                .store
                .insert_message(session.id, Role::Assistant, "<think>r</think>earlier", 2)
                .await
                .unwrap();
            state.store.request_cancel(session.id, 1000).await.unwrap();
            let context = EditorContext {
                file_path: "src/main.rs".into(),
                cursor_line: 1,
                cursor_col: 1,
                selection: None,
            };
            let make_body = || SendMessageBody {
                content: "go".into(),
                model: Some("chosen".into()),
                editor_context: Some(context.clone()),
                browser_preferences: Some(openwebide_core::BrowserPreferences {
                    timezone: Some("America/Chicago".into()),
                    locale: Some("en-US".into()),
                    hour_cycle: Some("h12".into()),
                    utc_offset_minutes: Some(-300),
                }),
                queued_prompt: None,
            };
            let plan = build_run_plan(&state, user.id, session.id, make_body())
                .await
                .unwrap();
            assert_eq!(
                plan.user_content,
                if project_id.is_some() {
                    format!("{}go", context.format_prompt_injection())
                } else {
                    "go".into()
                }
            );
            assert_eq!(
                plan.environment.browser_preferences,
                make_body().browser_preferences
            );
            assert!(
                openwebide_agent::context::chat_context(&plan.environment)
                    .contains("America/Chicago")
            );
            let mut resolved_connection = connection.clone();
            resolved_connection.model = Some("chosen".into());
            assert_eq!(plan.connection, resolved_connection);
            assert_eq!(
                state.store.get_connection(connection.id).await.unwrap(),
                connection.clone()
            );
            assert_eq!(plan.request.connection_id, connection.id);
            assert_eq!(plan.request.model.as_deref(), Some("chosen"));
            assert!(
                plan.request
                    .system_prompt
                    .as_ref()
                    .unwrap()
                    .starts_with("Be helpful\n\nCurrent Date & Time: ")
            );
            let mut model_history = history.clone();
            model_history.content = "earlier".into();
            assert_eq!(plan.request.messages, vec![model_history]);
            if project_id.is_some() {
                assert_eq!(
                    plan.kind,
                    RunKind::Agent {
                        project_path: "repos/app".into()
                    }
                );
                let mut tools = workspace_tools();
                openwebide_agent::plugins::configure(
                    &mut tools,
                    &mut None,
                    &openwebide_agent::plugins::PluginContext {
                        bindings: &[],
                        memories: &openwebide_core::ProjectMemories::default(),
                        skills: &openwebide_core::ProjectSkills::default(),
                        context_limit: None,
                    },
                );
                tools.push(openwebide_agent::tasks::executor::definition());
                assert_eq!(plan.request.tools, tools);
            } else {
                assert_eq!(plan.kind, RunKind::WebChat);
                let mut tools = openwebide_agent::session::projectless_tools();
                openwebide_agent::plugins::configure(
                    &mut tools,
                    &mut None,
                    &openwebide_agent::plugins::PluginContext {
                        bindings: &[],
                        memories: &openwebide_core::ProjectMemories {
                            enabled: false,
                            entries: Vec::new(),
                        },
                        skills: &openwebide_core::ProjectSkills {
                            enabled: false,
                            entries: Vec::new(),
                        },
                        context_limit: None,
                    },
                );
                tools.push(openwebide_agent::tasks::executor::definition());
                assert_eq!(plan.request.tools, tools);
            }
            let mut configured = state.store.get_connection(connection.id).await.unwrap();
            for selection in [
                openwebide_core::ToolSelection::Selected(vec![
                    "read_file".into(),
                    "search_web".into(),
                ]),
                openwebide_core::ToolSelection::ChatOnly,
            ] {
                configured.tool_selection = selection.clone();
                state.store.update_connection(&configured).await.unwrap();
                let selected = build_run_plan(&state, user.id, session.id, make_body())
                    .await
                    .unwrap();
                assert!(
                    selected
                        .request
                        .tools
                        .iter()
                        .all(|tool| selection.allows(&tool.name))
                );
                assert!(
                    !selected
                        .request
                        .tools
                        .iter()
                        .any(|tool| tool.name == "task")
                );
                if selection == openwebide_core::ToolSelection::ChatOnly {
                    assert_eq!(selected.kind, RunKind::Chat);
                    assert!(selected.request.tools.is_empty());
                }
            }
            configured.tool_selection = openwebide_core::ToolSelection::All;
            state.store.update_connection(&configured).await.unwrap();
            assert_eq!(
                state.store.list_messages(session.id).await.unwrap(),
                vec![history]
            );
            assert!(
                state
                    .store
                    .cancel_requested_since(session.id, 0)
                    .await
                    .unwrap()
            );
            assert!(
                build_run_plan(&state, other.id, session.id, make_body())
                    .await
                    .is_err()
            );
        }
    });
}

#[test]
fn test_health() {
    let resp = health();
    assert_eq!(resp.status(), 200);
}

#[test]
fn test_raw_headers() {
    let headers = raw_headers("logo.svg");
    assert!(headers.contains(&("x-content-type-options", "nosniff".to_string())));
    assert!(headers.contains(&("content-security-policy", "sandbox".to_string())));
    assert!(headers.contains(&("content-type", "image/svg+xml".to_string())));

    let headers = raw_headers("x.html");
    assert!(headers.contains(&("x-content-type-options", "nosniff".to_string())));
    assert!(headers.contains(&("content-security-policy", "sandbox".to_string())));
    assert!(headers.contains(&("content-type", "application/octet-stream".to_string())));
    assert!(headers.contains(&("content-disposition", "attachment".to_string())));

    let headers = raw_headers("a.png");
    assert!(headers.contains(&("content-security-policy", "sandbox".to_string())));
    assert!(headers.contains(&("content-type", "image/png".to_string())));
}

#[test]
fn test_normalize_project_path() {
    use openwebide_core::WorkspaceMode;
    assert_eq!(
        normalize_project_path(WorkspaceMode::Remote, Some("repos/x/".to_string()))
            .map_err(|_| ())
            .unwrap()
            .unwrap(),
        "repos/x"
    );
    assert_eq!(
        normalize_project_path(WorkspaceMode::Remote, Some("./a//b".to_string()))
            .map_err(|_| ())
            .unwrap()
            .unwrap(),
        "a/b"
    );
    assert!(normalize_project_path(WorkspaceMode::Remote, Some("../x".to_string())).is_err());
    assert_eq!(
        normalize_project_path(WorkspaceMode::Remote, Some("/".to_string()))
            .map_err(|_| ())
            .unwrap()
            .unwrap(),
        ""
    );
    assert_eq!(
        normalize_project_path(WorkspaceMode::Remote, Some("///".to_string()))
            .map_err(|_| ())
            .unwrap()
            .unwrap(),
        ""
    );
    assert_eq!(
        normalize_project_path(WorkspaceMode::Remote, Some("".to_string()))
            .map_err(|_| ())
            .unwrap()
            .unwrap(),
        ""
    );
}

#[test]
fn test_strip_base() {
    use openwebide_core::FileEntry;
    let e = vec![FileEntry {
        name: "main.rs".into(),
        path: "repos/x/src/main.rs".into(),
        is_dir: false,
        size: 0,
    }];
    let stripped = strip_base("repos/x/", e);
    assert_eq!(stripped[0].path, "src/main.rs");
}

#[test]
fn read_limited_enforces_byte_limit() {
    futures::executor::block_on(async {
        let limit = 10;
        // limit-1 and limit bytes are accepted.
        for n in [limit - 1, limit] {
            let body = http_body_util::Full::new(Bytes::from(vec![b'x'; n]));
            let text = super::read_limited(http_body_util::Limited::new(body, limit), limit)
                .await
                .unwrap();
            assert_eq!(text, "x".repeat(n));
        }
        // limit+1 bytes is refused with 413, not a generic 400.
        let body = http_body_util::Full::new(Bytes::from(vec![b'x'; limit + 1]));
        let err = super::read_limited(http_body_util::Limited::new(body, limit), limit)
            .await
            .unwrap_err();
        assert_eq!(err.into_response().status().as_u16(), 413);
    });
}

#[test]
fn read_limited_rejects_non_utf8() {
    futures::executor::block_on(async {
        let body = http_body_util::Full::new(Bytes::from(vec![0xff, 0xfe, 0xfd]));
        let err = super::read_limited(http_body_util::Limited::new(body, 1024), 1024)
            .await
            .unwrap_err();
        assert_eq!(err.into_response().status().as_u16(), 400);
    });
}

#[test]
fn include_ignored_flag_parsing() {
    assert!(is_include_ignored(Some("1".into())));
    assert!(is_include_ignored(Some("true".into())));
    assert!(!is_include_ignored(None));
    assert!(!is_include_ignored(Some("0".into())));
    assert!(!is_include_ignored(Some("yes".into())));
    assert!(!is_include_ignored(Some("TRUE".into())));
    assert!(!is_include_ignored(Some("".into())));
}

#[test]
fn test_bridge_token() {
    use crate::state::{AppDb, AppState};
    use openwebide_core::UserRole;
    use openwebide_storage::Store;

    futures::executor::block_on(async {
        let db = AppDb::open_in_memory().unwrap();
        let store = Store::new(db);
        store.migrate().await.unwrap();
        let user_record = store
            .insert_user("u", "hash", UserRole::User, 1)
            .await
            .unwrap();
        let user = user_record.public();

        let state = AppState { store };

        // Without secret
        let err = bridge_token(&state, AuthedUser::from(user.clone()))
            .await
            .unwrap_err();
        assert_eq!(err.into_response().status().as_u16(), 503);

        // With secret
        let test_secret = "test-secret-12345678901234567890";
        state
            .store
            .set_setting("bridge_secret_cache", test_secret)
            .await
            .unwrap();

        let resp = bridge_token(&state, AuthedUser::from(user.clone()))
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);

        let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

        let token = json["token"].as_str().unwrap();
        let expires_at = json["expires_at"].as_i64().unwrap();

        let now = crate::state::now();
        assert!(expires_at > now);
        assert!(expires_at <= now + 120);

        let claims = openwebide_auth::verify_token_at(test_secret, token, now).unwrap();
        assert_eq!(claims.user_id, user.id.get());
    });
}

#[test]
fn project_database_error_does_not_downgrade_to_chat() {
    use openwebide_storage::db::Db;
    futures::executor::block_on(async {
        let path = std::env::temp_dir().join(format!(
            "openwebide-project-error-{}.sqlite",
            std::process::id()
        ));
        let db = crate::state::AppDb::open(&path).unwrap();
        let store = openwebide_storage::Store::new(db);
        store.migrate().await.unwrap();
        let user = store
            .insert_user("db-error", "hash", openwebide_core::UserRole::User, 1)
            .await
            .unwrap();
        let connection = store
            .insert_connection(&NewConnection {
                name: "server".into(),
                kind: openwebide_core::ProviderKind::LlamaCpp,
                base_url: "http://server".into(),
                model: None,
                context_limit: None,
            })
            .await
            .unwrap();
        let project = store
            .create_project(
                &NewProject {
                    name: "remote".into(),
                    mode: WorkspaceMode::Remote,
                    path: Some("repo".into()),
                },
                user.id,
                1,
            )
            .await
            .unwrap();
        let session = store
            .create_session("s", Some(connection.id), None, Some(project.id), user.id, 1)
            .await
            .unwrap();
        let state = AppState { store };
        let sabotage = crate::state::AppDb::open(&path).unwrap();
        sabotage
            .execute("ALTER TABLE projects RENAME TO unavailable_projects", &[])
            .await
            .unwrap();
        let result = build_run_plan(
            &state,
            user.id,
            session.id,
            SendMessageBody {
                content: "hello".into(),
                model: None,
                editor_context: None,
                browser_preferences: None,
                queued_prompt: None,
            },
        )
        .await;
        drop(state);
        drop(sabotage);
        std::fs::remove_file(path).unwrap();
        let error = result.unwrap_err();
        assert!(error.to_string().contains("projects"));
        assert_eq!(error.into_response().status().as_u16(), 500);
    });
}

#[test]
fn pending_edit_handlers_and_resolution_body_contract() {
    futures::executor::block_on(async {
        let state = AppState::new().await.unwrap();
        let user = state
            .store
            .insert_user("owner", "hash", openwebide_core::UserRole::User, 1)
            .await
            .unwrap()
            .public();
        let other = state
            .store
            .insert_user("other", "hash", openwebide_core::UserRole::User, 1)
            .await
            .unwrap()
            .public();
        let project = state
            .store
            .create_project(
                &NewProject {
                    name: "p".into(),
                    mode: WorkspaceMode::Remote,
                    path: None,
                },
                user.id,
                1,
            )
            .await
            .unwrap();
        let session = state
            .store
            .create_session("s", None, None, Some(project.id), user.id, 1)
            .await
            .unwrap();
        let diff = FileDiff {
            path: "nested/a.txt".into(),
            old: None,
            new: "new".into(),
            old_unavailable: false,
            backup_path: None,
        };
        state
            .store
            .upsert_tool_step(session.id, 1, "source", "write_file", "write", 1, None)
            .await
            .unwrap();
        state
            .store
            .complete_tool_step(user.id, session.id, "source", true, "written", Some(&diff))
            .await
            .unwrap();
        let path = format!("/api/projects/{}/pending-edits", project.id);
        let response = list_pending_edits(&state, &path, AuthedUser::from(user.clone()))
            .await
            .unwrap();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let edits: Vec<openwebide_core::PersistedEdit> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(edits[0].diff, diff);
        assert!(
            list_pending_edits(&state, &path, AuthedUser::from(other))
                .await
                .is_err()
        );
        assert!(
            list_pending_edits(
                &state,
                &(path.clone() + "/extra"),
                AuthedUser::from(user.clone())
            )
            .await
            .is_err()
        );
        for body in [
            r#"{"path":"a","revision":"bad","decision":"accepted"}"#,
            r#"{"path":"a","revision":1,"decision":"unknown"}"#,
            r#"{"path":"a","decision":"rejected"}"#,
        ] {
            assert!(parse_json::<openwebide_core::ResolveEditRequest>(body.into()).is_err());
        }
        let request: openwebide_core::ResolveEditRequest =
            parse_json(r#"{"path":"nested/a.txt","revision":1,"decision":"rejected"}"#.into())
                .unwrap();
        let resolved = state
            .store
            .resolve_pending_edit(user.id, project.id, &request)
            .await
            .unwrap();
        assert_eq!(resolved.decision, openwebide_core::EditDecision::Rejected);
        assert!(
            state
                .store
                .list_pending_edits(user.id, project.id)
                .await
                .unwrap()
                .is_empty()
        );
    });
}

#[test]
fn resolution_validation_rejects_pending_and_nonpositive_revisions() {
    use openwebide_core::{EditDecision, ResolveEditRequest};
    for revision in [-1, 0, 1] {
        for decision in [
            EditDecision::Pending,
            EditDecision::Accepted,
            EditDecision::Rejected,
        ] {
            let request = ResolveEditRequest {
                path: "a".into(),
                revision,
                decision,
            };
            assert_eq!(
                validate_edit_resolution(&request).is_ok(),
                revision > 0 && decision != EditDecision::Pending
            );
        }
    }
}

#[test]
fn browser_runtime_scrubs_secrets_and_native_runtime_keeps_them() {
    futures::executor::block_on(async {
        let store = openwebide_storage::Store::new(crate::state::AppDb::open_in_memory().unwrap());
        store.migrate().await.unwrap();
        let user = store
            .insert_user("u", "hash", openwebide_core::UserRole::Admin, 1)
            .await
            .unwrap();
        let connection = store
            .insert_connection(&NewConnection {
                name: "server".into(),
                kind: openwebide_core::ProviderKind::Ollama,
                base_url: "http://localhost:11434".into(),
                model: Some("main".into()),
                context_limit: None,
            })
            .await
            .unwrap();
        store
            .save_server_settings(
                connection.id,
                &openwebide_core::ServerSettingsUpdate {
                    api_key: Some("test-secret".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let state = AppState { store };
        for native in [false, true] {
            let runtime = super::model_setup::runtime(&state, user.id, connection.id, None)
                .await
                .unwrap();
            let response = super::model_setup::runtime_response(runtime, native);
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let runtime: openwebide_core::ModelRuntime = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                runtime.transport.api_key.as_deref(),
                native.then_some("test-secret")
            );
            assert_eq!(runtime.settings.auto_compact_threshold, Some(85));
            assert_eq!(runtime.settings.fast.unwrap().model, "main");
        }
    });
}

#[test]
fn approval_modes_are_user_scoped_and_edit_mode_never_approves_commands() {
    futures::executor::block_on(async {
        use openwebide_core::{ApprovalCheck, ApprovalMode, ToolCall};
        let store = openwebide_storage::Store::new(crate::state::AppDb::open_in_memory().unwrap());
        store.migrate().await.unwrap();
        let user = store
            .insert_user("alice", "hash", openwebide_core::UserRole::Admin, 1)
            .await
            .unwrap();
        let other = store
            .insert_user("bob", "hash", openwebide_core::UserRole::User, 1)
            .await
            .unwrap();
        let session = store
            .create_session("s", None, None, None, user.id, 1)
            .await
            .unwrap();
        for (mode, write, command) in [
            (ApprovalMode::Default, false, false),
            (ApprovalMode::AutoAcceptEdits, true, false),
            (ApprovalMode::Yolo, true, true),
        ] {
            store
                .set_user_setting(
                    user.id,
                    &ApprovalMode::setting_key(session.id),
                    &serde_json::to_string(&mode).unwrap(),
                )
                .await
                .unwrap();
            for (tool, expected) in [("write_file", write), ("run_command", command)] {
                let check = ApprovalCheck {
                    connection_id: 1,
                    model: None,
                    call: ToolCall {
                        id: "a1t1c0".into(),
                        name: tool.into(),
                        arguments: "{}".into(),
                    },
                };
                assert_eq!(
                    super::approvals::decision(&store, user.id, session.id, &check)
                        .await
                        .unwrap()
                        .approved,
                    expected
                );
                assert!(
                    super::approvals::decision(&store, other.id, session.id, &check)
                        .await
                        .is_err()
                );
            }
        }
    });
}

#[test]
fn auto_classifier_uses_primary_without_fast_model_and_respects_fast_override() {
    futures::executor::block_on(async {
        use openwebide_core::{ApprovalCheck, ModelDefaults, ModelSelection, ToolCall};
        let store = openwebide_storage::Store::new(crate::state::AppDb::open_in_memory().unwrap());
        store.migrate().await.unwrap();
        let user = store
            .insert_user("u", "hash", openwebide_core::UserRole::Admin, 1)
            .await
            .unwrap();
        let server = store
            .insert_connection(&NewConnection {
                name: "server".into(),
                kind: openwebide_core::ProviderKind::Ollama,
                base_url: "http://localhost:11434".into(),
                model: Some("main".into()),
                context_limit: None,
            })
            .await
            .unwrap();
        let session = store
            .create_session("s", Some(server.id), None, None, user.id, 1)
            .await
            .unwrap();
        store
            .insert_message(session.id, Role::User, "Update the README", 2)
            .await
            .unwrap();
        let check = ApprovalCheck {
            connection_id: server.id,
            model: Some("selected-main".into()),
            call: ToolCall {
                id: "a1t1c0".into(),
                name: "write_file".into(),
                arguments: r#"{"path":"README.md","content":"updated"}"#.into(),
            },
        };
        let (_, request) = super::approvals::classifier_plan(&store, user.id, session.id, &check)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request.model.as_deref(), Some("selected-main"));
        assert!(request.messages[0].content.contains("Update the README"));
        store
            .save_model_defaults(
                user.id,
                &ModelDefaults {
                    fast: Some(ModelSelection {
                        server_id: server.id,
                        model: "fast".into(),
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let (_, request) = super::approvals::classifier_plan(&store, user.id, session.id, &check)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request.model.as_deref(), Some("fast"));
        assert_eq!(request.model_settings.tools, Some(false));
        assert!(request.tools.is_empty());
    });
}

#[test]
fn project_file_paths_normalize_before_joining_and_do_not_escape_project() {
    futures::executor::block_on(async {
        let store = openwebide_storage::Store::new(crate::state::AppDb::open_in_memory().unwrap());
        store.migrate_with(&|_| true).await.unwrap();
        let user = store
            .insert_user("paths", "hash", openwebide_core::UserRole::Admin, 1)
            .await
            .unwrap();
        let project = store
            .create_project(
                &NewProject {
                    name: "app".into(),
                    mode: WorkspaceMode::Remote,
                    path: Some("repos/app".into()),
                },
                user.id,
                1,
            )
            .await
            .unwrap();
        let state = AppState { store };
        assert_eq!(
            remote_project_path(&state, user.id, project.id, "\\src\\.\\a.rs")
                .await
                .unwrap()
                .0,
            "repos/app/src/a.rs"
        );
        for path in ["../other", "src/../../other", ".spin/db", "a/.spin/db"] {
            assert_eq!(
                remote_project_path(&state, user.id, project.id, path)
                    .await
                    .unwrap_err()
                    .into_response()
                    .status()
                    .as_u16(),
                400
            );
        }
    });
}

#[test]
fn empty_search_queries_return_400_before_filesystem_access() {
    for value in [None, Some(String::new()), Some(" \t\n".into())] {
        let error = super::files::search_query(value).unwrap_err();
        assert_eq!(error.into_response().status().as_u16(), 400);
    }
    assert_eq!(
        super::files::search_query(Some("hello".into())).unwrap(),
        "hello"
    );
}

#[test]
fn todo_endpoints_share_owned_validation_and_durability_in_every_workspace() {
    futures::executor::block_on(async {
        for mode in [
            Some(WorkspaceMode::Local),
            Some(WorkspaceMode::Remote),
            None,
        ] {
            let state = AppState::new().await.unwrap();
            let owner = state
                .store
                .insert_user("owner", "hash", openwebide_core::UserRole::Admin, 1)
                .await
                .unwrap();
            let other = state
                .store
                .insert_user("other", "hash", openwebide_core::UserRole::User, 1)
                .await
                .unwrap();
            let project = if let Some(mode) = mode {
                Some(
                    state
                        .store
                        .create_project(
                            &NewProject {
                                name: "project".into(),
                                mode,
                                path: Some("test".into()),
                            },
                            owner.id,
                            1,
                        )
                        .await
                        .unwrap()
                        .id,
                )
            } else {
                None
            };
            let session = state
                .store
                .create_session("session", None, None, project, owner.id, 1)
                .await
                .unwrap()
                .id;
            let prompt = state
                .store
                .insert_message(session, Role::User, "task", 1)
                .await
                .unwrap();
            let user = AuthedUser {
                id: owner.id,
                role: owner.role,
            };
            let other = AuthedUser {
                id: other.id,
                role: other.role,
            };
            let path = format!("/api/sessions/{session}/todos");
            let plan = openwebide_core::TodoPlan {
                todos: vec![openwebide_core::TodoItem {
                    id: "inspect".into(),
                    content: "Inspect the code".into(),
                    status: openwebide_core::TodoStatus::Pending,
                }],
            };
            let request = |anchor, plan: &openwebide_core::TodoPlan| TodoPlanBody {
                anchor_message_id: anchor,
                plan: plan.clone(),
            };
            assert_eq!(
                get_todo_plan(&state, &path, user).await.unwrap().status(),
                200
            );
            assert_eq!(
                write_todo_plan_body(&state, session, user, request(prompt.id, &plan))
                    .await
                    .unwrap()
                    .status(),
                201
            );
            let response = get_todo_plan(&state, &path, user)
                .await
                .unwrap()
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes();
            let persisted: openwebide_core::TodoUpdate = serde_json::from_slice(&response).unwrap();
            assert_eq!(persisted.plan, plan);
            assert_eq!(persisted.anchor_message_id, prompt.id);
            assert_eq!(
                get_todo_plan(&state, &path, other)
                    .await
                    .unwrap_err()
                    .into_response()
                    .status(),
                404
            );
            assert_eq!(
                write_todo_plan_body(&state, session, other, request(prompt.id, &plan))
                    .await
                    .unwrap_err()
                    .into_response()
                    .status(),
                404
            );
            let invalid = openwebide_core::TodoPlan {
                todos: vec![openwebide_core::TodoItem {
                    id: "bad".into(),
                    content: String::new(),
                    status: openwebide_core::TodoStatus::Pending,
                }],
            };
            assert_eq!(
                write_todo_plan_body(&state, session, user, request(prompt.id, &invalid))
                    .await
                    .unwrap_err()
                    .into_response()
                    .status(),
                400
            );
            state
                .store
                .insert_message(session, Role::User, "new task", 2)
                .await
                .unwrap();
            assert_eq!(
                write_todo_plan_body(&state, session, user, request(prompt.id, &plan))
                    .await
                    .unwrap_err()
                    .into_response()
                    .status(),
                409
            );
            assert_eq!(
                state.store.get_todo_plan(user.id, session).await.unwrap(),
                Some(persisted)
            );
        }
    });
}

#[test]
fn editor_recovery_routes_validate_owned_revisioned_snapshots_in_both_modes() {
    use openwebide_core::editor::{
        Document, EditorRecovery, EditorRecoveryFile, EditorRecoveryRecord, EditorRecoveryRoot,
        RecoveryScroll,
    };
    futures::executor::block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let state = AppState::new().await.unwrap();
            let owner = state
                .store
                .insert_user("owner", "hash", openwebide_core::UserRole::Admin, 1)
                .await
                .unwrap();
            let other = state
                .store
                .insert_user("other", "hash", openwebide_core::UserRole::User, 1)
                .await
                .unwrap();
            let project = state
                .store
                .create_project(
                    &NewProject {
                        name: "editor".into(),
                        mode,
                        path: Some("project".into()),
                    },
                    owner.id,
                    1,
                )
                .await
                .unwrap();
            let user = AuthedUser {
                id: owner.id,
                role: owner.role,
            };
            let other = AuthedUser {
                id: other.id,
                role: other.role,
            };
            let path = format!("/api/projects/{}/editor-recovery", project.id);
            let mut document = Document::new("α\r\n");
            document.replace_selections("draft ", None).unwrap();
            let record = EditorRecoveryRecord {
                revision: 0,
                state: EditorRecovery {
                    format: 1,
                    root: Some(EditorRecoveryRoot::for_project(&project)),
                    selected: Some("main.rs".into()),
                    files: vec![EditorRecoveryFile {
                        path: "main.rs".into(),
                        document: Some(document.recovery()),
                        scroll: RecoveryScroll::default(),
                        read_only: false,
                    }],
                },
            };
            assert_eq!(
                super::editor_recovery::get(&state, &path, user)
                    .await
                    .unwrap()
                    .status(),
                200
            );
            assert_eq!(
                super::editor_recovery::save_record(&state, project.id, other, record.clone())
                    .await
                    .unwrap_err()
                    .into_response()
                    .status(),
                404
            );
            assert_eq!(
                super::editor_recovery::save_record(&state, project.id, user, record.clone())
                    .await
                    .unwrap()
                    .status(),
                200
            );
            assert_eq!(
                super::editor_recovery::save_record(&state, project.id, user, record.clone())
                    .await
                    .unwrap_err()
                    .into_response()
                    .status(),
                409
            );
            let response = super::editor_recovery::get(&state, &path, user)
                .await
                .unwrap()
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes();
            let persisted: EditorRecoveryRecord = serde_json::from_slice(&response).unwrap();
            assert_eq!(persisted.revision, 1);
            assert_eq!(persisted.state, record.state);
            let mut invalid = persisted.clone();
            invalid.state.files[0].path = "../outside".into();
            assert_eq!(
                super::editor_recovery::save_record(&state, project.id, user, invalid.clone())
                    .await
                    .unwrap_err()
                    .into_response()
                    .status(),
                400
            );
            invalid = persisted.clone();
            invalid.revision = -1;
            assert_eq!(
                super::editor_recovery::save_record(&state, project.id, user, invalid.clone())
                    .await
                    .unwrap_err()
                    .into_response()
                    .status(),
                400
            );
            invalid = persisted.clone();
            invalid.state.root.as_mut().unwrap().path = Some("changed".into());
            assert_eq!(
                super::editor_recovery::save_record(&state, project.id, user, invalid.clone())
                    .await
                    .unwrap_err()
                    .into_response()
                    .status(),
                409
            );
            let settings = super::settings::get_settings(&state, user)
                .await
                .unwrap()
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&settings).unwrap(),
                serde_json::json!({})
            );
            assert_eq!(
                super::settings::validate_setting_key(&format!("editor_recovery_{}", project.id))
                    .unwrap_err()
                    .into_response()
                    .status(),
                400
            );
            assert_eq!(
                state
                    .store
                    .editor_recovery(user.id, project.id)
                    .await
                    .unwrap(),
                persisted
            );
        }
    });
}

#[test]
fn prompt_api_and_default_selection_are_user_scoped() {
    futures::executor::block_on(async {
        let state = AppState::new().await.unwrap();
        let alice = state
            .store
            .insert_user("alice", "hash", openwebide_core::UserRole::Admin, 1)
            .await
            .unwrap();
        let bob = state
            .store
            .insert_user("bob", "hash", openwebide_core::UserRole::User, 1)
            .await
            .unwrap();
        let user = AuthedUser {
            id: bob.id,
            role: bob.role,
        };
        let prompt = state
            .store
            .insert_system_prompt(alice.id, "private", "Private instructions")
            .await
            .unwrap();
        let setting = |id: i64| {
            serde_json::from_value::<super::settings::SettingBody>(
                json!({"key": "default_prompt", "value": id.to_string()}),
            )
            .unwrap()
        };
        let response = super::prompts::list_system_prompts(&state, user)
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            serde_json::from_slice::<Vec<SystemPrompt>>(&body).unwrap(),
            Vec::new()
        );
        let path = format!("/api/system-prompts/{}", prompt.id);
        let error = super::prompts::delete_system_prompt(&state, &path, user)
            .await
            .unwrap_err();
        assert_eq!(error.into_response().status().as_u16(), 404);
        let error = super::settings::write_setting_body(&state, user, setting(prompt.id))
            .await
            .unwrap_err();
        assert_eq!(error.into_response().status().as_u16(), 404);
        assert_eq!(
            state
                .store
                .get_user_setting(bob.id, "default_prompt")
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            state
                .store
                .get_system_prompt(prompt.id, alice.id)
                .await
                .unwrap()
                .content,
            "Private instructions"
        );
        let own = state
            .store
            .insert_system_prompt(bob.id, "private", "Bob's instructions")
            .await
            .unwrap();
        assert_eq!(
            super::settings::write_setting_body(&state, user, setting(own.id))
                .await
                .unwrap()
                .status()
                .as_u16(),
            200
        );
    });
}

#[test]
fn project_memory_run_planning_includes_enabled_context_and_tools_and_omits_projectless_data() {
    futures::executor::block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store =
                openwebide_storage::Store::new(crate::state::AppDb::open_in_memory().unwrap());
            store.migrate().await.unwrap();
            let user = store
                .insert_user("owner", "hash", openwebide_core::UserRole::Admin, 0)
                .await
                .unwrap();
            let connection = store
                .insert_connection(&NewConnection {
                    name: "server".into(),
                    kind: openwebide_core::ProviderKind::LlamaCpp,
                    base_url: "http://server".into(),
                    model: None,
                    context_limit: Some(32768),
                })
                .await
                .unwrap();
            let project = store
                .create_project(
                    &NewProject {
                        name: "project".into(),
                        mode,
                        path: Some("test".into()),
                    },
                    user.id,
                    0,
                )
                .await
                .unwrap()
                .id;
            let session = store
                .create_session(
                    "session",
                    Some(connection.id),
                    None,
                    Some(project),
                    user.id,
                    0,
                )
                .await
                .unwrap()
                .id;
            store
                .memory_command(
                    user.id,
                    project,
                    &openwebide_core::MemoryCommand::Create {
                        auto_title: false,
                        title: "Build".into(),
                        content: "Run cargo test".into(),
                    },
                    false,
                    1,
                )
                .await
                .unwrap();
            let host = install_collection_plugin(&store, user.id, "memories").await;
            let state = AppState { store };
            let body = || SendMessageBody {
                content: "go".into(),
                model: None,
                editor_context: None,
                browser_preferences: None,
                queued_prompt: None,
            };
            let plan =
                build_run_plan_with_plugin_transport(&state, user.id, session, body(), &host)
                    .await
                    .unwrap();
            assert!(
                plan.request
                    .tools
                    .iter()
                    .any(|tool| tool.name == "memory_search")
            );
            assert!(
                plan.request
                    .system_prompt
                    .unwrap()
                    .contains("Run cargo test")
            );
            let mut selected = state.store.get_connection(connection.id).await.unwrap();
            selected.tool_selection =
                openwebide_core::ToolSelection::Selected(vec!["memory_read".into()]);
            state.store.update_connection(&selected).await.unwrap();
            let plan =
                build_run_plan_with_plugin_transport(&state, user.id, session, body(), &host)
                    .await
                    .unwrap();
            assert_eq!(
                plan.request
                    .tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["memory_read"]
            );
            assert_eq!(
                matches!(plan.kind, RunKind::Agent { .. }),
                mode == WorkspaceMode::Remote,
            );
            selected.tool_selection = openwebide_core::ToolSelection::All;
            state.store.update_connection(&selected).await.unwrap();
            state
                .store
                .memory_command(
                    user.id,
                    project,
                    &openwebide_core::MemoryCommand::SetEnabled { enabled: false },
                    false,
                    2,
                )
                .await
                .unwrap();
            let plan =
                build_run_plan_with_plugin_transport(&state, user.id, session, body(), &host)
                    .await
                    .unwrap();
            assert!(
                !plan
                    .request
                    .tools
                    .iter()
                    .any(|tool| tool.name.starts_with("memory_"))
            );
            assert!(
                !plan
                    .request
                    .system_prompt
                    .unwrap()
                    .contains("Run cargo test")
            );
            let no_root = state
                .store
                .create_project(
                    &NewProject {
                        name: "Named project".into(),
                        mode,
                        path: None,
                    },
                    user.id,
                    0,
                )
                .await
                .unwrap()
                .id;
            let named = state
                .store
                .create_session(
                    "named",
                    Some(connection.id),
                    None,
                    Some(no_root),
                    user.id,
                    0,
                )
                .await
                .unwrap()
                .id;
            let plan = build_run_plan_with_plugin_transport(&state, user.id, named, body(), &host)
                .await
                .unwrap();
            assert_eq!(plan.kind, RunKind::WebChat);
            assert!(
                plan.request
                    .tools
                    .iter()
                    .any(|tool| tool.name == "memory_create")
            );
            let empty = state
                .store
                .create_session("projectless", Some(connection.id), None, None, user.id, 0)
                .await
                .unwrap()
                .id;
            let plan = build_run_plan_with_plugin_transport(&state, user.id, empty, body(), &host)
                .await
                .unwrap();
            assert!(
                !plan
                    .request
                    .tools
                    .iter()
                    .any(|tool| tool.name.starts_with("memory_"))
            );
            assert!(
                !plan
                    .request
                    .system_prompt
                    .unwrap()
                    .contains("Run cargo test")
            );
        }
    });
}

#[test]
fn scheduled_model_overrides_are_run_scoped_in_every_workspace() {
    use openwebide_core::scheduled::{
        ExecutionHost, HostBinding, Schedule, SessionTarget, TaskCommand, TaskDraft,
    };
    futures::executor::block_on(async {
        for mode in [
            None,
            Some(WorkspaceMode::Local),
            Some(WorkspaceMode::Remote),
        ] {
            let store =
                openwebide_storage::Store::new(crate::state::AppDb::open_in_memory().unwrap());
            store.migrate_with(&|_| true).await.unwrap();
            let user = store
                .insert_user("owner", "hash", openwebide_core::UserRole::Admin, 0)
                .await
                .unwrap()
                .id;
            let other = store
                .insert_user("other", "hash", openwebide_core::UserRole::User, 0)
                .await
                .unwrap()
                .id;
            let mut servers = Vec::new();
            for name in ["Session", "Override"] {
                servers.push(
                    store
                        .insert_connection(&NewConnection {
                            name: name.into(),
                            kind: openwebide_core::ProviderKind::Ollama,
                            base_url: "http://localhost:11434".into(),
                            model: Some(name.into()),
                            context_limit: Some(8192),
                        })
                        .await
                        .unwrap(),
                );
            }
            let project = match mode {
                Some(mode) => Some(
                    store
                        .create_project(
                            &NewProject {
                                name: "Project".into(),
                                mode,
                                path: Some("project".into()),
                            },
                            user,
                            0,
                        )
                        .await
                        .unwrap()
                        .id,
                ),
                None => None,
            };
            let session = store
                .create_session("Session", Some(servers[0].id), None, project, user, 0)
                .await
                .unwrap();
            let host = ExecutionHost {
                id: if mode == Some(WorkspaceMode::Local) {
                    "paired"
                } else {
                    "server"
                }
                .into(),
                name: "Host".into(),
                last_seen: 0,
            };
            let binding = HostBinding {
                host_id: host.id.clone(),
                path: "project".into(),
            };
            let selection = openwebide_core::ModelSelection {
                server_id: servers[1].id,
                model: "chosen-override".into(),
            };
            let mut draft = TaskDraft {
                model: Some(selection.clone()),
                auto_title: false,
                title: "Task".into(),
                prompt: "Do work".into(),
                session_id: session.id,
                session_target: SessionTarget::Existing,
                schedule: Schedule::Cron {
                    expression: "* * * * *".into(),
                    timezone: "UTC".into(),
                },
                enabled: true,
            };
            let task = store
                .scheduled_command(
                    user,
                    project,
                    &TaskCommand::Create {
                        draft: draft.clone(),
                    },
                    Some(&binding),
                    false,
                    0,
                )
                .await
                .unwrap()
                .remove(0);
            assert_eq!(task.draft.model, Some(selection.clone()));
            let delivery = store.due_scheduled(&host, 60).await.unwrap().remove(0);
            let key = delivery.prompt.key();
            assert_eq!(
                store
                    .scheduled_prompt_model(user, session.id, key)
                    .await
                    .unwrap(),
                Some(selection)
            );
            assert!(
                store
                    .scheduled_prompt_model(other, session.id, key)
                    .await
                    .is_err()
            );
            let state = AppState { store };
            let body = || SendMessageBody {
                content: delivery.prompt.content.clone(),
                model: None,
                editor_context: None,
                browser_preferences: None,
                queued_prompt: Some(key),
            };
            let plan = build_run_plan(&state, user, session.id, body())
                .await
                .unwrap();
            assert_eq!(plan.connection.id, servers[1].id);
            assert_eq!(plan.request.model.as_deref(), Some("chosen-override"));
            assert_eq!(
                state
                    .store
                    .get_session(session.id, user)
                    .await
                    .unwrap()
                    .connection_id,
                session.connection_id
            );
            assert!(
                state
                    .store
                    .get_user_setting(user, &format!("session_model_{}", session.id))
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(
                build_run_plan(&state, other, session.id, body())
                    .await
                    .is_err()
            );
            let mut disabled = servers[1].clone();
            disabled.enabled = false;
            state.store.update_connection(&disabled).await.unwrap();
            assert!(
                build_run_plan(&state, user, session.id, body())
                    .await
                    .is_err()
            );
            assert_eq!(
                state
                    .store
                    .list_queued_prompts(user, session.id)
                    .await
                    .unwrap()[0]
                    .key(),
                key
            );
            assert!(
                state
                    .store
                    .scheduled_command(
                        user,
                        project,
                        &TaskCommand::Update {
                            id: task.id,
                            revision: task.revision,
                            draft: draft.clone()
                        },
                        None,
                        false,
                        61
                    )
                    .await
                    .is_err()
            );
            state.store.update_connection(&servers[1]).await.unwrap();
            // Clearing an override invalidates the previous occurrence rather than running stale settings.
            draft.model = None;
            let updated = state
                .store
                .scheduled_command(
                    user,
                    project,
                    &TaskCommand::Update {
                        id: task.id,
                        revision: task.revision,
                        draft: draft.clone(),
                    },
                    None,
                    false,
                    61,
                )
                .await
                .unwrap()
                .remove(0);
            assert!(updated.draft.model.is_none());
            assert!(
                build_run_plan(&state, user, session.id, body())
                    .await
                    .is_err()
            );
            state
                .store
                .set_user_setting(
                    user,
                    &format!("session_model_{}", session.id),
                    &serde_json::json!({"connection_id":servers[0].id,"model":"recently-selected"})
                        .to_string(),
                )
                .await
                .unwrap();
            let next = state
                .store
                .due_scheduled(&host, 120)
                .await
                .unwrap()
                .remove(0);
            let mut current = body();
            current.queued_prompt = Some(next.prompt.key());
            let plan = build_run_plan(&state, user, session.id, current)
                .await
                .unwrap();
            assert_eq!(plan.connection.id, servers[0].id);
            assert_eq!(plan.request.model.as_deref(), Some("recently-selected"));
            let ordinary = state
                .store
                .enqueue_prompt(user, session.id, "Ordinary prompt", 121)
                .await
                .unwrap();
            assert!(
                state
                    .store
                    .scheduled_prompt_model(user, session.id, ordinary.key())
                    .await
                    .unwrap()
                    .is_none()
            );
            let mut ordinary_body = body();
            ordinary_body.content.clone_from(&ordinary.content);
            ordinary_body.queued_prompt = Some(ordinary.key());
            ordinary_body.model = Some("manual-choice".into());
            let ordinary_plan = build_run_plan(&state, user, session.id, ordinary_body)
                .await
                .unwrap();
            assert_eq!(
                ordinary_plan.request.model.as_deref(),
                Some("manual-choice")
            );
            draft.model = Some(openwebide_core::ModelSelection {
                server_id: 99999,
                model: "missing".into(),
            });
            assert!(
                state
                    .store
                    .scheduled_command(
                        user,
                        project,
                        &TaskCommand::Update {
                            id: updated.id,
                            revision: updated.revision,
                            draft
                        },
                        None,
                        false,
                        121
                    )
                    .await
                    .is_err()
            );
        }
    });
}

#[test]
fn project_skill_run_planning_includes_enabled_context_and_tools_and_omits_projectless_data() {
    futures::executor::block_on(async {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let store =
                openwebide_storage::Store::new(crate::state::AppDb::open_in_memory().unwrap());
            store.migrate().await.unwrap();
            let user = store
                .insert_user("owner", "hash", openwebide_core::UserRole::Admin, 0)
                .await
                .unwrap();
            let connection = store
                .insert_connection(&NewConnection {
                    name: "server".into(),
                    kind: openwebide_core::ProviderKind::LlamaCpp,
                    base_url: "http://server".into(),
                    model: None,
                    context_limit: Some(32768),
                })
                .await
                .unwrap();
            let project = store
                .create_project(
                    &NewProject {
                        name: "project".into(),
                        mode,
                        path: Some("test".into()),
                    },
                    user.id,
                    0,
                )
                .await
                .unwrap()
                .id;
            let session = store
                .create_session(
                    "session",
                    Some(connection.id),
                    None,
                    Some(project),
                    user.id,
                    0,
                )
                .await
                .unwrap()
                .id;
            store
                .skill_command(
                    user.id,
                    project,
                    &openwebide_core::SkillCommand::Create {
                        draft: openwebide_core::SkillDraft {
                            name: "build-check".into(),
                            description: "Check builds when reviewing changes".into(),
                            instructions: "PRIVATE INSTRUCTIONS".into(),
                            enabled: true,
                            resources: Vec::new(),
                            metadata: Default::default(),
                        },
                    },
                    false,
                    1,
                )
                .await
                .unwrap();
            let host = install_collection_plugin(&store, user.id, "skills").await;
            let state = AppState { store };
            let body = || SendMessageBody {
                content: "go".into(),
                model: None,
                editor_context: None,
                browser_preferences: None,
                queued_prompt: None,
            };
            let plan =
                build_run_plan_with_plugin_transport(&state, user.id, session, body(), &host)
                    .await
                    .unwrap();
            assert!(
                plan.request
                    .tools
                    .iter()
                    .any(|tool| tool.name == "skill_list")
            );
            assert!(
                plan.request
                    .system_prompt
                    .as_ref()
                    .unwrap()
                    .contains("Check builds when reviewing changes")
            );
            assert!(
                !plan
                    .request
                    .system_prompt
                    .as_ref()
                    .unwrap()
                    .contains("PRIVATE INSTRUCTIONS")
            );
            let mut selected = state.store.get_connection(connection.id).await.unwrap();
            selected.tool_selection =
                openwebide_core::ToolSelection::Selected(vec!["skill_read".into()]);
            state.store.update_connection(&selected).await.unwrap();
            let plan =
                build_run_plan_with_plugin_transport(&state, user.id, session, body(), &host)
                    .await
                    .unwrap();
            assert_eq!(
                plan.request
                    .tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["skill_read"]
            );
            assert_eq!(
                matches!(plan.kind, RunKind::Agent { .. }),
                mode == WorkspaceMode::Remote,
            );
            selected.tool_selection = openwebide_core::ToolSelection::All;
            state.store.update_connection(&selected).await.unwrap();
            state
                .store
                .skill_command(
                    user.id,
                    project,
                    &openwebide_core::SkillCommand::SetEnabled { enabled: false },
                    false,
                    2,
                )
                .await
                .unwrap();
            let plan =
                build_run_plan_with_plugin_transport(&state, user.id, session, body(), &host)
                    .await
                    .unwrap();
            assert!(
                !plan
                    .request
                    .tools
                    .iter()
                    .any(|tool| tool.name.starts_with("skill_"))
            );
            assert!(
                !plan
                    .request
                    .system_prompt
                    .unwrap()
                    .contains("Check builds when reviewing changes")
            );
            let no_root = state
                .store
                .create_project(
                    &NewProject {
                        name: "Named project".into(),
                        mode,
                        path: None,
                    },
                    user.id,
                    0,
                )
                .await
                .unwrap()
                .id;
            let named = state
                .store
                .create_session(
                    "named",
                    Some(connection.id),
                    None,
                    Some(no_root),
                    user.id,
                    0,
                )
                .await
                .unwrap()
                .id;
            let plan = build_run_plan_with_plugin_transport(&state, user.id, named, body(), &host)
                .await
                .unwrap();
            assert_eq!(plan.kind, RunKind::WebChat);
            assert!(
                plan.request
                    .tools
                    .iter()
                    .any(|tool| tool.name == "skill_create")
            );
            let empty = state
                .store
                .create_session("projectless", Some(connection.id), None, None, user.id, 0)
                .await
                .unwrap()
                .id;
            let plan = build_run_plan_with_plugin_transport(&state, user.id, empty, body(), &host)
                .await
                .unwrap();
            assert!(
                !plan
                    .request
                    .tools
                    .iter()
                    .any(|tool| tool.name.starts_with("skill_"))
            );
            assert!(
                !plan
                    .request
                    .system_prompt
                    .unwrap()
                    .contains("Check builds when reviewing changes")
            );
        }
    });
}

#[test]
fn host_tools_require_configured_ssh_admin_and_projectless_scope_in_both_modes() {
    futures::executor::block_on(async {
        use openwebide_core::{NewProject, UserRole, WorkspaceMode};
        let store = openwebide_storage::Store::new(crate::state::AppDb::open_in_memory().unwrap());
        store.migrate().await.unwrap();
        let admin = store
            .insert_user("host-admin", "hash", UserRole::Admin, 1)
            .await
            .unwrap();
        let member = store
            .insert_user("host-member", "hash", UserRole::User, 1)
            .await
            .unwrap();
        let connection = store
            .insert_connection(&NewConnection {
                name: "model".into(),
                kind: openwebide_core::ProviderKind::LlamaCpp,
                base_url: "http://server".into(),
                model: Some("model".into()),
                context_limit: Some(32768),
            })
            .await
            .unwrap();
        let state = AppState { store };
        let session = state
            .store
            .create_session("Host", Some(connection.id), None, None, admin.id, 1)
            .await
            .unwrap();
        let body = || SendMessageBody {
            content: "Inspect host".into(),
            model: None,
            editor_context: None,
            browser_preferences: None,
            queued_prompt: None,
        };
        let host_tool = |tool: &openwebide_core::ToolDefinition| {
            openwebide_agent::host_admin::is_host_tool(&tool.name)
        };
        assert!(
            !build_run_plan(&state, admin.id, session.id, body())
                .await
                .unwrap()
                .request
                .tools
                .iter()
                .any(host_tool)
        );
        state
            .store
            .save_host_connection(&openwebide_core::host_admin::HostConnection {
                username: "admin".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let plan = build_run_plan(&state, admin.id, session.id, body())
            .await
            .unwrap();
        assert!(plan.request.tools.iter().any(host_tool));
        assert_eq!(plan.kind, RunKind::WebChat);
        let member_session = state
            .store
            .create_session("Host", Some(connection.id), None, None, member.id, 1)
            .await
            .unwrap();
        assert!(
            !build_run_plan(&state, member.id, member_session.id, body())
                .await
                .unwrap()
                .request
                .tools
                .iter()
                .any(host_tool)
        );
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let project = state
                .store
                .create_project(
                    &NewProject {
                        name: format!("{mode:?}"),
                        path: Some(format!("/tmp/host-test-{mode:?}")),
                        mode,
                    },
                    admin.id,
                    1,
                )
                .await
                .unwrap();
            let project_session = state
                .store
                .create_session(
                    "Project",
                    Some(connection.id),
                    None,
                    Some(project.id),
                    admin.id,
                    1,
                )
                .await
                .unwrap();
            assert!(
                !build_run_plan(&state, admin.id, project_session.id, body())
                    .await
                    .unwrap()
                    .request
                    .tools
                    .iter()
                    .any(host_tool)
            );
        }
        let mut selected = state.store.get_connection(connection.id).await.unwrap();
        selected.tool_selection =
            openwebide_core::ToolSelection::Selected(vec!["host_inspect".into()]);
        state.store.update_connection(&selected).await.unwrap();
        let selected_plan = build_run_plan(&state, admin.id, session.id, body())
            .await
            .unwrap();
        assert_eq!(
            selected_plan
                .request
                .tools
                .iter()
                .filter(|tool| host_tool(tool))
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec!["host_inspect"]
        );
    });
}

/// Mock only the host component transport; real database grants and collection
/// callbacks still run through the backend planning facade.
#[derive(Clone)]
struct CollectionContextHost {
    collection: &'static str,
    prepared: openwebide_core::plugins::PreparedPlugin,
}
impl openwebide_agent::plugins::execution::PluginTransport for CollectionContextHost {
    async fn ensure_prepared(
        &self,
        expected: &openwebide_core::plugins::PreparedPlugin,
    ) -> Result<openwebide_core::plugins::PreparedPlugin, String> {
        assert_eq!(expected, &self.prepared);
        Ok(expected.clone())
    }
    async fn start(
        &self,
        call: openwebide_core::plugins::execution::InvokePlugin,
    ) -> Result<openwebide_core::plugins::execution::PluginInvocation, String> {
        use openwebide_core::plugins::execution::{PluginInvocation, PluginOperation, PluginStep};
        assert!(matches!(call.operation, PluginOperation::Context));
        assert_eq!(call.prepared, self.prepared);
        Ok(PluginInvocation {
            id: "context".into(),
            step: PluginStep::Ready,
        })
    }
    async fn resume(
        &self,
        response: openwebide_core::plugins::execution::ContinuePlugin,
    ) -> Result<openwebide_core::plugins::execution::PluginInvocation, String> {
        use openwebide_core::plugins::execution::{PluginInvocation, PluginStep};
        assert_eq!(response.id, "context");
        let step = if response.sequence == 0 {
            PluginStep::HostCall {
                sequence: 1,
                capability: "collections".into(),
                payload: serde_json::json!({
                    "collection":self.collection, "operation":{"action":"list","after":0}
                })
                .to_string(),
            }
        } else {
            assert_eq!(response.sequence, 1);
            let result: openwebide_core::plugins::records::CollectionResult =
                serde_json::from_str(&response.response?).map_err(|error| error.to_string())?;
            let prompt = result.enabled.then(|| {
                result
                    .records
                    .iter()
                    .map(|record| {
                        if self.collection == "memories" {
                            record.value["content"].as_str().unwrap().to_owned()
                        } else {
                            record.value["draft"]["description"]
                                .as_str()
                                .unwrap()
                                .to_owned()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            let disabled_tools: Vec<_> = self
                .prepared
                .manifest
                .contributions
                .tools
                .iter()
                .filter(|_| !result.enabled)
                .map(|tool| tool.name.clone())
                .collect();
            PluginStep::Complete {
                ok: true,
                content: serde_json::json!({"prompt":prompt,"disabled_tools":disabled_tools})
                    .to_string(),
                summary: "Fixture planning complete".into(),
            }
        };
        Ok(PluginInvocation {
            id: "context".into(),
            step,
        })
    }
    fn cancel(&self, _: String) {}
}
async fn install_collection_plugin(
    store: &openwebide_storage::Store<crate::state::AppDb>,
    user: openwebide_core::UserId,
    collection: &'static str,
) -> CollectionContextHost {
    use openwebide_core::plugins::{PluginTool, RustPlugin};
    let mut package = openwebide_core::plugins::testing::package();
    package.skills.clear();
    let manifest = &mut package.prepared.manifest;
    manifest.contributions.skills.clear();
    manifest.compatibility.plugin_api = 3;
    let names = if collection == "memories" {
        [
            "memory_create",
            "memory_update",
            "memory_delete",
            "memory_search",
            "memory_list",
            "memory_read",
        ]
    } else {
        [
            "skill_create",
            "skill_update",
            "skill_delete",
            "skill_list",
            "skill_read",
            "skill_discover",
        ]
    };
    manifest.contributions.tools = names
        .into_iter()
        .map(|name| PluginTool {
            name: name.into(),
            description: "Collection fixture".into(),
            parameters: serde_json::json!({"type":"object"}),
            requires_approval: false,
        })
        .collect();
    manifest.executable = Some(RustPlugin {
        manifest: "Cargo.toml".into(),
        library: "collection_fixture".into(),
        sdk_version: "0.1.0".into(),
        capabilities: vec!["collections".into()],
    });
    store
        .record_plugin(
            user,
            &openwebide_core::plugins::RecordPlugin {
                approved_capabilities: vec!["collections".into()],
                prepared: package.prepared.clone(),
                revision: None,
                package: Some(Box::new(package.clone())),
                update_policy: None,
            },
            0,
        )
        .await
        .unwrap();
    CollectionContextHost {
        collection,
        prepared: package.prepared,
    }
}

#[test]
fn unavailable_bundle_host_preserves_account_access_existing_plugins_and_pending_defaults() {
    futures::executor::block_on(async {
        let store = openwebide_storage::Store::new(crate::state::AppDb::open_in_memory().unwrap());
        store.migrate().await.unwrap();
        let user = store
            .insert_user("bundle-owner", "hash", openwebide_core::UserRole::Admin, 0)
            .await
            .unwrap();
        let prepared = openwebide_core::plugins::testing::receipt();
        store
            .record_plugin(
                user.id,
                &openwebide_core::plugins::RecordPlugin {
                    approved_capabilities: Vec::new(),
                    prepared,
                    package: None,
                    revision: None,
                    update_policy: None,
                },
                0,
            )
            .await
            .unwrap();
        let state = AppState { store };
        let authed = AuthedUser {
            id: user.id,
            role: user.role,
        };
        assert_eq!(super::auth::me(&state, authed).await.unwrap().status(), 200);
        assert_eq!(
            super::plugins::list(&state, authed).await.unwrap().status(),
            200
        );
        assert_eq!(
            state
                .store
                .plugin_installations(user.id)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            state
                .store
                .pending_bundled_plugins(user.id)
                .await
                .unwrap()
                .len(),
            4
        );
    });
}
