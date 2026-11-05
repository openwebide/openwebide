//! Persistent project language services. Both authenticated host routes use this
//! implementation; connection loss does not close a project's native service.
use super::{process::Process, rpc::Client};
use openwebide_core::plugins::languages::{
    LanguageServer,
    intelligence::{self as protocol, Query, Request, Response},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{OnceCell, watch};

mod documents;
mod messages;

pub const IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const MAX_SERVICES: usize = 4;
const MAX_OWNER_SERVICES: usize = 2;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// LSP explicitly permits servers to cancel read requests when their index
/// changes. Retry only those two codes while this immutable document snapshot
/// still owns the session lock; caller cancellation drops the entire operation.
async fn request_current(client: &Client, method: &str, params: Value) -> Result<Value, String> {
    tokio::time::timeout(REQUEST_TIMEOUT, async {
        for attempt in 0..3 {
            match client
                .request(method, params.clone(), REQUEST_TIMEOUT)
                .await
            {
                Ok(value) => return Ok(value),
                Err(super::rpc::Error::Remote {
                    code: -32801 | -32802,
                    ..
                }) if attempt < 2 => {
                    tokio::time::sleep(Duration::from_millis(50 * (attempt + 1))).await;
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        unreachable!("final attempt returns its result")
    })
    .await
    .map_err(|_| "Language-server request timed out".to_string())?
}

/// Trusted adapter data, never deserialized from a plugin or model request.
#[derive(Clone)]
pub struct Scope {
    pub owner: String,
    pub project: i64,
    pub root: PathBuf,
    pub provider: String,
    pub digest: String,
    pub server: LanguageServer,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    owner: String,
    project: i64,
    root: PathBuf,
    workspace: PathBuf,
    provider: String,
    digest: String,
    server: String,
    client: String,
}
#[derive(Clone, Default)]
pub struct Host {
    inner: Arc<Inner>,
}
#[derive(Default)]
struct Inner {
    entries: Mutex<BTreeMap<Key, Arc<Entry>>>,
    reaping: std::sync::atomic::AtomicBool,
}
struct Entry {
    session: OnceCell<Session>,
    used: Mutex<Instant>,
    stopped: watch::Sender<bool>,
    active: Mutex<BTreeMap<String, (u64, watch::Sender<bool>)>>,
    failure: Mutex<Failure>,
}
#[derive(Default)]
struct Failure {
    count: u32,
    until: Option<Instant>,
    reason: String,
}
struct Session {
    id: String,
    root: PathBuf,
    process: Mutex<Process>,
    client: Client,
    capabilities: Value,
    documents: tokio::sync::Mutex<documents::Documents>,
    diagnostics: Arc<Mutex<BTreeMap<String, documents::Diagnostics>>>,
    pump: tokio::task::AbortHandle,
}
impl Entry {
    fn new() -> Self {
        Self {
            session: OnceCell::new(),
            used: Mutex::new(Instant::now()),
            stopped: watch::channel(false).0,
            active: Mutex::default(),
            failure: Mutex::default(),
        }
    }
    fn stop(&self) {
        self.stopped.send_replace(true);
        if let Some(session) = self.session.get() {
            session.process.lock().unwrap().stop();
        }
    }
}
impl Drop for Entry {
    fn drop(&mut self) {
        self.stop();
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.pump.abort();
        self.process.get_mut().unwrap().stop();
    }
}

impl Host {
    pub async fn query(&self, mut scope: Scope, input: &Request) -> Result<Response, String> {
        input.validate()?;
        if scope.project <= 0 || scope.owner.is_empty() {
            return Err("Language services require project ownership".into());
        }
        let language = scope
            .server
            .language_id
            .take()
            .unwrap_or_else(|| input.language.clone());
        scope.root = tokio::fs::canonicalize(&scope.root)
            .await
            .map_err(|error| format!("Project folder is unavailable: {error}"))?;
        let path = if matches!(input.query, Query::Close) {
            // The validated relative path identifies retained state; closing it
            // must also work after the file or its parent has been removed.
            scope.root.join(&input.path)
        } else {
            documents::source_path(&scope.root, &input.path).await?
        };
        let workspace =
            documents::workspace_root(&scope.root, &path, &scope.server.root_markers).await;
        let server_config =
            serde_json::to_string(&scope.server).map_err(|error| error.to_string())?;
        let key = Key {
            owner: scope.owner.clone(),
            project: scope.project,
            root: scope.root.clone(),
            workspace: workspace.clone(),
            provider: scope.provider.clone(),
            digest: scope.digest.clone(),
            server: server_config,
            client: input.client.clone(),
        };
        let entry = self.entry(&key)?;
        self.start_reaper();
        {
            let failure = entry.failure.lock().unwrap();
            if failure.until.is_some_and(|until| until > Instant::now()) {
                return Err(format!(
                    "Language service is waiting to retry: {}",
                    failure.reason
                ));
            }
        }
        let (cancel, mut cancelled) = watch::channel(false);
        {
            let mut active = entry.active.lock().unwrap();
            if active
                .get(&input.document)
                .is_some_and(|(request, _)| *request >= input.request)
            {
                return Err("Language-server request was superseded".into());
            }
            if active.len() >= 32 && !active.contains_key(&input.document) {
                return Err("Too many concurrent language documents".into());
            }
            if let Some((_, previous)) =
                active.insert(input.document.clone(), (input.request, cancel))
            {
                previous.send_replace(true);
            }
        }
        let _active = ActiveRequest {
            entry: Arc::clone(&entry),
            document: input.document.clone(),
            request: input.request,
        };
        let mut stopped = entry.stopped.subscribe();
        if *stopped.borrow() {
            return Err("Language service was closed".into());
        }
        let result = tokio::select! {
            _ = stopped.changed() => Err("Language service was closed".into()),
            _ = cancelled.changed() => Err("Language-server request was superseded".into()),
            result = async {
                let session = entry.session.get_or_try_init(|| async {
                    match Session::start(&scope,&workspace).await {
                        Ok(session)=>{*entry.failure.lock().unwrap()=Failure::default();Ok(session)},
                        Err(error)=>{
                            let mut failure=entry.failure.lock().unwrap();
                            failure.count=failure.count.saturating_add(1);
                            let delay=(1_u64<<failure.count.saturating_sub(1).min(5)).min(30);
                            failure.until=Some(Instant::now()+Duration::from_secs(delay));failure.reason=error.clone();
                            Err(error)
                        }
                    }
                }).await?;
                let mut documents = session.documents.lock().await;
                documents.query(session,input,&path,&language).await
            } => result,
        };
        if *stopped.borrow() || *cancelled.borrow() {
            return Err("Language service request was closed or superseded".into());
        }
        if entry
            .session
            .get()
            .is_some_and(|session| session.client.is_closed())
        {
            let mut entries = self.inner.entries.lock().unwrap();
            if entries
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current, &entry))
            {
                entries.remove(&key);
            }
        }
        result
    }
    fn entry(&self, key: &Key) -> Result<Arc<Entry>, String> {
        let mut entries = self.inner.entries.lock().unwrap();
        if let Some(entry) = entries.get(key) {
            *entry.used.lock().unwrap() = Instant::now();
            return Ok(Arc::clone(entry));
        }
        let owner_full = entries
            .keys()
            .filter(|entry| entry.owner == key.owner)
            .count()
            >= MAX_OWNER_SERVICES;
        if entries.len() >= MAX_SERVICES || owner_full {
            let oldest = entries
                .iter()
                .filter(|(candidate, entry)| {
                    (!owner_full || candidate.owner == key.owner)
                        && entry.active.lock().unwrap().is_empty()
                })
                .min_by_key(|(_, entry)| *entry.used.lock().unwrap())
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                if let Some(entry) = entries.remove(&oldest) {
                    entry.stop();
                }
            } else {
                return Err(
                    "Language-server capacity is busy; retry after current requests finish".into(),
                );
            }
        }
        let entry = Arc::new(Entry::new());
        entries.insert(key.clone(), Arc::clone(&entry));
        Ok(entry)
    }
    /// Revocation is immediate and wakes in-flight requests; it does not wait
    /// for the idle cache to expire or change a provider beneath a request.
    pub fn revoke(&self, owner: &str, project: Option<i64>, provider: Option<&str>) {
        self.inner.entries.lock().unwrap().retain(|key, entry| {
            let matches = key.owner == owner
                && project.is_none_or(|id| key.project == id)
                && provider.is_none_or(|provider| key.provider == provider);
            if matches {
                entry.stop();
            }
            !matches
        });
    }
    pub fn shutdown(&self) {
        for entry in std::mem::take(&mut *self.inner.entries.lock().unwrap()).into_values() {
            entry.stop();
        }
    }
    fn start_reaper(&self) {
        if self
            .inner
            .reaping
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                inner.entries.lock().unwrap().retain(|_, entry| {
                    let recent = entry.used.lock().unwrap().elapsed() < IDLE_TIMEOUT;
                    let busy = !entry.active.lock().unwrap().is_empty();
                    let retain = recent || busy;
                    if !retain {
                        entry.stop();
                    }
                    retain
                });
            }
        });
    }
}

#[cfg(test)]
mod tests;
struct ActiveRequest {
    entry: Arc<Entry>,
    document: String,
    request: u64,
}
impl Drop for ActiveRequest {
    fn drop(&mut self) {
        let mut active = self.entry.active.lock().unwrap();
        if active
            .get(&self.document)
            .is_some_and(|(request, _)| *request == self.request)
        {
            active.remove(&self.document);
        }
        drop(active);
        *self.entry.used.lock().unwrap() = Instant::now();
    }
}

impl Session {
    async fn start(scope: &Scope, workspace: &std::path::Path) -> Result<Self, String> {
        let uri =
            url::Url::from_directory_path(workspace).map_err(|()| "Invalid project folder URI")?;
        let (mut process, events) = Process::spawn(&scope.server, workspace)?;
        let client = process.client();
        let diagnostics = Arc::new(Mutex::new(BTreeMap::new()));
        let pump = messages::pump(
            client.clone(),
            events,
            Arc::clone(&diagnostics),
            &uri,
            scope.server.settings.clone(),
        );
        let initialized = match client
            .request(
                "initialize",
                openwebide_agent::plugins::languages::initialization_params(
                    uri.as_str(),
                    scope.server.initialization_options.clone(),
                    std::process::id(),
                ),
                Duration::from_secs(60),
            )
            .await
        {
            Ok(initialized) => initialized,
            Err(error) => {
                let detail = process.failure_detail().await;
                return Err(format!(
                    "Cannot initialize language server '{}': {error}. {detail} Install the server and its toolchain on this execution host.",
                    scope.server.command
                ));
            }
        };
        let capabilities = initialized
            .get("capabilities")
            .filter(|value| value.is_object())
            .cloned()
            .ok_or("Language server omitted its capabilities")?;
        if capabilities
            .get("positionEncoding")
            .and_then(Value::as_str)
            .is_some_and(|encoding| encoding != "utf-16")
        {
            return Err("Language server did not negotiate UTF-16 positions".into());
        }
        let synchronization = capabilities.get("textDocumentSync").and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.get("change").and_then(Value::as_i64))
        });
        if !matches!(synchronization, Some(1 | 2)) {
            return Err("Language server does not support live document synchronization".into());
        }
        client
            .notify("initialized", json!({}))
            .await
            .map_err(|error| error.to_string())?;
        if !scope.server.settings.is_null() {
            client
                .notify(
                    "workspace/didChangeConfiguration",
                    json!({"settings":scope.server.settings}),
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(Self {
            root: scope.root.clone(),
            id: format!("{:032x}", rand::random::<u128>()),
            process: Mutex::new(process),
            client,
            capabilities,
            documents: tokio::sync::Mutex::new(documents::Documents::default()),
            diagnostics,
            pump,
        })
    }
}
