use std::collections::{HashMap, HashSet};

use leptos::prelude::*;
use openwebide_core::{FileEntry, vfs::SearchOptions};
use web_sys::wasm_bindgen::JsCast;

use crate::state::{
    git::GitState, layout::LayoutState, projects::ProjectsState, workspace::WorkspaceState,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct TreeRow {
    entry: FileEntry,
    depth: u32,
    parent: String,
    nested: bool,
}

/// A collapsible project file tree with directory contents loaded lazily and cached in
/// `entries` (keyed by directory path, root = "").
#[component]
pub fn FileTree(
    on_toggle: Callback<String>,
    on_open: Callback<String>,
    #[prop(optional)] on_grant_access: Option<Callback<()>>,
) -> impl IntoView {
    let workspace = expect_context::<WorkspaceState>();
    let projects = expect_context::<ProjectsState>();
    let layout = expect_context::<LayoutState>();

    let root = NodeRef::<leptos::html::Div>::new();
    let tree_actions = use_context::<crate::state_actions::file_tree::FileTreeActions>();
    let entries = workspace.entries.read_only();
    let expanded = workspace.expanded.read_only();
    let tree_width = layout.tree_width.read_only();
    let needs_grant = Signal::derive(move || {
        projects
            .active_project
            .get()
            .is_some_and(|id| projects.needs_grant.with(|ids| ids.contains(&id)))
    });

    // Flatten the (lazily loaded) tree into a list of (entry, depth) pairs,
    // following only expanded directories. Recomputes when `entries` or
    // `expanded` change. A flat list avoids a recursive component, which
    // would otherwise create a recursive opaque return type.
    let flat = Memo::new(move |_| {
        let mut map = entries.get();
        for children in map.values_mut() {
            openwebide_core::vfs::sort_file_entries(children);
            if !layout.preferences.get().include_hidden {
                children.retain(|entry| {
                    !entry.name.starts_with('.')
                        || tree_actions.is_some_and(|actions| {
                            actions.reveal_target.get().is_some_and(|path| {
                                path == entry.path || path.starts_with(&format!("{}/", entry.path))
                            })
                        })
                });
            }
        }
        let exp = expanded.get();
        let mut result = Vec::new();
        fn traverse(
            map: &HashMap<String, Vec<FileEntry>>,
            exp: &HashSet<String>,
            dir: &str,
            depth: u32,
            result: &mut Vec<TreeRow>,
        ) {
            if let Some(children) = map.get(dir) {
                let parents = openwebide_core::file_nesting::parents(children);
                let mut groups: HashMap<&str, Vec<&FileEntry>> = HashMap::new();
                for child in children {
                    let parent = parents.get(&child.path).map_or(dir, String::as_str);
                    groups.entry(parent).or_default().push(child);
                }
                append_group(map, exp, &groups, dir, depth, result);
            }
        }
        fn append_group(
            map: &HashMap<String, Vec<FileEntry>>,
            exp: &HashSet<String>,
            groups: &HashMap<&str, Vec<&FileEntry>>,
            parent: &str,
            depth: u32,
            result: &mut Vec<TreeRow>,
        ) {
            if let Some(children) = groups.get(parent) {
                for child in children {
                    let nested = groups.contains_key(child.path.as_str());
                    result.push(TreeRow {
                        entry: (*child).clone(),
                        depth,
                        parent: parent.to_string(),
                        nested,
                    });
                    if exp.contains(&child.path) {
                        if child.is_dir {
                            traverse(map, exp, &child.path, depth + 1, result);
                        } else if nested {
                            append_group(map, exp, groups, &child.path, depth + 1, result);
                        }
                    }
                }
            }
        }
        traverse(&map, &exp, "", 0, &mut result);
        result
    });
    let row_details = Memo::new(move |_| {
        flat.with(|rows| {
            rows.iter()
                .map(|row| {
                    (
                        row.entry.path.clone(),
                        (row.depth, row.parent.clone(), row.nested),
                    )
                })
                .collect::<HashMap<_, _>>()
        })
    });

    Effect::new(move |_| {
        entries.track();
        let Some(actions) = tree_actions else {
            return;
        };
        let Some(path) = actions.reveal_target.get() else {
            return;
        };
        let parents = entries.with(|map| {
            map.get(openwebide_core::workspace_entries::parent(&path))
                .map(|children| openwebide_core::file_nesting::parents(children))
                .unwrap_or_default()
        });
        let mut nested_parents = Vec::new();
        let mut child = path.as_str();
        while let Some(parent) = parents.get(child) {
            nested_parents.push(parent.clone());
            child = parent;
        }
        if workspace.expanded.with_untracked(|expanded| {
            nested_parents
                .iter()
                .any(|parent| !expanded.contains(parent))
        }) {
            workspace
                .expanded
                .update(|expanded| expanded.extend(nested_parents));
        }
        let epoch = actions.epoch.get();
        request_animation_frame(move || {
            if actions.epoch.try_get_untracked() != Some(epoch)
                || actions.reveal_target.try_get_untracked() != Some(Some(path.clone()))
            {
                return;
            }
            let Some(root) = root.get_untracked() else {
                return;
            };
            let Ok(rows) = root.query_selector_all("[data-tree-path]") else {
                return;
            };
            for index in 0..rows.length() {
                if let Some(row) = rows
                    .item(index)
                    .and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
                    && row.get_attribute("data-tree-path").as_deref() == Some(&path)
                {
                    row.scroll_into_view();
                    let _ = row.focus();
                    actions.reveal_target.set(None);
                    break;
                }
            }
        });
    });

    view! {
        <div
            class="file-tree" node_ref=root
            class:compact-tree=move || !layout.phone.get()
            style=move || format!("width: {}px; flex: none;", tree_width.get())
        >
            <Show when=move || needs_grant.get() fallback=|| ()>
                <div style="padding: 12px; text-align: center;">
                    <p>"This browser needs permission to access the project folder. Grant access or select the folder again to reconnect it."</p>
                    <crate::components::Button
                        variant=crate::components::ButtonVariant::Primary
                        size=crate::components::ButtonSize::Sm
                        on_click=Callback::new(move |_| {
                            if let Some(cb) = &on_grant_access {
                                cb.run(());
                            }
                        })
                    >
                        "Grant folder access"
                    </crate::components::Button>
                </div>
            </Show>
                        <div class="tree-root" role="tree" aria-label="Project files">
                            <For
                                each=move || flat.get()
                                key=move |row| (projects.active_project.get_untracked(), row.entry.path.clone(), row.entry.is_dir)
                                children=move |row| {
                                    let path = StoredValue::new(row.entry.path.clone());
                                    let details = Signal::derive(move || row_details.with(|rows| rows.get(&path.get_value()).cloned().unwrap_or_default()));
                                    view! { <FileTreeEntry entry=row.entry depth=Signal::derive(move || details.get().0) tree_parent=Signal::derive(move || details.get().1) nested=Signal::derive(move || details.get().2) on_toggle=on_toggle on_open=on_open /> }

                                }
                            />
                        </div>

        </div>
    }
}

/// Project search shares the workspace search state and actions in both modes.
#[component]
pub fn SearchPane(
    on_open: Callback<String>,
    on_search_input: Callback<(String, SearchOptions)>,
    on_cancel_search: Callback<()>,
    on_search: Callback<(String, SearchOptions)>,
    include_ignored: ReadSignal<bool>,
    on_toggle_include_ignored: Callback<()>,
    on_clear_search: Callback<()>,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    on_cleanup(move || on_cancel_search.run(()));
    let workspace = expect_context::<WorkspaceState>();
    let search_results = workspace.search.read_only();
    let open_file = workspace.open_file.read_only();
    let query = RwSignal::new(String::new());
    let search_input = NodeRef::<leptos::html::Input>::new();
    let layout = expect_context::<crate::state::layout::LayoutState>();
    Effect::new(move |_| {
        workspace.active_project.track();
        query.set(String::new());
        if let Some(input) = search_input.get() {
            input.set_value("");
        }
    });
    let searching = Signal::derive(move || {
        layout
            .preferences
            .with(|prefs| prefs.files_view == crate::state::responsive::FilesView::Search)
    });
    let actions = expect_context::<crate::state_actions::layout::LayoutActions>();
    Effect::new(move |_| {
        if searching.get() {
            leptos::leptos_dom::helpers::queue_microtask(move || {
                if searching.try_get_untracked() == Some(true)
                    && let Some(Some(input)) = search_input.try_get_untracked()
                {
                    let _ = input.focus();
                }
            });
        } else {
            on_cancel_search.run(());
        }
    });
    view! { <div class="search-pane">

            <div id="project-search" hidden=move || !searching.get()>
            <super::ui::PanelSearchRow class="file-tree-search">
                <input
                    type="text"
                    class="form-input panel-search-input search-input"
                    aria-label="Search project files"
                    placeholder="Search files…"
                    node_ref=search_input
                    on:keydown=move |event: web_sys::KeyboardEvent| {
                        if event.key() == "Escape" && !event.is_composing() {
                            event.prevent_default();
                            event.stop_propagation();
                            on_clear_search.run(());
                            query.set(String::new());
                            if let Some(input) = search_input.get() { input.set_value(""); }
                            actions.select_files_view.run(crate::state::responsive::FilesView::Explorer);
                            leptos::leptos_dom::helpers::queue_microtask(move || {
                                if searching.try_get_untracked() == Some(false)
                                    && let Some(button) = document().query_selector("[data-file-search-toggle]").ok().flatten().and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok()) { let _ = button.focus(); }
                            });
                        }
                    }
                    on:input=move |e: web_sys::Event| {
                        if let Some(target) = e.target()
                            && let Some(input) = target.dyn_ref::<web_sys::HtmlInputElement>()
                        {
                            let q = input.value();
                            query.set(q.clone());
                            if q.is_empty() {
                                on_clear_search.run(());
                            } else {
                                on_search_input.run((
                                    q,
                                    SearchOptions {
                                        include_ignored: include_ignored.get(),
                                    },
                                ));
                            }
                        }
                    }
                />
                <button
                    class="icon-btn"
                    title="Include ignored folders (.git, target, node_modules, dist)"
                    aria-pressed=move || include_ignored.get().to_string()
                    on:click=move |_| {
                        on_toggle_include_ignored.run(());
                        if let Some(input) = search_input.get() {
                            let q = input.value();
                            if !q.is_empty() {
                                on_search.run((
                                    q,
                                    SearchOptions {
                                        include_ignored: include_ignored.get(),
                                    },
                                ));
                            }
                        }
                    }
                >
                    <crate::components::ui::Icon name=crate::components::ui::IconName::FolderSearch />
                </button>
            </super::ui::PanelSearchRow>
            </div>
                <div class="search-file-views" hidden=move || searching.get() && !query.get().is_empty()>{children.map(|children| children())}</div>
                <div class="tree-root search-results" hidden=move || !searching.get() || query.get().is_empty()>
                    <For
                        each=move || search_results.get().unwrap_or_default()
                        key=|e| format!("{}:{}", e.path, e.line)
                        children=move |e| {
                            let path_class = e.path.clone();
                            let path_click = e.path.clone();
                            let path = e.path.clone();
                            let line = e.line;
                            let text = e.text.clone();
                            view! {
                                <div
                                    class=move || {
                                        if open_file.get().as_deref() == Some(path_class.as_str()) {
                                            "tree-item selected".to_string()
                                        } else {
                                            "tree-item".to_string()
                                        }
                                    }
                                    on:click=move |_| on_open.run(path_click.clone())
                                >
                                    <span class="tree-hit-path">{path}</span>
                                    <span class="tree-hit-line">{line}</span>
                                    <span class="tree-hit-text">{text}</span>
                                </div>
                            }
                        }
                    />
                </div>
    </div> }
}

/// Explorer creation actions can share a toolbar with the Files view switcher.
#[component]
pub fn FileActions(on_new_file: Callback<()>, on_new_dir: Callback<()>) -> impl IntoView {
    let layout = expect_context::<LayoutState>();
    let layout_actions = use_context::<crate::state_actions::layout::LayoutActions>();
    let workspace_actions = use_context::<crate::state_actions::workspace::WorkspaceActions>();
    let tree_actions = use_context::<crate::state_actions::file_tree::FileTreeActions>();
    view! { <super::dropdown::ActionMenu aria_label="File actions">
        <h3 class="ui-menu-heading">"Create"</h3>
        <button role="menuitem" class="ui-dropdown-item recent-item icon-btn" title="New file" on:click=move |_| on_new_file.run(())>
            <crate::components::ui::Icon name=crate::components::ui::IconName::File />
        <span>"New file"</span></button>
        <button role="menuitem" class="ui-dropdown-item recent-item icon-btn" title="New folder" on:click=move |_| on_new_dir.run(())>
            <crate::components::ui::Icon name=crate::components::ui::IconName::Folder />
        <span>"New folder"</span></button>
        <h3 class="ui-menu-heading">"Tree"</h3>
        <button role="menuitem" type="button" class="ui-dropdown-item recent-item" disabled=move || tree_actions.is_none_or(|actions| actions.expanding.get()) on:click=move |_| { if let Some(actions) = tree_actions { actions.expand_all(); } }><crate::components::ui::Icon name=crate::components::ui::IconName::UnfoldVertical /><span>"Expand all"</span></button>
        <button role="menuitem" type="button" class="ui-dropdown-item recent-item" disabled=tree_actions.is_none() on:click=move |_| { if let Some(actions) = tree_actions { actions.collapse_all(); } }><crate::components::ui::Icon name=crate::components::ui::IconName::FoldVertical /><span>"Collapse all"</span></button>

        <button role="menuitem" type="button" class="ui-dropdown-item recent-item" disabled=workspace_actions.is_none() on:click=move |_| { if let Some(actions) = workspace_actions { actions.refresh_tree.run(()); } }><crate::components::ui::Icon name=crate::components::ui::IconName::RefreshCw /><span>"Refresh files"</span></button>
        <button role="menuitemcheckbox" type="button" class="ui-dropdown-item recent-item" aria-checked=move || layout.preferences.with(|prefs| prefs.include_hidden).to_string() disabled=layout_actions.is_none() on:click=move |_| { if let Some(actions) = layout_actions { actions.set_tree_preferences.run(!layout.preferences.get_untracked().include_hidden); } }><crate::components::ui::Icon name=crate::components::ui::IconName::Eye /><span>"Include hidden files and folders"</span></button>
        <Show when=move || layout_actions.is_some()>
            <h3 class="ui-menu-heading">"Panel"</h3>
            <button role="menuitem" type="button" class="ui-dropdown-item recent-item" on:click=move |_| { if let Some(actions) = layout_actions { actions.move_panel.run((crate::state::layout::Panel::Files, false)); } }><crate::components::ui::Icon name=crate::components::ui::IconName::ArrowLeft /><span>"Move panel left"</span></button>
            <button role="menuitem" type="button" class="ui-dropdown-item recent-item" on:click=move |_| { if let Some(actions) = layout_actions { actions.move_panel.run((crate::state::layout::Panel::Files, true)); } }><crate::components::ui::Icon name=crate::components::ui::IconName::ArrowRight /><span>"Move panel right"</span></button>
        </Show>
    </super::dropdown::ActionMenu> }
}

fn tree_rows(target: Option<web_sys::EventTarget>) -> Vec<web_sys::HtmlElement> {
    target
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        .and_then(|target| target.closest(".tree-root").ok().flatten())
        .and_then(|tree| tree.query_selector_all(".tree-item").ok())
        .map_or_else(Vec::new, |rows| {
            (0..rows.length())
                .filter_map(|index| rows.item(index))
                .filter_map(|row| row.dyn_into::<web_sys::HtmlElement>().ok())
                .collect()
        })
}

/// One entry owns its menu and gestures; every action uses the shared facade.
#[component]
pub(super) fn FileTreeEntry(
    entry: FileEntry,
    #[prop(into)] depth: Signal<u32>,
    #[prop(optional, into)] tree_parent: Signal<String>,
    #[prop(optional, into)] nested: Signal<bool>,
    on_toggle: Callback<String>,
    on_open: Callback<String>,
    #[prop(default = false)] changes_only: bool,
) -> impl IntoView {
    use super::ui::{Icon, IconName};
    use openwebide_core::workspace_entries::parent;
    let workspace = expect_context::<WorkspaceState>();
    let git = expect_context::<GitState>();
    let is_dir = entry.is_dir;
    let entry = StoredValue::new(entry);
    let root = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        if let Some(root) = root.get() {
            let _ = root.set_attribute("aria-level", &(depth.get() + 1).to_string());
        }
    });
    let toggle = Callback::new(move |()| {
        if is_dir {
            on_toggle.run(entry.get_value().path);
        } else {
            workspace.expanded.update(|expanded| {
                let path = entry.get_value().path;
                if !expanded.remove(&path) {
                    expanded.insert(path);
                }
            });
        }
    });
    let activate = Callback::new(move |()| {
        if is_dir {
            on_toggle.run(entry.get_value().path);
        } else {
            on_open.run(entry.get_value().path);
        }
    });
    let folder_summaries = git.folder_summaries;
    let description = Signal::derive(move || {
        let value = entry.get_value();
        let available = git.status.with(|repo| {
            repo.as_ref().is_some_and(|repo| {
                repo.availability == openwebide_core::git::GitStatusAvailability::Complete
            })
        });
        let status = if is_dir {
            folder_summaries.with(|summaries| {
                summaries.get(&value.path).map_or_else(
                    || {
                        if available {
                            "Unchanged folder".into()
                        } else {
                            "Descendant status unavailable".into()
                        }
                    },
                    crate::git_status::FolderSummary::description,
                )
            })
        } else {
            git.status.with(|repo| {
                repo.as_ref()
                    .and_then(|repo| repo.files.get(&value.path))
                    .copied()
                    .map_or(
                        if available {
                            crate::git_status::Status::Clean
                        } else {
                            crate::git_status::Status::Unavailable
                        },
                        crate::git_status::Status::from,
                    )
                    .presentation()
                    .text
                    .to_owned()
            })
        };
        format!("{}, {status}", value.path)
    });
    view! {
        <div node_ref=root data-context-menu="" class=move || if workspace.open_file.get().as_deref() == Some(&entry.get_value().path) {"tree-item selected"} else {"tree-item"}
            style=move || format!("padding-left: {}px", 8 + depth.get() as usize * 14) tabindex="0" role="treeitem" aria-label=move ||description.get() title=move ||description.get() data-tree-path=entry.get_value().path
            aria-expanded=move || (is_dir || nested.get()).then(|| workspace.expanded.with(|dirs| dirs.contains(&entry.get_value().path)).to_string())
            on:click=move |_| activate.run(())
            on:keydown=move |event: web_sys::KeyboardEvent| {
                if event.target().and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                    .is_some_and(|target| target.closest(".ui-dropdown").ok().flatten().is_some()) {return;}
                let rows = tree_rows(event.target());
                let index = rows.iter().position(|row| row.get_attribute("data-tree-path").as_deref() == Some(&entry.get_value().path)).unwrap_or(0);
                let target = match event.key().as_str() {
                    "ArrowDown" => rows.get(index + 1),
                    "ArrowUp" => rows.get(index.saturating_sub(1)),
                    "Home" => rows.first(), "End" => rows.last(),
                    "ArrowRight" if is_dir || nested.get_untracked() => {
                        if workspace.expanded.with_untracked(|dirs| dirs.contains(&entry.get_value().path)) {rows.get(index + 1)}
                        else {toggle.run(()); None}
                    }
                    "ArrowLeft" => {
                        if (is_dir || nested.get_untracked()) && workspace.expanded.with_untracked(|dirs| dirs.contains(&entry.get_value().path)) {toggle.run(()); None}
                        else {rows.iter().find(|row| row.get_attribute("data-tree-path").as_deref() == Some(if changes_only {parent(&entry.get_value().path).to_string()} else {tree_parent.get_untracked()}.as_str()))}
                    }
                    _ => None,
                };
                if matches!(event.key().as_str(), "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" | "Home" | "End") {
                    event.prevent_default(); if let Some(target) = target {let _ = target.focus();} return;
                }
                if matches!(event.key().as_str(), "Enter" | " ") {event.prevent_default(); activate.run(());}
            }
            >
            {(!changes_only).then(|| view! {
                <span class="tree-disclosure">
                    <Show when=move || is_dir || nested.get()>
                        <button type="button" class="icon-btn ui-icon" aria-label=move || format!("{} {}", if workspace.expanded.with(|paths| paths.contains(&entry.get_value().path)) {"Collapse"} else {"Expand"}, entry.get_value().name)
                            on:keydown=move |event: web_sys::KeyboardEvent| { if matches!(event.key().as_str(), "Enter" | " ") { event.stop_propagation(); } }
                            on:click=move |event| {event.stop_propagation(); toggle.run(());}>
                            <Icon name=Signal::derive(move || if workspace.expanded.with(|paths| paths.contains(&entry.get_value().path)) {IconName::ChevronDown} else {IconName::ChevronRight}) />
                        </button>
                    </Show>
                </span>
            })}
            {if is_dir {
                view! {<span class=move ||folder_summaries.with(|summaries|format!("tree-icon {}",summaries.get(&entry.get_value().path).map_or("",crate::git_status::FolderSummary::class))) title=move ||folder_summaries.with(|summaries|summaries.get(&entry.get_value().path).map_or_else(||if git.status.with(|repo|repo.as_ref().is_none_or(|repo|repo.availability!=openwebide_core::git::GitStatusAvailability::Complete)){"Descendant status unavailable".into()}else{"Unchanged folder".into()},crate::git_status::FolderSummary::description))>
                    <Icon name=Signal::derive(move ||if folder_summaries.with(|summaries|summaries.contains_key(&entry.get_value().path)){IconName::FolderGit}else if workspace.expanded.with(|dirs|dirs.contains(&entry.get_value().path)){IconName::FolderOpen}else{IconName::Folder}) />
                </span>}.into_any()
            } else {
                view! {<super::ui::GitStatusIcon status=Signal::derive(move ||git.status.with(|repo|repo.as_ref().and_then(|repo|repo.files.get(&entry.get_value().path)).copied().map_or(if git.status.with(|repo|repo.as_ref().is_none_or(|repo|repo.availability!=openwebide_core::git::GitStatusAvailability::Complete)){crate::git_status::Status::Unavailable}else{crate::git_status::Status::Clean},crate::git_status::Status::from))) />}.into_any()
            }}
            <span class="tree-name">{entry.get_value().name}</span>
            {move || git.status.with(|repo| {
                let repo = repo.as_ref()?;
                let path = entry.get_value().path;
                let counts = if is_dir {
                    let prefix = format!("{path}/");
                    repo.file_line_stats.iter().filter(|(file, _)| file.starts_with(&prefix)).fold(openwebide_core::GitLineStats::default(), |mut total, (_, changes)| { total.insertions += changes.insertions; total.deletions += changes.deletions; total })
                } else { repo.file_line_stats.get(&path).copied().unwrap_or_default() };
                let status = repo.files.get(&path).map(|status| status.description()).unwrap_or("Contains changed files");
                (counts.insertions > 0 || counts.deletions > 0).then(|| view! { <span class="tree-line-stats" aria-label=format!("{status}: {} added lines, {} removed lines", counts.insertions, counts.deletions) title=status><span class="git-insertions">{format!("+{}", counts.insertions)}</span><span class="git-deletions">{format!("−{}", counts.deletions)}</span></span> }.into_any())
            })}
            <FileEntryMenu entry=entry.get_value() changes_only=changes_only />
        </div>
    }
}

/// The same file operations are available from tree rows and editor tabs.
#[component]
pub(super) fn FileEntryMenu(
    entry: FileEntry,
    #[prop(default = false)] changes_only: bool,
    #[prop(default = false)] context_only: bool,
    #[prop(optional)] children: Option<ChildrenFn>,
) -> impl IntoView {
    use super::{
        dropdown::Dropdown,
        ui::{Icon, IconName},
    };
    use crate::state_actions::file_tree::FileTreeActions;
    use openwebide_core::{
        git::{GitPathAction, GitPathChanges},
        vfs::VfsEntryKind,
    };
    let git = expect_context::<GitState>();
    let actions = use_context::<FileTreeActions>();
    let is_dir = entry.is_dir;
    let entry = StoredValue::new(entry);
    let open = RwSignal::new(false);
    let anchor = RwSignal::new(None::<(f64, f64)>);
    let changes = RwSignal::new(None::<Result<GitPathChanges, String>>);
    let menu_generation = StoredValue::new(0_u64);
    let owner = Owner::current().expect("File tree entry has an owner");
    let load_changes = Callback::new(move |()| {
        changes.set(None);
        menu_generation.update_value(|generation| *generation += 1);
        let generation = menu_generation.get_value();
        if let Some(actions) = actions {
            let epoch = actions.epoch.get_untracked();
            leptos::task::spawn_local(async move {
                let result = actions.path_changes().await;
                if actions.epoch.try_get_untracked() == Some(epoch)
                    && menu_generation.try_get_value() == Some(generation)
                    && open.try_get_untracked() == Some(true)
                {
                    changes.set(Some(result));
                }
            });
        }
    });
    let show = Callback::new(move |point: Option<(f64, f64)>| {
        if actions.is_none() {
            return;
        }
        anchor.set(point);
        open.set(true);
        load_changes.run(());
    });
    Effect::new(move |_| {
        if let Some(actions) = actions {
            actions.epoch.get();
        }
        open.set(false);
    });
    let root = NodeRef::<leptos::html::Span>::new();
    super::context_menu::context_menu_target(
        move || {
            actions?.epoch.get();
            root.get()?
                .closest("[data-context-menu]")
                .ok()
                .flatten()?
                .dyn_into::<web_sys::HtmlElement>()
                .ok()
        },
        show,
    );
    let stage = Signal::derive(move || {
        changes.with(|changes| {
            changes
                .as_ref()
                .and_then(|changes| changes.as_ref().ok())
                .is_some_and(|changes| changes.has_unstaged(&entry.get_value().path))
        })
    });
    let unstage = Signal::derive(move || {
        changes.with(|changes| {
            changes
                .as_ref()
                .and_then(|changes| changes.as_ref().ok())
                .is_some_and(|changes| changes.has_staged(&entry.get_value().path))
        })
    });
    let revert = Signal::derive(move || {
        changes.with(|changes| {
            changes
                .as_ref()
                .and_then(|changes| changes.as_ref().ok())
                .is_some_and(|changes| {
                    changes.has_head && changes.has_tracked_changes(&entry.get_value().path)
                })
        })
    });
    let untracked = Signal::derive(move || {
        changes.with(|changes| {
            changes
                .as_ref()
                .and_then(|changes| changes.as_ref().ok())
                .is_some_and(|changes| {
                    changes.untracked.iter().any(|path| {
                        openwebide_core::workspace_entries::contains_path(
                            &entry.get_value().path,
                            path,
                        )
                    })
                })
        })
    });
    let review = Signal::derive(move || {
        stage.get()
            || unstage.get()
            || changes_only
            || git.status.with(|status| {
                status.as_ref().is_some_and(|status| {
                    status.files.keys().any(|path| {
                        openwebide_core::workspace_entries::contains_path(
                            &entry.get_value().path,
                            path,
                        )
                    })
                })
            })
    });
    let disabled = Signal::derive(move || {
        actions.is_none_or(FileTreeActions::disabled)
            || openwebide_core::workspace_entries::entry_path(&entry.get_value().path).is_err()
    });
    view! { <span class="ui-action-menu-context" node_ref=root>
            {actions.map(move |actions| view! {
                <Dropdown aria_label="File actions" class="ui-action-menu tree-entry-menu" trigger_class=if context_only { "sr-only" } else { "icon-btn ui-icon" } hide_caret=true
                    retain_contents=true open=open pointer_anchor=anchor.into() on_open=Callback::new(move |()| {anchor.set(None); load_changes.run(());})
                    label=|| view! {<Icon name=IconName::Ellipsis />}>
                    <div class="ui-action-items" on:click=move |event| {
                        if event.target().and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                            .is_some_and(|target| target.closest("button").ok().flatten().is_some()) {open.set(false);}
                    }>{move || changes.with(|result| result.as_ref().and_then(|result| result.as_ref().err()).map(|error| view! {<div class="form-hint" role="status">{format!("Git actions unavailable: {error}")}</div>}))}
                    {owner.with(|| view! {
                        {children.as_ref().map(|children| children())}
                        {(!changes_only).then(|| view! {
                        <h3 class="ui-menu-heading">{if is_dir {"Folder"} else {"File"}}</h3>
                        {is_dir.then(|| view! {
                            <button class="recent-item" role="menuitem" disabled=move || disabled.get()
                                on:click=move |_| actions.create(&entry.get_value().path, VfsEntryKind::File)><crate::components::ui::Icon name=crate::components::ui::IconName::File /><span>"New file"</span></button>
                            <button class="recent-item" role="menuitem" disabled=move || disabled.get()
                                on:click=move |_| actions.create(&entry.get_value().path, VfsEntryKind::Directory)><crate::components::ui::Icon name=crate::components::ui::IconName::Folder /><span>"New folder"</span></button>
                        })}
                        <button class="recent-item" role="menuitem" disabled=move || disabled.get() on:click=move |_| actions.move_entry(&entry.get_value(), true)><crate::components::ui::Icon name=crate::components::ui::IconName::Pencil /><span>"Rename"</span></button>
                        <button class="recent-item" role="menuitem" disabled=move || disabled.get() on:click=move |_| actions.move_entry(&entry.get_value(), false)><crate::components::ui::Icon name=crate::components::ui::IconName::FolderInput /><span>"Move"</span></button>
                        <button class="recent-item" role="menuitem" on:click=move |_| actions.copy_path(&entry.get_value().path)><crate::components::ui::Icon name=crate::components::ui::IconName::Copy /><span>"Copy path"</span></button>
                        {context_only.then(|| view! { <button class="recent-item" role="menuitem" disabled=move || disabled.get() on:click=move |_| actions.reveal(&entry.get_value().path)><crate::components::ui::Icon name=crate::components::ui::IconName::FolderOpen /><span>"Reveal in Files"</span></button> })}
                        <button class="recent-item" role="menuitem" disabled=move || disabled.get() on:click=move |_| actions.delete(&entry.get_value())><crate::components::ui::Icon name=crate::components::ui::IconName::X /><span>"Delete"</span></button>
                        })}
                        <h3 class="ui-menu-heading">"Git"</h3>
                        {(!is_dir).then(|| view!{<button class="recent-item" role="menuitem" on:click=move |_| actions.history(&entry.get_value().path)><Icon name=IconName::FileClock /><span>"View file history"</span></button>})}
                        <button class="recent-item" role="menuitem" disabled=move || disabled.get() || !stage.get()
                            on:click=move |_| actions.git_action(&entry.get_value().path, GitPathAction::Stage)><crate::components::ui::Icon name=crate::components::ui::IconName::Plus /><span>{move || if untracked.get() {"Add / track"} else {"Stage"}}</span></button>
                        <button class="recent-item" role="menuitem" disabled=move || disabled.get() || !unstage.get()
                            on:click=move |_| actions.git_action(&entry.get_value().path, GitPathAction::Unstage)><crate::components::ui::Icon name=crate::components::ui::IconName::Minus /><span>"Unstage"</span></button>
                        <button class="recent-item" role="menuitem" disabled=move || disabled.get() || !untracked.get()
                            on:click=move |_| actions.ignore(&entry.get_value())><crate::components::ui::Icon name=crate::components::ui::IconName::EyeOff /><span>"Ignore"</span></button>
                        <button class="recent-item" role="menuitem" disabled=move || disabled.get() || !revert.get()
                            on:click=move |_| actions.git_action(&entry.get_value().path, GitPathAction::Revert)><crate::components::ui::Icon name=crate::components::ui::IconName::Undo2 /><span>"Revert changes"</span></button>
                        <h3 class="ui-menu-heading">"Chat"</h3>
                        <button class="recent-item" role="menuitem" title="Explain the purpose, behavior and how the code works" disabled=move || actions.disabled() on:click=move |_| actions.chat(&entry.get_value(), "Explain how this works:", false)><crate::components::ui::Icon name=crate::components::ui::IconName::Info /><span>"Explain in chat"</span></button>
                        <button class="recent-item" role="menuitem" title="Give a brief overview of the purpose and key contents" disabled=move || actions.disabled() on:click=move |_| actions.chat(&entry.get_value(), "Give a concise overview of", false)><crate::components::ui::Icon name=crate::components::ui::IconName::FileText /><span>"Summarize in chat"</span></button>
                        <button class="recent-item" role="menuitem" disabled=move || actions.disabled() || !review.get()
                            on:click=move |_| actions.chat(&entry.get_value(), "Review changes for bugs and regressions in", true)><crate::components::ui::Icon name=crate::components::ui::IconName::ClipboardCheck /><span>"Review changes in chat"</span></button>
                    })}</div>
                </Dropdown>
            })}
    </span> }
}
