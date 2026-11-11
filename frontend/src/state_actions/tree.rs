//! Refresh visible directory listings through the same workspace API in both modes.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    time::Duration,
};

use futures::future::{AbortHandle, Either, abortable, select};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    state::{auth::AuthState, projects::ProjectsState, workspace::WorkspaceState},
    workspace::Workspace,
};

pub(super) fn watch_tree(
    projects: ProjectsState,
    workspace: WorkspaceState,
    auth: AuthState,
    workspace_for: Callback<i64, Option<Workspace>>,
    refresh_git: Callback<()>,
) {
    let epoch = Rc::new(Cell::new(0u64));
    let timer = Rc::new(RefCell::new(None::<IntervalHandle>));
    let request = Rc::new(RefCell::new(None::<AbortHandle>));
    let cleanup_epoch = epoch.clone();
    let cleanup_timer = timer.clone();
    let cleanup_request = request.clone();
    let cleanup = send_wrapper::SendWrapper::new((cleanup_epoch, cleanup_timer, cleanup_request));
    on_cleanup(move || {
        let (cleanup_epoch, cleanup_timer, cleanup_request) = cleanup.take();
        cleanup_epoch.set(cleanup_epoch.get() + 1);
        if let Some(timer) = cleanup_timer.borrow_mut().take() {
            timer.clear();
        }
        if let Some(request) = cleanup_request.borrow_mut().take() {
            request.abort();
        }
    });
    Effect::new(move |_| {
        let project = projects.active_project.get();
        let generation = auth.generation.get();
        // Replacing or restoring a local folder invalidates an in-flight listing too.
        projects.local_handles.track();
        epoch.set(epoch.get() + 1);
        if let Some(timer) = timer.borrow_mut().take() {
            timer.clear();
        }
        if let Some(request) = request.borrow_mut().take() {
            request.abort();
        }
        let Some(project_id) = project else {
            return;
        };
        let token = epoch.get();
        let epoch = epoch.clone();
        let request = request.clone();
        *timer.borrow_mut() = set_interval_with_handle(
            move || {
                if web_sys::window()
                    .is_none_or(|window| window.document().is_none_or(|document| document.hidden()))
                    || projects
                        .needs_grant
                        .with_untracked(|ids| ids.contains(&project_id))
                    || request.borrow().is_some()
                    || workspace.editor_operation_active_untracked(project_id)
                    || workspace.workspace_saves_blocked(project_id)
                {
                    return;
                }
                refresh_git.run(());
                let Some(ws) = workspace_for.run(project_id) else {
                    return;
                };
                let epoch = epoch.clone();
                let current = move || {
                    epoch.get() == token
                        && projects.active_project.try_get_untracked() == Some(Some(project_id))
                        && auth.generation.try_get_untracked() == Some(generation)
                };
                let (future, handle) = abortable(refresh_tree(
                    ws,
                    projects,
                    workspace,
                    project_id,
                    current.clone(),
                ));
                *request.borrow_mut() = Some(handle);
                let request = request.clone();
                spawn_local(async move {
                    let _ = future.await;
                    if current() {
                        request.borrow_mut().take();
                    }
                });
            },
            Duration::from_secs(2),
        )
        .ok();
    });
}

pub(super) async fn refresh_tree(
    ws: Workspace,
    projects: ProjectsState,
    workspace: WorkspaceState,
    project_id: i64,
    current: impl Fn() -> bool,
) -> Result<(), String> {
    let mut failure = None;
    let mut dirs = VecDeque::from([String::new()]);
    while let Some(dir) = dirs.pop_front() {
        if !current()
            || workspace.editor_operation_active_untracked(project_id)
            || workspace.workspace_saves_blocked(project_id)
            || web_sys::window()
                .is_none_or(|window| window.document().is_none_or(|document| document.hidden()))
        {
            return Ok(());
        }
        if !dir.is_empty()
            && !workspace
                .expanded
                .with_untracked(|expanded| expanded.contains(&dir))
        {
            continue;
        }
        let before = workspace
            .entries
            .with_untracked(|listings| listings.get(&dir).cloned());
        let result = match select(
            Box::pin(ws.list(&dir)),
            Box::pin(crate::util::sleep_ms(5_000)),
        )
        .await
        {
            Either::Left((result, _)) => result,
            Either::Right(_) => return Err("Refreshing files timed out".into()),
        };
        match result {
            Ok(mut entries) => {
                if !current()
                    || workspace.editor_operation_active_untracked(project_id)
                    || workspace.workspace_saves_blocked(project_id)
                {
                    return Ok(());
                }
                // Stable ordering avoids publishing changes caused only by enumeration order.
                openwebide_core::vfs::sort_file_entries(&mut entries);
                let mut old = before.clone();
                if let Some(old) = old.as_mut() {
                    openwebide_core::vfs::sort_file_entries(old);
                }
                // A foreground load that finished while we waited takes precedence.
                if workspace
                    .entries
                    .with_untracked(|listings| listings.get(&dir).cloned())
                    == before
                    && old.as_ref() != Some(&entries)
                {
                    workspace.entries.update(|listings| {
                        if let Some(previous) = &before {
                            for removed in previous.iter().filter(|entry| {
                                entry.is_dir
                                    && !entries
                                        .iter()
                                        .any(|new| new.is_dir && new.path == entry.path)
                            }) {
                                let prefix = format!("{}/", removed.path);
                                listings.retain(|path, _| {
                                    path != &removed.path && !path.starts_with(&prefix)
                                });
                            }
                        }
                        listings.insert(dir.clone(), entries);
                    });
                }
                workspace.entries.with_untracked(|listings| {
                    if let Some(entries) = listings.get(&dir) {
                        workspace.expanded.with_untracked(|expanded| {
                            dirs.extend(
                                entries
                                    .iter()
                                    .filter(|entry| entry.is_dir && expanded.contains(&entry.path))
                                    .map(|entry| entry.path.clone()),
                            );
                        });
                    }
                });
            }
            Err(error) => {
                if !current() {
                    return Ok(());
                }
                if error.needs_folder_access() {
                    projects.needs_grant.update(|ids| {
                        ids.insert(project_id);
                    });
                    return Err(error.to_string());
                }
                failure = Some(error.to_string());
                // Transient failures keep the last known listing and retry next tick.
            }
        }
    }
    failure.map_or(Ok(()), Err)
}
