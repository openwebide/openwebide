#[cfg(target_arch = "wasm32")]
pub mod api;
#[cfg(target_arch = "wasm32")]
pub mod app;
#[cfg(target_arch = "wasm32")]
pub mod backend;
#[cfg(target_arch = "wasm32")]
pub mod bridge;
pub mod bridge_address;
#[cfg(target_arch = "wasm32")]
pub mod browser_preferences;
pub mod commands;
#[cfg(target_arch = "wasm32")]
pub mod components;
pub mod conversation;
#[cfg(target_arch = "wasm32")]
pub mod editor_worker;
pub mod git_status;
pub mod git_timeline;
pub mod history;
#[cfg(target_arch = "wasm32")]
pub mod host_admin;
#[cfg(target_arch = "wasm32")]
pub mod idb;
#[cfg(target_arch = "wasm32")]
pub mod local_agent;
#[cfg(target_arch = "wasm32")]
pub mod local_fs;
pub mod markdown;
#[cfg(target_arch = "wasm32")]
pub mod model_setup;
#[cfg(target_arch = "wasm32")]
pub mod monitors;
pub mod notifications;
pub mod pending;
#[cfg(target_arch = "wasm32")]
pub mod plugin_actions;
#[cfg(target_arch = "wasm32")]
pub mod plugin_bridge;
#[cfg(target_arch = "wasm32")]
pub mod project_git;
#[cfg(target_arch = "wasm32")]
pub mod project_host;
#[cfg(target_arch = "wasm32")]
pub mod project_memory;
#[cfg(target_arch = "wasm32")]
pub mod project_plugins;
#[cfg(target_arch = "wasm32")]
pub mod project_runs;
pub mod project_setup;
#[cfg(target_arch = "wasm32")]
pub mod project_skills;
#[cfg(target_arch = "wasm32")]
pub mod scheduled;
pub mod sse;
pub mod state;
#[cfg(target_arch = "wasm32")]
pub mod state_actions;
pub mod tabs;
pub mod terminal_output;
#[cfg(all(target_arch = "wasm32", any(test, feature = "test-support")))]
pub mod testing;
pub mod text;
pub mod tool_output;
pub mod turn_summary;
#[cfg(target_arch = "wasm32")]
pub mod util;
#[cfg(target_arch = "wasm32")]
pub mod web_push;
#[cfg(target_arch = "wasm32")]
pub mod workspace;

#[cfg(target_arch = "wasm32")]
pub fn mount() {
    std::panic::set_hook(Box::new(|info| {
        web_sys::console::error_1(&info.to_string().into());
    }));
    if editor_worker::mount_worker() {
        return;
    }
    editor_worker::enable();
    leptos::mount::mount_to_body(app::App);
}

/// Parse the bridge's project-relative probe path.
pub fn parse_probe_output(stdout: &str, nonce: &str) -> Option<String> {
    let probe = format!(".openwebide-probe-{nonce}");
    let path = stdout.trim_end_matches(['\n', '\r']).strip_prefix("./")?;
    if path == probe {
        return Some(String::new());
    }
    let cwd = path.strip_suffix(&format!("/{probe}"))?;
    if cwd.is_empty()
        || cwd
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || cwd.contains(['\n', '\r'])
    {
        return None;
    }
    Some(cwd.to_string())
}

#[cfg(test)]
mod probe_tests {
    use super::parse_probe_output;

    #[test]
    fn probe_paths() {
        assert_eq!(
            parse_probe_output("./repos/x/.openwebide-probe-ab12\n", "ab12"),
            Some("repos/x".into())
        );
        assert_eq!(
            parse_probe_output("./.openwebide-probe-ab12", "ab12"),
            Some(String::new())
        );
        for output in [
            "",
            "unrelated",
            "./repos/x/.openwebide-probe-other",
            "./../.openwebide-probe-ab12",
            "./x\n/y/.openwebide-probe-ab12",
        ] {
            assert_eq!(parse_probe_output(output, "ab12"), None);
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub mod prompt;

pub mod viewport;

#[cfg(target_arch = "wasm32")]
pub mod clipboard;

pub mod omnibar;
