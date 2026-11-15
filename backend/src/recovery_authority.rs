//! Backend namespace for original recovery owners. Only private native routes
//! call this issuer after authenticating the project/journal. Session and bridge
//! secret rotation cannot change an existing owner's credential.
use crate::{error::ApiError, state::AppState};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use openwebide_core::workspace_transaction::{
    RECOVERY_AUTHORITY_DOMAIN, RecoveryAuthority, WorkspaceJournalResponse,
    WorkspaceTransactionScope,
};
use rand::Rng;

const SECRET_KEY: &str = "workspace_recovery_secret_v1";
#[cfg(test)]
mod tests;

async fn original_secret(state: &AppState) -> Result<Vec<u8>, ApiError> {
    if state.store.get_setting(SECRET_KEY).await?.is_none() {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        // Concurrent component instances must all observe the winning insert.
        state
            .store
            .insert_setting_if_absent(SECRET_KEY, &URL_SAFE_NO_PAD.encode(bytes))
            .await?;
    }
    let saved = state
        .store
        .get_setting(SECRET_KEY)
        .await?
        .ok_or_else(|| ApiError::internal("Original recovery secret is unavailable"))?;
    if saved.len() != 43 {
        return Err(ApiError::internal("Original recovery secret is invalid"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(saved)
        .map_err(|_| ApiError::internal("Original recovery secret is invalid"))?;
    if bytes.len() != 32 {
        return Err(ApiError::internal("Original recovery secret is invalid"));
    }
    Ok(bytes)
}

/// Issue from an already-authorized, authoritative store read. Neither caller
/// claims nor an earlier owner's secret are accepted by this interface. The
/// credential alone never authorizes mutation or replaces native phase checks.
pub(crate) async fn issue(
    state: &AppState,
    user: i64,
    project: i64,
    scope: Option<WorkspaceTransactionScope>,
) -> Result<WorkspaceJournalResponse, ApiError> {
    let scope =
        scope.ok_or_else(|| ApiError::not_found("Original workspace recovery is unavailable"))?;
    let (current, activity) = state
        .store
        .workspace_transaction_activity(openwebide_core::UserId::new(user), project, &scope.id)
        .await?
        .ok_or_else(|| ApiError::not_found("Original workspace recovery is unavailable"))?;
    if current != scope {
        return Err(ApiError::new(
            409,
            "Original workspace recovery changed; reload before continuing",
        ));
    }
    let key = original_secret(state).await?;
    let authority = credential(user, project, &current, &key)?;
    Ok(WorkspaceJournalResponse::RecoveryScope {
        scope,
        authority: Box::new(authority),
        activity,
    })
}

fn credential(
    user: i64,
    project: i64,
    scope: &WorkspaceTransactionScope,
    key: &[u8],
) -> Result<RecoveryAuthority, ApiError> {
    let binding = RecoveryAuthority::binding_for_scope(user, project, scope)
        .map_err(|_| ApiError::internal("Invalid original recovery scope"))?;
    let secret = openwebide_auth::derive_secret(key, RECOVERY_AUTHORITY_DOMAIN, &binding);
    RecoveryAuthority::for_scope(user, project, scope, secret)
        .map_err(|_| ApiError::internal("Invalid original recovery authority"))
}

pub(crate) async fn verify(
    state: &AppState,
    authority: &RecoveryAuthority,
) -> Result<(), ApiError> {
    authority
        .validate()
        .map_err(|_| ApiError::new(401, "Invalid original recovery authority"))?;
    // Verification never creates a replacement issuer for missing private state.
    let key = existing_secret(state).await?;
    let scope = WorkspaceTransactionScope {
        id: authority.transaction.clone(),
        generation: authority.generation.clone(),
        revision: 1,
        root_identity: authority.root_identity.clone(),
        root: authority.root.clone(),
        worker: None,
        accepted_language_review_digest: None,
    };
    if credential(authority.user, authority.project, &scope, &key)? != *authority {
        return Err(ApiError::new(401, "Original recovery authority changed"));
    }
    Ok(())
}

async fn existing_secret(state: &AppState) -> Result<Vec<u8>, ApiError> {
    let saved = state
        .store
        .get_setting(SECRET_KEY)
        .await?
        .ok_or_else(|| ApiError::new(401, "Original recovery issuer is unavailable"))?;
    let key = URL_SAFE_NO_PAD
        .decode(saved)
        .map_err(|_| ApiError::internal("Invalid original recovery issuer"))?;
    if key.len() != 32 {
        return Err(ApiError::internal("Invalid original recovery issuer"));
    }
    Ok(key)
}

/// Derive only within the already-existing backend namespace. Public routing
/// claims reveal no credential; a valid private body must prove possession next.
pub(crate) async fn command_message_authority(
    state: &AppState,
    message: &openwebide_core::plugins::languages::intelligence::actions::CommandRecoveryMessage,
) -> Result<RecoveryAuthority, ApiError> {
    message.context().map_err(ApiError::bad_request)?;
    credential(
        message.user,
        message.project,
        &message.scope,
        &existing_secret(state).await?,
    )
}

/// The private retained settlement, rather than public history, remains the
/// source of authority after pruning. Never initialize a replacement issuer.
pub(crate) async fn issue_original_command(
    state: &AppState,
    request: &openwebide_core::plugins::languages::intelligence::actions::OriginalCommandRequest,
    expected: Option<openwebide_core::workspace_transaction::WorkspaceCommandRecovery>,
) -> Result<WorkspaceJournalResponse, ApiError> {
    use openwebide_core::plugins::languages::intelligence::actions::CommandJournalOperation;
    let expected =
        expected.ok_or_else(|| ApiError::not_found("Original command recovery is unavailable"))?;
    let mut read = request.clone();
    read.operation = CommandJournalOperation::Scope;
    let WorkspaceJournalResponse::CommandScope {
        recovery: Some(current),
    } = state.store.original_language_command(&read, 0).await?
    else {
        return Err(ApiError::not_found(
            "Original command recovery is unavailable",
        ));
    };
    if *current != expected {
        return Err(ApiError::new(
            409,
            "Original command recovery changed; reload before continuing",
        ));
    }
    let authority = credential(
        request.authority.user,
        request.authority.project,
        &current.scope,
        &existing_secret(state).await?,
    )?;
    if authority != request.authority {
        return Err(ApiError::new(401, "Original recovery authority changed"));
    }
    Ok(WorkspaceJournalResponse::CommandRecoveryScope {
        recovery: current,
        authority: Box::new(authority),
    })
}

/// Command identity and both phases must still equal the authenticated compact
/// observation. The caller never supplies or reflects an old private secret.
pub(crate) async fn issue_command(
    state: &AppState,
    user: i64,
    project: i64,
    expected: Option<openwebide_core::workspace_transaction::WorkspaceCommandRecovery>,
) -> Result<WorkspaceJournalResponse, ApiError> {
    let expected =
        expected.ok_or_else(|| ApiError::not_found("Original command recovery is unavailable"))?;
    expected
        .validate()
        .map_err(|_| ApiError::internal("Original command recovery metadata is invalid"))?;
    let recovery = state
        .store
        .workspace_command_recovery(
            openwebide_core::UserId::new(user),
            project,
            &expected.scope.id,
        )
        .await?
        .ok_or_else(|| ApiError::not_found("Original command recovery is unavailable"))?;
    if recovery != expected {
        return Err(ApiError::new(
            409,
            "Original command recovery changed; reload before continuing",
        ));
    }
    let key = original_secret(state).await?;
    let authority = credential(user, project, &recovery.scope, &key)?;
    Ok(WorkspaceJournalResponse::CommandRecoveryScope {
        recovery: Box::new(recovery),
        authority: Box::new(authority),
    })
}
