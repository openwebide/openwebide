pub mod auth;
pub mod chat;
pub mod git;
pub mod layout;
pub mod projects;
#[cfg(target_arch = "wasm32")]
pub mod scheduled;
pub mod sessions;
pub mod settings;
pub mod slash;
pub mod ui;
pub mod workspace;

pub mod reviews;

pub mod responsive;

pub mod editor_recovery;

pub mod editor_motion;

pub mod memories;
pub mod plugins;
pub mod skills;

#[cfg(target_arch = "wasm32")]
pub mod questions;

#[cfg(target_arch = "wasm32")]
pub mod monitors;

pub mod plugin_panels;
