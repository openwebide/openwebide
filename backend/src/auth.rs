//! Local account authentication: password hashing and signed bearer tokens.
//!
//! The pure crypto (argon2id hashing, HMAC token sign/verify) lives in the
//! `openwebide-auth` crate so it is unit-testable natively; this module wires
//! it to the app: the signing secret is a random 32-byte value persisted in
//! the `settings` table, so tokens stay valid across requests (Spin
//! components are otherwise stateless).

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use openwebide_core::{User, UserId, UserRole};

#[derive(Debug, Clone, Copy)]
pub struct AuthedUser {
    pub id: UserId,
    pub role: UserRole,
}

impl From<User> for AuthedUser {
    fn from(user: User) -> Self {
        Self {
            id: user.id,
            role: user.role,
        }
    }
}
use rand::Rng;
use spin_sdk::http::HeaderMap;

use crate::error::ApiError;
use crate::state::{AppState, unix_now_checked};

/// Token lifetime: 30 days.
const TOKEN_TTL_SECS: i64 = 30 * 24 * 60 * 60;
/// The `settings` key holding the token-signing secret.
const SECRET_KEY: &str = "auth_secret";

/// Hash a password with argon2id.
pub fn hash_password(password: &str) -> Result<String, ApiError> {
    openwebide_auth::hash_password(password).map_err(|e| ApiError::internal(e.to_string()))
}

pub fn verify_password_or_dummy(password: &str, hash: Option<&str>) -> bool {
    openwebide_auth::verify_password_or_dummy(password, hash)
}

pub fn session_cookie(headers: &HeaderMap) -> Option<String> {
    for cookie in headers
        .get_all("cookie")
        .iter()
        .filter_map(|h| h.to_str().ok())
    {
        for part in cookie.split(';') {
            let part = part.trim();
            if let Some(token) = part.strip_prefix("owide_session=") {
                return Some(token.to_string());
            }
        }
    }
    None
}

pub fn csrf_header_ok(headers: &HeaderMap) -> bool {
    headers.get("x-openwebide").and_then(|h| h.to_str().ok()) == Some("1")
}

pub fn is_https(headers: &HeaderMap) -> bool {
    let url = headers
        .get("spin-full-url")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    url.starts_with("https://") || proto == "https"
}

pub fn set_cookie(token: &str, secure: bool) -> String {
    let mut c =
        format!("owide_session={token}; Path=/api; HttpOnly; SameSite=Strict; Max-Age=2592000");
    if secure {
        c.push_str("; Secure");
    }
    c
}

pub fn clear_cookie(secure: bool) -> String {
    let mut c = "owide_session=; Path=/api; HttpOnly; SameSite=Strict; Max-Age=0".to_string();
    if secure {
        c.push_str("; Secure");
    }
    c
}

/// Read the token-signing secret, creating and persisting it on first use.
async fn get_or_create_secret(state: &AppState) -> Result<String, ApiError> {
    if let Some(secret) = state.store.get_setting(SECRET_KEY).await? {
        return Ok(secret);
    }
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    state
        .store
        .insert_setting_if_absent(SECRET_KEY, &URL_SAFE_NO_PAD.encode(bytes))
        .await?;
    state
        .store
        .get_setting(SECRET_KEY)
        .await?
        .ok_or_else(|| ApiError::internal("auth secret missing after insert"))
}

/// Sign a bearer token for `user_id`, valid until `now + TOKEN_TTL_SECS`.
fn sign_token(
    secret: &str,
    user_id: UserId,
    now: Option<i64>,
    epoch: i64,
) -> Result<String, ApiError> {
    let now = now.ok_or_else(|| ApiError::internal("system clock unavailable"))?;
    Ok(openwebide_auth::sign_token_expires(
        secret,
        user_id.get(),
        now + TOKEN_TTL_SECS,
        epoch,
    ))
}

/// Verify a bearer token at `now` (unix seconds), returning the user id if
/// valid and unexpired. A `None` clock fails closed.
fn verify_token(
    secret: &str,
    token: &str,
    now: Option<i64>,
) -> Result<Option<openwebide_auth::TokenClaims>, ApiError> {
    let now = now.ok_or_else(|| ApiError::internal("system clock unavailable"))?;
    Ok(openwebide_auth::verify_token_at(secret, token, now))
}

/// Issue a signed bearer token.
pub async fn issue_token(
    state: &AppState,
    user: &openwebide_storage::store::UserRecord,
) -> Result<String, ApiError> {
    let secret = get_or_create_secret(state).await?;
    sign_token(&secret, user.id, unix_now_checked(), user.token_epoch)
}

fn bridge_principal(headers: &HeaderMap, secret: Option<&str>) -> Result<Option<UserId>, ApiError> {
    reject_plugin_transport(headers)?;
    let Some(auth) = headers.get("authorization") else {
        return Ok(None);
    };
    let token = auth.to_str().ok().and_then(|a| a.strip_prefix("Bearer "));
    if !matches!((token, secret), (Some(token), Some(secret)) if constant_time_eq(token.as_bytes(), secret.as_bytes()))
    {
        return Err(ApiError::unauthorized("invalid bridge secret"));
    }
    let user_id = headers
        .get("x-openwebide-user")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<i64>().ok())
        .map(UserId::new)
        .ok_or_else(|| ApiError::unauthorized("missing acting user"))?;
    Ok(Some(user_id))
}

/// Authenticate a request from its headers, returning the account.
pub async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<User, ApiError> {
    reject_plugin_transport(headers)?;
    if headers.contains_key("authorization") {
        let secret = crate::bridge::bridge_secret(&state.store).await?;
        if let Some(id) = bridge_principal(headers, secret.as_deref())? {
            return state
                .store
                .get_user(id)
                .await?
                .map(|user| user.public())
                .ok_or_else(|| ApiError::unauthorized("unknown user"));
        }
    }
    let token = session_cookie(headers).ok_or_else(|| ApiError::unauthorized("not signed in"))?;
    if !csrf_header_ok(headers) {
        return Err(ApiError::unauthorized("not signed in"));
    }

    let secret = get_or_create_secret(state).await?;
    session_user(state, &secret, &token)
        .await?
        .ok_or_else(|| ApiError::unauthorized("invalid or expired token"))
}

/// Validate only the session cookie for the read-only prepaint theme endpoint.
pub(crate) async fn theme_user(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<User>, ApiError> {
    let Some(token) = session_cookie(headers) else {
        return Ok(None);
    };
    let Some(secret) = state.store.get_setting(SECRET_KEY).await? else {
        return Ok(None);
    };
    session_user(state, &secret, &token).await
}

async fn session_user(
    state: &AppState,
    secret: &str,
    token: &str,
) -> Result<Option<User>, ApiError> {
    let Some(claims) = verify_token(secret, token, unix_now_checked())? else {
        return Ok(None);
    };
    let Some(user) = state.store.get_user(UserId::new(claims.user_id)).await? else {
        return Ok(None);
    };
    Ok((user.token_epoch == claims.epoch).then(|| user.public()))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn reject_plugin_transport(headers: &HeaderMap) -> Result<(), ApiError> {
    if headers.contains_key(openwebide_core::plugins::execution::PLUGIN_HTTP_HEADER) {
        return Err(ApiError::unauthorized(
            "Plugin HTTP cannot authenticate to host control endpoints",
        ));
    }
    Ok(())
}

/// Internal delivery dispatcher: authenticates the service without assuming a user identity.
pub async fn require_bridge_service(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    reject_plugin_transport(headers)?;
    let secret = crate::bridge::bridge_secret(&state.store).await?;
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if !matches!((token, secret.as_deref()), (Some(token), Some(secret)) if constant_time_eq(token.as_bytes(), secret.as_bytes()))
    {
        return Err(ApiError::unauthorized("invalid bridge secret"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use spin_sdk::http::HeaderMap;

    #[test]
    fn plugin_http_cannot_authenticate_as_a_bridge_service_or_account() {
        futures::executor::block_on(async {
            let state = AppState::new().await.unwrap();
            state
                .store
                .set_setting("bridge_secret_cache", "secret")
                .await
                .unwrap();
            let user = state
                .store
                .insert_user(
                    "plugin-boundary-owner",
                    "hash",
                    openwebide_core::UserRole::Admin,
                    1,
                )
                .await
                .unwrap();
            let cookie = issue_token(&state, &user).await.unwrap();
            for marker in ["1", "", "0"] {
                let mut headers = HeaderMap::new();
                headers.insert(
                    openwebide_core::plugins::execution::PLUGIN_HTTP_HEADER,
                    marker.parse().unwrap(),
                );
                headers.insert("authorization", "Bearer secret".parse().unwrap());
                headers.insert("x-openwebide-user", user.id.to_string().parse().unwrap());
                assert_eq!(
                    require_bridge_service(&state, &headers)
                        .await
                        .unwrap_err()
                        .into_response()
                        .status()
                        .as_u16(),
                    401
                );
                assert!(bridge_principal(&headers, Some("secret")).is_err());
                assert_eq!(
                    authenticate(&state, &headers)
                        .await
                        .unwrap_err()
                        .into_response()
                        .status()
                        .as_u16(),
                    401
                );
                headers.remove("authorization");
                headers.insert("cookie", format!("owide_session={cookie}").parse().unwrap());
                headers.insert("x-openwebide", "1".parse().unwrap());
                assert_eq!(
                    authenticate(&state, &headers)
                        .await
                        .unwrap_err()
                        .into_response()
                        .status()
                        .as_u16(),
                    401
                );
            }
        });
    }
    #[test]
    fn shared_secret_comparison() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"Secret"));
        assert!(!constant_time_eq(b"secret", b"secret-more"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn bridge_authentication_table_and_cookie_path() {
        futures::executor::block_on(async {
            let state = AppState::new().await.unwrap();
            let user = state
                .store
                .insert_user("alice", "hash", openwebide_core::UserRole::Admin, 1)
                .await
                .unwrap();
            state
                .store
                .set_setting("bridge_secret_cache", "secret")
                .await
                .unwrap();
            for (token, acting_user, expected) in [
                ("secret", Some(user.id.to_string()), true),
                ("wrong", Some(user.id.to_string()), false),
                ("secret", None, false),
                ("secret", Some("999".into()), false),
                ("secret", Some("bad".into()), false),
            ] {
                let mut headers = HeaderMap::new();
                headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
                if let Some(id) = acting_user {
                    headers.insert("x-openwebide-user", id.parse().unwrap());
                }
                let result = authenticate(&state, &headers).await;
                if expected {
                    assert_eq!(result.unwrap().id, user.id);
                } else {
                    assert_eq!(result.unwrap_err().into_response().status().as_u16(), 401);
                }
            }
            let mut headers = HeaderMap::new();
            assert_eq!(bridge_principal(&headers, Some("secret")).unwrap(), None);
            let token = issue_token(&state, &user).await.unwrap();
            headers.insert("cookie", format!("owide_session={token}").parse().unwrap());
            assert!(authenticate(&state, &headers).await.is_err());
            headers.insert("x-openwebide", "1".parse().unwrap());
            assert_eq!(authenticate(&state, &headers).await.unwrap().id, user.id);
        });
    }

    #[test]
    fn concurrent_secret_initialization_reuses_one_database_value() {
        futures::executor::block_on(async {
            let state = AppState::new().await.unwrap();
            let barrier = std::sync::Barrier::new(8);
            let secrets = std::thread::scope(|scope| {
                let tasks = (0..8)
                    .map(|_| {
                        scope.spawn(|| {
                            barrier.wait();
                            futures::executor::block_on(get_or_create_secret(&state)).unwrap()
                        })
                    })
                    .collect::<Vec<_>>();
                tasks
                    .into_iter()
                    .map(|task| task.join().unwrap())
                    .collect::<Vec<_>>()
            });
            assert!(secrets.iter().all(|secret| secret == &secrets[0]));
            assert_eq!(
                state.store.get_setting(SECRET_KEY).await.unwrap(),
                Some(secrets[0].clone())
            );
        });
    }

    #[test]
    fn verify_token_rejects_expired_token() {
        let secret = "test-secret";
        let valid = openwebide_auth::sign_token_expires(secret, 7, 3_000, 2);
        assert!(verify_token(secret, &valid, Some(2_000)).unwrap().is_some());
        let expired = openwebide_auth::sign_token_expires(secret, 7, 1_000, 2);
        assert!(matches!(
            verify_token(secret, &expired, Some(2_000)),
            Ok(None)
        ));

        let err = verify_token(secret, &expired, None).unwrap_err();
        assert_eq!(err.into_response().status().as_u16(), 500);
    }

    #[test]
    fn test_session_cookie() {
        let mut h = HeaderMap::new();
        h.append("cookie", "a=1; owide_session=x; b=2".parse().unwrap());
        assert_eq!(session_cookie(&h).as_deref(), Some("x"));
    }

    #[test]
    fn test_csrf_header_ok() {
        let mut h = HeaderMap::new();
        assert!(!csrf_header_ok(&h));
        h.insert("x-openwebide", "1".parse().unwrap());
        assert!(csrf_header_ok(&h));
    }

    #[test]
    fn test_cookie_flags() {
        assert_eq!(
            set_cookie("t", false),
            "owide_session=t; Path=/api; HttpOnly; SameSite=Strict; Max-Age=2592000"
        );
        assert_eq!(
            set_cookie("t", true),
            "owide_session=t; Path=/api; HttpOnly; SameSite=Strict; Max-Age=2592000; Secure"
        );
    }
}
