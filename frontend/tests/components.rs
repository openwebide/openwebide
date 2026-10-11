#![cfg(target_arch = "wasm32")]

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

#[path = "components/assistance.rs"]
mod assistance;
#[path = "components/models.rs"]
mod models;
#[path = "components/pending_edits.rs"]
mod pending_edits;
#[path = "components/permissions.rs"]
mod permissions;
#[path = "components/scheduled.rs"]
mod scheduled;
#[path = "components/streaming.rs"]
mod streaming;
#[path = "components/support.rs"]
mod support;

#[path = "components/terminal.rs"]
mod terminal;

#[path = "components/runs.rs"]
mod runs;

#[path = "components/local_bridge.rs"]
mod local_bridge;

#[path = "components/persistence.rs"]
mod persistence;

#[path = "components/editor.rs"]
mod editor;

#[path = "components/search.rs"]
mod search;

#[path = "components/tree_refresh.rs"]
mod tree_refresh;

#[path = "components/idb.rs"]
mod idb;

#[path = "components/ui.rs"]
mod ui;

#[path = "components/modals.rs"]
mod modals;

#[path = "components/recent_projects.rs"]
mod recent_projects;

#[path = "components/composer.rs"]
mod composer;

#[path = "components/project_git.rs"]
mod project_git;

#[path = "components/panels.rs"]
mod panels;

#[path = "components/queue.rs"]
mod queue;

#[path = "components/branches.rs"]
mod branches;

#[path = "components/notifications.rs"]
mod notifications;

#[path = "components/todos.rs"]
mod todos;

#[path = "components/tool_steps.rs"]
mod tool_steps;

#[path = "components/commands.rs"]
mod commands;

#[path = "components/sessions.rs"]
mod sessions;

#[path = "components/installation.rs"]
mod installation;

#[path = "components/file_tree.rs"]
mod file_tree;

#[path = "components/chat_controls.rs"]
mod chat_controls;

#[path = "components/tool_budgets.rs"]
mod tool_budgets;

#[path = "components/omnibar.rs"]
mod omnibar;

#[path = "components/memories.rs"]
mod memories;
#[path = "components/skills.rs"]
mod skills;

#[path = "components/chat_details.rs"]
mod chat_details;

#[path = "components/host_admin.rs"]
mod host_admin;

#[path = "components/git_history.rs"]
mod git_history;
#[path = "components/monitors.rs"]
mod monitors;
#[path = "components/questions.rs"]
mod questions;

#[path = "components/git_changes.rs"]
mod git_changes;

#[path = "components/review_regressions.rs"]
mod review_regressions;

#[path = "components/plugins.rs"]
mod plugins;

#[path = "components/release_notes.rs"]
mod release_notes;

#[path = "components/mcp.rs"]
mod mcp;
