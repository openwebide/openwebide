//! Native process lifetime and stdio primitives. Commands come only from the
//! selected installed declaration; invocation data cannot replace the command.
use super::rpc::{Client, ServerMessage};
use openwebide_core::plugins::languages::LanguageServer;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use std::process::Stdio;
use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, BufReader},
    sync::mpsc,
};

pub struct Process {
    client: Client,
    child: Option<tokio::process::Child>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    owner: Option<crate::exec::process_owner::Owner>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    server_pid: u32,
    termination: crate::exec::proc::Cleanup,
    terminated: Option<tokio::sync::watch::Sender<Option<Result<(), String>>>>,
    stderr: Arc<Mutex<VecDeque<u8>>>,
    stderr_task: Option<tokio::task::JoinHandle<()>>,
    #[cfg(unix)]
    group: Option<crate::exec::proc::GroupGuard>,
    #[cfg(windows)]
    job: Option<std::os::windows::io::OwnedHandle>,
}
impl Process {
    pub fn spawn(
        server: &LanguageServer,
        root: &Path,
    ) -> Result<(Self, mpsc::Receiver<ServerMessage>), String> {
        Self::spawn_resolved(server, root, Path::new(&server.command))
    }
    pub(crate) fn spawn_resolved(
        server: &LanguageServer,
        root: &Path,
        program: &Path,
    ) -> Result<(Self, mpsc::Receiver<ServerMessage>), String> {
        Self::spawn_configured(server, root, program, &std::collections::BTreeMap::new())
    }
    pub(crate) fn spawn_configured(
        server: &LanguageServer,
        root: &Path,
        program: &Path,
        environment: &std::collections::BTreeMap<String, String>,
    ) -> Result<(Self, mpsc::Receiver<ServerMessage>), String> {
        if server.cache.is_some() {
            return Err("Language-server cache paths must be bound before launching".into());
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let mut command = tokio::process::Command::new(program);
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        command
            .args(&server.args)
            .envs(environment)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
        command.process_group(0);
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let mut child = command.spawn().map_err(|error| {
            format!("Cannot start language server '{}': {error}. Install the declared server and its toolchain on this execution host.", server.command)
        })?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let (owner, mut child, server_pid) =
            crate::exec::process_owner::Owner::spawn(program, &server.args, root, environment)?;
        #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
        let group = Some(crate::exec::proc::GroupGuard::new(
            i32::try_from(child.id().ok_or("Language server has no process ID")?)
                .map_err(|error| error.to_string())?,
        ));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let group = None;
        #[cfg(windows)]
        let job = crate::exec::proc::windows_job(&child)?;
        let input = child
            .stdin
            .take()
            .ok_or("Language server has no input pipe")?;
        let output = child
            .stdout
            .take()
            .ok_or("Language server has no output pipe")?;
        let (client, events) = Client::connect(input, BufReader::new(output));
        let mut errors = child
            .stderr
            .take()
            .ok_or("Language server has no diagnostic pipe")?;
        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        let retained = Arc::clone(&stderr);
        let stderr_task = tokio::spawn(async move {
            let mut bytes = [0; 4096];
            while let Ok(count) = errors.read(&mut bytes).await {
                if count == 0 {
                    break;
                }
                let mut retained = retained.lock().unwrap();
                retained.extend(&bytes[..count]);
                while retained.len() > 16 * 1024 {
                    retained.pop_front();
                }
            }
        });
        let (terminated, termination) = crate::exec::proc::cleanup_channel();
        Ok((
            Self {
                client,
                child: Some(child),
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                owner: Some(owner),
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                server_pid,
                termination,
                terminated: Some(terminated),
                stderr,
                stderr_task: Some(stderr_task),
                #[cfg(unix)]
                group,
                #[cfg(windows)]
                job: Some(job),
            },
            events,
        ))
    }
    pub fn client(&self) -> Client {
        self.client.clone()
    }
    pub fn id(&self) -> Option<u32> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        return self.child.as_ref().map(|_| self.server_pid);
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        self.child.as_ref().and_then(tokio::process::Child::id)
    }
    pub(crate) fn resource_id(&self) -> Option<u32> {
        self.child.as_ref().and_then(tokio::process::Child::id)
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn owner(&self) -> Option<crate::exec::process_owner::Owner> {
        self.owner.clone()
    }
    pub(crate) fn termination(&self) -> crate::exec::proc::Cleanup {
        self.termination.clone()
    }
    pub async fn failure_detail(&mut self) -> String {
        self.stop();
        let _ =
            tokio::time::timeout(Duration::from_millis(100), self.termination.clone().wait()).await;
        if let Some(mut task) = self.stderr_task.take()
            && tokio::time::timeout(Duration::from_millis(100), &mut task)
                .await
                .is_err()
        {
            task.abort();
        }
        let bytes = self
            .stderr
            .lock()
            .unwrap()
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let tail = String::from_utf8_lossy(&bytes);
        let mut start = tail.len().saturating_sub(1024);
        while !tail.is_char_boundary(start) {
            start += 1;
        }
        tail[start..]
            .chars()
            .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
            .collect()
    }
    pub fn stop(&mut self) {
        self.client.close();
        let Some(mut child) = self.child.take() else {
            return;
        };
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(owner) = self.owner.take() {
            owner.stop();
            let done = self
                .terminated
                .take()
                .expect("A process has one termination owner");
            crate::exec::proc::track_cleanup(tokio::spawn(async move {
                let result = tokio::time::timeout(Duration::from_secs(3), child.wait())
                    .await
                    .map_err(|_| "Native process owner has not confirmed cleanup".to_string())
                    .and_then(|result| result.map_err(|e| e.to_string()))
                    .and_then(|status| {
                        if status.success() {
                            Ok(())
                        } else {
                            Err("Native process owner exited without confirming cleanup".into())
                        }
                    });
                drop(owner);
                done.send_replace(Some(result));
            }));
            return;
        }
        let _ = child.start_kill();
        #[cfg(unix)]
        let group = self
            .group
            .take()
            .map(crate::exec::proc::GroupGuard::terminate);
        #[cfg(windows)]
        let job = self.job.take();
        let done = self
            .terminated
            .take()
            .expect("A process has one termination owner");
        crate::exec::proc::track_cleanup(tokio::spawn(async move {
            let reaped = async {
                tokio::time::timeout(Duration::from_secs(3), child.wait())
                    .await
                    .map_err(|_| "Native process did not exit after termination".to_string())?
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            };
            let group_stopped = async {
                #[cfg(unix)]
                if let Some(group) = group {
                    return group.wait().await;
                }
                #[cfg(windows)]
                if let Some(job) = job {
                    return crate::exec::proc::terminate_job(job).await;
                }
                Ok::<(), String>(())
            };
            let (group, child) = tokio::join!(group_stopped, reaped);
            done.send_replace(Some(group.and(child)));
        }));
    }
    pub async fn shutdown(&mut self) {
        if !self.client.is_closed() {
            let _ = self
                .client
                .request("shutdown", serde_json::Value::Null, Duration::from_secs(2))
                .await;
            let _ = self.client.notify("exit", serde_json::Value::Null).await;
            if let Some(child) = self.child.as_mut() {
                let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            }
        }
        self.stop();
        let _ = self.termination.clone().wait().await;
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        self.stop();
        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
    }
}
