//! Compile pinned source without executing package build code in the host context.
use anyhow::{Context, Result, bail};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

pub const TOOLCHAIN: &str = "1.98.1";
pub const TARGET: &str = "wasm32-wasip2";
pub const WASI_SDK_VERSION: &str = "34.0";
const SDK_MANIFEST: &str = include_str!("../../plugin-sdk/Cargo.toml");
const SDK_SOURCE: &str = include_str!("../../plugin-sdk/src/lib.rs");
const SDK_WIT: &str = include_str!("../../plugin-sdk/wit/plugin.wit");
const SDK_PANELS: &str = include_str!("../../plugin-sdk/src/panels.rs");
const SDK_LANGUAGES: &str = include_str!("../../plugin-sdk/src/languages.rs");

pub fn sdk_digest() -> String {
    use sha2::{Digest, Sha256};
    let bytes = Sha256::digest(format!(
        "{SDK_MANIFEST}\n{SDK_SOURCE}\n{SDK_WIT}\n{SDK_PANELS}\n{SDK_LANGUAGES}\n{WASI_SDK_VERSION}"
    ));
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The source directory must already be a verified immutable package snapshot.
/// Outputs are kept separate from the snapshot and published only after validation.
pub fn compile(source: &Path, output_name: &str, staging: &Path) -> Result<Vec<u8>> {
    compile_cancellable(source, output_name, staging, &AtomicBool::new(false))
}

/// Cancellation stops dependency retrieval and compilation, including descendants.
/// The caller retains the flag while awaiting the blocking compiler task.
pub fn compile_cancellable(
    source: &Path,
    output_name: &str,
    staging: &Path,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>> {
    check_cancelled(cancelled)?;
    if output_name.is_empty()
        || !output_name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        bail!("Invalid Rust library name");
    }
    if !source.join("Cargo.lock").is_file() || !source.join("Cargo.toml").is_file() {
        bail!("Rust plugins require Cargo.toml and a committed Cargo.lock");
    }
    let private = container_staging()?;
    let staging = private
        .as_ref()
        .map_or(staging, tempfile::TempDir::path)
        .canonicalize()?;
    let original = source.canonicalize()?;
    let source = compiler_source(&original, &staging)?;
    let work = staging.join("work");
    std::fs::create_dir_all(&work)?;
    let sdk = staging.join("sdk");
    for (path, contents) in [
        ("Cargo.toml", SDK_MANIFEST),
        ("src/lib.rs", SDK_SOURCE),
        ("src/panels.rs", SDK_PANELS),
        ("src/languages.rs", SDK_LANGUAGES),
        ("wit/plugin.wit", SDK_WIT),
    ] {
        let path = sdk.join(path);
        std::fs::create_dir_all(path.parent().expect("SDK parent"))?;
        std::fs::write(path, contents.replace("\n[lints]\nworkspace = true\n", ""))?;
    }
    #[cfg(target_os = "linux")]
    // SAFETY: geteuid has no arguments and does not mutate process state.
    if unsafe { libc::geteuid() } == 0 {
        // Account caches and the daemon's umask can make files owner-only. The
        // dropped compiler identity reads only these copied public inputs;
        // preserve the private cache's permissions and ownership.
        read_only_input(&source)?;
        read_only_input(&sdk)?;
    }
    let cargo_home = work.join("cargo-home");
    std::fs::create_dir_all(&cargo_home)?;
    let rustup_home = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".rustup")))
        .context("Configure the host Rust toolchain")?
        .canonicalize()?;
    let cargo = tool(&rustup_home, "cargo")?;
    let rustc = tool(&rustup_home, "rustc")?;
    let wasi = wasi_sdk()?;
    let patch = format!(
        "patch.crates-io.openwebide-plugin-sdk.path={:?}",
        sdk.to_string_lossy()
    );
    let common = ["--locked", "--manifest-path"];
    // Fetch performs no package build-script execution. Cargo config is read from
    // this trusted staging cwd, not from the plugin's working directory. Fetch
    // the complete lockfile: filtering to the WASI target can omit native
    // dependencies needed by proc macros and build scripts during cross builds.
    let mut fetch = sandbox(
        &cargo,
        &source,
        &work,
        &rustup_home,
        &sdk,
        true,
        wasi.as_deref(),
    )?;
    configure(
        &mut fetch,
        &rustc,
        &rustup_home,
        &cargo_home,
        &work,
        wasi.as_deref(),
    );
    fetch
        .arg("fetch")
        .args(common)
        .arg(source.join("Cargo.toml"))
        .args(["--config", &patch]);
    run_cancellable(fetch, Duration::from_secs(180), cancelled)
        .context("Fetch Rust plugin dependencies")?;
    let mut build = sandbox(
        &cargo,
        &source,
        &work,
        &rustup_home,
        &sdk,
        false,
        wasi.as_deref(),
    )?;
    configure(
        &mut build,
        &rustc,
        &rustup_home,
        &cargo_home,
        &work,
        wasi.as_deref(),
    );
    build
        .args(["build", "--release", "--offline"])
        .args(common)
        .arg(source.join("Cargo.toml"))
        .args(["--target", TARGET, "--config", &patch]);
    run_cancellable(build, Duration::from_secs(600), cancelled)
        .context("Compile Rust plugin in sandbox")?;
    check_cancelled(cancelled)?;
    let output = work
        .join("target")
        .join(TARGET)
        .join("release")
        .join(format!("{output_name}.wasm"));
    let metadata = std::fs::metadata(&output)
        .context("Rust plugin did not produce the declared WASM library")?;
    if metadata.len() > 32 * 1024 * 1024 {
        bail!("Compiled plugin exceeds artifact limit");
    }
    Ok(std::fs::read(output)?)
}
fn tool(rustup_home: &Path, name: &str) -> Result<PathBuf> {
    let output = Command::new("rustup")
        .args(["which", "--toolchain", TOOLCHAIN, name])
        .env("RUSTUP_HOME", rustup_home)
        .output()?;
    if !output.status.success() {
        bail!("Install Rust {TOOLCHAIN} with the {TARGET} target on the execution host");
    }
    Ok(PathBuf::from(std::str::from_utf8(&output.stdout)?.trim()).canonicalize()?)
}
fn configure(
    command: &mut Command,
    rustc: &Path,
    rustup: &Path,
    cargo_home: &Path,
    staging: &Path,
    wasi: Option<&Path>,
) {
    command
        .env_clear()
        .current_dir(staging)
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", staging)
        .env("TMPDIR", staging)
        .env("RUSTUP_HOME", rustup)
        .env("CARGO_HOME", cargo_home)
        .env("RUSTC", rustc)
        .env("CARGO_TARGET_DIR", staging.join("target"));
    if let Some(wasi) = wasi {
        command
            .env("WASI_SDK_PATH", wasi)
            .env("WASI_SYSROOT", wasi.join("share/wasi-sysroot"));
    }
}

fn wasi_sdk() -> Result<Option<PathBuf>> {
    std::env::var_os("WASI_SDK_PATH")
        .map(|path| {
            let path = PathBuf::from(path)
                .canonicalize()
                .context("Locate the configured WASI SDK")?;
            let version = std::fs::read_to_string(path.join("VERSION"))
                .context("Read the WASI SDK version")?;
            if version.lines().next() != Some(WASI_SDK_VERSION)
                || !path.join("bin/clang").is_file()
                || !path
                    .join("share/wasi-sysroot/include/wasm32-wasip2/stdio.h")
                    .is_file()
            {
                bail!("Configure WASI_SDK_PATH with the complete WASI SDK {WASI_SDK_VERSION}");
            }
            Ok(path)
        })
        .transpose()
}
#[cfg(target_os = "macos")]
fn sandbox(
    cargo: &Path,
    source: &Path,
    staging: &Path,
    rustup: &Path,
    sdk: &Path,
    network: bool,
    wasi: Option<&Path>,
) -> Result<Command> {
    use std::os::unix::fs::MetadataExt;

    // Child processes inherit this profile, including build.rs and procedural macros.
    let quote = |path: &Path| serde_json::to_string(&path.to_string_lossy()).expect("path JSON");
    if !Path::new("/usr/bin/sandbox-exec").is_file() {
        bail!("Install the host build sandbox");
    }
    // xcrun may use a full Xcode installation under /Applications rather than
    // Command Line Tools under /Library. Permit only the selected installation,
    // including its shared frameworks, without exposing unrelated applications.
    let selected = Command::new("/usr/bin/xcode-select")
        .arg("--print-path")
        .output()?;
    if !selected.status.success() {
        bail!("Install and select the host's Apple developer tools");
    }
    let developer = PathBuf::from(std::str::from_utf8(&selected.stdout)?.trim())
        .canonicalize()
        .context("Locate the selected Apple developer tools")?;
    let developer = if developer
        .file_name()
        .is_some_and(|name| name == "Developer")
        && developer
            .parent()
            .is_some_and(|parent| parent.file_name().is_some_and(|name| name == "Contents"))
    {
        developer.parent().expect("selected Xcode contents")
    } else {
        developer.as_path()
    };
    let mut extra_read = wasi
        .map(|path| format!("(subpath {})", quote(path)))
        .unwrap_or_default();
    // Seatbelt checks the resolved vnode. /etc/resolv.conf is normally a
    // system-owned link into /private/var/run, outside the general reads.
    // Add only this trusted file during fetch. A missing or unusual resolver
    // grants no extra read; it need not prevent cached or system-resolver fetches.
    if network
        && let Ok(resolver) = Path::new("/etc/resolv.conf").canonicalize()
        && matches!(
            resolver.to_str(),
            Some("/private/etc/resolv.conf" | "/private/var/run/resolv.conf")
        )
        && let Ok(metadata) = std::fs::metadata(&resolver)
        && metadata.is_file()
        && metadata.uid() == 0
        && metadata.mode() & 0o022 == 0
    {
        extra_read.push_str(&format!("(literal {})", quote(&resolver)));
    }
    let profile = format!(
        "(version 1)(deny default)(allow process*)(allow file-read-metadata)(allow sysctl-read)(allow mach-lookup)(allow file-read* (literal \"/\") (literal \"/private/etc/ssl/openssl.cnf\") (literal \"/private/etc/ssl/cert.pem\") (literal \"/private/etc/hosts\") (subpath \"/System\") (subpath \"/usr\") (subpath \"/Library\") (subpath \"/bin\") (subpath \"/dev\") (subpath {}) (subpath {}) (subpath {}) (subpath {}) (subpath {}){})(allow file-write* (subpath {}) (literal \"/dev/null\")){}",
        quote(source),
        quote(staging),
        quote(rustup),
        quote(sdk),
        quote(developer),
        extra_read,
        quote(staging),
        if network { "(allow network*)" } else { "" }
    );
    let mut result = Command::new("/usr/bin/sandbox-exec");
    result.args(["-p", &profile]).arg(cargo);
    Ok(result)
}
#[cfg(target_os = "linux")]
fn sandbox(
    cargo: &Path,
    source: &Path,
    staging: &Path,
    rustup: &Path,
    sdk: &Path,
    network: bool,
    wasi: Option<&Path>,
) -> Result<Command> {
    // SAFETY: geteuid has no arguments and does not mutate process state.
    if unsafe { libc::geteuid() } == 0 {
        let executable = std::env::current_exe()?;
        let directory = executable.parent().context("Compiler launcher directory")?;
        let directory = if directory
            .file_name()
            .is_some_and(|name| name == "deps" || name == "examples")
        {
            directory.parent().context("Compiler launcher parent")?
        } else {
            directory
        };
        let helper = std::env::var_os("OPENWEBIDE_PLUGIN_BUILD_HELPER")
            .map(PathBuf::from)
            .unwrap_or_else(|| directory.join("openwebide-plugin-build"));
        if !helper.is_file() {
            bail!("Install the Linux container compiler launcher alongside the execution host");
        }
        let mut result = Command::new(helper);
        result
            .arg(if network { "fetch" } else { "build" })
            .arg(source)
            .arg(staging)
            .arg(sdk)
            .arg(rustup)
            .arg(cargo);
        return Ok(result);
    }
    if !Path::new("/usr/bin/bwrap").is_file() {
        bail!("Install bubblewrap for isolated plugin builds");
    }
    let mut result = Command::new("/usr/bin/bwrap");
    result.args([
        "--die-with-parent",
        "--unshare-all",
        "--new-session",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
    ]);
    if network {
        result.arg("--share-net");
    }
    // Distribution compiler/ld symlinks commonly resolve through this public
    // system directory (for example /usr/bin/cc -> /etc/alternatives/cc).
    for path in ["/usr", "/bin", "/lib", "/lib64", "/etc/alternatives"] {
        if Path::new(path).exists() {
            result.args(["--ro-bind", path, path]);
        }
    }
    result
        .arg("--ro-bind")
        .arg(source)
        .arg(source)
        .arg("--ro-bind")
        .arg(rustup)
        .arg(rustup)
        .arg("--ro-bind")
        .arg(sdk)
        .arg(sdk);
    if let Some(wasi) = wasi {
        result.arg("--ro-bind").arg(wasi).arg(wasi);
    }
    // Fetch may use TLS and DNS, without granting access to private host files.
    if network {
        for path in [
            "/etc/ssl/certs",
            "/etc/pki/tls",
            "/etc/pki/ca-trust",
            "/etc/resolv.conf",
            "/etc/hosts",
            "/etc/nsswitch.conf",
            "/etc/ld.so.cache",
        ] {
            if Path::new(path).exists() {
                result.args(["--ro-bind", path, path]);
            }
        }
    }
    result
        .arg("--bind")
        .arg(staging)
        .arg(staging)
        .arg("--chdir")
        .arg(staging)
        .arg(cargo);
    Ok(result)
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn sandbox(
    _cargo: &Path,
    _source: &Path,
    _staging: &Path,
    _rustup: &Path,
    _sdk: &Path,
    _network: bool,
    _wasi: Option<&Path>,
) -> Result<Command> {
    bail!("Isolated Rust plugin builds are not available on this host platform")
}

#[cfg_attr(
    not(target_os = "linux"),
    allow(
        clippy::unnecessary_wraps,
        reason = "Linux container preparation performs fallible filesystem operations"
    )
)]
fn compiler_source(original: &Path, staging: &Path) -> Result<PathBuf> {
    #[cfg(target_os = "linux")]
    // SAFETY: geteuid has no arguments and does not mutate process state.
    if unsafe { libc::geteuid() } == 0 {
        let source = staging.join("source");
        copy_source(original, &source, &mut 0, &mut 0)?;
        return Ok(source);
    }
    let _ = staging;
    Ok(original.into())
}

#[cfg_attr(
    not(target_os = "linux"),
    allow(
        clippy::unnecessary_wraps,
        reason = "Linux container preparation creates a temporary directory"
    )
)]
fn container_staging() -> Result<Option<tempfile::TempDir>> {
    #[cfg(target_os = "linux")]
    // SAFETY: geteuid has no arguments and does not mutate process state.
    if unsafe { libc::geteuid() } == 0 {
        // Do not relax private cache ancestors to let the isolated UID traverse
        // them. Only public source/SDK and temporary outputs live in /tmp.
        return Ok(Some(
            tempfile::Builder::new()
                .prefix("openwebide-build-")
                .tempdir_in("/tmp")?,
        ));
    }
    Ok(None)
}
#[cfg(target_os = "linux")]
fn read_only_input(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = path.symlink_metadata()?;
    let mode = if metadata.is_dir() {
        for entry in std::fs::read_dir(path)? {
            read_only_input(&entry?.path())?;
        }
        0o555
    } else if metadata.is_file() {
        if metadata.permissions().mode() & 0o111 != 0 {
            0o555
        } else {
            0o444
        }
    } else {
        bail!("Compiler input cannot contain links or special files");
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}
#[cfg(target_os = "linux")]
fn copy_source(original: &Path, target: &Path, files: &mut usize, bytes: &mut u64) -> Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(original)? {
        let entry = entry?;
        if entry.file_name() == ".git" || entry.file_name() == "target" {
            continue;
        }
        let metadata = entry.path().symlink_metadata()?;
        if metadata.is_dir() {
            copy_source(&entry.path(), &target.join(entry.file_name()), files, bytes)?;
        } else if metadata.is_file() {
            *files += 1;
            *bytes += metadata.len();
            if *files > 2048 || *bytes > 16 * 1024 * 1024 {
                bail!("Rust plugin source exceeds its limit");
            }
            std::fs::copy(entry.path(), target.join(entry.file_name()))?;
        } else {
            bail!("Rust plugin source cannot contain links or special files");
        }
    }
    Ok(())
}
fn check_cancelled(cancelled: &AtomicBool) -> Result<()> {
    if cancelled.load(Ordering::Acquire) {
        bail!("Plugin build was cancelled");
    }
    Ok(())
}

#[cfg(test)]
fn run(command: Command, timeout: Duration) -> Result<()> {
    run_cancellable(command, timeout, &AtomicBool::new(false))
}

struct BuildLog {
    bytes: Box<[AtomicU8]>,
    published: AtomicUsize,
}
impl BuildLog {
    fn new() -> Self {
        Self {
            bytes: (0..128 * 1024).map(|_| AtomicU8::new(0)).collect(),
            published: AtomicUsize::new(0),
        }
    }

    /// One reader writes each prefix byte once. Publish only complete chunks so
    /// a stopped reader cannot block a snapshot or expose an unfinished chunk.
    fn record(&self, offset: usize, bytes: &[u8]) -> usize {
        let count = bytes.len().min(self.bytes.len().saturating_sub(offset));
        if count == 0 {
            return offset;
        }
        for (target, byte) in self.bytes[offset..offset + count].iter().zip(bytes) {
            target.store(*byte, Ordering::Relaxed);
        }
        let end = offset + count;
        self.published.store(end, Ordering::Release);
        end
    }

    fn snapshot(&self) -> Vec<u8> {
        let count = self.published.load(Ordering::Acquire);
        self.bytes[..count]
            .iter()
            .map(|byte| byte.load(Ordering::Relaxed))
            .collect()
    }
}

fn run_cancellable(mut command: Command, timeout: Duration, cancelled: &AtomicBool) -> Result<()> {
    check_cancelled(cancelled)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let retained = Arc::new(BuildLog::new());
    let child = command
        .spawn()
        .context("Unable to start isolated plugin build; check host prerequisites")?;
    let mut child = BuildProcess(child);
    let stderr = child.0.stderr.take().expect("piped build log");
    let (logs, completed) = std::sync::mpsc::channel();
    let reader_log = retained.clone();
    std::thread::spawn(move || {
        let mut written = 0;
        let mut buffer = [0u8; 8192];
        let mut stream = stderr;
        while let Ok(count) = stream.read(&mut buffer) {
            if count == 0 {
                break;
            }
            written = reader_log.record(written, &buffer[..count]);
        }
        let _ = logs.send(());
    });
    let outcome = wait_for_build(&mut child.0, timeout, cancelled);
    // A successful Cargo exit does not authorize background build-script work.
    // Stop remaining descendants before waiting for their inherited log pipe.
    drop(child);
    let _ = completed.recv_timeout(Duration::from_secs(1));
    let log = retained.snapshot();
    drop(retained);
    let status = outcome
        .and_then(|status| {
            check_cancelled(cancelled)?;
            Ok(status)
        })
        .map_err(|error| {
            if log.is_empty() {
                error
            } else {
                let context = format!(
                    "{error}\nRetained plugin build stderr (at most 128 KiB): {}",
                    String::from_utf8_lossy(&log)
                );
                error.context(context)
            }
        })?;
    if !status.success() {
        bail!(
            "Plugin preparation failed ({status}): {}",
            String::from_utf8_lossy(&log)
        );
    }
    Ok(())
}

fn wait_for_build(
    child: &mut std::process::Child,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<std::process::ExitStatus> {
    let started = Instant::now();
    loop {
        check_cancelled(cancelled)?;
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if started.elapsed() > timeout {
            bail!("Plugin build exceeded its time limit");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

struct BuildProcess(std::process::Child);
impl Drop for BuildProcess {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Ok(pid) = i32::try_from(self.0.id()) {
            // SAFETY: each build starts in its own process group in run().
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(all(test, unix))]
mod process_tests {
    use super::*;

    #[test]
    fn successful_build_cleans_up_children_holding_the_log_pipe() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30 & exit 0"]);
        let started = Instant::now();
        run(command, Duration::from_secs(2)).unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn timed_out_build_stops_without_waiting_for_its_children() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30"]);
        let started = Instant::now();
        assert!(
            run(command, Duration::from_millis(50))
                .unwrap_err()
                .to_string()
                .contains("time limit")
        );
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn timed_out_build_retains_bounded_stderr_after_child_cleanup() {
        let directory = tempfile::tempdir().unwrap();
        let written = directory.path().join("stderr-written");
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "printf 'compiler-timeout-marker' >&2; i=0; \
             while [ \"$i\" -lt 4096 ]; do printf '%064d' 0 >&2; i=$((i+1)); done; \
             printf 'omitted-tail-marker' >&2; : > \"$1\"; sleep 30 & wait",
                "compiler-log-test",
            ])
            .arg(&written);
        let started = Instant::now();
        let error = run(command, Duration::from_millis(500)).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(
            written.exists(),
            "Fixture did not fill the retained log budget"
        );
        let error = error.to_string();
        assert!(error.contains("time limit"));
        assert!(error.contains("compiler-timeout-marker"));
        assert!(!error.contains("omitted-tail-marker"));
        assert!(error.len() < 128 * 1024 + 1024);
    }

    #[test]
    fn cancellation_retains_stderr_after_child_cleanup() {
        let cancelled = AtomicBool::new(false);
        let directory = tempfile::tempdir().unwrap();
        let ready = directory.path().join("stderr-written");
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let started = Instant::now();
                while !ready.exists() && started.elapsed() < Duration::from_secs(2) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                cancelled.store(true, Ordering::Release);
            });
            let mut command = Command::new("/bin/sh");
            command
                .args([
                    "-c",
                    "printf 'compiler-cancel-marker' >&2; : > \"$1\"; sleep 30 & wait",
                    "compiler-log-test",
                ])
                .arg(&ready);
            let started = Instant::now();
            let error = run_cancellable(command, Duration::from_secs(30), &cancelled)
                .unwrap_err()
                .to_string();
            assert!(started.elapsed() < Duration::from_secs(3));
            assert!(error.contains("cancelled"));
            assert!(error.contains("compiler-cancel-marker"));
            assert!(ready.exists());
        });
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn timeout_retains_available_stderr_when_another_writer_keeps_the_pipe_open() {
        use std::os::fd::AsRawFd;
        use std::os::unix::{net::UnixDatagram, process::CommandExt};

        let (retained_writer, sender) = UnixDatagram::pair().unwrap();
        let bytes = u32::try_from(std::mem::size_of::<libc::c_int>()).unwrap();
        // SAFETY: these macros only calculate the space for one descriptor.
        let (length, space) = unsafe { (libc::CMSG_LEN(bytes), libc::CMSG_SPACE(bytes)) };
        assert!(usize::try_from(space).unwrap() <= 8 * std::mem::size_of::<usize>());
        #[cfg(target_os = "linux")]
        let (length, space) = (
            usize::try_from(length).unwrap(),
            usize::try_from(space).unwrap(),
        );
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf 'retained-pipe-marker' >&2; sleep 30"]);
        // SAFETY: after fork this callback uses only stack buffers and sendmsg.
        // Passing the child's configured stderr into our own socket queue keeps
        // one writer open without spawning an escaped or unowned process.
        unsafe {
            command.pre_exec(move || {
                let mut control = [0usize; 8];
                let mut payload = *b"x";
                let mut vector = libc::iovec {
                    iov_base: payload.as_mut_ptr().cast(),
                    iov_len: payload.len(),
                };
                let mut message: libc::msghdr = std::mem::zeroed();
                message.msg_iov = &raw mut vector;
                message.msg_iovlen = 1;
                message.msg_control = control.as_mut_ptr().cast();
                message.msg_controllen = space;
                let header = libc::CMSG_FIRSTHDR(&message);
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                (*header).cmsg_len = length;
                libc::CMSG_DATA(header)
                    .cast::<libc::c_int>()
                    .write_unaligned(libc::STDERR_FILENO);
                if libc::sendmsg(sender.as_raw_fd(), &message, 0) != 1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let started = Instant::now();
        let error = run(command, Duration::from_millis(500))
            .unwrap_err()
            .to_string();
        assert!(started.elapsed() >= Duration::from_secs(1));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(error.contains("time limit"));
        assert!(error.contains("retained-pipe-marker"));
        drop(retained_writer);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn fetch_sandbox_reads_only_the_resolved_system_dns_file() {
        let directory = tempfile::tempdir().unwrap();
        let allowed = directory.path().join("compiler");
        std::fs::create_dir(&allowed).unwrap();
        let private = directory.path().join("private");
        std::fs::write(&private, "private-build-test").unwrap();
        let resolver = Path::new("/etc/resolv.conf").canonicalize().unwrap();
        let expected = std::fs::read(&resolver).unwrap();
        assert!(!expected.is_empty());
        let cat = |path: &Path, network| {
            let mut command = sandbox(
                Path::new("/bin/cat"),
                &allowed,
                &allowed,
                &allowed,
                &allowed,
                network,
                None,
            )
            .unwrap();
            command.arg(path).output().unwrap()
        };
        let fetch = cat(Path::new("/etc/resolv.conf"), true);
        assert!(fetch.status.success());
        assert!(
            fetch.stdout == expected,
            "Fetch sandbox did not read the original DNS configuration"
        );
        assert!(!cat(&resolver, false).status.success());
        assert!(!cat(&private, true).status.success());
        assert!(!cat(&private, false).status.success());
    }

    #[test]
    fn cancelled_build_stops_running_process_and_releases_its_log_pipe() {
        let cancelled = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(100));
                cancelled.store(true, Ordering::Release);
            });
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "sleep 30 & wait"]);
            let started = Instant::now();
            assert!(
                run_cancellable(command, Duration::from_secs(30), &cancelled)
                    .unwrap_err()
                    .to_string()
                    .contains("cancelled")
            );
            assert!(started.elapsed() < Duration::from_secs(3));
        });
    }

    #[test]
    fn cancellation_before_start_does_not_execute_build_code() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("executed");
        let mut command = Command::new("/usr/bin/touch");
        command.arg(&marker);
        assert!(run_cancellable(command, Duration::from_secs(2), &AtomicBool::new(true)).is_err());
        assert!(!marker.exists());
    }
}
