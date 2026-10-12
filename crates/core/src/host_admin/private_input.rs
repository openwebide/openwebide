//! Private terminal replies: ciphertext on every API boundary, scoped to one live prompt.
use serde::{Deserialize, Serialize};

pub const MAX_REPLY_BYTES: usize = 4096;
pub const MAX_REPLIES: usize = 128;
const VERSION: u8 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostInputKeyRequest {
    pub operation: i64,
    pub token: String,
}
impl HostInputKeyRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.operation <= 0 || self.token.is_empty() || self.token.len() > 256 {
            return Err("Invalid private terminal prompt.".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostInputKey {
    pub version: u8,
    pub user: i64,
    pub session: i64,
    pub prompt: HostInputKeyRequest,
    pub key_id: String,
    pub public_key: String,
}
impl HostInputKey {
    pub fn validate_scope(
        &self,
        user: i64,
        session: i64,
        prompt: &HostInputKeyRequest,
    ) -> Result<(), String> {
        prompt.validate()?;
        if self.version != VERSION
            || self.user != user
            || self.session != session
            || &self.prompt != prompt
            || self.key_id.is_empty()
            || self.key_id.len() > 128
            || self.public_key.len() > 128
        {
            return Err("Private terminal encryption key does not match this prompt.".into());
        }
        Ok(())
    }
    #[cfg(feature = "private-input")]
    fn aad(&self, request_id: &str) -> Result<Vec<u8>, String> {
        serde_json::to_vec(&(
            "openwebide-private-terminal-v1",
            self.user,
            self.session,
            &self.prompt,
            &self.key_id,
            request_id,
        ))
        .map_err(|_| "Private terminal encryption unavailable.".into())
    }
}
/// There is intentionally no plaintext field or plaintext fallback in the wire format.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostInput {
    pub version: u8,
    pub prompt: HostInputKeyRequest,
    pub key_id: String,
    pub request_id: String,
    pub encapsulated_key: String,
    pub ciphertext: String,
}
impl HostInput {
    pub fn validate(&self) -> Result<(), String> {
        self.prompt.validate()?;
        if self.version != VERSION
            || self.key_id.is_empty()
            || self.key_id.len() > 128
            || self.request_id.len() != 32
            || !self.request_id.bytes().all(|b| b.is_ascii_hexdigit())
            || self.encapsulated_key.len() > 128
            || self.ciphertext.len() > 5500
        {
            return Err("Invalid encrypted terminal reply.".into());
        }
        Ok(())
    }
}

#[cfg(feature = "private-input")]
mod crypto {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use hpke::{
        Deserializable, Kem, OpModeR, OpModeS, Serializable,
        aead::AesGcm256,
        kdf::HkdfSha256,
        kem::X25519HkdfSha256,
        rand_core::{RngCore, SeedableRng},
    };
    use rand_chacha::ChaCha20Rng;
    use std::collections::HashSet;
    type Suite = X25519HkdfSha256;
    const INFO: &[u8] = b"Open WebIDE private terminal reply v1";
    const INVALID: &str = "Private terminal reply could not be authenticated.";

    /// Private key and replay history live only at the terminal endpoint.
    pub struct PrivateInputRecipient {
        secret: <Suite as Kem>::PrivateKey,
        key: HostInputKey,
        seen: HashSet<String>,
    }
    impl PrivateInputRecipient {
        /// Entropy must come from the host CSPRNG. Never persist or transmit this recipient.
        pub fn new(
            user: i64,
            session: i64,
            prompt: HostInputKeyRequest,
            entropy: [u8; 32],
        ) -> Self {
            let (secret, public) = Suite::derive_keypair(&entropy);
            let public_key = STANDARD.encode(public.to_bytes());
            Self {
                secret,
                key: HostInputKey {
                    version: VERSION,
                    user,
                    session,
                    prompt,
                    key_id: public_key.clone(),
                    public_key,
                },
                seen: HashSet::new(),
            }
        }
        pub fn public_key(&self) -> HostInputKey {
            self.key.clone()
        }
        pub fn open(
            &mut self,
            user: i64,
            session: i64,
            input: &HostInput,
        ) -> Result<String, String> {
            input.validate()?;
            self.key.validate_scope(user, session, &input.prompt)?;
            if input.key_id != self.key.key_id
                || self.seen.contains(&input.request_id)
                || self.seen.len() >= MAX_REPLIES
            {
                return Err(INVALID.into());
            }
            let encapsulated = STANDARD
                .decode(&input.encapsulated_key)
                .map_err(|_| INVALID)?;
            let encapsulated =
                <Suite as Kem>::EncappedKey::from_bytes(&encapsulated).map_err(|_| INVALID)?;
            let ciphertext = STANDARD.decode(&input.ciphertext).map_err(|_| INVALID)?;
            let plaintext = hpke::single_shot_open::<AesGcm256, HkdfSha256, Suite>(
                &OpModeR::Base,
                &self.secret,
                &encapsulated,
                INFO,
                &ciphertext,
                &self.key.aad(&input.request_id)?,
            )
            .map_err(|_| INVALID)?;
            if plaintext.len() > MAX_REPLY_BYTES {
                return Err(INVALID.into());
            }
            let reply = String::from_utf8(plaintext).map_err(|_| INVALID)?;
            self.seen.insert(input.request_id.clone());
            Ok(reply)
        }
    }
    /// Browser supplies Web Crypto entropy; native callers supply host CSPRNG entropy.
    pub fn encrypt_reply(
        key: &HostInputKey,
        user: i64,
        session: i64,
        prompt: &HostInputKeyRequest,
        plaintext: &str,
        entropy: [u8; 32],
    ) -> Result<HostInput, String> {
        key.validate_scope(user, session, prompt)?;
        if plaintext.len() > MAX_REPLY_BYTES {
            return Err("Private terminal reply exceeds 4096 bytes.".into());
        }
        let public = STANDARD.decode(&key.public_key).map_err(|_| INVALID)?;
        let public = <Suite as Kem>::PublicKey::from_bytes(&public).map_err(|_| INVALID)?;
        let mut rng = ChaCha20Rng::from_seed(entropy);
        let mut id = [0_u8; 16];
        rng.fill_bytes(&mut id);
        let request_id: String = id.iter().map(|byte| format!("{byte:02x}")).collect();
        let (encapsulated, ciphertext) = hpke::single_shot_seal::<AesGcm256, HkdfSha256, Suite, _>(
            &OpModeS::Base,
            &public,
            INFO,
            plaintext.as_bytes(),
            &key.aad(&request_id)?,
            &mut rng,
        )
        .map_err(|_| "Private terminal encryption failed.")?;
        Ok(HostInput {
            version: VERSION,
            prompt: prompt.clone(),
            key_id: key.key_id.clone(),
            request_id,
            encapsulated_key: STANDARD.encode(encapsulated.to_bytes()),
            ciphertext: STANDARD.encode(ciphertext),
        })
    }
}
#[cfg(feature = "private-input")]
pub use crypto::{PrivateInputRecipient, encrypt_reply};

#[cfg(all(feature = "private-input", any(test, feature = "test-support")))]
pub mod testing {
    use super::*;
    pub fn assert_private_reply_contract() {
        let prompt = HostInputKeyRequest {
            operation: 9,
            token: "prompt".into(),
        };
        let mut recipient = PrivateInputRecipient::new(7, 8, prompt.clone(), [1; 32]);
        let key = recipient.public_key();
        let encrypted = encrypt_reply(&key, 7, 8, &prompt, "private-password\n", [2; 32]).unwrap();
        assert!(
            !serde_json::to_string(&encrypted)
                .unwrap()
                .contains("private-password")
        );
        for change in 0..8 {
            let mut changed = encrypted.clone();
            match change {
                0 => changed.prompt.operation += 1,
                1 => changed.prompt.token.push('x'),
                2 => changed.key_id.push('x'),
                3 => {
                    let replacement = if changed.request_id.starts_with('f') {
                        "e"
                    } else {
                        "f"
                    };
                    changed.request_id.replace_range(0..1, replacement);
                }
                4 => {
                    let replacement = if changed.ciphertext.starts_with('A') {
                        "B"
                    } else {
                        "A"
                    };
                    changed.ciphertext.replace_range(0..1, replacement);
                }
                5 => {
                    let replacement = if changed.encapsulated_key.starts_with('A') {
                        "B"
                    } else {
                        "A"
                    };
                    changed.encapsulated_key.replace_range(0..1, replacement);
                }
                6 => changed.version += 1,
                _ => changed.ciphertext = "A".repeat(5501),
            }
            assert!(recipient.open(7, 8, &changed).is_err(), "change {change}");
        }
        assert!(recipient.open(6, 8, &encrypted).is_err());
        assert!(recipient.open(7, 6, &encrypted).is_err());
        let mut replacement = PrivateInputRecipient::new(7, 8, prompt.clone(), [3; 32]);
        assert!(replacement.open(7, 8, &encrypted).is_err());
        assert_eq!(
            recipient.open(7, 8, &encrypted).unwrap(),
            "private-password\n"
        );
        assert!(recipient.open(7, 8, &encrypted).is_err());
        assert!(
            encrypt_reply(
                &key,
                7,
                8,
                &prompt,
                &"x".repeat(MAX_REPLY_BYTES + 1),
                [4; 32]
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<HostInput>(
                r#"{"operation":9,"token":"prompt","data":"plaintext"}"#
            )
            .is_err()
        );
    }
}

#[cfg(all(test, feature = "private-input"))]
mod tests {
    #[test]
    fn private_reply_scope_tampering_replay_rotation_and_limits() {
        super::testing::assert_private_reply_contract();
    }
}
