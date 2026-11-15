//! Private messages for runtimes that already share an enduring credential.
//! Callers bind their complete protocol context and enforce plaintext limits.
use super::{URL_SAFE_NO_PAD, derive_secret};
use base64::Engine as _;
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use rand::Rng;
const INVALID: &str = "Private recovery message could not be authenticated";
fn cipher(secret: &[u8], domain: &str, context: &[u8]) -> Result<XChaCha20Poly1305, String> {
    let key = URL_SAFE_NO_PAD
        .decode(derive_secret(secret, domain, context))
        .map_err(|_| INVALID)?;
    XChaCha20Poly1305::new_from_slice(&key).map_err(|_| INVALID.into())
}
/// Encrypt and authenticate with a fresh random 192-bit nonce. Direction and
/// protocol/version belong in `domain`; immutable request identity in `context`.
///
/// # Errors
/// Returns a generic error if the message cannot be encrypted.
pub fn seal(
    secret: &[u8],
    domain: &str,
    context: &[u8],
    plaintext: &[u8],
) -> Result<(String, String), String> {
    let mut nonce = [0u8; 24];
    rand::rng().fill_bytes(&mut nonce);
    let ciphertext = cipher(secret, domain, context)?
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: context,
            },
        )
        .map_err(|_| INVALID)?;
    Ok((
        URL_SAFE_NO_PAD.encode(nonce),
        URL_SAFE_NO_PAD.encode(ciphertext),
    ))
}
/// Authenticate before returning any plaintext. Bound encoded input before
/// decoding, then check the exact plaintext limit independently.
///
/// # Errors
/// Returns a generic error for oversized, malformed or unauthenticated data.
pub fn open(
    secret: &[u8],
    domain: &str,
    context: &[u8],
    nonce: &str,
    ciphertext: &str,
    limit: usize,
) -> Result<Vec<u8>, String> {
    if nonce.len() != 32
        || ciphertext.len() > limit.saturating_add(16).div_ceil(3).saturating_mul(4)
    {
        return Err(INVALID.into());
    }
    let nonce: [u8; 24] = URL_SAFE_NO_PAD
        .decode(nonce)
        .map_err(|_| INVALID)?
        .try_into()
        .map_err(|_| INVALID)?;
    let ciphertext = URL_SAFE_NO_PAD.decode(ciphertext).map_err(|_| INVALID)?;
    let plaintext = cipher(secret, domain, context)?
        .decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: context,
            },
        )
        .map_err(|_| INVALID)?;
    if plaintext.len() > limit {
        return Err(INVALID.into());
    }
    Ok(plaintext)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_messages_bind_direction_scope_and_credential_before_returning_plaintext() {
        let secret = b"the enduring original recovery credential";
        let plain = b"private original authority and worker";
        let (nonce, body) = seal(secret, "request-v1", b"original scope", plain).unwrap();
        assert_eq!(
            open(
                secret,
                "request-v1",
                b"original scope",
                &nonce,
                &body,
                plain.len()
            )
            .unwrap(),
            plain
        );
        for (key, domain, context) in [
            (
                &b"replacement credential"[..],
                "request-v1",
                &b"original scope"[..],
            ),
            (secret.as_slice(), "response-v1", &b"original scope"[..]),
            (secret.as_slice(), "request-v1", &b"other scope"[..]),
        ] {
            assert!(open(key, domain, context, &nonce, &body, plain.len()).is_err());
        }
        assert!(
            open(
                secret,
                "request-v1",
                b"original scope",
                &nonce,
                &body,
                plain.len() - 1
            )
            .is_err()
        );
        for (n, b) in [
            ("invalid".to_string(), body.clone()),
            (nonce.clone(), format!("{body}A")),
            (URL_SAFE_NO_PAD.encode([0u8; 24]), body.clone()),
        ] {
            assert!(open(secret, "request-v1", b"original scope", &n, &b, plain.len()).is_err());
        }
        let next = seal(secret, "request-v1", b"original scope", plain).unwrap();
        assert_ne!(next.0, nonce);
        assert_ne!(next.1, body);
    }
}
