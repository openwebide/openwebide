//! Password hashing (argon2id) and signed bearer tokens.
//!
//! A token is `base64url("{user_id}.{expires_at}.{epoch}")` + `"."` +
//! `base64url(hmac_sha256(secret, payload))`. These are pure functions with no
//! storage or clock dependency, so the crypto is unit-testable natively — the
//! backend is a wasm cdylib whose own tests never run in CI.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use rand::Rng;
use sha2::Sha256;
pub mod private_messages;

type HmacSha256 = Hmac<Sha256>;

/// Error from password hashing.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("argon2 params: {0}")]
    Argon2Params(String),
    #[error("hash password: {0}")]
    HashPassword(String),
}

/// A dummy PHC hash for timing mitigation against unknown usernames.
/// Generated with `hash_password("dummy")` and today's parameters.
pub const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$xD6DXNOs9E1M48yIXKg2kA$x4Fk1NzUT6CR+hHX6OW+KEPpDKgduAl5IicLVX/Au4I";

/// Hash a password with argon2id.
///
/// # Errors
/// Returns an error if the hashing parameters or password hashing fail.
#[must_use = "password hashing errors must be handled"]
pub fn hash_password(password: &str) -> Result<String, AuthError> {
    use argon2::{Algorithm, Argon2, Params, PasswordHasher, Version};
    // 16 random bytes as the salt; `hash_password_with_salt` base64-encodes them
    // into the PHC salt field.
    let mut salt_bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut salt_bytes);
    let params =
        Params::new(19_456, 2, 1, None).map_err(|e| AuthError::Argon2Params(e.to_string()))?;
    let hash = Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_with_salt(password.as_bytes(), &salt_bytes)
        .map_err(|e| AuthError::HashPassword(e.to_string()))?;
    Ok(hash.to_string())
}

/// Verify a password against a stored argon2id hash.
#[must_use]
pub fn verify_password(password: &str, hash: &str) -> bool {
    use argon2::{Argon2, PasswordVerifier, password_hash::phc::PasswordHash};
    PasswordHash::new(hash)
        .ok()
        .and_then(|ph| {
            Argon2::default()
                .verify_password(password.as_bytes(), &ph)
                .ok()
        })
        .is_some()
}

/// Run exactly one argon2 verify. Returns false if `hash` is None, but runs a verify against `DUMMY_HASH` to prevent timing attacks.
#[must_use]
pub fn verify_password_or_dummy(password: &str, hash: Option<&str>) -> bool {
    let actual_hash = hash.unwrap_or(DUMMY_HASH);
    let ok = verify_password(password, actual_hash);
    hash.is_some() && ok
}

/// Calculate lockout seconds based on consecutive failures.
#[must_use]
pub fn lockout_secs(failures: i64) -> i64 {
    if failures < 5 {
        0
    } else {
        std::cmp::min(30 << (failures - 5), 900)
    }
}

#[derive(Debug, PartialEq)]
pub struct TokenClaims {
    pub user_id: i64,
    pub epoch: i64,
}

/// Sign a bearer token for `user_id` and `epoch`, valid until `expires_at` (unix seconds).
#[must_use]
pub fn sign_token_expires(secret: &str, user_id: i64, expires_at: i64, epoch: i64) -> String {
    let payload = format!("{user_id}.{expires_at}.{epoch}");
    let signature = hmac(secret, payload.as_bytes());
    format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(payload.as_bytes()),
        URL_SAFE_NO_PAD.encode(signature)
    )
}

/// Verify a bearer token at time `now` (unix seconds), returning the claims
/// if the signature is valid and the token is unexpired.
#[must_use]
pub fn verify_token_at(secret: &str, token: &str, now: i64) -> Option<TokenClaims> {
    let (payload_b64, sig_b64) = token.split_once('.')?;
    let payload_bytes = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let payload = std::str::from_utf8(&payload_bytes).ok()?;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(payload.as_bytes());
    let actual = URL_SAFE_NO_PAD.decode(sig_b64).ok()?;
    if mac.verify_slice(&actual).is_err() {
        return None;
    }
    let parts: Vec<&str> = payload.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let user_id: i64 = parts[0].parse().ok()?;
    let expires_at: i64 = parts[1].parse().ok()?;
    let epoch: i64 = parts[2].parse().ok()?;

    (expires_at >= now).then_some(TokenClaims { user_id, epoch })
}

fn hmac(secret: &str, payload: &[u8]) -> Vec<u8> {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(payload);
    mac.finalize().into_bytes().to_vec()
}

/// Derive a deterministic private credential in a separate protocol namespace.
///
/// Domain and payload lengths use fixed-width framing on every target. The
/// caller owns credential lifetime, canonical claims and authorization policy.
///
/// # Panics
/// Panics if a domain or payload length cannot be represented by a `u64`.
#[must_use]
pub fn derive_secret(secret: &[u8], domain: &str, payload: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(b"openwebide-derived-secret-v1\0");
    mac.update(
        &u64::try_from(domain.len())
            .expect("domain length fits u64")
            .to_be_bytes(),
    );
    mac.update(domain.as_bytes());
    mac.update(
        &u64::try_from(payload.len())
            .expect("payload length fits u64")
            .to_be_bytes(),
    );
    mac.update(payload);
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;

    #[test]
    fn derived_secret_matches_the_portable_known_vector() {
        let key: [u8; 32] = std::array::from_fn(|index| u8::try_from(index).unwrap());
        assert_eq!(
            derive_secret(
                &key,
                "workspace-recovery-v1",
                br#"[7,11,"transaction","generation","root"]"#,
            ),
            "utkrQJQnJWToQQdHSO_O0f6vexnEeD_G6wO4-UedNo8"
        );
    }

    #[test]
    fn derived_secrets_separate_keys_domains_and_claims_without_boundary_ambiguity() {
        let original = derive_secret(b"secret", "workspace-recovery-v1", b"original");
        assert_ne!(
            original,
            derive_secret(b"other-secret", "workspace-recovery-v1", b"original")
        );
        assert_ne!(
            original,
            derive_secret(b"secret", "other-protocol", b"original")
        );
        assert_ne!(
            original,
            derive_secret(b"secret", "workspace-recovery-v1", b"replacement")
        );
        assert_ne!(
            derive_secret(b"secret", "ab", b"c"),
            derive_secret(b"secret", "a", b"bc")
        );
    }

    #[test]
    fn a_derived_credential_cannot_supply_a_bearer_token_signature() {
        let valid = sign_token_expires("secret", 7, NOW + 100, 2);
        assert!(verify_token_at("secret", &valid, NOW).is_some());
        let payload = format!("7.{}.2", NOW + 100);
        let forged = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(payload.as_bytes()),
            derive_secret(b"secret", "workspace-recovery-v1", payload.as_bytes())
        );
        assert!(verify_token_at("secret", &forged, NOW).is_none());
    }

    #[test]
    fn malformed_tokens_are_rejected() {
        for token in ["", "no-dot", ".", "***.***", "a.b.c", "eA.", "_w.eA"] {
            assert!(verify_token_at("secret", token, NOW).is_none(), "{token}");
        }
        for payload in [
            "bad.1000100.2",
            "7.bad.2",
            "7.1000100.bad",
            "7.1000100.2.extra",
        ] {
            let token = format!(
                "{}.{}",
                URL_SAFE_NO_PAD.encode(payload),
                URL_SAFE_NO_PAD.encode(hmac("secret", payload.as_bytes()))
            );
            assert!(verify_token_at("secret", &token, NOW).is_none());
        }
    }

    #[test]
    fn token_roundtrip() {
        let secret = "test-secret";
        let token = sign_token_expires(secret, 7, NOW + 100, 2);
        let claims = verify_token_at(secret, &token, NOW).unwrap();
        assert_eq!(claims.user_id, 7);
        assert_eq!(claims.epoch, 2);
    }

    #[test]
    fn token_rejects_wrong_secret() {
        let token = sign_token_expires("secret-a", 7, NOW + 100, 2);
        assert!(verify_token_at("secret-b", &token, NOW).is_none());
    }

    #[test]
    fn token_rejects_expired() {
        let secret = "test-secret";
        let token = sign_token_expires(secret, 7, NOW - 1, 2);
        assert!(verify_token_at(secret, &token, NOW).is_none());
    }

    #[test]
    fn token_rejects_tampered_payload() {
        let secret = "test-secret";
        let token = sign_token_expires(secret, 7, NOW + 100, 2);
        let (payload, sig) = token.split_once('.').unwrap();
        // Change the user id in the payload; the signature no longer matches.
        let mut forged = URL_SAFE_NO_PAD.decode(payload).unwrap();
        forged[0] = b'8';
        let forged = format!("{}.{}", URL_SAFE_NO_PAD.encode(forged), sig);
        assert!(verify_token_at(secret, &forged, NOW).is_none());
    }

    #[test]
    fn token_rejects_legacy_two_part_token() {
        let secret = "test-secret";
        let payload = format!("{}.{}", 7, NOW + 100);
        let signature = hmac(secret, payload.as_bytes());
        let legacy_token = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(payload.as_bytes()),
            URL_SAFE_NO_PAD.encode(signature)
        );
        assert!(verify_token_at(secret, &legacy_token, NOW).is_none());
    }

    #[test]
    fn password_hash_roundtrip() {
        let hash = hash_password("hunter2").unwrap();
        assert!(verify_password("hunter2", &hash));
        assert!(!verify_password("wrong", &hash));
    }

    #[test]
    fn dummy_hash_valid() {
        assert!(verify_password("dummy", DUMMY_HASH));
        assert!(!verify_password_or_dummy("wrong", None));
    }

    #[test]
    fn lockout_secs_calculation() {
        assert_eq!(lockout_secs(4), 0);
        assert_eq!(lockout_secs(5), 30);
        assert_eq!(lockout_secs(6), 60);
        assert_eq!(lockout_secs(20), 900);
    }

    /// A PHC string produced by argon2 0.5 / password-hash 0.5 (recorded before the
    /// 0.6 bump) must still verify.
    #[test]
    fn argon2_0_5_hash_still_verifies() {
        let hash = "$argon2id$v=19$m=19456,t=2,p=1$cyxg2ERXZqGjDGOUqMYIVw$zCWESEqdxK7yti9KLn9p6x3vbNyHS40MdPgoX9iLtvA";
        assert!(verify_password("correct horse", hash));
        assert!(!verify_password("wrong", hash));
    }
}
