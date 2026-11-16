//! Native original-object identity, shared by resource recovery and folder admission.
use serde::{Deserialize, Serialize};
use std::{fs::File, io, time::SystemTime};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Identity {
    device: u64,
    inode: u64,
    created: Option<SystemTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    volume: Option<[u8; 16]>,
}
#[cfg(target_os = "macos")]
mod volume;
impl Identity {
    pub fn of(file: &File) -> io::Result<Self> {
        #[cfg(target_os = "macos")]
        return Self::observations(file).map(|(current, _)| current);
        #[cfg(not(target_os = "macos"))]
        Self::legacy(file)
    }
    pub fn legacy(file: &File) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            created: metadata.created().ok(),
            volume: None,
        })
    }
    #[cfg(target_os = "macos")]
    pub fn observations(file: &File) -> io::Result<(Self, Self)> {
        let legacy = Self::legacy(file)?;
        let mut current = legacy.clone();
        if let Ok(volume) = volume::uuid(file) {
            current.device = 0;
            current.volume = Some(volume);
        }
        Ok((current, legacy))
    }
    pub fn uses_device_number(&self) -> bool {
        self.volume.is_none()
    }
    pub fn matches(&self, file: &File) -> io::Result<bool> {
        let observed = if self.uses_device_number() {
            Self::legacy(file)?
        } else {
            Self::of(file)?
        };
        Ok(*self == observed)
    }
    /// Observe a possible legacy upgrade candidate, without granting recovery.
    /// Only the explicit empty-registry inspector may use this weaker relation.
    #[cfg(target_os = "macos")]
    pub(crate) fn legacy_upgrade(&self, file: &File) -> io::Result<Self> {
        let actual = Self::legacy(file)?;
        let current = Self::of(file)?;
        if !self.uses_device_number()
            || !self.valid()
            || !self.has_birth_time()
            || self.inode != actual.inode
            || self.created != actual.created
            || !current.has_persistent_volume()
        {
            return Err(io::Error::other(
                "The original legacy object cannot be inspected for upgrade",
            ));
        }
        Ok(current)
    }
    pub fn valid(&self) -> bool {
        self.volume
            .is_none_or(|volume| self.device == 0 && volume != [0; 16])
    }
    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            clippy::unused_self,
            reason = "Other supported hosts use the device identity without a volume UUID"
        )
    )]
    pub fn has_persistent_volume(&self) -> bool {
        #[cfg(target_os = "macos")]
        return self.volume.is_some();
        #[cfg(not(target_os = "macos"))]
        true
    }
    pub fn fingerprint(&self) -> String {
        let mut bytes = Vec::with_capacity(40);
        if let Some(volume) = self.volume {
            bytes.extend_from_slice(b"volume-identity:1\0");
            bytes.extend_from_slice(&volume);
        } else {
            bytes.extend_from_slice(&self.device.to_le_bytes());
        }
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
        self.inode == other.inode
            && (self.same_device(other)
                || self.volume.is_some() != other.volume.is_some()
                // macOS can reallocate device numbers after remount. Two
                // legacy observations cannot disprove an alias by that number.
                // This only excludes admission; original recovery still needs
                // equality of the complete persisted identity.
                || (cfg!(target_os = "macos") && self.volume.is_none() && other.volume.is_none()))
    }
    pub fn same_device(&self, other: &Self) -> bool {
        self.volume == other.volume && self.device == other.device
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
            volume: None,
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
            volume: None,
        };
        let replacement = Identity {
            created: Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(2)),
            ..original.clone()
        };
        assert_ne!(original, replacement);
        assert_ne!(original.fingerprint(), replacement.fingerprint());
        assert!(original.same_device(&replacement));
    }

    #[test]
    fn legacy_wire_identity_keeps_its_exact_fingerprint_and_descriptor_matching() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("original");
        std::fs::write(&path, b"original").unwrap();
        let file = File::open(&path).unwrap();
        let legacy = Identity::legacy(&file).unwrap();
        let encoded = serde_json::to_value(&legacy).unwrap();
        assert!(encoded.get("volume").is_none());
        let restored: Identity = serde_json::from_value(encoded).unwrap();
        assert_eq!(restored, legacy);
        assert_eq!(restored.fingerprint(), legacy.fingerprint());
        assert!(restored.matches(&file).unwrap());

        std::fs::rename(&path, directory.path().join("retained")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        assert!(restored.matches(&file).unwrap());
        assert!(!restored.matches(&File::open(&path).unwrap()).unwrap());
    }

    #[test]
    fn volume_identity_distinguishes_other_volumes_and_rejects_incoherent_encodings() {
        let original = Identity {
            device: 0,
            inode: 42,
            created: Some(SystemTime::UNIX_EPOCH),
            volume: Some([1; 16]),
        };
        let other = Identity {
            volume: Some([2; 16]),
            ..original.clone()
        };
        assert!(original.valid());
        assert_ne!(original.fingerprint(), other.fingerprint());
        assert!(!original.same_device(&other));
        assert!(!original.may_alias(&other));
        let legacy = Identity {
            device: 17,
            volume: None,
            ..original.clone()
        };
        assert!(original.may_alias(&legacy));
        assert!(legacy.may_alias(&original));
        assert_ne!(original.fingerprint(), legacy.fingerprint());
        let another_device = Identity {
            device: 18,
            ..legacy.clone()
        };
        assert_eq!(legacy.may_alias(&another_device), cfg!(target_os = "macos"));
        assert_ne!(legacy, another_device);
        assert_ne!(legacy.fingerprint(), another_device.fingerprint());
        assert!(
            !Identity {
                device: 17,
                ..original.clone()
            }
            .valid()
        );
        assert!(
            !Identity {
                volume: Some([0; 16]),
                ..original
            }
            .valid()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn descriptor_volume_identity_survives_rename_and_rejects_a_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("original");
        std::fs::write(&path, b"original").unwrap();
        let file = File::open(&path).unwrap();
        let (original, legacy) = Identity::observations(&file).unwrap();
        assert!(original.valid());
        assert!(original.has_persistent_volume());
        assert!(!original.uses_device_number());
        assert!(legacy.uses_device_number());
        assert_ne!(original.fingerprint(), legacy.fingerprint());
        let restored: Identity =
            serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
        assert!(restored.matches(&file).unwrap());

        let renamed = directory.path().join("renamed");
        std::fs::rename(&path, &renamed).unwrap();
        assert_eq!(
            Identity::of(&File::open(renamed).unwrap()).unwrap(),
            original
        );
        std::fs::write(&path, b"original").unwrap();
        assert!(!restored.matches(&File::open(path).unwrap()).unwrap());
    }
}
