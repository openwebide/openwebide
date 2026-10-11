//! Runs inside a normal root container; the child probe is actual Rust code.
#![cfg(target_os = "linux")]
use std::{os::unix::fs::PermissionsExt, process::Command};

#[test]
fn container_compiler_isolates_files_metadata_network_and_identity() {
    // Native Linux users use bubblewrap, which the runtime's source tests cover.
    // SAFETY: geteuid has no arguments and does not mutate process state.
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let stage = root.path().join("stage");
    let source = stage.join("source");
    let sdk = stage.join("sdk");
    let work = stage.join("work");
    for path in [&source, &sdk, &work] {
        std::fs::create_dir_all(path).unwrap();
    }
    let secret = root.path().join("private.txt");
    std::fs::write(&secret, "host sentinel").unwrap();
    for path in [source.join("immutable"), sdk.join("immutable")] {
        std::fs::write(&path, "original").unwrap();
        // World writable so data denial specifically tests the filesystem rules.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let executable = std::env::current_exe().unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_openwebide-plugin-build"))
        .args(["build"])
        .args([
            source.as_path(),
            work.as_path(),
            sdk.as_path(),
            executable.parent().unwrap(),
            executable.as_path(),
        ])
        .args(["--exact", "sandbox_probe", "--ignored", "--nocapture"])
        .env("PROBE_SECRET", &secret)
        .env("PROBE_SOURCE", &source)
        .env("PROBE_SDK", &sdk)
        .env("PROBE_ADDRESS", listener.local_addr().unwrap().to_string())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(work.join("result")).unwrap(),
        "isolated"
    );
    assert_eq!(
        std::fs::read_to_string(source.join("immutable")).unwrap(),
        "original"
    );
}

#[test]
#[ignore = "Executed only inside the isolated compiler child"]
fn sandbox_probe() {
    // SAFETY: geteuid has no arguments and does not mutate process state.
    assert_ne!(unsafe { libc::geteuid() }, 0);
    assert!(Command::new("/usr/bin/true").status().unwrap().success());
    assert!(std::fs::read(std::env::var_os("PROBE_SECRET").unwrap()).is_err());
    for variable in ["PROBE_SOURCE", "PROBE_SDK"] {
        let path = std::path::PathBuf::from(std::env::var_os(variable).unwrap()).join("immutable");
        assert!(std::fs::write(&path, "escaped").is_err());
        assert!(std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o0)).is_err());
    }
    let address = std::env::var("PROBE_ADDRESS").unwrap().parse().unwrap();
    assert!(
        std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_secs(1)).is_err()
    );
    std::fs::write("result", "isolated").unwrap();
}
