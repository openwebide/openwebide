//! Native process lifetime and stdio primitives. Commands come only from the
//! selected installed declaration; invocation data cannot replace the command.
use super::rpc::{Client, ServerMessage};
use openwebide_core::plugins::languages::LanguageServer;
use std::{
    collections::VecDeque,
    path::Path,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, BufReader},
    sync::mpsc,
};

pub struct Process {
    client: Client,
    child: tokio::process::Child,
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
        let mut command = tokio::process::Command::new(&server.command);
        command
            .args(&server.args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|error| {
            format!("Cannot start language server '{}': {error}. Install the declared server and its toolchain on this execution host.", server.command)
        })?;
        #[cfg(unix)]
        let group = crate::exec::proc::GroupGuard::new(
            i32::try_from(child.id().ok_or("Language server has no process ID")?)
                .map_err(|error| error.to_string())?,
        );
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
        Ok((
            Self {
                client,
                child,
                stderr,
                stderr_task: Some(stderr_task),
                #[cfg(unix)]
                group: Some(group),
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
        self.child.id()
    }
    pub async fn failure_detail(&mut self) -> String {
        self.stop();
        let _ = tokio::time::timeout(Duration::from_millis(100), self.child.wait()).await;
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
        let _ = self.child.start_kill();
        #[cfg(unix)]
        self.group.take();
        #[cfg(windows)]
        self.job.take();
    }
    pub async fn shutdown(&mut self) {
        if !self.client.is_closed() {
            let _ = self
                .client
                .request("shutdown", serde_json::Value::Null, Duration::from_secs(2))
                .await;
            let _ = self.client.notify("exit", serde_json::Value::Null).await;
            let _ = tokio::time::timeout(Duration::from_secs(1), self.child.wait()).await;
        }
        self.stop();
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
