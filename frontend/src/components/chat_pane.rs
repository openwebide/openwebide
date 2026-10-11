use std::time::Duration;

use js_sys::Date;

use crate::state::{
    chat::ChatState, layout::LayoutState, projects::ProjectsState, settings::SettingsState,
};
use leptos::prelude::*;
use openwebide_core::{
    Connection, FileDiff, ModelInfo, Role, paint_inline_diff,
    tui::{SessionTelemetry, SlashCommand, extract_editor_context_prelude, parse_thinking},
};
use web_sys::wasm_bindgen::JsCast;

pub use crate::conversation::{ConversationItem, ToolStepResult};

pub(crate) use crate::markdown::render as render_markdown;

/// Render the diff for a file edit: the changed path and the removed/added lines.
fn render_diff_view(diff: FileDiff) -> impl IntoView {
    let lines = paint_inline_diff(&diff)
        .into_iter()
        .filter(|line| line.marker != ' ')
        .collect::<Vec<_>>();
    let path = diff.path.clone();
    view! {
        <div class="tui-diff-box">
            <div class="tui-diff-path">"diff: " {path}</div>
            <div class="tui-diff-lines">
                {lines.into_iter().map(|line| {
                    let mark = line.marker;
                    let line_class = if mark == '+' {
                        "tui-diff-line add"
                    } else {
                        "tui-diff-line del"
                    };
                    view! {
                        <div class=line_class><span>{mark} " "</span>{super::editor::render_painted_diff(line.tokens)}{line.ending_note.map(|note| view! { <span class="form-hint">{note}</span> })}</div>
                    }
                }).collect::<Vec<_>>()}
            </div>
        </div>
    }
}

fn render_approval_diff(diff: Memo<Option<FileDiff>>) -> impl IntoView {
    let expanded = RwSignal::new(false);
    let lines = Memo::new(move |_| {
        diff.with(|diff| {
            diff.as_ref()
                .map(paint_inline_diff)
                .unwrap_or_default()
                .into_iter()
                .filter(|line| line.marker != ' ')
                .collect::<Vec<_>>()
        })
    });
    let has_more = move || lines.with(|lines| lines.len() > 40);
    view! {
        <div class="tui-diff-box">
            <div class="tui-diff-path">"diff: " {move || diff.with(|diff| diff.as_ref().map(|diff| diff.path.clone()).unwrap_or_default())}</div>
            <div class="tui-diff-lines">
                {move || lines.with(|lines| {
                    lines.iter().take(if expanded.get() { lines.len() } else { 40 }).cloned().map(|line| {
                        let class = if line.marker == '+' { "tui-diff-line add" } else { "tui-diff-line del" };
                        view! {
                            <div class=class>
                                <span>{line.marker} " "</span>
                                {super::editor::render_painted_diff(line.tokens)}
                                {line.ending_note.map(|note| view! { <span class="form-hint">{note}</span> })}
                            </div>
                        }
                    }).collect::<Vec<_>>()
                })}
            </div>
            <Show when=move || has_more() && !expanded.get()>
                <button class="btn" on:click=move |_| expanded.set(true)>"Show full diff"</button>
            </Show>
        </div>
    }
}

/// Render a single assistant message with collapsible thinking stream.
fn render_assistant_message(content: Memo<String>) -> AnyView {
    let parsed = Memo::new(move |_| content.with(|content| parse_thinking(content)));
    let thinking_sig =
        Memo::new(move |_| parsed.with(|parsed| parsed.thinking.clone().unwrap_or_default()));
    let answer_sig = Memo::new(move |_| parsed.with(|parsed| parsed.answer.clone()));
    let is_thinking_active = Memo::new(move |_| parsed.with(|parsed| parsed.is_thinking));

    let timing = RwSignal::new(openwebide_core::tui::ReasoningTiming::default());
    let now = RwSignal::new(Date::now());
    let timer: StoredValue<Option<IntervalHandle>> = StoredValue::new(None);
    Effect::new(move |_| {
        let active = is_thinking_active.get();
        let timestamp = Date::now();
        timing.update(|timing| timing.observe(active, timestamp));
        now.set(timestamp);
        if active {
            // Keep one interval through all chunks of this reasoning phase.
            if timer.get_value().is_none() {
                timer.set_value(
                    set_interval_with_handle(
                        move || now.set(Date::now()),
                        Duration::from_millis(100),
                    )
                    .ok(),
                );
            }
        } else {
            timer.update_value(|timer| {
                if let Some(handle) = timer.take() {
                    handle.clear();
                }
            });
        }
    });
    on_cleanup(move || {
        if let Some(handle) = timer.get_value() {
            handle.clear();
        }
    });
    let tokens =
        Memo::new(move |_| thinking_sig.with(|text| openwebide_core::tui::estimate_tokens(text)));
    let summary = move || {
        openwebide_core::tui::reasoning_summary(
            is_thinking_active.get(),
            timing.get().elapsed_ms(now.get()),
            tokens.get(),
        )
    };

    view! {
        <div class="tui-stream-line tui-assistant">
            <Show when=move || !thinking_sig.with(String::is_empty) fallback=|| ()>
                <super::ui::DisclosurePanel class="tui-thinking-box" toggle_class="tui-thinking-summary"
                    title="Toggle reasoning trace; token counts are estimated"
                    active=Signal::derive(move || is_thinking_active.get())
                    summary=move || view! {
                        <span class="tui-think-meta">{summary}</span>
                        <Show when=move || is_thinking_active.get()><span class="tui-spinner" aria-hidden="true"/></Show>
                    }>
                    <div class="tui-thinking-trace">
                        <pre class="tui-thinking-pre">{move || thinking_sig.get()}</pre>
                    </div>
                </super::ui::DisclosurePanel>
            </Show>

            <Show when=move || !answer_sig.with(String::is_empty) fallback=|| ()>
                <div
                    class="tui-assistant-body markdown"
                    inner_html=move || render_markdown(&answer_sig.get())
                />
            </Show>
        </div>
    }.into_any()
}

/// Render a single user message with extracted editor context pill if present.
fn render_user_message(content: Memo<String>, actions: AnyView) -> AnyView {
    let prompt = Memo::new(move |_| openwebide_core::PromptContent::decode(&content.get()));
    let pill_sig = Memo::new(move |_| {
        prompt.with(|prompt| {
            extract_editor_context_prelude(&prompt.text)
                .0
                .map(ToString::to_string)
        })
    });
    let text_sig = Memo::new(move |_| {
        prompt.with(|prompt| extract_editor_context_prelude(&prompt.text).1.to_string())
    });

    view! {
        <div class="tui-stream-line tui-user" data-context-menu="">
            <Show when=move || pill_sig.with(Option::is_some) fallback=|| ()>
                <div class="tui-attached-pill">
                    <span class="tui-pill-icon"><crate::components::ui::Icon name=crate::components::ui::IconName::Paperclip /></span>
                    <span class="tui-pill-text">{move || pill_sig.get().unwrap_or_default()}</span>
                </div>
            </Show>
            <crate::prompt::PromptHistory content=content />
            <div class="tui-user-prompt">
                <span class="tui-glyph user">"❯"</span>
                <span class="tui-user-text">{move || text_sig.get()}</span>
            </div>
            {actions}
        </div>
    }
    .into_any()
}

/// Render an agent tool step as a terminal TUI box with box-drawing glyphs.
fn render_tool_step(
    item: RwSignal<ConversationItem>,
    awaiting_step: Memo<Option<(String, String)>>,
    on_permission: Callback<(String, bool)>,
    on_permission_always: Callback<String>,
) -> impl IntoView {
    let chat = expect_context::<ChatState>();
    let running = Memo::new(move |_| {
        chat.streaming.get() && chat.streaming_session.get() == chat.active_session.get()
    });
    let result_sig = Memo::new(move |_| {
        item.with(|item| match item {
            ConversationItem::ToolStep { result, .. } => result.clone(),
            _ => None,
        })
    });
    let id_sig = Memo::new(move |_| {
        item.with(|item| match item {
            ConversationItem::ToolStep { id, .. } => id.clone(),
            _ => String::new(),
        })
    });
    let name_sig = Memo::new(move |_| {
        item.with(|item| match item {
            ConversationItem::ToolStep { name, .. } => name.clone(),
            _ => String::new(),
        })
    });
    let summary = Memo::new(move |_| {
        item.with(|item| match item {
            ConversationItem::ToolStep { summary, .. } => summary.clone(),
            _ => String::new(),
        })
    });
    let awaiting_permission = Memo::new(move |_| {
        item.with(|item| {
            matches!(
                item,
                ConversationItem::ToolStep {
                    awaiting_permission: true,
                    ..
                }
            )
        })
    });
    let is_current_awaiting = Memo::new(move |_| {
        awaiting_step.with(|step| {
            step.as_ref()
                .is_some_and(|(id, _)| id_sig.with(|row_id| id == row_id))
        })
    });
    let preview = Memo::new(move |_| {
        item.with(|item| match item {
            ConversationItem::ToolStep { diff, .. } => diff.clone(),
            _ => None,
        })
    });
    let note = Memo::new(move |_| {
        item.with(|item| match item {
            ConversationItem::ToolStep { note, .. } => note.clone(),
            _ => None,
        })
    });
    let timing = Memo::new(move |_| {
        item.with(|item| match item {
            ConversationItem::ToolStep { timing, result, .. } => {
                timing.filter(|timing| timing.finished || (running.get() && result.is_none()))
            }
            _ => None,
        })
    });
    let status_class = move || {
        result_sig.with(|result| match result {
            Some(result) if result.ok => "ok",
            Some(_) => "err",
            None if awaiting_permission.get() => "awaiting",
            None if timing.get().is_some_and(|timing| timing.finished) => "stopped",
            None if !running.get() => "stopped",
            None => "running",
        })
    };
    let status_badge = move || match status_class() {
        "ok" => "[✔ ok]",
        "err" => "[✖ err]",
        "awaiting" => "[? permission required]",
        "stopped" => "[⏹ stopped]",
        _ => "[running]",
    };
    let show_diff = RwSignal::new(true);

    view! {
        <div class=move || format!("tui-box tui-tool-box {}", status_class())>
            <div class="tui-tool-topbar">
                <span class="tui-box-corner">"┌─"</span>
                <span class="tui-tool-tag">"[tool]"</span>
                <span class="tui-tool-title">{move || format!(" {}(\"{}\") ", name_sig.get(), summary.get())}</span>
                <span class="tui-tool-spacer"></span>
                <super::tool_duration::ToolDuration timing=timing />
                <Show when=move || status_class() == "running"><span class="tui-spinner" aria-hidden="true"/></Show>
                <span class=move || format!("tui-tool-status-badge {}", status_class())>{status_badge}</span>
                <span class="tui-box-corner">"─┐"</span>
            </div>

            <div class="tui-tool-inner">
                <Show when=move || result_sig.with(Option::is_none)>
                    {move || note.get().map(|note| view! { <div class="tui-perm-text">{note}</div> })}
                    <Show when=move || show_diff.get() && preview.with(|diff| diff.as_ref().is_some_and(|diff| !diff.old_unavailable))>
                        {render_approval_diff(preview)}
                    </Show>
                    <Show when=move || preview.with(|diff| diff.as_ref().is_some_and(|diff| diff.old_unavailable))>
                        <div class="tui-perm-text">"diff unavailable"</div>
                    </Show>
                </Show>
                <Show
                    when=move || awaiting_permission.get() && result_sig.get().is_none() && is_current_awaiting.get()
                    fallback=|| ()
                >
                    <div class="tui-permission-prompt">
                        <div class="tui-perm-text">
                            {move || format!("? Allow {} \"{}\"?", name_sig.get(), summary.get())}
                        </div>
                        <div class="tui-perm-buttons">
                            <button
                                class="btn approve tui-perm-btn btn-y"
                                title="Approve this call [Alt+Y]"
                                on:click=move |_| on_permission.run((id_sig.get(), true))
                            >
                                "[Alt+Y]es"
                            </button>
                            <button
                                class="btn deny tui-perm-btn btn-n"
                                title="Deny this call [Alt+N]"
                                on:click=move |_| on_permission.run((id_sig.get(), false))
                            >
                                "[Alt+N]o"
                            </button>
                            <Show
                                when=move || name_sig.get() == "write_file"
                                fallback=|| ()
                            >
                                <button
                                    class="btn send tui-perm-btn btn-a"
                                    title="Auto-accept edits for this session [Alt+A]"
                                    on:click=move |_| on_permission_always.run(id_sig.get())
                                >
                                    "[Alt+A]uto-accept edits"
                                </button>
                            </Show>
                            <button
                                class="btn ghost tui-perm-btn btn-d"
                                title="Toggle diff inspection [Alt+D]"
                                on:click=move |_| show_diff.update(|v| *v = !*v)
                            >
                                "[Alt+D]iff"
                            </button>
                        </div>
                    </div>
                </Show>

                <Show
                    when=move || awaiting_permission.get() && result_sig.get().is_none() && !is_current_awaiting.get()
                    fallback=|| ()
                >
                    <div class="tui-tool-pending muted">
                        "[canceled: no longer pending]"
                    </div>
                </Show>

                <Show
                    when=move || status_class() == "running"
                    fallback=|| ()
                >
                    <div class="tui-tool-pending">
                        <span class="tui-spinner"/>
                        {move || if name_sig.get() == openwebide_core::questions::TOOL_NAME {
                            " waiting for your answers..."
                        } else {
                            " executing tool call on host..."
                        }}
                    </div>
                </Show>

                <Show
                    when=move || result_sig.get().is_some_and(|r| r.diff.is_some()) && show_diff.get()
                    fallback=|| ()
                >
                    {move || {
                        let r = result_sig.get().unwrap();
                        render_diff_view(r.diff.unwrap())
                    }}
                </Show>

                <Show
                    when=move || result_sig.with(Option::is_some)
                    fallback=|| ()
                >
                    {move || {
                        let r = result_sig.get().unwrap();
                        view! {
                            <super::tool_output::ToolOutput text=r.summary />
                        }
                    }}
                </Show>
            </div>

            <div class="tui-tool-bottombar">
                <span class="tui-box-corner">"└"</span>
                <span class="tui-tool-barline"></span>
                <span class="tui-box-corner">"┘"</span>
            </div>
        </div>
    }
}

fn conversation_blocks(
    messages: crate::state::chat::ConversationStore,
) -> Vec<crate::state::chat::ConversationHandle> {
    messages.handles.with(|handles| {
        let mut previous_tool = false;
        handles
            .iter()
            .copied()
            .filter(|handle| {
                handle.visible.get() && !handle.item.with(crate::conversation::is_run_context)
            })
            .filter(|handle| {
                let tool = handle.item.with(crate::conversation::is_activity);
                let include = !tool || !previous_tool;
                previous_tool = tool;
                include
            })
            .collect()
    })
}

/// A stable group anchored to its first call; new live calls join without remounting.
fn render_tool_group(
    messages: crate::state::chat::ConversationStore,
    first: u64,
    awaiting_step: Memo<Option<(String, String)>>,
    on_permission: Callback<(String, bool)>,
    on_permission_always: Callback<String>,
) -> impl IntoView {
    let rows = Memo::new(move |_| {
        messages.handles.with(|handles| {
            let visible = handles.iter().copied().filter(|handle| {
                handle.visible.get() && !handle.item.with(crate::conversation::is_run_context)
            });
            visible
                .skip_while(|handle| handle.key != first)
                .take_while(|handle| handle.item.with(crate::conversation::is_activity))
                .collect::<Vec<_>>()
        })
    });
    let names = Memo::new(move |_| {
        rows.with(|rows| {
            rows.iter()
                .map(|handle| {
                    handle.item.with(|item| match item {
                        ConversationItem::ToolStep { name, .. } => name.clone(),
                        _ => String::new(),
                    })
                })
                .collect::<Vec<_>>()
        })
    });
    let counts = Memo::new(move |_| {
        names.with(|names| {
            crate::conversation::tool_count_labels(
                names
                    .iter()
                    .filter(|name| !name.is_empty())
                    .map(String::as_str),
            )
        })
    });
    let approval = Signal::derive(move || {
        awaiting_step.with(|awaiting| {
            awaiting.as_ref().is_some_and(|(id, _)| {
                rows.with(|rows| {
                    rows.iter().any(|handle| {
                        handle.permission.with(|permission| {
                            permission
                                .as_ref()
                                .is_some_and(|(pending, _)| pending == id)
                        })
                    })
                })
            })
        })
    });
    let chat = expect_context::<ChatState>();
    let running = Signal::derive(move || {
        chat.streaming.get()
            && chat.streaming_session.get() == chat.active_session.get()
            && rows.with(|rows| {
                rows.iter().any(|row| {
                    row.item.with(|item| {
                        matches!(item, ConversationItem::ToolStep {
                    result: None, awaiting_permission: false, timing, ..
                } if !timing.is_some_and(|timing| timing.finished))
                    })
                })
            })
    });
    view! {
        <super::ui::DisclosurePanel class="tui-tool-group" force_open=Signal::derive(move || approval.get() || rows.with(|rows| rows.iter().any(|row| row.item.with(|item| matches!(item, ConversationItem::ToolStep { result: Some(result), .. } if !result.ok)))))
            summary=move || view! {
                <Show when=move || running.get()><span class="tui-spinner" aria-hidden="true"/></Show>
                <span class="tui-tool-tag">{move || rows.with(|rows| format!("{} steps", rows.len()))}</span>
                <span class="form-hint" title="Total recorded tool execution time, excluding approval waits">{move || rows.with(|rows| {
                    let elapsed: u64 = rows.iter().map(|row| row.item.with(|item| match item { ConversationItem::ToolStep { timing: Some(timing), .. } => timing.elapsed_ms, _ => 0 })).sum();
                    format!("· {}.{}s", elapsed / 1000, elapsed % 1000 / 100)
                })}</span>
                {move || counts.get().into_iter().map(|label| view! { <code class="tui-tool-count">{label}</code> }).collect_view()}
            }>
            <For each=move || rows.get() key=|handle| (handle.key, handle.item.with(crate::conversation::is_activity)) children=move |handle| {
                match handle.item.get_untracked() {
                    ConversationItem::Message(_) => render_assistant_message(Memo::new(move |_| handle.item.with(|item| match item { ConversationItem::Message(message) => message.content.clone(), _ => String::new() }))),
                    _ => render_tool_step(handle.item, awaiting_step, on_permission, on_permission_always).into_any(),
                }
            } />
        </super::ui::DisclosurePanel>
    }
}

/// Discover models only when a connection is expanded in the picker.
#[component]
fn ConnectionModels(
    connection: Connection,
    active_connection: Signal<Option<i64>>,
    models: ReadSignal<Vec<ModelInfo>>,
    on_select: Callback<(i64, String)>,
) -> impl IntoView {
    let api = expect_context::<crate::backend::Api>();
    let chat = expect_context::<ChatState>();
    let id = connection.id;
    let enabled = connection.enabled;
    let expanded = RwSignal::new(active_connection.get_untracked() == Some(id));
    let discovered = RwSignal::new(Vec::<ModelInfo>::new());
    let loading = RwSignal::new(false);
    let error = RwSignal::new(Option::<String>::None);
    let request = StoredValue::new(0_u64);
    Effect::new(move |_| {
        request.update_value(|generation| *generation += 1);
        let generation = request.get_value();
        if !expanded.get() || active_connection.get() == Some(id) || !enabled {
            return;
        }
        loading.set(true);
        error.set(None);
        leptos::task::spawn_local(async move {
            let result = api.with_value(Clone::clone).list_models(id).await;
            if request.try_get_value() != Some(generation) {
                return;
            }
            loading.set(false);
            match result {
                Ok(models) => discovered.set(models),
                Err(message) => error.set(Some(message)),
            }
        });
    });
    let available = Signal::derive(move || {
        if active_connection.get() == Some(id) {
            models.get()
        } else {
            discovered.get()
        }
    });
    view! {
        <div class="tui-connection-group" data-connection-id=id.to_string()>
            <button class="ui-dropdown-item recent-item tui-connection-heading" role="menuitem"
                aria-expanded=move || expanded.get().to_string()
                disabled=!enabled
                on:click=move |_| expanded.update(|value| *value = !*value)>
                <super::ui::Icon name=Signal::derive(move || if expanded.get() { super::ui::IconName::ChevronDown } else { super::ui::IconName::ChevronRight }) /> " " {connection.name}
            </button>
            <Show when=move || expanded.get()>
                <Show when=move || loading.get()><span class="form-hint">"Loading models…"</span></Show>
                <Show when=move || error.get().is_some()><span class="form-hint">{move || error.get()}</span></Show>
                <Show when=move || !loading.get() && error.get().is_none() && available.get().is_empty()>
                    <span class="form-hint">"No models available"</span>
                </Show>
                <For each=move || available.get() key=|model| model.name.clone() children=move |model| {
                    let name = model.name.clone();
                    let active_name = name.clone();
                    view! {
                        <button class="ui-dropdown-item recent-item tui-connection-model" role="menuitemradio"
                            aria-checked=move || (active_connection.get() == Some(id) && chat.session_telemetry.with(|telemetry| telemetry.model == active_name)).to_string()
                            disabled=move || chat.streaming.get() || chat.connection_changing.get()
                            on:click=move |_| on_select.run((id, name.clone()))>
                            {model.name}
                        </button>
                    }
                } />
            </Show>
        </div>
    }
}

/// Powerline-style statusline segment above composer.
#[component]
fn TuiStatusLine(
    streaming: ReadSignal<bool>,
    has_awaiting: Signal<bool>,
    session_telemetry: ReadSignal<SessionTelemetry>,
    local_mode: Signal<bool>,
    models: ReadSignal<Vec<ModelInfo>>,
    on_select_connection_model: Callback<(i64, String)>,
) -> impl IntoView {
    let gauge_color_class = move || {
        let pct = session_telemetry.with(SessionTelemetry::context_percent);
        if pct >= 85.0 {
            "ctx-danger"
        } else if pct >= 70.0 {
            "ctx-warning"
        } else {
            "ctx-normal"
        }
    };

    let ui = expect_context::<crate::state::ui::UiState>();
    let show_model_menu = RwSignal::new(false);
    let chat = expect_context::<ChatState>();
    let settings = expect_context::<SettingsState>();
    let active_connection = Signal::derive(move || {
        chat.active_session
            .get()
            .and_then(|id| {
                chat.sessions.with(|sessions| {
                    sessions
                        .iter()
                        .find(|session| session.id == id)
                        .and_then(|session| session.connection_id)
                })
            })
            .or_else(|| {
                chat.draft_connection
                    .get()
                    .filter(|_| chat.active_session.get().is_none())
            })
            .or_else(|| {
                settings
                    .model_setup
                    .get()
                    .defaults
                    .primary
                    .map(|selection| selection.server_id)
            })
            .or_else(|| settings.default_connection.get())
            .or_else(|| {
                settings.connections.with(|connections| {
                    connections
                        .iter()
                        .find(|connection| connection.enabled)
                        .map(|connection| connection.id)
                })
            })
    });
    let on_select = Callback::new(move |selection: (i64, String)| {
        on_select_connection_model.run(selection);
        show_model_menu.set(false);
    });

    view! {
        <div class="tui-statusline">
            <super::approval_mode::ApprovalModePicker />
            <Show when=move || streaming.get() || has_awaiting.get()><span class="tui-run-state">{move || if has_awaiting.get() { "Awaiting" } else { "Running" }}</span></Show>
            <span class="tui-sep">"│"</span>
            <super::dropdown::Dropdown class="tui-model-picker" trigger_class="btn ghost tui-model-name" menu_class="tui-model-menu" aria_label="Choose connection and model" open=show_model_menu above=true label=move || view! { <span>{move || session_telemetry.with(|telemetry| telemetry.model.clone())}</span> }>
                <For each=move || settings.connections.get()
                    key=|connection| (connection.id, connection.name.clone(), connection.base_url.clone(), connection.enabled)
                    children=move |connection| view! {
                        <ConnectionModels connection=connection active_connection=active_connection models=models on_select=on_select />
                    }
                />
            </super::dropdown::Dropdown>
            <span class="tui-sep">"│"</span>
            <button type="button" class=move || format!("btn sm ghost tui-ctx-gauge {}", gauge_color_class()) title="Context Window Utilization" aria-label="View context usage" on:click=move |_| ui.context_open.set(true)>
                <span class="tui-context-detail">
                "Ctx: "
                {move || session_telemetry.with(SessionTelemetry::compact_context_tokens)}
                "/"
                {move || session_telemetry.with(SessionTelemetry::compact_context_limit)}
                </span><span class="tui-context-percent">" ("
                {move || format!("{:.0}%", session_telemetry.with(SessionTelemetry::context_percent))}
                ")"</span>
            </button>
            <span class="tui-sep">"│"</span>
            <button type="button" class="btn sm ghost tui-speed" title="Generation statistics" aria-label="View generation statistics" on:click=move |_| ui.generation_open.set(true)>
                {move || session_telemetry.with(SessionTelemetry::speed_text)}
            </button>
            <span class="tui-sep">"│"</span>
            <span class="tui-workspace-mode">
                {move || if local_mode.get() { "Local" } else { "Remote" }}
            </span>
            <span class="tui-sep">"│"</span>
            <span class="tui-tools-count">
                {move || format!("{} tools", session_telemetry.with(|telemetry| telemetry.tool_calls_count))}
            </span>
            <super::monitors::ConversationMonitors />
            <super::goal::GoalStatusIndicator />
        </div>
    }
}

#[component]
pub fn ChatPane(
    on_select_connection_model: Callback<(i64, String)>,
    on_send: Callback<()>,
    on_resume_run: Callback<()>,
    on_stop: Callback<()>,
    /// Approve or deny a gated tool call: `(tool_call_id, approved)`.
    on_permission: Callback<(String, bool)>,
    on_permission_always: Callback<String>,
    on_slash_command: Callback<SlashCommand>,
    #[prop(optional)] on_rewind: Option<Callback<i64>>,
    #[prop(optional)] queue_actions: Option<crate::state_actions::prompt_queue::PromptQueueActions>,
    #[prop(optional)] conversation_actions: Option<
        crate::state_actions::conversation::ConversationActions,
    >,
) -> impl IntoView {
    let ui = expect_context::<crate::state::ui::UiState>();
    let reviews = use_context::<crate::state::reviews::ReviewsState>();
    let chat = expect_context::<ChatState>();
    let layout = expect_context::<LayoutState>();
    let projects = expect_context::<ProjectsState>();
    let questions = use_context::<crate::state::questions::QuestionsState>();
    let questions_available = questions.is_some();
    let host_available = use_context::<crate::host_admin::HostState>().is_some();
    let assistance = crate::state_actions::assistance::ChatAssistance::new(
        expect_context::<crate::backend::Api>(),
        expect_context::<crate::state::auth::AuthState>(),
        chat,
        projects,
    );
    let context_assistance =
        crate::state_actions::context_assistance::ContextAssistance::from_context();
    let messages = chat.messages;
    let streaming = chat.streaming.read_only();
    let draft = chat.draft.read_only();
    let set_draft = chat.draft.write_only();
    let models = chat.models.read_only();
    let session_telemetry = chat.session_telemetry.read_only();
    let has_session = chat.has_session;
    let local_mode = Signal::from(projects.local_mode);
    let scroll_ref = NodeRef::<leptos::html::Div>::new();
    let input_ref = NodeRef::<leptos::html::Textarea>::new();
    crate::viewport::install_composer(input_ref);
    let prompt_composer = crate::prompt::Composer::new(input_ref);
    provide_context(prompt_composer);
    let on_send = prompt_composer.after(on_send);
    let on_stop = prompt_composer.after(on_stop);
    let on_resume_run = prompt_composer.after(on_resume_run);
    let on_permission = prompt_composer.after(on_permission);
    let on_permission_always = prompt_composer.after(on_permission_always);
    let on_select_connection_model = prompt_composer.after(on_select_connection_model);
    let on_rewind = on_rewind.map(|action| prompt_composer.after(action));
    let context_assistance = crate::state_actions::context_assistance::ContextAssistance {
        attach: prompt_composer.after(context_assistance.attach),
        ..context_assistance
    };
    let queue_actions =
        queue_actions.map(
            |actions| crate::state_actions::prompt_queue::PromptQueueActions {
                enqueue: prompt_composer.after(actions.enqueue),
                steer: prompt_composer.after(actions.steer),
                edit: prompt_composer.after(actions.edit),
                remove: prompt_composer.after(actions.remove),
                toggle: prompt_composer.after(actions.toggle),
                cancel_edit: prompt_composer.after(actions.cancel_edit),
            },
        );
    let conversation_actions = conversation_actions.map(|actions| {
        crate::state_actions::conversation::ConversationActions {
            edit: prompt_composer.after(actions.edit),
            fork: prompt_composer.after(actions.fork),
            cancel_edit: prompt_composer.after(actions.cancel_edit),
        }
    });
    let slash_index = RwSignal::new(0usize);
    let slash_dismissed = RwSignal::new(None::<String>);
    let slash_options = Memo::new(move |_| {
        let text = draft.get();
        if slash_dismissed.get().as_ref() == Some(&text) {
            Vec::new()
        } else {
            openwebide_core::tui::slash_suggestions(&text)
        }
    });
    let complete_slash = move |info: openwebide_core::tui::SlashInfo| {
        set_draft.set(format!(
            "{}{}",
            info.command,
            if info.arguments.is_empty() { "" } else { " " }
        ));
        slash_dismissed.set(Some(draft.get_untracked()));
        slash_index.set(0);
        if let Some(input) = input_ref.get_untracked() {
            let _ = input.focus();
        }
    };
    let slash_hint = Memo::new(move |_| {
        let text = draft.get();
        let (name, _) = text.split_once(' ')?;
        openwebide_core::tui::SLASH_COMMANDS
            .iter()
            .find(|info| info.command == name)
            .copied()
    });

    let composing = RwSignal::new(false);
    let caret_at_end = RwSignal::new(true);
    let input_scroll = RwSignal::new((0.0_f64, 0.0_f64));
    let update_caret = move || {
        if let Some(input) = input_ref.get_untracked() {
            let end = u32::try_from(input.value().encode_utf16().count()).ok();
            caret_at_end.set(
                input.selection_start().ok().flatten() == end
                    && input.selection_end().ok().flatten() == end,
            );
        }
    };
    let inline_hint = Memo::new(move |_| {
        if streaming.get()
            || composing.get()
            || chat.awaiting_step_id.get().is_some()
            || chat.prompt_edit.get().is_some()
            || chat.queue_edit.get().is_some()
            || !slash_options.with(Vec::is_empty)
        {
            return None;
        }
        let prefix = draft.get();
        assistance.next_actions.with(|prompts| {
            prompts
                .iter()
                .find(|prompt| prompt.starts_with(&prefix) && prompt.len() > prefix.len())
                .cloned()
        })
    });

    // Readline prompt history state
    let prompt_history = chat.prompt_history;
    let history_index = RwSignal::new(Option::<usize>::None);
    let draft_backup = RwSignal::new(String::new());

    // Check if any tool step is currently awaiting permission
    let awaiting_step_id = chat.awaiting_step_id;
    let awaiting_step = Memo::new(move |_| {
        let awaiting_id = awaiting_step_id.get()?;
        messages.handles.with(|handles| {
            handles
                .iter()
                .rev()
                .find_map(|handle| handle.permission.get().filter(|(id, _)| *id == awaiting_id))
        })
    });
    let has_awaiting = Signal::derive(move || awaiting_step.get().is_some());

    // Keep the newest message in view as tokens arrive.
    Effect::new(move || {
        messages.changed.get();
        assistance.next_actions.track();
        context_assistance.suggestions.track();
        leptos::leptos_dom::helpers::request_animation_frame(move || {
            if let Some(Some(el)) = scroll_ref.try_get_untracked() {
                el.set_scroll_top(f64::from(el.scroll_height()));
            }
        });
    });

    // Mirror the draft signal into the textarea so it clears after a send.
    Effect::new(move || {
        let value = draft.get();
        if let Some(ta) = input_ref.get() {
            if ta.value() != value {
                ta.set_value(&value);
            }
            crate::viewport::fit_composer(&ta);
        }
    });

    let submission_blocked = Memo::new(move |_| {
        chat.compacting.get()
            || chat.goal_busy.get()
            || chat.branching.get()
            || chat.queue_busy.get()
            || chat.rewinding.get()
            || reviews.is_some_and(|state| state.busy.get().is_some())
            || chat.creating_session.get()
            || chat.reading_images.get()
    });
    let submit_or_command = {
        move || {
            let current = draft.get().trim().to_string();
            if current.is_empty() && chat.prompt_images.with(Vec::is_empty) {
                return;
            }

            prompt_history.update(|history| crate::history::push_history(history, current.clone()));
            history_index.set(None);

            if chat.prompt_images.with(Vec::is_empty)
                && current.starts_with('/')
                && let Some(cmd) = SlashCommand::parse(&current)
            {
                set_draft.set(String::new());
                on_slash_command.run(cmd);
                return;
            }

            if (streaming.get_untracked() || chat.queue_edit.get_untracked().is_some())
                && let Some(queue) = queue_actions
            {
                queue.enqueue.run(());
            } else if !streaming.get_untracked() {
                on_send.run(());
            }
        }
    };

    view! {
        <main
            class="chat-pane tui-pane"
            on:click=move |event: web_sys::MouseEvent| {
                if let Some(target) = event.target().and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                    && let Ok(Some(button)) = target.closest("button")
                    && !button.has_attribute("aria-haspopup")
                { prompt_composer.focus(); }
            }
                on:dragover=move |event: web_sys::DragEvent| { if event.data_transfer().is_some_and(|transfer| transfer.types().includes(&wasm_bindgen::JsValue::from_str("Files"), 0)) { event.prevent_default(); } }
                on:drop=move |event: web_sys::DragEvent| { if let Some(files) = event.data_transfer().and_then(|transfer| transfer.files()) && files.length() > 0 { event.prevent_default(); prompt_composer.import(files); } }
            style=move || format!("width: {}px; flex: none;", layout.chat_width.get())
        >
            <Show when=move||questions_available><super::questions::QuestionsPanel/></Show>
            <Show when=move||host_available && projects.active_project.get().is_none()><super::host_admin::HostPanel/></Show>
            <div class="messages tui-stream" node_ref=scroll_ref>
            <Show
                when=move || !messages.handles.with(Vec::is_empty)
                fallback=move || {
                    view! {
                        <div class="chat-welcome">
                            <div class="empty-state tui-empty-state">
                                <div class="chat-welcome-heading">
                                    <super::ui::LogoMark class="chat-welcome-logo" />
                                    <div>
                                        <h1>"Open WebIDE"</h1>
                                        <p>"A home for your code and local models."</p>
                                    </div>
                                </div>
                                <p class="chat-welcome-tip">"Ask a question, describe a change, or type "<code>"/help"</code>" to explore commands."</p>
                            </div>
                        </div>
                    }
                }
            >
                    <div class="tui-stream-spacer"></div>
                    <Show when=move || assistance.recap.get().is_some() && !streaming.get()>
                        <details class="chat-recap"><summary>"Where you left off"</summary><p class="form-hint">{move || assistance.recap.get().unwrap_or_default()}</p></details>
                    </Show>
                    <For
                        each=move || conversation_blocks(messages)
                        key=|handle| (handle.key, handle.item.with(crate::conversation::is_activity))
                        children=move |handle| {
                            let item = handle.item;
                            match item.get_untracked() {
                                ConversationItem::Task(_) => render_task_run(Memo::new(move |_| item.with(|item| match item { ConversationItem::Task(task) => (**task).clone(), _ => unreachable!() })), awaiting_step, on_permission, on_permission_always).into_any(),
                                ConversationItem::Stopped { .. } => view! { <div class="stopped-marker tui-stopped-marker">"⏹ execution aborted"</div> }.into_any(),
                                ConversationItem::ToolStep { .. } => render_tool_group(messages, handle.key, awaiting_step, on_permission, on_permission_always).into_any(),
                                ConversationItem::Message(ref message) if crate::conversation::is_reasoning_activity(message) => render_tool_group(messages, handle.key, awaiting_step, on_permission, on_permission_always).into_any(),
                                ConversationItem::Message(_) | ConversationItem::Notice { .. } => {
                                    let content = Memo::new(move |_| item.with(|item| match item {
                                        ConversationItem::Message(message) => message.content.clone(),
                                        ConversationItem::Notice { text, .. } => text.clone(),
                                        _ => String::new(),
                                    }));
                                    let system = Memo::new(move |_| item.with(|item| matches!(item, ConversationItem::Message(message) if message.role == Role::System)));
                                    let assistant = Memo::new(move |_| item.with(|item| !matches!(item, ConversationItem::Message(message) if message.role != Role::Assistant)));
                                    view! {
                                        <Show when=move || system.get() fallback=move || view! {
                                            <Show when=move || assistant.get() fallback=move || view! {
                                                {render_user_message(content, view! { <div class="tui-prompt-actions"><Show when=move || item.with(|item| matches!(item, ConversationItem::Message(message) if message.id > 0 && message.role == Role::User && (conversation_actions.is_some() || on_rewind.is_some() || chat.run_contexts.with(|contexts| contexts.contains_key(&message.id)))))><super::dropdown::ActionMenu aria_label="Message actions">
                                                <Show when=move || item.with(|item| matches!(item, ConversationItem::Message(message) if chat.run_contexts.with(|contexts| contexts.contains_key(&message.id))))>
                                                    <button role="menuitem" type="button" class="ui-dropdown-item recent-item" aria-label="Run context" data-message-id=move || item.with(|item| match item { ConversationItem::Message(message) => message.id, _ => 0 }) on:click=move |_| {
                                                        if let Some(id) = item.with_untracked(|item| match item { ConversationItem::Message(message) => Some(message.id), _ => None }) { ui.run_context.set(Some(id)); }
                                                    }><super::ui::Icon name=super::ui::IconName::FileText/><span>"Run context"</span></button>
                                                </Show>

                                                <Show when=move || conversation_actions.is_some() && item.with(|item| matches!(item, ConversationItem::Message(message) if message.id > 0 && message.role == Role::User))>
                                                    <button role="menuitem" type="button" class="ui-dropdown-item recent-item icon-btn ui-icon tui-edit-prompt" title="Edit prompt" aria-label="Edit prompt" data-message-id=move || item.with(|item| match item { ConversationItem::Message(message) => message.id, _ => 0 }) disabled=move || streaming.get() || chat.rewinding.get() || chat.branching.get() || chat.queue_busy.get() || chat.reading_images.get() on:click=move |_| {
                                                        if let (Some(actions), Some(id)) = (conversation_actions, item.with_untracked(|item| match item { ConversationItem::Message(message) => Some(message.id), _ => None })) { actions.edit.run(id); }
                                                    }><span aria-hidden="true"><crate::components::ui::Icon name=crate::components::ui::IconName::Pencil /></span><span>"Edit prompt"</span></button>
                                                    <button role="menuitem" type="button" class="ui-dropdown-item recent-item icon-btn ui-icon tui-fork-prompt" aria-label="Fork conversation from this prompt" data-message-id=move || item.with(|item| match item { ConversationItem::Message(message) => message.id, _ => 0 }) title="Fork conversation" disabled=move || streaming.get() || chat.rewinding.get() || chat.branching.get() || chat.queue_busy.get() || chat.reading_images.get() on:click=move |_| {
                                                        if let (Some(actions), Some(id)) = (conversation_actions, item.with_untracked(|item| match item { ConversationItem::Message(message) => Some(message.id), _ => None })) { actions.fork.run(id); }
                                                    }><span aria-hidden="true"><crate::components::ui::Icon name=crate::components::ui::IconName::GitFork /></span><span>"Fork conversation"</span></button>
                                                </Show>
                                                <Show when=move || on_rewind.is_some() && item.with(|item| matches!(item, ConversationItem::Message(message) if message.id > 0 && message.role == Role::User))>
                                                    <button role="menuitem" type="button" class="ui-dropdown-item recent-item icon-btn ui-icon tui-rewind" title="Rewind to this prompt" aria-label="Rewind to this prompt" data-message-id=move || item.with(|item| match item { ConversationItem::Message(message) => message.id, _ => 0 }) disabled=move || streaming.get() || chat.rewinding.get()
                                                        on:click=move |_| {
                                                            if let (Some(action), Some(id)) = (on_rewind, item.with(|item| match item { ConversationItem::Message(message) => Some(message.id), _ => None })) { action.run(id); }
                                                        }><span aria-hidden="true"><crate::components::ui::Icon name=crate::components::ui::IconName::Undo2 /></span><span>"Rewind to this prompt"</span></button>
                                                </Show>
                                                </super::dropdown::ActionMenu></Show></div> }.into_any())}
                                                {move || item.with(|item| match item { ConversationItem::Message(message) if message.id > 0 => view! { <crate::components::RunChangesPanel message=message.id /> }.into_any(), _ => ().into_any() })}
                                                {move || item.with(|item| match item { ConversationItem::Message(message) if message.id > 0 && message.role == Role::User => view! { <super::turn_summary::TurnSummary message=message.id /> }.into_any(), _ => ().into_any() })}
                                            }>
                                                {render_assistant_message(content)}
                                            </Show>
                                        }>
                                            <super::ui::DisclosurePanel class="tui-thinking-box" toggle_class="tui-thinking-summary" summary=move || view! { <span>{move || if content.get().starts_with(openwebide_core::COMPACTION_PREFIX) { "Conversation summary" } else { "Run context" }}</span> }>
                                                <div class="tui-thinking-trace">
                                                    <pre class="tui-thinking-pre">{move || {
                                                        let text = content.get();
                                                        openwebide_core::Compaction::parse(&text).map_or_else(|| text.strip_prefix(openwebide_core::RUN_CONTEXT_PREFIX).unwrap_or(&text).to_string(), |compaction| compaction.summary)
                                                    }}</pre>
                                                </div>
                                            </super::ui::DisclosurePanel>
                                        </Show>
                                    }.into_any()
                                }
                            }
                        }
                    />
                    <Show when=move || assistance.completion.get().is_some() && !streaming.get()>
                        <p class="form-hint chat-completion" role="status"><span role="img" aria-label="Conversation recap" title="Conversation recap">"↪"</span>" "{move || assistance.completion.get().unwrap_or_default()}</p>
                    </Show>
                    <Show when=move || assistance.activity.get().is_some() && streaming.get()>
                        <p class="form-hint chat-activity" role="status">{move || assistance.activity.get().unwrap_or_default()}</p>
                    </Show>
                    <Show when=move || chat.interrupted_run.get().is_some() && !streaming.get()>
                        <div class="tui-stopped-marker">
                            "This run was interrupted. Resume continues from saved history without replaying unfinished tools. "
                            <button class="btn send" disabled=move||questions.is_some_and(|state|!state.questions.with(Vec::is_empty)) on:click=move |_| on_resume_run.run(())>"Resume"</button>
                            <button class="btn" on:click=move |_| chat.dismiss_interrupted_run()>"Dismiss"</button>
                        </div>
                    </Show>
            </Show>
            <Show when=move || !assistance.next_actions.with(Vec::is_empty) && !streaming.get() && draft.get().is_empty() && chat.prompt_images.with(Vec::is_empty) && chat.prompt_edit.get().is_none() && chat.queue_edit.get().is_none()>
                <div class="chat-followups" aria-label="Suggested next actions">
                    {move || assistance.next_actions.get().into_iter().map(|prompt| {
                        let label = prompt.clone();
                        let accessible_label = label.clone();
                        view! { <button class="btn md" type="button" aria-label=accessible_label title="Send this prompt" disabled=move || submission_blocked.get() on:click=move |_| {
                            if !submission_blocked.get_untracked() && !streaming.get_untracked() && chat.draft.get_untracked().is_empty() && chat.prompt_images.with_untracked(Vec::is_empty) {
                                history_index.set(None);
                                prompt_history.update(|history| crate::history::push_history(history, prompt.clone()));
                                chat.draft.set(prompt.clone());
                                on_send.run(());
                            }
                        }><span class="tui-glyph user" aria-hidden="true">"❯"</span><span>{label}</span></button> }
                    }).collect::<Vec<_>>()}
                </div>
            </Show>
            <Show when=move || !context_assistance.suggestions.with(Vec::is_empty)>
                <div class="chat-context-suggestions" aria-label="Suggested context">
                    {move || context_assistance.suggestions.get().into_iter().map(|candidate| {
                        let label = candidate.label();
                        view! { <button class="btn ghost" title="Attach this context" on:click=move |_| context_assistance.attach.run(candidate.clone())><super::ui::Icon name=super::ui::IconName::Paperclip/>{label}</button> }
                    }).collect::<Vec<_>>()}
                </div>
            </Show>
            <Show when=move || context_assistance.error.get().is_some()><p class="form-hint" role="alert">{move || context_assistance.error.get().unwrap_or_default()}</p></Show>
            </div>

            <TuiStatusLine
                streaming=streaming
                has_awaiting=has_awaiting
                session_telemetry=session_telemetry
                local_mode=local_mode
                models=models
                on_select_connection_model=on_select_connection_model
            />

            <Show when=move || chat.compacting.get()><p class="form-hint" role="status">"Compacting conversation…" <button class="btn stop" on:click=move |_| on_stop.run(())>"Stop"</button></p></Show>
            <super::goal::GoalNotice />
            <super::todo_plan::TodoPlanPanel />
            <Show when=move || chat.prompt_edit.get().is_some()>
                <div class="tui-prompt-edit"><span>"Editing an earlier prompt. Send starts a new branch."</span>
                    {conversation_actions.map(|actions| view! { <button class="btn ghost" disabled=move || streaming.get() on:click=move |_| actions.cancel_edit.run(())>"Cancel edit"</button> })}
                </div>
            </Show>
            <Show when=move || chat.branching.get()><div class="tui-prompt-edit">"Copying conversation…"</div></Show>
            {queue_actions.map(|actions| view! { <crate::components::chat_pane::PromptQueueControls actions=actions /> })}
            <Show when=move || !slash_options.with(Vec::is_empty)>
                <div class="slash-suggestions" id="slash-suggestions" role="listbox" aria-label="Slash commands">
                    {move || slash_options.get().into_iter().enumerate().map(|(index, info)| view! {
                        <button class="ui-dropdown-item" class:active=move || slash_index.get() == index id=format!("slash-option-{index}") role="option" aria-selected=move || (slash_index.get() == index).to_string() type="button" on:mousedown=move |event| event.prevent_default() on:click=move |_| complete_slash(info)>
                            <code>{info.command} " " {info.arguments}</code><span>{info.description}</span>
                        </button>
                    }).collect_view()}
                </div>
            </Show>
            <Show when=move || slash_hint.get().is_some()><p class="form-hint slash-hint">{move || slash_hint.get().map(|info| format!("{} {} · {}", info.command, info.arguments, info.description))}</p></Show>
            <div class="composer tui-composer" class:is-streaming=move || streaming.get()>
                <span class="tui-prompt-glyph">"❯"</span>
                <div class="tui-input-shell">
                <crate::prompt::MentionSuggestions composer=prompt_composer/>
                <textarea
                    class="composer-input tui-input"
                    rows="1"
                    aria-label="Chat message"
                    aria-autocomplete=move || if inline_hint.get().is_some() {"both"} else {"list"}
                    aria-describedby=move || (inline_hint.get().is_some() && caret_at_end.get()).then_some("composer-inline-hint")
                    aria-controls="slash-suggestions"
                    aria-expanded=move || (!slash_options.with(Vec::is_empty)).to_string()
                    aria-activedescendant=move || (!slash_options.with(Vec::is_empty)).then(|| format!("slash-option-{}", slash_index.get()))
                    title="Enter to send or queue; Ctrl/⌘+Enter to steer; Escape to stop; Shift/Alt+Enter for a newline; Up/Down for history; Right Arrow accepts a hint at the end of the input; paste/drop images; @ for file references"
                    node_ref=input_ref
                    placeholder=move || {
                        if inline_hint.get().is_some() && caret_at_end.get() {
                            ""
                        } else if let Some((_, name)) = awaiting_step.get() {
                            if name == "write_file" {
                                "? Tool awaiting approval: press [Alt+Y]es, [Alt+N]o, or [Alt+A]uto-accept edits..."
                            } else {
                                "? Tool awaiting approval: press [Alt+Y]es, or [Alt+N]o..."
                            }
                        } else if streaming.get() {
                            "Queue a follow-up…"
                        } else if has_session.get() {
                            "Ask a question or /command"
                        } else {
                            "Start a chat or /help"
                        }
                    }
                    on:paste=move |event: web_sys::ClipboardEvent| { if let Some(files) = event.clipboard_data().and_then(|data| data.files()) && files.length() > 0 { event.prevent_default(); prompt_composer.import(files); } }
                    on:compositionstart=move |_| composing.set(true)
                    on:compositionend=move |_| { composing.set(false); update_caret(); }
                    on:select=move |_| update_caret()
                    on:scroll=move |_| { if let Some(input) = input_ref.get_untracked() { input_scroll.set((input.scroll_left(), input.scroll_top())); } }
                    on:click=move |_| { update_caret(); prompt_composer.update(); }
                    on:keyup=move |event: web_sys::KeyboardEvent| { update_caret(); if !["ArrowUp", "ArrowDown", "Escape", "Enter", "Tab"].contains(&event.key().as_str()) { prompt_composer.update(); } }
                    on:input=move |e: web_sys::Event| {
                        if let Some(target) = e.target()
                            && let Some(textarea) = target.dyn_ref::<web_sys::HtmlTextAreaElement>()
                        {
                            set_draft.set(textarea.value());
                            update_caret();
                            slash_index.set(0);
                            slash_dismissed.set(None);
                            prompt_composer.update();
                        }
                    }
                    on:keydown={
                        let submit = submit_or_command;
                        move |e: leptos::ev::KeyboardEvent| {
                            if e.is_composing() { return; }
                            if !e.ctrl_key() && !e.meta_key() && !e.alt_key() && !e.shift_key() {
                                let options = slash_options.get_untracked();
                                if !options.is_empty() {
                                    match e.key().as_str() {
                                        "ArrowDown" => { e.prevent_default(); slash_index.update(|index| *index=(*index+1)%options.len()); return; },
                                        "ArrowUp" => { e.prevent_default(); slash_index.update(|index| *index=(*index+options.len()-1)%options.len()); return; },
                                        "Escape" => { e.prevent_default(); slash_dismissed.set(Some(draft.get_untracked())); return; },
                                        "Tab" | "Enter" if e.key() == "Tab" || !options.iter().any(|info| info.command == draft.get_untracked()) => {
                                            e.prevent_default(); complete_slash(options[slash_index.get_untracked().min(options.len()-1)]); return;
                                        },
                                        _ => {},
                                    }
                                }
                            }
                            if prompt_composer.key(&e) { return; }
                            let key = e.key();
                            if key == "ArrowRight" && !e.ctrl_key() && !e.meta_key() && !e.alt_key() && !e.shift_key()
                                && let Some(hint) = inline_hint.get_untracked()
                                && let Some(input) = input_ref.get_untracked()
                            {
                                let value = input.value();
                                let end = u32::try_from(value.encode_utf16().count()).ok();
                                if value == draft.get_untracked()
                                    && input.selection_start().ok().flatten() == end
                                    && input.selection_end().ok().flatten() == end
                                {
                                    e.prevent_default();
                                    set_draft.set(hint.clone());
                                    history_index.set(None);
                                    input.set_value(&hint);
                                    let end = u32::try_from(hint.encode_utf16().count()).unwrap_or(u32::MAX);
                                    let _ = input.set_selection_range(end, end);
                                    update_caret();
                                    prompt_composer.update();
                                    return;
                                }
                            }
                            // Intercept permission handshake if waiting for approval
                            if let Some((id, name)) = awaiting_step.get()
                                && e.alt_key() && !e.ctrl_key() && !e.meta_key()
                            {
                                let code = e.code();
                                if code == "KeyY" {
                                    e.prevent_default();
                                    on_permission.run((id, true));
                                    return;
                                }
                                if code == "KeyN" {
                                    e.prevent_default();
                                    on_permission.run((id, false));
                                    return;
                                }
                                if code == "KeyA" && name == "write_file" {
                                    e.prevent_default();
                                    on_permission_always.run(id);
                                    return;
                                }
                            }

                            if key == "Enter" && (e.ctrl_key() || e.meta_key()) && !e.shift_key() && streaming.get_untracked() {
                                e.prevent_default();
                                if let Some(actions) = queue_actions && !chat.queue_busy.get_untracked() && !chat.reading_images.get_untracked() && chat.queue_edit.get_untracked().is_none() && (!draft.get_untracked().trim().is_empty() || !chat.prompt_images.with_untracked(Vec::is_empty)) { actions.steer.run(()); }
                                return;
                            }

                            // Enter submits
                            if key == "Enter" && !e.shift_key() && !e.alt_key() {
                                e.prevent_default();
                                submit();
                                return;
                            }

                            // Readline history navigation with Up/Down
                            if key == "ArrowUp" {
                                let hist = prompt_history.get();
                                if !hist.is_empty() {
                                    let is_at_start = input_ref.get()
                                        .and_then(|el| el.selection_start().ok().flatten())
                                        .unwrap_or(0) == 0;
                                    if is_at_start || draft.get().is_empty() {
                                        e.prevent_default();
                                        match history_index.get() {
                                            None => {
                                                draft_backup.set(draft.get());
                                                let last_idx = hist.len() - 1;
                                                history_index.set(Some(last_idx));
                                                set_draft.set(hist[last_idx].clone());
                                            }
                                            Some(idx) if idx > 0 => {
                                                let prev = idx - 1;
                                                history_index.set(Some(prev));
                                                set_draft.set(hist[prev].clone());
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                return;
                            }

                            if key == "ArrowDown" {
                                let hist = prompt_history.get();
                                if let Some(idx) = history_index.get() {
                                    e.prevent_default();
                                    if idx + 1 < hist.len() {
                                        let next = idx + 1;
                                        history_index.set(Some(next));
                                        set_draft.set(hist[next].clone());
                                    } else {
                                        history_index.set(None);
                                        set_draft.set(draft_backup.get());
                                    }
                                }
                                return;
                            }

                            // Esc detaches context pill if draft is empty, or cancels run if streaming
                            if key == "Escape" {
                                if chat.active_editor_context.get().is_some() && draft.get().trim().is_empty() {
                                    e.prevent_default();
                                    chat.active_editor_context.set(None);
                                    return;
                                }
                                if streaming.get() {
                                    e.prevent_default();
                                    on_stop.run(());
                                    return;
                                }
                            }

                            // Ctrl+C cancels streaming if no text selected
                            if (e.ctrl_key() || e.meta_key()) && key == "c" && streaming.get() {
                                let has_selection = input_ref.get()
                                    .map(|el| {
                                        let s = el.selection_start().ok().flatten().unwrap_or(0);
                                        let end = el.selection_end().ok().flatten().unwrap_or(0);
                                        end > s
                                    })
                                    .unwrap_or(false);
                                if !has_selection {
                                    e.prevent_default();
                                    on_stop.run(());
                                }
                            }
                        }
                    }
                />
                <Show when=move || inline_hint.get().is_some() && caret_at_end.get()>
                    <div class="tui-inline-hint" aria-hidden="true"><div style=move || { let (left, top) = input_scroll.get(); format!("transform:translate({}px,{}px)",-left,-top) }><span class="tui-hint-prefix">{move || draft.get()}</span>{move || inline_hint.get().and_then(|hint| hint.strip_prefix(&draft.get()).map(str::to_owned))}</div></div>
                    <span class="sr-only" id="composer-inline-hint">{move || inline_hint.get().map(|hint| format!("Suggestion: {hint}. Press Right Arrow to accept."))}</span>
                </Show>
                </div>

                <div class="tui-composer-actions">
                <crate::prompt::PromptControls composer=prompt_composer/>
                <Show
                    when=move || streaming.get()
                    fallback=move || {
                        let submit = submit_or_command;
                        view! {
                            <button
                                class="btn send tui-btn-send ui-icon"
                                title="Send (Enter)"
                                aria-label=move || if chat.queue_edit.get().is_some() { "Save queued prompt" } else if chat.prompt_edit.get().is_some() { "Send edit" } else { "Send" }
                                disabled=move || submission_blocked.get() || (draft.with(|d| d.trim().is_empty()) && chat.prompt_images.with(Vec::is_empty))
                                on:click=move |_| submit()
                            >
                                <super::ui::Icon name=super::ui::IconName::ArrowUp /><span class="sr-only">{move || if chat.queue_edit.get().is_some() { "Save queued prompt" } else if chat.prompt_edit.get().is_some() { "Send edit" } else { "Send" }}</span>
                            </button>
                        }
                    }
                >
                    {queue_actions.map(|actions| view! {
                        <button class="btn send tui-btn-queue ui-icon" title="Queue follow-up (Enter)" aria-label="Queue follow-up" disabled=move || chat.queue_busy.get() || chat.reading_images.get() || chat.rewinding.get() || (draft.with(|draft| draft.trim().is_empty()) && chat.prompt_images.with(Vec::is_empty)) on:click=move |_| actions.enqueue.run(())><super::ui::Icon name=super::ui::IconName::ArrowUp /><span class="sr-only">{move || if chat.queue_edit.get().is_some() { "Save queued prompt" } else { "Queue" }}</span></button>
                        <button class="btn ghost tui-btn-steer ui-icon" aria-label="Steer" title="Steer: stop and send this guidance first (Ctrl/⌘+Enter)" disabled=move || chat.queue_busy.get() || chat.reading_images.get() || chat.queue_edit.get().is_some() || (draft.with(|draft| draft.trim().is_empty()) && chat.prompt_images.with(Vec::is_empty)) on:click=move |_| actions.steer.run(())><super::ui::Icon name=super::ui::IconName::CornerUpLeft /><span class="sr-only">"Steer"</span></button>
                    })}
                    <button class="btn stop tui-btn-stop ui-icon" title="Stop (Escape)" aria-label="Stop" on:click=move |_| on_stop.run(())><super::ui::Icon name=super::ui::IconName::Square /><span class="sr-only">"Stop"</span></button>
                </Show>
                </div>
            </div>
            <super::chat_details::ChatDetails/>
        </main>
    }
}

#[component]
fn PromptQueueControls(
    actions: crate::state_actions::prompt_queue::PromptQueueActions,
) -> impl IntoView {
    let chat = expect_context::<ChatState>();
    view! {
        <Show when=move || !chat.queued_prompts.with(Vec::is_empty) || chat.queue_edit.get().is_some()>
            <section class="tui-prompt-queue" aria-label="Queued prompts">
                <div class="tui-queue-heading"><strong>"Queued prompts"</strong>
                    <button class="btn ghost tui-queue-toggle" disabled=move || chat.queue_busy.get() || chat.queue_loading.get() || chat.queue_edit.get().is_some() on:click=move |_| actions.toggle.run(())>{move || if chat.active_session.get().is_some_and(|session| chat.queue_running.with(|sessions| sessions.contains(&session))) { "Pause queue" } else { "Run queue" }}</button>
                    <Show when=move || chat.queue_edit.get().is_some()><button class="btn ghost" on:click=move |_| actions.cancel_edit.run(())>"Cancel edit"</button></Show>
                </div>
                <For each=move || chat.queued_prompts.get() key=|prompt| (prompt.id, prompt.revision) children=move |prompt| {
                    let key = prompt.key();
                    let host_delivered = prompt.is_host_delivered();
                    let content = openwebide_core::PromptContent::decode(&prompt.content);
                    let text = extract_editor_context_prelude(&content.text).1.chars().take(200).collect::<String>();
                    let label = if text.trim().is_empty() { format!("{} image(s)", content.images.len()) } else if content.images.is_empty() { text } else { format!("{text} · {} image(s)", content.images.len()) };
                    view! { <div class="tui-queued-prompt" data-queue-id=prompt.id>
                        <span class="tui-queue-kind">{if prompt.plugin_run.is_some() {"Plugin"} else if prompt.scheduled_task.is_some() {"Scheduled"} else if prompt.guidance { "Guidance" } else { "Next" }}</span><span class="tui-queue-label">{label}</span>
                        <button class="btn ghost" disabled=move || host_delivered || chat.queue_busy.get() || chat.queue_delivering.get().is_some_and(|(_, delivering)| delivering == key) on:click=move |_| actions.edit.run(key)>"Edit"</button>
                        <button class="btn ghost" disabled=move || chat.queue_busy.get() || chat.queue_delivering.get().is_some_and(|(_, delivering)| delivering == key) on:click=move |_| actions.remove.run(key)>"Remove"</button>
                    </div> }
                } />
            </section>
        </Show>
    }
}

fn render_task_run(
    task: Memo<openwebide_core::TaskSnapshot>,
    awaiting_step: Memo<Option<(String, String)>>,
    on_permission: Callback<(String, bool)>,
    on_permission_always: Callback<String>,
) -> AnyView {
    let expanded = RwSignal::new(false);
    let timing = Memo::new(move |_| task.with(|task| task.task.timing));
    let status = Memo::new(move |_| {
        task.with(|task| match task.task.status {
            openwebide_core::TaskStatus::Queued => "Queued",
            openwebide_core::TaskStatus::Running => "Running",
            openwebide_core::TaskStatus::WaitingForApproval => "Waiting for approval",
            openwebide_core::TaskStatus::Completed => "Completed",
            openwebide_core::TaskStatus::Failed => "Failed",
            openwebide_core::TaskStatus::Cancelled => "Cancelled",
        })
    });
    let active_approval = Memo::new(move |_| task.with(|task| task.pending_permission().is_some()));
    let live_text = Memo::new(move |_| {
        task.with(|task| {
            if let Some(openwebide_core::RunEvent::Done { message }) = &task.run.finished {
                message.content.clone()
            } else if task.run.text.is_empty() && !task.run.reasoning.is_empty() {
                format!(
                    "{}{}",
                    openwebide_core::ESCAPED_REASONING_OPEN,
                    openwebide_core::escape_reasoning(&task.run.reasoning)
                )
            } else {
                openwebide_core::with_reasoning(&task.run.reasoning, &task.run.text)
            }
        })
    });
    let history = crate::state::chat::ConversationStore::new();
    Effect::new(move |_| {
        let items = task.with(|task| {
            task.run
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| match item {
                    openwebide_core::RunItem::Message(message) => {
                        ConversationItem::Message(message.clone())
                    }
                    openwebide_core::RunItem::Step(step) => ConversationItem::ToolStep {
                        timing: step.timing,
                        key: index as u64,
                        id: step.id.clone(),
                        name: step.name.clone(),
                        summary: step.summary.clone(),
                        result: step.result.as_ref().map(|result| ToolStepResult {
                            ok: result.ok,
                            summary: result.summary.clone(),
                            diff: result.diff.clone(),
                        }),
                        diff: step.diff.as_deref().cloned(),
                        note: step.note.clone(),
                        awaiting_permission: step.awaiting_permission,
                    },
                })
                .collect()
        });
        history.reconcile(move |current| *current = items);
    });
    view! {
        <div class="tui-task-run">
            <button class="btn tui-task-heading" aria-expanded=move || expanded.get() || active_approval.get() on:click=move |_| expanded.update(|value| *value = !*value)>
                <span><super::ui::Icon name=Signal::derive(move || if expanded.get() || active_approval.get() { super::ui::IconName::ChevronDown } else { super::ui::IconName::ChevronRight }) /></span>
                <strong>{move || task.with(|task| task.task.description.clone())}</strong>
                <span class="muted">{status}</span>
                <super::tool_duration::ToolDuration timing=timing title="Child task elapsed time, including approval waiting" />
                <span class="muted">{move || task.with(|task| format!("{}{} tokens · {} tools", if task.usages().iter().any(|usage| usage.estimated) { "~" } else { "" }, task.total_tokens(), task.task.tool_count))}</span>
            </button>
            <div class="tui-task-content" hidden=move || !expanded.get() && !active_approval.get()>
                <For each=move || conversation_blocks(history) key=|handle| (handle.key, handle.item.with(crate::conversation::is_activity)) children=move |handle| {
                    let item = handle.item;
                    match item.get_untracked() {
                        ConversationItem::ToolStep { .. } => render_tool_group(history, handle.key, awaiting_step, on_permission, on_permission_always).into_any(),
                        ConversationItem::Message(ref message) if crate::conversation::is_reasoning_activity(message) => render_tool_group(history, handle.key, awaiting_step, on_permission, on_permission_always).into_any(),
                        ConversationItem::Message(message) => {
                            let content = Memo::new(move |_| item.with(|item| match item { ConversationItem::Message(message) => message.content.clone(), _ => String::new() }));
                            if message.role == Role::User { render_user_message(content, ().into_any()) }
                            else if message.role == Role::System { view! { <super::ui::DisclosurePanel class="tui-thinking-box" toggle_class="tui-thinking-summary" summary=|| view! { <span>"Child context"</span> }><div class="tui-thinking-trace"><pre class="tui-thinking-pre">{content}</pre></div></super::ui::DisclosurePanel> }.into_any() }
                            else { render_assistant_message(content) }
                        }
                        _ => ().into_any(),
                    }
                } />
                {render_assistant_message(live_text)}
                <Show when=move || task.with(|task| matches!(task.task.status, openwebide_core::TaskStatus::Failed | openwebide_core::TaskStatus::Cancelled))>
                    <div class="form-hint">{move || task.with(|task| task.task.result.clone().unwrap_or_default())}</div>
                </Show>
                <For each=move || task.with(|task| task.children.iter().map(|child| child.task.id.clone()).collect::<Vec<_>>()) key=|id| id.clone() children=move |id| {
                    let child = Memo::new(move |_| task.with(|task| task.children.iter().find(|child| child.task.id == id).expect("Child retained in history").clone()));
                    render_task_run(child, awaiting_step, on_permission, on_permission_always)
                } />
            </div>
        </div>
    }.into_any()
}
