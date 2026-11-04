use super::*;
// -- auth --------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct RegisterBody {
    username: String,
    password: String,
}

/// Create the first (admin) account. Registration closes once any account
/// exists, so this returns 403 after the first user signs up.
pub(crate) async fn register(req: Request, state: &AppState) -> Result<JsonResp, ApiError> {
    if !crate::auth::csrf_header_ok(req.headers()) {
        return Err(ApiError::unauthorized("not signed in"));
    }
    let is_https = crate::auth::is_https(req.headers());
    let body = read_body(req, AUTH_BODY_LIMIT).await?;
    let reg: RegisterBody = parse_json(body)?;
    let username = reg.username.trim();
    if username.is_empty() {
        return Err(ApiError::bad_request("username is required"));
    }
    if reg.password.len() < 8 {
        return Err(ApiError::bad_request(
            "password must be at least 8 characters",
        ));
    }
    if state.store.count_users().await? > 0 {
        return Err(ApiError::forbidden(
            "registration is closed; an account already exists",
        ));
    }
    let password_hash = crate::auth::hash_password(&reg.password)?;
    let user_opt = state
        .store
        .insert_first_admin(username, &password_hash, now())
        .await?;

    let Some(user) = user_opt else {
        return Err(ApiError::forbidden(
            "registration is closed; an account already exists",
        ));
    };
    // Pre-auth projects and sessions (user_id NULL) belong to whoever signs
    // up first, so nothing created before accounts existed is lost to scoping.
    state.store.reassign_orphaned_projects(user.id).await?;
    state.store.reassign_orphaned_sessions(user.id).await?;
    state
        .store
        .reassign_orphaned_system_prompts(user.id)
        .await?;
    let token = crate::auth::issue_token(state, &user).await?;
    let cookie = crate::auth::set_cookie(&token, is_https);
    let mut resp = json_response(201, &json!({ "user": user.public() }));
    resp.headers_mut()
        .insert("set-cookie", cookie.parse().unwrap());
    Ok(resp)
}

#[derive(Deserialize)]
pub(super) struct LoginBody {
    username: String,
    password: String,
}

pub(crate) async fn login(req: Request, state: &AppState) -> Result<JsonResp, ApiError> {
    if !crate::auth::csrf_header_ok(req.headers()) {
        return Err(ApiError::unauthorized("not signed in"));
    }
    let is_https = crate::auth::is_https(req.headers());
    let body = read_body(req, AUTH_BODY_LIMIT).await?;
    let creds: LoginBody = parse_json(body)?;
    let username = creds.username.trim();

    if let Some((failures, last_failed)) = state.store.login_failures(username).await? {
        let lockout = openwebide_auth::lockout_secs(failures);
        if lockout > 0 {
            let elapsed = now() - last_failed;
            if elapsed < lockout {
                return Err(ApiError::too_many_requests(format!(
                    "too many failed login attempts; try again in {}s",
                    lockout - elapsed
                )));
            }
        }
    }

    let user_opt = state.store.get_user_by_username(username).await?;
    let hash = user_opt.as_ref().map(|u| u.password_hash.as_str());

    if !crate::auth::verify_password_or_dummy(&creds.password, hash) {
        state.store.record_login_failure(username, now()).await?;
        return Err(ApiError::unauthorized("invalid username or password"));
    }

    let user = user_opt.unwrap();
    state.store.clear_login_failures(username).await?;

    let token = crate::auth::issue_token(state, &user).await?;
    let cookie = crate::auth::set_cookie(&token, is_https);

    let mut resp = json_response(200, &json!({ "user": user.public() }));
    resp.headers_mut()
        .insert("set-cookie", cookie.parse().unwrap());
    Ok(resp)
}

/// The authenticated account (set by the router from the bearer token).
pub(crate) async fn me(state: &AppState, user: AuthedUser) -> Result<JsonResp, ApiError> {
    // Plugin list/binding requests initialize defaults independently. Account
    // access must not wait for a cold compiler/runtime or an unavailable host.
    let user = state.store.get_user(user.id).await?.map(|u| {
        let mut account = u.public();
        account.role = user.role;
        account
    });
    Ok(json_response(200, &json!({ "user": user })))
}

pub(crate) async fn logout(req: Request, state: &AppState) -> Result<JsonResp, ApiError> {
    if !crate::auth::csrf_header_ok(req.headers()) {
        return Err(ApiError::unauthorized("not signed in"));
    }
    if let Ok(user) = crate::auth::authenticate(state, req.headers()).await {
        if let Err(error) = state.store.bump_token_epoch(user.id).await {
            eprintln!("logout user {}: bump_token_epoch: {error}", user.id);
        }
        state.store.remove_user_push_subscriptions(user.id).await?;
    }
    let is_https = crate::auth::is_https(req.headers());
    let cookie = crate::auth::clear_cookie(is_https);
    let mut resp = json_response(200, &json!({ "ok": true }));
    resp.headers_mut()
        .insert("set-cookie", cookie.parse().unwrap());
    Ok(resp)
}

// -- health ------------------------------------------------------------------

pub(crate) fn health() -> JsonResp {
    json_response(
        200,
        &Health {
            status: "ok".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
    )
}
