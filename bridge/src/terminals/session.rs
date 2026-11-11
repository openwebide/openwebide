//! Process session management, ring buffering, and client broadcasting.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use openwebide_core::{BridgeServerMessage, BridgeSessionInfo};
use tokio::sync::mpsc;

use crate::exec::proc::Signal;
use crate::terminals::seq_ring::SeqRing;

/// Maximum size of the output ring buffer in bytes (2MB default per session).
const MAX_RING_BUFFER_BYTES: usize = 2 * 1024 * 1024;

/// An active process or interactive PTY session.
pub struct Session {
    pub id: String,
    pub command: String,
    pub pty: bool,
    pub started_at: u64,
    /// The OS pid of the spawned child, which is also its process group id: every session is
    /// spawned as its own group leader (`process_group(0)`, or a PTY session leader via
    /// `setsid`). `None` on platforms without process groups (Windows).
    pub pid: Option<i32>,
    pub running: Arc<AtomicBool>,
    pub exited_at: Mutex<Option<Instant>>,
    detached_at: Mutex<Option<Instant>>,
    pub ring: Mutex<SeqRing<BridgeServerMessage>>,
    stdin_tx: Mutex<Option<mpsc::Sender<String>>>,
    resize_tx: Mutex<Option<mpsc::Sender<(u16, u16)>>>,
    kill_tx: Mutex<Option<mpsc::Sender<Signal>>>,
}

impl Session {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        command: String,
        pty: bool,
        pid: Option<i32>,
        stdin_tx: mpsc::Sender<String>,
        resize_tx: mpsc::Sender<(u16, u16)>,
        kill_tx: mpsc::Sender<Signal>,
    ) -> Self {
        let started_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();

        Self {
            id,
            command,
            pty,
            started_at,
            pid,
            running: Arc::new(AtomicBool::new(true)),
            exited_at: Mutex::new(None),
            detached_at: Mutex::new(Some(Instant::now())),
            ring: Mutex::new(SeqRing::new(Some(MAX_RING_BUFFER_BYTES), None)),
            stdin_tx: Mutex::new(Some(stdin_tx)),
            resize_tx: Mutex::new(Some(resize_tx)),
            kill_tx: Mutex::new(Some(kill_tx)),
        }
    }

    /// Send input data to the process's stdin or PTY. A no-op once the session has exited.
    pub fn try_send_input(&self, data: String) -> Result<(), &'static str> {
        let tx = self.stdin_tx.lock().unwrap().clone();
        if let Some(tx) = tx {
            match tx.try_send(data) {
                Ok(()) => Ok(()),
                Err(mpsc::error::TrySendError::Full(_)) => Err("input buffer full"),
                Err(mpsc::error::TrySendError::Closed(_)) => Err("session has exited"),
            }
        } else {
            Err("session has exited")
        }
    }

    /// Resize the PTY. A no-op for headless sessions or once the session has exited.
    pub fn try_resize(&self, cols: u16, rows: u16) {
        let tx = self.resize_tx.lock().unwrap().clone();
        if let Some(tx) = tx {
            let _ = tx.try_send((cols, rows));
        }
    }

    /// Request the process be sent `signal`. A no-op once the session has exited.
    pub fn try_kill(&self, signal: Signal) {
        let tx = self.kill_tx.lock().unwrap().clone();
        if let Some(tx) = tx {
            let _ = tx.try_send(signal);
        }
    }

    /// Append output to buffer and notify all active subscribers.
    pub fn push_output(&self, stream: &str, data: &str) {
        let mut ring = self.ring.lock().unwrap();
        let mut start = 0;
        while start < data.len() {
            let mut end = start + 65536;
            if end >= data.len() {
                end = data.len();
            } else {
                while !data.is_char_boundary(end) {
                    end -= 1;
                }
            }
            if start == end {
                break;
            }
            let chunk = &data[start..end];
            ring.push(
                |seq| BridgeServerMessage::Output {
                    id: self.id.clone(),
                    seq,
                    stream: stream.to_string(),
                    data: chunk.to_string(),
                },
                chunk.len(),
            );
            start = end;
        }
    }

    /// Mark the session as terminated with the final exit code, and drop the input/resize/kill
    /// senders so their receiving tasks end and the underlying fds (PTY master, child stdin)
    /// close.
    pub fn emit_exit(&self, code: Option<i32>, signal: Option<String>) {
        self.running.store(false, Ordering::SeqCst);
        *self.exited_at.lock().unwrap() = Some(Instant::now());
        self.stdin_tx.lock().unwrap().take();
        self.resize_tx.lock().unwrap().take();
        self.kill_tx.lock().unwrap().take();
        let msg = BridgeServerMessage::Exited {
            id: self.id.clone(),
            exit_code: code,
            signal,
        };
        self.push_event(msg);
    }

    /// Emit an error message.
    pub fn emit_error(&self, message: &str) {
        let msg = BridgeServerMessage::Error {
            id: self.id.clone(),
            message: message.to_string(),
        };
        self.push_event(msg);
    }

    pub fn push_event(&self, msg: BridgeServerMessage) {
        let mut ring = self.ring.lock().unwrap();
        ring.push(|_| msg, 0);
    }

    pub fn to_info(&self) -> BridgeSessionInfo {
        BridgeSessionInfo {
            id: self.id.clone(),
            command: self.command.clone(),
            running: self.running.load(Ordering::SeqCst),
            pty: self.pty,
            started_at: self.started_at,
        }
    }
}

/// Global session manager overseeing all active sessions.
#[derive(Clone, Default)]
pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<String, Arc<Session>>>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn try_insert(&self, session: Arc<Session>) -> Result<(), &'static str> {
        use std::collections::hash_map::Entry;
        let mut sessions = self.sessions.write().unwrap();
        match sessions.entry(session.id.clone()) {
            Entry::Vacant(e) => {
                e.insert(session);
                Ok(())
            }
            Entry::Occupied(_) => Err("session id already exists"),
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions.read().unwrap().get(id).cloned()
    }

    pub fn list(&self) -> Vec<BridgeSessionInfo> {
        self.sessions
            .read()
            .unwrap()
            .values()
            .map(|s| s.to_info())
            .collect()
    }

    pub fn remove(&self, id: &str) {
        self.sessions.write().unwrap().remove(id);
    }

    /// Remove every exited session whose `exited_at` is at least `ttl` old. Running sessions are
    /// never reaped, however long they've been alive. Returns how many were removed.
    pub fn reap(&self, ttl: Duration) -> usize {
        let mut sessions = self.sessions.write().unwrap();
        let expired: Vec<String> = sessions
            .iter()
            .filter(|(_, s)| {
                !s.running.load(Ordering::SeqCst)
                    && s.exited_at
                        .lock()
                        .unwrap()
                        .is_some_and(|t| t.elapsed() >= ttl)
            })
            .map(|(id, _)| id.clone())
            .collect();
        let count = expired.len();
        for id in expired {
            sessions.remove(&id);
        }
        count
    }

    /// Detached interactive shells expire; headless jobs retain their separate lifecycle.
    pub fn reap_detached(&self, ttl: Duration) {
        for session in self.sessions.read().unwrap().values() {
            if !session.pty || !session.running.load(Ordering::SeqCst) {
                continue;
            }
            let mut detached = session.detached_at.lock().unwrap();
            if session.ring.lock().unwrap().has_subscribers() {
                *detached = None;
            } else if detached.get_or_insert_with(Instant::now).elapsed() >= ttl {
                session.try_kill(Signal::Hup);
                *detached = Some(Instant::now());
            }
        }
    }

    /// Terminate every running session's process group concurrently, for graceful daemon
    /// shutdown.
    pub async fn kill_all(&self) {
        #[cfg(unix)]
        {
            let sessions: Vec<Arc<Session>> =
                self.sessions.read().unwrap().values().cloned().collect();
            let kills = sessions
                .iter()
                .filter(|s| s.running.load(Ordering::SeqCst))
                .filter_map(|s| s.pid.map(|pid| (pid, s.pty)))
                .map(|(pgid, pty)| {
                    crate::exec::proc::terminate_group_with(
                        pgid,
                        Duration::from_secs(2),
                        if pty { Signal::Hup } else { Signal::Term },
                    )
                });
            futures::future::join_all(kills).await;
        }
        #[cfg(windows)]
        for session in self.sessions.read().unwrap().values() {
            session.try_kill(Signal::Kill);
        }
        crate::exec::proc::wait_for_cleanup().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_session(id: &str) -> Arc<Session> {
        let (stdin_tx, _) = mpsc::channel(1);
        let (resize_tx, _) = mpsc::channel(1);
        let (kill_tx, _) = mpsc::channel(1);
        Arc::new(Session::new(
            id.into(),
            "echo test".into(),
            false,
            Some(1234),
            stdin_tx,
            resize_tx,
            kill_tx,
        ))
    }

    #[test]
    fn detached_pty_expires_but_attached_and_headless_sessions_do_not() {
        let (input, _) = mpsc::channel(1);
        let (resize, _) = mpsc::channel(1);
        let (kill, mut signals) = mpsc::channel(1);
        let session = Arc::new(Session::new(
            "detached".into(),
            "sh".into(),
            true,
            None,
            input,
            resize,
            kill,
        ));
        let manager = SessionManager::new();
        manager.try_insert(session.clone()).unwrap();
        let attached = session.ring.lock().unwrap().subscribe();
        manager.reap_detached(Duration::ZERO);
        assert!(signals.try_recv().is_err());
        drop(attached);
        manager.reap_detached(Duration::ZERO);
        assert_eq!(signals.try_recv().unwrap(), Signal::Hup);
        let headless = test_session("job");
        manager.try_insert(headless.clone()).unwrap();
        manager.reap_detached(Duration::ZERO);
        assert!(headless.detached_at.lock().unwrap().is_some());
    }

    #[test]
    fn test_session_manager() {
        let mgr = SessionManager::new();
        let sess = test_session("sess-1");

        assert!(mgr.try_insert(sess.clone()).is_ok());
        assert!(mgr.get("sess-1").is_some());
        assert_eq!(mgr.list().len(), 1);
        assert_eq!(mgr.list()[0].id, "sess-1");

        assert!(mgr.try_insert(sess).is_err()); // duplicate

        mgr.remove("sess-1");
        assert!(mgr.get("sess-1").is_none());
        assert_eq!(mgr.list().len(), 0);
    }

    #[test]
    fn emit_exit_drops_senders() {
        let sess = test_session("sess-1");
        assert!(sess.stdin_tx.lock().unwrap().is_some());
        assert!(sess.resize_tx.lock().unwrap().is_some());
        assert!(sess.kill_tx.lock().unwrap().is_some());

        sess.emit_exit(Some(0), None);

        assert!(!sess.running.load(Ordering::SeqCst));
        assert!(sess.exited_at.lock().unwrap().is_some());
        assert!(sess.stdin_tx.lock().unwrap().is_none());
        assert!(sess.resize_tx.lock().unwrap().is_none());
        assert!(sess.kill_tx.lock().unwrap().is_none());
    }

    #[test]
    fn reap_removes_only_expired_exited() {
        let mgr = SessionManager::new();

        let still_running = test_session("running");
        assert!(mgr.try_insert(still_running).is_ok());

        let freshly_exited = test_session("fresh");
        freshly_exited.emit_exit(Some(0), None);
        assert!(mgr.try_insert(freshly_exited).is_ok());

        let long_exited = test_session("stale");
        long_exited.emit_exit(Some(0), None);
        *long_exited.exited_at.lock().unwrap() = Some(Instant::now() - Duration::from_secs(3600));
        assert!(mgr.try_insert(long_exited).is_ok());

        let removed = mgr.reap(Duration::from_secs(60));

        assert_eq!(removed, 1);
        assert!(mgr.get("running").is_some());
        assert!(mgr.get("fresh").is_some());
        assert!(mgr.get("stale").is_none());
    }
}
