//! Thin client for the backend REST API, including SSE streaming.

use gloo_net::http::{Method, RequestBuilder};

use leptos::prelude::{GetUntracked, Set, Update, WithValue};
use openwebide_core::{
    ChatCompletion, ChatMessage, ChatRequest, ChatSession, Connection, ConversationEntry,
    EditorContext, FileDiff, FileEntry, GitBranchInfo, GitCheckoutRequest, GitCheckoutResult,
    GitCommitRequest, GitCommitResult, GitRepoStatus, GitSyncRequest, GitSyncResult, Health,
    ModelInfo, NewConnection, NewProject, NewSession, PersistedEdit, Project, ProviderKind,
    ResolveEditRequest, Role, RunEvent, SearchHit, SystemPrompt, TurnTelemetry, User,
    WebSearchResult, WorkspaceMode, vfs::SearchOptions,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use wasm_bindgen_futures::JsFuture;
use web_sys::wasm_bindgen::JsCast;
use web_sys::{AbortSignal, ReadableStreamDefaultReader, ReadableStreamReadResult};

pub(crate) struct CommandFetchGuard(pub(crate) web_sys::AbortController);

impl Drop for CommandFetchGuard {
    fn drop(&mut self) {
        // Dropping a fetch future alone leaves the browser's HTTP connection occupied.
        self.0.abort();
    }
}

#[derive(Clone, Copy)]
pub struct BackendApi {
    base: leptos::prelude::StoredValue<String>,
    pub signed_in: leptos::prelude::RwSignal<bool>,
    pub session_expired: leptos::prelude::RwSignal<bool>,
    cross_origin: bool,
    session_revision: leptos::prelude::RwSignal<u64>,
}

/// The `{user, token}` payload returned by register and login.
#[derive(Deserialize)]
struct AuthResponse {
    user: User,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HealthState {
    Online { version: String },
    Offline,
}

struct RequestError {
    status: Option<u16>,
    message: String,
}
impl RequestError {
    fn transport(message: String) -> Self {
        Self {
            status: None,
            message,
        }
    }
    fn into_recovery(self) -> crate::backend::RecoveryError {
        if self.status == Some(409) {
            crate::backend::RecoveryError::Conflict(self.message)
        } else {
            crate::backend::RecoveryError::Unavailable(self.message)
        }
    }
}

impl BackendApi {
    fn base(&self) -> String {
        self.base.with_value(String::clone)
    }

    pub fn session_expired(&self) -> leptos::prelude::ReadSignal<bool> {
        self.session_expired.read_only()
    }

    /// Same-origin by default; `?api=http://host:port/api` overrides the
    /// base for development against a separately served backend.
    pub fn from_location() -> Self {
        let location = web_sys::window().map(|w| w.location());
        let origin = location
            .as_ref()
            .and_then(|l| l.origin().ok())
            .unwrap_or_else(|| "http://localhost:3000".to_string());
        let base = location
            .clone()
            .and_then(|l| l.search().ok())
            .and_then(|search| query_param(&search, "api"))
            .map(|v| v.trim_end_matches('/').to_string())
            .unwrap_or_else(|| format!("{origin}/api"));

        let cross_origin = !base.starts_with(&origin);

        if let Some(window) = web_sys::window()
            && let Ok(Some(storage)) = window.local_storage()
        {
            let _ = storage.remove_item("owide_token");
        }

        Self {
            base: leptos::prelude::StoredValue::new(base),
            signed_in: leptos::prelude::RwSignal::new(false),
            session_expired: leptos::prelude::RwSignal::new(false),
            cross_origin,
            session_revision: leptos::prelude::RwSignal::new(0),
        }
    }

    pub async fn host_connection(
        &self,
    ) -> Result<openwebide_core::host_admin::HostConnection, String> {
        self.get("/host/connection").await
    }
    pub async fn save_host_connection(
        &self,
        connection: &openwebide_core::host_admin::HostConnection,
    ) -> Result<openwebide_core::host_admin::HostConnection, String> {
        self.put("/host/connection", connection).await
    }
    pub async fn probe_host_connection(
        &self,
    ) -> Result<openwebide_core::host_admin::HostEnvironment, String> {
        self.post("/host/probe", &serde_json::json!({})).await
    }
    pub async fn host_view(
        &self,
        session: i64,
        request: &openwebide_core::host_admin::HostRequest,
    ) -> Result<openwebide_core::host_admin::HostResponse, String> {
        self.post(&format!("/sessions/{session}/host"), request)
            .await
    }
    pub async fn host_input(
        &self,
        session: i64,
        input: &openwebide_core::host_admin::HostInput,
    ) -> Result<(), String> {
        let _: serde_json::Value = self
            .post(&format!("/sessions/{session}/host-input"), input)
            .await?;
        Ok(())
    }

    pub async fn push_config(&self) -> Result<openwebide_core::push::PushConfig, String> {
        self.get("/push/config").await
    }
    pub async fn save_push_subscription(
        &self,
        subscription: &openwebide_core::push::PushSubscription,
    ) -> Result<(), String> {
        let _: openwebide_core::push::PushStatus =
            self.post("/push/subscriptions", subscription).await?;
        Ok(())
    }

    // -- auth --------------------------------------------------------------

    /// Register a new local account (only the first account may register).
    /// On success the returned token is stored for subsequent requests.
    pub async fn register(&self, username: &str, password: &str) -> Result<User, String> {
        let resp: AuthResponse = self
            .post(
                "/auth/register",
                &json!({ "username": username, "password": password }),
            )
            .await?;
        self.session_revision
            .update(|revision| *revision = revision.wrapping_add(1));
        self.signed_in.set(true);
        Ok(resp.user)
    }

    /// Log in with an existing account. On success the token is stored.
    pub async fn login(&self, username: &str, password: &str) -> Result<User, String> {
        let resp: AuthResponse = self
            .post(
                "/auth/login",
                &json!({ "username": username, "password": password }),
            )
            .await?;
        self.session_revision
            .update(|revision| *revision = revision.wrapping_add(1));
        self.signed_in.set(true);
        Ok(resp.user)
    }

    /// The account for the current token.
    pub async fn me(&self) -> Result<User, String> {
        let resp: serde_json::Value = self.get("/auth/me").await?;
        let user: User = serde_json::from_value(resp["user"].clone()).map_err(|e| e.to_string())?;
        self.session_revision
            .update(|revision| *revision = revision.wrapping_add(1));
        self.signed_in.set(true);
        Ok(user)
    }

    pub async fn logout(&self) -> Result<(), String> {
        self.session_revision
            .update(|revision| *revision = revision.wrapping_add(1));
        let _ = self
            .post::<_, serde_json::Value>("/auth/logout", &json!({}))
            .await;
        self.signed_in.set(false);
        Ok(())
    }

    pub async fn bridge_token(&self) -> Result<(String, i64), String> {
        #[derive(serde::Deserialize)]
        struct TokenResp {
            token: String,
            expires_at: i64,
        }
        let resp: TokenResp = self.post("/bridge/token", &json!({})).await?;
        Ok((resp.token, resp.expires_at))
    }

    pub async fn health(&self) -> Result<Health, String> {
        self.get("/health").await
    }

    pub async fn list_connections(&self) -> Result<Vec<Connection>, String> {
        self.get("/connections").await
    }

    pub async fn create_connection(
        &self,
        name: &str,
        kind: ProviderKind,
        base_url: &str,
        model: Option<&str>,
        context_limit: Option<usize>,
    ) -> Result<Connection, String> {
        let body = NewConnection {
            name: name.to_string(),
            kind,
            base_url: base_url.to_string(),
            model: model.map(str::to_string),
            context_limit,
        };
        self.post("/connections", &body).await
    }

    pub async fn update_connection(&self, connection: &Connection) -> Result<Connection, String> {
        self.put(&format!("/connections/{}", connection.id), connection)
            .await
    }

    pub async fn delete_connection(&self, id: i64) -> Result<(), String> {
        self.request::<(), _>(Method::DELETE, &format!("/connections/{id}"), None, false)
            .await
    }

    pub async fn list_sessions(&self) -> Result<Vec<ChatSession>, String> {
        self.get("/sessions").await
    }
    pub async fn search_sessions(
        &self,
        search: &openwebide_core::SessionSearch,
    ) -> Result<Vec<ChatSession>, String> {
        self.post("/sessions/search", search).await
    }
    pub async fn session_preferences(
        &self,
        id: i64,
        preferences: &openwebide_core::SessionPreferences,
    ) -> Result<ChatSession, String> {
        self.put(&format!("/sessions/{id}/preferences"), preferences)
            .await
    }
    pub async fn session_title(&self, id: i64) -> Result<Option<ChatSession>, String> {
        self.post(&format!("/sessions/{id}/title"), &serde_json::json!({}))
            .await
    }
    pub async fn export_session(&self, id: i64) -> Result<openwebide_core::SessionExport, String> {
        self.get(&format!("/sessions/{id}/export")).await
    }

    /// List the models a connection's provider reports.
    pub async fn list_models(&self, connection_id: i64) -> Result<Vec<ModelInfo>, String> {
        self.get(&format!("/models?connection_id={connection_id}"))
            .await
    }

    // -- system prompts ----------------------------------------------------

    pub async fn list_system_prompts(&self) -> Result<Vec<SystemPrompt>, String> {
        self.get("/system-prompts").await
    }

    pub async fn create_system_prompt(
        &self,
        name: &str,
        content: &str,
    ) -> Result<SystemPrompt, String> {
        self.post(
            "/system-prompts",
            &json!({ "name": name, "content": content }),
        )
        .await
    }

    pub async fn update_system_prompt(
        &self,
        id: i64,
        name: &str,
        content: &str,
    ) -> Result<SystemPrompt, String> {
        self.put(
            &format!("/system-prompts/{id}"),
            &json!({ "name": name, "content": content }),
        )
        .await
    }

    pub async fn delete_system_prompt(&self, id: i64) -> Result<(), String> {
        self.request::<(), _>(
            Method::DELETE,
            &format!("/system-prompts/{id}"),
            None,
            false,
        )
        .await
    }

    // -- settings ----------------------------------------------------------

    pub async fn editor_recovery(
        &self,
        project: i64,
    ) -> Result<openwebide_core::editor::EditorRecoveryRecord, crate::backend::RecoveryError> {
        let record: openwebide_core::editor::EditorRecoveryRecord = self
            .request_typed::<(), _>(
                Method::GET,
                &format!("/projects/{project}/editor-recovery"),
                None,
                false,
            )
            .await
            .map_err(RequestError::into_recovery)?;
        record
            .state
            .validate()
            .map_err(crate::backend::RecoveryError::Unavailable)?;
        if record.revision < 0 {
            return Err(crate::backend::RecoveryError::Unavailable(
                "Invalid editor recovery revision returned by server".into(),
            ));
        }
        Ok(record)
    }

    pub async fn save_editor_recovery(
        &self,
        project: i64,
        record: &openwebide_core::editor::EditorRecoveryRecord,
    ) -> Result<i64, crate::backend::RecoveryError> {
        #[derive(Deserialize)]
        struct Saved {
            revision: i64,
        }
        record
            .state
            .validate()
            .map_err(crate::backend::RecoveryError::Unavailable)?;
        let expected = record
            .revision
            .checked_add(1)
            .filter(|_| record.revision >= 0)
            .ok_or_else(|| {
                crate::backend::RecoveryError::Unavailable(
                    "Invalid editor recovery revision".into(),
                )
            })?;
        let response: Saved = self
            .request_typed(
                Method::PUT,
                &format!("/projects/{project}/editor-recovery"),
                Some(record),
                false,
            )
            .await
            .map_err(RequestError::into_recovery)?;
        if response.revision != expected {
            return Err(crate::backend::RecoveryError::Unavailable(
                "Invalid editor recovery revision returned by server".into(),
            ));
        }
        Ok(response.revision)
    }

    pub async fn get_settings(&self) -> Result<std::collections::BTreeMap<String, String>, String> {
        self.get("/settings").await
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> Result<(), String> {
        let _resp: serde_json::Value = self
            .put("/settings", &json!({ "key": key, "value": value }))
            .await?;
        Ok(())
    }

    pub async fn set_session_connection(
        &self,
        id: i64,
        connection_id: i64,
    ) -> Result<ChatSession, String> {
        self.put(
            &format!("/sessions/{id}/connection"),
            &json!({"connection_id": connection_id}),
        )
        .await
    }

    pub async fn preview_server(
        &self,
        probe: &openwebide_core::ModelProbe,
    ) -> Result<openwebide_core::ServerDiscovery, String> {
        self.post("/models/preview", probe).await
    }
    pub async fn save_model_setup(
        &self,
        probe: &openwebide_core::ModelProbe,
        profiles: &[openwebide_core::ModelProfile],
    ) -> Result<(Connection, openwebide_core::ModelSetup), String> {
        self.post(
            "/model-setup/save",
            &serde_json::json!({"probe":probe,"profiles":profiles}),
        )
        .await
    }
    pub async fn test_model(
        &self,
        probe: &openwebide_core::ModelProbe,
    ) -> Result<openwebide_core::ModelTestResult, String> {
        self.post("/models/preview/test", probe).await
    }
    pub async fn preview_model(
        &self,
        probe: &openwebide_core::ModelProbe,
    ) -> Result<openwebide_core::ModelDetection, String> {
        self.post("/models/preview/detect", probe).await
    }
    pub async fn detect_model(
        &self,
        id: i64,
        model: &str,
    ) -> Result<openwebide_core::ModelDetection, String> {
        self.post(
            "/model-setup/detect",
            &serde_json::json!({"server_id": id, "model": model}),
        )
        .await
    }
    pub async fn inspect_server(
        &self,
        base_url: &str,
        kind: Option<ProviderKind>,
    ) -> Result<openwebide_core::ServerDiscovery, String> {
        self.post(
            "/model-setup/inspect",
            &serde_json::json!({"base_url": base_url, "kind": kind}),
        )
        .await
    }
    pub async fn discover_servers(&self) -> Result<Vec<openwebide_core::ServerDiscovery>, String> {
        self.post("/model-setup/discover", &serde_json::json!({}))
            .await
    }
    pub async fn model_runtime(
        &self,
        id: i64,
        model: Option<&str>,
    ) -> Result<openwebide_core::ModelRuntime, String> {
        let query = model
            .map(|model| format!("?model={}", urlenc(model)))
            .unwrap_or_default();
        self.get(&format!("/connections/{id}/runtime{query}")).await
    }
    pub async fn model_setup(&self) -> Result<openwebide_core::ModelSetup, String> {
        self.get("/model-setup").await
    }
    pub async fn save_model_defaults(
        &self,
        defaults: &openwebide_core::ModelDefaults,
    ) -> Result<openwebide_core::ModelSetup, String> {
        self.put("/model-setup/defaults", defaults).await
    }
    pub async fn save_model_profile(
        &self,
        profile: &openwebide_core::ModelProfile,
    ) -> Result<openwebide_core::ModelSetup, String> {
        self.put("/model-setup/profile", profile).await
    }
    pub async fn server_settings(
        &self,
        id: i64,
    ) -> Result<openwebide_core::ServerSettings, String> {
        self.get(&format!("/connections/{id}/settings")).await
    }
    pub async fn save_server_settings(
        &self,
        id: i64,
        update: &openwebide_core::ServerSettingsUpdate,
    ) -> Result<openwebide_core::ServerSettings, String> {
        self.put(&format!("/connections/{id}/settings"), update)
            .await
    }

    pub async fn startup_context(
        &self,
        project_id: i64,
        tools: bool,
        connection: Option<i64>,
    ) -> Result<String, String> {
        let response: serde_json::Value = self
            .get(&format!(
                "/projects/{project_id}/files/context?tools={tools}&connection={}&browser_preferences={}",
                connection.map_or_else(String::new, |id| id.to_string()),
                js_sys::encode_uri_component(
                    &serde_json::to_string(&crate::browser_preferences::capture())
                        .map_err(|error| error.to_string())?
                )
            ))
            .await?;
        Ok(response["content"].as_str().unwrap_or_default().to_owned())
    }

    pub async fn create_session(
        &self,
        name: &str,
        connection_id: Option<i64>,
        system_prompt_id: Option<i64>,
        project_id: Option<i64>,
    ) -> Result<ChatSession, String> {
        let body = NewSession {
            auto_title: true,
            name: name.to_string(),
            connection_id,
            system_prompt_id,
            project_id,
        };
        self.post("/sessions", &body).await
    }

    // -- projects ----------------------------------------------------------

    pub async fn list_projects(&self) -> Result<Vec<Project>, String> {
        self.get("/projects").await
    }

    pub async fn create_project(
        &self,
        name: &str,
        mode: WorkspaceMode,
        path: Option<String>,
    ) -> Result<Project, String> {
        let body = NewProject {
            name: name.to_string(),
            mode,
            path,
        };
        self.post("/projects", &body).await
    }

    pub async fn rename_project(&self, id: i64, name: &str) -> Result<Project, String> {
        self.put(&format!("/projects/{id}"), &json!({ "name": name }))
            .await
    }

    pub async fn delete_project(&self, id: i64) -> Result<(), String> {
        self.request::<(), _>(Method::DELETE, &format!("/projects/{id}"), None, false)
            .await
    }

    // -- project files (remote mode) ---------------------------------------

    pub async fn list_files(&self, project_id: i64, path: &str) -> Result<Vec<FileEntry>, String> {
        self.get(&format!(
            "/projects/{project_id}/files?path={}",
            urlenc(path)
        ))
        .await
    }

    pub async fn read_file_object_url(
        &self,
        project_id: i64,
        path: &str,
    ) -> Result<String, String> {
        let url = format!(
            "{}/projects/{project_id}/files/raw?path={}",
            self.base(),
            urlenc(path)
        );
        let builder = self.builder(&url, Method::GET);
        let req = builder.build().map_err(|e| e.to_string())?;

        let is_signed_in = self.signed_in.get_untracked();
        let resp = req.send().await.map_err(|e| e.to_string())?;
        if !resp.ok() {
            if resp.status() == 401 && is_signed_in {
                self.signed_in.set(false);
                self.session_expired.set(true);
            }
            return Err(self.error_from(resp).await);
        }
        let web_resp = web_sys::Response::from(resp);
        let blob_promise = web_resp.blob().map_err(|e| format!("{e:?}"))?;
        let blob_js = JsFuture::from(blob_promise)
            .await
            .map_err(|e| format!("{e:?}"))?;
        let blob: web_sys::Blob = blob_js.unchecked_into();
        crate::workspace::preview_object_url(&blob, path).map_err(|e| format!("{e:?}"))
    }

    /// Read a file's contents as text.
    pub async fn canonical_file_path(&self, project: i64, path: &str) -> Result<String, String> {
        let value: serde_json::Value = self
            .get(&format!(
                "/projects/{project}/files/canonical?path={}",
                urlenc(path)
            ))
            .await?;
        value["path"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "Invalid canonical path response".into())
    }
    pub async fn read_file(&self, project_id: i64, path: &str) -> Result<String, String> {
        let value: serde_json::Value = self
            .get(&format!(
                "/projects/{project_id}/files/read?path={}",
                urlenc(path)
            ))
            .await?;
        Ok(value["content"].as_str().unwrap_or_default().to_string())
    }

    pub async fn read_file_lossy(&self, project_id: i64, path: &str) -> Result<String, String> {
        self.read_file_bytes(project_id, path)
            .await
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }
    pub async fn read_file_bytes(&self, project_id: i64, path: &str) -> Result<Vec<u8>, String> {
        let url = format!(
            "{}/projects/{project_id}/files/raw?path={}",
            self.base(),
            urlenc(path)
        );
        let builder = self
            .builder(&url, Method::GET)
            .header("Cache-Control", "no-cache");
        let req = builder.build().map_err(|e| e.to_string())?;

        let is_signed_in = self.signed_in.get_untracked();
        let resp = req.send().await.map_err(|e| e.to_string())?;
        if !resp.ok() {
            if resp.status() == 401 && is_signed_in {
                self.signed_in.set(false);
                self.session_expired.set(true);
            }
            return Err(self.error_from(resp).await);
        }
        resp.binary().await.map_err(|e| e.to_string())
    }

    /// Write text to a file (raw text body, not JSON).
    pub async fn write_file(
        &self,
        project_id: i64,
        path: &str,
        content: &str,
    ) -> Result<(), String> {
        let url = format!(
            "{}/projects/{project_id}/files/write?path={}",
            self.base(),
            urlenc(path)
        );
        let builder = self
            .builder(&url, Method::PUT)
            .header("content-type", "text/plain");
        let req = builder
            .body(content.to_string())
            .map_err(|e| e.to_string())?;

        let is_signed_in = self.signed_in.get_untracked();
        let resp = req.send().await.map_err(|e| e.to_string())?;
        if !resp.ok() {
            if resp.status() == 401 && is_signed_in {
                self.signed_in.set(false);
                self.session_expired.set(true);
            }
            return Err(self.error_from(resp).await);
        }
        Ok(())
    }

    pub async fn write_file_bytes(
        &self,
        project: i64,
        path: &str,
        bytes: &[u8],
    ) -> Result<(), String> {
        let url = format!(
            "{}/projects/{project}/files/write?path={}",
            self.base(),
            urlenc(path)
        );
        let request = self
            .builder(&url, Method::PUT)
            .header("content-type", "application/octet-stream")
            .body(js_sys::Uint8Array::from(bytes))
            .map_err(|e| e.to_string())?;
        let response = request.send().await.map_err(|e| e.to_string())?;
        if response.ok() {
            Ok(())
        } else {
            Err(self.error_from(response).await)
        }
    }
    pub async fn copy_file(&self, project_id: i64, from: &str, to: &str) -> Result<(), String> {
        let body = serde_json::json!({
            "from": from,
            "to": to,
        });
        self.request::<_, serde_json::Value>(
            Method::POST,
            &format!("/projects/{project_id}/files/copy"),
            Some(&body),
            false,
        )
        .await?;
        Ok(())
    }

    /// Create an empty file or a directory.
    pub async fn create_file(
        &self,
        project_id: i64,
        path: &str,
        is_dir: bool,
    ) -> Result<(), String> {
        let kind = if is_dir { "dir" } else { "file" };
        let _value: serde_json::Value = self
            .post(
                &format!(
                    "/projects/{project_id}/files/create?path={}&type={kind}",
                    urlenc(path)
                ),
                &json!({}),
            )
            .await?;
        Ok(())
    }

    /// Delete the file at `path`.
    pub async fn delete_file(&self, project_id: i64, path: &str) -> Result<(), String> {
        self.request::<(), _>(
            Method::DELETE,
            &format!("/projects/{project_id}/files/delete?path={}", urlenc(path)),
            None,
            false,
        )
        .await
    }

    /// List a directory of the host mount for the remote file browser.
    /// `path` is relative to the mount root (e.g. `~/source`); empty = the
    /// root itself.
    pub async fn browse(&self, path: &str) -> Result<Vec<FileEntry>, String> {
        self.get(&format!("/browse?path={}", urlenc(path))).await
    }

    /// Full-text search: return the lines of every file whose content contains
    /// `query` (case-insensitive).
    pub async fn search_content(
        &self,
        project_id: i64,
        query: &str,
        path: &str,
        opts: SearchOptions,
    ) -> Result<Vec<SearchHit>, String> {
        let mut url = format!(
            "/projects/{project_id}/files/content-search?q={}&path={}",
            urlenc(query),
            urlenc(path)
        );
        if opts.include_ignored {
            url.push_str("&include_ignored=1");
        }
        self.get(&url).await
    }

    // -- git operations (Phase 13) -----------------------------------------

    fn git_endpoint(project_id: Option<i64>, sub: &str) -> String {
        if let Some(id) = project_id {
            format!("/projects/{id}/git/{sub}")
        } else {
            format!("/git/{sub}")
        }
    }

    pub async fn session_search_suggestions(
        &self,
        search: &openwebide_core::SessionSearch,
    ) -> Result<openwebide_core::SessionSearchResults, String> {
        self.post("/sessions/search-suggestions", search).await
    }
    pub async fn assistance(
        &self,
        request: &openwebide_core::AssistanceRequest,
    ) -> Result<Option<String>, String> {
        let mut request = request.clone();
        if !request.staged_draft {
            request.input = openwebide_core::assistance::input_excerpt(&request.input);
        }
        self.post("/assistance", &request).await
    }
    pub async fn staged_assistance(
        &self,
        request: &openwebide_core::AssistanceRequest,
    ) -> Result<openwebide_core::assistance::GitDraftResult, String> {
        self.post("/assistance", request).await
    }
    pub async fn model_complete(
        &self,
        request: &openwebide_core::ChatRequest,
    ) -> Result<openwebide_core::ChatCompletion, String> {
        self.post("/models/complete", request).await
    }
    pub async fn model_complete_with_timeout(
        &self,
        request: &openwebide_core::ChatRequest,
        timeout_seconds: u32,
    ) -> Result<openwebide_core::ChatCompletion, String> {
        self.post(
            "/models/background",
            &openwebide_core::BackgroundCompletion {
                request: request.clone(),
                timeout_seconds,
            },
        )
        .await
    }
    pub async fn model_tokens(
        &self,
        request: &openwebide_core::ChatRequest,
    ) -> Result<Option<usize>, String> {
        self.post("/models/tokens", request).await
    }

    pub async fn git_status(&self, project_id: Option<i64>) -> Result<GitRepoStatus, String> {
        self.get(&Self::git_endpoint(project_id, "status")).await
    }

    pub async fn git_diff(
        &self,
        project_id: Option<i64>,
        path: Option<&str>,
    ) -> Result<String, String> {
        let ep = Self::git_endpoint(project_id, "diff");
        let query = match path {
            Some(p) => format!("{ep}?path={}", urlenc(p)),
            None => ep,
        };
        let res: openwebide_core::GitDiff = self.get(&query).await?;
        Ok(res.diff)
    }

    pub async fn git_file_head(
        &self,
        project_id: Option<i64>,
        path: &str,
    ) -> Result<String, String> {
        let ep = Self::git_endpoint(project_id, "show");
        let query = format!("{ep}?path={}", urlenc(path));
        let res: openwebide_core::GitFileContent = self.get(&query).await.map_err(|error| {
            if error == "binary file at HEAD" {
                "binary file".into()
            } else {
                error
            }
        })?;
        res.into_text()
    }

    pub async fn git_stash(
        &self,
        project_id: Option<i64>,
        request: &openwebide_core::git::GitStashRequest,
    ) -> Result<openwebide_core::git::GitStashResult, String> {
        self.post(&Self::git_endpoint(project_id, "stash"), request)
            .await
    }
    pub async fn git_index_diff(&self, project_id: Option<i64>) -> Result<String, String> {
        let diff: openwebide_core::GitDiff = self
            .post(
                &Self::git_endpoint(project_id, "index-diff"),
                &serde_json::json!({}),
            )
            .await?;
        Ok(diff.diff)
    }
    pub async fn git_history(
        &self,
        project_id: Option<i64>,
        request: &openwebide_core::git::GitHistoryRequest,
    ) -> Result<openwebide_core::git::GitHistoryPage, String> {
        self.post(&Self::git_endpoint(project_id, "history"), request)
            .await
    }
    pub async fn git_commit_diff(
        &self,
        project_id: Option<i64>,
        request: &openwebide_core::git::GitCommitDiffRequest,
    ) -> Result<openwebide_core::git::GitCommitDiff, String> {
        self.post(&Self::git_endpoint(project_id, "commit-diff"), request)
            .await
    }

    pub async fn git_branches(
        &self,
        project_id: Option<i64>,
    ) -> Result<Vec<GitBranchInfo>, String> {
        self.get(&Self::git_endpoint(project_id, "branches")).await
    }

    pub async fn git_path_changes(
        &self,
        project_id: Option<i64>,
    ) -> Result<openwebide_core::git::GitPathChanges, String> {
        self.get(&Self::git_endpoint(project_id, "path-status"))
            .await
    }
    pub async fn git_path_action(
        &self,
        project_id: Option<i64>,
        request: &openwebide_core::git::GitPathRequest,
    ) -> Result<openwebide_core::git::GitPathChanges, String> {
        self.post(&Self::git_endpoint(project_id, "path"), request)
            .await
    }

    pub async fn git_commit(
        &self,
        project_id: Option<i64>,
        req: &GitCommitRequest,
    ) -> Result<GitCommitResult, String> {
        self.post(&Self::git_endpoint(project_id, "commit"), req)
            .await
    }

    pub async fn git_checkout(
        &self,
        project_id: Option<i64>,
        req: &GitCheckoutRequest,
    ) -> Result<GitCheckoutResult, String> {
        self.post(&Self::git_endpoint(project_id, "checkout"), req)
            .await
    }

    pub async fn git_sync(
        &self,
        project_id: Option<i64>,
        req: &GitSyncRequest,
    ) -> Result<GitSyncResult, String> {
        self.post(&Self::git_endpoint(project_id, "sync"), req)
            .await
    }

    pub async fn rename_session(&self, id: i64, name: &str) -> Result<ChatSession, String> {
        self.put(&format!("/sessions/{id}"), &json!({ "name": name }))
            .await
    }

    pub async fn delete_session(&self, id: i64) -> Result<(), String> {
        self.request::<(), _>(Method::DELETE, &format!("/sessions/{id}"), None, false)
            .await
    }

    /// Ask the backend to stop the session's in-flight run. The run ends at
    /// its next step boundary; the stream then emits `Cancelled`.
    pub async fn cancel_session(&self, session_id: i64) -> Result<(), String> {
        let _value: serde_json::Value = self
            .request::<(), _>(
                Method::POST,
                &format!("/sessions/{session_id}/cancel"),
                None,
                false,
            )
            .await?;
        Ok(())
    }

    /// Record the user's decision on a gated tool call. The in-flight run
    /// picks it up on its next poll (about half a second later).
    pub async fn set_permission(
        &self,
        session_id: i64,
        tool_call_id: &str,
        approved: bool,
    ) -> Result<(), String> {
        let enc_id = js_sys::encode_uri_component(tool_call_id)
            .as_string()
            .unwrap_or_else(|| tool_call_id.to_string());
        let _value: serde_json::Value = self
            .post(
                &format!("/sessions/{session_id}/permissions/{enc_id}"),
                &json!({ "approved": approved }),
            )
            .await?;
        Ok(())
    }

    /// The session's conversation: chat messages interleaved with the agent's
    /// tool steps, in order.
    pub async fn list_messages(&self, session_id: i64) -> Result<Vec<ConversationEntry>, String> {
        self.get(&format!("/sessions/{session_id}/messages")).await
    }

    pub async fn get_goal(&self, session: i64) -> Result<Option<openwebide_core::Goal>, String> {
        self.get(&format!("/sessions/{session}/goal")).await
    }
    pub async fn update_goal(
        &self,
        session: i64,
        revision: u64,
        command: &openwebide_core::GoalCommand,
    ) -> Result<openwebide_core::Goal, String> {
        self.post(
            &format!("/sessions/{session}/goal"),
            &json!({"expected_revision":revision,"command":command}),
        )
        .await
    }
    pub async fn dispatch_goal(
        &self,
        session: i64,
        revision: u64,
        command: &openwebide_core::GoalCommand,
        binding: Option<&openwebide_core::scheduled::HostBinding>,
    ) -> Result<openwebide_core::Goal, String> {
        self.post(&format!("/sessions/{session}/goal"),&json!({"expected_revision":revision,"command":command,"worker":true,"binding":binding})).await
    }
    pub async fn compact_session(
        &self,
        session: i64,
        model: Option<&str>,
    ) -> Result<ChatMessage, String> {
        self.post(
            &format!("/sessions/{session}/compact"),
            &json!({"model":model}),
        )
        .await
    }
    pub async fn scheduled_tasks(
        &self,
        project: Option<i64>,
    ) -> Result<Vec<openwebide_core::scheduled::ScheduledTask>, String> {
        self.get(&format!(
            "/scheduled-tasks{}",
            project.map_or_else(String::new, |id| format!("?project_id={id}"))
        ))
        .await
    }
    pub async fn scheduled_monitors(
        &self,
        session: i64,
    ) -> Result<Vec<openwebide_core::scheduled::ScheduledTask>, String> {
        self.get(&format!("/sessions/{session}/monitors")).await
    }
    pub async fn bind_background_host(
        &self,
        project: i64,
        binding: &openwebide_core::scheduled::HostBinding,
    ) -> Result<(), String> {
        let _: serde_json::Value = self
            .post(&format!("/projects/{project}/execution-host"), binding)
            .await?;
        Ok(())
    }
    pub async fn scheduled_command(
        &self,
        project: Option<i64>,
        command: &openwebide_core::scheduled::TaskCommand,
        binding: Option<&openwebide_core::scheduled::HostBinding>,
    ) -> Result<Vec<openwebide_core::scheduled::ScheduledTask>, String> {
        self.post(
            "/scheduled-tasks",
            &json!({"project_id":project,"command":command,"binding":binding}),
        )
        .await
    }
    pub async fn scheduled_session_command(
        &self,
        session: i64,
        command: &openwebide_core::scheduled::TaskCommand,
    ) -> Result<Vec<openwebide_core::scheduled::ScheduledTask>, String> {
        self.post(&format!("/sessions/{session}/scheduled-tasks"), command)
            .await
    }
    pub async fn run_lease(&self, session: i64, token: &str, release: bool) -> Result<(), String> {
        let _: openwebide_core::scheduled::RunControl = self
            .post(
                &format!("/sessions/{session}/run-lease"),
                &json!({"token":token,"release":release}),
            )
            .await?;
        Ok(())
    }
    pub async fn memories(
        &self,
        id: i64,
        session: bool,
    ) -> Result<openwebide_core::ProjectMemories, String> {
        self.get(&format!(
            "/{}/{id}/memories",
            if session { "sessions" } else { "projects" }
        ))
        .await
    }
    pub async fn memory_command(
        &self,
        id: i64,
        command: &openwebide_core::MemoryCommand,
        session: bool,
    ) -> Result<openwebide_core::ProjectMemories, String> {
        self.post(
            &format!(
                "/{}/{id}/memories",
                if session { "sessions" } else { "projects" }
            ),
            command,
        )
        .await
    }
    pub async fn skills(
        &self,
        id: i64,
        session: bool,
    ) -> Result<openwebide_core::ProjectSkills, String> {
        self.get(&format!(
            "/{}/{id}/skills",
            if session { "sessions" } else { "projects" }
        ))
        .await
    }
    pub async fn plugin_marketplaces(
        &self,
    ) -> Result<openwebide_core::plugins::marketplace::MarketplaceSettings, String> {
        self.get("/plugin-marketplaces").await
    }
    pub async fn save_plugin_marketplaces(
        &self,
        request: &openwebide_core::plugins::marketplace::SaveMarketplaces,
    ) -> Result<openwebide_core::plugins::marketplace::MarketplaceSettings, String> {
        self.post("/plugin-marketplaces", request).await
    }
    pub async fn refresh_plugin_marketplaces(
        &self,
    ) -> Result<openwebide_core::plugins::marketplace::MarketplaceRefresh, String> {
        self.post("/plugin-marketplaces/refresh", &()).await
    }
    pub async fn project_plugins(
        &self,
        project: i64,
    ) -> Result<Vec<openwebide_core::plugins::ProjectPlugin>, String> {
        self.get(&format!("/projects/{project}/plugins")).await
    }
    pub async fn project_plugin_command(
        &self,
        project: i64,
        command: &openwebide_core::plugins::ProjectPluginCommand,
    ) -> Result<Vec<openwebide_core::plugins::ProjectPlugin>, String> {
        self.post(&format!("/projects/{project}/plugins"), command)
            .await
    }
    pub async fn remove_plugin(
        &self,
        request: &openwebide_core::plugins::RemovePlugin,
    ) -> Result<Vec<openwebide_core::plugins::PluginInstallation>, String> {
        self.post("/plugins/remove", request).await
    }
    pub async fn plugin_package(
        &self,
        project: Option<i64>,
        expected: &openwebide_core::plugins::PreparedPlugin,
    ) -> Result<openwebide_core::plugins::PluginPackage, String> {
        let path = project.map_or_else(
            || "/plugins/package".into(),
            |id| format!("/projects/{id}/plugins/package"),
        );
        self.post(&path, expected).await
    }
    pub async fn plugin_execution_grants(
        &self,
        session: i64,
        plugins: &[openwebide_core::plugins::PreparedPlugin],
    ) -> Result<std::collections::BTreeMap<String, String>, String> {
        self.post(&format!("/sessions/{session}/plugin-grants"), &plugins)
            .await
    }
    pub async fn plugin_context_grants(
        &self,
        request: &openwebide_core::plugins::execution::PluginGrantRequest,
    ) -> Result<std::collections::BTreeMap<String, String>, String> {
        self.post("/plugins/execution-grants", request).await
    }
    pub async fn plugin_context_host_request(
        &self,
        request: &openwebide_core::plugins::execution::PluginHostRequest,
    ) -> Result<String, String> {
        self.post("/plugins/host", request).await
    }
    pub async fn start_plugin_invocation(
        &self,
        request: &openwebide_core::plugins::execution::PluginStartRequest,
    ) -> Result<openwebide_core::plugins::execution::PluginInvocation, String> {
        self.post("/plugins/invoke", request).await
    }
    pub async fn continue_plugin_invocation(
        &self,
        request: &openwebide_core::plugins::execution::ContinuePlugin,
    ) -> Result<openwebide_core::plugins::execution::PluginInvocation, String> {
        self.post("/plugins/continue", request).await
    }
    pub async fn cancel_plugin_invocation(&self, id: &str) -> Result<(), String> {
        let _: serde_json::Value = self
            .post("/plugins/cancel", &serde_json::json!({"id":id}))
            .await?;
        Ok(())
    }
    pub async fn plugin_host_request(
        &self,
        session: i64,
        request: &openwebide_core::plugins::execution::PluginHostRequest,
    ) -> Result<String, String> {
        self.post(&format!("/sessions/{session}/plugin-host"), request)
            .await
    }
    pub async fn plugin_installations(
        &self,
    ) -> Result<Vec<openwebide_core::plugins::PluginInstallation>, String> {
        self.get("/plugins").await
    }
    pub async fn prepare_plugin(
        &self,
        project: Option<i64>,
        source: &openwebide_core::plugins::PluginSource,
    ) -> Result<openwebide_core::plugins::PreparedPlugin, String> {
        let path = project.map_or_else(
            || "/plugins/prepare".into(),
            |id| format!("/projects/{id}/plugins/prepare"),
        );
        self.post(&path, source).await
    }
    pub async fn plugin_preparation(
        &self,
        project: Option<i64>,
        command: &openwebide_core::plugins::preparation::PreparationCommand,
    ) -> Result<openwebide_core::plugins::preparation::PluginPreparation, String> {
        command.validate().map_err(|error| error.to_string())?;
        let path = project.map_or_else(
            || "/plugins/preparation".into(),
            |id| format!("/projects/{id}/plugins/preparation"),
        );
        self.request(Method::POST, &path, Some(command), true).await
    }
    pub async fn record_plugin(
        &self,
        request: &openwebide_core::plugins::RecordPlugin,
    ) -> Result<Vec<openwebide_core::plugins::PluginInstallation>, String> {
        self.post("/plugins", request).await
    }
    pub async fn skill_command(
        &self,
        id: i64,
        command: &openwebide_core::SkillCommand,
        session: bool,
    ) -> Result<openwebide_core::ProjectSkills, String> {
        self.post(
            &format!(
                "/{}/{id}/skills",
                if session { "sessions" } else { "projects" }
            ),
            command,
        )
        .await
    }
    pub async fn question_command(
        &self,
        session: i64,
        command: &openwebide_core::questions::QuestionCommand,
    ) -> Result<openwebide_core::questions::QuestionResult, String> {
        self.post(&format!("/sessions/{session}/questions"), command)
            .await
    }

    pub async fn get_todo_plan(
        &self,
        session: i64,
    ) -> Result<Option<openwebide_core::TodoUpdate>, String> {
        self.get(&format!("/sessions/{session}/todos")).await
    }
    pub async fn write_todo_plan(
        &self,
        session: i64,
        anchor: i64,
        plan: &openwebide_core::TodoPlan,
    ) -> Result<openwebide_core::TodoUpdate, String> {
        self.post(
            &format!("/sessions/{session}/todos"),
            &json!({"anchor_message_id":anchor,"plan":plan}),
        )
        .await
    }
    pub async fn fork_session(
        &self,
        session: i64,
        message: i64,
    ) -> Result<openwebide_core::ForkedSession, String> {
        self.post(
            &format!("/sessions/{session}/fork"),
            &json!({"message_id":message}),
        )
        .await
    }
    pub async fn list_queued_prompts(
        &self,
        session: i64,
    ) -> Result<Vec<openwebide_core::QueuedPrompt>, String> {
        self.get(&format!("/sessions/{session}/queue")).await
    }
    pub async fn enqueue_prompt(
        &self,
        session: i64,
        content: &str,
        guidance: bool,
    ) -> Result<openwebide_core::QueuedPrompt, String> {
        self.post(
            &format!("/sessions/{session}/queue"),
            &json!({"content":content,"guidance":guidance}),
        )
        .await
    }
    pub async fn update_queued_prompt(
        &self,
        session: i64,
        key: openwebide_core::QueuedPromptKey,
        content: &str,
    ) -> Result<openwebide_core::QueuedPrompt, String> {
        self.put(
            &format!("/sessions/{session}/queue"),
            &json!({"key":key,"content":content}),
        )
        .await
    }
    pub async fn remove_queued_prompt(
        &self,
        session: i64,
        key: openwebide_core::QueuedPromptKey,
    ) -> Result<(), String> {
        self.request(
            Method::DELETE,
            &format!("/sessions/{session}/queue"),
            Some(&json!({"key":key})),
            false,
        )
        .await
    }
    pub async fn consume_queued_prompt(
        &self,
        session: i64,
        key: openwebide_core::QueuedPromptKey,
        content: &str,
    ) -> Result<ChatMessage, String> {
        self.post(
            &format!("/sessions/{session}/queue/send"),
            &json!({"key":key,"content":content}),
        )
        .await
    }

    pub async fn list_run_changes(
        &self,
        project: i64,
    ) -> Result<Vec<openwebide_core::RunChange>, String> {
        self.get(&format!("/projects/{project}/run-changes")).await
    }
    pub async fn preview_run_review(
        &self,
        project: i64,
        request: &openwebide_core::ReviewRequest,
    ) -> Result<openwebide_core::ReviewPlan, String> {
        self.post(&format!("/projects/{project}/reviews/preview"), request)
            .await
    }
    pub async fn prepare_run_review(
        &self,
        project: i64,
        request: &openwebide_core::ReviewRequest,
    ) -> Result<openwebide_core::ReviewPlan, String> {
        self.post(&format!("/projects/{project}/reviews/prepare"), request)
            .await
    }
    pub async fn complete_run_review(
        &self,
        project: i64,
        request: &openwebide_core::ReviewRequest,
    ) -> Result<openwebide_core::RunChange, String> {
        self.post(&format!("/projects/{project}/reviews/complete"), request)
            .await
    }
    pub async fn prepare_rewind(
        &self,
        session: i64,
        message: i64,
    ) -> Result<openwebide_core::RewindPlan, String> {
        self.post(
            &format!("/sessions/{session}/rewind"),
            &json!({ "message_id": message }),
        )
        .await
    }
    pub async fn complete_rewind(
        &self,
        session: i64,
        message: i64,
    ) -> Result<Vec<ConversationEntry>, String> {
        self.post(
            &format!("/sessions/{session}/rewind/complete"),
            &json!({ "message_id": message }),
        )
        .await
    }

    /// Complete a tool-capable chat request via the backend provider.
    pub async fn approval_check(
        &self,
        session: i64,
        check: &openwebide_core::ApprovalCheck,
    ) -> Result<openwebide_core::ApprovalDecision, String> {
        self.post(&format!("/sessions/{session}/approval-check"), check)
            .await
    }

    pub async fn chat_tools(&self, request: &ChatRequest) -> Result<ChatCompletion, String> {
        self.post("/chat-tools", request).await
    }

    /// Persist a user or assistant message to the session.
    pub async fn persist_message(
        &self,
        session_id: i64,
        role: Role,
        content: &str,
        usage: Option<&TurnTelemetry>,
        tool_calls: Option<&[openwebide_core::ToolCall]>,
    ) -> Result<ChatMessage, String> {
        self.post(
            &format!("/sessions/{session_id}/messages/persist"),
            &json!({ "role": role, "content": content, "usage": usage, "tool_calls": tool_calls }),
        )
        .await
    }

    /// The model's context window, in tokens, as resolved by the backend
    /// (the connection's configured value, else provider discovery).
    /// `Ok(None)` when neither source reports one.
    pub async fn model_context(
        &self,
        connection_id: i64,
        model: Option<&str>,
    ) -> Result<Option<usize>, String> {
        let mut path = format!("/models/context?connection_id={connection_id}");
        if let Some(m) = model {
            path.push_str(&format!("&model={}", urlenc(m)));
        }
        let value: serde_json::Value = self.get(&path).await?;
        Ok(value
            .get("context_limit")
            .and_then(serde_json::Value::as_u64)
            .map(|n| usize::try_from(n).unwrap_or(usize::MAX)))
    }

    /// Record (or refresh) an agent tool step.
    #[allow(clippy::too_many_arguments)]
    pub async fn save_project_checkpoint(
        &self,
        session: i64,
        id: &str,
        checkpoint: &openwebide_core::rewind::ProjectCheckpoint,
    ) -> Result<(), String> {
        let _: serde_json::Value = self.post(&format!("/sessions/{session}/tool-steps/upsert"), &json!({"anchor_message_id":0,"tool_call_id":id,"name":"","summary":"","checkpoint":checkpoint})).await?;
        Ok(())
    }
    pub async fn save_task(
        &self,
        session: i64,
        anchor: i64,
        snapshot: &openwebide_core::TaskSnapshot,
    ) -> Result<(), String> {
        let _: serde_json::Value = self
            .post(
                &format!("/sessions/{session}/tasks"),
                &json!({"anchor_message_id":anchor,"snapshot":snapshot}),
            )
            .await?;
        Ok(())
    }
    pub async fn save_tool_timing(
        &self,
        session: i64,
        id: &str,
        timing: &openwebide_core::ToolTiming,
    ) -> Result<(), String> {
        let _: serde_json::Value = self
            .post(
                &format!("/sessions/{session}/tool-steps/timing"),
                &json!({"tool_call_id":id,"timing":timing}),
            )
            .await?;
        Ok(())
    }
    pub async fn upsert_tool_step(
        &self,
        session_id: i64,
        anchor_message_id: i64,
        tool_call_id: &str,
        name: &str,
        summary: &str,
        diff: Option<&FileDiff>,
    ) -> Result<(), String> {
        let _value: serde_json::Value = self
            .post(
                &format!("/sessions/{session_id}/tool-steps/upsert"),
                &json!({
                    "anchor_message_id": anchor_message_id,
                    "tool_call_id": tool_call_id,
                    "name": name,
                    "summary": summary,
                    "diff": diff,
                }),
            )
            .await?;
        Ok(())
    }

    /// Record the final outcome of an agent tool step.
    pub async fn complete_tool_step(
        &self,
        session_id: i64,
        tool_call_id: &str,
        ok: bool,
        result_summary: &str,
        diff: Option<&FileDiff>,
    ) -> Result<(), String> {
        let _value: serde_json::Value = self
            .post(
                &format!("/sessions/{session_id}/tool-steps/complete"),
                &json!({
                    "tool_call_id": tool_call_id,
                    "ok": ok,
                    "result_summary": result_summary,
                    "diff": diff,
                }),
            )
            .await?;
        Ok(())
    }

    pub async fn list_pending_edits(&self, project_id: i64) -> Result<Vec<PersistedEdit>, String> {
        self.get(&format!("/projects/{project_id}/pending-edits"))
            .await
    }

    pub async fn resolve_pending_edit(
        &self,
        project_id: i64,
        request: &ResolveEditRequest,
    ) -> Result<PersistedEdit, String> {
        self.post(
            &format!("/projects/{project_id}/pending-edits/resolve"),
            request,
        )
        .await
    }

    /// Search the web for documentation, API references, or solutions.
    pub async fn web_search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<WebSearchResult>, String> {
        self.request::<(), _>(
            Method::GET,
            &format!("/web/search?query={}&limit={}", urlenc(query), limit),
            None,
            true,
        )
        .await
    }

    /// Fetch a web page and return sanitized Markdown.
    pub async fn fetch_web_page(&self, target_url: &str) -> Result<String, String> {
        let resp: serde_json::Value = self
            .request::<(), _>(
                Method::GET,
                &format!("/web/fetch?url={}", urlenc(target_url)),
                None,
                true,
            )
            .await?;
        resp.get("content")
            .and_then(|v| v.as_str())
            .map(ToString::to_string)
            .ok_or_else(|| "missing content in web fetch response".to_string())
    }

    /// Send a user message and stream the assistant reply back via SSE.
    ///
    /// `on_event` is invoked for every event as it arrives. Returns `Err` on
    /// transport failure (including abort) once the stream ends.
    #[allow(
        clippy::too_many_arguments,
        reason = "Streaming transport carries prompt delivery metadata and callbacks"
    )]
    pub async fn send_message(
        &self,
        session_id: i64,
        content: &str,
        model: Option<&str>,
        editor_context: Option<&EditorContext>,
        browser_preferences: Option<&openwebide_core::BrowserPreferences>,
        queued_prompt: Option<openwebide_core::QueuedPromptKey>,
        signal: Option<&AbortSignal>,
        mut on_event: impl FnMut(RunEvent),
    ) -> Result<(), String> {
        let url = format!("{}/sessions/{session_id}/messages", self.base());
        let builder = self.builder(&url, Method::POST);
        let req = builder
            .abort_signal(signal)
            .json(&json!({
                "content": content,
                "model": model,
                "editor_context": editor_context,
                "browser_preferences": browser_preferences,
                "queued_prompt": queued_prompt,
            }))
            .map_err(|e| e.to_string())?;

        let is_signed_in = self.signed_in.get_untracked();
        let resp = req.send().await.map_err(|e| e.to_string())?;
        if !resp.ok() {
            if resp.status() == 401 && is_signed_in {
                self.signed_in.set(false);
                self.session_expired.set(true);
            }
            return Err(self.error_from(resp).await);
        }
        let stream = resp
            .body()
            .ok_or_else(|| "response has no body".to_string())?;
        let reader: ReadableStreamDefaultReader = stream
            .get_reader()
            .dyn_into()
            .map_err(|e| format!("{e:?}"))?;
        let mut decoder = openwebide_core::utf8::Utf8Decoder::new();
        let mut frames = crate::sse::FrameBuffer::new();

        loop {
            let read = reader.read();
            // `reader.read()` resolves to a plain `{done, value}` object — a
            // dictionary type with no JS constructor — so `dyn_into` (an
            // `instanceof` check) would reject it. `unchecked_into` just
            // reinterprets the value, which is safe here because the shape is
            // guaranteed by the stream API.
            let chunk: ReadableStreamReadResult = JsFuture::from(read)
                .await
                .map_err(|e| format!("{e:?}"))?
                .unchecked_into();
            if chunk.get_done().unwrap_or(false) {
                break;
            }
            let value: js_sys::Uint8Array =
                chunk.get_value().dyn_into().map_err(|e| format!("{e:?}"))?;
            let bytes = value.to_vec();
            for f in frames.push(&decoder.push(&bytes)) {
                if let Some(event) = crate::sse::parse_frame(&f) {
                    on_event(event);
                }
            }
        }
        for f in frames.push(&decoder.finish()) {
            if let Some(event) = crate::sse::parse_frame(&f) {
                on_event(event);
            }
        }
        Ok(())
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        self.request::<(), T>(Method::GET, path, None, false).await
    }

    async fn post<T: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<R, String> {
        self.request(Method::POST, path, Some(body), false).await
    }

    async fn put<T: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<R, String> {
        self.request(Method::PUT, path, Some(body), false).await
    }

    fn builder(&self, url: &str, method: Method) -> RequestBuilder {
        let mut builder = RequestBuilder::new(url)
            .method(method)
            .header("x-openwebide", "1");
        if self.cross_origin {
            builder = builder.credentials(web_sys::RequestCredentials::Include);
        }
        builder
    }

    async fn request<T: Serialize, R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&T>,
        abort_on_drop: bool,
    ) -> Result<R, String> {
        self.request_typed(method, path, body, abort_on_drop)
            .await
            .map_err(|error| error.message)
    }

    async fn request_typed<T, R>(
        &self,
        method: Method,
        path: &str,
        body: Option<&T>,
        abort_on_drop: bool,
    ) -> Result<R, RequestError>
    where
        T: Serialize,
        R: DeserializeOwned,
    {
        let url = format!("{}{path}", self.base());
        let is_delete = method == Method::DELETE;
        let guard = if abort_on_drop {
            Some(CommandFetchGuard(web_sys::AbortController::new().map_err(
                |e| RequestError::transport(format!("request cancellation error: {e:?}")),
            )?))
        } else {
            None
        };
        let signal = guard.as_ref().map(|guard| guard.0.signal());
        let builder = self.builder(&url, method).abort_signal(signal.as_ref());
        let req = match body {
            Some(body) => builder.json(body),
            None => builder.build(),
        }
        .map_err(|e| RequestError::transport(e.to_string()))?;

        let is_signed_in = self.signed_in.get_untracked();
        let session_revision = self.session_revision.get_untracked();
        let resp = req
            .send()
            .await
            .map_err(|e| RequestError::transport(e.to_string()))?;
        if !resp.ok() {
            if resp.status() == 401
                && is_signed_in
                && self.session_revision.get_untracked() == session_revision
                && !path.starts_with("/auth/")
            {
                self.signed_in.set(false);
                self.session_expired.set(true);
            }
            let status = resp.status();
            return Err(RequestError {
                status: Some(status),
                message: self.error_from(resp).await,
            });
        }
        if is_delete {
            // Delete responses need no decoded body.
            return serde_json::from_value(serde_json::Value::Null)
                .map_err(|e| RequestError::transport(e.to_string()));
        }
        resp.json()
            .await
            .map_err(|e| RequestError::transport(e.to_string()))
    }

    async fn error_from(&self, resp: gloo_net::http::Response) -> String {
        let text = resp.text().await.unwrap_or_default();
        serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("error")?.as_str().map(String::from))
            .unwrap_or_else(|| {
                if text.is_empty() {
                    format!("HTTP {}", resp.status())
                } else {
                    text
                }
            })
    }
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.trim_start_matches('?').split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

pub use crate::text::urlenc;

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen(
        inline_js = "let original; let signal; export function captureFetch() { original = window.fetch; window.fetch = request => { signal = request.signal; return new Promise(() => {}); }; } export function capturedSignal() { return signal; } export function restoreFetch() { window.fetch = original; }"
    )]
    extern "C" {
        #[wasm_bindgen(js_name = captureFetch)]
        fn capture_fetch();
        #[wasm_bindgen(js_name = capturedSignal)]
        fn captured_signal() -> AbortSignal;
        #[wasm_bindgen(js_name = restoreFetch)]
        fn restore_fetch();
    }
    #[wasm_bindgen_test]
    async fn dropping_web_tool_requests_aborts_fetch() {
        let owner = leptos::prelude::Owner::new();
        let api = owner.with(BackendApi::from_location);
        capture_fetch();
        let mut fetch = Box::pin(api.fetch_web_page("https://example.com"));
        assert!(futures::poll!(&mut fetch).is_pending());
        let signal = captured_signal();
        assert!(!signal.aborted());
        drop(fetch);
        assert!(signal.aborted());
        let mut search = Box::pin(api.web_search("example", 1));
        assert!(futures::poll!(&mut search).is_pending());
        let signal = captured_signal();
        assert!(!signal.aborted());
        drop(search);
        assert!(signal.aborted());
        restore_fetch();
        owner.cleanup();
    }
}

#[cfg(test)]
mod streaming_tests {
    use super::*;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_test::*;

    #[wasm_bindgen(inline_js = r#"
        export function streamFetch(wire) {
            const original = window.fetch;
            window.fetch = async () => {
                const bytes = new TextEncoder().encode(wire);
                let offset = 0;
                return new Response(new ReadableStream({
                    pull(controller) {
                        if (offset < bytes.length) {
                            controller.enqueue(bytes.slice(offset, ++offset));
                        } else {
                            controller.close();
                        }
                    }
                }), {headers: {'Content-Type': 'text/event-stream'}});
            };
            return () => { window.fetch = original; };
        }
    "#)]
    extern "C" {
        #[wasm_bindgen(js_name = streamFetch)]
        fn stream_fetch(wire: &str) -> js_sys::Function;
    }

    #[wasm_bindgen_test]
    async fn fallback_reader_matches_websocket_events() {
        let owner = leptos::prelude::Owner::new();
        let api = owner.with(BackendApi::from_location);
        for delimiter in ["\n", "\r\n", "\r"] {
            let wire = [
                ": heartbeat",
                "",
                "event: ignored",
                "data:{\"kind\":\"delta\",",
                "data: \"content\":\"café 🦀\"}",
                "",
                "data: invalid",
                "",
                "data:{\"kind\":\"cancelled\"}",
                "",
                "data:{\"kind\":\"error\",\"message\":\"unfinished\"}",
                "",
            ]
            .join(delimiter);
            let restore = stream_fetch(&wire);
            let mut events = Vec::new();
            let result = api
                .send_message(1, "hello", None, None, None, None, None, |event| {
                    events.push(event);
                })
                .await;
            restore.call0(&JsValue::UNDEFINED).unwrap();
            result.unwrap();
            let expected = [
                RunEvent::Delta {
                    content: "café 🦀".into(),
                },
                RunEvent::Cancelled,
            ];
            let ws_events: Vec<_> = expected
                .iter()
                .enumerate()
                .map(|(index, event)| {
                    let message = openwebide_core::BridgeServerMessage::RunEvent {
                        run_id: "run".into(),
                        seq: index as u64 + 1,
                        event: event.clone(),
                    };
                    let decoded: openwebide_core::BridgeServerMessage =
                        serde_json::from_str(&serde_json::to_string(&message).unwrap()).unwrap();
                    let openwebide_core::BridgeServerMessage::RunEvent { event, .. } = decoded
                    else {
                        unreachable!()
                    };
                    event
                })
                .collect();
            assert_eq!(events, ws_events);
        }
        owner.cleanup();
    }
}

#[cfg(test)]
mod recovery_transport_tests {
    use super::*;
    use crate::backend::RecoveryError;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_test::*;

    #[wasm_bindgen(inline_js = r#"
        export function recoveryFetch(status, body) {
            const original = window.fetch;
            const requests = [];
            window.fetch = async request => {
                requests.push({method: request.method, url: request.url, header: request.headers.get('x-openwebide')});
                return new Response(body, {status, headers: {'Content-Type': 'application/json'}});
            };
            return {requests, restore: () => {window.fetch = original;}};
        }
        export function recoveryPending() {
            const original = window.fetch;
            let resolve;
            window.fetch = () => new Promise(done => {resolve = done;});
            return {restore: () => {window.fetch = original;}, finish: () => resolve(new Response('{"error":"expired"}', {status:401}))};
        }
        export function recoveryPendingFinish(state) {state.finish();}
        export function recoveryFetchRestore(state) {state.restore();}
        export function recoveryRequestHeader(state) {return state.requests[0].header;}
    "#)]
    extern "C" {
        #[wasm_bindgen(js_name = recoveryFetch)]
        fn recovery_fetch(status: u16, body: &str) -> JsValue;
        #[wasm_bindgen(js_name = recoveryFetchRestore)]
        fn restore(state: &JsValue);
        #[wasm_bindgen(js_name = recoveryPending)]
        fn pending() -> JsValue;
        #[wasm_bindgen(js_name = recoveryPendingFinish)]
        fn finish(state: &JsValue);
        #[wasm_bindgen(js_name = recoveryRequestHeader)]
        fn header(state: &JsValue) -> String;
    }

    #[wasm_bindgen_test]
    async fn recovery_uses_http_status_authentication_and_shared_request_policy() {
        let owner = leptos::prelude::Owner::new();
        let api = owner.with(BackendApi::from_location);
        let record = openwebide_core::editor::EditorRecoveryRecord::default();
        let state = recovery_fetch(409, r#"{"error":"different window"}"#);
        assert!(
            matches!(api.save_editor_recovery(1, &record).await, Err(RecoveryError::Conflict(message)) if message == "different window")
        );
        assert_eq!(header(&state), "1");
        restore(&state);
        let state = recovery_fetch(500, r#"{"error":"409 conflict is just text"}"#);
        assert!(matches!(
            api.editor_recovery(1).await,
            Err(RecoveryError::Unavailable(_))
        ));
        restore(&state);
        api.signed_in.set(true);
        let state = recovery_fetch(401, r#"{"error":"expired"}"#);
        assert!(matches!(
            api.editor_recovery(1).await,
            Err(RecoveryError::Unavailable(_))
        ));
        assert!(api.session_expired.get_untracked());
        assert!(!api.signed_in.get_untracked());
        restore(&state);
        let state = recovery_fetch(200, r#"{"revision":2}"#);
        let advanced = openwebide_core::editor::EditorRecoveryRecord {
            revision: 1,
            state: record.state.clone(),
        };
        assert_eq!(api.save_editor_recovery(1, &advanced).await.unwrap(), 2);
        restore(&state);
        let state = recovery_fetch(200, r#"{"revision":0}"#);
        assert!(matches!(
            api.save_editor_recovery(1, &record).await,
            Err(RecoveryError::Unavailable(_))
        ));
        restore(&state);
        let mut invalid = record.clone();
        invalid.state.format = 99;
        let state = recovery_fetch(200, &serde_json::to_string(&invalid).unwrap());
        assert!(matches!(
            api.editor_recovery(1).await,
            Err(RecoveryError::Unavailable(_))
        ));
        restore(&state);
        api.signed_in.set(true);
        api.session_expired.set(false);
        let state = pending();
        let mut request = Box::pin(api.editor_recovery(1));
        assert!(futures::poll!(&mut request).is_pending());
        api.session_revision.update(|revision| *revision += 1);
        finish(&state);
        assert!(matches!(request.await, Err(RecoveryError::Unavailable(_))));
        assert!(!api.session_expired.get_untracked());
        assert!(api.signed_in.get_untracked());
        restore(&state);
        owner.cleanup();
    }
}
