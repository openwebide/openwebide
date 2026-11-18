use super::dropdown::{DropdownSelect, SelectOption};
use crate::state::git::GitState;
use leptos::prelude::*;

/// The same shared branch selector in Changes and the footer.
#[component]
pub fn BranchPicker(
    #[prop(default = Callback::new(|()| ()))] on_load: Callback<()>,
    #[prop(default = Callback::new(|_: String| ()))] on_select: Callback<String>,
    #[prop(default = Callback::new(|()| ()))] on_new: Callback<()>,
    #[prop(default = false)] above: bool,
) -> impl IntoView {
    let git = expect_context::<GitState>();
    let current = Signal::derive(move || {
        git.status.with(|status| {
            format!(
                "branch:{}",
                status
                    .as_ref()
                    .map(|status| status.branch.as_str())
                    .unwrap_or_default()
            )
        })
    });
    let options = Signal::derive(move || {
        let branch = git.status.with(|status| {
            status
                .as_ref()
                .map(|status| status.branch.clone())
                .unwrap_or_default()
        });
        let mut options = vec![SelectOption::new(
            format!("branch:{branch}"),
            if branch.is_empty() {
                "Branch unavailable".to_owned()
            } else {
                branch.clone()
            },
        )];
        options.extend(
            git.branches
                .get()
                .into_iter()
                .filter(|item| item.name != branch)
                .map(|item| SelectOption::new(format!("branch:{}", item.name), item.name)),
        );
        if git.branches_error.get().is_some() {
            options.push(SelectOption {
                value: "error".into(),
                label: "Could not load branches — reopen to retry".into(),
                disabled: true,
            });
        }
        options.push(SelectOption::new("new", "New branch…"));
        options
    });
    view! { <DropdownSelect label="Git branch" class="branch-picker" trigger_class="btn ghost git-branch-select" value=current options=options above=above ready=Signal::derive(move || !git.branches_loading.get() || git.branches.with(|branches| !branches.is_empty())) on_open=on_load disabled=Signal::derive(move || git.branch_busy.get() || git.status.get().is_none())
    on_change=Callback::new(move |value: String| { if value == "new" { on_new.run(()); } else if let Some(branch) = value.strip_prefix("branch:") && value != current.get_untracked() { on_select.run(branch.to_string()); } }) /> }
}
