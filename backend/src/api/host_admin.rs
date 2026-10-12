//! Host journal routes: service-secret writes; authenticated project-less reads.
use super::*;
use openwebide_core::host_admin::{HostJournalCommand, HostJournalResult, HostRequest};

pub(crate) async fn journal(req: Request, state: &AppState) -> Result<JsonResp, ApiError> {
    let command: HostJournalCommand = parse_json(read_body(req, 2 * JSON_BODY_LIMIT).await?)?;
    let result = match command {
        HostJournalCommand::Connection => {
            HostJournalResult::Connection(state.store.host_connection().await?)
        }
        HostJournalCommand::Prepare {
            user,
            session,
            request_id,
            plan,
            boot_id,
            connection_revision,
        } => HostJournalResult::Operation(Box::new(
            state
                .store
                .prepare_host_operation(
                    user,
                    session,
                    &request_id,
                    &plan,
                    &boot_id,
                    connection_revision,
                    now(),
                )
                .await?,
        )),
        HostJournalCommand::Get { user, session, id } => HostJournalResult::Operation(Box::new(
            state.store.host_operation(user, session, id).await?,
        )),
        HostJournalCommand::List { user, session } => {
            HostJournalResult::Operations(state.store.host_operations(user, session).await?)
        }
        HostJournalCommand::Claim {
            user,
            session,
            id,
            target,
            boot_id,
            connection_revision,
        } => HostJournalResult::Operation(Box::new(
            state
                .store
                .claim_host_operation(
                    user,
                    session,
                    id,
                    &target,
                    &boot_id,
                    connection_revision,
                    now(),
                )
                .await?,
        )),
        HostJournalCommand::Save { operation } => {
            state.store.save_host_operation(&operation).await?;
            HostJournalResult::Saved
        }
        HostJournalCommand::Active => {
            HostJournalResult::Operations(state.store.active_host_operations().await?)
        }
    };
    Ok(json_response(200, &result))
}
pub(crate) async fn inspect(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    admin(user)?;
    let session = session_id(path)?;
    state
        .store
        .require_host_session(user.id.get(), session)
        .await?;
    let request: HostRequest = parse_json(read_body(req, 4096).await?)?;
    // UI endpoint cannot bypass agent approval for mutations.
    if !matches!(request, HostRequest::Inspect | HostRequest::Operations) {
        return Err(ApiError::forbidden(
            "Prepare and approve host operations in project-less chat.",
        ));
    }
    let body =
        serde_json::json!({"user":user.id.get(), "session":session, "request":request}).to_string();
    let (status, response) = crate::bridge::send(&state.store, "/host/admin", body).await?;
    if status != 200 {
        return Err(ApiError::bad_gateway(
            "Host administration is unavailable; check the server bridge.",
        ));
    }
    let response: openwebide_core::host_admin::HostResponse =
        serde_json::from_slice(&response).map_err(|error| ApiError::internal(error.to_string()))?;
    Ok(json_response(200, &response))
}

fn admin(user: AuthedUser) -> Result<(), ApiError> {
    if user.role != openwebide_core::UserRole::Admin {
        Err(ApiError::forbidden(
            "Host administration requires an administrator account.",
        ))
    } else {
        Ok(())
    }
}
pub(crate) async fn connection(
    req: Request,
    state: &AppState,
    user: AuthedUser,
    write: bool,
) -> Result<JsonResp, ApiError> {
    admin(user)?;
    let connection = if write {
        let connection: openwebide_core::host_admin::HostConnection =
            parse_json(read_body(req, 128 * 1024).await?)?;
        state.store.save_host_connection(&connection).await?
    } else {
        state.store.host_connection().await?
    };
    Ok(json_response(200, &connection))
}
pub(crate) async fn probe(state: &AppState, user: AuthedUser) -> Result<JsonResp, ApiError> {
    admin(user)?;
    let (status, body) = crate::bridge::send(&state.store, "/host/probe", "{}".into()).await?;
    if status != 200 {
        return Err(ApiError::bad_gateway(format!(
            "Host connection failed: {}",
            String::from_utf8_lossy(&body)
        )));
    }
    let environment: openwebide_core::host_admin::HostEnvironment =
        serde_json::from_slice(&body).map_err(|error| ApiError::internal(error.to_string()))?;
    Ok(json_response(200, &environment))
}
pub(crate) async fn input(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    admin(user)?;
    let session = session_id(path)?;
    state
        .store
        .require_host_session(user.id.get(), session)
        .await?;
    let input: openwebide_core::host_admin::HostInput =
        parse_json(read_body(req, 16 * 1024).await?)?;
    input.validate().map_err(ApiError::bad_request)?;
    let payload =
        serde_json::json!({"user":user.id.get(),"session":session,"input":input}).to_string();
    let (status, body) = crate::bridge::send(&state.store, "/host/input", payload).await?;
    if status != 200 {
        return Err(ApiError::conflict(
            "This host prompt is no longer available; refresh the operation.",
        ));
    }
    let _: serde_json::Value =
        serde_json::from_slice(&body).map_err(|error| ApiError::internal(error.to_string()))?;
    Ok(json_response(200, &serde_json::json!({"ok":true})))
}

pub(crate) async fn input_key(
    req: Request,
    state: &AppState,
    path: &str,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    admin(user)?;
    let session = session_id(path)?;
    state
        .store
        .require_host_session(user.id.get(), session)
        .await?;
    let prompt: openwebide_core::host_admin::HostInputKeyRequest =
        parse_json(read_body(req, 4096).await?)?;
    prompt.validate().map_err(ApiError::bad_request)?;
    let payload = serde_json::json!({"user": user.id.get(), "session": session, "prompt": prompt})
        .to_string();
    let (status, body) = crate::bridge::send(&state.store, "/host/input-key", payload).await?;
    if status != 200 {
        return Err(ApiError::conflict(
            "This host prompt is no longer available; refresh the operation.",
        ));
    }
    let key: openwebide_core::host_admin::HostInputKey =
        serde_json::from_slice(&body).map_err(|error| ApiError::internal(error.to_string()))?;
    key.validate_scope(user.id.get(), session, &prompt)
        .map_err(ApiError::conflict)?;
    Ok(json_response(200, &key))
}
