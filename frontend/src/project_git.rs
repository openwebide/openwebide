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
        Self { host, projects }
    }
    pub fn revision(self) -> Option<u64> {
        self.host.revision()
    }
    pub async fn repository(self, project: Option<i64>) -> Result<GitRepository, String> {
        let project_id = project.or_else(|| self.projects.active_project.get_untracked());
        let is_local = project_id
            .and_then(|id| self.projects.project(id))
            .is_some_and(|p| p.mode == openwebide_core::WorkspaceMode::Local);

        if is_local {
            if let Ok(ProjectExecution::Local(client)) = self.host.resolve(project_id).await {
                return Ok(GitRepository::Local(client));
            }
            if let Some(handle) = project_id.and_then(|id| {
                self.projects
                    .local_handles
                    .with_untracked(|handles| handles.get(&id).cloned())
            }) {
                let vfs = crate::local_fs::BrowserFsaVfs::new(handle);
                let local_git = openwebide_core::git::LocalGit::new(vfs);
                return Ok(GitRepository::Browser(local_git));
            }
        }

        match self.host.resolve(project).await? {
            ProjectExecution::Remote {
                api, project_id, ..
            } => Ok(GitRepository::Remote { api, project_id }),
            ProjectExecution::Local(client) => Ok(GitRepository::Local(client)),
        }
    }
}

pub enum GitRepository {
    Remote { api: Api, project_id: Option<i64> },
    Local(BrowserBridgeClient),
    Browser(openwebide_core::git::LocalGit<crate::local_fs::BrowserFsaVfs>),
}

impl GitRepository {
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
