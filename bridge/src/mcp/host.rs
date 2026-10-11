//! Host filesystem, lifetime and process primitives. MCP policy is shared in agent::mcp.
use super::Connection;
use openwebide_agent::mcp::{self, Config, Transport};
use openwebide_core::{
    ToolCall,
    mcp::{Catalog, Tool},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::AsyncReadExt,
    sync::{Mutex, OwnedSemaphorePermit, Semaphore},
};

#[derive(Clone)]
pub struct Host {
    entries: Arc<Mutex<BTreeMap<String, Arc<Entry>>>>,
    capacity: Arc<Semaphore>,
    catalogs: Arc<Semaphore>,
    config: Option<PathBuf>,
}
struct Entry {
    owner: String,
    catalog: Catalog,
    servers: BTreeMap<String, Arc<Mutex<Process>>>,
    last_used: std::sync::Mutex<Instant>,
    closed: std::sync::atomic::AtomicBool,
    cancelled: tokio::sync::Notify,
    _permit: OwnedSemaphorePermit,
}
struct Process {
    connection: Option<Connection>,
    config: mcp::Server,
    cwd: PathBuf,
    tools: Vec<Tool>,
    _permit: OwnedSemaphorePermit,
}
impl Default for Host {
    fn default() -> Self {
        let config = std::env::var_os("OPENWEBIDE_MCP_CONFIG")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                    .map(|home| PathBuf::from(home).join(".openwebide/mcp.json"))
            });
        Self::new(config)
    }
}
impl Host {
    pub fn new(config: Option<PathBuf>) -> Self {
        Self {
            entries: Arc::default(),
            capacity: Arc::new(Semaphore::new(32)),
            catalogs: Arc::new(Semaphore::new(32)),
            config,
        }
    }
    async fn read_config(path: &Path) -> Result<Config, String> {
        let file = match tokio::fs::File::open(path).await {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Config::default());
            }
            Err(error) => return Err(format!("Cannot read MCP configuration: {error}")),
        };
        let mut bytes = Vec::new();
        file.take(256 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| error.to_string())?;
        Config::parse(&bytes)
    }
    pub async fn open(
        &self,
        owner: String,
        cwd: PathBuf,
        project_config: bool,
    ) -> Result<Catalog, String> {
        self.reap().await;
        let global = match &self.config {
            Some(path) => Self::read_config(path).await?,
            None => Config::default(),
        };
        let config = if project_config {
            global.merge(Self::read_config(&cwd.join(".openwebide/mcp.json")).await?)?
        } else {
            global
        };
        if config.servers.values().all(|server| server.disabled) {
            return Ok(Catalog::default());
        }
        let permit = self
            .catalogs
            .clone()
            .try_acquire_owned()
            .map_err(|_| "Too many active MCP catalogs")?;
        let id = format!("{:032x}", rand::random::<u128>());
        let prepared = mcp::prepare_servers(
            &Factory {
                capacity: self.capacity.clone(),
                cwd,
            },
            config,
        )
        .await;
        let mut catalog = prepared.catalog;
        if catalog.tools.is_empty() {
            return Ok(catalog);
        }
        catalog.id = id.clone();
        let servers = prepared
            .connections
            .into_iter()
            .map(|(name, mut process, tools)| {
                process.tools = tools;
                (name, Arc::new(Mutex::new(process)))
            })
            .collect();
        let entry = Arc::new(Entry {
            owner,
            catalog: catalog.clone(),
            servers,
            last_used: std::sync::Mutex::new(Instant::now()),
            closed: std::sync::atomic::AtomicBool::new(false),
            cancelled: tokio::sync::Notify::new(),
            _permit: permit,
        });
        self.entries.lock().await.insert(id, entry);
        Ok(catalog)
    }
    async fn entry(&self, owner: &str, id: &str) -> Result<Arc<Entry>, String> {
        let entry = self
            .entries
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or("MCP catalog expired; start a new run")?;
        if entry.owner != owner {
            return Err("MCP catalog belongs to another host account".into());
        }
        *entry.last_used.lock().unwrap() = Instant::now();
        Ok(entry)
    }
    pub async fn close(&self, owner: &str, id: &str) -> Result<(), String> {
        let entry = self.entry(owner, id).await?;
        entry
            .closed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        entry.cancelled.notify_waiters();
        self.entries.lock().await.remove(id);
        Ok(())
    }
    pub async fn reap(&self) {
        let ids: Vec<_> = self
            .entries
            .lock()
            .await
            .iter()
            .filter(|(_, entry)| {
                entry.last_used.lock().unwrap().elapsed() >= Duration::from_secs(60 * 60)
            })
            .map(|(id, entry)| (id.clone(), entry.owner.clone()))
            .collect();
        for (id, owner) in ids {
            let _ = self.close(&owner, &id).await;
        }
    }
    pub async fn shutdown(&self) {
        let entries = std::mem::take(&mut *self.entries.lock().await);
        for entry in entries.into_values() {
            entry
                .closed
                .store(true, std::sync::atomic::Ordering::SeqCst);
            entry.cancelled.notify_waiters();
        }
    }
    pub async fn invoke(
        &self,
        owner: &str,
        catalog: &Catalog,
        call: &ToolCall,
    ) -> Result<openwebide_agent::ToolOutcome, String> {
        let entry = self.entry(owner, &catalog.id).await?;
        if entry.catalog != *catalog {
            return Err("MCP catalog changed".into());
        }
        let tool = entry
            .catalog
            .tools
            .iter()
            .find(|tool| tool.definition.name == call.name)
            .ok_or("MCP tool was not discovered for this run")?;
        let cancelled = entry.cancelled.notified();
        tokio::pin!(cancelled);
        cancelled.as_mut().enable();
        if entry.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("MCP catalog closed".into());
        }
        let server = entry
            .servers
            .get(&tool.server)
            .ok_or("MCP server is unavailable")?;
        let mut process = tokio::select! {
            process = tokio::time::timeout(Duration::from_secs(60), server.lock()) => process.map_err(|_| "MCP server is busy; waiting for its connection timed out")?,
            () = &mut cancelled => return Err("MCP invocation cancelled".into()),
        };
        if entry.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("MCP catalog closed".into());
        }
        // A dropped/failed request owns and closes its process. Never replay an
        // in-flight side effect; cancellation also covers waiting and rediscovery.
        let result = tokio::select! {
            result = async {
                let mut connection = if let Some(connection) = process.connection.take() {
                    connection
                } else {
                    let mut connection = Connection::spawn(&process.config, &process.cwd)?;
                    let tools = tokio::time::timeout(Duration::from_secs(15), async {
                        mcp::initialize(&mut connection).await?;
                        mcp::discover(&mut connection, &tool.server).await
                    }).await.map_err(|_| "MCP server rediscovery timed out")??;
                    if tools != process.tools {
                        return Err("MCP tools changed after restart; start a new run".into());
                    }
                    connection
                };
                let outcome = mcp::call(&mut connection, tool, call).await?;
                Ok((outcome, connection))
            } => result,
            () = &mut cancelled => Err("MCP invocation cancelled".into()),
        };
        match result {
            Ok((outcome, connection)) => {
                process.connection = Some(connection);
                Ok(outcome)
            }
            Err(error) => Err(error),
        }
    }
    pub fn transport(&self, owner: String, id: String) -> NativeTransport {
        let lease = Arc::new(Lease {
            host: self.clone(),
            owner: owner.clone(),
            id,
        });
        NativeTransport {
            _lease: lease,
            host: self.clone(),
            owner,
        }
    }
}
#[derive(Clone)]
pub struct NativeTransport {
    _lease: Arc<Lease>,
    host: Host,
    owner: String,
}
impl Transport for NativeTransport {
    async fn invoke(
        &self,
        catalog: &Catalog,
        call: &ToolCall,
    ) -> Result<openwebide_agent::ToolOutcome, String> {
        self.host.invoke(&self.owner, catalog, call).await
    }
}

struct Lease {
    host: Host,
    owner: String,
    id: String,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if !self.id.is_empty()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let host = self.host.clone();
            let owner = self.owner.clone();
            let id = self.id.clone();
            runtime.spawn(async move {
                let _ = host.close(&owner, &id).await;
            });
        }
    }
}

struct Factory {
    capacity: Arc<Semaphore>,
    cwd: PathBuf,
}
impl mcp::ProcessHost for Factory {
    type Connection = Process;
    async fn spawn(&self, config: &mcp::Server) -> Result<Process, String> {
        let permit = self
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| "MCP host process limit reached".to_owned())?;
        Ok(Process {
            connection: Some(Connection::spawn(config, &self.cwd)?),
            config: config.clone(),
            cwd: self.cwd.clone(),
            tools: Vec::new(),
            _permit: permit,
        })
    }
    async fn bounded<F: std::future::Future + Send>(
        &self,
        future: F,
        seconds: u64,
    ) -> Result<F::Output, String> {
        tokio::time::timeout(Duration::from_secs(seconds), future)
            .await
            .map_err(|_| "MCP discovery timed out".into())
    }
}
impl mcp::Rpc for Process {
    async fn probe(&mut self) -> Result<Option<serde_json::Value>, String> {
        self.connection
            .as_mut()
            .ok_or("MCP connection closed")?
            .probe()
            .await
    }
    async fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.connection
            .as_mut()
            .ok_or("MCP connection closed")?
            .request(method, params)
            .await
    }
    async fn notify(&mut self, method: &str, params: serde_json::Value) -> Result<(), String> {
        self.connection
            .as_mut()
            .ok_or("MCP connection closed")?
            .notify(method, params)
            .await
    }
}
