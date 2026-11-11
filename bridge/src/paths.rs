use std::path::{Component, Path, PathBuf};

pub fn canonical_root(p: &Path) -> Result<PathBuf, String> {
    let canonical = p
        .canonicalize()
        .map_err(|e| format!("failed to canonicalize workspace root: {e}"))?;
    if !canonical.is_dir() {
        return Err(format!(
            "workspace root is not a directory: {}",
            canonical.display()
        ));
    }
    Ok(canonical)
}

pub fn resolve_in_root(root: &Path, requested: Option<&str>) -> Result<PathBuf, String> {
    let req = requested.unwrap_or("");
    if req.is_empty() {
        return Ok(root.to_path_buf());
    }

    let normalized = normalize_joined(root, req);

    if !normalized.starts_with(root) {
        return Err(format!("cwd escapes workspace root: {req}"));
    }

    match std::fs::metadata(&normalized) {
        Ok(m) if m.is_dir() => Ok(normalized),
        _ => Err(format!("cwd does not exist: {req}")),
    }
}

fn normalize_joined(root: &Path, rel: &str) -> PathBuf {
    let mut normalized = PathBuf::new();
    for comp in root.join(rel).components() {
        match comp {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            _ => normalized.push(comp),
        }
    }
    normalized
}

pub fn resolve_file_in_root(root: &Path, rel: &str) -> Result<PathBuf, String> {
    if rel.is_empty() || Path::new(rel).is_absolute() {
        return Err(format!("invalid relative file path: {rel}"));
    }
    if Path::new(rel)
        .components()
        .any(|c| c == Component::Normal(".spin".as_ref()))
    {
        return Err(".spin is not accessible".into());
    }
    let normalized = normalize_joined(root, rel);
    if !normalized.starts_with(root) {
        return Err(format!("path escapes workspace root: {rel}"));
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn create_temp_dir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("webide-test-{}", std::process::id()));
        // Make it unique by adding an incrementing number
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        p.push(format!("dir-{}", COUNTER.fetch_add(1, Ordering::SeqCst)));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn test_canonical_root() {
        let p = create_temp_dir();
        assert!(canonical_root(&p).is_ok());

        let non_existent = p.join("not_here");
        assert!(canonical_root(&non_existent).is_err());

        let file = p.join("file");
        fs::write(&file, "test").unwrap();
        assert!(canonical_root(&file).is_err());
    }

    #[test]
    fn test_resolve_in_root() {
        let p = create_temp_dir();
        let root = p.canonicalize().unwrap();

        let sub = root.join("sub");
        fs::create_dir(&sub).unwrap();

        let file = root.join("file");
        fs::write(&file, "test").unwrap();

        #[cfg(unix)]
        {
            let ext = create_temp_dir();
            let ext_dir = ext.canonicalize().unwrap().join("ext_dir");
            fs::create_dir(&ext_dir).unwrap();
            let sym = root.join("sym");
            std::os::unix::fs::symlink(&ext_dir, &sym).unwrap();
            assert_eq!(resolve_in_root(&root, Some("sym")).unwrap(), sym);
        }

        // "" -> Ok
        assert_eq!(resolve_in_root(&root, Some("")).unwrap(), root);
        assert_eq!(resolve_in_root(&root, None).unwrap(), root);

        // "sub" -> Ok
        assert_eq!(resolve_in_root(&root, Some("sub")).unwrap(), sub);

        // absolute-inside -> Ok
        assert_eq!(
            resolve_in_root(&root, Some(sub.to_str().unwrap())).unwrap(),
            sub
        );

        // "../.." -> Err
        assert!(
            resolve_in_root(&root, Some("../.."))
                .unwrap_err()
                .contains("escapes")
        );

        // "sub/../.." -> Err
        assert!(
            resolve_in_root(&root, Some("sub/../.."))
                .unwrap_err()
                .contains("escapes")
        );

        // "/etc" -> Err
        assert!(
            resolve_in_root(&root, Some("/etc"))
                .unwrap_err()
                .contains("escapes")
        );

        // nonexistent -> Err
        assert!(
            resolve_in_root(&root, Some("nope"))
                .unwrap_err()
                .contains("does not exist")
        );

        // file -> Err
        assert!(
            resolve_in_root(&root, Some("file"))
                .unwrap_err()
                .contains("does not exist")
        );
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;

    #[test]
    fn file_paths_resolve_without_existence_checks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("exists"), "x").unwrap();
        for rel in [
            "exists",
            "missing",
            "missing-parent/leaf",
            "a/../leaf",
            "./leaf",
        ] {
            let result = resolve_file_in_root(&root, rel).unwrap();
            assert!(result.starts_with(&root));
        }
        for rel in [
            "",
            "/etc/passwd",
            "../leaf",
            "a/../../leaf",
            ".spin/db",
            "a/.spin/../leaf",
        ] {
            assert!(resolve_file_in_root(&root, rel).is_err(), "{rel}");
        }
    }
}
