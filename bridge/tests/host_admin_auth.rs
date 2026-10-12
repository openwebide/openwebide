use bytes::Bytes;
use http_body_util::Full;
use hyper::Request;
use openwebide_bridge::{ServerConfig, run_server_until};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::net::{TcpListener, TcpStream};

async fn status(port: u16, path: &str, token: Option<&str>, origin: Option<&str>) -> u16 {
    let io = hyper_util::rt::TokioIo::new(TcpStream::connect(("127.0.0.1", port)).await.unwrap());
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io).await.unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut request = Request::builder()
        .method("POST")
        .uri(format!("http://127.0.0.1:{port}{path}"))
        .header("host", format!("127.0.0.1:{port}"))
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    if let Some(origin) = origin {
        request = request.header("origin", origin);
    }
    sender
        .send_request(request.body(Full::new(Bytes::from_static(b"{}"))).unwrap())
        .await
        .unwrap()
        .status()
        .as_u16()
}

#[tokio::test]
async fn host_routes_require_the_server_secret_and_reject_browser_and_companion_access() {
    let secret: Arc<str> = "host-admin-test-secret-only".into();
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    let browser = openwebide_auth::sign_token_expires(&secret, 1, now + 120, 0);
    for paired in [false, true] {
        let workspace = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut config = ServerConfig::new(
            workspace.path().into(),
            secret.clone(),
            paired.then(|| "companion-pairing-token".into()),
        );
        config.backend_url = "http://127.0.0.1:9/api".into();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(run_server_until(listener, config, async {
            let _ = stopped.await;
        }));
        for path in [
            "/host/admin",
            "/host/input",
            "/host/input-key",
            "/host/probe",
        ] {
            assert_eq!(status(port, path, None, None).await, 401);
            assert_eq!(status(port, path, Some(&browser), None).await, 401);
            assert_eq!(
                status(port, path, Some(&browser), Some("http://localhost:3000")).await,
                403
            );
            if paired {
                assert_eq!(status(port, path, Some(&secret), None).await, 403);
                assert_eq!(
                    status(
                        port,
                        path,
                        Some("companion-pairing-token"),
                        Some("http://localhost:3000")
                    )
                    .await,
                    403
                );
            } else {
                let result = status(port, path, Some(&secret), None).await;
                assert!(
                    ![401, 403].contains(&result),
                    "Server secret was incorrectly rejected at {path}"
                );
            }
        }
        stop.send(()).unwrap();
        server.await.unwrap();
    }
}
