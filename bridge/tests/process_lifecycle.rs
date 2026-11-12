//! Integration tests for bridge process lifecycle: killing the command's process group when the
//! `/exec` client disconnects mid-request.

mod common;

use std::time::Duration;

use common::TestDir;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

#[tokio::test]
async fn exec_client_disconnect_kills_group() {
    disconnect_kills_group(false).await;
}

#[tokio::test]
async fn paired_browser_disconnect_kills_the_started_command_group() {
    disconnect_kills_group(true).await;
}

async fn disconnect_kills_group(paired: bool) {
    use openwebide_bridge::{ServerConfig, run_server_until};
    use tokio::net::TcpListener;

    let test_dir = TestDir::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = ServerConfig::new(
        test_dir.path.clone(),
        "dummy_secret".into(),
        paired.then(|| "command-pairing-token".into()),
    );
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(run_server_until(listener, config, async {
        let _ = stopped.await;
    }));
    let direct = test_dir.path.join("direct");
    let grandchild = test_dir.path.join("grandchild");
    let ready = test_dir.path.join("ready");
    let grandchild_ready = test_dir.path.join("grandchild-ready");
    let payload = serde_json::json!({
        "command": "printf ready > ready; (printf ready > grandchild-ready; sleep 3; touch grandchild) & sleep 3; touch direct",
        "cancel_on_disconnect": true,
    })
    .to_string();
    let credentials = if paired {
        "Origin: http://localhost:8080\r\nAuthorization: Bearer command-pairing-token\r\n"
    } else {
        "Authorization: Bearer dummy_secret\r\n"
    };
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let req = format!(
        "POST /exec HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{credentials}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(req.as_bytes()).await.unwrap();

    // Authentication rejection must not make a no-child test pass. Both the
    // command and its real background descendant must start before disconnect.
    tokio::time::timeout(Duration::from_secs(2), async {
        while !ready.exists() || !grandchild_ready.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("The authenticated command and descendant did not start");
    assert!(!direct.exists() && !grandchild.exists());
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(stream);

    // Preserve the original delayed-write oracle and observation deadline.
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(
        !direct.exists() && !grandchild.exists(),
        "disconnecting the /exec client should kill the command's whole process group"
    );
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(6), server)
        .await
        .unwrap()
        .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_waits_for_delayed_process_group_cleanup() {
    use openwebide_bridge::{ServerConfig, run_server_until};
    use tokio::net::TcpListener;
    let dir = TestDir::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = ServerConfig::new(dir.path.clone(), std::sync::Arc::from("dummy_secret"), None);
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        run_server_until(listener, config, async {
            let _ = stopped.await;
        })
        .await;
    });
    let pid_file = dir.path.join("pid");
    let body = serde_json::json!({"command": format!("trap '' TERM; echo $$ > {}; while :; do sleep 1; done", pid_file.display())}).to_string();
    let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    client.write_all(format!("POST /exec HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer dummy_secret\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", body.len(), body).as_bytes()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !pid_file.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let pid: i32 = std::fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(6), server)
        .await
        .unwrap()
        .unwrap();
    // SAFETY: signal zero only checks whether the test-owned process group remains alive.
    let remaining = unsafe { libc::kill(-pid, 0) };
    if remaining != -1 {
        let observed = std::process::Command::new("ps")
            .args(["-eo", "pid=,ppid=,pgid=,stat=,comm="])
            .output();
        let observed = match observed {
            Ok(observed) => observed,
            Err(error) => {
                eprintln!("Cannot snapshot owned group {pid}: {error}");
                assert_eq!(remaining, -1);
                return;
            }
        };
        let rows = String::from_utf8_lossy(&observed.stdout)
            .lines()
            .filter(|line| {
                line.split_whitespace()
                    .nth(2)
                    .and_then(|field| field.parse::<i32>().ok())
                    == Some(pid)
            })
            .map(str::to_string)
            .collect::<Vec<_>>();
        eprintln!(
            "Owned group after server shutdown: pgid={pid}, signal_zero={remaining}, ps_status={}, members={rows:?}",
            observed.status
        );
    }
    assert_eq!(remaining, -1);
}
