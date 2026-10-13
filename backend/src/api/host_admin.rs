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

/// Host-scoped updates are independent of the currently selected workspace mode.
pub(crate) async fn updates(
    req: Request,
    state: &AppState,
    user: AuthedUser,
) -> Result<JsonResp, ApiError> {
    use openwebide_core::host_admin::updates::{
        Build, UpdateCheck, UpdateHostRequest, UpdateJob, UpdatePhase, UpdateRequest,
        UpdateResponse,
    };
    admin(user)?;
    let request: UpdateRequest = parse_json(read_body(req, 16384).await?)?;
    let running = Build {
        version: env!("CARGO_PKG_VERSION").into(),
        commit: option_env!("OPENWEBIDE_BUILD_COMMIT")
            .unwrap_or("unknown")
            .into(),
        schema: openwebide_storage::migrations::SCHEMA_VERSION,
    };
    let relay = async |request: UpdateHostRequest| -> Result<Vec<u8>, ApiError> {
        let body = serde_json::to_string(&request)
            .map_err(|error| ApiError::internal(error.to_string()))?;
        let (status, body) = crate::bridge::send(&state.store, "/host/updates", body).await?;
        if status != 200 {
            #[derive(Deserialize)]
            struct Failure {
                error: String,
            }
            let detail = serde_json::from_slice::<Failure>(&body).map_or_else(
                |_| "Host updater unavailable. Check SSH and host update prerequisites.".into(),
                |failure| failure.error.chars().take(1024).collect::<String>(),
            );
            return Err(ApiError::bad_gateway(detail));
        }
        Ok(body.to_vec())
    };
    let mut response = match request {
        UpdateRequest::Discover => {
            let body = relay(UpdateHostRequest::Discover).await?;
            let deployment = serde_json::from_slice(&body)
                .map_err(|error| ApiError::internal(error.to_string()))?;
            UpdateResponse::Discovered { deployment }
        }
        UpdateRequest::Settings => UpdateResponse::Settings {
            settings: state.store.host_update_settings().await?,
            running,
            job: state.store.host_update_job().await?,
        },
        UpdateRequest::Save { settings } => UpdateResponse::Settings {
            settings: state.store.save_host_update_settings(&settings).await?,
            running,
            job: state.store.host_update_job().await?,
        },
        UpdateRequest::Check => {
            let settings = state.store.host_update_settings().await?;
            let body = relay(UpdateHostRequest::Check { settings, running }).await?;
            let check: UpdateCheck = serde_json::from_slice(&body)
                .map_err(|error| ApiError::internal(error.to_string()))?;
            UpdateResponse::Check { check }
        }
        UpdateRequest::Start {
            candidate,
            settings_revision,
            connection_revision,
        } => {
            candidate
                .validate(&running)
                .map_err(ApiError::bad_request)?;
            if !candidate.differs_from(&running) {
                return Err(ApiError::bad_request(
                    "The selected build is already running.",
                ));
            }
            let settings = state.store.host_update_settings().await?;
            if !state.store.host_connection().await?.is_configured() {
                return Err(ApiError::bad_request(
                    "Configure the host SSH connection before installing a release.",
                ));
            }
            if matches!(
                settings.deployment,
                openwebide_core::host_admin::updates::Deployment::Unconfigured
            ) {
                return Err(ApiError::bad_request("Configure a host updater first."));
            }
            let identity: [u8; 16] = rand::random();
            let mut job = UpdateJob {
                id: identity.iter().map(|byte| format!("{byte:02x}")).collect(),
                candidate,
                phase: UpdatePhase::Starting,
                detail: "Starting the host updater.".into(),
                settings_revision,
                connection_revision,
                created_at: now(),
            };
            state.store.claim_host_update(&job).await?;
            match relay(UpdateHostRequest::Start {
                settings,
                running: running.clone(),
                job: job.clone(),
            })
            .await
            {
                Ok(body) => {
                    use openwebide_core::host_admin::updates::UpdateLaunch;
                    match serde_json::from_slice::<UpdateLaunch>(&body)
                        .unwrap_or(UpdateLaunch::Uncertain)
                    {
                        UpdateLaunch::Started => {
                            job.phase = UpdatePhase::Running;
                            job.detail = "Host update started; reconnecting after restart.".into();
                        }
                        UpdateLaunch::Rejected { detail } => {
                            job.phase = UpdatePhase::Failed;
                            job.detail = detail;
                        }
                        UpdateLaunch::Uncertain => {
                            job.phase = UpdatePhase::Unknown;
                            job.detail = "No launch acknowledgement. Refresh status before trying another update.".into();
                        }
                    }
                }
                Err(_) => {
                    job.phase = UpdatePhase::Unknown;
                    job.detail =
                        "No launch acknowledgement. Refresh status before trying another update."
                            .into();
                }
            }
            state.store.save_host_update_job(&job).await?;
            UpdateResponse::Status {
                job: Some(job),
                running,
            }
        }
        UpdateRequest::Status => {
            let mut job = state.store.host_update_job().await?;
            if let Some(current) = job.as_mut() {
                let receipt = if current.phase.active() && current.candidate.differs_from(&running)
                {
                    relay(UpdateHostRequest::Status { id: current.id.clone() }).await.ok()
                        .map(|body| serde_json::from_slice::<openwebide_core::host_admin::updates::UpdateReceipt>(&body)
                            .map_err(|error| ApiError::internal(error.to_string()))).transpose()?
                } else {
                    None
                };
                openwebide_core::host_admin::updates::reconcile(current, &running, receipt, now());
                state.store.save_host_update_job(current).await?;
            }
            UpdateResponse::Status { job, running }
        }
    };
    if let UpdateResponse::Settings {
        job: Some(job),
        running,
        ..
    }
    | UpdateResponse::Status {
        job: Some(job),
        running,
    } = &mut response
    {
        let previous = job.clone();
        openwebide_core::host_admin::updates::reconcile(job, running, None, now());
        if *job != previous {
            state.store.save_host_update_job(job).await?;
        }
    }
    Ok(json_response(200, &response))
}
