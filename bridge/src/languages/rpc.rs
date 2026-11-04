//! Concurrent JSON-RPC over framed stdio. A dedicated writer finishes each
//! frame even if its caller is cancelled; abandoned requests send cancellation.
use super::framing;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufRead, AsyncWrite},
    sync::{mpsc, oneshot, watch},
};

const MAX_PENDING: usize = 8;
const MAX_EVENTS: usize = 128;
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Transport(String),
    #[error("Language server error {code}: {message}")]
    Remote {
        code: i64,
        message: String,
        data: Value,
    },
}

#[derive(Debug)]
pub struct ServerMessage {
    pub method: String,
    pub params: Value,
    pub id: Option<Value>,
}

type Reply = oneshot::Sender<Result<Value, Error>>;
struct Shared {
    pending: Mutex<(u64, BTreeMap<u64, Reply>)>,
    output: mpsc::Sender<Value>,
    closed: watch::Sender<Option<String>>,
}
impl Shared {
    fn fail(&self, error: String) {
        self.closed.send_if_modified(|closed| {
            if closed.is_some() {
                false
            } else {
                *closed = Some(error.clone());
                true
            }
        });
        let pending = std::mem::take(&mut self.pending.lock().unwrap().1);
        for reply in pending.into_values() {
            let _ = reply.send(Err(Error::Transport(error.clone())));
        }
    }
}

struct Session {
    shared: Arc<Shared>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.shared.fail("Language-server connection closed".into());
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Cloneable RPC handle. Native process ownership is held by the host service;
/// this primitive also accepts in-memory streams for protocol contract tests.
#[derive(Clone)]
pub struct Client(Arc<Session>);
impl Client {
    pub fn connect(
        mut writer: impl AsyncWrite + Unpin + Send + 'static,
        mut reader: impl AsyncBufRead + Unpin + Send + 'static,
    ) -> (Self, mpsc::Receiver<ServerMessage>) {
        let (output, mut queued) = mpsc::channel(MAX_PENDING);
        let (closed, mut writer_closed) = watch::channel(None);
        let shared = Arc::new(Shared {
            pending: Mutex::new((0, BTreeMap::new())),
            output,
            closed,
        });
        let sending = Arc::clone(&shared);
        let write_task = tokio::spawn(async move {
            loop {
                if sending.closed.borrow().is_some() {
                    break;
                }
                let message = tokio::select! {
                    _ = writer_closed.changed() => break,
                    message = queued.recv() => match message { Some(message) => message, None => break },
                };
                match tokio::time::timeout(WRITE_TIMEOUT, framing::write(&mut writer, &message))
                    .await
                {
                    Ok(Ok(())) => {}
                    result => {
                        sending.fail(match result {
                            Ok(Err(error)) => error,
                            _ => "Language-server write timed out".into(),
                        });
                        break;
                    }
                }
            }
        });
        let (events, received) = mpsc::channel(MAX_EVENTS);
        let receiving = Arc::clone(&shared);
        let mut reader_closed = shared.closed.subscribe();
        let read_task = tokio::spawn(async move {
            loop {
                if receiving.closed.borrow().is_some() {
                    break;
                }
                let message = tokio::select! {
                    _ = reader_closed.changed() => break,
                    message = framing::read(&mut reader) => message,
                };
                let result = match message {
                    Ok(message) => dispatch(&receiving, &events, message),
                    Err(error) => Err(error),
                };
                if let Err(error) = result {
                    receiving.fail(error);
                    break;
                }
            }
        });
        (
            Self(Arc::new(Session {
                shared,
                tasks: vec![write_task, read_task],
            })),
            received,
        )
    }

    pub fn is_closed(&self) -> bool {
        self.0.shared.closed.borrow().is_some()
    }
    pub fn close(&self) {
        self.0
            .shared
            .fail("Language-server connection closed".into());
    }

    pub async fn send(&self, message: Value) -> Result<(), Error> {
        if let Some(error) = self.0.shared.closed.borrow().clone() {
            return Err(Error::Transport(error));
        }
        tokio::time::timeout(WRITE_TIMEOUT, self.0.shared.output.send(message))
            .await
            .map_err(|_| Error::Transport("Language-server output queue timed out".into()))?
            .map_err(|_| Error::Transport("Language-server output closed".into()))
    }
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), Error> {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}))
            .await
    }
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, Error> {
        let (sender, receiver) = oneshot::channel();
        let id = {
            let mut pending = self.0.shared.pending.lock().unwrap();
            if let Some(error) = self.0.shared.closed.borrow().clone() {
                return Err(Error::Transport(error));
            }
            if pending.1.len() >= MAX_PENDING {
                return Err(Error::Transport(
                    "Too many pending language-server requests".into(),
                ));
            }
            pending.0 = pending
                .0
                .checked_add(1)
                .filter(|id| i32::try_from(*id).is_ok())
                .ok_or_else(|| {
                    Error::Transport("Language-server request sequence exhausted".into())
                })?;
            let id = pending.0;
            pending.1.insert(id, sender);
            id
        };
        let _guard = RequestGuard {
            shared: Arc::clone(&self.0.shared),
            id,
        };
        tokio::time::timeout(timeout, async {
            self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
                .await?;
            receiver
                .await
                .map_err(|_| Error::Transport("Language-server response channel closed".into()))?
        })
        .await
        .map_err(|_| Error::Transport("Language-server request timed out".into()))?
    }
}

#[cfg(test)]
mod tests;

struct RequestGuard {
    shared: Arc<Shared>,
    id: u64,
}
impl Drop for RequestGuard {
    fn drop(&mut self) {
        if self
            .shared
            .pending
            .lock()
            .unwrap()
            .1
            .remove(&self.id)
            .is_none()
        {
            return;
        }
        let shared = Arc::clone(&self.shared);
        let message = json!({"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":self.id}});
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if !matches!(
                    tokio::time::timeout(WRITE_TIMEOUT, shared.output.send(message)).await,
                    Ok(Ok(()))
                ) {
                    shared.fail("Language-server cancellation could not be delivered".into());
                }
            });
        }
    }
}

fn dispatch(
    shared: &Shared,
    events: &mpsc::Sender<ServerMessage>,
    message: Value,
) -> Result<(), String> {
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err("Invalid language-server JSON-RPC version".into());
    }
    if let Some(method) = message.get("method") {
        let method = method
            .as_str()
            .filter(|method| !method.is_empty() && method.len() <= 256)
            .ok_or("Invalid language-server method")?;
        let id = message.get("id").cloned();
        if id
            .as_ref()
            .is_some_and(|id| !id.is_i64() && !id.as_str().is_some_and(|id| id.len() <= 256))
        {
            return Err("Invalid language-server request ID".into());
        }
        return events
            .try_send(ServerMessage {
                method: method.into(),
                params: message.get("params").cloned().unwrap_or(Value::Null),
                id,
            })
            .map_err(|_| "Language-server event queue is full or closed".into());
    }
    let id = message
        .get("id")
        .and_then(Value::as_u64)
        .ok_or("Invalid language-server response ID")?;
    let result = match (message.get("result"), message.get("error")) {
        (Some(result), None) => Ok(result.clone()),
        (None, Some(error)) => Err(Error::Remote {
            code: error
                .get("code")
                .and_then(Value::as_i64)
                .ok_or("Invalid language-server error code")?,
            message: error
                .get("message")
                .and_then(Value::as_str)
                .ok_or("Invalid language-server error message")?
                .into(),
            data: error.get("data").cloned().unwrap_or(Value::Null),
        }),
        _ => return Err("Language-server response must contain a result or error".into()),
    };
    // A cancelled request may respond late. Its opaque numeric ID is never
    // reassigned to a newer request and cannot satisfy another caller.
    if let Some(reply) = shared.pending.lock().unwrap().1.remove(&id) {
        let _ = reply.send(result);
    }
    Ok(())
}
