//! Ordinary workspace writes share one critical section and fresh recovery
//! policy. Filesystem/Git adapters remain transport primitives.
use crate::{
    backend::{Api, Backend},
    state::{auth::AuthState, projects::ProjectsState, settings::SettingsState},
    state_actions::workspace::EditorRoot,
    workspace_transactions::{with_workspace_lock, workspace_lock_name},
};
use leptos::prelude::*;
use std::{future::Future, rc::Rc};

/// A captured project/account/root scope, not a cached permission to write.
#[derive(Clone)]
pub struct WorkspaceMutations {
    api: Api,
    project: i64,
    current: Rc<dyn Fn() -> bool>,
    preflight: Rc<dyn Fn() -> Result<(), String>>,
}
impl WorkspaceMutations {
    pub fn new(api: Api, project: i64, current: Rc<dyn Fn() -> bool>) -> Result<Self, String> {
        if project <= 0 || !current() {
            return Err("The workspace mutation scope changed".into());
        }
        Ok(Self {
            api,
            project,
            current,
            preflight: Rc::new(|| Ok(())),
        })
    }
    pub fn for_project(
        api: Api,
        projects: ProjectsState,
        auth: AuthState,
        settings: Option<SettingsState>,
        project: i64,
        current: impl Fn() -> bool + 'static,
    ) -> Result<Self, String> {
        let original = projects
            .project(project)
            .ok_or("The project is unavailable")?;
        let root = EditorRoot::for_project(&original, projects, settings);
        let account = auth.generation.get_untracked();
        Self::new(
            api,
            project,
            Rc::new(move || {
                current()
                    && auth.generation.try_get_untracked() == Some(account)
                    && projects.project(project).is_some_and(|project| {
                        !root.changed(&EditorRoot::for_project(&project, projects, settings))
                    })
            }),
        )
    }
    pub fn with_current(mut self, current: impl Fn() -> bool + 'static) -> Self {
        let original = self.current;
        self.current = Rc::new(move || original() && current());
        self
    }
    pub fn with_preflight(mut self, check: impl Fn() -> Result<(), String> + 'static) -> Self {
        let previous = self.preflight;
        self.preflight = Rc::new(move || {
            previous()?;
            check()
        });
        self
    }
    /// Own the complete operation independently of its observer. Cancelling an
    /// awaiting UI task cannot release the lock while its accepted writes run.
    pub async fn run<T: 'static>(
        &self,
        work: impl Future<Output = Result<T, String>> + 'static,
    ) -> Result<T, String> {
        if !(self.current)() {
            return Err("The workspace mutation scope changed".into());
        }
        let (api, project, current, preflight) = (
            self.api,
            self.project,
            self.current.clone(),
            self.preflight.clone(),
        );
        with_workspace_lock(workspace_lock_name(project), async move {
            if !current() {
                return Err("The workspace changed while waiting to write".into());
            }
            let backend = api
                .try_with_value(Clone::clone)
                .ok_or("The workspace closed")?;
            ensure_idle(backend, project, current.clone()).await?;
            if !current() {
                return Err("The workspace changed before writing".into());
            }
            preflight()?;
            work.await
        })
        .await
    }
}

/// Observe both durable fences in one fresh database snapshot. A failed or late observation is never an
/// idle result; callers must already hold the owning filesystem critical section.
pub(crate) async fn ensure_idle(
    backend: Rc<dyn Backend>,
    project: i64,
    current: Rc<dyn Fn() -> bool>,
) -> Result<(), String> {
    if !current() {
        return Err("The workspace changed before recovery inspection".into());
    }
    let result = crate::util::with_deadline(
        async {
            let activity = backend
                .workspace_activity(project)
                .await
                .map_err(|e| e.to_string())?;
            activity.ensure_idle().map_err(str::to_owned)
        },
        30_000,
    )
    .await;
    if !current() {
        return Err("The workspace changed during recovery inspection".into());
    }
    result.ok_or_else(|| {
        "Workspace recovery inspection timed out; refresh before changing files".to_string()
    })?
}

#[cfg(test)]
mod tests;
