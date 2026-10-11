use super::*;
#[test]
fn completion_authority_is_checked_before_model_access_in_both_modes() {
    futures::executor::block_on(async {
        for mode in [
            openwebide_core::WorkspaceMode::Local,
            openwebide_core::WorkspaceMode::Remote,
        ] {
            let store =
                openwebide_storage::Store::new(crate::state::AppDb::open_in_memory().unwrap());
            store.migrate().await.unwrap();
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
            let project = store
                .create_project(
                    &openwebide_core::NewProject {
                        name: "p".into(),
                        mode,
                        path: Some("p".into()),
                    },
                    user,
                    0,
                )
                .await
                .unwrap()
                .id;
            let session = store
                .create_session("s", None, None, Some(project), user, 0)
                .await
                .unwrap()
                .id;
            let mut plugin = openwebide_core::plugins::testing::receipt();
            plugin.manifest.compatibility.plugin_api = 3;
            plugin.manifest.contributions.skills.clear();
            plugin.manifest.contributions.tools = vec![openwebide_core::plugins::PluginTool {
                name: "summarize".into(),
                description: "Summarize supplied text".into(),
                parameters: json!({"type":"object"}),
                requires_approval: false,
            }];
            plugin.manifest.executable = Some(openwebide_core::plugins::RustPlugin {
                manifest: "Cargo.toml".into(),
                library: "summary".into(),
                sdk_version: "0.1.0".into(),
                capabilities: vec!["completion".into()],
            });
            store
                .record_plugin(
                    user,
                    &RecordPlugin {
                        prepared: plugin.clone(),
                        revision: None,
                        update_policy: None,
                        approved_capabilities: Vec::new(),
                        package: Some(Box::new(openwebide_core::plugins::PluginPackage {
                            prepared: plugin.clone(),
                            skills: Vec::new(),
                        })),
                    },
                    now(),
                )
                .await
                .unwrap();
            let token = "a".repeat(32);
            store
                .issue_plugin_grant(user, session, &plugin, &token, now())
                .await
                .unwrap();
            let request = openwebide_core::plugins::execution::PluginHostRequest {
                grant: token,
                capability: "completion".into(),
                payload:
                    json!({"system_prompt":"Plugin policy","prompt":"Fact","max_output_tokens":64})
                        .to_string(),
            };
            let authorized = execute_host_request(&store, user, Some(session), &request)
                .await
                .unwrap_err();
            assert_eq!(
                authorized.public_message(),
                "Session has no model connection"
            );
            assert!(
                execute_host_request(&store, other, Some(session), &request)
                    .await
                    .unwrap_err()
                    .into_response()
                    .status()
                    .as_u16()
                    == 404
            );
            let context_token = "d".repeat(32);
            store
                .issue_plugin_context_grant(
                    user,
                    &openwebide_core::plugins::execution::PluginExecutionContext {
                        user_action: false,
                        project_id: Some(project),
                        session_id: None,
                        primary: None,
                    },
                    &plugin,
                    &context_token,
                    now(),
                )
                .await
                .unwrap();
            let context_request = openwebide_core::plugins::execution::PluginHostRequest {
                grant: context_token,
                ..request.clone()
            };
            assert_eq!(
                execute_host_request(&store, user, None, &context_request)
                    .await
                    .unwrap_err()
                    .public_message(),
                "No primary model is configured"
            );
            assert_eq!(
                execute_host_request(&store, other, None, &context_request)
                    .await
                    .unwrap_err()
                    .into_response()
                    .status()
                    .as_u16(),
                404
            );
            assert_eq!(
                execute_host_request(&store, user, Some(session), &context_request)
                    .await
                    .unwrap_err()
                    .into_response()
                    .status()
                    .as_u16(),
                404
            );
            let mut invalid = request.clone();
            invalid.grant = "b".repeat(32);
            assert_eq!(
                execute_host_request(&store, user, Some(session), &invalid)
                    .await
                    .unwrap_err()
                    .into_response()
                    .status()
                    .as_u16(),
                404
            );
            invalid = request.clone();
            invalid.capability = "collections".into();
            assert_eq!(
                execute_host_request(&store, user, Some(session), &invalid)
                    .await
                    .unwrap_err()
                    .into_response()
                    .status()
                    .as_u16(),
                400
            );
            invalid = request.clone();
            invalid.payload =
                json!({"system_prompt":"x","prompt":"Fact","max_output_tokens":0}).to_string();
            assert!(
                execute_host_request(&store, user, Some(session), &invalid)
                    .await
                    .unwrap_err()
                    .public_message()
                    .contains("1–1024")
            );
            let server = store
                .insert_connection(&openwebide_core::NewConnection {
                    name: "Model".into(),
                    kind: openwebide_core::ProviderKind::Ollama,
                    base_url: "http://localhost:11434".into(),
                    model: Some("server-default".into()),
                    context_limit: None,
                })
                .await
                .unwrap();
            let selected = |model: &str| openwebide_core::ModelSelection {
                server_id: server.id,
                model: model.into(),
            };
            store
                .save_model_defaults(
                    user,
                    &openwebide_core::ModelDefaults {
                        primary: Some(selected("account-default")),
                        fast: None,
                    },
                )
                .await
                .unwrap();
            let state = AppState { store };
            let context = openwebide_core::plugins::execution::PluginExecutionContext {
                user_action: false,
                project_id: Some(project),
                session_id: None,
                primary: None,
            };
            let grants = context_grants(&state, user, &context, std::slice::from_ref(&plugin))
                .await
                .unwrap();
            let pinned_request = openwebide_core::plugins::execution::PluginHostRequest {
                grant: grants[&plugin.digest].clone(),
                ..request.clone()
            };
            state
                .store
                .save_model_defaults(
                    user,
                    &openwebide_core::ModelDefaults {
                        primary: Some(selected("changed-default")),
                        fast: None,
                    },
                )
                .await
                .unwrap();
            assert_eq!(
                state
                    .store
                    .authorize_plugin_context(user, &pinned_request, now())
                    .await
                    .unwrap()
                    .1
                    .primary,
                Some(selected("account-default"))
            );
            let run_context = openwebide_core::plugins::execution::PluginExecutionContext {
                session_id: Some(session),
                primary: Some(selected("run-override")),
                ..context.clone()
            };
            let run_grants =
                context_grants(&state, user, &run_context, std::slice::from_ref(&plugin))
                    .await
                    .unwrap();
            let run_request = openwebide_core::plugins::execution::PluginHostRequest {
                grant: run_grants[&plugin.digest].clone(),
                ..request.clone()
            };
            assert_eq!(
                state
                    .store
                    .authorize_plugin_execution(user, Some(session), &run_request, now())
                    .await
                    .unwrap()
                    .1,
                run_context
            );
            assert!(
                state
                    .store
                    .get_session(session, user)
                    .await
                    .unwrap()
                    .connection_id
                    .is_none()
            );
            assert_eq!(state.store.list_sessions(user).await.unwrap().len(), 1);
            assert!(
                context_grants(&state, other, &context, std::slice::from_ref(&plugin))
                    .await
                    .is_err()
            );
            assert!(
                context_grants(&state, user, &context, &[plugin.clone(), plugin.clone()])
                    .await
                    .is_err()
            );
        }
    });
}
