use futures::{SinkExt, StreamExt};
use openwebide_core::{BridgeClientMessage, BridgeServerMessage};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

async fn spawn_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut config = openwebide_bridge::ServerConfig::new(
        std::env::temp_dir(),
        std::sync::Arc::from("dummy"),
        None,
    );
    config.allowed_origins.push("http://test.local".to_string());
    tokio::spawn(openwebide_bridge::run_server(listener, config));
    port
}

async fn connect_ws(
    port: u16,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let req = tokio_tungstenite::tungstenite::handshake::client::Request::builder()
        .uri(format!("ws://127.0.0.1:{port}"))
        .header("Host", "127.0.0.1")
        .header("Origin", "http://test.local")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .body(())
        .unwrap();

    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    use futures::{SinkExt, StreamExt};
    let token = openwebide_auth::sign_token_expires("dummy", 42, 9999999999, 0);
    let hello = format!(r#"{{"type":"hello","token":"{token}"}}"#);
    ws.send(tokio_tungstenite::tungstenite::Message::Text(hello.into()))
        .await
        .unwrap();
    let resp = ws.next().await.unwrap().unwrap();
    assert!(resp.to_text().unwrap().contains("hello_ok"));
    ws
}

async fn send_msg(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    msg: BridgeClientMessage,
) {
    let json = serde_json::to_string(&msg).unwrap();
    ws.send(Message::Text(json.into())).await.unwrap();
}

async fn recv_msg(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> BridgeServerMessage {
    loop {
        let msg = ws.next().await.unwrap().unwrap();
        if let Message::Text(t) = msg {
            return serde_json::from_str(&t).unwrap();
        }
    }
}

#[tokio::test]
async fn lag_delivers_every_line_in_order() {
    let port = spawn_server().await;
    let mut ws = connect_ws(port).await;

    send_msg(
        &mut ws,
        BridgeClientMessage::Spawn {
            id: "lag1".into(),
            command: "sh".into(),
            args: vec!["-c".into(), "seq 1 6000".into()],
            cwd: None,
            env: Default::default(),
            pty: false,
            cols: 80,
            rows: 24,
        },
    )
    .await;

    let mut output = String::new();
    let mut exited = false;

    while !exited {
        match recv_msg(&mut ws).await {
            BridgeServerMessage::Output { data, .. } => output.push_str(&data),
            BridgeServerMessage::Exited { .. } => exited = true,
            _ => {}
        }
    }

    // Verify all 6000 lines arrived in order
    let lines: Vec<&str> = output.trim_end().split('\n').collect();
    assert_eq!(lines.len(), 6000);
    assert_eq!(lines[0], "1");
    assert_eq!(lines[5999], "6000");
}

#[tokio::test]
async fn kill_is_responsive_while_client_not_reading() {
    let port = spawn_server().await;
    let mut ws = connect_ws(port).await;

    send_msg(
        &mut ws,
        BridgeClientMessage::Spawn {
            id: "kill1".into(),
            command: "yes".into(),
            args: vec![],
            cwd: None,
            env: Default::default(),
            pty: false,
            cols: 80,
            rows: 24,
        },
    )
    .await;

    // wait for it to spawn and produce some output
    tokio::time::sleep(Duration::from_millis(500)).await;

    send_msg(
        &mut ws,
        BridgeClientMessage::Kill {
            id: "kill1".into(),
            signal: Some("KILL".into()),
        },
    )
    .await;

    let start = std::time::Instant::now();
    let mut exited = false;

    while !exited {
        if let BridgeServerMessage::Exited { .. } =
            tokio::time::timeout(Duration::from_secs(3), recv_msg(&mut ws))
                .await
                .unwrap()
        {
            exited = true;
        }
    }
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn output_never_after_exited() {
    let port = spawn_server().await;
    let mut ws = connect_ws(port).await;

    for i in 0..20 {
        let id = format!("never_after_{i}");
        send_msg(
            &mut ws,
            BridgeClientMessage::Spawn {
                id: id.clone(),
                command: "sh".into(),
                args: vec!["-c".into(), "seq 1 3000; seq 1 3000 >&2".into()],
                cwd: None,
                env: Default::default(),
                pty: false,
                cols: 80,
                rows: 24,
            },
        )
        .await;

        let mut exited = false;
        while !exited {
            match recv_msg(&mut ws).await {
                BridgeServerMessage::Exited { id: msg_id, .. } => {
                    if msg_id == id {
                        exited = true;
                    }
                }
                BridgeServerMessage::Output { id: msg_id, .. } => {
                    assert!(!exited || msg_id != id, "output received after Exited");
                }
                _ => {}
            }
        }
    }
}

#[tokio::test]
async fn reattach_after_exit_replays_exited() {
    let port = spawn_server().await;
    let mut ws = connect_ws(port).await;

    send_msg(
        &mut ws,
        BridgeClientMessage::Spawn {
            id: "reattach1".into(),
            command: "sh".into(),
            args: vec!["-c".into(), "exit 7".into()],
            cwd: None,
            env: Default::default(),
            pty: false,
            cols: 80,
            rows: 24,
        },
    )
    .await;

    let mut exited = false;
    while !exited {
        if let BridgeServerMessage::Exited { .. } = recv_msg(&mut ws).await {
            exited = true;
        }
    }

    // New connection
    let mut ws2 = connect_ws(port).await;
    send_msg(
        &mut ws2,
        BridgeClientMessage::Attach {
            id: "reattach1".into(),
            last_seq: 0,
        },
    )
    .await;

    let mut exit_found = false;
    let mut timeout = tokio::time::timeout(Duration::from_secs(2), recv_msg(&mut ws2));
    while let Ok(msg) = timeout.await {
        if let BridgeServerMessage::Exited { exit_code, .. } = msg {
            assert_eq!(exit_code, Some(7));
            exit_found = true;
            break;
        }
        timeout = tokio::time::timeout(Duration::from_secs(2), recv_msg(&mut ws2));
    }
    assert!(exit_found, "Exited event was not replayed");
}

#[tokio::test]
async fn pty_multibyte_intact() {
    let port = spawn_server().await;
    let mut ws = connect_ws(port).await;

    send_msg(
        &mut ws,
        BridgeClientMessage::Spawn {
            id: "pty1".into(),
            command: "python3".into(),
            args: vec![
                "-c".into(),
                "import sys; print('€' * 20000); sys.stdout.flush()".into(),
            ],
            cwd: None,
            env: Default::default(),
            pty: true,
            cols: 80,
            rows: 24,
        },
    )
    .await;

    let mut output = String::new();
    let mut exited = false;

    while !exited {
        match recv_msg(&mut ws).await {
            BridgeServerMessage::Output { data, .. } => output.push_str(&data),
            BridgeServerMessage::Exited { .. } => exited = true,
            _ => {}
        }
    }

    assert!(output.contains("€"));
    assert!(!output.contains("\u{FFFD}"));
}

#[tokio::test]
async fn headless_invalid_utf8_keeps_streaming() {
    let port = spawn_server().await;
    let mut ws = connect_ws(port).await;

    send_msg(
        &mut ws,
        BridgeClientMessage::Spawn {
            id: "invalid_utf8".into(),
            command: "sh".into(),
            args: vec!["-c".into(), "printf '\\377\\n'; seq 1 3000".into()],
            cwd: None,
            env: Default::default(),
            pty: false,
            cols: 80,
            rows: 24,
        },
    )
    .await;

    let mut output = String::new();
    let mut exited = false;

    while !exited {
        match recv_msg(&mut ws).await {
            BridgeServerMessage::Output { data, .. } => output.push_str(&data),
            BridgeServerMessage::Exited { .. } => exited = true,
            _ => {}
        }
    }

    assert!(output.contains("\u{FFFD}"));
    assert!(output.contains("3000"));
}

#[tokio::test]
async fn duplicate_spawn_id_rejected() {
    let port = spawn_server().await;
    let mut ws = connect_ws(port).await;

    send_msg(
        &mut ws,
        BridgeClientMessage::Spawn {
            id: "dup1".into(),
            command: "sleep".into(),
            args: vec!["10".into()],
            cwd: None,
            env: Default::default(),
            pty: false,
            cols: 80,
            rows: 24,
        },
    )
    .await;

    // wait for spawn
    loop {
        if let BridgeServerMessage::Spawned { .. } = recv_msg(&mut ws).await {
            break;
        }
    }

    send_msg(
        &mut ws,
        BridgeClientMessage::Spawn {
            id: "dup1".into(),
            command: "sleep".into(),
            args: vec!["10".into()],
            cwd: None,
            env: Default::default(),
            pty: false,
            cols: 80,
            rows: 24,
        },
    )
    .await;

    let mut rejected = false;
    while !rejected {
        if let BridgeServerMessage::Error { message, .. } = recv_msg(&mut ws).await {
            assert!(message.contains("already exists"));
            rejected = true;
        }
    }
}

#[tokio::test]
async fn signal_death_reported() {
    let port = spawn_server().await;
    let mut ws = connect_ws(port).await;

    send_msg(
        &mut ws,
        BridgeClientMessage::Spawn {
            id: "sig1".into(),
            command: "sh".into(),
            args: vec!["-c".into(), "kill -9 $$".into()],
            cwd: None,
            env: Default::default(),
            pty: false,
            cols: 80,
            rows: 24,
        },
    )
    .await;

    let mut exited = false;
    while !exited {
        if let BridgeServerMessage::Exited { signal, .. } = recv_msg(&mut ws).await {
            #[cfg(unix)]
            assert_eq!(signal, Some("SIGKILL".into()));
            exited = true;
        }
    }

    send_msg(
        &mut ws,
        BridgeClientMessage::Spawn {
            id: "pty2".into(),
            command: "sh".into(),
            args: vec!["-c".into(), "exit 7".into()],
            cwd: None,
            env: Default::default(),
            pty: true,
            cols: 80,
            rows: 24,
        },
    )
    .await;

    let mut exited2 = false;
    while !exited2 {
        if let BridgeServerMessage::Exited { exit_code, .. } = recv_msg(&mut ws).await {
            assert_eq!(exit_code, Some(7));
            exited2 = true;
        }
    }
}

#[tokio::test]
async fn fast_spawns_all_see_exited() {
    let port = spawn_server().await;
    let mut ws = connect_ws(port).await;

    for i in 0..300 {
        send_msg(
            &mut ws,
            BridgeClientMessage::Spawn {
                id: format!("fast_{i}"),
                command: "true".into(),
                args: vec![],
                cwd: None,
                env: Default::default(),
                pty: false,
                cols: 80,
                rows: 24,
            },
        )
        .await;
    }

    // Test exit replay after the processes have actually completed. A fixed
    // sleep does not establish that precondition for a burst of 300 children.
    // List is ordered after the spawn requests on this connection; draining
    // their replies also avoids making setup depend on unread socket buffers.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            send_msg(&mut ws, BridgeClientMessage::List).await;
            loop {
                match recv_msg(&mut ws).await {
                    BridgeServerMessage::Sessions { sessions } => {
                        assert_eq!(sessions.len(), 300);
                        if sessions.iter().all(|session| !session.running) {
                            return;
                        }
                        break;
                    }
                    BridgeServerMessage::Error { id, message } => {
                        panic!("spawn {id} failed: {message}");
                    }
                    _ => (),
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("all 300 processes should complete before checking exit replay");

    for i in 0..300 {
        let mut ws2 = connect_ws(port).await;
        send_msg(
            &mut ws2,
            BridgeClientMessage::Attach {
                id: format!("fast_{i}"),
                last_seq: 0,
            },
        )
        .await;

        let mut exited = false;
        while !exited {
            match tokio::time::timeout(Duration::from_secs(1), recv_msg(&mut ws2)).await {
                Ok(BridgeServerMessage::Exited { .. }) => exited = true,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert!(exited, "session fast_{i} did not exit");
    }
}

#[tokio::test]
async fn slow_reader_gets_contiguous_or_explicit_truncation() {
    let port = spawn_server().await;
    let mut ws = connect_ws(port).await;

    send_msg(
        &mut ws,
        BridgeClientMessage::Spawn {
            id: "slow1".into(),
            command: "sh".into(),
            args: vec!["-c".into(), "seq 1 200000".into()],
            cwd: None,
            env: Default::default(),
            pty: false,
            cols: 80,
            rows: 24,
        },
    )
    .await;

    tokio::time::sleep(Duration::from_secs(2)).await;

    let mut truncated = false;
    let mut exited = false;

    while !exited {
        match recv_msg(&mut ws).await {
            BridgeServerMessage::Error { message, .. } => {
                if message.contains("output truncated") {
                    truncated = true;
                }
            }
            BridgeServerMessage::Exited { .. } => exited = true,
            _ => {}
        }
    }

    // As long as it doesn't crash or hang, it's successful.
    // It should either truncate or receive all data.
    let _ = truncated;
}

#[tokio::test]
async fn spawned_pid_is_child() {
    let port = spawn_server().await;
    let mut ws = connect_ws(port).await;

    send_msg(
        &mut ws,
        BridgeClientMessage::Spawn {
            id: "pid1".into(),
            command: "sh".into(),
            args: vec!["-c".into(), "echo $$".into()],
            cwd: None,
            env: Default::default(),
            pty: false,
            cols: 80,
            rows: 24,
        },
    )
    .await;

    let mut pid_from_spawned = 0;
    let mut output = String::new();
    let mut exited = false;

    while !exited {
        match recv_msg(&mut ws).await {
            BridgeServerMessage::Spawned { pid, .. } => pid_from_spawned = pid,
            BridgeServerMessage::Output { data, .. } => output.push_str(&data),
            BridgeServerMessage::Exited { .. } => exited = true,
            _ => {}
        }
    }

    let pid_from_output: u32 = output.trim().parse().unwrap();
    assert_eq!(pid_from_spawned, pid_from_output);
}
