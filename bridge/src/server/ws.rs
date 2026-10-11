use super::ServerConfig;
use crate::terminals::{headless::spawn_headless, pty::spawn_pty, session::SessionManager};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use openwebide_core::{BridgeClientMessage, BridgeServerMessage};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
const WS_PING_INTERVAL: Duration = Duration::from_secs(30);
const WS_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

pub(crate) enum WriterCmd {
    Send(Box<BridgeServerMessage>),
    Attach {
        session: std::sync::Arc<crate::terminals::session::Session>,
        after_seq: u64,
    },
}

impl WriterCmd {
    pub(crate) fn send(message: BridgeServerMessage) -> Self {
        Self::Send(Box::new(message))
    }
}

#[tracing::instrument(skip_all)]
pub(super) async fn handle_websocket<S>(
    ws_stream: tokio_tungstenite::WebSocketStream<S>,
    sessions: SessionManager,
    config: ServerConfig,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut ws_tx, mut ws_rx) = ws_stream.split();
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<WriterCmd>(256);

    let writer_task = tokio::spawn(async move {
        let mut attached_session: Option<Arc<crate::terminals::session::Session>> = None;
        let mut attached_cursor = 0;
        let mut watch_rx: Option<tokio::sync::watch::Receiver<u64>> = None;

        let mut ping_interval =
            tokio::time::interval_at(Instant::now() + WS_PING_INTERVAL, WS_PING_INTERVAL);
        ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            let watch_future = async {
                if let Some(rx) = watch_rx.as_mut() {
                    let _ = rx.changed().await;
                } else {
                    std::future::pending::<()>().await;
                }
            };

            tokio::select! {
                _ = ping_interval.tick() => {
                    if ws_tx.send(Message::Ping(Bytes::new())).await.is_err() {
                        break;
                    }
                }

                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(WriterCmd::Send(msg)) => {
                            if let Ok(json_str) = serde_json::to_string(&msg)
                                && ws_tx.send(Message::Text(json_str.into())).await.is_err()
                            {
                                break;
                            }
                        }
                        Some(WriterCmd::Attach { session, after_seq }) => {
                            attached_session = Some(session.clone());
                            watch_rx = Some(session.ring.lock().unwrap().subscribe());
                            attached_cursor = after_seq;

                            // Immediately drain the ring from `after_seq`
                            loop {
                                let (batch, truncated) = session.ring.lock().unwrap().read_after(attached_cursor, 256);
                                if let Some(dropped) = truncated {
                                    let err = BridgeServerMessage::Error {
                                        id: session.id.clone(),
                                        message: format!("output truncated: {dropped} chunks dropped"),
                                    };
                                    if ws_tx.send(Message::Text(serde_json::to_string(&err).unwrap().into())).await.is_err() { return; }
                                    attached_cursor = session.ring.lock().unwrap().first_seq().saturating_sub(1);
                                    continue;
                                }
                                if batch.is_empty() {
                                    break;
                                }
                                for (seq, msg) in batch {
                                    if let Ok(json) = serde_json::to_string(&msg)
                                        && ws_tx.send(Message::Text(json.into())).await.is_err()
                                    {
                                        return;
                                    }
                                    attached_cursor = seq;
                                }
                            }
                        }
                        None => break,
                    }
                }

                () = watch_future => {
                    if let Some(sess) = &attached_session {
                        loop {
                            let (batch, truncated) = sess.ring.lock().unwrap().read_after(attached_cursor, 256);
                            if let Some(dropped) = truncated {
                                let err = BridgeServerMessage::Error {
                                    id: sess.id.clone(),
                                    message: format!("output truncated: {dropped} chunks dropped"),
                                };
                                if ws_tx.send(Message::Text(serde_json::to_string(&err).unwrap().into())).await.is_err() { return; }
                                attached_cursor = sess.ring.lock().unwrap().first_seq().saturating_sub(1);
                                continue;
                            }
                            if batch.is_empty() {
                                break;
                            }
                            for (seq, msg) in batch {
                                if let Ok(json) = serde_json::to_string(&msg)
                                    && ws_tx.send(Message::Text(json.into())).await.is_err()
                                {
                                    return;
                                }
                                attached_cursor = seq;
                            }
                        }
                    }
                }
            }
        }
    });

    let http = crate::runs::http_client::ReqwestHttpClient::default();
    let backend = Arc::new(crate::runs::backend_client::BackendClient::new(
        config.backend_url.clone(),
        config.secret.clone(),
        http.clone(),
    ));
    let mut connection = Connection {
        sessions,
        config,
        http,
        backend,
        cmd_tx: cmd_tx.clone(),
        forwarders: Arc::new(Mutex::new(std::collections::HashMap::new())),
        completions: std::collections::HashMap::new(),
        starts: tokio::task::JoinSet::new(),
        attached_id: None,
        principal: None,
    };
    let start_time = std::time::Instant::now();

    loop {
        let timeout_dur = if connection.principal.is_none() {
            let elapsed = start_time.elapsed();
            if elapsed >= Duration::from_secs(10) {
                break;
            }
            Duration::from_secs(10) - elapsed
        } else {
            WS_IDLE_TIMEOUT
        };

        let inbound = tokio::select! {
            () = cmd_tx.closed() => break,
            inbound = tokio::time::timeout(timeout_dur, ws_rx.next()) => inbound,
        };

        let msg_res = match inbound {
            Ok(Some(m)) => m,
            Ok(None) => break,
            Err(_) => break, // Idle timeout
        };

        let Ok(msg) = msg_res else {
            break;
        };

        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Binary(b) => String::from_utf8_lossy(&b).to_string(),
            Message::Ping(_) => {
                // tungstenite auto-replies to ping
                continue;
            }
            Message::Pong(_) => continue,
            Message::Close(_) => break,
            Message::Frame(_) => continue,
        };

        if connection.principal.is_none() && text.len() > 65536 {
            break;
        }

        let client_msg: BridgeClientMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                let err = BridgeServerMessage::Error {
                    id: connection.attached_id.clone().unwrap_or_default(),
                    message: format!("invalid message: {e}"),
                };
                if cmd_tx.send(WriterCmd::send(err)).await.is_err() {
                    break;
                }
                continue;
            }
        };

        while connection.starts.try_join_next().is_some() {}
        connection.completions.retain(|_, task| !task.is_finished());
        connection
            .forwarders
            .lock()
            .unwrap()
            .retain(|_, task| !task.is_finished());
        if !cfg!(feature = "tls")
            || !matches!(
                connection.principal,
                Some(crate::auth::Principal::User { .. })
            )
        {
            let rejection = match &client_msg {
                BridgeClientMessage::RunStart { run_id, .. }
                | BridgeClientMessage::RunAttach { run_id, .. }
                | BridgeClientMessage::RunCancel { run_id }
                | BridgeClientMessage::RunPermission { run_id, .. } => {
                    Some(BridgeServerMessage::RunRejected {
                        run_id: run_id.clone(),
                        code: openwebide_core::RunRejectCode::Unauthorized,
                        message: "unauthorized: runs unavailable for this principal".into(),
                    })
                }
                BridgeClientMessage::RunList { .. } => Some(BridgeServerMessage::RunRejected {
                    run_id: String::new(),
                    code: openwebide_core::RunRejectCode::Unauthorized,
                    message: "unauthorized: runs unavailable for this principal".into(),
                }),
                BridgeClientMessage::CompletionStart { id, .. }
                | BridgeClientMessage::CompletionCancel { id } => {
                    Some(BridgeServerMessage::CompletionEnd {
                        id: id.clone(),
                        error: Some("unauthorized".into()),
                    })
                }
                _ => None,
            };
            if let Some(rejection) = rejection {
                if cmd_tx.send(WriterCmd::send(rejection)).await.is_err() {
                    break;
                }
                continue;
            }
        }
        if connection.principal.is_none()
            && !matches!(client_msg, BridgeClientMessage::Hello { .. })
        {
            let id = match &client_msg {
                BridgeClientMessage::Spawn { id, .. } => id.clone(),
                BridgeClientMessage::Input { id, .. } => id.clone(),
                BridgeClientMessage::Resize { id, .. } => id.clone(),
                BridgeClientMessage::Kill { id, .. } => id.clone(),
                BridgeClientMessage::Attach { id, .. } => id.clone(),
                _ => "".to_string(),
            };
            let err = BridgeServerMessage::Error {
                id,
                message: "unauthorized: send hello first".to_string(),
            };
            if cmd_tx.send(WriterCmd::send(err)).await.is_err() {
                break;
            }
            continue;
        }

        let result = match client_msg {
            message @ BridgeClientMessage::Hello { .. } => connection.on_hello(message).await,
            message @ BridgeClientMessage::RunStart { .. } => {
                connection.on_run_start(message).await
            }
            message @ BridgeClientMessage::RunAttach { .. } => {
                connection.on_run_attach(message).await
            }
            message @ BridgeClientMessage::RunCancel { .. } => {
                connection.on_run_cancel(message).await
            }
            message @ BridgeClientMessage::RunPermission { .. } => {
                connection.on_run_permission(message).await
            }
            message @ BridgeClientMessage::RunList { .. } => connection.on_run_list(message).await,
            message @ BridgeClientMessage::CompletionStart { .. } => {
                connection.on_completion_start(message).await
            }
            message @ BridgeClientMessage::CompletionCancel { .. } => {
                connection.on_completion_cancel(message).await
            }
            message @ BridgeClientMessage::Spawn { .. } => connection.on_spawn(message).await,
            message @ BridgeClientMessage::Input { .. } => connection.on_input(message).await,
            message @ BridgeClientMessage::Resize { .. } => connection.on_resize(message).await,
            message @ BridgeClientMessage::Kill { .. } => connection.on_kill(message).await,
            message @ BridgeClientMessage::Attach { .. } => connection.on_attach(message).await,
            message @ BridgeClientMessage::List => connection.on_list(message).await,
        };
        if result.is_err() {
            break;
        }
    }
    for (_, task) in connection.forwarders.lock().unwrap().drain() {
        task.abort();
    }
    for task in connection.completions.into_values() {
        task.abort();
    }
    // Planning may already have persisted the user message; let accepted starts finish.
    connection.starts.detach_all();
    writer_task.abort();
}

struct Connection {
    sessions: SessionManager,
    config: ServerConfig,
    http: crate::runs::http_client::ReqwestHttpClient,
    backend: Arc<crate::runs::backend_client::BackendClient>,
    cmd_tx: tokio::sync::mpsc::Sender<WriterCmd>,
    forwarders: Arc<Mutex<std::collections::HashMap<String, tokio::task::JoinHandle<()>>>>,
    completions: std::collections::HashMap<String, tokio::task::JoinHandle<()>>,
    starts: tokio::task::JoinSet<()>,
    attached_id: Option<String>,
    principal: Option<crate::auth::Principal>,
}
impl Connection {
    async fn on_hello(
        &mut self,
        message: BridgeClientMessage,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<WriterCmd>> {
        let BridgeClientMessage::Hello { token } = message else {
            unreachable!()
        };
        let Self {
            config,
            principal,
            cmd_tx,
            ..
        } = self;
        if principal.is_some() {
            cmd_tx
                .send(WriterCmd::send(BridgeServerMessage::HelloError {
                    message: "already authenticated".into(),
                }))
                .await?;
            return Ok(());
        }
        let now = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap_or(i64::MAX);
        match crate::auth::authenticate(&token, config, now) {
            Ok(p) => {
                let user_id = match p {
                    crate::auth::Principal::User { user_id } => Some(user_id),
                    crate::auth::Principal::Paired => None,
                };
                *principal = Some(p);
                cmd_tx
                    .send(WriterCmd::send(BridgeServerMessage::HelloOk {
                        user_id,
                        protocol: 1,
                        runs: cfg!(feature = "tls") && user_id.is_some(),
                    }))
                    .await?;
            }
            Err(e) => {
                cmd_tx
                    .send(WriterCmd::send(BridgeServerMessage::HelloError {
                        message: e,
                    }))
                    .await?;
            }
        }

        Ok(())
    }
    async fn on_run_start(
        &mut self,
        message: BridgeClientMessage,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<WriterCmd>> {
        let BridgeClientMessage::RunStart {
            run_id,
            session_id,
            content,
            model,
            editor_context,
            browser_preferences,
            queued_prompt,
        } = message
        else {
            unreachable!()
        };
        let Self {
            config,
            http,
            backend,
            forwarders,
            starts,
            principal,
            cmd_tx,
            ..
        } = self;
        if !cfg!(feature = "tls") || !matches!(principal, Some(crate::auth::Principal::User { .. }))
        {
            cmd_tx
                .send(WriterCmd::send(BridgeServerMessage::RunRejected {
                    run_id,
                    code: openwebide_core::RunRejectCode::Unauthorized,
                    message: "unauthorized".into(),
                }))
                .await?;
            return Ok(());
        }
        let principal = principal.clone().unwrap();
        let config = config.clone();
        let backend = backend.clone();
        let http = http.clone();
        let sender = cmd_tx.clone();
        let forwarders = forwarders.clone();
        let start = crate::runs::StartRun {
            host_path: None,
            run_id: run_id.clone(),
            session_id,
            content,
            model,
            editor_context,
            browser_preferences,
            queued_prompt,
        };
        let run = match config.runs.reserve(&principal, &start) {
            Ok(run) => run,
            Err((code, message)) => {
                sender
                    .send(WriterCmd::send(BridgeServerMessage::RunRejected {
                        run_id,
                        code,
                        message,
                    }))
                    .await?;
                return Ok(());
            }
        };
        starts.spawn(async move {
            match config
                .runs
                .prepare(
                    run,
                    start,
                    &config.workspace_root,
                    backend,
                    |plan| {
                        let memo = config.tool_stream_memos.get_or_insert(&plan.connection);
                        openwebide_llm::registry::Provider::for_connection_with_memo(
                            &plan.connection,
                            http.with_transport(plan.transport.clone()),
                            memo,
                        )
                    },
                    crate::runs::RunHost {
                        execution: config.execution.clone(),
                        plugins: crate::plugins::transport::PluginExecutionHost {
                            paired: config.pairing_token.is_some(),
                            installer: config.plugins.clone(),
                            invocations: config.plugin_invocations.clone(),
                        },
                    },
                )
                .await
            {
                Ok(run) => {
                    let mut tasks = forwarders.lock().unwrap();
                    if let Some(old) = tasks.remove(&run_id) {
                        old.abort();
                    }
                    tasks.insert(run_id, tokio::spawn(run.forward(Some(0), sender)));
                }
                Err((code, message)) => {
                    let _ = sender
                        .send(WriterCmd::send(BridgeServerMessage::RunRejected {
                            run_id,
                            code,
                            message,
                        }))
                        .await;
                }
            }
        });

        Ok(())
    }
    async fn on_run_attach(
        &mut self,
        message: BridgeClientMessage,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<WriterCmd>> {
        let BridgeClientMessage::RunAttach { run_id, last_seq } = message else {
            unreachable!()
        };
        let Self {
            config,
            forwarders,
            principal,
            cmd_tx,
            ..
        } = self;
        let run = principal
            .as_ref()
            .filter(|_| cfg!(feature = "tls"))
            .ok_or_else(|| "run not found".to_string())
            .and_then(|p| config.runs.get(p, &run_id));
        match run {
            Ok(run) => {
                if let Some(old) = forwarders.lock().unwrap().remove(&run_id) {
                    old.abort();
                }
                forwarders
                    .lock()
                    .unwrap()
                    .insert(run_id, tokio::spawn(run.forward(last_seq, cmd_tx.clone())));
            }
            Err(message) => {
                cmd_tx
                    .send(WriterCmd::send(BridgeServerMessage::Error {
                        id: run_id,
                        message,
                    }))
                    .await?;
            }
        }

        Ok(())
    }
    async fn on_run_cancel(
        &mut self,
        message: BridgeClientMessage,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<WriterCmd>> {
        let BridgeClientMessage::RunCancel { run_id } = message else {
            unreachable!()
        };
        let Self {
            config,
            principal,
            cmd_tx,
            ..
        } = self;
        match principal
            .as_ref()
            .filter(|_| cfg!(feature = "tls"))
            .ok_or_else(|| "run not found".to_string())
            .and_then(|p| config.runs.get(p, &run_id))
        {
            Ok(run) => run.cancel.cancel(),
            Err(message) => {
                cmd_tx
                    .send(WriterCmd::send(BridgeServerMessage::Error {
                        id: run_id,
                        message,
                    }))
                    .await?;
            }
        }

        Ok(())
    }
    async fn on_run_permission(
        &mut self,
        message: BridgeClientMessage,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<WriterCmd>> {
        let BridgeClientMessage::RunPermission {
            run_id,
            tool_call_id,
            approved,
        } = message
        else {
            unreachable!()
        };
        let Self {
            config,
            principal,
            cmd_tx,
            ..
        } = self;
        let result = principal
            .as_ref()
            .filter(|_| cfg!(feature = "tls"))
            .ok_or_else(|| "run not found".to_string())
            .and_then(|p| config.runs.get(p, &run_id))
            .and_then(|run| run.gate.decide(&tool_call_id, approved));
        if let Err(message) = result {
            cmd_tx
                .send(WriterCmd::send(BridgeServerMessage::Error {
                    id: run_id,
                    message,
                }))
                .await?;
        }

        Ok(())
    }
    async fn on_run_list(
        &mut self,
        message: BridgeClientMessage,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<WriterCmd>> {
        let BridgeClientMessage::RunList { session_id } = message else {
            unreachable!()
        };
        let Self {
            config,
            principal,
            cmd_tx,
            ..
        } = self;
        match principal.as_ref().filter(|_| cfg!(feature = "tls")) {
            Some(p @ crate::auth::Principal::User { .. }) => {
                cmd_tx
                    .send(WriterCmd::send(BridgeServerMessage::Runs {
                        session_id,
                        runs: config.runs.list(p, session_id),
                    }))
                    .await?;
            }
            _ => {
                cmd_tx
                    .send(WriterCmd::send(BridgeServerMessage::RunRejected {
                        run_id: String::new(),
                        code: openwebide_core::RunRejectCode::Unauthorized,
                        message: "unauthorized".into(),
                    }))
                    .await?;
            }
        }

        Ok(())
    }
    async fn on_completion_start(
        &mut self,
        message: BridgeClientMessage,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<WriterCmd>> {
        let BridgeClientMessage::CompletionStart { id, request } = message else {
            unreachable!()
        };
        let Self {
            config,
            http,
            backend,
            completions,
            principal,
            cmd_tx,
            ..
        } = self;
        if !cfg!(feature = "tls") || !matches!(principal, Some(crate::auth::Principal::User { .. }))
        {
            cmd_tx
                .send(WriterCmd::send(BridgeServerMessage::CompletionEnd {
                    id,
                    error: Some("unauthorized".into()),
                }))
                .await?;
            return Ok(());
        }
        if let Some(old) = completions.remove(&id) {
            old.abort();
        }
        completions.insert(
            id.clone(),
            tokio::spawn(crate::runs::complete(
                principal.clone().unwrap(),
                id,
                request,
                backend.clone(),
                http.clone(),
                config.tool_stream_memos.clone(),
                cmd_tx.clone(),
            )),
        );

        Ok(())
    }
    async fn on_completion_cancel(
        &mut self,
        message: BridgeClientMessage,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<WriterCmd>> {
        let BridgeClientMessage::CompletionCancel { id } = message else {
            unreachable!()
        };
        let Self { completions, .. } = self;
        if let Some(task) = completions.remove(&id) {
            task.abort();
        }

        Ok(())
    }
    async fn on_spawn(
        &mut self,
        message: BridgeClientMessage,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<WriterCmd>> {
        let BridgeClientMessage::Spawn {
            id,
            command,
            args,
            cwd,
            env,
            pty,
            cols,
            rows,
        } = message
        else {
            unreachable!()
        };
        let Self {
            sessions,
            config,
            attached_id,
            cmd_tx,
            ..
        } = self;
        if sessions.get(&id).is_some() {
            let err = BridgeServerMessage::Error {
                id,
                message: "session id already exists".to_string(),
            };
            cmd_tx.send(WriterCmd::send(err)).await?;
            return Ok(());
        }

        let result = crate::exec::SpawnSpec::in_root(
            command,
            args,
            cwd.as_deref(),
            env,
            &config.workspace_root,
        )
        .map_err(|error| error.to_string())
        .and_then(|spec| {
            if pty {
                spawn_pty(id.clone(), spec, cols, rows)
            } else {
                spawn_headless(id.clone(), spec)
            }
        });

        match result {
            Ok(session) => {
                if sessions.try_insert(session.clone()).is_err() {
                    // Race lost
                    session.try_kill(crate::exec::proc::Signal::Kill);
                    let err = BridgeServerMessage::Error {
                        id,
                        message: "session id already exists".to_string(),
                    };
                    cmd_tx.send(WriterCmd::send(err)).await?;
                    return Ok(());
                }

                let spawned = BridgeServerMessage::Spawned {
                    id: id.clone(),
                    pid: session.pid.and_then(|p| u32::try_from(p).ok()).unwrap_or(0),
                    pty,
                };
                cmd_tx.send(WriterCmd::send(spawned)).await?;

                *attached_id = Some(id.clone());
                cmd_tx
                    .send(WriterCmd::Attach {
                        session,
                        after_seq: 0,
                    })
                    .await?;
            }
            Err(e) => {
                let err = BridgeServerMessage::Error { id, message: e };
                cmd_tx.send(WriterCmd::send(err)).await?;
            }
        }

        Ok(())
    }
    async fn on_input(
        &mut self,
        message: BridgeClientMessage,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<WriterCmd>> {
        let BridgeClientMessage::Input { id, data } = message else {
            unreachable!()
        };
        let Self {
            sessions, cmd_tx, ..
        } = self;
        if let Some(sess) = sessions.get(&id)
            && let Err(e) = sess.try_send_input(data)
        {
            let err = BridgeServerMessage::Error {
                id,
                message: e.to_string(),
            };
            match cmd_tx.try_send(WriterCmd::send(err)) {
                Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Closed(cmd)) => {
                    return Err(tokio::sync::mpsc::error::SendError(cmd));
                }
            }
        }

        Ok(())
    }
    async fn on_resize(
        &mut self,
        message: BridgeClientMessage,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<WriterCmd>> {
        let BridgeClientMessage::Resize { id, cols, rows } = message else {
            unreachable!()
        };
        let Self { sessions, .. } = self;
        if let Some(sess) = sessions.get(&id) {
            sess.try_resize(cols, rows);
        }

        Ok(())
    }
    async fn on_kill(
        &mut self,
        message: BridgeClientMessage,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<WriterCmd>> {
        let BridgeClientMessage::Kill { id, signal } = message else {
            unreachable!()
        };
        let Self {
            sessions, cmd_tx, ..
        } = self;
        match crate::exec::proc::parse_signal(signal.as_deref()) {
            Ok(sig) => {
                if let Some(sess) = sessions.get(&id) {
                    sess.try_kill(if sess.pty && signal.is_none() {
                        crate::exec::proc::Signal::Hup
                    } else {
                        sig
                    });
                }
            }
            Err(message) => {
                let err = BridgeServerMessage::Error { id, message };
                match cmd_tx.try_send(WriterCmd::send(err)) {
                    Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(cmd)) => {
                        return Err(tokio::sync::mpsc::error::SendError(cmd));
                    }
                }
            }
        }

        Ok(())
    }
    async fn on_attach(
        &mut self,
        message: BridgeClientMessage,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<WriterCmd>> {
        let BridgeClientMessage::Attach { id, last_seq } = message else {
            unreachable!()
        };
        let Self {
            sessions,
            attached_id,
            cmd_tx,
            ..
        } = self;
        if let Some(session) = sessions.get(&id) {
            *attached_id = Some(id.clone());
            cmd_tx
                .send(WriterCmd::Attach {
                    session,
                    after_seq: last_seq,
                })
                .await?;
        } else {
            let err = BridgeServerMessage::Error {
                id: id.clone(),
                message: format!("session not found: {id}"),
            };
            cmd_tx.send(WriterCmd::send(err)).await?;
        }

        Ok(())
    }
    async fn on_list(
        &mut self,
        message: BridgeClientMessage,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<WriterCmd>> {
        let BridgeClientMessage::List = message else {
            unreachable!()
        };
        let Self {
            sessions, cmd_tx, ..
        } = self;
        let active = sessions.list();
        let msg = BridgeServerMessage::Sessions { sessions: active };
        cmd_tx.send(WriterCmd::send(msg)).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::proc::Signal;
    use crate::terminals::session::Session;
    use tokio::sync::mpsc;

    fn connection(cmd_tx: mpsc::Sender<WriterCmd>) -> Connection {
        let config = ServerConfig::new(std::env::temp_dir(), Arc::from("test-secret"), None);
        let http = crate::runs::http_client::ReqwestHttpClient::default();
        let backend = Arc::new(crate::runs::backend_client::BackendClient::new(
            config.backend_url.clone(),
            config.secret.clone(),
            http.clone(),
        ));
        Connection {
            sessions: SessionManager::new(),
            config,
            http,
            backend,
            cmd_tx,
            forwarders: Arc::new(Mutex::new(std::collections::HashMap::new())),
            completions: std::collections::HashMap::new(),
            starts: tokio::task::JoinSet::new(),
            attached_id: None,
            principal: Some(crate::auth::Principal::User { user_id: 1 }),
        }
    }

    fn session(id: &str) -> (Arc<Session>, mpsc::Receiver<Signal>) {
        let (stdin_tx, _) = mpsc::channel(1);
        let (resize_tx, _) = mpsc::channel(1);
        let (kill_tx, kill_rx) = mpsc::channel(1);
        (
            Arc::new(Session::new(
                id.into(),
                "test".into(),
                false,
                None,
                stdin_tx,
                resize_tx,
                kill_tx,
            )),
            kill_rx,
        )
    }

    #[tokio::test]
    async fn full_error_queue_does_not_block_kill() {
        let (cmd_tx, _cmd_rx) = mpsc::channel(1);
        assert!(
            cmd_tx
                .try_send(WriterCmd::send(BridgeServerMessage::Error {
                    id: String::new(),
                    message: "queued".into(),
                }))
                .is_ok()
        );
        let mut connection = connection(cmd_tx);
        let (exited, _) = session("exited");
        exited.emit_exit(Some(0), None);
        let (live, mut kill_rx) = session("live");
        assert!(connection.sessions.try_insert(exited).is_ok());
        assert!(connection.sessions.try_insert(live).is_ok());
        tokio::time::timeout(Duration::from_millis(500), async {
            for _ in 0..300 {
                assert!(
                    connection
                        .on_input(BridgeClientMessage::Input {
                            id: "exited".into(),
                            data: "input".into(),
                        })
                        .await
                        .is_ok()
                );
                assert!(
                    connection
                        .on_kill(BridgeClientMessage::Kill {
                            id: "live".into(),
                            signal: Some("invalid".into()),
                        })
                        .await
                        .is_ok()
                );
            }
            assert!(
                connection
                    .on_kill(BridgeClientMessage::Kill {
                        id: "live".into(),
                        signal: Some("KILL".into()),
                    })
                    .await
                    .is_ok()
            );
            assert!(matches!(kill_rx.recv().await, Some(Signal::Kill)));
        })
        .await
        .expect("error responses blocked control dispatch");
    }

    #[tokio::test]
    async fn closed_error_queue_ends_reader_handlers() {
        let (cmd_tx, cmd_rx) = mpsc::channel(1);
        drop(cmd_rx);
        let mut connection = connection(cmd_tx);
        let (exited, _) = session("exited");
        exited.emit_exit(Some(0), None);
        assert!(connection.sessions.try_insert(exited).is_ok());
        assert!(
            connection
                .on_input(BridgeClientMessage::Input {
                    id: "exited".into(),
                    data: "input".into(),
                })
                .await
                .is_err()
        );
        assert!(
            connection
                .on_kill(BridgeClientMessage::Kill {
                    id: "exited".into(),
                    signal: Some("invalid".into()),
                })
                .await
                .is_err()
        );
    }
}
