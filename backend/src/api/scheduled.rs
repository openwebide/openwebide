use super::*;
use openwebide_core::scheduled::{DispatchResult, ExecutionHost, HostBinding, TaskCommand};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandBody {
    project_id: Option<i64>,
    command: TaskCommand,
    #[serde(default)]
    binding: Option<HostBinding>,
}
pub(crate) async fn list(
    state: &AppState,
    query: Option<&str>,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let project = query
        .and_then(|query| query.strip_prefix("project_id="))
        .map(str::parse::<i64>)
        .transpose()
        .map_err(|_| ApiError::bad_request("Invalid project"))?;
    Ok(json_response(
        200,
        &state.store.scheduled_tasks(user.id, project, now()).await?,
    ))
}
pub(crate) async fn monitors(
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    Ok(json_response(
        200,
        &state
            .store
            .scheduled_monitors(user.id, session_id(path)?, now())
            .await?,
    ))
}
pub(crate) async fn bind_host(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let binding: HostBinding = parse_json(read_body(req, 8192).await?)?;
    state
        .store
        .bind_background_host(
            user.id,
            path_id(
                path.strip_suffix("/execution-host")
                    .ok_or_else(|| ApiError::bad_request("Invalid execution host path"))?,
                "/api/projects",
            )?,
            &binding,
        )
        .await?;
    Ok(json_response(200, &json!({"ok":true})))
}
pub(crate) async fn command(
    req: Request,
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let body: CommandBody = parse_json(read_body(req, 64 * 1024).await?)?;
    let command =
        super::naming::task(&state.store, user.id, body.project_id, body.command, false).await?;
    Ok(json_response(
        200,
        &state
            .store
            .scheduled_command(
                user.id,
                body.project_id,
                &command,
                body.binding.as_ref(),
                false,
                now(),
            )
            .await?,
    ))
}
pub(crate) async fn session_command(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let command: TaskCommand = parse_json(read_body(req, 64 * 1024).await?)?;
    let session = session_id(path)?;
    let project = state.store.get_session(session, user.id).await?.project_id;
    let mut command = command;
    if let TaskCommand::Create { draft } | TaskCommand::Update { draft, .. } = &mut command
        && draft.session_target == openwebide_core::scheduled::SessionTarget::Existing
        && draft.session_id == 0
    {
        draft.session_id = session;
    }
    let command = super::naming::task(&state.store, user.id, project, command, true).await?;
    Ok(json_response(
        200,
        &state
            .store
            .scheduled_session_command(user.id, session_id(path)?, &command, now())
            .await?,
    ))
}
pub(crate) async fn due(req: Request, state: &AppState) -> Result<JsonResp, ApiError> {
    let host: ExecutionHost = parse_json(read_body(req, 4096).await?)?;
    Ok(json_response(
        200,
        &state.store.due_scheduled(&host, now()).await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultBody {
    host_id: String,
    result: DispatchResult,
}
pub(crate) async fn result(req: Request, state: &AppState) -> Result<JsonResp, ApiError> {
    let mut body: ResultBody = parse_json(read_body(req, 4096).await?)?;
    let assessment = if body.result.status == "complete" {
        goal_assessment(state, &body.host_id, body.result.run_id).await?
    } else {
        None
    };
    if body.result.status == "complete"
        && let Some((user, session, anchor)) = state
            .store
            .scheduled_run_session(&body.host_id, body.result.run_id)
            .await?
    {
        let messages = state.store.list_messages(session).await?;
        let final_message = super::completion::after_prompt(&messages, anchor);
        if let Some(message) = final_message
            && let Some(summary) =
                super::completion::summary(&state.store, user, session, Some(message)).await
        {
            body.result.detail = summary;
        }
    }
    state
        .store
        .scheduled_result_evaluated(&body.host_id, &body.result, assessment.as_ref(), now())
        .await?;
    Ok(json_response(200, &json!({"ok":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseBody {
    token: String,
    release: bool,
    #[serde(default)]
    since: i64,
    #[serde(default)]
    permission_id: Option<String>,
}
pub(crate) async fn lease(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    let body: LeaseBody = parse_json(read_body(req, 4096).await?)?;
    let session = session_id(path)?;
    state
        .store
        .session_run_lease(user.id, session, &body.token, body.release, now())
        .await?;
    let cancelled = state
        .store
        .cancel_requested_since(session, body.since)
        .await?
        || state
            .store
            .goal_run_cancelled(user.id, session, &body.token)
            .await?;
    let approved = if let Some(id) = body.permission_id {
        state.store.take_tool_permission(session, &id).await?
    } else {
        None
    };
    Ok(json_response(
        200,
        &openwebide_core::scheduled::RunControl {
            cancelled,
            approved,
        },
    ))
}

/// Evidence gathering and evaluation are shared across server and paired-host runs.
async fn goal_assessment(
    state: &AppState,
    host: &str,
    run: i64,
) -> Result<Option<openwebide_storage::store::GoalTurnAssessment>, ApiError> {
    let Some((user, goal, anchor)) = state.store.goal_run_context(host, run).await? else {
        return Ok(None);
    };
    let session = state.store.get_session(goal.session_id, user).await?;
    let messages = state.store.list_messages(goal.session_id).await?;
    let Some(last) = messages.last().filter(|message| {
        message.role == Role::Assistant && message.tool_calls.as_ref().is_none_or(Vec::is_empty)
    }) else {
        return Ok(None);
    };
    if super::completion::after_prompt(&messages, anchor) != Some(last.id) {
        return Ok(None);
    };
    let steps = state.store.list_tool_steps(goal.session_id).await?;
    let steps = steps
        .iter()
        .filter(|step| step.anchor_message_id == anchor)
        .collect::<Vec<_>>();
    let mut evidence = format!(
        "Goal: {}\n\nFinal response: {}",
        goal.objective,
        openwebide_core::strip_reasoning(&last.content)
            .chars()
            .take(4000)
            .collect::<String>()
    );
    for step in steps.iter().rev().take(16) {
        evidence.push_str(&format!(
            "\nTool {}: {:?}. {}",
            step.name,
            step.ok,
            step.result_summary
                .as_deref()
                .unwrap_or("unfinished")
                .chars()
                .take(800)
                .collect::<String>()
        ));
    }
    for message in messages.iter().rev().skip(1).take(12) {
        evidence.push_str(&format!(
            "\nEarlier {:?}: {}",
            message.role,
            openwebide_core::strip_reasoning(&message.content)
                .chars()
                .take(400)
                .collect::<String>()
        ));
    }
    let evidence = openwebide_core::assistance::input_excerpt(&evidence);
    let connection = session.connection_id.or(state
        .store
        .model_setup(user)
        .await?
        .defaults
        .primary
        .map(|model| model.server_id));
    let Some(connection_id) = connection else {
        return Ok(None);
    };
    let request = openwebide_core::AssistanceRequest {
        kind: openwebide_core::AssistanceKind::GoalEvaluation,
        model: None,
        staged_draft: false,
        connection_id,
        session_id: Some(goal.session_id),
        project_id: session.project_id,
        input: evidence,
    };
    let generated = super::assistance::execute(&state.store, user, &request).await;
    let Ok(Some(text)) = generated else {
        return Ok(None);
    };
    let Ok(evaluation) = openwebide_core::goal::GoalEvaluation::parse(&text) else {
        return Ok(None);
    };
    Ok(Some(openwebide_storage::store::GoalTurnAssessment {
        evaluation,
        last_message: last.id,
        used_tools: !steps.is_empty(),
    }))
}
