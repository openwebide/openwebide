//! Ordinary file changes use the same VFS implementation as other native writers.
use super::{Vfs, VfsEntryKind, VfsError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileMutationRequest {
    pub root: String,
    pub mutation: FileMutation,
}

impl FileMutationRequest {
    /// Bounded JSON envelope, including base64 for one maximum-size binary file.
    pub fn limit() -> usize {
        usize::try_from(super::MAX_READ_BYTES * 4 / 3 + 64 * 1024).unwrap_or(usize::MAX)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileMutation {
    Write {
        path: String,
        #[serde(with = "contents")]
        contents: Vec<u8>,
    },
    Replace {
        path: String,
        #[serde(with = "contents")]
        contents: Vec<u8>,
    },
    Create {
        path: String,
        kind: VfsEntryKind,
    },
    Delete {
        path: String,
    },
    Remove {
        path: String,
        kind: VfsEntryKind,
    },
    Copy {
        from: String,
        to: String,
    },
    Move {
        from: String,
        to: String,
        kind: VfsEntryKind,
    },
}

impl FileMutation {
    pub async fn apply(&self, files: &impl Vfs) -> Result<(), VfsError> {
        match self {
            Self::Write { path, contents } => files.write_bytes(path, contents).await,
            Self::Replace { path, contents } => files.replace_atomic(path, contents).await,
            Self::Create { path, kind } => files.create(path, *kind).await,
            Self::Delete { path } => files.delete(path).await,
            Self::Remove { path, kind } => files.remove_entry(path, *kind).await,
            Self::Copy { from, to } => files.copy(from, to).await,
            Self::Move { from, to, kind } => files.move_atomic(from, to, *kind).await,
        }
    }
}

mod contents {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _, ser::Error as _};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        if bytes.len() as u64 > super::super::MAX_READ_BYTES {
            return Err(S::Error::custom("File exceeds the workspace write limit"));
        }
        serializer.collect_str(&base64::display::Base64Display::new(bytes, &STANDARD))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.len() > super::FileMutationRequest::limit() {
            return Err(D::Error::custom("File exceeds the workspace write limit"));
        }
        let bytes = STANDARD.decode(value).map_err(D::Error::custom)?;
        if bytes.len() as u64 > super::super::MAX_READ_BYTES {
            return Err(D::Error::custom("File exceeds the workspace write limit"));
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryVfs;

    #[test]
    fn transported_changes_preserve_binary_contents_and_object_semantics() {
        futures::executor::block_on(async {
            let files = MemoryVfs::new();
            let mutations = [
                FileMutation::Create {
                    path: "dir".into(),
                    kind: VfsEntryKind::Directory,
                },
                FileMutation::Write {
                    path: "dir/source".into(),
                    contents: vec![0, 255, 13, 10],
                },
                FileMutation::Copy {
                    from: "dir/source".into(),
                    to: "dir/copy".into(),
                },
                FileMutation::Move {
                    from: "dir/source".into(),
                    to: "dir/moved".into(),
                    kind: VfsEntryKind::File,
                },
            ];
            for mutation in mutations {
                let wire = serde_json::to_vec(&FileMutationRequest {
                    root: "project".into(),
                    mutation,
                })
                .unwrap();
                let request: FileMutationRequest = serde_json::from_slice(&wire).unwrap();
                request.mutation.apply(&files).await.unwrap();
            }
            assert_eq!(
                files.read_bytes("dir/copy").await.unwrap(),
                [0, 255, 13, 10]
            );
            assert_eq!(
                files.read_bytes("dir/moved").await.unwrap(),
                [0, 255, 13, 10]
            );
            assert!(matches!(
                files.read("dir/source").await,
                Err(VfsError::NotFound(_))
            ));
            // MemoryVfs intentionally lacks these optional capabilities. The
            // transport must preserve refusal rather than substitute a write
            // or recursive delete. Native success cases run over real HTTP.
            for mutation in [
                FileMutation::Replace {
                    path: "dir/moved".into(),
                    contents: b"after\r\n".to_vec(),
                },
                FileMutation::Remove {
                    path: "dir/copy".into(),
                    kind: VfsEntryKind::File,
                },
                FileMutation::Remove {
                    path: "dir".into(),
                    kind: VfsEntryKind::Directory,
                },
            ] {
                let wire = serde_json::to_vec(&mutation).unwrap();
                let mutation: FileMutation = serde_json::from_slice(&wire).unwrap();
                assert!(matches!(mutation.apply(&files).await, Err(VfsError::Io(_))));
            }
            assert_eq!(
                files.read_bytes("dir/moved").await.unwrap(),
                [0, 255, 13, 10]
            );
            assert_eq!(
                files.read_bytes("dir/copy").await.unwrap(),
                [0, 255, 13, 10]
            );
            FileMutation::Delete { path: "dir".into() }
                .apply(&files)
                .await
                .unwrap();
            assert!(files.list("").await.unwrap().is_empty());
        });
    }

    #[test]
    fn binary_transport_rejects_malformed_and_oversized_contents() {
        assert!(
            serde_json::from_str::<FileMutation>(
                r#"{"operation":"write","path":"file","contents":"not base64!"}"#
            )
            .is_err()
        );
        let oversized = usize::try_from(super::super::MAX_READ_BYTES).unwrap() + 1;
        let mutation = FileMutation::Write {
            path: "file".into(),
            contents: vec![0; oversized],
        };
        assert!(serde_json::to_string(&mutation).is_err());
        let mut value = serde_json::json!({"operation":"write","path":"file","contents":""});
        use base64::Engine;
        value["contents"] = base64::engine::general_purpose::STANDARD
            .encode(vec![0; oversized])
            .into();
        assert!(serde_json::from_value::<FileMutation>(value).is_err());
    }
}
