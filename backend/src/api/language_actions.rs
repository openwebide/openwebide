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
        })
        .map_err(|e| ApiError::bad_request(e.to_string()))?,
    )
    .await
    .map_err(ApiError::bad_request)?;
    Ok(json_response(200, &progress))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwebide_core::{NewProject, UserRole};

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
