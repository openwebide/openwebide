//! Authenticated durable action and native command transport for both modes.
use super::*;
use openwebide_core::plugins::languages::intelligence::actions::{
    ActionJournalRequest, CommandDispatch, CommandProgress, HostCommandRequest, MAX_APPROVAL_BYTES,
};

fn project(path: &str, suffix: &str) -> Result<i64, ApiError> {
    path_id(
        path.strip_suffix(suffix)
            .ok_or_else(|| ApiError::bad_request("Invalid language action address"))?,
        "/api/projects",
    )
}

pub(crate) async fn journal<B>(
    req: Request<B>,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError>
where
    B: http_body::Body + Send + 'static,
    B::Data: AsRef<[u8]> + Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let project = project(path, "/language-actions")?;
    state.store.get_project(project, user.id).await?;
    let request: ActionJournalRequest =
        parse_json(read_body(req, MAX_APPROVAL_BYTES + 4 * 1024 * 1024).await?)?;
    Ok(json_response(
        200,
        &state
            .store
            .language_action_request(user.id, project, &request, now())
            .await?,
    ))
}

pub(crate) async fn recovery<B>(
    req: Request<B>,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError>
where
    B: http_body::Body + Send + 'static,
    B::Data: AsRef<[u8]> + Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let project = project(path, "/language-actions/recovery")?;
    state.store.get_project(project, user.id).await?;
    let request: openwebide_core::plugins::languages::intelligence::actions::ActionRecoveryRequest =
        parse_json(
            read_body(
                req,
                openwebide_core::plugins::languages::intelligence::actions::MAX_COMMAND_BYTES
                    + 8192,
            )
            .await?,
        )?;
    let mut response = json_response(
        200,
        &state
            .store
            .language_action_recovery(user.id, project, &request, now())
            .await?,
    );
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    Ok(response)
}

pub(crate) async fn dispatch<B>(
    req: Request<B>,
    state: &AppState,
    path: &str,
    user: AuthedUser,
    authorize_only: bool,
) -> Result<JsonResp, ApiError>
where
    B: http_body::Body + Send + 'static,
    B::Data: AsRef<[u8]> + Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let project = project(
        path,
        if authorize_only {
            "/language-commands/authorize"
        } else {
            "/language-commands"
        },
    )?;
    let owned = state.store.get_project(project, user.id).await?;
    let request: CommandDispatch = parse_json(read_body(req, MAX_APPROVAL_BYTES).await?)?;
    let command = state
        .store
        .authorize_language_command(user.id, project, &request, now())
        .await?;
    if authorize_only {
        return Ok(json_response(200, &command));
    }
    if let Some(progress) = &command.progress {
        return Ok(json_response(200, progress));
    }
    let root = openwebide_core::run::execution_root(&owned).ok_or_else(|| {
        ApiError::bad_request("Use the paired execution host for this local project")
    })?;
    let progress: CommandProgress = plugins::send_plugin_rpc(
        &state.store,
        user.id,
        "/plugins/languages/command",
        serde_json::to_value(HostCommandRequest {
            user: None,
            root,
            command,
            authority: request.call.as_ref().map(|call| {
                openwebide_core::plugins::languages::intelligence::actions::CommandAuthority {
                    grant: call.grant.clone(),
                    session_id: call.session_id,
                }
            }),
            journal: None,
        })
        .map_err(|e| ApiError::bad_request(e.to_string()))?,
    )
    .await
    .map_err(ApiError::bad_request)?;
    Ok(json_response(200, &progress))
}

pub(crate) async fn reconnect<B>(
    req: Request<B>,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError>
where
    B: http_body::Body + Send + 'static,
    B::Data: AsRef<[u8]> + Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    use openwebide_core::plugins::languages::intelligence::actions::CommandCallbackRequest;
    let project = project(path, "/language-commands/reconnect")?;
    let owned = state.store.get_project(project, user.id).await?;
    if owned.mode != openwebide_core::WorkspaceMode::Remote {
        return Err(ApiError::bad_request(
            "Use the paired execution host for this local project",
        ));
    }
    let mut request: CommandCallbackRequest = parse_json(read_body(req, 8192).await?)?;
    request.validate().map_err(ApiError::bad_request)?;
    if request.project != project {
        return Err(ApiError::bad_request("Command recovery project changed"));
    }
    request.user = Some(user.id.get());
    let changed: bool = plugins::send_plugin_rpc(
        &state.store,
        user.id,
        "/plugins/languages/command/reconnect",
        serde_json::to_value(request)
            .map_err(|_| ApiError::bad_request("Invalid command callback request"))?,
    )
    .await
    .map_err(ApiError::bad_request)?;
    Ok(json_response(200, &changed))
}

pub(crate) async fn delegate<B>(
    req: Request<B>,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError>
where
    B: http_body::Body + Send + 'static,
    B::Data: AsRef<[u8]> + Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let project = project(path, "/language-commands/delegate")?;
    let request:openwebide_core::plugins::languages::intelligence::actions::CommandDelegationRequest=parse_json(read_body(req,64*1024).await?)?;
    let grant = state
        .store
        .delegate_language_command(user.id, project, &request, now())
        .await?;
    let mut response = json_response(200, &grant);
    response.headers_mut().insert(
        "cache-control",
        spin_sdk::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

pub(crate) async fn delegated<B>(req: Request<B>, state: &AppState) -> Result<JsonResp, ApiError>
where
    B: http_body::Body + Send + 'static,
    B::Data: AsRef<[u8]> + Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    use openwebide_core::plugins::languages::intelligence::actions::{
        CommandJournalOperation, DelegatedCommandRequest, MAX_COMMAND_BYTES,
    };
    if ["origin", "sec-fetch-site", "cookie"]
        .iter()
        .any(|header| req.headers().contains_key(*header))
    {
        return Err(ApiError::new(
            403,
            "Paired command callbacks require the native worker transport",
        ));
    }
    let input: DelegatedCommandRequest =
        parse_json(read_body(req, MAX_COMMAND_BYTES + 64 * 1024).await?)?;
    let mut value = state
        .store
        .delegated_language_command(&input, now())
        .await?;
    if matches!(input.operation, CommandJournalOperation::Scope) {
        let openwebide_core::workspace_transaction::WorkspaceJournalResponse::CommandScope {
            recovery,
        } = value
        else {
            return Err(ApiError::internal("Invalid private command scope"));
        };
        value = crate::recovery_authority::issue_command(
            state,
            input.delegation.user,
            input.delegation.project,
            recovery.map(|v| *v),
        )
        .await?;
    }
    let mut response = json_response(200, &value);
    response.headers_mut().insert(
        "cache-control",
        spin_sdk::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

pub(crate) async fn original<B>(req: Request<B>, state: &AppState) -> Result<JsonResp, ApiError>
where
    B: http_body::Body + Send + 'static,
    B::Data: AsRef<[u8]> + Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    use openwebide_core::plugins::languages::intelligence::actions::{
        MAX_COMMAND_BYTES, OriginalCommandRequest,
    };
    if ["origin", "sec-fetch-site", "cookie"]
        .iter()
        .any(|h| req.headers().contains_key(*h))
    {
        return Err(ApiError::new(
            403,
            "Original command recovery requires the native transport",
        ));
    }
    let input: OriginalCommandRequest =
        parse_json(read_body(req, MAX_COMMAND_BYTES + 64 * 1024).await?)?;
    let value = original_reply(state, &input).await?;
    let mut response = json_response(200, &value);
    response.headers_mut().insert(
        "cache-control",
        spin_sdk::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

async fn original_reply(
    state: &AppState,
    input: &openwebide_core::plugins::languages::intelligence::actions::OriginalCommandRequest,
) -> Result<openwebide_core::workspace_transaction::WorkspaceJournalResponse, ApiError> {
    use openwebide_core::plugins::languages::intelligence::actions::CommandJournalOperation;
    input.validate().map_err(ApiError::bad_request)?;
    crate::recovery_authority::verify(state, &input.authority).await?;
    let mut value = state.store.original_language_command(input, now()).await?;
    if matches!(input.operation, CommandJournalOperation::Scope) {
        let openwebide_core::workspace_transaction::WorkspaceJournalResponse::CommandScope {
            recovery,
        } = value
        else {
            return Err(ApiError::internal("Invalid original command scope"));
        };
        value =
            crate::recovery_authority::issue_original_command(state, input, recovery.map(|r| *r))
                .await?;
    }
    Ok(value)
}

pub(crate) async fn original_private<B>(
    req: Request<B>,
    state: &AppState,
) -> Result<JsonResp, ApiError>
where
    B: http_body::Body + Send + 'static,
    B::Data: AsRef<[u8]> + Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    use openwebide_core::plugins::languages::intelligence::actions::{
        COMMAND_RECOVERY_MESSAGE_LIMIT, COMMAND_RECOVERY_PLAINTEXT_LIMIT,
        COMMAND_RECOVERY_REPLY_DOMAIN, COMMAND_RECOVERY_REQUEST_DOMAIN, CommandRecoveryMessage,
        CommandRecoveryReply, OriginalCommandRequest,
    };
    if ["origin", "sec-fetch-site", "cookie"]
        .iter()
        .any(|h| req.headers().contains_key(*h))
    {
        return Err(ApiError::new(
            403,
            "Original command recovery requires the native transport",
        ));
    }
    let message: CommandRecoveryMessage =
        parse_json(read_body(req, COMMAND_RECOVERY_MESSAGE_LIMIT).await?)?;
    let authority = crate::recovery_authority::command_message_authority(state, &message).await?;
    let context = message.context().map_err(ApiError::bad_request)?;
    let plaintext = openwebide_auth::private_messages::open(
        authority.secret.as_bytes(),
        COMMAND_RECOVERY_REQUEST_DOMAIN,
        &context,
        &message.nonce,
        &message.ciphertext,
        COMMAND_RECOVERY_PLAINTEXT_LIMIT,
    )
    .map_err(|_| ApiError::new(401, "Original command message could not be authenticated"))?;
    let input: OriginalCommandRequest = serde_json::from_slice(&plaintext)
        .map_err(|_| ApiError::bad_request("Invalid original command message"))?;
    if input.authority != authority || !message.matches_authority(&input.authority) {
        return Err(ApiError::new(
            401,
            "Original command message changed identity",
        ));
    }
    let value = original_reply(state, &input).await?;
    let plaintext = serde_json::to_vec(&value)
        .map_err(|_| ApiError::internal("Invalid original command reply"))?;
    if plaintext.len() > COMMAND_RECOVERY_PLAINTEXT_LIMIT {
        return Err(ApiError::internal(
            "Original command reply exceeds its bound",
        ));
    }
    let (nonce, ciphertext) = openwebide_auth::private_messages::seal(
        authority.secret.as_bytes(),
        COMMAND_RECOVERY_REPLY_DOMAIN,
        &message.reply_context().map_err(ApiError::bad_request)?,
        &plaintext,
    )
    .map_err(|_| ApiError::internal("Original command reply is unavailable"))?;
    let mut response = json_response(200, &CommandRecoveryReply { nonce, ciphertext });
    response.headers_mut().insert(
        "cache-control",
        spin_sdk::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwebide_core::{NewProject, UserRole};
    mod reconnect;
    mod recorded;

    fn request(body: &str, size: usize) -> Request<FullBody<Bytes>> {
        Request::builder()
            .method("POST")
            .uri("http://localhost/api")
            .header("content-length", size.to_string())
            .body(FullBody::new(Bytes::copy_from_slice(body.as_bytes())))
            .unwrap()
    }

    #[test]
    fn action_http_contract_checks_ownership_limits_and_claims_in_both_modes() {
        futures::executor::block_on(async {
            for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
                let state = AppState::new().await.unwrap();
                let owner = state
                    .store
                    .insert_user("owner", "hash", UserRole::Admin, 1)
                    .await
                    .unwrap();
                let other = state
                    .store
                    .insert_user("other", "hash", UserRole::User, 1)
                    .await
                    .unwrap();
                let project = state
                    .store
                    .create_project(
                        &NewProject {
                            name: "commands".into(),
                            mode,
                            path: Some("project".into()),
                        },
                        owner.id,
                        1,
                    )
                    .await
                    .unwrap();
                let owner = AuthedUser {
                    id: owner.id,
                    role: owner.role,
                };
                let other = AuthedUser {
                    id: other.id,
                    role: other.role,
                };
                let path = format!("/api/projects/{}/language-actions", project.id);
                assert_eq!(
                    journal(request("", MAX_APPROVAL_BYTES * 2), &state, &path, other)
                        .await
                        .unwrap_err()
                        .into_response()
                        .status(),
                    404
                );
                assert_eq!(
                    journal(request("", MAX_APPROVAL_BYTES * 2), &state, &path, owner)
                        .await
                        .unwrap_err()
                        .into_response()
                        .status(),
                    413
                );
                let body = r#"{"operation":"load","id":null}"#;
                let response = journal(request(body, body.len()), &state, &path, owner)
                    .await
                    .unwrap();
                assert_eq!(response.status(), 200);
                assert_eq!(
                    response.into_body().collect().await.unwrap().to_bytes(),
                    "null"
                );
                for authorize_only in [false, true] {
                    let path = format!(
                        "/api/projects/{}/language-commands{}",
                        project.id,
                        if authorize_only { "/authorize" } else { "" }
                    );
                    assert_eq!(
                        dispatch(
                            request("", MAX_APPROVAL_BYTES + 1),
                            &state,
                            &path,
                            other,
                            authorize_only
                        )
                        .await
                        .unwrap_err()
                        .into_response()
                        .status(),
                        404
                    );
                    assert_eq!(
                        dispatch(
                            request("", MAX_APPROVAL_BYTES + 1),
                            &state,
                            &path,
                            owner,
                            authorize_only
                        )
                        .await
                        .unwrap_err()
                        .into_response()
                        .status(),
                        413
                    );
                    let body =
                        json!({"id":"a".repeat(32),"operation":"start","call":null}).to_string();
                    assert_eq!(
                        dispatch(
                            request(&body, body.len()),
                            &state,
                            &path,
                            owner,
                            authorize_only
                        )
                        .await
                        .unwrap_err()
                        .into_response()
                        .status(),
                        409
                    );
                    let body = json!({"id":"a".repeat(32),"operation":"start","call":null,"command":"arbitrary-command"}).to_string();
                    assert_eq!(
                        dispatch(
                            request(&body, body.len()),
                            &state,
                            &path,
                            owner,
                            authorize_only
                        )
                        .await
                        .unwrap_err()
                        .into_response()
                        .status(),
                        400
                    );
                }
            }
        });
    }
}
