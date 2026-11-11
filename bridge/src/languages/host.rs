//! Persistent project language services. Both authenticated host routes use this
//! implementation; connection loss does not close a project's native service.
use super::{process::Process, rpc::Client};
use openwebide_core::plugins::languages::{
    LanguageServer, ProjectFiles, VirtualDocument, VirtualDocumentParameters,
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod admission;
mod analysis;
mod cache;
mod commands;
mod documents;
mod files;
mod generation;
mod inputs;
mod messages;
mod prefetch;
mod program;
mod repository;
mod resources;
mod reuse;
pub use resources::{MemoryLimits, ResourceUsage};

pub const IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const MAX_SERVICES: usize = 4;
const MAX_OWNER_SERVICES: usize = 2;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// LSP explicitly permits servers to cancel read requests when their index
/// changes. Retries retain the immutable source scope and deadline; cancellation
/// drops the operation. Mutating commands never enter this retry policy.
async fn request_current(client: &Client, method: &str, params: Value) -> Result<Value, String> {
    tokio::time::timeout(REQUEST_TIMEOUT, async {
        for attempt in 0..3 {
            match client
                .request(method, params.clone(), REQUEST_TIMEOUT)
                .await
            {
                Ok(value) => return Ok(value),
                Err(super::rpc::Error::Remote { code, .. })
                    if attempt < 2
                        && !matches!(
                            method,
                            "workspace/executeCommand" | "workspace/applyEdit"
                        )
                        && matches!(code, -32801 | -32802) =>
                {
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
    cache: cache::Manager,
    prefetch: prefetch::Queue,
    coordinator: crate::folder_coordination::Coordinator,
    limits: MemoryLimits,
    resources: Mutex<ResourceUsage>,
    entries: Mutex<BTreeMap<Key, Arc<Entry>>>,
    outcomes: Mutex<commands::Outcomes>,
    reaping: std::sync::atomic::AtomicBool,
}
struct Entry {
    active_uses: std::sync::atomic::AtomicUsize,
    starting: Mutex<Option<ProcessIdentity>>,
    session: OnceCell<Session>,
    handoff: Mutex<Option<reuse::Warm>>,
    retained: Mutex<Option<documents::Documents>>,
    used: Mutex<Instant>,
    stopped: watch::Sender<bool>,
    activity: Mutex<Activity>,
    failure: Mutex<Failure>,
}
/// Sampling retains only original-process identity, never a service or its
/// documents. The completion channel follows handoff and distinguishes PID reuse.
#[derive(Clone)]
struct ProcessIdentity {
    pid: u32,
    completion: crate::exec::proc::Cleanup,
}
impl ProcessIdentity {
    fn capture(process: &Process) -> Option<Self> {
        Some(Self {
            pid: process.resource_id()?,
            completion: process.termination(),
        })
    }
    fn matches(&self, other: &Self) -> bool {
        self.pid == other.pid && self.completion.same_process(&other.completion)
    }
}
#[derive(Default)]
struct Failure {
    count: u32,
    until: Option<Instant>,
    reason: String,
}
#[derive(Default)]
struct Activity {
    active: BTreeMap<String, InFlight>,
    // Also retain requests cancelled during startup, before Documents exists.
    watermarks: BTreeMap<String, (u64, u64, Instant)>,
}
struct InFlight {
    request: u64,
    cancel: watch::Sender<bool>,
    prefetch: bool,
}
struct Session {
    analysis: analysis::Analysis,
    // Cancellation is advisory to a server. Do not queue more speculative work
    // on an original process after an interrupted prefetch. Warm handoff
    // retains this state; only starting a replacement process resets it.
    prefetch_enabled: std::sync::atomic::AtomicBool,
    sources: OnceCell<generation::Tracker>,
    cache: Option<cache::Use>,
    id: String,
    root: PathBuf,
    root_handle: same_file::Handle,
    workspace: PathBuf,
    workspace_handle: same_file::Handle,
    repository: repository::Repository,
    program: program::Program,
    inputs: inputs::Inputs,
    commands: Mutex<commands::Commands>,
    process: Mutex<Process>,
    client: Client,
    capabilities: Value,
    virtual_documents: Vec<VirtualDocument>,
    documents: Arc<tokio::sync::Mutex<documents::Documents>>,
    diagnostics: Arc<Mutex<BTreeMap<String, documents::Diagnostics>>>,
    pump: tokio::task::AbortHandle,
}
impl Entry {
    fn new() -> Self {
        Self {
            active_uses: std::sync::atomic::AtomicUsize::new(0),
            starting: Mutex::new(None),
            session: OnceCell::new(),
            handoff: Mutex::default(),
            retained: Mutex::default(),
            used: Mutex::new(Instant::now()),
            stopped: watch::channel(false).0,
            activity: Mutex::default(),
            failure: Mutex::default(),
        }
    }
    fn process_identity(&self) -> Option<ProcessIdentity> {
        self.session.get().map_or_else(
            || self.starting.lock().unwrap().clone(),
            |session| ProcessIdentity::capture(&session.process.lock().unwrap()),
        )
    }
    fn busy(&self) -> bool {
        let stopped = *self.stopped.borrow();
        !self.activity.lock().unwrap().active.is_empty()
            || self.active_uses.load(std::sync::atomic::Ordering::Relaxed) != 0
            || (!stopped
                && self.session.get().is_some_and(|session| {
                    !session.client.is_closed() && !session.commands.lock().unwrap().reusable()
                }))
    }
    fn stop(&self) {
        self.stopped.send_replace(true);
        if let Some(session) = self.session.get() {
            session.process.lock().unwrap().stop();
            if let Some(cache) = &session.cache {
                cache.retire();
            }
        }
        self.handoff.lock().unwrap().take();
    }
    async fn stop_and_wait(&self) -> Result<(), String> {
        self.stop();
        let completion = self
            .session
            .get()
            .map(|session| session.process.lock().unwrap().termination());
        if let Some(completion) = completion {
            completion.wait().await?;
        }
        Ok(())
    }
    async fn check_project(
        &self,
        session: &Session,
        input: &Request,
        documents: &documents::Documents,
        path: &std::path::Path,
    ) -> Result<(), String> {
        self.check_context(session).await?;
        let current = session.inputs.observe(&session.root).await?;
        if matches!(input.query, Query::PrepareAction { .. }) {
            if !session.inputs.is_baseline(&current) {
                return Err("The reviewed action's project inputs or original source changed before application".into());
            }
            return commands::prepare_synchronization(
                session,
                input,
                &current.snapshot,
                documents,
                path,
            );
        }
        if matches!(input.query, Query::SynchronizeAction { .. }) {
            return commands::verify_synchronization(session, input, &current);
        }
        if !session.inputs.is_baseline(&current) {
            if commands::has_pending_action(session) {
                return Err("A matching reviewed command is being applied; finish or discard that action before requesting analysis".into());
            }
            let error = "Declared project inputs changed; retry to synchronize current buffers with a fresh language service".to_string();
            self.failure.lock().unwrap().reason.clone_from(&error);
            self.stop();
            return Err(error);
        }
        Ok(())
    }
    async fn check_context(&self, session: &Session) -> Result<(), String> {
        session.check_context().await.inspect_err(|error| {
            self.failure.lock().unwrap().reason.clone_from(error);
            self.stop();
        })
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
    /// Package metadata selects source files for optional speculative work.
    /// Ordinary requests keep their existing fresh-server path; complete-source
    /// cache lookup needs a separate validated context before it can reuse data.
    pub async fn query_package(
        &self,
        scope: Scope,
        input: &Request,
        prepared: &openwebide_core::plugins::PreparedPlugin,
    ) -> Result<Response, String> {
        if input.query.is_prefetch()
            || input.query.produces_analysis()
            || matches!(input.query, Query::ValidateAnalysis { .. })
        {
            let sources = ProjectFiles::for_server(prepared, &input.language)?;
            self.query_with_sources(scope, input, sources).await
        } else {
            self.query(scope, input).await
        }
    }
    pub async fn query(&self, scope: Scope, input: &Request) -> Result<Response, String> {
        self.query_scoped(scope, input, None).await
    }
    /// Installed-package callers supply the same published file associations
    /// on both authenticated transports. Raw native peers without declarations
    /// cannot make reusable project-source observations.
    pub async fn query_with_sources(
        &self,
        scope: Scope,
        input: &Request,
        sources: ProjectFiles,
    ) -> Result<Response, String> {
        self.query_scoped(scope, input, Some(sources)).await
    }
    async fn query_scoped(
        &self,
        scope: Scope,
        input: &Request,
        sources: Option<ProjectFiles>,
    ) -> Result<Response, String> {
        if input.query.is_prefetch() {
            let permit = self.inner.prefetch.acquire()?;
            let mut cancelled = permit.cancelled.clone();
            tokio::select! {
                biased;
                _ = cancelled.changed() => Err("Language prefetch was superseded by interactive work".into()),
                result = tokio::time::timeout(prefetch::TIMEOUT, self.query_selected(scope, input,sources.as_ref())) =>
                    result.map_err(|_| "Language prefetch exceeded its time budget".to_string())?,
            }
        } else {
            self.query_selected(scope, input, sources.as_ref()).await
        }
    }
    async fn query_selected(
        &self,
        mut scope: Scope,
        input: &Request,
        sources: Option<&ProjectFiles>,
    ) -> Result<Response, String> {
        input.validate()?;
        if !scope.server.role.is_language()
            && !matches!(
                input.query,
                Query::Synchronize
                    | Query::Prefetch {
                        kind: protocol::PrefetchKind::Diagnostics
                    }
                    | Query::SynchronizeAction { .. }
                    | Query::PrepareAction { .. }
                    | Query::ReleaseAction { .. }
                    | Query::Saved
                    | Query::Close
                    | Query::CodeActions { .. }
                    | Query::ResolveAction { .. }
                    | Query::ReviewAction { .. }
                    | Query::ValidateAnalysis { .. }
            )
        {
            return Err("This installed provider supplies diagnostics and fixes, not primary language intelligence".into());
        }
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
        let server_config =
            serde_json::to_string(&scope.server).map_err(|error| error.to_string())?;
        let mut key = Key {
            owner: scope.owner.clone(),
            project: scope.project,
            root: scope.root.clone(),
            workspace: scope.root.clone(),
            provider: scope.provider.clone(),
            digest: scope.digest.clone(),
            server: server_config,
            client: input.client.clone(),
        };
        if matches!(input.query, Query::Close) {
            // Markers and files may have disappeared since didOpen. Close the
            // retained scope without discovering or starting another service.
            return self.close(&key, input, &language).await;
        }
        self.check_pending_action(&key, input)?;
        let path = if matches!(
            input.query,
            Query::SynchronizeAction { close: true, .. } | Query::ReleaseAction { .. }
        ) {
            scope.root.join(&input.path)
        } else {
            documents::source_path(&scope.root, &input.path).await?
        };
        let (mut active, mut cancelled) = if let Query::SynchronizeAction { action, .. }
        | Query::PrepareAction { action }
        | Query::ReleaseAction { action } = &input.query
        {
            let (retained, active, cancelled) = self.acquire_action(&key, input, &action.server)?;
            key = retained;
            (active, cancelled)
        } else if let Query::ValidateAnalysis { context } = &input.query {
            let (retained, active, cancelled) =
                self.acquire_action(&key, input, &context.server)?;
            key = retained;
            (active, cancelled)
        } else {
            key.workspace =
                documents::workspace_root(&scope.root, &path, &scope.server.root_markers).await;
            self.acquire(&key, input)?
        };
        let workspace = &key.workspace;
        let entry = &active.entry;
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
        let mut stopped = entry.stopped.subscribe();
        if *stopped.borrow() {
            return Err("Language service was closed".into());
        }
        let current = cancelled.clone();
        let result = tokio::select! {
            _ = stopped.changed() => Err("Language service was closed".into()),
            _ = cancelled.changed() => Err("Language-server request was superseded".into()),
            result = async {
                let session = if matches!(input.query, Query::SynchronizeAction { .. } | Query::PrepareAction { .. } | Query::ReleaseAction { .. } | Query::ValidateAnalysis { .. }) {
                    entry.session.get().ok_or("The original language service is unavailable; action synchronization never starts a replacement")?
                } else { entry.session.get_or_try_init(|| async {
                    match reuse::initialize(&scope,workspace,entry,&self.inner).await {
                        Ok(session)=>{*entry.failure.lock().unwrap()=Failure::default();Ok(session)},
                        Err(error)=>{
                            *entry.starting.lock().unwrap()=None;
                            let mut failure=entry.failure.lock().unwrap();
                            failure.count=failure.count.saturating_add(1);
                            let delay=(1_u64<<failure.count.saturating_sub(1).min(5)).min(30);
                            failure.until=Some(Instant::now()+Duration::from_secs(delay));failure.reason=error.clone();
                            Err(error)
                        }
                    }
                }).await? };
                if input.query.is_prefetch() && !session.prefetch_enabled.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(prefetch::SUSPENDED.into());
                }
                let mut documents = session.documents.lock().await;
                // Close can race the last poll of initialize. Never publish an
                // overlay after its close observed an uninitialized session.
                if *current.borrow() || *entry.stopped.borrow() {
                    return Err("Language service request was closed or superseded".into());
                }
                if matches!(input.query, Query::ReleaseAction { .. }) {
                    commands::release_synchronization(session,input)?;
                    return documents.query(session,input,&path,&language).await.map(|(response,_)|response);
                }
                let sources=if let Some(files)=sources {
                    let tracker=session.sources.get_or_init(||async {generation::Tracker::new(files.clone())}).await;
                    if !tracker.matches(files) {return Err("Project-source associations changed for this service".into());}
                    Some(tracker)
                } else {session.sources.get()};
                self.check_pending_action(&key,input)?;
                entry.check_project(session,input,&documents,&path).await?;
                let mut before=if input.query.produces_analysis() || matches!(input.query, Query::ValidateAnalysis { .. }) {
                    analysis::observe(&input.query,sources,&mut documents,&session.root).await?
                } else {None};
                if let Some(before) = before { documents.observe_generation(before); }
                let normalized = input.query.foreground_query().map(|query| Request { query, ..input.clone() });
                let result = if let Query::ValidateAnalysis { context } = &input.query {
                    documents.validate_analysis(session, input, &path, context, before.ok_or("The original analysis has no complete project-source observation")?).map(|(response,affected)|(response,Some(affected)))
                } else {
                    documents.query(session,normalized.as_ref().unwrap_or(input),&path,&language).await
                };
                if let Some(original)=before {
                    if let Some(after)=analysis::observe(&input.query,sources,&mut documents,&session.root).await? {
                    if after!=original {
                        documents.observe_generation(after);
                        return Err("Project files changed during language analysis; retry with the current project".into());
                    }
                    } else {before=None;}
                }
                entry.check_project(session,input,&documents,&path).await?;
                self.check_pending_action(&key,input)?;
                let (mut response,affected)=result?;
                if let Some(affected)=&affected {affected.ensure_current(&session.root)?;}
                if input.query.produces_analysis() && let Some(before)=before {
                    response.context=Some(session.analysis.issue_observed(&session.id,input,before,documents.overlay_revision(),&response.result,affected)?);
                }
                response.validate(input)?;
                Ok(response)
            } => result,
        };
        if *stopped.borrow() || *cancelled.borrow() {
            let reason = entry.failure.lock().unwrap().reason.clone();
            return Err(if reason.is_empty() {
                "Language service request was closed or superseded".into()
            } else {
                reason
            });
        }
        if entry
            .session
            .get()
            .is_some_and(|session| session.client.is_closed())
        {
            let mut entries = self.inner.entries.lock().unwrap();
            if entries
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current, entry))
            {
                entries.remove(&key);
            }
        }
        // A returned error (for example, a changed source inventory) has still
        // completed its work. Only interruption/timeout leaves unknown native
        // computation and suspends further speculation on this process.
        active.completed = true;
        result
    }
    /// An approved compound edit may briefly have neither its old nor final
    /// input inventory. Fence ordinary analysis across retained workspaces,
    /// including when a moved marker would otherwise discover another process.
    fn check_pending_action(&self, scope: &Key, input: &Request) -> Result<(), String> {
        if matches!(
            input.query,
            Query::PrepareAction { .. }
                | Query::SynchronizeAction { .. }
                | Query::ReleaseAction { .. }
                | Query::Close
        ) {
            return Ok(());
        }
        let entries = self.inner.entries.lock().unwrap();
        if entries.iter().any(|(key, entry)| {
            key.owner == scope.owner
                && key.project == scope.project
                && key.root == scope.root
                && key.provider == scope.provider
                && key.digest == scope.digest
                && key.server == scope.server
                && key.client == scope.client
                && !*entry.stopped.borrow()
                && entry
                    .session
                    .get()
                    .is_some_and(commands::has_pending_action)
        }) {
            return Err("A matching reviewed command is being applied; finish or discard that action before requesting analysis".into());
        }
        Ok(())
    }
    fn acquire(
        &self,
        key: &Key,
        input: &Request,
    ) -> Result<(ActiveRequest, watch::Receiver<bool>), String> {
        let mut entries = self.inner.entries.lock().unwrap();
        // Validate stale requests before they can cancel another document's
        // speculative work, and admit before allocation can evict any service.
        if let Some(entry) = entries.get(key) {
            entry.activity.lock().unwrap().check(input)?;
        }
        prefetch::admit(&self.inner, &entries, key, input)?;
        let entry = Self::entry(&mut entries, key)?;
        reuse::offer(&mut entries, key, &entry);
        // Registration and lookup share the eviction lock. A returned service
        // is already busy, including while its process is still starting.
        ActiveRequest::register(entry, input)
    }
    fn acquire_action(
        &self,
        scope: &Key,
        input: &Request,
        server: &str,
    ) -> Result<(Key, ActiveRequest, watch::Receiver<bool>), String> {
        let entries = self.inner.entries.lock().unwrap();
        let (key,entry)=entries.iter().find(|(key,entry)| key.owner==scope.owner && key.project==scope.project && key.root==scope.root
            && key.provider==scope.provider && key.digest==scope.digest && key.server==scope.server && key.client==scope.client
            && !*entry.stopped.borrow() && entry.session.get().is_some_and(|session|session.id==server))
            .ok_or("The original language service is unavailable; action synchronization never starts a replacement")?;
        entry.activity.lock().unwrap().check(input)?;
        prefetch::admit(&self.inner, &entries, key, input)?;
        let (active, cancelled) = ActiveRequest::register(entry.clone(), input)?;
        Ok((key.clone(), active, cancelled))
    }
    fn entry(entries: &mut BTreeMap<Key, Arc<Entry>>, key: &Key) -> Result<Arc<Entry>, String> {
        let mut retained = None;
        if let Some(entry) = entries.get(key) {
            let restart = *entry.stopped.borrow()
                && entry
                    .failure
                    .lock()
                    .unwrap()
                    .until
                    .is_none_or(|until| until <= Instant::now());
            if !restart {
                *entry.used.lock().unwrap() = Instant::now();
                return Ok(Arc::clone(entry));
            }
            retained = reuse::snapshot(entry);
            entries.remove(key);
        }
        let owner_full = entries
            .keys()
            .filter(|entry| entry.owner == key.owner)
            .count()
            >= MAX_OWNER_SERVICES;
        let mut handoff = None;
        if entries.len() >= MAX_SERVICES || owner_full {
            let oldest = entries
                .iter()
                .filter(|(candidate, entry)| {
                    (!owner_full || candidate.owner == key.owner) && !entry.busy()
                })
                .min_by_key(|(_, entry)| *entry.used.lock().unwrap())
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                if let Some(entry) = entries.remove(&oldest) {
                    handoff = reuse::retire(entry, &oldest, key);
                }
            } else {
                return Err(
                    "Language-server capacity is busy; finish current requests or discard reviewed actions before retrying".into(),
                );
            }
        }
        let entry = Arc::new(Entry::new());
        *entry.retained.lock().unwrap() = retained;
        if let Some(handoff) = handoff {
            *entry.starting.lock().unwrap() = handoff.identity();
            *entry.handoff.lock().unwrap() = Some(handoff);
        }
        entries.insert(key.clone(), Arc::clone(&entry));
        Ok(entry)
    }
    async fn close(&self, key: &Key, input: &Request, language: &str) -> Result<Response, String> {
        let acquired = {
            let entries = self.inner.entries.lock().unwrap();
            let matching = entries
                .iter()
                .filter(|(candidate, _)| {
                    candidate.owner == key.owner
                        && candidate.project == key.project
                        && candidate.root == key.root
                        && candidate.provider == key.provider
                        && candidate.digest == key.digest
                        && candidate.server == key.server
                        && candidate.client == key.client
                })
                .map(|(_, entry)| Arc::clone(entry))
                .collect::<Vec<_>>();
            // Check the entire scope before cancelling any retained request.
            for entry in &matching {
                entry.activity.lock().unwrap().check(input)?;
            }
            prefetch::admit(&self.inner, &entries, key, input)?;
            matching
                .into_iter()
                .map(|entry| ActiveRequest::register(entry, input))
                .collect::<Result<Vec<_>, _>>()?
        };
        // An inert acknowledgement has no executable server identity. No entry,
        // process, retry timer or startup failure is created by a cold close.
        let mut response = Response {
            version: protocol::VERSION,
            client: input.client.clone(),
            document: input.document.clone(),
            request: input.request,
            revision: input.revision,
            source_digest: input.source_digest.clone(),
            server: "closed".into(),
            context: None,
            capabilities: json!({}),
            result: Value::Null,
            diagnostics: vec![],
            diagnostics_ready: true,
        };
        for (active, mut cancelled) in acquired {
            let mut stopped = active.entry.stopped.subscribe();
            if *stopped.borrow() {
                continue;
            }
            if let Some(session) = active.entry.session.get() {
                response = tokio::select! {
                    _ = stopped.changed() => continue,
                    _ = cancelled.changed() => return Err("Language close request was superseded".into()),
                    result = async {
                        let mut documents = session.documents.lock().await;
                        documents.query(session, input, &key.root.join(&input.path), language).await.map(|(response,_)|response)
                    } => result?,
                };
            } else if let Some(documents) = active.entry.retained.lock().unwrap().as_mut() {
                documents.close_retained(input)?;
            }
        }
        Ok(response)
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
                tokio::time::sleep(Duration::from_secs(2)).await;
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                resources::enforce(&inner).await;
                cache::poll(&inner).await;
                inner.outcomes.lock().unwrap().prune();
                let mut entries = inner.entries.lock().unwrap();
                reuse::consolidate_idle(&mut entries);
                entries.retain(|_, entry| {
                    let recent = entry.used.lock().unwrap().elapsed() < IDLE_TIMEOUT;
                    let busy = entry.busy();
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
    prefetch: bool,
    completed: bool,
}
impl Activity {
    fn check(&mut self, input: &Request) -> Result<(), String> {
        if self
            .watermarks
            .get(&input.document)
            .is_some_and(|(request, revision, _)| {
                input.request <= *request || input.revision < *revision
            })
        {
            return Err("Language-server request is stale or was superseded".into());
        }
        if self.active.len() >= 32 && !self.active.contains_key(&input.document) {
            return Err("Too many concurrent language documents".into());
        }
        if !self.watermarks.contains_key(&input.document) && self.watermarks.len() >= 128 {
            self.watermarks.retain(|id, (_, _, seen)| {
                self.active.contains_key(id) || seen.elapsed() < 2 * REQUEST_TIMEOUT
            });
            if self.watermarks.len() >= 128 {
                return Err(
                    "Too many recent language documents; retry after current requests expire"
                        .into(),
                );
            }
        }
        Ok(())
    }
}
impl ActiveRequest {
    fn register(
        entry: Arc<Entry>,
        input: &Request,
    ) -> Result<(Self, watch::Receiver<bool>), String> {
        let mut activity = entry.activity.lock().unwrap();
        activity.check(input)?;
        let (cancel, cancelled) = watch::channel(false);
        activity.watermarks.insert(
            input.document.clone(),
            (input.request, input.revision, Instant::now()),
        );
        if let Some(previous) = activity.active.insert(
            input.document.clone(),
            InFlight {
                request: input.request,
                cancel,
                prefetch: input.query.is_prefetch(),
            },
        ) {
            previous.cancel.send_replace(true);
        }
        drop(activity);
        Ok((
            Self {
                entry,
                document: input.document.clone(),
                request: input.request,
                prefetch: input.query.is_prefetch(),
                completed: false,
            },
            cancelled,
        ))
    }
}
impl Drop for ActiveRequest {
    fn drop(&mut self) {
        if self.prefetch
            && !self.completed
            && let Some(session) = self.entry.session.get()
        {
            // Publish suspension before freeing the activity slot. A new
            // prefetch must not race into a server still computing the old RPC.
            session
                .prefetch_enabled
                .store(false, std::sync::atomic::Ordering::Relaxed);
        }
        let mut activity = self.entry.activity.lock().unwrap();
        if activity
            .active
            .get(&self.document)
            .is_some_and(|active| active.request == self.request)
        {
            activity.active.remove(&self.document);
        }
        drop(activity);
        *self.entry.used.lock().unwrap() = Instant::now();
    }
}

// The startup PID must disappear on cancellation as well as failure. Completed
// sessions expose their process directly; a stale PID must never be sampled.
struct StartingProcess<'a>(&'a Mutex<Option<ProcessIdentity>>);
impl Drop for StartingProcess<'_> {
    fn drop(&mut self) {
        *self.0.lock().unwrap() = None;
    }
}

impl Session {
    async fn check_context(&self) -> Result<(), String> {
        self.check_folders()?;
        if let Some(cache) = &self.cache {
            cache.check()?;
        }
        self.program.check().await?;
        self.repository.check(&self.workspace).await?;
        self.check_folders()
    }
    fn check_folders(&self) -> Result<(), String> {
        if !same_file::Handle::from_path(&self.root).is_ok_and(|handle| handle == self.root_handle)
        {
            return Err(
                "The physical project folder changed since language service startup".into(),
            );
        }
        if !same_file::Handle::from_path(&self.workspace)
            .is_ok_and(|handle| handle == self.workspace_handle)
        {
            return Err("The physical language workspace changed since service startup".into());
        }
        Ok(())
    }
    async fn start(
        scope: &Scope,
        workspace: &std::path::Path,
        entry: &Entry,
        inner: &Inner,
    ) -> Result<Self, String> {
        openwebide_core::plugins::languages::validate_virtual_documents(
            &scope.server.virtual_documents,
        )?;
        let uri =
            url::Url::from_directory_path(workspace).map_err(|()| "Invalid project folder URI")?;
        let root_handle =
            same_file::Handle::from_path(&scope.root).map_err(|error| error.to_string())?;
        let workspace_handle =
            same_file::Handle::from_path(workspace).map_err(|error| error.to_string())?;
        let repository = repository::Repository::new(workspace).await?;
        let inputs = inputs::Inputs::new(&scope.root, scope.server.project_inputs.as_ref()).await?;
        let program = program::Program::prepare(&scope.server.command, workspace).await?;
        let mut cache = inner
            .cache
            .prepare(scope, workspace, &program, &inputs, &inner.coordinator)
            .await?;
        let bound = if let Some(cache) = &cache {
            openwebide_core::plugins::languages::bind_server_cache(
                &scope.server,
                cache
                    .data
                    .to_str()
                    .ok_or("Language cache path must be UTF-8")?,
            )?
        } else {
            openwebide_core::plugins::languages::BoundServer {
                server: scope.server.clone(),
                environment: BTreeMap::new(),
            }
        };
        if let Some(cache) = cache.as_mut() {
            cache.starting();
        }
        let (mut process, events) = Process::spawn_configured(
            &bound.server,
            workspace,
            program.path(),
            &bound.environment,
        )?;
        let cache = cache.map(|cache| cache.activate(process.termination()));
        let _starting = StartingProcess(&entry.starting);
        *entry.starting.lock().unwrap() = ProcessIdentity::capture(&process);
        let client = process.client();
        let diagnostics = Arc::new(Mutex::new(BTreeMap::new()));
        let pump = messages::pump(
            client.clone(),
            events,
            Arc::clone(&diagnostics),
            &uri,
            bound.server.settings.clone(),
        );
        let initialized = match client
            .request(
                "initialize",
                openwebide_agent::plugins::languages::initialization_params(
                    uri.as_str(),
                    bound.server.initialization_options.clone(),
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
        if !bound.server.settings.is_null() {
            client
                .notify(
                    "workspace/didChangeConfiguration",
                    json!({"settings":bound.server.settings}),
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        if !same_file::Handle::from_path(&scope.root).is_ok_and(|handle| handle == root_handle) {
            return Err("The project folder changed during language service startup".into());
        }
        if !same_file::Handle::from_path(workspace).is_ok_and(|handle| handle == workspace_handle) {
            return Err("The language workspace changed during service startup".into());
        }
        repository.check(workspace).await?;
        if !inputs.is_baseline(&inputs.observe(&scope.root).await?) {
            return Err("Declared project inputs changed during language service startup".into());
        }
        let program = program
            .initialized(process.id().ok_or("Language server has no process ID")?)
            .await?;
        Ok(Self {
            analysis: analysis::Analysis::default(),
            prefetch_enabled: std::sync::atomic::AtomicBool::new(true),
            sources: OnceCell::new(),
            cache,
            root: scope.root.clone(),
            root_handle,
            workspace: workspace.into(),
            workspace_handle,
            repository,
            program,
            inputs,
            commands: Mutex::default(),
            id: format!("{:032x}", rand::random::<u128>()),
            process: Mutex::new(process),
            client,
            capabilities,
            virtual_documents: scope.server.virtual_documents.clone(),
            documents: Arc::new(tokio::sync::Mutex::new(documents::Documents::default())),
            diagnostics,
            pump,
        })
    }
}
