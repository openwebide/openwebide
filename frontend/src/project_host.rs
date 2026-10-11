//! Project execution host facade shared by Git, terminals, model discovery and agent startup.

use std::{cell::Cell, collections::HashMap, rc::Rc};

use futures::{
    FutureExt,
    future::{LocalBoxFuture, Shared},
};
use leptos::prelude::*;
use openwebide_core::WorkspaceMode;

use crate::{
    backend::Api,
    bridge::{BridgeConfig, BridgeCredentials},
    local_agent::{BRIDGE_FOLDER_NOTICE, BrowserBridgeClient, resolve_bridge_cwd_guarded},
    state::{auth::AuthState, projects::ProjectsState, settings::SettingsState},
};

type LocalRepository = Shared<LocalBoxFuture<'static, Result<BrowserBridgeClient, String>>>;

#[derive(Clone, Copy)]
pub struct ProjectHost {
    api: Api,
    projects: ProjectsState,
    settings: SettingsState,
    auth: AuthState,
    local: StoredValue<HashMap<i64, LocalRepository>, LocalStorage>,
    epoch: StoredValue<Rc<Cell<u64>>, LocalStorage>,
}

impl ProjectHost {
    pub fn new(
        api: Api,
        projects: ProjectsState,
        settings: SettingsState,
        auth: AuthState,
    ) -> Self {
        let local = StoredValue::new_local(HashMap::new());
        let epoch = StoredValue::new_local(Rc::new(Cell::new(0u64)));
        let previous = StoredValue::new_local(None);
        Effect::new(move |_| {
            let identity = (
                projects.projects.with(|projects| {
                    projects
                        .iter()
                        .map(|project| (project.id, (project.mode, project.path.clone())))
                        .collect::<std::collections::BTreeMap<_, _>>()
                }),
                projects.local_handles.with(|handles| {
                    handles
                        .iter()
                        .map(|(id, handle)| (*id, handle.clone()))
                        .collect::<std::collections::BTreeMap<_, _>>()
                }),
                settings.bridge_url.get(),
                auth.generation.get(),
            );
            if previous.with_value(|previous| previous.as_ref() != Some(&identity)) {
                local.update_value(HashMap::clear);
                epoch.with_value(|epoch| epoch.set(epoch.get().wrapping_add(1)));
            }
            previous.set_value(Some(identity));
        });
        Self {
            api,
            projects,
            settings,
            auth,
            local,
            epoch,
        }
    }

    /// Mode selection for unattended work stays at the host capability boundary.
    pub async fn background_binding(
        self,
        project: Option<i64>,
        current: impl Fn() -> bool + Clone + 'static,
    ) -> Result<Option<openwebide_core::scheduled::HostBinding>, String> {
        match project.and_then(|id| self.projects.project(id)) {
            Some(project) if project.mode == WorkspaceMode::Local => {
                self.scheduled_binding(project.id, current).await.map(Some)
            }
            Some(_) => Ok(None),
            None if project.is_none() => Ok(None),
            None => Err("Project is no longer available".into()),
        }
    }
    pub async fn scheduled_binding(
        self,
        id: i64,
        current: impl Fn() -> bool + Clone + 'static,
    ) -> Result<openwebide_core::scheduled::HostBinding, String> {
        let host = self
            .resolve_guarded(Some(id), true, current.clone())
            .await?;
        let path = host
            .cwd()
            .ok_or("Connect the project folder to its execution host first.")?;
        let config = BridgeConfig::new(&self.settings.bridge_url.get_untracked());
        let credential = BridgeCredentials::new(self.api).credential().await?;
        let response = gloo_net::http::Request::get(&format!("{}/scheduler/host", config.http_url))
            .header("Authorization", &format!("Bearer {credential}"))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        if !response.ok() {
            return Err("Update and configure the paired execution host before scheduling.".into());
        }
        let host: openwebide_core::scheduled::ExecutionHost =
            response.json().await.map_err(|error| error.to_string())?;
        if !current() {
            return Err("Project access changed".into());
        }
        Ok(openwebide_core::scheduled::HostBinding::new(host.id, path))
    }
    pub async fn discover_servers(self) -> Result<Vec<openwebide_core::ServerDiscovery>, String> {
        let local = self
            .projects
            .active_project
            .get_untracked()
            .and_then(|id| self.projects.project(id))
            .is_some_and(|project| project.mode == WorkspaceMode::Local);
        if !local {
            return self.api.with_value(Clone::clone).discover_servers().await;
        }
        let config = BridgeConfig::new(&self.settings.bridge_url.get_untracked());
        let token = BridgeCredentials::new(self.api).credential().await?;
        let guard = crate::api::CommandFetchGuard(
            web_sys::AbortController::new().map_err(|error| format!("{error:?}"))?,
        );
        let response =
            gloo_net::http::Request::post(&format!("{}/models/discover", config.http_url))
                .abort_signal(Some(&guard.0.signal()))
                .header("Content-Type", "application/json")
                .header("Authorization", &format!("Bearer {token}"))
                .body("{}")
                .map_err(|error| error.to_string())?
                .send()
                .await
                .map_err(|error| format!("Model discovery bridge unavailable: {error}"))?;
        if !response.ok() {
            return Err(format!(
                "Model discovery bridge returned HTTP {}",
                response.status()
            ));
        }
        response.json().await.map_err(|error| error.to_string())
    }

    pub async fn startup_context(
        self,
        id: i64,
        connection: Option<i64>,
        model: Option<&str>,
    ) -> Result<String, String> {
        let project = self
            .projects
            .project(id)
            .ok_or("Project is no longer available")?;
        let runtime = match connection {
            Some(connection) => Some(
                self.api
                    .with_value(Clone::clone)
                    .model_runtime(connection, model)
                    .await?,
            ),
            None => None,
        };
        let tools_enabled = runtime.as_ref().is_none_or(|runtime| {
            runtime.settings.tools != Some(false)
                && runtime.connection.tool_selection != openwebide_core::ToolSelection::ChatOnly
        });
        // Adapter selection; both hosts use RunContext and the same tool policy.
        if project.mode == WorkspaceMode::Remote {
            return self
                .api
                .with_value(Clone::clone)
                .startup_context(id, tools_enabled, connection)
                .await;
        }
        let handle = self
            .projects
            .local_handles
            .with_untracked(|handles| handles.get(&id).cloned())
            .ok_or("Reconnect this folder using Grant folder access in the file tree.")?;
        let host = self.resolve_guarded(Some(id), true, || true).await.ok();
        let cwd = host.as_ref().and_then(ProjectExecution::cwd);
        let environment = openwebide_core::RunEnvironment {
            browser_preferences: crate::browser_preferences::capture(),
            project_name: Some(project.name),
            project_root: Some(
                cwd.clone()
                    .unwrap_or_else(|| format!("Browser-selected folder: {}", handle.name())),
            ),
            mode: Some(project.mode),
            timestamp: openwebide_core::now_seconds(js_sys::Date::now()),
        };
        let mut tools = if tools_enabled {
            crate::local_agent::local_tools(cwd.as_deref())
        } else {
            Vec::new()
        };
        if let Some(runtime) = runtime {
            runtime.connection.tool_selection.apply(&mut tools);
        }
        let mut context = openwebide_agent::context::RunContext::new(environment);
        let vfs = crate::local_fs::BrowserFsaVfs::new(handle);
        let bridge = host.and_then(|host| match host {
            ProjectExecution::Local(bridge) => Some(bridge),
            ProjectExecution::Remote { .. } => None,
        });
        Ok(context.startup(&vfs, &bridge, &tools).await)
    }

    pub fn revision(self) -> Option<u64> {
        self.epoch.try_with_value(|epoch| epoch.get())
    }

    /// Hosts with no browser verification can resolve synchronously.
    pub fn resolve_immediate(
        self,
        project_id: Option<i64>,
    ) -> Option<Result<ProjectExecution, String>> {
        let Some(id) = project_id else {
            return Some(Ok(ProjectExecution::Remote {
                api: self.api,
                project_id,
                cwd: None,
            }));
        };
        let Some(project) = self.projects.project(id) else {
            return Some(Err("Project is no longer available".into()));
        };
        if project.mode == WorkspaceMode::Remote {
            return Some(Ok(ProjectExecution::Remote {
                api: self.api,
                project_id,
                cwd: openwebide_core::run::execution_root(&project),
            }));
        }
        None
    }

    /// Plugin processes run on the host independently of browser folder visibility.
    pub fn plugin_host(
        self,
        project_id: Option<i64>,
        current: impl Fn() -> bool,
    ) -> Result<PluginExecutionHost, String> {
        if !current() {
            return Err("Plugin action context changed".into());
        }
        if let Some(id) = project_id {
            let project = self
                .projects
                .project(id)
                .ok_or("Project is no longer available")?;
            if project.mode == WorkspaceMode::Local {
                let config = BridgeConfig::new(&self.settings.bridge_url.get_untracked());
                return Ok(PluginExecutionHost::Local(
                    crate::plugin_bridge::PluginBridgeClient::new(
                        config.http_url,
                        BridgeCredentials::new(self.api),
                    ),
                ));
            }
        }
        Ok(PluginExecutionHost::Remote {
            api: self.api,
            project_id,
        })
    }

    pub async fn resolve(self, project_id: Option<i64>) -> Result<ProjectExecution, String> {
        self.resolve_guarded(project_id, false, || true).await
    }
    pub async fn resolve_guarded(
        self,
        project_id: Option<i64>,
        fresh: bool,
        guard: impl Fn() -> bool + Clone + 'static,
    ) -> Result<ProjectExecution, String> {
        if !guard() {
            return Err("Project access changed".into());
        }
        if fresh && let Some(id) = project_id {
            self.local.update_value(|cache| {
                cache.remove(&id);
            });
        }

        if let Some(host) = self.resolve_immediate(project_id) {
            return host;
        }
        let id = project_id.ok_or("Project is no longer available")?;
        let epoch = self.epoch.with_value(Clone::clone);
        let token = epoch.get();
        let generation = self.auth.generation.get_untracked();
        let url = self.settings.bridge_url.get_untracked();
        let current = move || {
            guard()
                && epoch.get() == token
                && self.auth.generation.try_get_untracked() == Some(generation)
                && self.settings.bridge_url.try_get_untracked().as_ref() == Some(&url)
                && self
                    .projects
                    .projects
                    .try_with_untracked(|projects| projects.iter().any(|p| p.id == id))
                    == Some(true)
        };
        let cached = self.local.with_value(|cache| cache.get(&id).cloned());
        let pending = match cached {
            Some(cached) => cached,
            None => {
                let handle = self
                    .projects
                    .local_handles
                    .with_untracked(|handles| handles.get(&id).cloned())
                    .ok_or("Reconnect this folder using Grant folder access in the file tree.")?;
                let config = BridgeConfig::new(&self.settings.bridge_url.get_untracked());
                let credentials = BridgeCredentials::new(self.api);
                let resolving = current.clone();
                let pending = async move {
                    let cwd = resolve_bridge_cwd_guarded(
                        self.api,
                        handle,
                        id,
                        &config,
                        &credentials,
                        resolving,
                    )
                    .await
                    .ok_or_else(|| BRIDGE_FOLDER_NOTICE.to_string())?;
                    Ok(BrowserBridgeClient::for_project(
                        config.http_url,
                        cwd,
                        credentials,
                    ))
                }
                .boxed_local()
                .shared();
                self.local.update_value(|cache| {
                    cache.insert(id, pending.clone());
                });
                pending
            }
        };
        let result = pending.await;
        if !current() {
            return Err("Project or bridge changed while resolving Git access".into());
        }
        match result {
            Ok(client) if client.cwd_verified() => Ok(ProjectExecution::Local(client)),
            Ok(_) | Err(_) => {
                self.local.update_value(|cache| {
                    cache.remove(&id);
                });
                Err(BRIDGE_FOLDER_NOTICE.into())
            }
        }
    }
}

pub enum ProjectExecution {
    Remote {
        api: Api,
        project_id: Option<i64>,
        cwd: Option<String>,
    },
    Local(BrowserBridgeClient),
}
impl ProjectExecution {
    pub fn local_bridge(&self) -> Option<BrowserBridgeClient> {
        match self {
            Self::Local(client) => Some(client.clone()),
            Self::Remote { .. } => None,
        }
    }
    pub fn cwd(&self) -> Option<String> {
        match self {
            Self::Remote { cwd, .. } => cwd.clone(),
            Self::Local(client) => Some(client.cwd().to_owned()),
        }
    }
}

/// Native plugin transport selection; workspace access is a separate capability.
pub enum PluginExecutionHost {
    Remote { api: Api, project_id: Option<i64> },
    Local(crate::plugin_bridge::PluginBridgeClient),
}
impl PluginExecutionHost {
    pub fn local_bridge(self) -> Option<crate::plugin_bridge::PluginBridgeClient> {
        match self {
            Self::Local(client) => Some(client),
            Self::Remote { .. } => None,
        }
    }
}
