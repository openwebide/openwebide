//! Browser capability proof and HTTP transport. Filesystem and transaction
//! semantics use the shared companion adapter and core interpreter.
use crate::{
    backend::Api,
    bridge::{BridgeConfig, BridgeCredentials},
    plugin_bridge::PluginBridgeClient,
};
use openwebide_core::workspace_transaction::companion::{
    CompanionFiles, FolderAction, FolderReply, FolderRequest, FolderTransport, challenge_name,
    fingerprint,
};
use std::{cell::Cell, rc::Rc};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{Blob, FileSystemDirectoryHandle, FileSystemFileHandle};

const REQUEST_TIMEOUT_MS: u32 = 30_000;
const OPEN_TIMEOUT_MS: u32 = 60_000;

async fn observe<T>(
    work: impl std::future::Future<Output = Result<T, String>>,
    milliseconds: u32,
) -> Result<T, String> {
    crate::util::with_deadline(work, milliseconds)
        .await
        .ok_or_else(|| "The folder response timed out. Reconnect the original folder before recovery; the execution host may still be finishing its accepted operation.".to_string())?
}

async fn send(client: &PluginBridgeClient, request: &FolderRequest) -> Result<FolderReply, String> {
    request.validate()?;
    client
        .host_request(
            "/workspace/files",
            serde_json::to_value(request).map_err(|error| error.to_string())?,
        )
        .await
}

pub struct BrowserFolderTransport {
    client: PluginBridgeClient,
    current: Rc<dyn Fn() -> bool>,
    handle: FileSystemDirectoryHandle,
    cleanup: Option<Rc<Cleanup>>,
}
struct Cleanup {
    client: PluginBridgeClient,
    user: i64,
    project: i64,
    session: String,
    closed: Cell<bool>,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        if self.closed.get() {
            return;
        }
        let client = self.client.clone();
        let request = FolderRequest {
            user: self.user,
            project: self.project,
            action: FolderAction::Close {
                session: self.session.clone(),
            },
        };
        spawn_local(async move {
            let _ = observe(send(&client, &request), REQUEST_TIMEOUT_MS).await;
        });
    }
}
impl FolderTransport for BrowserFolderTransport {
    async fn request(&self, request: &FolderRequest) -> Result<FolderReply, String> {
        self.request_with_deadline(request, REQUEST_TIMEOUT_MS)
            .await
    }
}
impl BrowserFolderTransport {
    async fn request_with_deadline(
        &self,
        request: &FolderRequest,
        milliseconds: u32,
    ) -> Result<FolderReply, String> {
        observe(self.dispatch(request), milliseconds).await
    }
    async fn dispatch(&self, request: &FolderRequest) -> Result<FolderReply, String> {
        let closing = matches!(request.action, FolderAction::Close { .. });
        if !closing && !(self.current)() {
            return Err("The selected browser folder changed".into());
        }
        if !closing {
            crate::local_fs::ensure_permission(&self.handle)
                .await
                .map_err(|error| error.to_string())?;
            if !(self.current)() {
                return Err("The selected browser folder changed while checking access".into());
            }
        }
        let result = send(&self.client, request).await?;
        if closing
            && matches!(&result, FolderReply::Closed)
            && let Some(cleanup) = &self.cleanup
        {
            cleanup.closed.set(true);
        }
        if !closing && !(self.current)() {
            return Err("The selected browser folder changed during its filesystem call".into());
        }
        if !closing {
            crate::local_fs::ensure_permission(&self.handle)
                .await
                .map_err(|error| error.to_string())?;
            if !(self.current)() {
                return Err("The selected browser folder changed after checking access".into());
            }
        }
        Ok(result)
    }
}

pub async fn open(
    api: Api,
    config: BridgeConfig,
    user: i64,
    project: i64,
    handle: FileSystemDirectoryHandle,
    expected: Option<String>,
    current: Rc<dyn Fn() -> bool>,
) -> Result<CompanionFiles<BrowserFolderTransport>, String> {
    observe(
        open_folder(api, config, user, project, handle, expected, current),
        OPEN_TIMEOUT_MS,
    )
    .await
}

async fn open_folder(
    api: Api,
    config: BridgeConfig,
    user: i64,
    project: i64,
    handle: FileSystemDirectoryHandle,
    expected: Option<String>,
    current: Rc<dyn Fn() -> bool>,
) -> Result<CompanionFiles<BrowserFolderTransport>, String> {
    if !current() {
        return Err("The selected browser folder changed".into());
    }
    let credentials = BridgeCredentials::new(api);
    // Resolve this exact candidate handle, including when an old origin-bound
    // handle is lost. The fresh challenge below establishes physical authority;
    // the existing path hint/discovery only locates its execution-host adapter.
    let path = crate::local_agent::resolve_bridge_cwd_guarded(
        api,
        handle.clone(),
        project,
        &config,
        &credentials,
        || current(),
    )
    .await
    .ok_or("Reconnect the selected folder to its paired execution host")?;
    let client = PluginBridgeClient::new(config.http_url, credentials);
    let request = FolderRequest {
        user,
        project,
        action: FolderAction::Open {
            path,
            expected: expected.clone(),
        },
    };
    let reply = observe(send(&client, &request), REQUEST_TIMEOUT_MS).await?;
    let FolderReply::Opened {
        session,
        challenge,
        identity,
    } = reply
    else {
        return Err("Invalid native folder challenge".into());
    };
    if challenge_name(&session)? != challenge
        || !fingerprint(&identity)
        || expected
            .as_ref()
            .is_some_and(|expected| *expected != identity)
    {
        return Err("Native folder challenge does not match the original folder".into());
    }
    let transport = BrowserFolderTransport {
        client: client.clone(),
        current: current.clone(),
        handle: handle.clone(),
        cleanup: Some(Rc::new(Cleanup {
            client,
            user,
            project,
            session: session.clone(),
            closed: Cell::new(false),
        })),
    };
    if !current() {
        return Err("The folder changed before proving access".into());
    }
    // This narrow raw-handle read deliberately bypasses normal file access:
    // only the exact validated, private challenge name may be read here.
    let file: FileSystemFileHandle = JsFuture::from(handle.get_file_handle(&challenge))
        .await
        .map_err(|error| {
            format!("The selected browser folder cannot read its native proof: {error:?}")
        })?
        .dyn_into()
        .map_err(|_| "The folder proof is not a file")?;
    let blob: Blob = JsFuture::from(file.get_file())
        .await
        .map_err(|error| format!("Could not read the folder proof: {error:?}"))?
        .dyn_into()
        .map_err(|_| "The folder proof is not readable")?;
    if !(0.0..=64.0).contains(&blob.size()) {
        return Err("Invalid folder proof size".into());
    }
    let proof = JsFuture::from(blob.text())
        .await
        .map_err(|error| format!("Could not read the folder proof: {error:?}"))?
        .as_string()
        .filter(|value| fingerprint(value))
        .ok_or("Invalid folder proof content")?;
    match transport
        .request(&FolderRequest {
            user,
            project,
            action: FolderAction::Prove {
                session: session.clone(),
                proof,
            },
        })
        .await?
    {
        FolderReply::Ready {
            identity: confirmed,
            worker,
        } if confirmed == identity => {
            CompanionFiles::new(transport, user, project, session, identity, worker)
        }
        _ => Err("Native folder proof was not acknowledged".into()),
    }
}

#[cfg(test)]
mod tests;
