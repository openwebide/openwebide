//! Native Git/object and atomic filesystem primitives for shared plugin policy.
mod http;
pub mod invocations;
pub mod jobs;
pub mod runs;
pub mod transport;
use openwebide_core::plugins::{
    MAX_PACKAGE_BYTES, MAX_PACKAGE_FILE_BYTES, MAX_PACKAGE_FILES, PackageFile, PackageFileKind,
    PluginError, PluginFuture, PluginHost, PluginSource, PreparedPlugin, prepare_plugin,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, sync::Mutex};
pub mod preparations;

/// Only host binaries contain the bundled plugin files.
fn bundled_files(source: &PluginSource) -> Option<Vec<PackageFile>> {
    #[derive(serde::Deserialize)]
    struct Bundle {
        repository: String,
        commit: String,
        plugins: Vec<Package>,
    }
    #[derive(serde::Deserialize)]
    struct Package {
        path: String,
        files: Vec<File>,
    }
    #[derive(serde::Deserialize)]
    struct File {
        path: String,
        content: String,
        executable: bool,
    }
    let bundle: Bundle = serde_json::from_str(include_str!("../bundled/plugins.json"))
        .expect("valid bundled host plugins");
    if source.repository != bundle.repository || source.commit != bundle.commit {
        return None;
    }
    bundle
        .plugins
        .into_iter()
        .find(|package| package.path == source.path)
        .map(|package| {
            package
                .files
                .into_iter()
                .map(|file| PackageFile {
                    path: file.path,
                    content: file.content.into_bytes(),
                    kind: if file.executable {
                        PackageFileKind::Executable
                    } else {
                        PackageFileKind::File
                    },
                })
                .collect()
        })
}

#[derive(Clone, Debug)]
pub struct NativePluginInstaller {
    root: Option<PathBuf>,
    lock: Arc<Mutex<()>>,
    pub(crate) preparations: preparations::Preparations,
}
impl Default for NativePluginInstaller {
    fn default() -> Self {
        Self::new(default_root())
    }
}
impl NativePluginInstaller {
    pub fn new(root: Option<PathBuf>) -> Self {
        Self {
            root,
            lock: Arc::new(Mutex::new(())),
            preparations: preparations::Preparations::default(),
        }
    }

    pub async fn component(
        &self,
        owner: &str,
        prepared: &PreparedPlugin,
    ) -> Result<Vec<u8>, PluginError> {
        prepared.validate()?;
        if prepared.manifest.executable.is_none() {
            return Err(PluginError::Invalid(
                "Plugin does not declare an executable.".into(),
            ));
        }
        let _guard = self.lock.lock().await;
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| host_error("Plugin cache is not configured."))?;
        if !root.is_absolute() {
            return Err(host_error("Plugin cache path must be absolute."));
        }
        let host = ScopedHost {
            cancelled: Arc::new(AtomicBool::new(false)),
            root: root.join(key(owner)),
            host_id: prepared.host_id.clone(),
        };
        let manifest_path = host
            .snapshot(&prepared.source, &prepared.digest)
            .join("plugin.json");
        reject_link(&manifest_path)?;
        if std::fs::metadata(&manifest_path).map_err(io_error)?.len()
            > MAX_PACKAGE_FILE_BYTES as u64
        {
            return Err(host_error("Cached plugin manifest exceeds its limit."));
        }
        let manifest: openwebide_core::plugins::PluginManifest =
            serde_json::from_slice(&std::fs::read(manifest_path).map_err(io_error)?)
                .map_err(|error| host_error(error.to_string()))?;
        if manifest != prepared.manifest {
            return Err(host_error(
                "Plugin receipt does not match its prepared executable.",
            ));
        }
        load_artifact(&host.artifact(&prepared.source, &prepared.digest))
    }
    pub async fn package(
        &self,
        owner: &str,
        host_id: String,
        expected: &PreparedPlugin,
    ) -> Result<openwebide_core::plugins::PluginPackage, PluginError> {
        let _guard = self.lock.lock().await;
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| host_error("Configure OPENWEBIDE_PLUGIN_DIR on this execution host."))?;
        if !root.is_absolute() {
            return Err(host_error("Plugin cache path must be absolute."));
        }
        let host = ScopedHost {
            cancelled: Arc::new(AtomicBool::new(false)),
            root: root.join(key(owner)),
            host_id,
        };
        openwebide_core::plugins::load_plugin_package(&host, expected).await
    }
    pub async fn catalog(
        &self,
        owner: &str,
        source: &openwebide_core::plugins::marketplace::MarketplaceSource,
        now: i64,
    ) -> Result<openwebide_core::plugins::marketplace::CachedMarketplace, PluginError> {
        use openwebide_core::plugins::marketplace::{
            CachedMarketplace, MAX_CATALOG_BYTES, MarketplaceCatalog,
        };
        source.validate()?;
        let _guard = self.lock.lock().await;
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| host_error("Configure OPENWEBIDE_PLUGIN_DIR on this execution host."))?;
        if !root.is_absolute() {
            return Err(host_error("Plugin cache path must be absolute."));
        }
        let host = ScopedHost {
            cancelled: Arc::new(AtomicBool::new(false)),
            root: root.join(key(owner)),
            host_id: String::new(),
        };
        let repo = host.root.join("repositories").join(key(&source.repository));
        let parent = repo.parent().expect("repository parent");
        create_directory(parent)?;
        if !repo.exists() {
            let stage = tempfile::tempdir_in(parent).map_err(io_error)?;
            let advertised = git(
                stage.path(),
                &["ls-remote", "--", &source.repository, "HEAD"],
                4096,
                false,
            )
            .await?
            .expect("Git output");
            let hash = std::str::from_utf8(&advertised)
                .map_err(|_| host_error("Invalid Git advertisement."))?
                .split_whitespace()
                .next()
                .unwrap_or_default();
            let format = if hash.len() == 64 {
                "--object-format=sha256"
            } else {
                "--object-format=sha1"
            };
            git(stage.path(), &["init", "--bare", format, "."], 4096, false).await?;
            std::fs::rename(stage.path(), &repo).map_err(io_error)?;
        }
        reject_link(&repo)?;
        let reference = if source.reference.is_empty() {
            "HEAD"
        } else {
            &source.reference
        };
        git(
            &repo,
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                "--depth=1",
                "--",
                &source.repository,
                reference,
            ],
            4096,
            false,
        )
        .await?;
        let commit = git(
            &repo,
            &["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
            256,
            false,
        )
        .await?
        .expect("Git output");
        let commit = String::from_utf8(commit)
            .map_err(|_| host_error("Invalid catalog commit."))?
            .trim()
            .to_owned();
        let object = format!("{commit}:{}", source.path);
        let metadata = git(
            &repo,
            &["ls-tree", &commit, "--", &source.path],
            4096,
            false,
        )
        .await?
        .expect("Git output");
        if !metadata.starts_with(b"100644 blob ") && !metadata.starts_with(b"100755 blob ") {
            return Err(PluginError::Invalid(
                "Marketplace must be a regular Git file.".into(),
            ));
        }
        let bytes = git(
            &repo,
            &["cat-file", "blob", &object],
            MAX_CATALOG_BYTES,
            false,
        )
        .await?
        .expect("Git output");
        let snapshot = CachedMarketplace {
            source: source.clone(),
            commit,
            catalog: MarketplaceCatalog::parse(&bytes)?,
            fetched_at: now,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }
    pub async fn prepare(
        &self,
        owner: &str,
        host_id: String,
        source: &PluginSource,
    ) -> Result<PreparedPlugin, PluginError> {
        source.validate()?;
        let adapter = preparations::NativePreparationHost {
            installer: self,
            owner,
            host_id,
            clock: std::time::Instant::now(),
        };
        openwebide_core::plugins::preparation::prepare_on_host(&adapter, source)
            .await
            .map_err(host_error)
    }

    pub(super) async fn prepare_cancellable(
        &self,
        owner: &str,
        host_id: String,
        source: &PluginSource,
        cancelled: Arc<AtomicBool>,
    ) -> Result<PreparedPlugin, PluginError> {
        source.validate()?;
        let _guard = self.lock.lock().await;
        if cancelled.load(Ordering::Acquire) {
            return Err(host_error("Plugin preparation was cancelled."));
        }
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| host_error("Configure OPENWEBIDE_PLUGIN_DIR on this execution host."))?;
        if !root.is_absolute() {
            return Err(host_error(
                "OPENWEBIDE_PLUGIN_DIR must be an absolute host cache path.",
            ));
        }
        let host = ScopedHost {
            cancelled,
            root: root.join(key(owner)),
            host_id,
        };
        prepare_plugin(&host, source).await
    }
}

pub fn default_root() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("OPENWEBIDE_PLUGIN_DIR") {
        return Some(root.into());
    }
    if let Some(root) = std::env::var_os("XDG_DATA_HOME") {
        return Some(PathBuf::from(root).join("openwebide/plugins"));
    }
    #[allow(deprecated)] // Match the native bridge's existing home-directory resolution.
    std::env::home_dir().map(|home| home.join(".local/share/openwebide/plugins"))
}
fn key(value: &str) -> String {
    openwebide_core::plugins::content_digest(value.as_bytes())
}
fn host_error(message: impl Into<String>) -> PluginError {
    PluginError::Host(message.into())
}
fn io_error(error: std::io::Error) -> PluginError {
    host_error(error.to_string())
}

struct ScopedHost {
    cancelled: Arc<AtomicBool>,
    root: PathBuf,
    host_id: String,
}
impl ScopedHost {
    fn repository(&self, source: &PluginSource) -> PathBuf {
        self.root.join("repositories").join(key(&source.repository))
    }
    fn artifact(&self, source: &PluginSource, digest: &str) -> PathBuf {
        self.root.join("artifacts").join(key(&format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            source.repository,
            source.path,
            source.commit,
            digest,
            openwebide_plugin_runtime::build::TOOLCHAIN,
            openwebide_plugin_runtime::build::sdk_digest(),
        )))
    }
    fn snapshot(&self, source: &PluginSource, digest: &str) -> PathBuf {
        self.root
            .join("versions")
            .join(key(&format!("{}\n{}", source.repository, source.path)))
            .join(&source.commit)
            .join(digest)
    }
}

impl PluginHost for ScopedHost {
    fn host_id(&self) -> String {
        self.host_id.clone()
    }
    fn files<'a>(&'a self, source: &'a PluginSource) -> PluginFuture<'a, Vec<PackageFile>> {
        Box::pin(async move {
            if let Some(files) = bundled_files(source) {
                return Ok(files);
            }
            let repo = self.repository(source);
            create_directory(repo.parent().expect("repository parent"))?;
            if !repo.exists() {
                let stage = tempfile::tempdir_in(repo.parent().expect("repository parent"))
                    .map_err(io_error)?;
                let format = if source.commit.len() == 64 {
                    "--object-format=sha256"
                } else {
                    "--object-format=sha1"
                };
                git(stage.path(), &["init", "--bare", format, "."], 4096, false).await?;
                std::fs::rename(stage.path(), &repo).map_err(io_error)?;
            }
            reject_link(&repo)?;
            let object = format!("{}^{{commit}}", source.commit);
            if git(&repo, &["cat-file", "-e", &object], 4096, true)
                .await?
                .is_none()
            {
                git(
                    &repo,
                    &[
                        "fetch",
                        "--quiet",
                        "--no-tags",
                        "--depth=1",
                        "--",
                        &source.repository,
                        &source.commit,
                    ],
                    4096,
                    false,
                )
                .await?;
            }
            let resolved = git(&repo, &["rev-parse", "--verify", &object], 256, false)
                .await?
                .expect("successful Git output");
            if resolved.strip_suffix(b"\n").unwrap_or(&resolved) != source.commit.as_bytes() {
                return Err(PluginError::Invalid(
                    "The selected object is not the requested commit.".into(),
                ));
            }
            let tree = if source.path == "." {
                source.commit.clone()
            } else {
                format!("{}:{}", source.commit, source.path)
            };
            let entries = git(
                &repo,
                &["ls-tree", "-r", "-z", &tree],
                2 * 1024 * 1024,
                false,
            )
            .await?
            .expect("successful Git output");
            let mut files = Vec::new();
            let mut bytes = 0;
            for entry in entries
                .split(|byte| *byte == 0)
                .filter(|entry| !entry.is_empty())
            {
                if files.len() >= MAX_PACKAGE_FILES {
                    return Err(PluginError::Invalid("Too many plugin files.".into()));
                }
                let entry = std::str::from_utf8(entry)
                    .map_err(|_| PluginError::Invalid("Plugin filenames must be UTF-8.".into()))?;
                let (metadata, path) = entry
                    .split_once('\t')
                    .ok_or_else(|| host_error("Invalid Git tree response."))?;
                let fields = metadata.split_whitespace().collect::<Vec<_>>();
                if fields.len() != 3 {
                    return Err(host_error("Invalid Git tree metadata."));
                }
                let kind = match fields[0] {
                    "100644" => PackageFileKind::File,
                    "100755" => PackageFileKind::Executable,
                    "120000" => PackageFileKind::Symlink,
                    "160000" => PackageFileKind::Submodule,
                    _ => return Err(PluginError::Invalid("Unsupported Git file mode.".into())),
                };
                let content = if matches!(kind, PackageFileKind::File | PackageFileKind::Executable)
                {
                    git(
                        &repo,
                        &["cat-file", "blob", fields[2]],
                        MAX_PACKAGE_FILE_BYTES,
                        false,
                    )
                    .await?
                    .expect("successful Git output")
                } else {
                    Vec::new()
                };
                bytes += content.len();
                if bytes > MAX_PACKAGE_BYTES {
                    return Err(PluginError::Invalid("Plugin package is too large.".into()));
                }
                files.push(PackageFile {
                    path: path.into(),
                    content,
                    kind,
                });
            }
            Ok(files)
        })
    }
    fn compile<'a>(
        &'a self,
        source: &'a PluginSource,
        digest: &'a str,
        manifest: &'a openwebide_core::plugins::PluginManifest,
    ) -> PluginFuture<'a, ()> {
        Box::pin(async move {
            let artifact = self.artifact(source, digest);
            let snapshot = self.snapshot(source, digest);
            let manifest = manifest.clone();
            let source = source.clone();
            let digest = digest.to_owned();
            let cancelled = self.cancelled.clone();
            tokio::task::spawn_blocking(move || {
                if cancelled.load(Ordering::Acquire) {
                    return Err(host_error("Plugin preparation was cancelled."));
                }
                if artifact.exists() {
                    load_artifact(&artifact)?;
                    return Ok(());
                }
                let parent = artifact.parent().expect("artifact parent");
                create_directory(parent)?;
                let stage = tempfile::tempdir_in(parent).map_err(io_error)?;
                let build_dir = stage.path().join("build");
                let bytes = if let Some(bytes) = bundled_component(&source, &digest)? {
                    bytes
                } else {
                    if cfg!(feature = "bundled-defaults") && bundled_files(&source).is_some() {
                        return Err(host_error(
                            "The distribution is missing a compatible compiled default. Rebuild its plugin bundle.",
                        ));
                    }
                    let rust = manifest.executable.as_ref().expect("Rust manifest");
                    std::fs::create_dir(&build_dir).map_err(io_error)?;
                    openwebide_plugin_runtime::build::compile_cancellable(&snapshot, &rust.library, &build_dir, &cancelled)
                        .map_err(|error| host_error(format!("{error:#}")))?
                };
                let runtime = openwebide_plugin_runtime::Runtime::new()
                    .map_err(|error| host_error(error.to_string()))?;
                runtime
                    .validate_exports(&bytes, &manifest)
                    .map_err(|error| {
                        PluginError::Invalid(format!("Invalid plugin interface: {error:#}"))
                    })?;
                if cancelled.load(Ordering::Acquire) {
                    return Err(host_error("Plugin preparation was cancelled."));
                }
                // Build artifacts are disposable; retain only the validated component.
                if build_dir.exists() {
                    std::fs::remove_dir_all(&build_dir).map_err(io_error)?;
                }
                std::fs::write(stage.path().join("plugin.wasm"), &bytes).map_err(io_error)?;
                std::fs::write(
                    stage.path().join("digest"),
                    openwebide_core::plugins::content_digest(&bytes),
                )
                .map_err(io_error)?;
                std::fs::rename(stage.path(), &artifact).map_err(io_error)?;
                Ok(())
            })
            .await
            .map_err(|error| host_error(error.to_string()))?
        })
    }
    fn publish<'a>(
        &'a self,
        source: &'a PluginSource,
        digest: &'a str,
        files: &'a [PackageFile],
    ) -> PluginFuture<'a, ()> {
        Box::pin(async move {
            let destination = self.snapshot(source, digest);
            let parent = destination.parent().expect("snapshot parent");
            create_directory(parent)?;
            if destination.exists() {
                return verify_snapshot(&destination, files);
            }
            let stage = tempfile::tempdir_in(parent).map_err(io_error)?;
            for file in files {
                let target = stage.path().join(&file.path);
                std::fs::create_dir_all(target.parent().expect("file parent")).map_err(io_error)?;
                std::fs::write(&target, &file.content).map_err(io_error)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = if file.kind == PackageFileKind::Executable {
                        0o500
                    } else {
                        0o400
                    };
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))
                        .map_err(io_error)?;
                }
            }
            std::fs::rename(stage.path(), &destination).map_err(io_error)?;
            verify_snapshot(&destination, files)
        })
    }
}

fn bundled_component(source: &PluginSource, digest: &str) -> Result<Option<Vec<u8>>, PluginError> {
    use openwebide_plugin_runtime::bundled::CompiledBundle;
    static BUNDLE: std::sync::LazyLock<Result<CompiledBundle, String>> =
        std::sync::LazyLock::new(|| {
            serde_json::from_slice(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/bundled-artifacts.json"
            )))
            .map_err(|error| error.to_string())
        });
    BUNDLE
        .as_ref()
        .map_err(|error| host_error(error.clone()))?
        .component(source, digest)
        .map_err(|error| host_error(format!("{error:#}")))
}
fn load_artifact(path: &Path) -> Result<Vec<u8>, PluginError> {
    reject_link(path)?;
    let file = path.join("plugin.wasm");
    reject_link(&file)?;
    let digest = path.join("digest");
    reject_link(&digest)?;
    if std::fs::metadata(&file).map_err(io_error)?.len() > 32 * 1024 * 1024 {
        return Err(host_error("Cached plugin artifact exceeds its limit."));
    }
    let bytes = std::fs::read(file).map_err(io_error)?;
    if std::fs::read_to_string(digest).map_err(io_error)?
        != openwebide_core::plugins::content_digest(&bytes)
    {
        return Err(host_error("Cached plugin artifact has changed."));
    }
    Ok(bytes)
}

fn reject_link(path: &Path) -> Result<(), PluginError> {
    if std::fs::symlink_metadata(path)
        .map_err(io_error)?
        .file_type()
        .is_symlink()
    {
        return Err(host_error("Plugin cache paths cannot be symbolic links."));
    }
    Ok(())
}
fn create_directory(path: &Path) -> Result<(), PluginError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        if !parent.exists() {
            create_directory(parent)?;
        } else {
            reject_link(parent)?;
        }
    }
    std::fs::create_dir_all(path).map_err(io_error)?;
    reject_link(path)
}
fn verify_snapshot(root: &Path, files: &[PackageFile]) -> Result<(), PluginError> {
    reject_link(root)?;
    let mut pending = vec![root.to_path_buf()];
    let mut count = 0;
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            let kind = entry.file_type().map_err(io_error)?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                count += 1;
            } else {
                return Err(host_error(
                    "The cached plugin snapshot contains an unsupported file.",
                ));
            }
            if count > files.len() {
                return Err(host_error(
                    "The cached plugin snapshot contains extra files.",
                ));
            }
        }
    }
    if count != files.len() {
        return Err(host_error("The cached plugin snapshot has missing files."));
    }
    for file in files {
        let mut target = root.to_path_buf();
        for part in file.path.split('/') {
            target.push(part);
            reject_link(&target)?;
        }
        let metadata = std::fs::metadata(&target).map_err(io_error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let executable = metadata.permissions().mode() & 0o111 != 0;
            if executable != (file.kind == PackageFileKind::Executable) {
                return Err(host_error("The cached plugin snapshot file mode changed."));
            }
        }
        if !metadata.is_file()
            || metadata.len() != file.content.len() as u64
            || std::fs::read(&target).map_err(io_error)? != file.content
        {
            return Err(host_error(
                "The cached plugin snapshot changed; prepare a clean host cache.",
            ));
        }
    }
    Ok(())
}

async fn git(
    cwd: &Path,
    args: &[&str],
    limit: usize,
    allow_failure: bool,
) -> Result<Option<Vec<u8>>, PluginError> {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "protocol.ext.allow=never",
            "-c",
            "protocol.file.allow=never",
        ])
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(io_error)?;
    #[cfg(unix)]
    let group_guard = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .map(crate::exec::proc::GroupGuard::new);
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let result = tokio::time::timeout(Duration::from_secs(90), async {
        let out = async {
            let mut bytes = Vec::new();
            stdout
                .take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .await?;
            Ok::<_, std::io::Error>(bytes)
        };
        let err = async {
            let mut bytes = Vec::new();
            stderr.take(8192).read_to_end(&mut bytes).await?;
            Ok::<_, std::io::Error>(bytes)
        };
        tokio::try_join!(out, err, child.wait())
    })
    .await
    .map_err(|_| host_error("Plugin Git request timed out."))?
    .map_err(io_error)?;
    #[cfg(unix)]
    if let Some(guard) = group_guard {
        guard.disarm();
    }
    if result.0.len() > limit {
        return Err(PluginError::Invalid(
            "Plugin Git object exceeds its content limit.".into(),
        ));
    }
    if !result.2.success() {
        if allow_failure {
            return Ok(None);
        }
        tracing::warn!(detail = %String::from_utf8_lossy(&result.1), "Plugin Git request failed");
        return Err(host_error(
            "Could not fetch/read the pinned plugin commit. Check the repository and host Git credentials.",
        ));
    }
    Ok(Some(result.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwebide_core::plugins::{
        marketplace::MarketplaceSource,
        testing::{review_files, source},
    };

    pub(super) fn command(cwd: &Path, args: &[&str]) -> String {
        let result = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap().trim().into()
    }
    fn seed(root: &Path, owner: &str) -> PluginSource {
        let fixture = tempfile::tempdir().unwrap();
        for file in review_files() {
            let path = fixture.path().join("plugins/pr-review").join(file.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, file.content).unwrap();
        }
        command(fixture.path(), &["init", "-q"]);
        command(fixture.path(), &["add", "."]);
        command(
            fixture.path(),
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.org",
                "commit",
                "-qm",
                "fixture",
            ],
        );
        let mut input = source();
        input.commit = command(fixture.path(), &["rev-parse", "HEAD"]);
        let mut catalog = openwebide_core::plugins::testing::catalog().catalog;
        catalog.plugins[0].releases[0].source.commit = input.commit.clone();
        std::fs::write(
            fixture.path().join("marketplace.json"),
            serde_json::to_vec(&catalog).unwrap(),
        )
        .unwrap();
        command(fixture.path(), &["add", "marketplace.json"]);
        command(
            fixture.path(),
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.org",
                "commit",
                "-qm",
                "catalog",
            ],
        );

        let scoped = ScopedHost {
            cancelled: Arc::new(AtomicBool::new(false)),
            root: root.join(key(owner)),
            host_id: "host".into(),
        };
        let repository = scoped.repository(&input);
        std::fs::create_dir_all(repository.parent().unwrap()).unwrap();
        command(
            root,
            &[
                "clone",
                "--bare",
                fixture.path().to_str().unwrap(),
                repository.to_str().unwrap(),
            ],
        );
        input
    }

    #[tokio::test]
    async fn native_hosts_materialize_pinned_git_objects_atomically_and_reuse_offline() {
        for (owner, id) in [("paired", "local-host"), ("user:1", "remote-host")] {
            let root = tempfile::tempdir().unwrap();
            let source = seed(root.path(), owner);
            let installer = NativePluginInstaller::new(Some(root.path().into()));
            let prepared = installer.prepare(owner, id.into(), &source).await.unwrap();
            assert_eq!(prepared.manifest.name, "pr-review");
            assert_eq!(prepared.host_id, id);
            let snapshot = ScopedHost {
                cancelled: Arc::new(AtomicBool::new(false)),
                root: root.path().join(key(owner)),
                host_id: id.into(),
            }
            .snapshot(&source, &prepared.digest);
            assert_eq!(
                std::fs::read(snapshot.join("plugin.json")).unwrap(),
                review_files()[0].content
            );
            let (first, second) = tokio::join!(
                installer.prepare(owner, id.into(), &source),
                installer.prepare(owner, id.into(), &source)
            );
            assert_eq!(first.unwrap(), prepared);
            assert_eq!(second.unwrap(), prepared);
            let mut wrong_path = source.clone();
            wrong_path.path = "missing".into();
            assert!(
                installer
                    .prepare(owner, id.into(), &wrong_path)
                    .await
                    .is_err()
            );
            assert!(snapshot.join("plugin.json").exists());
        }
    }
    #[tokio::test]
    async fn uncached_commit_is_fetched_from_a_public_git_transport() {
        let root = tempfile::tempdir().unwrap();
        let original = seed(root.path(), "seed");
        let host = ScopedHost {
            cancelled: Arc::new(AtomicBool::new(false)),
            root: root.path().join(key("seed")),
            host_id: "seed".into(),
        };
        let repository = host.repository(&original);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut daemon = Command::new("git")
            .args([
                "daemon",
                "--export-all",
                "--reuseaddr",
                "--listen=127.0.0.1",
                &format!("--port={port}"),
                &format!("--base-path={}", repository.parent().unwrap().display()),
                repository.parent().unwrap().to_str().unwrap(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut ready = false;
        for _ in 0..100 {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(ready);
        let mut source = original;
        source.repository = format!(
            "git://127.0.0.1:{port}/{}",
            repository.file_name().unwrap().to_str().unwrap()
        );
        let installer = NativePluginInstaller::new(Some(root.path().into()));
        let marketplace = MarketplaceSource {
            repository: source.repository.clone(),
            reference: String::new(),
            path: "marketplace.json".into(),
        };
        let catalog = installer.catalog("user:1", &marketplace, 1).await.unwrap();
        let plugin = &catalog.catalog.plugins[0];
        assert_eq!(
            catalog
                .catalog
                .resolve(
                    &marketplace,
                    &plugin.publisher,
                    &plugin.name,
                    &plugin.releases[0].version
                )
                .unwrap(),
            source
        );
        let prepared = installer
            .prepare("user:1", "remote-host".into(), &source)
            .await
            .unwrap();
        daemon.kill().await.unwrap();
        assert_eq!(prepared.manifest.name, "pr-review");
        let package = installer
            .package("user:1", "remote-host".into(), &prepared)
            .await
            .unwrap();
        assert_eq!(package.prepared, prepared);
        assert!(
            package.skills[0]
                .resources
                .iter()
                .any(|r| r.name == "references/review-checks.md")
        );
        assert_eq!(
            installer
                .prepare("user:1", "remote-host".into(), &source)
                .await
                .unwrap(),
            prepared
        );
    }
    #[tokio::test]
    async fn corrupted_cache_is_not_reported_as_ready_or_repaired_in_place() {
        let root = tempfile::tempdir().unwrap();
        let source = seed(root.path(), "paired");
        let installer = NativePluginInstaller::new(Some(root.path().into()));
        let prepared = installer
            .prepare("paired", "host".into(), &source)
            .await
            .unwrap();
        let snapshot = ScopedHost {
            cancelled: Arc::new(AtomicBool::new(false)),
            root: root.path().join(key("paired")),
            host_id: "host".into(),
        }
        .snapshot(&source, &prepared.digest);
        let file = snapshot.join("README.md");
        std::fs::remove_file(&file).unwrap();
        std::fs::write(&file, "corrupted").unwrap();
        assert!(
            installer
                .prepare("paired", "host".into(), &source)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(file).unwrap(), "corrupted");
    }
    #[tokio::test]
    #[ignore = "Smoke test against the published official marketplace; requires network access"]
    async fn official_marketplace_reference_package_installs_and_loads_on_both_host_scopes() {
        let root = tempfile::tempdir().unwrap();
        let installer = NativePluginInstaller::new(Some(root.path().into()));
        for (owner, host) in [("paired", "local-host"), ("user:1", "remote-host")] {
            let source = MarketplaceSource::official();
            let catalog = installer.catalog(owner, &source, 1).await.unwrap();
            let plugin = catalog
                .catalog
                .plugins
                .iter()
                .find(|p| p.publisher == "openwebide" && p.name == "pr-review")
                .unwrap();
            let release = &plugin.releases[0];
            let source = catalog
                .catalog
                .resolve(&source, &plugin.publisher, &plugin.name, &release.version)
                .unwrap();
            let prepared = installer
                .prepare(owner, host.into(), &source)
                .await
                .unwrap();
            assert_eq!(prepared.manifest.version, release.version);
            let package = installer
                .package(owner, host.into(), &prepared)
                .await
                .unwrap();
            package.validate().unwrap();
            assert!(!package.skills.is_empty());
            assert!(!package.skills[0].resources.is_empty());
        }
    }

    #[tokio::test]
    async fn preparation_requires_configured_host_and_rejects_unpinned_requests() {
        let installer = NativePluginInstaller::new(None);
        assert!(matches!(
            installer.prepare("paired", "host".into(), &source()).await,
            Err(PluginError::Host(_))
        ));
        let mut source = source();
        source.commit = "main".into();
        assert!(matches!(
            installer.prepare("paired", "host".into(), &source).await,
            Err(PluginError::Invalid(_))
        ));
    }
}

#[cfg(test)]
mod bundled_tests {
    use super::*;
    #[test]
    fn selected_defaults_supply_source_owned_behavior() {
        for source in openwebide_core::plugins::bundled_plugin_sources() {
            let files = bundled_files(&source).expect("Selected source is embedded");
            let manifest = openwebide_core::plugins::validate_files(&files).unwrap();
            assert!(manifest.executable.is_some());
            assert!(manifest.contributions.tool_groups.is_empty());
            assert!(!manifest.contributions.tools.is_empty());
            assert!(files.iter().any(|file| file.path == "Cargo.lock"));
            assert!(files.iter().any(|file| file.path == "src/lib.rs"));
        }
    }
    #[cfg(feature = "bundled-defaults")]
    #[tokio::test]
    async fn bundled_core_plugins_prepare_offline_on_both_hosts_using_normal_validation() {
        let sources = openwebide_core::plugins::bundled_plugin_sources();
        assert_eq!(sources.len(), 4);
        for host in ["server-host", "paired-local-host"] {
            let root = tempfile::tempdir().unwrap();
            let installer = NativePluginInstaller::new(Some(root.path().to_path_buf()));
            for source in &sources {
                let files = bundled_files(source).unwrap();
                assert!(
                    bundled_component(source, &openwebide_core::plugins::package_digest(&files))
                        .unwrap()
                        .is_some()
                );
                let started = installer
                    .preparations
                    .start(
                        installer.clone(),
                        "owner".into(),
                        host.into(),
                        source.clone(),
                    )
                    .await
                    .unwrap();
                let prepared = tokio::time::timeout(Duration::from_secs(120), async {
                    loop {
                        let status = installer.preparations.status("owner", &started.id).await.unwrap();
                        status.validate().unwrap();
                        match status.state {
                            openwebide_core::plugins::preparation::PreparationState::Ready => {
                                break status.prepared.unwrap();
                            }
                            openwebide_core::plugins::preparation::PreparationState::Queued
                            | openwebide_core::plugins::preparation::PreparationState::Preparing => {
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                            _ => panic!("Bundled source preparation failed: {status:?}"),
                        }
                    }
                }).await.unwrap();
                let package = installer
                    .package("owner", host.into(), &prepared)
                    .await
                    .unwrap();
                assert_eq!(package.prepared, prepared);
                assert_eq!(prepared.host_id, host);
                assert!(prepared.manifest.contributions.tool_groups.is_empty());
                assert_eq!(prepared.manifest.version, "0.2.0");
                assert!(
                    !installer
                        .component("owner", &prepared)
                        .await
                        .unwrap()
                        .is_empty()
                );
                if prepared.manifest.name == "skill-authoring" {
                    assert_eq!(package.skills.len(), 1);
                } else {
                    assert!(package.skills.is_empty());
                }
            }
            assert!(!root.path().join(key("owner")).join("repositories").exists());
        }
        let mut optional = sources[0].clone();
        optional.path = "plugins/pr-review".into();
        assert!(bundled_files(&optional).is_none());
        let mut future = sources[0].clone();
        future.commit = "b".repeat(40);
        assert!(bundled_files(&future).is_none());
    }
}

#[cfg(test)]
mod rust_plugin_tests {
    use super::{tests::command, *};
    use openwebide_core::plugins::execution;
    struct DatabaseHost {
        store: Arc<openwebide_storage::Store<openwebide_storage::rusqlite_db::RusqliteDb>>,
        user: openwebide_core::UserId,
        session: Option<i64>,
        now: i64,
    }
    impl openwebide_agent::plugins::execution::GrantedHost for DatabaseHost {
        async fn request(&self, request: &execution::PluginHostRequest) -> Result<String, String> {
            let result = match self.session {
                Some(session) => {
                    self.store
                        .plugin_host_request(self.user, session, request, self.now)
                        .await
                }
                None => {
                    self.store
                        .plugin_context_host_request(self.user, request, self.now)
                        .await
                }
            };
            result.map_err(|error| error.to_string())
        }
    }
    impl openwebide_agent::plugins::jobs::JobHost for DatabaseHost {
        async fn request(
            &self,
            command: openwebide_core::plugins::jobs::JobServiceRequest,
        ) -> Result<openwebide_core::plugins::jobs::JobServiceResponse, String> {
            use openwebide_core::plugins::jobs::{
                JobServiceRequest as Request, JobServiceResponse as Response,
            };
            let db = DatabaseHost {
                store: self.store.clone(),
                user: self.user,
                session: None,
                now: self.now,
            };
            let result: Result<Response, openwebide_storage::StorageError> =
                tokio::task::spawn_blocking(move || {
                    futures::executor::block_on(async {
                        Ok(match command {
                            Request::Renew { delivery } => Response::Renewed(
                                db.store
                                    .renew_plugin_job(
                                        &delivery.host_id,
                                        delivery.id,
                                        &delivery.lease,
                                        db.now,
                                    )
                                    .await?,
                            ),
                            Request::Grant {
                                user_id: _,
                                delivery,
                            } => {
                                let grant = format!("{:032x}", delivery.id);
                                let (prepared, context) = db
                                    .store
                                    .issue_plugin_job_grant(
                                        db.user,
                                        &delivery.host_id,
                                        delivery.id,
                                        &delivery.lease,
                                        &grant,
                                        db.now,
                                    )
                                    .await?;
                                Response::Authorized {
                                    prepared: Box::new(prepared),
                                    context,
                                    grant,
                                }
                            }
                            Request::Callback {
                                user_id: _,
                                request,
                            } => Response::Callback(
                                db.store
                                    .plugin_context_host_request(db.user, &request, db.now)
                                    .await?,
                            ),
                            Request::Finish {
                                delivery,
                                success,
                                detail,
                            } => {
                                db.store
                                    .finish_plugin_job(
                                        &delivery.host_id,
                                        delivery.id,
                                        &delivery.lease,
                                        success,
                                        &detail,
                                        db.now,
                                    )
                                    .await?;
                                Response::Finished
                            }
                            Request::Claim { .. } => panic!("fixture already claimed its job"),
                        })
                    })
                })
                .await
                .map_err(|error| error.to_string())?;
            result.map_err(|error| error.to_string())
        }
        fn now(&self) -> i64 {
            self.now
        }
        async fn wait(&self, duration: Duration) {
            tokio::time::sleep(duration).await;
        }
    }
    struct NoBuiltin;
    impl openwebide_agent::ToolExecutor for NoBuiltin {
        fn describe(&self, call: &openwebide_core::ToolCall) -> String {
            call.name.clone()
        }
        async fn execute(&self, _: &openwebide_core::ToolCall) -> openwebide_agent::ToolOutcome {
            panic!("Plugin tool must not fall through to a built-in executor")
        }
    }
    async fn exercise_records(
        installer: &NativePluginInstaller,
        owner: &str,
        prepared: &PreparedPlugin,
    ) {
        use openwebide_agent::{
            ToolExecutor,
            plugins::execution::{GrantedServices, PluginTools},
        };
        use openwebide_core::{
            NewProject, UserRole, WorkspaceMode,
            plugins::{PluginPackage, RecordPlugin},
        };
        let store = Arc::new(openwebide_storage::Store::new(
            openwebide_storage::rusqlite_db::RusqliteDb::open_in_memory().unwrap(),
        ));
        store.migrate().await.unwrap();
        let user = store
            .insert_user("owner", "hash", UserRole::Admin, 0)
            .await
            .unwrap()
            .id;
        let mode = if owner == "paired" {
            WorkspaceMode::Local
        } else {
            WorkspaceMode::Remote
        };
        let project = store
            .create_project(
                &NewProject {
                    name: "project".into(),
                    mode,
                    path: Some("p".into()),
                },
                user,
                0,
            )
            .await
            .unwrap()
            .id;
        let session = store
            .create_session("session", None, None, Some(project), user, 0)
            .await
            .unwrap()
            .id;
        store
            .record_plugin(
                user,
                &RecordPlugin {
                    approved_capabilities: Vec::new(),
                    prepared: prepared.clone(),
                    revision: None,
                    update_policy: None,
                    package: Some(Box::new(PluginPackage {
                        prepared: prepared.clone(),
                        skills: Vec::new(),
                    })),
                },
                1,
            )
            .await
            .unwrap();
        let token = "d".repeat(32);
        store
            .issue_plugin_grant(user, session, prepared, &token, 2)
            .await
            .unwrap();
        let executor = PluginTools {
            executor: NoBuiltin,
            transport: transport::NativePluginTransport {
                installer: installer.clone(),
                invocations: invocations::Invocations::default(),
                owner: owner.into(),
            },
            services: GrantedServices {
                host: DatabaseHost {
                    store: store.clone(),
                    user,
                    session: Some(session),
                    now: 10,
                },
                grants: Arc::new([(prepared.digest.clone(), token)].into_iter().collect()),
            },
            plugins: Arc::new(vec![prepared.clone()]),
        };
        let call = openwebide_core::ToolCall {id:"fixture".into(), name:"fixture_echo".into(),
            arguments:serde_json::json!({"collection":"notes","operation":{"action":"create","value":{"text":"plugin-owned state"}}}).to_string()};
        let outcome = executor.execute(&call).await;
        assert!(outcome.ok, "{}", outcome.content);
        let result: serde_json::Value = serde_json::from_str(&outcome.content).unwrap();
        assert_eq!(result["records"][0]["value"]["text"], "plugin-owned state");
        let id = result["records"][0]["id"].as_i64().unwrap();
        let read = openwebide_core::ToolCall {
            arguments:
                serde_json::json!({"collection":"notes","operation":{"action":"read","id":id}})
                    .to_string(),
            ..call
        };
        let outcome = executor.execute(&read).await;
        assert!(outcome.ok);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&outcome.content).unwrap(),
            result
        );
        // Raw run requests execute inside source code through normal callback grants.
        let raw = executor.execute(&openwebide_core::ToolCall {
            id: "raw-run".into(), name: "fixture_echo".into(),
            arguments: serde_json::json!({"runs":{"action":"submit","key":"raw-one","prompt":"Source-owned prompt","target":{"kind":"origin"},"completion":{"event":"job_due","payload":{"text":"raw completion callback"}}}}).to_string(),
        }).await;
        assert!(raw.ok, "{}", raw.content);
        let raw: openwebide_core::plugins::runs::RunResult =
            serde_json::from_str(&raw.content).unwrap();
        assert_eq!(
            raw.runs[0].state,
            openwebide_core::plugins::runs::RunState::Pending
        );
        assert_eq!(raw.runs[0].session_id, Some(session));
        let pending = store
            .list_queued_prompts(user, session)
            .await
            .unwrap()
            .remove(0);
        assert_eq!(pending.plugin_run, Some(raw.runs[0].id));
        assert!(
            store
                .consume_queued_prompt(user, session, pending.key(), &pending.content, 3)
                .await
                .is_err()
        );
        let cancelled = executor.execute(&openwebide_core::ToolCall {
            id: "raw-cancel".into(), name: "fixture_echo".into(),
            arguments: serde_json::json!({"runs":{"action":"cancel","id":raw.runs[0].id,"revision":raw.runs[0].revision}}).to_string(),
        }).await;
        assert!(cancelled.ok, "{}", cancelled.content);
        assert!(
            store
                .list_queued_prompts(user, session)
                .await
                .unwrap()
                .is_empty()
        );
        // A real compiled SDK submission reserves and releases its declared completion event.
        let completion = store
            .claim_plugin_jobs(&prepared.host_id, 0, &"d".repeat(32), 20)
            .await
            .unwrap()
            .jobs
            .remove(0);
        assert_eq!(completion.job.payload["run"]["id"], raw.runs[0].id);
        assert_eq!(completion.job.payload["run"]["state"], "cancelled");
        assert_eq!(
            completion.job.payload["data"]["text"],
            "raw completion callback"
        );
        openwebide_agent::plugins::jobs::deliver(
            &DatabaseHost {
                store: store.clone(),
                user,
                session: None,
                now: 21,
            },
            &executor.transport,
            completion,
        )
        .await
        .unwrap();
        let conversations = executor.execute(&openwebide_core::ToolCall {
            id:"discover".into(),name:"fixture_echo".into(),
            arguments:serde_json::json!({"collections":{"collection":"conversations","operation":{"action":"list"}}}).to_string(),
        }).await;
        assert!(conversations.ok, "{}", conversations.content);
        let metadata: openwebide_core::plugins::records::CollectionResult =
            serde_json::from_str(&conversations.content).unwrap();
        assert_eq!(metadata.records.len(), 1);
        assert_eq!(metadata.records[0].id, session);
        assert_eq!(metadata.records[0].value["origin"], true);
        let task = executor.execute(&openwebide_core::ToolCall {
            id:"task-record".into(),name:"fixture_echo".into(),
            arguments:serde_json::json!({"collections":{"collection":"tasks","operation":{"action":"create","value":{
                "draft":{"title":"Plugin-owned task","prompt":"Source chooses the prompt","session_id":session,"session_target":"existing","schedule":{"kind":"once","at":100},"enabled":true},
                "next_run":111,"state":{"proof":"SDK task state"},"monitor":null
            }}}}).to_string(),
        }).await;
        assert!(task.ok, "{}", task.content);
        let task: openwebide_core::plugins::records::CollectionResult =
            serde_json::from_str(&task.content).unwrap();
        assert_eq!(task.records[0].value["owner"], prepared.storage_namespace());
        let visible = store.scheduled_tasks(user, Some(project), 3).await.unwrap();
        assert_eq!(visible[0].id, task.records[0].id);
        assert_eq!(visible[0].next_run, Some(111));
        let stale_submission = serde_json::json!({"runs":{"action":"submit","key":"guarded-task","prompt":"Only queue current tasks","target":{"kind":"origin"},"prerequisites":[{"capability":"collections","collection":"tasks","id":task.records[0].id,"revision":task.records[0].revision + 1}]}});
        let stale = executor
            .execute(&openwebide_core::ToolCall {
                id: "task-stale".into(),
                name: "fixture_echo".into(),
                arguments: stale_submission.to_string(),
            })
            .await;
        assert!(!stale.ok);
        let removed=executor.execute(&openwebide_core::ToolCall {
            id:"task-delete".into(),name:"fixture_echo".into(),
            arguments:serde_json::json!({"collections":{"collection":"tasks","operation":{"action":"delete","id":task.records[0].id,"revision":task.records[0].revision}}}).to_string(),
        }).await;
        assert!(removed.ok, "{}", removed.content);
        let mut deleted_submission = stale_submission;
        deleted_submission["runs"]["prerequisites"][0]["revision"] =
            serde_json::json!(task.records[0].revision);
        let deleted = executor
            .execute(&openwebide_core::ToolCall {
                id: "task-deleted".into(),
                name: "fixture_echo".into(),
                arguments: deleted_submission.to_string(),
            })
            .await;
        assert!(!deleted.ok);
        // Public SDK code schedules a real durable event; delivery uses the same executor.
        let scheduled = executor.execute(&openwebide_core::ToolCall {
            id:"schedule".into(), name:"fixture_echo".into(),
            arguments:serde_json::json!({"jobs":{"action":"schedule","key":"event-one","scope":"project","due_at":20,"expires_at":null,"event":"job_due","payload":{"text":"durable plugin event","discover":true}}}).to_string(),
        }).await;
        assert!(scheduled.ok, "{}", scheduled.content);
        let queued: openwebide_core::plugins::jobs::JobResult =
            serde_json::from_str(&scheduled.content).unwrap();
        let delivery = store
            .claim_plugin_jobs(&prepared.host_id, 0, &"e".repeat(32), 20)
            .await
            .unwrap()
            .jobs
            .remove(0);
        assert_eq!(delivery.job.id, queued.jobs[0].id);
        assert_eq!(delivery.context.session_id, None);
        openwebide_agent::plugins::jobs::deliver(
            &DatabaseHost {
                store: store.clone(),
                user,
                session: None,
                now: 21,
            },
            &executor.transport,
            delivery,
        )
        .await
        .unwrap();
        let completed = executor
            .execute(&openwebide_core::ToolCall {
                id: "job-state".into(),
                name: "fixture_echo".into(),
                arguments: serde_json::json!({"jobs":{"action":"read","id":queued.jobs[0].id}})
                    .to_string(),
            })
            .await;
        assert!(completed.ok, "{}", completed.content);
        let state: openwebide_core::plugins::jobs::JobResult =
            serde_json::from_str(&completed.content).unwrap();
        assert_eq!(
            state.jobs[0].state,
            openwebide_core::plugins::jobs::JobState::Completed
        );
        let events = executor
            .execute(&openwebide_core::ToolCall {
                id: "event-effect".into(),
                name: "fixture_echo".into(),
                arguments: serde_json::json!({"collection":"events","operation":{"action":"list"}})
                    .to_string(),
            })
            .await;
        assert!(events.ok, "{}", events.content);
        assert!(events.content.contains("durable plugin event"));
        assert!(events.content.contains("raw completion callback"));
        let effects: serde_json::Value = serde_json::from_str(&events.content).unwrap();
        let project_event = effects["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["value"]["payload"]["discover"] == true)
            .unwrap();
        assert_eq!(
            project_event["value"]["conversations"]["records"][0]["value"]["origin"],
            false
        );
        let grant = format!("{:032x}", queued.jobs[0].id);
        let callback = execution::PluginHostRequest {
            grant,
            capability: "records".into(),
            payload: serde_json::json!({"collection":"events","operation":{"action":"list"}})
                .to_string(),
        };
        assert!(
            store
                .plugin_context_host_request(user, &callback, 23)
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn source_compilation_validates_tools_caches_offline_and_rejects_artifact_corruption() {
        let root = tempfile::tempdir().unwrap();
        let manifest: openwebide_core::plugins::PluginManifest = serde_json::from_value(serde_json::json!({
            "schemaVersion":1,"publisher":"example","name":"fixture","version":"0.1.0",
            "displayName":"Fixture","description":"SDK adapter contract","license":"MIT",
            "compatibility":{"pluginApi":3},
            "executable":{"manifest":"Cargo.toml","library":"sdk_fixture","sdkVersion":"0.1.0","capabilities":["records","jobs","runs","collections"]},
            "contributions":{"skills":[],"events":["job_due"],"tools":[{"name":"fixture_echo","description":"Exercise the public host contract.","parameters":{"type":"object","properties":{}},"requires_approval":false}]}
        })).unwrap();
        let cargo = include_str!("../../crates/plugin-sdk/examples/fixture/Cargo.toml")
            .replace("{ path = \"../..\" }", "\"=0.1.0\"");
        let files = vec![
            ("plugin.json", serde_json::to_string(&manifest).unwrap()),
            ("Cargo.toml", cargo),
            (
                "Cargo.lock",
                include_str!("../../crates/plugin-sdk/examples/fixture/Cargo.lock").into(),
            ),
            (
                "src/lib.rs",
                include_str!("../../crates/plugin-sdk/examples/fixture/src/lib.rs").into(),
            ),
        ]
        .into_iter()
        .map(|(path, content)| PackageFile {
            path: path.into(),
            content: content.into_bytes(),
            kind: PackageFileKind::File,
        })
        .collect::<Vec<_>>();
        openwebide_core::plugins::validate_files(&files).unwrap();
        let repository = tempfile::tempdir().unwrap();
        for file in &files {
            let path = repository.path().join(&file.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, &file.content).unwrap();
        }
        command(repository.path(), &["init", "-q"]);
        command(repository.path(), &["add", "."]);
        command(
            repository.path(),
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.org",
                "commit",
                "-qm",
                "Rust source fixture",
            ],
        );
        let commit = command(repository.path(), &["rev-parse", "HEAD"]);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut daemon = Command::new("git")
            .args([
                "daemon",
                "--export-all",
                "--reuseaddr",
                "--listen=127.0.0.1",
                &format!("--port={port}"),
                &format!("--base-path={}", repository.path().display()),
                repository.path().to_str().unwrap(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut ready = false;
        for _ in 0..100 {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(ready, "public Git source fixture did not start");
        let source = PluginSource {
            repository: format!("git://127.0.0.1:{port}/"),
            commit,
            path: ".".into(),
        };
        let digest = openwebide_core::plugins::package_digest(&files);
        let installer = NativePluginInstaller::new(Some(root.path().to_path_buf()));
        for host_id in ["server-host", "paired-local-host"] {
            let owner = if host_id == "server-host" {
                "user:1"
            } else {
                "paired"
            };
            let host = ScopedHost {
                cancelled: Arc::new(AtomicBool::new(false)),
                root: root.path().join(key(owner)),
                host_id: host_id.into(),
            };
            assert!(!host.artifact(&source, &digest).exists());
            let prepared = PreparedPlugin {
                source: source.clone(),
                manifest: manifest.clone(),
                digest: digest.clone(),
                host_id: host_id.into(),
            };
            exercise_records(&installer, owner, &prepared).await;
            // The shared run workflow fetched source and compiled this previously cold host.
            let bytes = load_artifact(&host.artifact(&source, &digest)).unwrap();
            assert!(!bytes.is_empty());
            assert_eq!(
                installer
                    .prepare(owner, host_id.into(), &source)
                    .await
                    .unwrap(),
                prepared
            );

            let invocations = invocations::Invocations::default();
            let event = invocations
                .start(
                    &installer,
                    owner,
                    execution::InvokePlugin {
                        operation: execution::PluginOperation::Event,
                        prepared: prepared.clone(),
                        name: String::new(),
                        arguments: serde_json::json!({"name":"job_due","payload":{"job_id":7}})
                            .to_string(),
                    },
                )
                .await
                .unwrap();
            let event_import = invocations
                .resume(
                    owner,
                    execution::ContinuePlugin {
                        id: event.id.clone(),
                        sequence: 0,
                        response: Ok(String::new()),
                    },
                )
                .await
                .unwrap();
            let execution::PluginStep::HostCall {
                capability,
                payload,
                ..
            } = event_import.step
            else {
                panic!("event did not execute plugin code")
            };
            assert_eq!(capability, "records");
            let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(value["operation"]["value"]["event"], "job_due");
            assert_eq!(value["operation"]["value"]["payload"]["job_id"], 7);
            let event_result = invocations
                .resume(
                    owner,
                    execution::ContinuePlugin {
                        id: event.id,
                        sequence: 1,
                        response: Ok("{\"stored\":true}".into()),
                    },
                )
                .await
                .unwrap();
            assert!(matches!(
                event_result.step,
                execution::PluginStep::Complete { ok: true, .. }
            ));
            let context = invocations
                .start(
                    &installer,
                    owner,
                    execution::InvokePlugin {
                        operation: execution::PluginOperation::Context,
                        prepared: prepared.clone(),
                        name: String::new(),
                        arguments: "{\"budget_bytes\":64}".into(),
                    },
                )
                .await
                .unwrap();
            let context_read = invocations
                .resume(
                    owner,
                    execution::ContinuePlugin {
                        id: context.id.clone(),
                        sequence: 0,
                        response: Ok(String::new()),
                    },
                )
                .await
                .unwrap();
            let execution::PluginStep::HostCall {
                capability,
                payload,
                ..
            } = context_read.step
            else {
                panic!("context did not request its general record read");
            };
            assert_eq!(capability, "records");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&payload).unwrap()["operation"]["action"],
                "list"
            );
            let complete = invocations
                .resume(
                    owner,
                    execution::ContinuePlugin {
                        id: context.id,
                        sequence: 1,
                        response: Ok("{}".into()),
                    },
                )
                .await
                .unwrap();
            let execution::PluginStep::Complete {
                content, ok: true, ..
            } = complete.step
            else {
                panic!("context did not complete");
            };
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&content).unwrap()["prompt"],
                "Stored fact"
            );
            let mutation = invocations
                .start(
                    &installer,
                    owner,
                    execution::InvokePlugin {
                        operation: execution::PluginOperation::Context,
                        prepared: prepared.clone(),
                        name: String::new(),
                        arguments: "{\"budget_bytes\":1}".into(),
                    },
                )
                .await
                .unwrap();
            let failed = invocations
                .resume(
                    owner,
                    execution::ContinuePlugin {
                        id: mutation.id,
                        sequence: 0,
                        response: Ok(String::new()),
                    },
                )
                .await
                .unwrap();
            assert!(matches!(failed.step, execution::PluginStep::Failed { .. }));
            let ready = invocations
                .start(
                    &installer,
                    owner,
                    execution::InvokePlugin {
                        operation: execution::PluginOperation::Tool,
                        prepared: prepared.clone(),
                        name: "fixture_echo".into(),
                        arguments: "{}".into(),
                    },
                )
                .await
                .unwrap();
            assert!(matches!(ready.step, execution::PluginStep::Ready));
            assert!(
                invocations
                    .cancel("another-owner", &ready.id)
                    .await
                    .is_err()
            );
            let callback = invocations
                .resume(
                    owner,
                    execution::ContinuePlugin {
                        id: ready.id.clone(),
                        sequence: 0,
                        response: Ok(String::new()),
                    },
                )
                .await
                .unwrap();
            assert!(matches!(
                callback.step,
                execution::PluginStep::HostCall { sequence: 1, .. }
            ));
            let response = execution::ContinuePlugin {
                id: ready.id.clone(),
                sequence: 1,
                response: Ok("{\"value\":42}".into()),
            };
            assert!(
                invocations
                    .resume("another-owner", response.clone())
                    .await
                    .is_err()
            );
            let stale = execution::ContinuePlugin {
                sequence: 0,
                ..response.clone()
            };
            assert!(invocations.resume(owner, stale).await.is_err());
            let result = invocations.resume(owner, response.clone()).await.unwrap();
            assert!(matches!(
                result.step,
                execution::PluginStep::Complete { ok: true, .. }
            ));
            assert!(invocations.resume(owner, response).await.is_err());
            let ready = invocations
                .start(
                    &installer,
                    owner,
                    execution::InvokePlugin {
                        operation: execution::PluginOperation::Tool,
                        prepared,
                        name: "fixture_echo".into(),
                        arguments: "{}".into(),
                    },
                )
                .await
                .unwrap();
            let callback = invocations
                .resume(
                    owner,
                    execution::ContinuePlugin {
                        id: ready.id.clone(),
                        sequence: 0,
                        response: Ok(String::new()),
                    },
                )
                .await
                .unwrap();
            assert!(matches!(
                callback.step,
                execution::PluginStep::HostCall { sequence: 1, .. }
            ));
            invocations.cancel(owner, &ready.id).await.unwrap();
            assert!(
                invocations
                    .resume(
                        owner,
                        execution::ContinuePlugin {
                            id: ready.id,
                            sequence: 1,
                            response: Ok("{}".into()),
                        }
                    )
                    .await
                    .is_err()
            );
        }
        daemon.kill().await.unwrap();
        // Repeat preparation/execution with the source endpoint offline.
        for (owner, host_id) in [("user:1", "server-host"), ("paired", "paired-local-host")] {
            let prepared = installer
                .prepare(owner, host_id.into(), &source)
                .await
                .unwrap();
            exercise_records(&installer, owner, &prepared).await;
        }
        let host = ScopedHost {
            cancelled: Arc::new(AtomicBool::new(false)),
            root: root.path().join(key("user:1")),
            host_id: "server-host".into(),
        };
        std::fs::write(
            host.artifact(&source, &digest).join("plugin.wasm"),
            b"corrupt",
        )
        .unwrap();
        assert!(host.compile(&source, &digest, &manifest).await.is_err());
    }
}
