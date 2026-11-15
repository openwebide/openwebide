use futures::{SinkExt, StreamExt};
use std::path::PathBuf;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

use openwebide_bridge::{ServerConfig, run_server};
use openwebide_core::{BridgeClientMessage, BridgeServerMessage};

async fn start_with_config(config: ServerConfig) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        run_server(listener, config).await;
    });
    port
}

#[tokio::test]
async fn hello_with_valid_token_ok() {
    let secret = "test_secret_for_hello";
    let config = common::server_config(PathBuf::from("."), secret.into(), None);
    let port = start_with_config(config).await;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let token =
        openwebide_auth::sign_token_expires(secret, 42, i64::try_from(now).unwrap() + 120, 0);

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
        .await
        .unwrap();

    let hello_msg = BridgeClientMessage::Hello { token };
    ws.send(Message::Text(
        serde_json::to_string(&hello_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    let response = ws.next().await.unwrap().unwrap();
    if let Message::Text(txt) = response {
        let msg: BridgeServerMessage = serde_json::from_str(&txt).unwrap();
        match msg {
            BridgeServerMessage::HelloOk {
                user_id: Some(42),
                runs,
                ..
            } => assert_eq!(runs, cfg!(feature = "tls")),
            _ => panic!("Expected HelloOk with user_id 42, got: {msg:?}"),
        }
    } else {
        panic!("Expected text message");
    }
}

#[tokio::test]
async fn hello_with_expired_token_rejected() {
    let secret = "test_secret_for_hello";
    let config = common::server_config(PathBuf::from("."), secret.into(), None);
    let port = start_with_config(config).await;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let token =
        openwebide_auth::sign_token_expires(secret, 42, i64::try_from(now).unwrap() - 10, 0);

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
        .await
        .unwrap();

    let hello_msg = BridgeClientMessage::Hello { token };
    ws.send(Message::Text(
        serde_json::to_string(&hello_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    let response = ws.next().await.unwrap().unwrap();
    if let Message::Text(txt) = response {
        let msg: BridgeServerMessage = serde_json::from_str(&txt).unwrap();
        match msg {
            BridgeServerMessage::HelloError { message } => assert!(message.contains("expired")),
            _ => panic!("Expected HelloError, got: {msg:?}"),
        }
    } else {
        panic!("Expected text message");
    }
}

#[tokio::test]
async fn hello_with_login_secret_token_rejected() {
    let secret = "test_secret_for_hello";
    let login_secret = "different_secret_for_login";
    let config = common::server_config(PathBuf::from("."), secret.into(), None);
    let port = start_with_config(config).await;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let token =
        openwebide_auth::sign_token_expires(login_secret, 42, i64::try_from(now).unwrap() + 120, 0);

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
        .await
        .unwrap();

    let hello_msg = BridgeClientMessage::Hello { token };
    ws.send(Message::Text(
        serde_json::to_string(&hello_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    let response = ws.next().await.unwrap().unwrap();
    if let Message::Text(txt) = response {
        let msg: BridgeServerMessage = serde_json::from_str(&txt).unwrap();
        match msg {
            BridgeServerMessage::HelloError { .. } => {}
            _ => panic!("Expected HelloError, got: {msg:?}"),
        }
    } else {
        panic!("Expected text message");
    }
}

#[tokio::test]
async fn pairing_token_accepted() {
    let config = common::server_config(
        PathBuf::from("."),
        "secret".into(),
        Some("my_pairing_token".into()),
    );
    let port = start_with_config(config).await;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
        .await
        .unwrap();

    let hello_msg = BridgeClientMessage::Hello {
        token: "my_pairing_token".into(),
    };
    ws.send(Message::Text(
        serde_json::to_string(&hello_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    let response = ws.next().await.unwrap().unwrap();
    if let Message::Text(txt) = response {
        let msg: BridgeServerMessage = serde_json::from_str(&txt).unwrap();
        match msg {
            BridgeServerMessage::HelloOk {
                user_id: None,
                runs,
                ..
            } => assert!(!runs),
            _ => panic!("Expected HelloOk with None user_id, got: {msg:?}"),
        }
    } else {
        panic!("Expected text message");
    }
}

#[tokio::test]
async fn ws_terminal_requires_hello() {
    let config = common::server_config(PathBuf::from("."), "secret".into(), None);
    let port = start_with_config(config).await;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
        .await
        .unwrap();

    let spawn_msg = BridgeClientMessage::Spawn {
        id: "123".into(),
        command: "ls".into(),
        args: vec![],
        cwd: None,
        env: Default::default(),
        pty: false,
        cols: 80,
        rows: 24,
    };
    ws.send(Message::Text(
        serde_json::to_string(&spawn_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    let response = ws.next().await.unwrap().unwrap();
    if let Message::Text(txt) = response {
        let msg: BridgeServerMessage = serde_json::from_str(&txt).unwrap();
        match msg {
            BridgeServerMessage::Error { message, .. } => assert!(message.contains("hello first")),
            _ => panic!("Expected Error, got: {msg:?}"),
        }
    } else {
        panic!("Expected text message");
    }
}

#[tokio::test]
async fn hello_timeout_closes() {
    let config = common::server_config(PathBuf::from("."), "secret".into(), None);
    let port = start_with_config(config).await;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
        .await
        .unwrap();

    // Do nothing and wait for connection to drop
    let res = tokio::time::timeout(std::time::Duration::from_secs(12), ws.next()).await;
    match res {
        Ok(Some(Ok(Message::Close(_)) | Err(_)) | None) => {}
        _ => panic!("Expected connection to be closed by timeout, got {res:?}"),
    }
}

#[tokio::test]
async fn oversized_pre_hello_frame_closes() {
    let config = common::server_config(PathBuf::from("."), "secret".into(), None);
    let port = start_with_config(config).await;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
        .await
        .unwrap();

    let big_string = "a".repeat(66000);
    ws.send(Message::Text(big_string.into())).await.unwrap();

    let response = ws.next().await.unwrap();
    match response {
        Ok(Message::Close(_)) | Err(_) => {}
        _ => panic!("Expected connection to be closed, got {response:?}"),
    }
}

mod common;

#[tokio::test]
async fn browser_exec_requires_bridge_token() {
    let secret = "test_secret_for_hello";
    let config = common::server_config(PathBuf::from("."), secret.into(), None);
    let port = start_with_config(config).await;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let token =
        openwebide_auth::sign_token_expires(secret, 42, i64::try_from(now).unwrap() + 120, 0);

    let req_no_bearer = format!(
        "POST /exec HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://localhost:8080\r\nContent-Type: application/json
Content-Length: 2\r\n\r\n{{}}"
    );
    let resp_no_bearer = common::http(port, &req_no_bearer).await;
    let parsed_no_bearer = common::HttpResponse::parse(&resp_no_bearer);
    assert_eq!(parsed_no_bearer.status, 401);

    let req_with_bearer = format!(
        "POST /exec HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://localhost:8080\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json
Content-Length: 2\r\n\r\n{{}}"
    );
    let resp_with_bearer = common::http(port, &req_with_bearer).await;
    let parsed_with_bearer = common::HttpResponse::parse(&resp_with_bearer);
    // Might be 400 Bad Request because body is just `{}`, but not 401 Unauthorized!
    assert!(
        parsed_with_bearer.status != 401,
        "Expected not 401, got {}",
        parsed_with_bearer.status
    );
}

#[tokio::test]
async fn multiplexed_commands_require_hello_and_terminal_still_streams() {
    use openwebide_core::{ChatRequest, RunRejectCode};
    let port = start_with_config(common::server_config(
        std::env::temp_dir(),
        "secret".into(),
        Some("paired".into()),
    ))
    .await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
        .await
        .unwrap();
    let run = BridgeClientMessage::RunStart {
        run_id: "run-1".into(),
        session_id: 1,
        content: "hi".into(),
        model: None,
        editor_context: None,
        browser_preferences: None,
        queued_prompt: None,
    };
    let completion = BridgeClientMessage::CompletionStart {
        id: "c".into(),
        request: ChatRequest {
            model_settings: Default::default(),
            connection_id: 1,
            system_prompt: None,
            model: None,
            messages: vec![],
            tools: vec![],
        },
    };
    let spawn = BridgeClientMessage::Spawn {
        id: "shell".into(),
        command: "sh".into(),
        args: vec!["-c".into(), "printf terminal-ok".into()],
        cwd: None,
        env: Default::default(),
        pty: false,
        cols: 80,
        rows: 24,
    };
    for command in [run.clone(), completion.clone(), spawn.clone()] {
        ws.send(Message::Text(
            serde_json::to_string(&command).unwrap().into(),
        ))
        .await
        .unwrap();
        let response = ws.next().await.unwrap().unwrap();
        let response: BridgeServerMessage =
            serde_json::from_str(response.to_text().unwrap()).unwrap();
        match (command, response) {
            (
                BridgeClientMessage::RunStart { run_id, .. },
                BridgeServerMessage::RunRejected {
                    run_id: actual,
                    code,
                    ..
                },
            ) => {
                assert_eq!(actual, run_id);
                assert_eq!(code, RunRejectCode::Unauthorized);
            }
            (
                BridgeClientMessage::CompletionStart { id, .. },
                BridgeServerMessage::CompletionEnd { id: actual, error },
            ) => {
                assert_eq!(actual, id);
                assert_eq!(error.as_deref(), Some("unauthorized"));
            }
            (
                BridgeClientMessage::Spawn { id, .. },
                BridgeServerMessage::Error {
                    id: actual,
                    message,
                },
            ) => {
                assert_eq!(actual, id);
                assert!(message.contains("unauthorized"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    ws.send(Message::Text(r#"{"type":"hello","token":"paired"}"#.into()))
        .await
        .unwrap();
    assert!(
        ws.next()
            .await
            .unwrap()
            .unwrap()
            .to_text()
            .unwrap()
            .contains("hello_ok")
    );
    for command in [run, completion] {
        ws.send(Message::Text(
            serde_json::to_string(&command).unwrap().into(),
        ))
        .await
        .unwrap();
        let response = ws.next().await.unwrap().unwrap();
        let response: BridgeServerMessage =
            serde_json::from_str(response.to_text().unwrap()).unwrap();
        match (command, response) {
            (
                BridgeClientMessage::RunStart { run_id, .. },
                BridgeServerMessage::RunRejected {
                    run_id: actual,
                    code,
                    message,
                },
            ) => {
                assert_eq!(actual, run_id);
                assert_eq!(code, RunRejectCode::Unauthorized);
                assert!(message.contains("unauthorized"));
            }
            (
                BridgeClientMessage::CompletionStart { id, .. },
                BridgeServerMessage::CompletionEnd { id: actual, error },
            ) => {
                assert_eq!(actual, id);
                assert_eq!(error.as_deref(), Some("unauthorized"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    ws.send(Message::Text(serde_json::to_string(&spawn).unwrap().into()))
        .await
        .unwrap();
    let (mut spawned, mut output) = (false, String::new());
    loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let Message::Text(text) = frame else {
            continue;
        };
        match serde_json::from_str::<BridgeServerMessage>(&text).unwrap() {
            BridgeServerMessage::Spawned { .. } => spawned = true,
            BridgeServerMessage::Output { data, .. } => output.push_str(&data),
            BridgeServerMessage::Exited { exit_code, .. } => {
                assert_eq!(exit_code, Some(0));
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(spawned);
    assert_eq!(output, "terminal-ok");
}

#[cfg(not(feature = "tls"))]
#[tokio::test]
async fn http_only_signed_in_user_cannot_start_runs_or_completions() {
    let port = start_with_config(common::server_config(
        std::env::temp_dir(),
        "secret".into(),
        None,
    ))
    .await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let token = openwebide_auth::sign_token_expires("secret", 1, now + 120, 0);
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
        .await
        .unwrap();
    ws.send(Message::Text(
        serde_json::to_string(&BridgeClientMessage::Hello { token })
            .unwrap()
            .into(),
    ))
    .await
    .unwrap();
    let hello: BridgeServerMessage =
        serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    assert!(matches!(
        hello,
        BridgeServerMessage::HelloOk {
            runs: false,
            user_id: Some(1),
            ..
        }
    ));
    for request in [
        r#"{"type":"run_start","run_id":"r","session_id":1,"content":"go","model":null,"editor_context":null}"#,
        r#"{"type":"completion_start","id":"c","request":{"connection_id":1,"system_prompt":null,"messages":[]}}"#,
    ] {
        ws.send(Message::Text(request.into())).await.unwrap();
        let response: BridgeServerMessage =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert!(matches!(
            response,
            BridgeServerMessage::RunRejected {
                code: openwebide_core::RunRejectCode::Unauthorized,
                ..
            } | BridgeServerMessage::CompletionEnd { error: Some(_), .. }
        ));
    }
}
