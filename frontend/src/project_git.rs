//! Shared Git workflows select their transport through the project execution facade.
use crate::{
    backend::Api,
    local_agent::BrowserBridgeClient,
    project_host::{ProjectExecution, ProjectHost},
    state::{auth::AuthState, projects::ProjectsState, settings::SettingsState},
};
use leptos::prelude::*;
use openwebide_agent::BridgeClient;
use openwebide_core::{
    GitBranchInfo, GitCheckoutRequest, GitCheckoutResult, GitCommitRequest, GitCommitResult,
    GitRepoStatus, GitSyncRequest, GitSyncResult,
};

#[derive(Clone, Copy)]
pub struct ProjectGit {
    host: ProjectHost,
    projects: ProjectsState,
    api: Api,
    settings: SettingsState,
    auth: AuthState,
}
impl ProjectGit {
    pub fn new(
        api: Api,
        projects: ProjectsState,
        settings: SettingsState,
        auth: AuthState,
    ) -> Self {
        let host = ProjectHost::new(api, projects, settings, auth);
        provide_context(host);
        Self {
            host,
            projects,
            api,
            settings,
            auth,
        }
    }
    pub fn revision(self) -> Option<u64> {
        self.host.revision()
    }
    pub async fn repository(self, project: Option<i64>) -> Result<GitRepository, String> {
        let selected = self.projects.active_project.get_untracked();
        let project_id = project.or(selected);
        let mutations = project_id
            .map(|id| {
                crate::workspace_mutations::WorkspaceMutations::for_project(
                    self.api,
                    self.projects,
                    self.auth,
                    Some(self.settings),
                    id,
                    move || self.projects.active_project.try_get_untracked() == Some(selected),
                )
            })
            .transpose()?;
        let adapter = self.repository_adapter(project_id).await?;
        Ok(GitRepository {
            adapter: std::rc::Rc::new(adapter),
            mutations,
        })
    }
    async fn repository_adapter(self, project_id: Option<i64>) -> Result<Repository, String> {
        let is_local = project_id
            .and_then(|id| self.projects.project(id))
            .is_some_and(|p| p.mode == openwebide_core::WorkspaceMode::Local);

        if is_local {
            match self.host.resolve(project_id).await {
                Ok(ProjectExecution::Local(client)) => return Ok(Repository::Local(client)),
                Ok(ProjectExecution::Remote { .. }) => {}
                Err(err) => {
                    if let Some(handle) = project_id.and_then(|id| {
                        self.projects
                            .local_handles
                            .with_untracked(|handles| handles.get(&id).cloned())
                    }) {
                        let vfs = crate::local_fs::BrowserFsaVfs::new(handle);
                        let local_git = openwebide_core::git::LocalGit::new(vfs);
                        if local_git.is_repo().await {
                            return Ok(Repository::Browser(local_git));
                        }
                    }
                    return Err(err);
                }
            }
        }

        match self.host.resolve(project_id).await? {
            ProjectExecution::Remote {
                api, project_id, ..
            } => Ok(Repository::Remote { api, project_id }),
            ProjectExecution::Local(client) => Ok(Repository::Local(client)),
        }
    }
}

/// All writes retain the project/account/root captured before host resolution.
/// Read-only calls remain available during recovery.
pub struct GitRepository {
    adapter: std::rc::Rc<Repository>,
    mutations: Option<crate::workspace_mutations::WorkspaceMutations>,
}
impl GitRepository {
    pub fn with_current(mut self, current: impl Fn() -> bool + 'static) -> Self {
        self.mutations = self.mutations.map(|scope| scope.with_current(current));
        self
    }
    pub fn with_preflight(mut self, check: impl Fn() -> Result<(), String> + 'static) -> Self {
        self.mutations = self.mutations.map(|scope| scope.with_preflight(check));
        self
    }
    fn mutations(&self) -> Result<&crate::workspace_mutations::WorkspaceMutations, String> {
        self.mutations
            .as_ref()
            .ok_or_else(|| "Select a project before changing Git files".into())
    }
    pub async fn stash(
        &self,
        request: &openwebide_core::git::GitStashRequest,
    ) -> Result<openwebide_core::git::GitStashResult, String> {
        if matches!(request.action, openwebide_core::git::GitStashAction::List) {
            return self.adapter.stash(request).await;
        }
        let (adapter, request) = (self.adapter.clone(), request.clone());
        self.mutations()?
            .run(async move { adapter.stash(&request).await })
            .await
    }
    pub async fn path_action(
        &self,
        request: &openwebide_core::git::GitPathRequest,
    ) -> Result<openwebide_core::git::GitPathChanges, String> {
        let (adapter, request) = (self.adapter.clone(), request.clone());
        self.mutations()?
            .run(async move { adapter.path_action(&request).await })
            .await
    }
    pub async fn commit(&self, request: &GitCommitRequest) -> Result<GitCommitResult, String> {
        let (adapter, request) = (self.adapter.clone(), request.clone());
        self.mutations()?
            .run(async move { adapter.commit(&request).await })
            .await
    }
    pub async fn checkout(
        &self,
        request: &GitCheckoutRequest,
    ) -> Result<GitCheckoutResult, String> {
        let (adapter, request) = (self.adapter.clone(), request.clone());
        self.mutations()?
            .run(async move { adapter.checkout(&request).await })
            .await
    }
    pub async fn sync(&self, request: &GitSyncRequest) -> Result<GitSyncResult, String> {
        let (adapter, request) = (self.adapter.clone(), request.clone());
        self.mutations()?
            .run(async move { adapter.sync(&request).await })
            .await
    }
    pub async fn index_diff(&self) -> Result<String, String> {
        self.adapter.index_diff().await
    }
    pub async fn history(
        &self,
        request: &openwebide_core::git::GitHistoryRequest,
    ) -> Result<openwebide_core::git::GitHistoryPage, String> {
        self.adapter.history(request).await
    }
    pub async fn commit_diff(
        &self,
        request: &openwebide_core::git::GitCommitDiffRequest,
    ) -> Result<openwebide_core::git::GitCommitDiff, String> {
        self.adapter.commit_diff(request).await
    }
    pub async fn status(&self) -> Result<GitRepoStatus, String> {
        self.adapter.status().await
    }
    pub async fn diff(&self, path: Option<&str>) -> Result<String, String> {
        self.adapter.diff(path).await
    }
    pub async fn file_head(&self, path: &str) -> Result<String, String> {
        self.adapter.file_head(path).await
    }
    pub async fn branches(&self) -> Result<Vec<GitBranchInfo>, String> {
        self.adapter.branches().await
    }
    pub async fn path_changes(&self) -> Result<openwebide_core::git::GitPathChanges, String> {
        self.adapter.path_changes().await
    }
}

enum Repository {
    Remote { api: Api, project_id: Option<i64> },
    Local(BrowserBridgeClient),
    Browser(openwebide_core::git::LocalGit<crate::local_fs::BrowserFsaVfs>),
}

impl Repository {
    pub async fn stash(
        &self,
        request: &openwebide_core::git::GitStashRequest,
    ) -> Result<openwebide_core::git::GitStashResult, String> {
        match self {
            Self::Remote { api, project_id } => {
                api.with_value(Clone::clone)
                    .git_stash(*project_id, request)
                    .await
            }
            Self::Local(client) => {
                client
                    .git_request(
                        "stash",
                        serde_json::to_value(request).map_err(|error| error.to_string())?,
                    )
                    .await
            }
            Self::Browser(local) => local.stash(request).await,
        }
    }
    pub async fn index_diff(&self) -> Result<String, String> {
        match self {
            Self::Remote { api, project_id } => {
                api.with_value(Clone::clone)
                    .git_index_diff(*project_id)
                    .await
            }
            Self::Local(client) => {
                let diff: openwebide_core::GitDiff = client
                    .git_request("index-diff", serde_json::json!({}))
                    .await?;
                Ok(diff.diff)
            }
            Self::Browser(local) => local.index_diff().await,
        }
    }

    pub async fn history(
        &self,
        request: &openwebide_core::git::GitHistoryRequest,
    ) -> Result<openwebide_core::git::GitHistoryPage, String> {
        match self {
            Self::Remote { api, project_id } => {
                api.with_value(Clone::clone)
                    .git_history(*project_id, request)
                    .await
            }
            Self::Local(client) => {
                client
                    .git_request(
                        "history",
                        serde_json::to_value(request).map_err(|error| error.to_string())?,
                    )
                    .await
            }
            Self::Browser(local) => local.history(request).await,
        }
    }
    pub async fn commit_diff(
        &self,
        request: &openwebide_core::git::GitCommitDiffRequest,
    ) -> Result<openwebide_core::git::GitCommitDiff, String> {
        match self {
            Self::Remote { api, project_id } => {
                api.with_value(Clone::clone)
                    .git_commit_diff(*project_id, request)
                    .await
            }
            Self::Local(client) => {
                client
                    .git_request(
                        "commit-diff",
                        serde_json::to_value(request).map_err(|error| error.to_string())?,
                    )
                    .await
            }
            Self::Browser(local) => local.commit_diff(request).await,
        }
    }

    pub async fn status(&self) -> Result<GitRepoStatus, String> {
        match self {
            Self::Remote { api, project_id } => {
                api.with_value(Clone::clone).git_status(*project_id).await
            }
            Self::Local(client) => client.git_status().await,
            Self::Browser(local) => local.status().await,
        }
    }

    pub async fn diff(&self, path: Option<&str>) -> Result<String, String> {
        match self {
            Self::Remote { api, project_id } => {
                api.with_value(Clone::clone)
                    .git_diff(*project_id, path)
                    .await
            }
            Self::Local(client) => client.git_diff(path).await,
            Self::Browser(local) => local.diff(path).await,
        }
    }

    pub async fn file_head(&self, path: &str) -> Result<String, String> {
        match self {
            Self::Remote { api, project_id } => {
                api.with_value(Clone::clone)
                    .git_file_head(*project_id, path)
                    .await
            }
            Self::Local(client) => {
                let value: openwebide_core::GitFileContent = client
                    .git_request("show", serde_json::json!({ "path": path }))
                    .await?;
                value.into_text()
            }
            Self::Browser(local) => local.file_head(path).await,
        }
    }

    pub async fn branches(&self) -> Result<Vec<GitBranchInfo>, String> {
        match self {
            Self::Remote { api, project_id } => {
                api.with_value(Clone::clone).git_branches(*project_id).await
            }
            Self::Local(client) => client.git_request("branches", serde_json::json!({})).await,
            Self::Browser(local) => local.branches().await,
        }
    }

    pub async fn path_changes(&self) -> Result<openwebide_core::git::GitPathChanges, String> {
        match self {
            Self::Remote { api, project_id } => {
                api.with_value(Clone::clone)
                    .git_path_changes(*project_id)
                    .await
            }
            Self::Local(client) => {
                client
                    .git_request("path-status", serde_json::json!({}))
                    .await
            }
            Self::Browser(local) => local.path_changes().await,
        }
    }
    pub async fn path_action(
        &self,
        request: &openwebide_core::git::GitPathRequest,
    ) -> Result<openwebide_core::git::GitPathChanges, String> {
        match self {
            Self::Remote { api, project_id } => {
                api.with_value(Clone::clone)
                    .git_path_action(*project_id, request)
                    .await
            }
            Self::Local(client) => {
                client
                    .git_request(
                        "path",
                        serde_json::to_value(request).map_err(|error| error.to_string())?,
                    )
                    .await
            }
            Self::Browser(local) => local.path_action(request).await,
        }
    }

    pub async fn commit(&self, request: &GitCommitRequest) -> Result<GitCommitResult, String> {
        match self {
            Self::Remote { api, project_id } => {
                api.with_value(Clone::clone)
                    .git_commit(*project_id, request)
                    .await
            }
            Self::Local(client) => client.git_commit(request).await,
            Self::Browser(local) => local.commit(request).await,
        }
    }

    pub async fn checkout(
        &self,
        request: &GitCheckoutRequest,
    ) -> Result<GitCheckoutResult, String> {
        match self {
            Self::Remote { api, project_id } => {
                api.with_value(Clone::clone)
                    .git_checkout(*project_id, request)
                    .await
            }
            Self::Local(client) => client.git_checkout(request).await,
            Self::Browser(local) => local.checkout(request).await,
        }
    }

    pub async fn sync(&self, request: &GitSyncRequest) -> Result<GitSyncResult, String> {
        match self {
            Self::Remote { api, project_id } => {
                api.with_value(Clone::clone)
                    .git_sync(*project_id, request)
                    .await
            }
            Self::Local(client) => {
                client
                    .git_request(
                        "sync",
                        serde_json::to_value(request).map_err(|error| error.to_string())?,
                    )
                    .await
            }
            Self::Browser(local) => local.sync(request).await,
        }
    }
}
