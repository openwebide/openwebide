//! Native original-object identity, shared by resource recovery and folder admission.
use serde::{Deserialize, Serialize};
use std::{fs::File, io, time::SystemTime};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Identity {
    device: u64,
    inode: u64,
    created: Option<SystemTime>,
}
impl Identity {
    pub fn of(file: &File) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            created: metadata.created().ok(),
        })
    }
    pub fn fingerprint(&self) -> String {
        let mut bytes = Vec::with_capacity(40);
        bytes.extend_from_slice(&self.device.to_le_bytes());
        bytes.extend_from_slice(&self.inode.to_le_bytes());
        match self.created {
            None => bytes.push(0),
            Some(time) => {
                let (sign, duration) = match time.duration_since(std::time::UNIX_EPOCH) {
                    Ok(duration) => (1, duration),
                    Err(error) => (2, error.duration()),
                };
                bytes.push(sign);
                bytes.extend_from_slice(&duration.as_secs().to_le_bytes());
                bytes.extend_from_slice(&duration.subsec_nanos().to_le_bytes());
            }
        }
        let version = if self.has_birth_time() { "u1" } else { "u0" };
        format!(
            "{version}.{}",
            openwebide_core::plugins::content_digest(&bytes)
        )
    }
    /// Conservative alias exclusion, including an unknown or reused birth ID.
    /// Recovering an owner still requires equality of the complete identity.
    pub fn may_alias(&self, other: &Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
    pub fn same_device(&self, other: &Self) -> bool {
        self.device == other.device
    }
    pub fn has_birth_time(&self) -> bool {
        self.created.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn birthless_identity_is_distinct_and_never_claims_persistent_original_capability() {
        let mut object = Identity {
            device: 17,
            inode: 42,
            created: None,
        };
        let birthless = object.fingerprint();
        assert!(birthless.starts_with("u0."));
        object.created = Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1));
        let persistent = object.fingerprint();
        assert!(persistent.starts_with("u1."));
        assert_ne!(birthless, persistent);
    }

    #[test]
    fn reused_device_and_inode_do_not_reuse_an_original_object() {
        let original = Identity {
            device: 17,
            inode: 42,
            created: Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1)),
        };
        let replacement = Identity {
            created: Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(2)),
            ..original.clone()
        };
        assert_ne!(original, replacement);
        assert_ne!(original.fingerprint(), replacement.fingerprint());
        assert!(original.same_device(&replacement));
    }
}
