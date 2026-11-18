use crate::backend::Api;
use crate::idb;
use leptos::prelude::WithValue;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BridgeConfig {
    pub ws_url: String,
    pub http_url: String,
}

impl BridgeConfig {
    pub fn new(url: &str) -> Self {
        let ws_url = url.trim().trim_end_matches('/').to_string();
        let http_url = if ws_url.starts_with("wss://") {
            ws_url.replacen("wss://", "https://", 1)
        } else if ws_url.starts_with("ws://") {
            ws_url.replacen("ws://", "http://", 1)
        } else {
            // Default to http if no scheme
            format!("http://{ws_url}")
        };
        Self { ws_url, http_url }
    }
}

pub fn default_bridge_url() -> String {
    web_sys::window().map_or_else(
        || "ws://127.0.0.1:3001".into(),
        |window| {
            let location = window.location();
            crate::bridge_address::default_address(
                &location.protocol().unwrap_or_default(),
                &location.host().unwrap_or_default(),
                &location.hostname().unwrap_or_else(|_| "127.0.0.1".into()),
            )
        },
    )
}

#[derive(Clone)]
pub struct BridgeCredentials {
    api: Api,
    cached_token: Arc<Mutex<Option<(String, i64)>>>,
}

impl BridgeCredentials {
    pub fn new(api: Api) -> Self {
        Self {
            api,
            cached_token: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn credential(&self) -> Result<String, String> {
        if let Ok(Some(pt)) = idb::get_bridge_pairing_token().await
            && !pt.trim().is_empty()
        {
            return Ok(pt);
        }

        #[allow(clippy::cast_possible_truncation)]
        // JS timestamps are fractional seconds; the cast saturates to i64.
        let now = (js_sys::Date::now() / 1000.0) as i64;
        let needs_refresh = {
            let cache = self.cached_token.lock().unwrap();
            match &*cache {
                Some((_, expires_at)) => now >= *expires_at - 30,
                None => true,
            }
        };

        if needs_refresh {
            let api = self
                .api
                .try_with_value(Clone::clone)
                .ok_or("Bridge request superseded.")?;
            let (token, expires_at) = api.bridge_token().await?;
            *self.cached_token.lock().unwrap() = Some((token.clone(), expires_at));
            Ok(token)
        } else {
            let cache = self.cached_token.lock().unwrap();
            Ok(cache.as_ref().unwrap().0.clone())
        }
    }

    pub fn clear_cache(&self) {
        *self.cached_token.lock().unwrap() = None;
    }
}

use futures::{
    SinkExt, StreamExt,
    channel::{mpsc, oneshot},
    future::{AbortHandle, LocalBoxFuture},
};
use leptos::prelude::*;
use openwebide_core::{BridgeClientMessage, BridgeServerMessage};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BridgeStatus {
    Connecting,
    Ready { runs: bool },
    Legacy,
    Rejected,
    Unavailable,
}

impl BridgeStatus {
    pub fn terminal_ready(self) -> bool {
        matches!(self, Self::Ready { .. } | Self::Legacy)
    }
}

pub trait BridgeSocket {
    fn send(&self, text: String) -> Result<(), String>;
}

pub trait BridgeTransport {
    fn open(
        &self,
        url: &str,
        on_message: Rc<dyn Fn(String)>,
        on_close: Rc<dyn Fn()>,
    ) -> Result<Rc<dyn BridgeSocket>, String>;
}

pub struct GlooTransport;

struct GlooSocket {
    sender: mpsc::UnboundedSender<String>,
}

impl BridgeSocket for GlooSocket {
    fn send(&self, text: String) -> Result<(), String> {
        self.sender
            .unbounded_send(text)
            .map_err(|error| error.to_string())
    }
}

impl Drop for GlooSocket {
    fn drop(&mut self) {
        self.sender.close_channel();
    }
}

impl BridgeTransport for GlooTransport {
    fn open(
        &self,
        url: &str,
        on_message: Rc<dyn Fn(String)>,
        on_close: Rc<dyn Fn()>,
    ) -> Result<Rc<dyn BridgeSocket>, String> {
        use gloo_net::websocket::{Message, futures::WebSocket};
        let socket = WebSocket::open(url).map_err(|error| error.to_string())?;
        let (mut writer, mut reader) = socket.split();
        let (sender, mut receiver) = mpsc::unbounded::<String>();
        let task = async move {
            let write = async move {
                while let Some(text) = receiver.next().await {
                    if writer.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
                let _ = writer.close().await;
            };
            let read = async move {
                while let Some(Ok(message)) = reader.next().await {
                    match message {
                        Message::Text(text) => on_message(text),
                        Message::Bytes(bytes) => {
                            on_message(String::from_utf8_lossy(&bytes).into_owned());
                        }
                    }
                }
            };
            futures::pin_mut!(write, read);
            let _ = futures::future::select(write, read).await;
            on_close();
        };
        leptos::task::spawn_local(task);
        Ok(Rc::new(GlooSocket { sender }))
    }
}

type MessageCallback = Rc<dyn Fn(BridgeServerMessage)>;
pub type CredentialSource = Rc<dyn Fn() -> LocalBoxFuture<'static, Result<String, String>>>;

static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);

struct BridgeInner {
    status: ArcRwSignal<BridgeStatus>,
    shells: RefCell<HashSet<String>>,
    pending_kills: RefCell<HashSet<String>>,
    closing: Cell<bool>,
    closed: Cell<bool>,
    cleanup_connection: RefCell<Option<Rc<BridgeInner>>>,
    socket: RefCell<Option<Rc<dyn BridgeSocket>>>,
    connection_id: Cell<u64>,
    terminal: RefCell<Option<MessageCallback>>,
    runs: RefCell<HashMap<String, MessageCallback>>,
    completions: RefCell<HashMap<String, mpsc::UnboundedSender<BridgeServerMessage>>>,
    abort: RefCell<Option<AbortHandle>>,
}

impl Drop for BridgeInner {
    fn drop(&mut self) {
        if let Some(abort) = self.abort.get_mut().take() {
            abort.abort();
        }
    }
}

#[derive(Clone)]
pub struct BridgeConn(Rc<BridgeInner>);

impl BridgeConn {
    pub fn new(config: BridgeConfig, credentials: BridgeCredentials) -> Self {
        Self::with_transport(
            config,
            Rc::new(GlooTransport),
            Rc::new(move || {
                let credentials = credentials.clone();
                Box::pin(async move {
                    credentials.clear_cache();
                    credentials.credential().await
                })
            }),
        )
    }

    pub fn with_transport(
        config: BridgeConfig,
        transport: Rc<dyn BridgeTransport>,
        credential: CredentialSource,
    ) -> Self {
        let inner = Rc::new(BridgeInner {
            status: ArcRwSignal::new(BridgeStatus::Connecting),
            shells: RefCell::new(HashSet::new()),
            pending_kills: RefCell::new(HashSet::new()),
            closing: Cell::new(false),
            closed: Cell::new(false),
            cleanup_connection: RefCell::new(None),
            socket: RefCell::new(None),
            connection_id: Cell::new(0),
            terminal: RefCell::new(None),
            runs: RefCell::new(HashMap::new()),
            completions: RefCell::new(HashMap::new()),
            abort: RefCell::new(None),
        });
        let weak = Rc::downgrade(&inner);
        let (task, abort) = futures::future::abortable(async move {
            let mut attempts = 0usize;
            loop {
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                inner.status.set(BridgeStatus::Connecting);
                drop(inner);
                let token = credential().await;
                let (closed_tx, closed_rx) = oneshot::channel();
                let closed_tx = RefCell::new(Some(closed_tx));
                let message_weak = weak.clone();
                let close_weak = weak.clone();
                let socket = match token {
                    Ok(token) => transport
                        .open(
                            &config.ws_url,
                            Rc::new(move |text| {
                                if let Some(inner) = message_weak.upgrade()
                                    && let Ok(message) = serde_json::from_str(&text)
                                {
                                    Self::route(&inner, message);
                                }
                            }),
                            Rc::new(move || {
                                if let Some(inner) = close_weak.upgrade()
                                    && !inner.closed.get()
                                {
                                    inner.status.set(BridgeStatus::Unavailable);
                                }
                                if let Some(sender) = closed_tx.borrow_mut().take() {
                                    let _ = sender.send(());
                                }
                            }),
                        )
                        .and_then(|socket| {
                            socket.send(
                                serde_json::to_string(&BridgeClientMessage::Hello { token })
                                    .map_err(|error| error.to_string())?,
                            )?;
                            Ok(socket)
                        }),
                    Err(error) => Err(error),
                };
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                if let Ok(socket) = socket {
                    inner
                        .connection_id
                        .set(NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed));
                    *inner.socket.borrow_mut() = Some(socket);
                    drop(inner);
                    let timeout = crate::util::sleep_ms(2000);
                    futures::pin_mut!(timeout, closed_rx);
                    if let futures::future::Either::Right(((), closed_rx)) =
                        futures::future::select(closed_rx, timeout).await
                    {
                        if let Some(inner) = weak.upgrade() {
                            if inner.status.get_untracked() == BridgeStatus::Connecting {
                                inner.status.set(BridgeStatus::Legacy);
                                Self::flush_kills(&inner);
                            }
                            if inner.status.get_untracked().terminal_ready() {
                                attempts = 0;
                            }
                        }
                        let _ = closed_rx.await;
                    }
                } else {
                    inner.status.set(BridgeStatus::Unavailable);
                    drop(inner);
                }
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                inner.socket.borrow_mut().take();
                inner.completions.borrow_mut().clear();
                drop(inner);
                let delay = [1000, 2000, 5000, 10000]
                    .get(attempts)
                    .copied()
                    .unwrap_or(30000);
                attempts = attempts.saturating_add(1);
                crate::util::sleep_ms(delay).await;
            }
        });
        *inner.abort.borrow_mut() = Some(abort);
        leptos::task::spawn_local(async move {
            let _ = task.await;
        });
        Self(inner)
    }

    pub fn status(&self) -> ArcRwSignal<BridgeStatus> {
        self.0.status.clone()
    }

    pub(crate) fn connection_id(&self) -> u64 {
        self.0.connection_id.get()
    }

    pub fn send(&self, message: BridgeClientMessage) -> Result<(), String> {
        if self.0.closing.get() || !self.0.status.get_untracked().terminal_ready() {
            return Err("bridge is not ready".into());
        }
        let text = serde_json::to_string(&message).map_err(|error| error.to_string())?;
        self.0
            .socket
            .borrow()
            .as_ref()
            .ok_or("bridge is disconnected")?
            .send(text)?;
        if let BridgeClientMessage::Spawn { id, .. } = message {
            self.0.shells.borrow_mut().insert(id);
        }
        Ok(())
    }

    pub fn kill_shell(&self, id: String) {
        if self.0.closed.get() {
            return;
        }
        self.0.pending_kills.borrow_mut().insert(id);
        Self::flush_kills(&self.0);
    }

    fn flush_kills(inner: &Rc<BridgeInner>) {
        if !inner.status.get_untracked().terminal_ready() {
            return;
        }
        let ids = inner
            .pending_kills
            .borrow()
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for id in ids {
            let message = BridgeClientMessage::Kill {
                id: id.clone(),
                signal: None,
            };
            let sent = inner.socket.borrow().as_ref().is_some_and(|socket| {
                socket
                    .send(serde_json::to_string(&message).unwrap())
                    .is_ok()
            });
            if !sent {
                return;
            }
            inner.pending_kills.borrow_mut().remove(&id);
            inner.shells.borrow_mut().remove(&id);
        }
        if inner.closing.get() {
            Self::finish_close(inner);
        }
    }

    fn finish_close(inner: &BridgeInner) {
        inner.closed.set(true);
        if let Some(abort) = inner.abort.borrow_mut().take() {
            abort.abort();
        }
        inner.socket.borrow_mut().take();
        inner.status.set(BridgeStatus::Unavailable);
        inner.cleanup_connection.borrow_mut().take();
    }

    pub fn register_terminal(&self, callback: MessageCallback) {
        *self.0.terminal.borrow_mut() = Some(callback);
    }
    pub fn unregister_terminal(&self) {
        self.0.terminal.borrow_mut().take();
    }
    pub fn register_run(&self, run_id: String, callback: MessageCallback) {
        self.0.runs.borrow_mut().insert(run_id, callback);
    }
    pub fn unregister_run(&self, run_id: &str) {
        self.0.runs.borrow_mut().remove(run_id);
    }
    pub fn register_completion(&self, id: String) -> mpsc::UnboundedReceiver<BridgeServerMessage> {
        let (sender, receiver) = mpsc::unbounded();
        self.0.completions.borrow_mut().insert(id, sender);
        receiver
    }
    pub fn unregister_completion(&self, id: &str) {
        self.0.completions.borrow_mut().remove(id);
    }

    pub fn close(&self) {
        if self.0.closing.replace(true) {
            return;
        }
        self.0
            .pending_kills
            .borrow_mut()
            .extend(self.0.shells.borrow().iter().cloned());
        self.0.terminal.borrow_mut().take();
        self.0.runs.borrow_mut().clear();
        self.0.completions.borrow_mut().clear();
        if self.0.pending_kills.borrow().is_empty() {
            Self::finish_close(&self.0);
        } else {
            // Retain cleanup across disconnects even after the last pane has unmounted.
            *self.0.cleanup_connection.borrow_mut() = Some(self.0.clone());
            Self::flush_kills(&self.0);
        }
    }

    fn route(inner: &Rc<BridgeInner>, message: BridgeServerMessage) {
        if inner.closed.get() {
            return;
        }
        if let BridgeServerMessage::Exited { id, .. } = &message {
            inner.shells.borrow_mut().remove(id);
        }
        match &message {
            BridgeServerMessage::HelloOk { runs, .. } => {
                inner.status.set(BridgeStatus::Ready { runs: *runs });
                Self::flush_kills(inner);
            }
            BridgeServerMessage::HelloError { .. } => inner.status.set(BridgeStatus::Rejected),
            BridgeServerMessage::Error { .. }
                if inner.status.get_untracked() == BridgeStatus::Connecting =>
            {
                inner.status.set(BridgeStatus::Legacy);
                Self::flush_kills(inner);
            }
            BridgeServerMessage::Error { id, .. } if inner.runs.borrow().contains_key(id) => {
                let callback = inner.runs.borrow().get(id).cloned();
                if let Some(callback) = callback {
                    callback(message);
                }
            }
            BridgeServerMessage::RunEvent { run_id, .. }
            | BridgeServerMessage::RunSnapshot { run_id, .. }
            | BridgeServerMessage::RunRejected { run_id, .. } => {
                let callback = inner.runs.borrow().get(run_id).cloned();
                if let Some(callback) = callback {
                    callback(message);
                }
            }
            BridgeServerMessage::Runs { .. } => {
                let callbacks = inner.runs.borrow().values().cloned().collect::<Vec<_>>();
                for callback in callbacks {
                    callback(message.clone());
                }
            }
            BridgeServerMessage::CompletionChunk { id, .. }
            | BridgeServerMessage::CompletionEnd { id, .. } => {
                if let Some(sender) = inner.completions.borrow().get(id) {
                    let _ = sender.unbounded_send(message.clone());
                }
                if matches!(message, BridgeServerMessage::CompletionEnd { .. }) {
                    inner.completions.borrow_mut().remove(id);
                }
            }
            _ => {
                let callback = inner.terminal.borrow().clone();
                if let Some(callback) = callback {
                    callback(message);
                }
            }
        }
    }
}
