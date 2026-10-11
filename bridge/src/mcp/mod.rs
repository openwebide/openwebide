//! Native stdio primitives shared by server and paired execution hosts.
pub mod host;
use openwebide_agent::mcp::{MAX_MESSAGE_BYTES, Rpc, Server};
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub struct Connection {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    next_id: u64,
    modern: bool,
    rpc_error: Option<Value>,
    probe_id: Option<u64>,
    #[cfg(unix)]
    _group: crate::exec::proc::GroupGuard,
    #[cfg(windows)]
    _job: std::os::windows::io::OwnedHandle,
}
impl Connection {
    pub fn spawn(server: &Server, cwd: &Path) -> Result<Self, String> {
        let mut command = tokio::process::Command::new(&server.command);
        command
            .args(&server.args)
            .envs(&server.env)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .spawn()
            .map_err(|error| format!("Cannot start MCP server: {error}"))?;
        #[cfg(unix)]
        let group = crate::exec::proc::GroupGuard::new(
            i32::try_from(child.id().ok_or("MCP server has no process ID")?)
                .map_err(|error| error.to_string())?,
        );
        #[cfg(windows)]
        let job = windows_job(&child)?;
        Ok(Self {
            stdin: child.stdin.take().ok_or("MCP server has no stdin")?,
            stdout: BufReader::new(child.stdout.take().ok_or("MCP server has no stdout")?),
            child,
            next_id: 1,
            modern: false,
            rpc_error: None,
            probe_id: None,
            #[cfg(unix)]
            _group: group,
            #[cfg(windows)]
            _job: job,
        })
    }
    async fn write(&mut self, value: &Value) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err("MCP message exceeds its limit".into());
        }
        bytes.push(b'\n');
        self.stdin
            .write_all(&bytes)
            .await
            .map_err(|error| format!("MCP server input closed: {error}"))?;
        self.stdin.flush().await.map_err(|error| error.to_string())
    }
    async fn read(&mut self) -> Result<Value, String> {
        let mut bytes = Vec::new();
        loop {
            let part = self
                .stdout
                .fill_buf()
                .await
                .map_err(|error| error.to_string())?;
            if part.is_empty() {
                return Err("MCP server closed its output".into());
            }
            let length = part
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(part.len(), |position| position + 1);
            if bytes.len() + length > MAX_MESSAGE_BYTES {
                return Err("MCP message exceeds its limit".into());
            }
            bytes.extend_from_slice(&part[..length]);
            self.stdout.consume(length);
            if bytes.last() == Some(&b'\n') {
                break;
            }
        }
        serde_json::from_slice(&bytes)
            .map_err(|error| format!("Invalid MCP JSON-RPC message: {error}"))
    }
    async fn exchange(&mut self, method: &str, mut params: Value) -> Result<Value, String> {
        self.rpc_error = None;
        if self.modern {
            params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{},"io.modelcontextprotocol/clientInfo":{"name":"Open WebIDE","version":env!("CARGO_PKG_VERSION")}});
        }

        if self
            .child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Err("MCP server exited".into());
        }
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or("MCP request ID exhausted")?;
        self.write(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
            .await?;
        loop {
            let message = self.read().await?;
            if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
                return Err("Invalid MCP JSON-RPC version".into());
            }
            if let Some(method) = message.get("method").and_then(Value::as_str) {
                if let Some(id) = message.get("id") {
                    let response = if method == "ping" {
                        json!({"jsonrpc":"2.0", "id":id, "result":{}})
                    } else {
                        json!({"jsonrpc":"2.0", "id":id, "error":{"code":-32601,"message":"Client capability is unavailable"}})
                    };
                    self.write(&response).await?;
                }
                continue;
            }
            if self.probe_id.is_some() && message.get("id").and_then(Value::as_u64) == self.probe_id
            {
                self.probe_id = None;
                continue;
            }
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                return Err("MCP response has an unexpected request ID".into());
            }
            if let Some(error) = message.get("error") {
                self.rpc_error = Some(error.clone());
                return Err(format!(
                    "MCP request failed: {}",
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("JSON-RPC error")
                ));
            }
            return message
                .get("result")
                .cloned()
                .ok_or_else(|| "MCP response omitted its result".into());
        }
    }
}
impl Rpc for Connection {
    async fn probe(&mut self) -> Result<Option<Value>, String> {
        self.modern = true;
        let probe_id = self.next_id;
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            self.exchange("server/discover", json!({})),
        )
        .await;
        match result {
            Ok(Ok(result)) => Ok(Some(result)),
            Ok(Err(error))
                if self
                    .rpc_error
                    .as_ref()
                    .and_then(|value| value.get("code"))
                    .and_then(Value::as_i64)
                    .is_some_and(|code| (-32099..=-32020).contains(&code)) =>
            {
                Err(error)
            }
            Ok(Err(error)) if self.rpc_error.is_none() => Err(error),
            _ => {
                self.modern = false;
                self.probe_id = Some(probe_id);
                Ok(None)
            }
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let timeout = if method == "tools/call" { 60 } else { 10 };
        tokio::time::timeout(Duration::from_secs(timeout), self.exchange(method, params))
            .await
            .map_err(|_| "MCP request timed out".to_owned())?
    }
    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.write(&json!({"jsonrpc":"2.0", "method":method, "params":params})),
        )
        .await
        .map_err(|_| "MCP notification timed out".to_owned())?
    }
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(windows)]
fn windows_job(child: &tokio::process::Child) -> Result<std::os::windows::io::OwnedHandle, String> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows_sys::Win32::{
        Foundation::HANDLE,
        System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        },
    };
    // SAFETY: null arguments request an unnamed job with default security. Wrap
    // its unique handle immediately so every subsequent error closes it.
    let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let job = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle) };
    // SAFETY: this Windows structure is valid with all-zero fields; the only
    // configured limit kills descendants when this owning job handle closes.
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let length = u32::try_from(std::mem::size_of_val(&info)).map_err(|error| error.to_string())?;
    let process = child
        .raw_handle()
        .ok_or("MCP server has no process handle")?;
    // SAFETY: job/process handles are alive and the pointer/size describe info.
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle() as HANDLE,
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&info).cast(),
            length,
        )
    } == 0
        || unsafe { AssignProcessToJobObject(job.as_raw_handle() as HANDLE, process as HANDLE) }
            == 0
    {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(job)
}
