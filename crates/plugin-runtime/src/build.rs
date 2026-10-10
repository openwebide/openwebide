//! Compile pinned source without executing package build code in the host context.
use anyhow::{Context, Result, bail};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub const TOOLCHAIN: &str = "1.98.1";
pub const TARGET: &str = "wasm32-wasip2";
const SDK_MANIFEST: &str = include_str!("../../plugin-sdk/Cargo.toml");
const SDK_SOURCE: &str = include_str!("../../plugin-sdk/src/lib.rs");
const SDK_WIT: &str = include_str!("../../plugin-sdk/wit/plugin.wit");

pub fn sdk_digest() -> String {
    use sha2::{Digest, Sha256};
    let bytes = Sha256::digest(format!("{SDK_MANIFEST}\n{SDK_SOURCE}\n{SDK_WIT}"));
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
        ("wit/plugin.wit", SDK_WIT),
    ] {
        let path = sdk.join(path);
        std::fs::create_dir_all(path.parent().expect("SDK parent"))?;
        std::fs::write(path, contents.replace("\n[lints]\nworkspace = true\n", ""))?;
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
    let patch = format!(
        "patch.crates-io.openwebide-plugin-sdk.path={:?}",
        sdk.to_string_lossy()
    );
    let common = ["--locked", "--manifest-path"];
    // Fetch performs no package build-script execution. Cargo config is read from
    // this trusted staging cwd, not from the plugin's working directory. Fetch
    // the complete lockfile: filtering to the WASI target can omit native
    // dependencies needed by proc macros and build scripts during cross builds.
    let mut fetch = sandbox(&cargo, &source, &work, &rustup_home, &sdk, true)?;
    configure(&mut fetch, &rustc, &rustup_home, &cargo_home, &work);
    fetch
        .arg("fetch")
        .args(common)
        .arg(source.join("Cargo.toml"))
        .args(["--config", &patch]);
    run_cancellable(fetch, Duration::from_secs(180), cancelled)
        .context("Fetch Rust plugin dependencies")?;
    let mut build = sandbox(&cargo, &source, &work, &rustup_home, &sdk, false)?;
    configure(&mut build, &rustc, &rustup_home, &cargo_home, &work);
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
}
#[cfg(target_os = "macos")]
fn sandbox(
    cargo: &Path,
    source: &Path,
    staging: &Path,
    rustup: &Path,
    sdk: &Path,
    network: bool,
) -> Result<Command> {
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
    let profile = format!(
        "(version 1)(deny default)(allow process*)(allow file-read-metadata)(allow sysctl-read)(allow mach-lookup)(allow file-read* (literal \"/\") (literal \"/private/etc/ssl/openssl.cnf\") (literal \"/private/etc/ssl/cert.pem\") (literal \"/private/etc/resolv.conf\") (literal \"/private/etc/hosts\") (subpath \"/System\") (subpath \"/usr\") (subpath \"/Library\") (subpath \"/bin\") (subpath \"/dev\") (subpath {}) (subpath {}) (subpath {}) (subpath {}) (subpath {}))(allow file-write* (subpath {}) (literal \"/dev/null\")){}",
        quote(source),
        quote(staging),
        quote(rustup),
        quote(sdk),
        quote(developer),
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
    for path in ["/usr", "/bin", "/lib", "/lib64"] {
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
    // Fetch may use TLS and DNS, without granting access to private host files.
    if network {
        for path in [
            "/etc/ssl/certs",
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
    let child = command
        .spawn()
        .context("Unable to start isolated plugin build; check host prerequisites")?;
    let mut child = BuildProcess(child);
    let stderr = child.0.stderr.take().expect("piped build log");
    let (logs, completed) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut retained = Vec::new();
        let mut buffer = [0u8; 8192];
        let mut stream = stderr;
        while let Ok(count) = stream.read(&mut buffer) {
            if count == 0 {
                break;
            }
            let remaining = (128 * 1024usize).saturating_sub(retained.len());
            retained.extend_from_slice(&buffer[..count.min(remaining)]);
        }
        let _ = logs.send(retained);
    });
    let started = Instant::now();
    let status = loop {
        check_cancelled(cancelled)?;
        if let Some(status) = child.0.try_wait()? {
            break status;
        }
        if started.elapsed() > timeout {
            bail!("Plugin build exceeded its time limit");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // A successful Cargo exit does not authorize background build-script work.
    // Stop remaining descendants before waiting for their inherited log pipe.
    drop(child);
    let log = completed
        .recv_timeout(Duration::from_secs(1))
        .unwrap_or_default();
    check_cancelled(cancelled)?;
    if !status.success() {
        bail!(
            "Plugin preparation failed ({status}): {}",
            String::from_utf8_lossy(&log)
        );
    }
    Ok(())
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
