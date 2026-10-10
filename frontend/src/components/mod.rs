mod about;
mod git_assistance;
pub use about::About;
mod auth_gate;
mod chat_pane;
mod commands;
mod confirm_dialog;
mod cron;
mod editor;
mod editor_geometry;
mod editor_motion;
mod editor_options;
pub(crate) mod editor_paint;
mod editor_pointer;
mod editor_rows;
mod editor_selections;
mod editor_tabs;
mod file_browser;
mod file_tree;
pub mod host_admin;
pub(crate) mod modal;
mod panel_resizer;
mod plugins;
pub use plugins::{PluginMarketplaceSources, Plugins, PluginsDialog};
mod prompt_dialog;
pub mod scheduled;
mod session_list;
mod settings;
mod sidebar;
mod status_bar;
mod tab_bar;
mod terminal_pane;
mod top_bar;
pub mod ui;

pub use auth_gate::AuthGate;
pub use chat_pane::{ChatPane, ToolStepResult};
pub use commands::{CommandDialogs, Omnibar};
pub use confirm_dialog::ConfirmDialog;
pub use editor::Editor;
#[cfg(feature = "test-support")]
pub use editor::{
    bounded_paragraph_matches_complete, bounded_wrapped_matches_complete, highlight_count,
    retained_paragraph_matches_complete, take_highlight_segment_bytes, take_highlight_source_bytes,
    take_paragraph_prefix_reconciliations, take_paragraph_suffix_probes, viewport_highlight_count,
    wrapped_coverage_matches_complete,
};
#[cfg(feature = "test-support")]
pub use editor_geometry::{
    cooperative_geometry_matches_complete_and_cancels,
    cooperative_text_nodes_match_complete_and_cancel, paragraph_rectangles_match_individual,
};
pub use file_browser::FileBrowser;
pub use file_tree::{FileTree, SearchPane};
pub use panel_resizer::PanelResizer;
pub use prompt_dialog::PromptDialog;
pub use session_list::SessionList;
pub use settings::Settings;
pub use sidebar::Sidebar;
pub use status_bar::StatusBar;
pub use tab_bar::TabBar;
pub use terminal_pane::{TerminalDock, TerminalPane};
pub use top_bar::TopBar;
pub use ui::{Button, ButtonSize, ButtonVariant};

pub mod model_setup;

mod approval_mode;

pub mod model_wizard;

mod tool_panel;
pub use tool_panel::{FilesPanel, PanelRail, ToolPanel};

mod run_changes;
pub use run_changes::RunChangesPanel;

mod chat_details;
mod context_usage;
pub use context_usage::ContextUsage;

mod todo_plan;

mod tool_output;

mod tool_duration;

mod turn_summary;

mod install_app;

mod git_pane;
pub use git_pane::GitPane;
pub use install_app::InstallApp;

pub mod branch_picker;
pub use branch_picker::BranchPicker;

pub mod dropdown;

pub use modal::Modal;

pub mod context_menu;

mod editor_recovery;

mod tab_actions;

pub mod goal;

pub mod tool_budget;

mod configuration;
pub use configuration::Configuration;

pub mod editor_chrome;

pub mod memories;
pub mod skills;

pub mod questions;

mod monitors;

pub use monitors::ConversationMonitors;

pub mod git_changes;
pub mod git_history;
