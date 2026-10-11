use crate::state::{
    auth::AuthState,
    chat::ChatState,
    layout::LayoutState,
    projects::ProjectsState,
    settings::{SettingsState, Theme},
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use openwebide_core::{WorkspaceMode, tui::EditorContext};
use wasm_bindgen::JsCast;

use crate::{api::HealthState, backend::Api};

pub struct ProjectEffectContext {
    pub api: Api,
    pub health: RwSignal<Option<HealthState>>,
    pub auth: AuthState,
    pub settings: SettingsState,
    pub projects: ProjectsState,
    pub chat: ChatState,
    pub layout: LayoutState,
    pub select_project: Callback<i64>,
}

/// Keep browser tabs and installed-app windows tied to the active workspace.
pub fn install_window_title(
    auth: AuthState,
    projects: ProjectsState,
    workspace: crate::state::workspace::WorkspaceState,
    chat: ChatState,
) {
    Effect::new(move |_| {
        let mut parts = Vec::new();
        if auth.user.get().is_some() {
            let project_id = projects.active_project.get();
            let project = projects.projects.with(|items| {
                items
                    .iter()
                    .find(|project| Some(project.id) == project_id)
                    .map(|project| project.name.clone())
            });
            if let Some(project) = project {
                if let Some(path) = workspace.open_file.get()
                    && let Some(name) = path.rsplit('/').find(|name| !name.is_empty())
                {
                    parts.push(name.to_string());
                }
                parts.push(project);
            }
            let session_id = workspace.active_session.get();
            if let Some(name) = chat.sessions.with(|items| {
                items
                    .iter()
                    .find(|session| {
                        Some(session.id) == session_id && session.project_id == project_id
                    })
                    .map(|session| session.name.clone())
            }) && !name.trim().is_empty()
            {
                parts.push(name);
            }
        }
        parts.push("Open WebIDE".to_string());
        document().set_title(&parts.join(" — "));
    });
    on_cleanup(|| document().set_title("Open WebIDE"));
}

pub fn install_keyboard_shortcuts(chat: ChatState) {
    let commands = expect_context::<super::commands::CommandActions>();
    let ui = expect_context::<crate::state::ui::UiState>();
    let select_approval_mode = super::approvals::mode_selector(chat);
    let listener =
        window_event_listener(leptos::ev::keydown, move |event: web_sys::KeyboardEvent| {
            if event.is_composing()
                || event.default_prevented()
                || crate::components::modal::modal_is_open()
            {
                return;
            }
            let modifier = event.ctrl_key() || event.meta_key();
            let key = event.key().to_lowercase();
            if ui.palette_open.get_untracked() && !(modifier && event.shift_key() && key == "p") {
                return;
            }
            let command = if modifier && event.shift_key() && key == "p" && !event.alt_key() {
                Some(crate::commands::Command::Palette)
            } else if modifier && !event.alt_key() && event.code() == "Slash" {
                Some(crate::commands::Command::Shortcuts)
            } else if modifier && !event.shift_key() && !event.alt_key() {
                match key.as_str() {
                    "`" => Some(crate::commands::Command::ToggleTerminal),
                    "l" => Some(crate::commands::Command::CaptureEditor),
                    "k" => Some(crate::commands::Command::CycleFocus),
                    _ => None,
                }
            } else {
                None
            };
            if let Some(command) = command {
                if command
                    .unavailable(commands.context.get_untracked())
                    .is_none()
                {
                    event.prevent_default();
                    commands.run.run(command);
                }
                return;
            }
            if key == "tab"
                && event.shift_key()
                && !modifier
                && !event.alt_key()
                && event
                    .target()
                    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                    .is_some_and(|element| element.class_list().contains("composer-input"))
            {
                event.prevent_default();
                select_approval_mode.run(super::approvals::current_mode(chat).next());
            }
        });
    on_cleanup(move || listener.remove());
    // Scope changes close transient command dialogs rather than applying an
    // action to another workspace, session or account.
    Effect::new(move |_| {
        let _ = commands.scope.get();
        ui.about_open.set(false);
        ui.palette_open.set(false);
        ui.shortcuts_open.set(false);
    });
}

pub(super) fn cycle_focus() {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let targets: Vec<_> = [
        ".composer-input",
        ".editor-textarea",
        ".file-tree",
        ".terminal-input, .terminal-pane",
    ]
    .iter()
    .filter_map(|selector| document.query_selector(selector).ok().flatten())
    .filter_map(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    .filter(|element| element.offset_width() + element.offset_height() > 0)
    .collect();
    if targets.is_empty() {
        return;
    }
    let active = document.active_element();
    let next = active
        .and_then(|active| {
            targets.iter().position(|target| {
                target.is_same_node(Some(&active)) || target.contains(Some(&active))
            })
        })
        .map_or(0, |index| (index + 1) % targets.len());
    let _ = targets[next].focus();
}

pub(super) fn capture_active_editor(
    workspace: crate::state::workspace::WorkspaceState,
    open_file: Option<String>,
    content: &str,
) -> Option<EditorContext> {
    let file_path = open_file?;
    let window = web_sys::window()?;
    let document = window.document()?;
    let textarea = document.query_selector(".editor-textarea").ok()??;
    let textarea = textarea.dyn_into::<web_sys::HtmlTextAreaElement>().ok()?;
    let selection_start = textarea.selection_start().ok().flatten().unwrap_or(0) as usize;
    let selection_end = textarea.selection_end().ok().flatten().unwrap_or(0) as usize;
    let actions = super::editor::EditorActions::new(workspace);
    if !crate::viewport::current_editor_target(actions, &textarea)
        || !actions.native_input_current(
            textarea
                .get_attribute("data-editor-native-bound")
                .as_deref()
                == Some("true"),
            textarea
                .get_attribute("data-editor-native-generation")
                .as_deref(),
        )
    {
        return None;
    }
    let native = openwebide_core::editor::Selection {
        anchor: selection_start,
        head: selection_end,
    };
    let selection = actions
        .input_source_selection(native)
        .or_else(|| actions.projection()?.source_native_selection(native).ok())?;
    let start = selection.range().start;
    let end = selection.range().end;
    Some(crate::text::editor_context(
        file_path,
        content,
        content[..start].encode_utf16().count(),
        content[..end].encode_utf16().count(),
    ))
}

pub fn install_project_effects(context: ProjectEffectContext) {
    let ProjectEffectContext {
        api,
        health,
        auth,
        settings,
        projects,
        chat,
        layout,
        select_project,
    } = context;
    let ui = expect_context::<crate::state::ui::UiState>();
    crate::viewport::install();
    let history_loaded = RwSignal::new(None::<u64>);
    let history_pending = RwSignal::new(None::<String>);
    let history_saving = RwSignal::new(false);
    let history_imported = RwSignal::new(false);
    let session_pending = StoredValue::new(std::collections::HashMap::<Option<i64>, i64>::new());
    let session_saving = RwSignal::new(false);
    let resize_listener = window_event_listener(leptos::ev::resize, move |_| {
        layout.fit(viewport_width());
    });
    on_cleanup(move || resize_listener.remove());
    Effect::new(move |_| {
        let generation = auth.generation.get();
        ui.whats_new_open.set(false);
        history_loaded.set(None);
        history_pending.set(None);
        history_saving.set(false);
        history_imported.set(false);
        session_pending.set_value(Default::default());
        session_saving.set(false);
        chat.last_sessions.set(Default::default());
        chat.last_chat_session.set(None);
        let Some(user) = auth.user.get() else {
            return;
        };
        spawn_local(async move {
            let health_result = api.with_value(Clone::clone).health().await;
            if auth.generation.try_get_untracked() != Some(generation) {
                return;
            }
            let backend_ok = match health_result {
                Ok(health_state) => {
                    health.set(Some(HealthState::Online {
                        version: health_state.version,
                    }));
                    true
                }
                Err(_) => {
                    health.set(Some(HealthState::Offline));
                    false
                }
            };
            if !backend_ok || auth.generation.try_get_untracked() != Some(generation) {
                return;
            }

            // Snapshot before the request so later saves survive even if the wall clock moves back.
            let candidates = crate::idb::orphan_candidates(user.id).await;
            if auth.generation.try_get_untracked() != Some(generation) {
                return;
            }
            let backend = api.with_value(Clone::clone);
            let editor_revision = settings.editor_preference_revision.get_untracked();
            let (
                settings_result,
                projects_result,
                connections_result,
                sessions_result,
                prompts_result,
                model_setup_result,
            ) = futures::join!(
                backend.get_settings(),
                backend.list_projects(),
                backend.list_connections(),
                backend.list_sessions(),
                backend.list_system_prompts(),
                backend.model_setup(),
            );
            if auth.generation.try_get_untracked() != Some(generation) {
                return;
            }
            if let Ok(setup) = model_setup_result {
                settings.model_setup.set(setup);
            }
            let mut stored_tab_ids = Vec::<i64>::new();
            let mut stored_active_project = None;
            let mut chat_was_active = false;
            if let Ok(values) = settings_result {
                ui.whats_new_open.set(
                    values
                        .get("last_seen_release_version")
                        .is_none_or(|version| version != env!("CARGO_PKG_VERSION")),
                );
                chat.approval_mode.set(
                    values
                        .iter()
                        .filter_map(|(key, value)| {
                            Some((
                                key.strip_prefix("approval_mode_")?.parse().ok()?,
                                serde_json::from_str(value).ok()?,
                            ))
                        })
                        .collect(),
                );
                chat.draft_approval_mode.set(
                    values
                        .get("draft_approval_mode")
                        .and_then(|value| serde_json::from_str(value).ok())
                        .unwrap_or(openwebide_core::ApprovalMode::NEW_SESSION),
                );

                chat.last_chat_session.set(
                    values
                        .get("last_chat_session")
                        .and_then(|value| value.parse().ok()),
                );
                chat.last_sessions.set(
                    values
                        .iter()
                        .filter_map(|(key, value)| {
                            Some((
                                key.strip_prefix("last_session_")?.parse().ok()?,
                                value.parse().ok()?,
                            ))
                        })
                        .collect(),
                );
                if settings.editor_preference_revision.get_untracked() == editor_revision {
                    settings.editor_preferences.set(
                        values
                            .get("editor_preferences")
                            .and_then(|value| {
                                serde_json::from_str::<openwebide_core::editor::EditorPreferences>(
                                    value,
                                )
                                .ok()
                            })
                            .unwrap_or_default()
                            .normalized(),
                    );
                }
                settings.browser_notifications.set(
                    values
                        .get("browser_notifications")
                        .is_some_and(|value| value == "true"),
                );
                if let Some(theme) = values.get("theme") {
                    settings.theme.set(Theme::parse(theme));
                }
                if let Some(url) = values.get("bridge_url") {
                    settings.bridge_url.set(url.to_string());
                }
                if let Some(id) = values
                    .get("default_connection")
                    .and_then(|value| value.parse::<i64>().ok())
                {
                    settings.default_connection.set(Some(id));
                }
                if let Some(id) = values
                    .get("default_prompt")
                    .and_then(|value| value.parse::<i64>().ok())
                {
                    settings.default_prompt.set(Some(id));
                }
                if let Some(json) = values.get("open_tabs")
                    && let Ok(ids) = serde_json::from_str::<Vec<i64>>(json)
                {
                    stored_tab_ids = ids;
                }
                chat_was_active = values.get("active_project").is_some_and(String::is_empty);
                if let Some(id) = values
                    .get("active_project")
                    .and_then(|value| value.parse::<i64>().ok())
                {
                    stored_active_project = Some(id);
                }
                if layout.history_tree_revision.get_untracked() == 0
                    && let Some(width) = values
                        .get("panel_history_tree_width")
                        .and_then(|value| value.parse::<f64>().ok())
                        .filter(|width| width.is_finite())
                {
                    layout.history_tree_width.set(width.clamp(160.0, 650.0));
                }
                if layout.width_revision.get_untracked() == 0 {
                    let mut widths = [
                        layout.sidebar_width.get_untracked(),
                        layout.tree_width.get_untracked(),
                        layout.chat_width.get_untracked(),
                    ];
                    for (i, key) in [
                        "panel_sidebar_width",
                        "panel_tree_width",
                        "panel_chat_width",
                    ]
                    .iter()
                    .enumerate()
                    {
                        if let Some(width) = values
                            .get(*key)
                            .and_then(|value| value.parse::<f64>().ok())
                            .filter(|width| width.is_finite())
                        {
                            widths[i] = width;
                        }
                    }
                    if let Some(width) = values
                        .get("panel_terminal_height")
                        .and_then(|value| value.parse::<f64>().ok())
                        .filter(|width| width.is_finite())
                    {
                        layout.terminal_height.set(width.clamp(140.0, 700.0));
                    }
                    if let Some(width) = values
                        .get("panel_history_width")
                        .and_then(|value| value.parse::<f64>().ok())
                        .filter(|width| width.is_finite())
                    {
                        layout.history_width.set(width.clamp(260.0, 1200.0));
                    }
                    layout.sidebar_width.set(widths[0].clamp(140.0, 480.0));
                    layout.tree_width.set(widths[1].clamp(160.0, 650.0));
                    layout.chat_width.set(widths[2].clamp(260.0, 1000.0));
                }
                layout.restore_preferences(&values);
                layout.restore_panels(values.get(crate::state::layout::PANEL_VISIBILITY_KEY));
                layout.fit(viewport_width());

                let mut history = values
                    .get("prompt_history")
                    .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok())
                    .unwrap_or_default();
                // Legacy browser data is consumed only when the account has no history setting.
                if let Some(storage) =
                    web_sys::window().and_then(|window| window.local_storage().ok().flatten())
                {
                    if !values.contains_key("prompt_history")
                        && let Some(entries) = storage
                            .get_item("owide-prompt-history")
                            .ok()
                            .flatten()
                            .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
                    {
                        for entry in entries {
                            crate::history::push_history(&mut history, entry);
                        }
                    }
                    if values.contains_key("prompt_history") {
                        let _ = storage.remove_item("owide-prompt-history");
                    } else {
                        history_imported.set(true);
                    }
                    let _ = storage.remove_item("owide-theme");
                }
                chat.prompt_history.set(history);
                history_loaded.set(Some(generation));
            }

            if let Ok(connections) = connections_result {
                settings.connections.set(connections);
            }
            if let Ok(project_list) = projects_result {
                let project_ids = project_list
                    .iter()
                    .map(|project| project.id)
                    .collect::<Vec<_>>();
                projects.projects.set(project_list);
                spawn_local(async move {
                    let result = match candidates {
                        Ok(candidates) => {
                            crate::idb::delete_orphan_handles(
                                user.id,
                                &candidates,
                                &project_ids,
                                move || auth.generation.try_get_untracked() == Some(generation),
                            )
                            .await
                        }
                        Err(error) => Err(error),
                    };
                    if let Err(error) = result {
                        leptos::logging::warn!("Local folder cleanup failed: {error}");
                    }
                });
            }
            for project in projects
                .projects
                .get()
                .into_iter()
                .filter(|project| project.mode == WorkspaceMode::Local)
            {
                let handle_result = crate::idb::load_handle(project.id).await;
                if auth.generation.try_get_untracked() != Some(generation) {
                    return;
                }
                if let Ok(Some(handle)) = handle_result {
                    projects.local_handles.update(|handles| {
                        handles.insert(project.id, handle);
                    });
                }
            }
            if let Ok(sessions) = sessions_result {
                chat.sessions.set(sessions);
            }
            if let Ok(prompts) = prompts_result {
                settings.system_prompts.set(prompts);
            }

            let all_projects = projects.projects.get();
            let mut restored_tab_ids = Vec::new();
            for id in &stored_tab_ids {
                if all_projects.iter().any(|project| project.id == *id)
                    && !restored_tab_ids.contains(id)
                {
                    restored_tab_ids.push(*id);
                }
            }
            if restored_tab_ids.is_empty()
                && !chat_was_active
                && let Some(first) = all_projects.first()
            {
                restored_tab_ids.push(first.id);
            }
            projects.open_tab_ids.set(restored_tab_ids.clone());
            let active_project = if chat_was_active {
                None
            } else {
                stored_active_project
                    .filter(|id| restored_tab_ids.contains(id))
                    .or_else(|| restored_tab_ids.first().copied())
            };
            if let Some(project_id) = active_project {
                select_project.run(project_id);
            } else {
                chat.restore_chat_session();
            }
            projects.projects_loaded.set(true);
        });
    });

    Effect::new(move |_| {
        let generation = auth.generation.get();
        if auth.user.get().is_none()
            || history_loaded.get() != Some(generation)
            || !projects.projects_loaded.get()
        {
            return;
        }
        let project_id = projects.active_project.get();
        let Some(session_id) = chat.active_session.get() else {
            return;
        };
        let remembered = match project_id {
            Some(id) => chat
                .last_sessions
                .with_untracked(|sessions| sessions.get(&id).copied()),
            None => chat.last_chat_session.get_untracked(),
        };
        if !chat.sessions.with(|sessions| {
            sessions
                .iter()
                .any(|session| session.id == session_id && session.project_id == project_id)
        }) || remembered == Some(session_id)
        {
            return;
        }
        if let Some(id) = project_id {
            chat.last_sessions.update(|sessions| {
                sessions.insert(id, session_id);
            });
        } else {
            chat.last_chat_session.set(Some(session_id));
        }
        session_pending.update_value(|pending| {
            pending.insert(project_id, session_id);
        });
        if session_saving.get_untracked() {
            return;
        }
        session_saving.set(true);
        spawn_local(async move {
            loop {
                if auth.generation.try_get_untracked() != Some(generation) {
                    return;
                }
                let next = session_pending.with_value(|pending| {
                    pending
                        .iter()
                        .next()
                        .map(|(project, session)| (*project, *session))
                });
                let Some((project_id, session_id)) = next else {
                    break;
                };
                session_pending.update_value(|pending| {
                    pending.remove(&project_id);
                });
                let key = project_id.map_or_else(
                    || "last_chat_session".to_string(),
                    |id| format!("last_session_{id}"),
                );
                if let Err(error) = api
                    .with_value(Clone::clone)
                    .set_setting(&key, &session_id.to_string())
                    .await
                    && auth.generation.try_get_untracked() == Some(generation)
                {
                    chat.error.set(Some(format!(
                        "Could not save the last used session: {error}"
                    )));
                }
            }
            session_saving.set(false);
        });
    });

    Effect::new(move |_| {
        let generation = auth.generation.get();
        if auth.user.get().is_none() || history_loaded.get() != Some(generation) {
            return;
        }
        let history = chat.prompt_history.get();
        if let Ok(json) = serde_json::to_string(&history) {
            history_pending.set(Some(json));
            if history_saving.get_untracked() {
                return;
            }
            history_saving.set(true);
            spawn_local(async move {
                loop {
                    if auth.generation.try_get_untracked() != Some(generation) {
                        return;
                    }
                    let mut next = None;
                    history_pending.update(|pending| next = pending.take());
                    let Some(json) = next else {
                        break;
                    };
                    if api
                        .with_value(Clone::clone)
                        .set_setting("prompt_history", &json)
                        .await
                        .is_ok()
                        && auth.generation.try_get_untracked() == Some(generation)
                        && history_imported.get_untracked()
                    {
                        if let Some(storage) = web_sys::window()
                            .and_then(|window| window.local_storage().ok().flatten())
                        {
                            let _ = storage.remove_item("owide-prompt-history");
                        }
                        history_imported.set(false);
                    }
                }
                history_saving.set(false);
            });
        }
    });

    Effect::new(move |_| {
        if !projects.projects_loaded.get() {
            return;
        }
        let ids = projects.open_tab_ids.get();
        if let Ok(json) = serde_json::to_string(&ids) {
            let backend = api.with_value(Clone::clone);
            let generation = auth.generation.get_untracked();
            spawn_local(async move {
                if auth.generation.try_get_untracked() == Some(generation) {
                    let _ = backend.set_setting("open_tabs", &json).await;
                }
            });
        }
    });

    Effect::new(move |_| {
        if !projects.projects_loaded.get() {
            return;
        }
        let active_project = projects.active_project.get();
        let value = active_project.map(|id| id.to_string()).unwrap_or_default();
        let backend = api.with_value(Clone::clone);
        let generation = auth.generation.get_untracked();
        spawn_local(async move {
            if auth.generation.try_get_untracked() == Some(generation) {
                let _ = backend.set_setting("active_project", &value).await;
            }
        });
    });
}

fn viewport_width() -> f64 {
    web_sys::window()
        .and_then(|window| window.inner_width().ok())
        .and_then(|value| value.as_f64())
        .unwrap_or(1200.0)
}
