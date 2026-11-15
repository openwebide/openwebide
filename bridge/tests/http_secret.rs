mod common;
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::{Method, Request};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

#[allow(deprecated)]
async fn start_server() -> (u16, Arc<str>) {
    let workspace = tempfile::Builder::new().tempdir().unwrap().into_path();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    // In these tests we just use a dummy secret
    let secret: Arc<str> = Arc::from("this_is_a_very_long_dummy_secret_32_bytes_min");
    let config = common::server_config(workspace, secret.clone(), None);

    tokio::spawn(openwebide_bridge::run_server(listener, config));
    (port, secret)
}

async fn request<B>(port: u16, req: Request<B>) -> hyper::Response<hyper::body::Incoming>
where
    B: hyper::body::Body + 'static + Send,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let stream = TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    let io = hyper_util::rt::TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await.unwrap();
    tokio::spawn(async move {
        if let Err(err) = conn.await {
            println!("Connection failed: {err:?}");
        }
    });
    sender.send_request(req).await.unwrap()
}

#[tokio::test]
async fn exec_without_secret_is_401() {
    let (port, _) = start_server().await;
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("http://127.0.0.1:{port}/exec"))
        .header("host", format!("127.0.0.1:{port}"))
        .header("content-type", "application/json")
        .body(http_body_util::Full::new(Bytes::from(
            r#"{"command":"echo test"}"#,
        )))
        .unwrap();

    let res = request(port, req).await;
    assert_eq!(res.status(), 401);
}

#[tokio::test]
async fn exec_with_secret_ok() {
    let (port, secret) = start_server().await;
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("http://127.0.0.1:{port}/exec"))
        .header("host", format!("127.0.0.1:{port}"))
        .header("authorization", format!("Bearer {secret}"))
        .header("content-type", "application/json")
        .body(http_body_util::Full::new(Bytes::from(
            r#"{"command":"echo test"}"#,
        )))
        .unwrap();

    let res = request(port, req).await;
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn git_without_secret_is_401() {
    let (port, _) = start_server().await;
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("http://127.0.0.1:{port}/git/status"))
        .header("host", format!("127.0.0.1:{port}"))
        .header("content-type", "application/json")
        .body(http_body_util::Full::new(Bytes::from(r#"{}"#)))
        .unwrap();

    let res = request(port, req).await;
    assert_eq!(res.status(), 401);
}

#[tokio::test]
async fn secret_endpoint_loopback_ok() {
    let (port, secret) = start_server().await;
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("http://127.0.0.1:{port}/secret"))
        .header("host", format!("127.0.0.1:{port}"))
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .unwrap();

    let res = request(port, req).await;
    assert_eq!(res.status(), 200);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["secret"], secret.as_ref());
}

#[tokio::test]
async fn secret_endpoint_with_origin_403() {
    let (port, _) = start_server().await;
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("http://127.0.0.1:{port}/secret"))
        .header("host", format!("127.0.0.1:{port}"))
        .header("origin", "http://127.0.0.1:3000")
        .header("content-type", "application/json")
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .unwrap();

    let res = request(port, req).await;
    assert_eq!(res.status(), 403);
}

#[tokio::test]
async fn browser_origin_exec_requires_token() {
    let (port, _) = start_server().await;
    // Without bearer, but WITH an origin, should not hit 401
    // (though in reality, missing CORS preflight or missing token in later steps might reject it,
    // in this step it should be allowed because origin is present).
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("http://127.0.0.1:{port}/exec"))
        .header("host", format!("127.0.0.1:{port}"))
        .header("origin", "http://127.0.0.1:3000")
        .header("content-type", "application/json")
        .body(http_body_util::Full::new(Bytes::from(
            r#"{"command":"echo test"}"#,
        )))
        .unwrap();

    let res = request(port, req).await;
    assert_eq!(res.status(), 401);
}

#[tokio::test]
async fn proxied_loopback_cannot_bootstrap_the_secret() {
    let (port, _) = start_server().await;
    for header in [
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
    ] {
        let req = Request::builder()
            .method(Method::POST)
            .uri(format!("http://127.0.0.1:{port}/secret"))
            .header("host", format!("127.0.0.1:{port}"))
            .header(header, "proxy-client")
            .body(http_body_util::Empty::<bytes::Bytes>::new())
            .unwrap();
        let res = request(port, req).await;
        assert_eq!(res.status(), 403, "{header}");
    }
}
