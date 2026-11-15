//! OS installation policy, shared by every bridge profile for the same UID.
//! It is deliberately unavailable through CLI flags, environment or user settings.
use super::*;

const INVALID: &str = "Native recovery installation policy is unsafe or unavailable";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    format: u32,
    directory: PathBuf,
}
fn trusted(metadata: &std::fs::Metadata, owner: u32, directory: bool) -> bool {
    metadata.uid() == owner
        && metadata.mode() & 0o022 == 0
        && if directory {
            metadata.is_dir()
        } else {
            metadata.is_file() && metadata.nlink() == 1
        }
}
fn policy(parent: &Dir, uid: u32, administrator: u32) -> Result<Option<PathBuf>, String> {
    if !trusted(
        &io(parent.try_clone())?
            .into_std_file()
            .metadata()
            .map_err(|_| INVALID)?,
        administrator,
        true,
    ) {
        return Err(INVALID.into());
    }
    let mut directory = io(parent.try_clone())?;
    for component in ["openwebide", "native-recovery"] {
        let metadata = match directory.symlink_metadata(component) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(INVALID.into()),
        };
        if !metadata.is_dir() {
            return Err(INVALID.into());
        }
        let next = directory.open_dir(component).map_err(|_| INVALID)?;
        if !trusted(
            &io(next.try_clone())?
                .into_std_file()
                .metadata()
                .map_err(|_| INVALID)?,
            administrator,
            true,
        ) {
            return Err(INVALID.into());
        }
        directory = next;
    }
    let name = format!("{uid}.json");
    match directory.symlink_metadata(&name) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Err(INVALID.into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(INVALID.into()),
    }
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(libc::O_NONBLOCK);
    let file = directory
        .open_with(&name, &options)
        .map_err(|_| INVALID)?
        .into_std();
    if !trusted(&file.metadata().map_err(|_| INVALID)?, administrator, false) {
        return Err(INVALID.into());
    }
    let data = bytes(&file, 8192).map_err(|_| INVALID)?;
    let policy: Policy = serde_json::from_slice(&data).map_err(|_| INVALID)?;
    if policy.format != 1
        || !policy.directory.is_absolute()
        || policy.directory.file_name().is_none()
        || policy
            .directory
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(INVALID.into());
    }
    Ok(Some(policy.directory))
}
fn prepare(directory: &Path, uid: u32) -> Result<(), String> {
    prepare_directory(directory, uid, crate::native_directory::sync)
}
fn prepare_directory(
    directory: &Path,
    uid: u32,
    flush: impl FnOnce(&Dir) -> std::io::Result<()>,
) -> Result<(), String> {
    let parent = Dir::open_ambient_dir(
        directory.parent().ok_or(INVALID)?,
        cap_std::ambient_authority(),
    )
    .map_err(|_| INVALID)?;
    let name = directory.file_name().ok_or(INVALID)?;
    match parent.symlink_metadata(name) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err(INVALID.into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = DirBuilder::new();
            builder.mode(0o700);
            match parent.create_dir_with(name, &builder) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(INVALID.into()),
            }
        }
        Err(_) => return Err(INVALID.into()),
    }
    if !parent.symlink_metadata(name).map_err(|_| INVALID)?.is_dir() {
        return Err(INVALID.into());
    }
    let file = parent.open_dir(name).map_err(|_| INVALID)?.into_std_file();
    private(&file, uid, true).map_err(|_| INVALID)?;
    // Existence alone cannot establish durability after a previous creation or
    // flush interruption. Every accepted retry must cross the same barrier.
    flush(&parent).map_err(|_| INVALID)?;
    Ok(())
}
pub(super) fn configured(uid: u32) -> Result<Option<PathBuf>, String> {
    let system =
        Dir::open_ambient_dir("/etc", cap_std::ambient_authority()).map_err(|_| INVALID)?;
    let selected = policy(&system, uid, 0)?;
    if let Some(directory) = &selected {
        prepare(directory, uid)?;
    }
    Ok(selected)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    fn fixture() -> (tempfile::TempDir, Dir, u32) {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("openwebide/native-recovery")).unwrap();
        let directory = Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
        // SAFETY: geteuid has no preconditions or borrowed return value.
        (root, directory, unsafe { libc::geteuid() })
    }
    #[test]
    fn per_uid_policy_is_strict_and_not_a_profile_setting() {
        let (root, directory, uid) = fixture();
        let file = root
            .path()
            .join(format!("openwebide/native-recovery/{uid}.json"));
        let state = root.path().join(".spin/principal");
        std::fs::create_dir(root.path().join(".spin")).unwrap();
        std::fs::write(
            &file,
            serde_json::to_vec(&serde_json::json!({"format":1,"directory":state})).unwrap(),
        )
        .unwrap();
        assert_eq!(policy(&directory, uid, uid).unwrap(), Some(state.clone()));
        assert_eq!(policy(&directory, uid + 1, uid).unwrap(), None);
        assert!(policy(&directory, uid, uid + 1).is_err());
        prepare(&state, uid).unwrap();
        let original = same_file::Handle::from_path(&state).unwrap();
        prepare(&state, uid).unwrap();
        assert_eq!(original, same_file::Handle::from_path(&state).unwrap());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(policy(&directory, uid, uid).is_err());
    }
    #[test]
    fn provisioning_retry_requires_durability_for_the_same_original_directory() {
        let (root, _, uid) = fixture();
        let root = root.keep();
        std::fs::create_dir(root.join(".spin")).unwrap();
        let state = root.join(".spin/principal");
        // Inject failure only at the flush primitive. Creation, ownership/mode
        // validation and the original directory observations use real files.
        let unavailable = |_: &Dir| {
            Err(std::io::Error::other(
                "Injected parent directory durability failure",
            ))
        };
        assert!(prepare_directory(&state, uid, unavailable).is_err());
        let original = same_file::Handle::from_path(&state).unwrap();
        assert!(
            prepare_directory(&state, uid, unavailable).is_err(),
            "An existing directory admitted a retry while durability was unavailable"
        );
        assert_eq!(original, same_file::Handle::from_path(&state).unwrap());
        prepare(&state, uid).unwrap();
        assert_eq!(original, same_file::Handle::from_path(&state).unwrap());
        println!("retained_provisioning_retry_fixture={}", root.display());
    }

    #[test]
    fn malformed_and_redirected_installation_state_never_falls_back() {
        let (root, directory, uid) = fixture();
        let file = root
            .path()
            .join(format!("openwebide/native-recovery/{uid}.json"));
        for value in [
            "{}",
            "{\"format\":1,\"directory\":\"relative\"}",
            "{\"format\":2,\"directory\":\"/data/principal\"}",
            "{\"format\":1,\"directory\":\"/data/../principal\"}",
        ] {
            std::fs::write(&file, value).unwrap();
            assert!(policy(&directory, uid, uid).is_err());
        }
        std::fs::remove_file(&file).unwrap();
        let other = root.path().join("other");
        std::fs::write(&other, b"{}").unwrap();
        symlink(&other, &file).unwrap();
        assert!(policy(&directory, uid, uid).is_err());
        let state = root.path().join("principal");
        symlink(root.path(), &state).unwrap();
        assert!(prepare(&state, uid).is_err());
        assert!(state.is_symlink());
    }
}
