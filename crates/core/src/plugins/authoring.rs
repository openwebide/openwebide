//! Package authoring through shared workspace primitives, independent of project mode.
use super::{
    MAX_PACKAGE_BYTES, MAX_PACKAGE_FILE_BYTES, MAX_PACKAGE_FILES, PackageFile, PackageFileKind,
    PluginManifest, package_digest, package_path, validate_files,
};
use crate::{Vfs, VfsError};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftFile {
    pub path: String,
    pub content: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackageValidation {
    pub manifest: PluginManifest,
    pub digest: String,
    pub files: usize,
    /// Executable builds and export compatibility are checked on host preparation.
    pub compiled: bool,
}
fn validate(files: &[PackageFile]) -> Result<PackageValidation, String> {
    let manifest = validate_files(files).map_err(|error| error.to_string())?;
    manifest
        .validate_activation()
        .map_err(|error| error.to_string())?;
    Ok(PackageValidation {
        manifest,
        digest: package_digest(files),
        files: files.len(),
        compiled: false,
    })
}
pub fn validate_draft(draft: &[DraftFile]) -> Result<PackageValidation, String> {
    let files: Vec<_> = draft
        .iter()
        .map(|file| PackageFile {
            path: file.path.clone(),
            content: file.content.as_bytes().to_vec(),
            kind: PackageFileKind::File,
        })
        .collect();
    validate(&files)
}

fn current(active: &impl Fn() -> bool) -> Result<(), String> {
    if active() {
        Ok(())
    } else {
        Err("Plugin authoring context changed.".into())
    }
}
fn join(root: &str, path: &str) -> String {
    if root.is_empty() {
        path.into()
    } else {
        format!("{root}/{path}")
    }
}
async fn canonical(vfs: &dyn Vfs, path: &str) -> Result<(), String> {
    if vfs
        .canonicalize(path)
        .await
        .map_err(|error| error.to_string())?
        != path
    {
        return Err("Plugin packages cannot contain symbolic links or aliased paths.".into());
    }
    Ok(())
}

pub async fn validate_workspace(
    vfs: &dyn Vfs,
    directory: &str,
    active: impl Fn() -> bool,
) -> Result<PackageValidation, String> {
    let root = crate::vfs::workspace_path(directory).map_err(|error| error.to_string())?;
    if root
        .split('/')
        .any(|part| part.eq_ignore_ascii_case(".git"))
    {
        return Err("Plugin packages cannot be authored inside Git metadata.".into());
    }
    current(&active)?;
    canonical(vfs, &root).await?;
    let manifest_path = join(&root, "plugin.json");
    canonical(vfs, &manifest_path).await?;
    let manifest_bytes = vfs
        .read_bytes(&manifest_path)
        .await
        .map_err(|error| error.to_string())?;
    if manifest_bytes.len() > MAX_PACKAGE_FILE_BYTES {
        return Err("Plugin manifest exceeds its content limit.".into());
    }
    let manifest: PluginManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("Invalid plugin.json: {error}"))?;
    let rust = manifest.executable.is_some();
    let mut pending = vec![root.clone()];
    let mut files = Vec::new();
    let mut visited = 0_usize;
    let mut total = 0_usize;
    while let Some(directory) = pending.pop() {
        current(&active)?;
        let entries = vfs
            .list(&directory)
            .await
            .map_err(|error| error.to_string())?;
        for entry in entries {
            current(&active)?;
            // Workspace metadata and generated Cargo output are not source package files.
            if entry.name.eq_ignore_ascii_case(".git")
                || (rust && directory == root && entry.is_dir && entry.name == "target")
            {
                continue;
            }
            visited += 1;
            if visited > MAX_PACKAGE_FILES {
                return Err("Plugin package exceeds its file/directory limit.".into());
            }
            let expected = join(&directory, &entry.name);
            if entry.path != expected || entry.name.contains('/') || entry.name.contains('\\') {
                return Err("Workspace returned a path outside the package directory.".into());
            }
            let relative = if root.is_empty() {
                entry.path.as_str()
            } else {
                entry
                    .path
                    .strip_prefix(&format!("{root}/"))
                    .ok_or("Package path escaped its directory.")?
            };
            package_path(relative).map_err(|error| error.to_string())?;
            canonical(vfs, &entry.path).await?;
            if entry.is_dir {
                pending.push(entry.path);
                continue;
            }
            if entry.size > MAX_PACKAGE_FILE_BYTES as u64 {
                return Err("Plugin file exceeds its content limit.".into());
            }
            let content = vfs
                .read_bytes(&entry.path)
                .await
                .map_err(|error| error.to_string())?;
            total = total
                .checked_add(content.len())
                .ok_or("Plugin package exceeds its content limit.")?;
            if content.len() > MAX_PACKAGE_FILE_BYTES || total > MAX_PACKAGE_BYTES {
                return Err("Plugin package exceeds its content limit.".into());
            }
            files.push(PackageFile {
                path: relative.into(),
                content,
                kind: PackageFileKind::File,
            });
        }
    }
    current(&active)?;
    if files
        .iter()
        .find(|file| file.path == "plugin.json")
        .is_none_or(|file| file.content != manifest_bytes)
    {
        return Err(
            "Plugin manifest changed while validating; retry against the current source.".into(),
        );
    }
    validate(&files)
}

/// Create a validated package without overwriting existing files. An interrupted
/// creation reports the files already claimed so the user can inspect the partial package.
pub async fn create_workspace(
    vfs: &dyn Vfs,
    directory: &str,
    draft: Vec<DraftFile>,
    active: impl Fn() -> bool,
) -> Result<PackageValidation, String> {
    package_path(directory).map_err(|error| error.to_string())?;
    crate::vfs::workspace_path(directory).map_err(|error| error.to_string())?;
    if directory
        .split('/')
        .any(|part| part.eq_ignore_ascii_case(".git"))
    {
        return Err("Plugin packages cannot be authored inside Git metadata.".into());
    }
    let files: Vec<_> = draft
        .into_iter()
        .map(|file| PackageFile {
            path: file.path,
            content: file.content.into_bytes(),
            kind: PackageFileKind::File,
        })
        .collect();
    let validated = validate(&files)?;
    current(&active)?;
    match vfs.list(directory).await {
        Ok(entries) if !entries.is_empty() => {
            return Err(
                "Create plugins in a new or empty directory; existing files are preserved.".into(),
            );
        }
        Ok(_) | Err(VfsError::NotFound(_)) => {}
        Err(error) => return Err(error.to_string()),
    }
    let mut claimed = Vec::new();
    let result = async {
        // Create each ancestor using the same canonical workspace primitive.
        for file in &files {
            current(&active)?;
            let path = join(directory, &file.path);
            let mut parent = String::new();
            for part in path.split('/').take(path.split('/').count() - 1) {
                parent = join(&parent, part);
                vfs.create(&parent, crate::vfs::VfsEntryKind::Directory)
                    .await
                    .map_err(|error| error.to_string())?;
                canonical(vfs, &parent).await?;
            }
            current(&active)?;
            vfs.create(&path, crate::vfs::VfsEntryKind::File)
                .await
                .map_err(|error| error.to_string())?;
            claimed.push(path.clone());
            current(&active)?;
            vfs.write_bytes(&path, &file.content)
                .await
                .map_err(|error| error.to_string())?;
        }
        current(&active)?;
        Ok(validated)
    }
    .await;
    result.map_err(|error: String| {
        if claimed.is_empty() {
            error
        } else {
            format!(
                "{error} Created files: {}. Inspect this partial package before retrying.",
                claimed.join(", ")
            )
        }
    })
}

#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    use super::*;
    /// Same authoring contract can run against browser, native and memory workspaces.
    pub async fn contract(vfs: &dyn Vfs) {
        let package = super::super::testing::review_files();
        let draft: Vec<_> = package
            .iter()
            .map(|file| DraftFile {
                path: file.path.clone(),
                content: String::from_utf8(file.content.clone()).unwrap(),
            })
            .collect();
        let created = create_workspace(vfs, "authored-plugin", draft.clone(), || true)
            .await
            .unwrap();
        assert!(!created.compiled);
        let validated = validate_workspace(vfs, "authored-plugin", || true)
            .await
            .unwrap();
        assert_eq!(created.digest, validated.digest);
        assert_eq!(created.manifest, validated.manifest);
        assert_eq!(created.files, package.len());
        assert!(
            create_workspace(vfs, "authored-plugin", draft.clone(), || true)
                .await
                .is_err()
        );
        assert_eq!(
            validate_workspace(vfs, "authored-plugin", || true)
                .await
                .unwrap()
                .digest,
            created.digest
        );
        let mut invalid = draft.clone();
        invalid[0].path = "../escape".into();
        assert!(
            create_workspace(vfs, "invalid-plugin", invalid, || true)
                .await
                .is_err()
        );
        assert!(
            vfs.list("invalid-plugin")
                .await
                .unwrap_or_default()
                .is_empty()
        );
        let mut rust_draft = draft.clone();
        let mut manifest: PluginManifest = serde_json::from_str(&rust_draft[0].content).unwrap();
        manifest.compatibility.plugin_api = 3;
        manifest.contributions.tools.push(super::super::PluginTool {
            name: "example_echo".into(),
            description: "Example source validation tool".into(),
            parameters: serde_json::json!({"type":"object"}),
            requires_approval: false,
        });
        manifest.executable = Some(super::super::RustPlugin {
            manifest: "Cargo.toml".into(),
            library: "example".into(),
            sdk_version: "0.1.0".into(),
            capabilities: Vec::new(),
        });
        rust_draft[0].content = serde_json::to_string(&manifest).unwrap();
        rust_draft.extend([
            DraftFile{path:"Cargo.toml".into(),content:"[package]\nname = \"example\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\ncrate-type = [\"cdylib\"]\n".into()},
            DraftFile{path:"Cargo.lock".into(),content:"version = 4\n".into()},
            DraftFile{path:"src/lib.rs".into(),content:"// Source compilation is checked on host preparation.\n".into()},
        ]);
        let rust_created = create_workspace(vfs, "rust-plugin", rust_draft, || true)
            .await
            .unwrap();
        vfs.create("rust-plugin/target", crate::vfs::VfsEntryKind::Directory)
            .await
            .unwrap();
        vfs.write("rust-plugin/target/generated", "build output")
            .await
            .unwrap();
        assert_eq!(
            validate_workspace(vfs, "rust-plugin", || true)
                .await
                .unwrap()
                .digest,
            rust_created.digest
        );
        for path in [".git/new-plugin", ".GIT/new-plugin", ".spin/new-plugin"] {
            assert!(
                create_workspace(vfs, path, draft.clone(), || true)
                    .await
                    .is_err()
            );
        }
        vfs.create("authored-plugin/.git", crate::vfs::VfsEntryKind::Directory)
            .await
            .unwrap();
        vfs.write("authored-plugin/.git/config", "workspace metadata")
            .await
            .unwrap();
        assert_eq!(
            validate_workspace(vfs, "authored-plugin", || true)
                .await
                .unwrap()
                .digest,
            created.digest
        );
        assert!(
            create_workspace(vfs, "cancelled-plugin", draft, || false)
                .await
                .is_err()
        );
        assert!(
            vfs.list("cancelled-plugin")
                .await
                .unwrap_or_default()
                .is_empty()
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authoring_contract_preserves_files_and_rejects_invalid_or_stale_requests() {
        futures::executor::block_on(testing::contract(&crate::MemoryVfs::new()));
    }
}
