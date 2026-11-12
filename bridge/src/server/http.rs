//! HTTP head/body limits and response helpers for the bridge's hyper server.
//!
//! Reset connections drop their observer. Commands can additionally opt into
//! cancellation on read EOF; ordinary half-closed HTTP clients remain supported.
//! Coordinated workers retain their folder lease through process-group cleanup.

use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::{Response, StatusCode};
use hyper_util::rt::TokioTimer;
use serde::de::DeserializeOwned;

/// Bridge HTTP/WebSocket resource limits.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_head: usize,
    pub max_body: usize,
    pub head_timeout: Duration,
    pub max_connections: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_head: 16 * 1024,
            max_body: 1024 * 1024,
            head_timeout: Duration::from_secs(10),
            max_connections: 256,
        }
    }
}

/// Errors from reading and decoding a JSON request body.
pub enum HttpError {
    /// Body exceeded the configured limit.
    TooLarge,
    /// Malformed body (I/O error or invalid JSON).
    BadRequest(String),
}

/// Build an HTTP/1 connection builder enforcing the bridge's head-size and idle-connection limits.
///
/// Connection-level `keep_alive(false)` is deliberately not set here: hyper stamps
/// `Connection: close` onto every response written on such a connection, including a `101`
/// WebSocket upgrade, which corrupts the RFC 6455 handshake (`Connection: upgrade` is
/// required). Instead, `respond` closes non-upgrade connections by setting the header itself,
/// which hyper honors per-response without touching the upgrade path.
pub fn builder(limits: &Limits) -> http1::Builder {
    let mut builder = http1::Builder::new();
    builder
        .half_close(true)
        .timer(TokioTimer::new())
        .header_read_timeout(limits.head_timeout)
        .max_buf_size(limits.max_head);
    builder
}

/// Read a request body up to `max` bytes.
pub async fn read_body(body: Incoming, max: usize) -> Result<Bytes, HttpError> {
    let collected =
        tokio::time::timeout(Duration::from_secs(30), Limited::new(body, max).collect())
            .await
            .map_err(|_| HttpError::BadRequest("Request body timed out.".into()))?
            .map_err(|err| {
                if err
                    .downcast_ref::<http_body_util::LengthLimitError>()
                    .is_some()
                {
                    HttpError::TooLarge
                } else {
                    HttpError::BadRequest(err.to_string())
                }
            })?;
    Ok(collected.to_bytes())
}

/// Read and JSON-decode a request body, enforcing `max` bytes.
pub async fn read_json<T: DeserializeOwned>(body: Incoming, max: usize) -> Result<T, HttpError> {
    let bytes = read_body(body, max).await?;
    serde_json::from_slice(&bytes)
        .map_err(|err| HttpError::BadRequest(format!("invalid JSON: {err}")))
}

/// Build a JSON response, applying step 08's CORS rules (echoed origin + `Vary: Origin`) when
/// `allowed_origin` is set.
pub fn respond(
    status: StatusCode,
    json: impl Into<Bytes>,
    allowed_origin: Option<&str>,
) -> Response<Full<Bytes>> {
    let mut builder = Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("connection", "close");
    if let Some(origin) = allowed_origin {
        builder = builder
            .header("access-control-allow-origin", origin)
            .header("vary", "Origin");
    }
    builder
        .body(Full::new(json.into()))
        .expect("static headers and JSON body always produce a valid response")
}

/// Bound stalled response writes without limiting command execution or WebSocket lifetime.
pub(super) struct WriteTimeout<T> {
    inner: T,
    timer: Option<std::pin::Pin<Box<tokio::time::Sleep>>>,
}
impl<T> WriteTimeout<T> {
    pub fn new(inner: T) -> Self {
        Self { inner, timer: None }
    }
    fn finish<R>(
        &mut self,
        cx: &mut std::task::Context<'_>,
        result: std::task::Poll<std::io::Result<R>>,
    ) -> std::task::Poll<std::io::Result<R>> {
        use std::future::Future;
        if result.is_ready() {
            self.timer = None;
            return result;
        }
        let timer = self
            .timer
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(Duration::from_secs(30))));
        if timer.as_mut().poll(cx).is_ready() {
            return std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Response write timed out.",
            )));
        }
        result
    }
}
impl<T: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for WriteTimeout<T> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl<T: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for WriteTimeout<T> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let result = std::pin::Pin::new(&mut self.inner).poll_write(cx, buf);
        self.finish(cx, result)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let result = std::pin::Pin::new(&mut self.inner).poll_flush(cx);
        self.finish(cx, result)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let result = std::pin::Pin::new(&mut self.inner).poll_shutdown(cx);
        self.finish(cx, result)
    }
}

type Peer = std::sync::Arc<tokio::net::tcp::OwnedReadHalf>;

/// Sharing Tokio's owned read half permits non-consuming closure observation
/// while hyper is waiting for a service response. No extra socket is opened.
pub(super) fn connection_io(
    stream: tokio::net::TcpStream,
) -> (
    impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    Peer,
) {
    let (read, write) = stream.into_split();
    let read = std::sync::Arc::new(read);
    (
        WriteTimeout::new(tokio::io::join(SharedRead(read.clone()), write)),
        read,
    )
}

struct SharedRead(Peer);
impl tokio::io::AsyncRead for SharedRead {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if buf.remaining() == 0 {
            return std::task::Poll::Ready(Ok(()));
        }
        let stream: &tokio::net::TcpStream = self.0.as_ref().as_ref();
        loop {
            std::task::ready!(stream.poll_read_ready(cx))?;
            match stream.try_read_buf(buf) {
                Ok(_) => return std::task::Poll::Ready(Ok(())),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return std::task::Poll::Ready(Err(error)),
            }
        }
    }
}

pub(super) async fn peer_closed(peer: Peer) {
    let stream: &tokio::net::TcpStream = peer.as_ref().as_ref();
    let mut byte = [0];
    loop {
        let Ok(ready) = stream.ready(tokio::io::Interest::READABLE).await else {
            return;
        };
        if ready.is_read_closed() || ready.is_error() {
            return;
        }
        match stream.peek(&mut byte).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                // Only unexpected pipelined data reaches this branch after
                // the complete body was decoded. Preserve it for hyper and
                // bound observation work until the peer closes its write side.
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test(start_paused = true)]
    async fn stalled_response_write_times_out_without_limiting_idle_reads() {
        let (writer, _reader) = tokio::io::duplex(1);
        let mut writer = WriteTimeout::new(writer);
        let error = writer.write_all(b"ab").await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn peer_closure_is_observed_without_draining_buffered_bytes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut writer = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let (mut reader, peer) = connection_io(stream);
        let mut empty = [];
        assert_eq!(reader.read(&mut empty).await.unwrap(), 0);
        writer.write_all(b"x").await.unwrap();
        writer.shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), peer_closed(peer))
            .await
            .unwrap();
        assert_eq!(reader.read_u8().await.unwrap(), b'x');
        let mut byte = [0];
        assert_eq!(reader.read(&mut byte).await.unwrap(), 0);
    }
}
