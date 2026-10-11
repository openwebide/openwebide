use super::*;
// -- sessions --------------------------------------------------------------------

pub(crate) async fn list_sessions(
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let user_id = user.id;
    let sessions = state.store.list_sessions(user_id).await?;
    Ok(json_response(200, &sessions))
}

pub(crate) async fn search_sessions(
    req: Request,
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let body = read_body(req, JSON_BODY_LIMIT).await?;
    let search: openwebide_core::SessionSearch = parse_json(body)?;
    search.validate().map_err(ApiError::bad_request)?;
    let sessions = state
        .store
        .search_sessions(user.id, search.project_id, &search.query, search.archived)
        .await?;
    Ok(json_response(200, &sessions))
}

pub(crate) async fn session_preferences(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let id = session_id(path)?;
    state.store.get_session(id, user.id).await?;
    let body = read_body(req, JSON_BODY_LIMIT).await?;
    let preferences: openwebide_core::SessionPreferences = parse_json(body)?;
    let session = state
        .store
        .set_session_preferences(user.id, id, &preferences)
        .await?;
    Ok(json_response(200, &session))
}

pub(crate) async fn export_session(
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let id = session_id(path)?;
    let export = state.store.export_session(user.id, id).await?;
    Ok(json_response(200, &export))
}

/// Titles are best-effort and committed only while the original name is still automatic.
pub(crate) async fn session_title(
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let id = session_id(path)?;
    let session = state.store.get_session(id, user.id).await?;
    if !session.auto_title {
        return Ok(json_response(200, &Some(session)));
    }
    let messages = state.store.list_messages(id).await?;
    let turns = i64::try_from(
        messages
            .iter()
            .filter(|message| message.role == Role::User)
            .count(),
    )
    .unwrap_or(i64::MAX);
    if !state
        .store
        .session_title_due(user.id, id, turns, now())
        .await?
    {
        return Ok(json_response(200, &Some(session)));
    }
    let Some(_) = openwebide_agent::title::initial_exchange(&messages) else {
        return Ok(json_response(
            200,
            &Option::<openwebide_core::ChatSession>::None,
        ));
    };
    let exchange = openwebide_agent::assistance::recent_activity(&messages);
    if exchange
        .last()
        .is_none_or(|message| message.role != Role::Assistant)
    {
        return Ok(json_response(
            200,
            &Option::<openwebide_core::ChatSession>::None,
        ));
    }
    let last_message = messages.iter().map(|message| message.id).max().unwrap_or(0);
    let setup = state.store.model_setup(user.id).await?;
    let connection = session.connection_id.or_else(|| {
        setup
            .defaults
            .primary
            .as_ref()
            .map(|selection| selection.server_id)
    });
    let Some(connection) = connection else {
        return Ok(json_response(
            200,
            &Option::<openwebide_core::ChatSession>::None,
        ));
    };
    let Ok(runtime) =
        super::model_setup::runtime_store(&state.store, user.id, connection, None).await
    else {
        return Ok(json_response(
            200,
            &Option::<openwebide_core::ChatSession>::None,
        ));
    };
    let source = super::model_operations::ModelSource {
        store: &state.store,
        user: user.id,
    };
    let title = match openwebide_agent::title::generate(&source, runtime, exchange).await {
        Ok(title) => {
            state
                .store
                .refresh_session_title(
                    user.id,
                    id,
                    session.title_revision,
                    &title,
                    last_message,
                    now(),
                )
                .await?
        }
        Err(_) => None,
    };
    Ok(json_response(200, &title))
}

pub(super) type CreateSessionBody = openwebide_core::NewSession;

pub(crate) async fn create_session(
    req: Request,
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let user_id = user.id;
    let body = read_body(req, JSON_BODY_LIMIT).await?;
    let new: CreateSessionBody = parse_json(body)?;
    let session = state
        .store
        .create_session_request(&new, user_id, now())
        .await?;
    Ok(json_response(201, &session))
}

#[derive(Deserialize)]
struct SessionConnectionBody {
    connection_id: i64,
}

pub(crate) async fn set_session_connection(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let id = session_id(path)?;
    state.store.get_session(id, user.id).await?;
    let body = read_body(req, JSON_BODY_LIMIT).await?;
    let selection: SessionConnectionBody = parse_json(body)?;
    if !state
        .store
        .get_connection(selection.connection_id)
        .await?
        .enabled
    {
        return Err(ApiError::bad_request("connection is disabled"));
    }
    let session = state
        .store
        .set_session_connection(id, selection.connection_id, user.id)
        .await?;
    Ok(json_response(200, &session))
}

#[derive(Deserialize)]
pub(super) struct RenameSessionBody {
    name: String,
}

pub(crate) async fn rename_session(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let user_id = user.id;
    let id = session_id(path)?;
    let body = read_body(req, JSON_BODY_LIMIT).await?;
    let rename: RenameSessionBody = parse_json(body)?;
    let session = state
        .store
        .rename_session(id, &rename.name, user_id)
        .await?;
    Ok(json_response(200, &session))
}

pub(crate) async fn delete_session(
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let user_id = user.id;
    let id = session_id(path)?;
    state.store.delete_session(id, user_id).await?;
    Ok(json_response(200, &json!({ "deleted": id })))
}

/// Request cancellation of the session's in-flight run. The flag is picked
/// up by the streaming request at its next step boundary (before the next
/// model call or tool execution); a run that is not in flight is unaffected.
pub(crate) async fn cancel_session(
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let user_id = user.id;
    let id = session_id(path)?;
    // Verify ownership before setting the flag.
    state.store.get_session(id, user_id).await?;
    state.store.request_cancel(id, now_ms()).await?;
    Ok(json_response(200, &json!({ "cancelled": id })))
}

#[derive(Deserialize)]
pub(super) struct PermissionBody {
    approved: bool,
}

/// Record the user's decision on a gated tool call. The waiting call consumes
/// it on its next poll, so it answers that call only; a decision for a call
/// that is no longer waiting is harmless (decisions are cleared when the run
/// ends).
pub(crate) async fn set_tool_permission(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let user_id = user.id;
    let (id, tool_call_id) = permission_path(path)?;
    // Verify ownership before recording the decision.
    state.store.get_session(id, user_id).await?;
    let body = read_body(req, JSON_BODY_LIMIT).await?;
    let decision: PermissionBody = parse_json(body)?;
    state
        .store
        .set_tool_permission(id, tool_call_id, decision.approved)
        .await?;
    Ok(json_response(200, &json!({ "ok": true })))
}

pub(crate) async fn list_messages(
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let user_id = user.id;
    let id = session_id(path)?;
    // Verify ownership before exposing the (unscoped) message list.
    state.store.get_session(id, user_id).await?;
    // Messages plus the agent's tool steps, interleaved, so a reloaded session
    // shows its steps again.
    let conversation = state.store.list_conversation(id).await?;
    Ok(json_response(200, &conversation))
}

pub(crate) enum QueueAction {
    List,
    Add,
    Update,
    Remove,
    Consume,
}

pub(crate) async fn fork_session(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let session = session_id(path)?;
    let body: RewindBody = parse_json(read_body(req, JSON_BODY_LIMIT).await?)?;
    Ok(json_response(
        201,
        &state
            .store
            .fork_session(user.id, session, body.message_id, now())
            .await?,
    ))
}

#[derive(Deserialize)]
struct QueueBody {
    #[serde(default)]
    guidance: bool,
    #[serde(default)]
    key: Option<openwebide_core::QueuedPromptKey>,
    #[serde(default)]
    content: String,
}

pub(crate) async fn queued_prompts(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
    action: QueueAction,
) -> Result<JsonResp, ApiError> {
    let session = session_id(path)?;
    state.store.get_session(session, user.id).await?;
    if matches!(action, QueueAction::List) {
        return Ok(json_response(
            200,
            &state.store.list_queued_prompts(user.id, session).await?,
        ));
    }
    let body: QueueBody = parse_json(read_body(req, CHAT_BODY_LIMIT).await?)?;
    if !matches!(action, QueueAction::Remove) {
        openwebide_core::chat_queue::validate_content(&body.content)
            .map_err(ApiError::bad_request)?;
    }
    if matches!(action, QueueAction::Add) {
        let prompt = if body.guidance {
            state
                .store
                .enqueue_guidance(user.id, session, &body.content, now())
                .await?
        } else {
            state
                .store
                .enqueue_prompt(user.id, session, &body.content, now())
                .await?
        };
        return Ok(json_response(201, &prompt));
    }
    let key = body
        .key
        .ok_or_else(|| ApiError::bad_request("queued prompt key is required"))?;
    match action {
        QueueAction::Update => Ok(json_response(
            200,
            &state
                .store
                .update_queued_prompt(user.id, session, key, &body.content)
                .await?,
        )),
        QueueAction::Remove => {
            state
                .store
                .remove_queued_prompt(user.id, session, key)
                .await?;
            Ok(json_response(200, &json!({"ok":true})))
        }
        QueueAction::Consume => Ok(json_response(
            201,
            &state
                .store
                .consume_queued_prompt(user.id, session, key, &body.content, now())
                .await?,
        )),
        QueueAction::List | QueueAction::Add => unreachable!(),
    }
}

#[derive(Deserialize)]
pub(super) struct SendMessageBody {
    #[serde(default)]
    pub(super) browser_preferences: Option<openwebide_core::BrowserPreferences>,
    pub(super) content: String,
    #[serde(default)]
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) editor_context: Option<EditorContext>,
    #[serde(default)]
    pub(super) queued_prompt: Option<openwebide_core::QueuedPromptKey>,
}

pub(super) async fn build_run_plan(
    state: &AppState,
    user_id: UserId,
    session_id: i64,
    send: SendMessageBody,
) -> Result<RunPlan, ApiError> {
    let host = super::plugins::PlanningHost::new(state, user_id, session_id);
    build_run_plan_with_plugin_transport(state, user_id, session_id, send, &host).await
}

pub(super) async fn build_run_plan_with_plugin_transport<T>(
    state: &AppState,
    user_id: UserId,
    session_id: i64,
    send: SendMessageBody,
    transport: &T,
) -> Result<RunPlan, ApiError>
where
    T: openwebide_agent::plugins::execution::PluginTransport,
{
    state.store.ensure_not_rewinding(session_id).await?;
    let session = state.store.get_session(session_id, user_id).await?;
    let task_model = match send.queued_prompt {
        Some(key) => state
            .store
            .scheduled_prompt_model(user_id, session_id, key)
            .await?
            .or(state
                .store
                .plugin_prompt_model(user_id, session_id, key)
                .await?),
        None => None,
    };
    let connection_id = task_model
        .as_ref()
        .map(|model| model.server_id)
        .or(session.connection_id)
        .ok_or_else(|| ApiError::bad_request("session has no connection; pick one first"))?;
    let saved = state
        .store
        .get_user_setting(user_id, &format!("session_model_{session_id}"))
        .await?
        .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok());
    let saved_model = saved
        .as_ref()
        .filter(|value| value["connection_id"].as_i64() == Some(connection_id))
        .and_then(|value| value["model"].as_str());
    let runtime = super::model_setup::runtime(
        state,
        user_id,
        connection_id,
        task_model
            .as_ref()
            .map(|model| model.model.as_str())
            .or(send.model.as_deref())
            .or(saved_model),
    )
    .await?;
    let system_prompt = match session.system_prompt_id {
        Some(id) => Some(state.store.get_system_prompt(id, user_id).await?.content),
        None => None,
    };
    let system_prompt = Some(with_temporal_context(system_prompt, now()));
    let history = openwebide_agent::session::history(
        state.store.list_messages(session_id).await?,
        &state.store.list_tool_steps(session_id).await?,
    );
    // Filesystem capability determines which tool primitives this host can offer.
    let project = match session.project_id {
        Some(id) => match state.store.get_project(id, user_id).await {
            Ok(project) => Some(project),
            Err(openwebide_storage::StorageError::NotFound(_)) => None,
            Err(error) => return Err(error.into()),
        },
        None => None,
    };
    let environment = openwebide_core::RunEnvironment {
        browser_preferences: send.browser_preferences,
        project_name: project.as_ref().map(|project| project.name.clone()),
        project_root: project
            .as_ref()
            .and_then(openwebide_core::run::execution_root),
        mode: project.as_ref().map(|project| project.mode),
        timestamp: now(),
    };
    let tools = if environment.project_root.is_some() {
        workspace_tools()
    } else {
        openwebide_agent::session::projectless_tools()
    };
    let memories = state.store.session_memories(user_id, session_id).await?;
    let mut input = openwebide_agent::session::PlanInput {
        environment,
        system_prompt,
        messages: history,
        tools,
        content: send.content,
        editor: send.editor_context,
    };
    super::plugins::ensure_bundled_plugins(state, user_id).await;
    let skills = state.store.session_skills(user_id, session_id).await?;
    if session.project_id.is_none()
        && state
            .store
            .get_user(user_id)
            .await?
            .is_some_and(|user| user.role == openwebide_core::UserRole::Admin)
        && state.store.host_connection().await?.is_configured()
    {
        input
            .tools
            .extend(openwebide_agent::host_admin::definitions());
    }
    let plugin_bindings = if let Some(project) = session.project_id {
        state.store.project_plugins(user_id, project).await?
    } else {
        openwebide_core::plugins::default_bindings(
            &state.store.plugin_installations(user_id).await?,
        )
    };
    openwebide_agent::plugins::configure(
        &mut input.tools,
        &mut input.system_prompt,
        &openwebide_agent::plugins::PluginContext {
            bindings: &plugin_bindings,
            memories: &memories,
            skills: &skills,
            context_limit: runtime.settings.context_limit,
        },
    );
    let plugin_context = openwebide_core::plugins::execution::PluginExecutionContext {
        user_action: false,
        project_id: session.project_id,
        session_id: Some(session_id),
        primary: runtime
            .connection
            .model
            .as_ref()
            .filter(|model| !model.trim().is_empty())
            .map(|model| openwebide_core::ModelSelection {
                server_id: runtime.connection.id,
                model: model.clone(),
            }),
    };
    let plugin_host = super::plugins::PlanningHost::new(state, user_id, session_id);
    let plan = openwebide_agent::session::plan_with_plugin_context(
        &runtime,
        input,
        &plugin_bindings,
        transport,
        plugin_host.clone(),
        |plugins| async move {
            super::plugins::context_grants(state, user_id, &plugin_context, &plugins)
                .await
                .map_err(|error| {
                    error.log_for_route("POST", "/api/sessions/run-plan");
                    error.public_message().to_owned()
                })
        },
    )
    .await;
    let mut plan = plan.map_err(ApiError::bad_request)?;
    plan.plugin_skills = openwebide_agent::skills::package_snapshot(&skills);
    plan.validate_prompt().map_err(ApiError::bad_request)?;
    Ok(plan)
}

pub(crate) async fn run_plan(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let user_id = user.id;
    let session_id = session_id(path)?;
    let native = req.headers().contains_key("authorization");
    let body = read_body(req, CHAT_BODY_LIMIT).await?;
    let send: SendMessageBody = parse_json(body)?;
    let mut plan = build_run_plan(state, user_id, session_id, send).await?;
    if !native {
        plan.transport = Default::default();
    }
    Ok(json_response(200, &plan))
}

/// Send a user message and stream the assistant reply back as SSE.
///
/// `state` is taken by value: the store is moved into the response body so
/// the assistant message can be persisted from inside the stream.
pub(crate) async fn send_session_message(
    req: Request,
    state: AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let started_ms = now_ms();
    let user_id = user.id;
    let session_id = session_id(path)?;
    let body = read_body(req, CHAT_BODY_LIMIT).await?;
    let send: SendMessageBody = parse_json(body)?;
    let queued_prompt = send.queued_prompt;
    let plan = build_run_plan(&state, user_id, session_id, send).await?;
    let lease = format!("spin-{session_id}-{started_ms}-{}", rand::random::<u128>());
    state
        .store
        .session_run_lease(user_id, session_id, &lease, false, now())
        .await?;
    let user_message = if let Some(key) = queued_prompt {
        state
            .store
            .consume_queued_prompt(user_id, session_id, key, &plan.user_content, now())
            .await
    } else {
        state
            .store
            .insert_message(session_id, Role::User, &plan.user_content, now())
            .await
    };
    let user_message = match user_message {
        Ok(message) => message,
        Err(error) => {
            let _ = state
                .store
                .session_run_lease(user_id, session_id, &lease, true, now())
                .await;
            return Err(error.into());
        }
    };
    let mut request = plan.request;
    request.messages.push(user_message.clone());
    let memo = ToolStreamMemo::new(plan.connection.tool_stream_unsupported);
    let connection_id = plan.connection.id;
    let tool_stream_revision = plan.connection.tool_stream_revision;
    let memo_model = plan.connection.model.clone().unwrap_or_default();
    let provider = Provider::for_connection_with_memo(
        &plan.connection,
        SpinHttpClient::default().with_transport(plan.transport),
        memo.clone(),
    );
    let store = Arc::new(state.store);
    let cancel =
        CancelFlag::new(store.clone(), session_id, started_ms).with_lease(user_id, lease.clone());
    let gate = PermissionPoller::new(store.clone(), session_id, started_ms);
    let memo_store = store.clone();
    let base = match &plan.kind {
        RunKind::Agent { project_path } => Some(project_path.clone()),
        _ => None,
    };
    let stream = if !matches!(plan.kind, RunKind::Chat) {
        agent_stream(
            store.clone(),
            user_id,
            session_id,
            user_message,
            request,
            plan.plugin_skills,
            plan.plugin_executables,
            plan.plugin_grants,
            provider,
            base,
            plan.environment,
            AgentConfig::default(),
            cancel,
            gate,
        )
    } else {
        let content = openwebide_agent::session::chat_context(&mut request, &plan.environment);
        let context = store
            .insert_message(session_id, Role::System, &content, now())
            .await?;
        let prepared = openwebide_agent::session::compact_request(
            &provider,
            &super::model_operations::ModelSource {
                store: store.clone(),
                user: user_id,
            },
            &mut request,
            &cancel,
            crate::agent::SessionPersistence {
                store: store.clone(),
                user: user_id,
                session: session_id,
                anchor: user_message.id,
            },
            session_id,
            user_message.id,
        )
        .await;
        if prepared.terminal {
            Box::pin(futures::stream::iter(
                [
                    openwebide_core::RunEvent::Message {
                        message: user_message,
                    },
                    openwebide_core::RunEvent::Message { message: context },
                ]
                .into_iter()
                .chain(prepared.events),
            ))
                as std::pin::Pin<Box<dyn futures::Stream<Item = openwebide_core::RunEvent> + Send>>
        } else {
            let mut stream = message_stream(
                store,
                user_id,
                session_id,
                user_message,
                provider.chat_stream(&request),
                started_ms,
                &request,
            );
            let first = stream.next().await;
            Box::pin(
                futures::stream::iter(
                    first
                        .into_iter()
                        .chain([openwebide_core::RunEvent::Message { message: context }]),
                )
                .chain(futures::stream::iter(prepared.events))
                .chain(stream),
            )
        }
    };

    Ok(Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(box_body(SseBody::new(Box::pin(stream.then(
            move |event| {
                let store = memo_store.clone();
                let memo = memo.clone();
                let model = memo_model.clone();
                let lease = lease.clone();
                async move {
                    if matches!(
                        event,
                        openwebide_core::RunEvent::Done { .. }
                            | openwebide_core::RunEvent::Cancelled
                            | openwebide_core::RunEvent::Error { .. }
                    ) {
                        let _ = store
                            .session_run_lease(user_id, session_id, &lease, true, now())
                            .await;
                    }
                    if memo.take_unrecorded()
                        && let Err(error) = store
                            .set_model_tool_stream_unsupported(
                                connection_id,
                                &model,
                                tool_stream_revision,
                            )
                            .await
                    {
                        eprintln!("session {session_id}: set_tool_stream_unsupported: {error}");
                    }
                    event
                }
            },
        )))))
        .expect("valid status and headers"))
}

/// Parse `/api/sessions/<id>...` into the session id.
/// Parse `/api/sessions/<id>/permissions/<tool_call_id>`.
#[derive(Deserialize)]
pub(super) struct PersistMessageBody {
    role: Role,
    content: String,
    #[serde(default)]
    usage: Option<TurnTelemetry>,
    #[serde(default)]
    tool_calls: Option<Vec<openwebide_core::ToolCall>>,
}

pub(crate) async fn persist_message(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let user_id = user.id;
    let id = session_id(path)?;
    state.store.get_session(id, user_id).await?;
    let body = read_body(req, CHAT_BODY_LIMIT).await?;
    let msg: PersistMessageBody = parse_json(body)?;
    if msg.role == Role::User {
        openwebide_core::PromptContent::attachments(&msg.content).map_err(ApiError::bad_request)?;
    }
    let message = state
        .store
        .insert_interim_message(
            id,
            msg.role,
            &msg.content,
            now(),
            msg.usage.as_ref(),
            msg.tool_calls.as_deref(),
        )
        .await?;
    Ok(json_response(201, &message))
}

/// Resolve a connection's context window: the connection's configured value,
/// else provider discovery. A provider error surfaces as `null`, not a 5xx,
/// so an unreachable runtime doesn't raise a banner.
#[derive(Deserialize)]
pub(super) struct UpsertToolStepBody {
    #[serde(default)]
    checkpoint: Option<openwebide_core::rewind::ProjectCheckpoint>,
    anchor_message_id: i64,
    tool_call_id: String,
    name: String,
    summary: String,
    #[serde(default)]
    diff: Option<FileDiff>,
}

pub(crate) async fn upsert_tool_step(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let user_id = user.id;
    let id = session_id(path)?;
    state.store.get_session(id, user_id).await?;
    let body = read_body(req, 96 * 1024 * 1024).await?;
    let step: UpsertToolStepBody = parse_json(body)?;
    if let Some(checkpoint) = &step.checkpoint {
        state
            .store
            .save_project_checkpoint(id, &step.tool_call_id, checkpoint)
            .await?;
    } else {
        state
            .store
            .upsert_tool_step(
                id,
                step.anchor_message_id,
                &step.tool_call_id,
                &step.name,
                &step.summary,
                now(),
                step.diff.as_ref(),
            )
            .await?;
    }
    Ok(json_response(200, &json!({ "ok": true })))
}

#[derive(Deserialize)]
pub(super) struct ToolTimingBody {
    tool_call_id: String,
    timing: openwebide_core::ToolTiming,
}

pub(crate) async fn save_tool_timing(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let id = session_id(path)?;
    state.store.get_session(id, user.id).await?;
    let body = read_body(req, CHAT_BODY_LIMIT).await?;
    save_tool_timing_body(&state.store, user.id, id, parse_json(body)?).await
}
pub(super) async fn save_tool_timing_body(
    store: &openwebide_storage::Store<crate::state::AppDb>,
    user: UserId,
    session: i64,
    body: ToolTimingBody,
) -> Result<JsonResp, ApiError> {
    store
        .save_tool_timing(user, session, &body.tool_call_id, &body.timing)
        .await?;
    Ok(json_response(200, &json!({"ok":true})))
}

#[derive(Deserialize)]
pub(super) struct CompleteToolStepBody {
    tool_call_id: String,
    ok: bool,
    result_summary: String,
    #[serde(default)]
    diff: Option<FileDiff>,
}

pub(crate) async fn complete_tool_step(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let user_id = user.id;
    let id = session_id(path)?;
    state.store.get_session(id, user_id).await?;
    let body = read_body(req, CHAT_BODY_LIMIT).await?;
    let step: CompleteToolStepBody = parse_json(body)?;
    state
        .store
        .complete_tool_step(
            user_id,
            id,
            &step.tool_call_id,
            step.ok,
            &step.result_summary,
            step.diff.as_ref(),
        )
        .await?;
    Ok(json_response(200, &json!({ "ok": true })))
}

#[derive(Deserialize)]
struct RewindBody {
    message_id: i64,
}

pub(crate) async fn rewind_session(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
    complete: bool,
) -> Result<JsonResp, ApiError> {
    let id = session_id(path)?;
    let body: RewindBody = parse_json(read_body(req, JSON_BODY_LIMIT).await?)?;
    if complete {
        let entries = state
            .store
            .complete_rewind(user.id, id, body.message_id)
            .await?;
        Ok(json_response(200, &entries))
    } else {
        let plan = state
            .store
            .prepare_rewind(user.id, id, body.message_id)
            .await?;
        Ok(json_response(200, &plan))
    }
}

#[derive(Deserialize)]
pub(super) struct TodoPlanBody {
    pub(super) anchor_message_id: i64,
    pub(super) plan: openwebide_core::TodoPlan,
}

pub(crate) async fn get_todo_plan(
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    Ok(json_response(
        200,
        &state
            .store
            .get_todo_plan(user.id, session_id(path)?)
            .await?,
    ))
}
pub(crate) async fn write_todo_plan(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let session = session_id(path)?;
    state.store.get_session(session, user.id).await?;
    let body: TodoPlanBody = parse_json(read_body(req, JSON_BODY_LIMIT).await?)?;
    write_todo_plan_body(state, session, user, body).await
}

pub(super) async fn write_todo_plan_body(
    state: &AppState,
    session: i64,
    user: AuthedUser,
    body: TodoPlanBody,
) -> Result<JsonResp, ApiError> {
    body.plan.validate().map_err(ApiError::bad_request)?;
    Ok(json_response(
        201,
        &state
            .store
            .write_todo_plan(user.id, session, body.anchor_message_id, &body.plan, now())
            .await?,
    ))
}

#[cfg(test)]
mod timing_tests {
    use super::*;
    use openwebide_core::{NewProject, ToolTiming, UserRole, WorkspaceMode};
    fn body(id: &str, timing: ToolTiming) -> ToolTimingBody {
        serde_json::from_value(json!({"tool_call_id":id,"timing":timing})).unwrap()
    }
    #[test]
    fn timing_api_is_owned_and_preserves_completed_durations_in_all_modes() {
        futures::executor::block_on(async {
            for mode in [
                Some(WorkspaceMode::Local),
                Some(WorkspaceMode::Remote),
                None,
            ] {
                let store =
                    openwebide_storage::Store::new(crate::state::AppDb::open_in_memory().unwrap());
                store.migrate_with(&|_| true).await.unwrap();
                let user = store
                    .insert_user("owner", "hash", UserRole::Admin, 1)
                    .await
                    .unwrap()
                    .id;
                let other = store
                    .insert_user("other", "hash", UserRole::User, 1)
                    .await
                    .unwrap()
                    .id;
                let project = if let Some(mode) = mode {
                    Some(
                        store
                            .create_project(
                                &NewProject {
                                    name: "test".into(),
                                    mode,
                                    path: Some("test".into()),
                                },
                                user,
                                1,
                            )
                            .await
                            .unwrap()
                            .id,
                    )
                } else {
                    None
                };
                let session = store
                    .create_session("test", None, None, project, user, 1)
                    .await
                    .unwrap()
                    .id;
                let anchor = store
                    .insert_message(session, Role::User, "prompt", 1)
                    .await
                    .unwrap()
                    .id;
                store
                    .upsert_tool_step(session, anchor, "t", "host_info", "host", 1, None)
                    .await
                    .unwrap();
                let timing = ToolTiming::start(1000).sample(4200, true);
                assert_eq!(
                    save_tool_timing_body(&store, user, session, body("t", timing))
                        .await
                        .unwrap()
                        .status(),
                    200
                );
                assert_eq!(
                    save_tool_timing_body(&store, other, session, body("t", timing))
                        .await
                        .unwrap_err()
                        .into_response()
                        .status()
                        .as_u16(),
                    404
                );
                assert_eq!(
                    save_tool_timing_body(&store, user, session, body("missing", timing))
                        .await
                        .unwrap_err()
                        .into_response()
                        .status()
                        .as_u16(),
                    404
                );
                assert_eq!(
                    save_tool_timing_body(
                        &store,
                        user,
                        session,
                        body("t", ToolTiming::start(1000))
                    )
                    .await
                    .unwrap()
                    .status(),
                    200
                );
                assert_eq!(
                    save_tool_timing_body(&store, user, session, body("t", ToolTiming::start(999)))
                        .await
                        .unwrap_err()
                        .into_response()
                        .status()
                        .as_u16(),
                    409
                );
                assert_eq!(
                    store.list_tool_steps(session).await.unwrap()[0].timing,
                    Some(timing)
                );
            }
            assert!(serde_json::from_value::<ToolTimingBody>(json!({"tool_call_id":"t","timing":{"started_at_ms":-1,"elapsed_ms":0,"finished":false}})).is_err());
        });
    }
}

#[derive(Deserialize)]
pub(super) struct TaskBody {
    anchor_message_id: i64,
    snapshot: openwebide_core::TaskSnapshot,
}
pub(crate) async fn save_task(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let session = session_id(path)?;
    state.store.get_session(session, user.id).await?;
    let body: TaskBody = parse_json(read_body(req, CHAT_BODY_LIMIT).await?)?;
    state
        .store
        .save_task(user.id, session, body.anchor_message_id, &body.snapshot)
        .await?;
    Ok(json_response(200, &json!({"ok":true})))
}

/// Explicit summaries are model-only operations shared by local and remote sessions.
pub(crate) async fn compact_session(
    req: Request,
    state: AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    #[derive(Deserialize)]
    struct Input {
        model: Option<String>,
    }
    let started_ms = now_ms();
    let session = session_id(path)?;
    state.store.get_session(session, user.id).await?;
    let input: Input = parse_json(read_body(req, CHAT_BODY_LIMIT).await?)?;
    let through = state
        .store
        .list_messages(session)
        .await?
        .last()
        .map(|message| message.id)
        .ok_or_else(|| ApiError::bad_request("There is no saved conversation to compact."))?;
    let plan = build_run_plan(
        &state,
        user.id,
        session,
        SendMessageBody {
            content: String::new(),
            model: input.model,
            editor_context: None,
            browser_preferences: None,
            queued_prompt: None,
        },
    )
    .await?;
    let provider = Provider::for_connection(
        &plan.connection,
        SpinHttpClient::default().with_transport(plan.transport),
    );
    let mut request = plan.request;
    let store = Arc::new(state.store);
    let cancel = CancelFlag::new(store.clone(), session, started_ms);
    let compaction = openwebide_agent::compaction::prepare_manual_cancelled(
        &provider,
        &super::model_operations::ModelSource {
            store: store.clone(),
            user: user.id,
        },
        &mut request,
        &cancel,
    )
    .await
    .map_err(ApiError::bad_request)?
    .ok_or_else(|| ApiError::bad_request("There is no conversation to compact."))?;
    let message = store
        .save_manual_compaction(user.id, session, through, &compaction, started_ms, now())
        .await?;
    Ok(json_response(200, &message))
}

pub(crate) async fn get_goal(
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    Ok(json_response(
        200,
        &state.store.get_goal(user.id, session_id(path)?).await?,
    ))
}
pub(crate) async fn update_goal(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    #[derive(Deserialize)]
    struct Input {
        expected_revision: u64,
        command: openwebide_core::GoalCommand,
        #[serde(default)]
        worker: bool,
        #[serde(default)]
        binding: Option<openwebide_core::scheduled::HostBinding>,
    }
    let input: Input = parse_json(read_body(req, 32_000).await?)?;
    let session = session_id(path)?;
    let hosted = input.worker
        || state
            .store
            .get_goal(user.id, session)
            .await?
            .is_some_and(|goal| goal.worker);
    let goal = if hosted {
        state
            .store
            .dispatch_goal(
                user.id,
                session,
                input.expected_revision,
                input.command,
                input.binding.as_ref(),
                now(),
            )
            .await?
    } else {
        state
            .store
            .update_goal(
                user.id,
                session,
                input.expected_revision,
                input.command,
                now(),
            )
            .await?
    };
    Ok(json_response(200, &goal))
}
