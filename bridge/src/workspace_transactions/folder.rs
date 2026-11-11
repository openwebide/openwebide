//! Native root selection only. The common worker owns transaction policy.
use super::*;

pub(super) enum ExecutionFolder {
    Remote(PathBuf),
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    Paired(Box<companion::ProvenFolder>),
}
impl ExecutionFolder {
    pub(super) fn mode(&self) -> WorkspaceMode {
        match self {
            Self::Remote(_) => WorkspaceMode::Remote,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::Paired(_) => WorkspaceMode::Local,
        }
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn owner(&self, user: i64) -> String {
        match self {
            Self::Remote(_) => format!("user:{user}"),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::Paired(folder) => folder.owner.clone(),
        }
    }
    pub(super) fn worker(&self) -> WorkspaceTransactionWorker {
        match self {
            Self::Remote(_) => WorkspaceTransactionWorker {
                id: format!("{:032x}", rand::random::<u128>()),
                secret: format!("{:032x}", rand::random::<u128>()),
            },
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::Paired(folder) => folder.worker.clone(),
        }
    }
    pub(super) fn capture(
        &self,
        scope: &WorkspaceTransactionScope,
        user: i64,
        project: i64,
    ) -> Result<CapturedRoot, String> {
        match self {
            Self::Remote(workspace) => Host::capture_root(
                workspace,
                scope
                    .root
                    .path
                    .as_deref()
                    .ok_or("The original remote folder is unavailable")?,
                user,
                project,
            ),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::Paired(folder) => Ok(folder.root.clone()),
        }
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) async fn exclude(
        &self,
        companion: &companion::Host,
        root: &CapturedRoot,
    ) -> Result<companion::FolderFence, String> {
        match self {
            Self::Remote(_) => companion.exclude(root).await,
            Self::Paired(folder) => companion.exclude_proven(folder).await,
        }
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn matches_state(&self, state: &WorkspaceTransactionState) -> Result<bool, String> {
        match self {
            Self::Remote(_) => Ok(true),
            Self::Paired(folder) => Ok(state.journal.plan.version == 2
                && state.journal.id == folder.scope.transaction
                && state.journal.plan.fingerprint()? == folder.scope.fingerprint),
        }
    }
}
