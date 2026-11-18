use super::dropdown::{ActionMenu, DropdownSelect, SelectOption};
use super::ui::{Icon, IconButton, IconName, PanelSearchRow, PanelToolbar};
use leptos::prelude::*;
use openwebide_core::git::history_graph;

/// A single inline History tool panel retains its selection while collapsed.
#[component]
pub fn GitHistory(
    on_open: Callback<String>,
    on_select_branch: Callback<String>,
    on_new_branch: Callback<()>,
) -> impl IntoView {
    let actions = crate::state_actions::git_history::GitHistoryActions::new();
    view! { <GitHistoryContent actions=actions on_open=on_open on_select_branch=on_select_branch on_new_branch=on_new_branch /> }
}

#[component]
fn GitHistoryContent(
    actions: crate::state_actions::git_history::GitHistoryActions,
    #[prop(default = false)] file_history: bool,
    on_open: Callback<String>,
    on_select_branch: Callback<String>,
    on_new_branch: Callback<()>,
) -> impl IntoView {
    let layout = expect_context::<crate::state::layout::LayoutState>();
    let crate::state_actions::git_history::GitHistoryActions {
        commits,
        refs,
        search,
        displayed,
        reference,
        loading,
        error,
        has_more,
        selected,
        diff,
        files,
        diff_loading,
        diff_error,
        parent,
        path: selected_path,
        load,
        select,
    } = actions;
    let summaries = Memo::new(move |_| {
        std::sync::Arc::new(crate::git_status::folders(
            files.get().iter().map(|file| {
                (
                    file.path.as_str(),
                    crate::git_status::Status::historical(&file.status),
                )
            }),
            openwebide_core::git::GitStatusAvailability::Complete,
        ))
    });
    let graph = Memo::new(move |_| {
        let history = commits.get();
        history
            .iter()
            .map(|commit| commit.hash.clone())
            .zip(history_graph(&history))
            .collect::<std::collections::BTreeMap<_, _>>()
    });
    view! {
        <div class="git-history">
            <PanelToolbar class="git-history-toolbar" inset=true>
                <PanelSearchRow class="git-history-search" unpadded=true>
                    <super::ui::TextInput label="Search commit messages" placeholder="Search commit messages…" value=search.read_only() on_change=Callback::new(move |value| search.set(value)) maxlength=256 />
                    <IconButton label="Clear search" disabled=Signal::derive(move || search.with(String::is_empty)) on_click=Callback::new(move |_| search.set(String::new()))><Icon name=IconName::X /></IconButton>
                </PanelSearchRow>
                <IconButton label="Refresh history" disabled=loading.read_only() on_click=Callback::new(move |_| load.run(false))><Icon name=IconName::RefreshCw /></IconButton>
                <Show when=move ||!file_history><ActionMenu aria_label="History actions">
                    <button role="menuitem" class="ui-dropdown-item recent-item btn" disabled=move || !reference.with(|reference| reference.as_ref().is_some_and(|value| value.starts_with("refs/heads/") || value.starts_with("refs/remotes/"))) on:click=move |_| {if let Some(branch)=reference.get_untracked().and_then(|value|value.strip_prefix("refs/heads/").or_else(||value.strip_prefix("refs/remotes/")).map(str::to_owned)) {on_select_branch.run(branch);}}><Icon name=IconName::GitBranch /><span>"Checkout branch"</span></button>
                    <button role="menuitem" class="ui-dropdown-item recent-item btn" on:click=move |_| on_new_branch.run(())><Icon name=IconName::Plus /><span>"New branch"</span></button>
                </ActionMenu></Show>
            </PanelToolbar>
            <PanelToolbar class="git-history-reference" inset=true>
                <Icon name=IconName::GitBranch />
                <DropdownSelect label="History branch" trigger_class="btn ghost" value=Signal::derive(move ||reference.get().unwrap_or_default()) options=Signal::derive(move || {
                    let mut options=vec![SelectOption::new("HEAD","Current branch")];if !file_history {options.insert(0,SelectOption::new("","All branches"));}
                    options.extend(refs.get().into_iter().map(|item| {let prefix=match item.kind.as_str(){"branch"=>"heads","remote"=>"remotes",_=>"tags"};SelectOption::new(format!("refs/{prefix}/{}",item.name),format!("{} · {}",item.name,item.kind))}));options
                }) on_change=Callback::new(move |value:String| {reference.set(if value.is_empty(){None}else{Some(value)});load.run(false);}) />
            </PanelToolbar>
            <div class="git-history-body">
                <div class="git-history-main">
                    <div class="git-history-list" role="group" aria-label="Commit history">
                        {move || displayed.with(|query| {
                            if query.reference != reference.get() {
                                let scope = match query.reference.as_deref() {
                                    None => "All branches",
                                    Some("HEAD") => "Current branch",
                                    Some(name) => name.strip_prefix("refs/heads/")
                                        .or_else(|| name.strip_prefix("refs/remotes/"))
                                        .or_else(|| name.strip_prefix("refs/tags/"))
                                        .unwrap_or(name),
                                };
                                let label = if query.search.is_empty() {
                                    format!("Showing results for {scope}")
                                } else {
                                    format!("Showing results for {scope} · {}", query.search)
                                };
                                Some(label)
                            } else if query.search != search.get() {
                                let label = if query.search.is_empty() {
                                    "Showing unfiltered results".into()
                                } else {
                                    format!("Showing results for {}", query.search)
                                };
                                Some(label)
                            } else {
                                None
                            }
                        }).map(|label| view! { <div class="panel-inline-inset git-history-applied-filter">{label}</div> })}
                        <For each=move ||{commits.get().into_iter().map(|commit|commit.hash).collect::<Vec<_>>()} key=Clone::clone children=move |hash|{view!{<HistoryRow hash=hash actions=actions graph=graph file_history=file_history />}} />
                        <Show when=move || loading.get() && commits.with(Vec::is_empty)><div class="git-history-empty" role="status">"Loading history…"</div></Show>
                        <Show when=move || !loading.get() && commits.with(Vec::is_empty) && error.get().is_none()><div class="git-history-empty">"No matching commits"</div></Show>
                        <super::ui::FeedbackOverlay message=error.read_only() on_dismiss=Callback::new(|()|()) />
                        <Show when=move || has_more.get()><button class="btn git-history-more" disabled=move || loading.get() on:click=move |_| load.run(true)>"Load older commits"</button></Show>
                    </div>
                    <div class="git-commit-details">
                        {move || selected.get().map(|commit| {
                            let parent_commit=StoredValue::new(commit.clone());
                            view!{
                                <div class="git-inspector-heading"><strong>{commit.subject.clone()}</strong><code>{commit.hash.chars().take(8).collect::<String>()}</code></div>
                                <details class="git-commit-metadata"><summary>{format!("{} · {}",commit.author,commit.authored_at.chars().take(10).collect::<String>())}</summary>
                                    <p>{format!("Author: {} <{}> · {}",commit.author,commit.author_email,commit.authored_at)}</p>
                                    <p>{format!("Committer: {} <{}> · {}",commit.committer,commit.committer_email,commit.committed_at)}</p><code>{commit.hash}</code><pre class="git-commit-message">{commit.message}</pre>
                                </details>
                                <Show when={let count=parent_commit.with_value(|commit|commit.parents.len());move || count > 1}>
                                    <DropdownSelect label="Compare with parent" trigger_class="btn ghost" value=Signal::derive(move ||parent.get().unwrap_or_default()) options=Signal::derive({move ||parent_commit.with_value(|commit|commit.parents.iter().enumerate().map(|(index,hash)|SelectOption::new(hash,format!("Parent {} · {}",index+1,&hash[..8]))).collect())}) on_change=Callback::new(move |hash|select.run((parent_commit.get_value(),Some(hash),None))) />
                                </Show>
                            }
                        })}
                        <Show when=move || selected.get().is_none()><div class="git-history-empty">"Select a commit to inspect its changes"</div></Show>
                        <div class="git-commit-inspector">
                            <div class="git-commit-files" style=move ||format!("--history-tree-width: {}px",layout.history_tree_width.get()) role="tree" aria-label="Changed files">{move || render_file_tree(files.get(), "", actions, on_open, summaries.get(),0)}</div>
                            <super::panel_resizer::PanelResizer kind=crate::state::layout::ActiveResizer::None panel=crate::state::layout::Panel::History size=layout.history_tree_width />
                            <div class="git-readonly-diff" role="region" aria-label="Read-only commit diff">
                                <div class="git-diff-filter"><span>{move || selected_path.get().unwrap_or_else(||"Changes".into())}</span><span class="ui-progress-slot"><Show when=move ||diff_loading.get()><super::ui::LoadingStatus label="Loading commit changes…" compact=true /></Show></span></div>
                                <super::ui::FeedbackOverlay message=diff_error.read_only() on_dismiss=Callback::new(|()|()) />
                                {move ||diff.get().map(|changes|view!{
                                    {changes.truncated.then(||view!{<p class="git-history-limit">"Preview limited to 5,000 lines or 512 KiB."</p>})}
                                    {changes.diff.is_empty().then(||view!{<p class="form-hint">"No text changes (the file may be binary or empty)."</p>})}
                                    {super::editor::render_commit_diff(&changes.diff,&selected_path.get().unwrap_or_default())}
                                })}
                            </div>
                        </div>
                    </div>
                </div>
            </div>
        </div>
    }
}

fn render_file_tree(
    files: Vec<openwebide_core::git::GitCommitFile>,
    prefix: &str,
    actions: crate::state_actions::git_history::GitHistoryActions,
    on_open: Callback<String>,
    summaries: std::sync::Arc<std::collections::BTreeMap<String, crate::git_status::FolderSummary>>,
    depth: usize,
) -> AnyView {
    let mut folders =
        std::collections::BTreeMap::<String, Vec<openwebide_core::git::GitCommitFile>>::new();
    let mut leaves = Vec::new();
    for file in files {
        let relative = file.path.strip_prefix(prefix).unwrap_or(&file.path);
        if let Some((folder, _)) = relative.split_once('/') {
            folders.entry(folder.to_owned()).or_default().push(file);
        } else {
            leaves.push(file);
        }
    }
    let directories=folders.into_iter().map(|(name,files)|{let next=format!("{prefix}{name}/");let summary=summaries.get(next.trim_end_matches('/')).cloned().unwrap_or_default();view!{<details class="git-history-folder" open><summary class="tree-item tree-row-depth" style=format!("--tree-depth:{depth}") aria-label=format!("{}, {}",next.trim_end_matches('/'),summary.description()) title=format!("{}, {}",next.trim_end_matches('/'),summary.description())><span class="tree-disclosure git-folder-chevron"><Icon name=IconName::ChevronDown /></span><span class=format!("tree-icon {}",summary.class()) aria-hidden="true"><Icon name=IconName::FolderGit /></span><span class="tree-name">{name.clone()}</span></summary><div role="group">{render_file_tree(files,&next,actions,on_open,summaries.clone(),depth+1)}</div></details>}}).collect_view();
    let entries=leaves.into_iter().map(|file|{let click=file.path.clone();let active=click.clone();let open=click.clone();let chosen=click.clone();let label=file.path.strip_prefix(prefix).unwrap_or(&file.path).to_owned();view!{<div class="git-commit-file" role="treeitem" aria-selected=move ||(actions.path.get().as_ref()==Some(&active)).to_string()>
        <button class="btn ghost tree-item tree-row-depth" style=format!("--tree-depth:{depth}") aria-label=format!("{}, {} ({}){}",file.path,crate::git_status::Status::historical(&file.status).presentation().text,file.status,file.previous_path.as_ref().map_or_else(String::new,|previous|format!(" from {previous}"))) title=format!("{} · {} ({}){}",file.path,crate::git_status::Status::historical(&file.status).presentation().text,file.status,file.previous_path.as_ref().map_or_else(String::new,|previous|format!(" · from {previous}"))) class:selected=move ||actions.path.get().as_ref()==Some(&chosen) on:click={let path=click.clone();move |_|{if let Some(commit)=actions.selected.get_untracked(){actions.select.run((commit,actions.parent.get_untracked(),Some(path.clone())));}}}><span class="tree-disclosure"></span><super::ui::GitStatusIcon status=crate::git_status::Status::historical(&file.status) /><span class="tree-name">{label}</span></button>
        <IconButton label="Open current file in editor" on_click=Callback::new(move |_|on_open.run(open.clone()))><Icon name=IconName::ExternalLink /></IconButton>
    </div>}}).collect_view();
    view! { {directories}{entries} }.into_any()
}

/// File history uses the same queries and inspector as repository History.
#[component]
pub fn FileHistory(path: String, on_close: Callback<()>) -> impl IntoView {
    let actions =
        crate::state_actions::git_history::GitHistoryActions::for_path(Some(path.clone()));
    let current_path = StoredValue::new(path.clone());
    let workspace_actions = expect_context::<crate::state_actions::workspace::WorkspaceActions>();
    let workspace = expect_context::<crate::state::workspace::WorkspaceState>();
    let projects = expect_context::<crate::state::projects::ProjectsState>();
    let auth = expect_context::<crate::state::auth::AuthState>();
    let chat = expect_context::<crate::state::chat::ChatState>();
    let layout = expect_context::<crate::state_actions::layout::LayoutActions>();
    let ui = expect_context::<crate::state::ui::UiState>();
    let git = expect_context::<crate::state::git::GitState>();
    git.file_history_generation
        .update(|generation| *generation += 1);
    let open_error = RwSignal::new(None);
    let open_current = Callback::new(move |_: String| {
        let modal_generation = git.file_history_generation.get_untracked();
        let epoch = auth.generation.get_untracked();
        let project = projects.active_project.get_untracked();
        let session = chat.active_session.get_untracked();
        let path = current_path.get_value();
        open_error.set(None);
        workspace_actions.open_with_completion.run((
            path.clone(),
            Callback::new(move |result: Result<(), String>| {
                if git.file_history_generation.try_get_untracked() != Some(modal_generation)
                    || auth.generation.try_get_untracked() != Some(epoch)
                    || projects.active_project.try_get_untracked() != Some(project)
                    || chat.active_session.try_get_untracked() != Some(session)
                    || current_path.try_get_value().as_ref() != Some(&path)
                {
                    return;
                }
                match result {
                    Err(error) => {
                        ui.notify(error.clone());
                        open_error.set(Some(error));
                    }
                    Ok(()) => {
                        layout.show.run(crate::state::layout::Panel::Editor);
                        let revision = workspace.editor_read_revision.get_untracked();
                        on_close.run(());
                        let path = path.clone();
                        leptos::task::spawn_local(async move {
                            crate::util::sleep_ms(0).await;
                            if git.file_history_generation.try_get_untracked()
                                != Some(modal_generation)
                                || auth.generation.try_get_untracked() != Some(epoch)
                                || projects.active_project.try_get_untracked() != Some(project)
                                || chat.active_session.try_get_untracked() != Some(session)
                                || git.file_history.try_get_untracked() != Some(None)
                                || workspace.editor_read_revision.try_get_untracked()
                                    != Some(revision)
                                || workspace.open_file.try_get_untracked() != Some(Some(path))
                            {
                                return;
                            }
                            if let Some(input) = document()
                                .query_selector("#panel-editor textarea")
                                .ok()
                                .flatten()
                            {
                                use wasm_bindgen::JsCast;
                                if let Ok(input) = input.dyn_into::<web_sys::HtmlElement>() {
                                    let _ = input.focus();
                                }
                            }
                        });
                    }
                }
            }),
        ));
    });
    let description = Signal::derive(move || path.clone());
    let title = Signal::derive(|| "File history".to_owned());
    view! {<super::modal::Modal title=title description=description on_close=on_close class="modal git-file-history-modal" size=super::ui::DialogSize::Available>
        <super::ui::FeedbackOverlay message=open_error.read_only() on_dismiss=Callback::new(|()|()) />
        <FileTimeline actions=actions />
        <GitHistoryContent actions=actions file_history=true on_open=open_current on_select_branch=Callback::new(|_|()) on_new_branch=Callback::new(|()|()) />
    </super::modal::Modal>}
}

#[component]
fn FileTimeline(actions: crate::state_actions::git_history::GitHistoryActions) -> impl IntoView {
    let root = NodeRef::<leptos::html::Div>::new();
    let width = RwSignal::new(800.0);
    Effect::new(move |_| {
        if let Some(root) = root.get() {
            crate::viewport::track_width((*root).clone().into(), width);
        }
    });
    let axis = Memo::new(move |_| {
        crate::git_timeline::layout(
            actions.commits.get().into_iter().map(|commit| {
                (
                    commit.hash,
                    crate::git_timeline::parse_timestamp(&commit.committed_at).unwrap_or(f64::NAN),
                )
            }),
            width.get(),
        )
    });
    let timezone = js_sys::Reflect::get(
        &js_sys::Intl::DateTimeFormat::new(&js_sys::Array::new(), &js_sys::Object::new())
            .resolved_options(),
        &wasm_bindgen::JsValue::from_str("timeZone"),
    )
    .ok()
    .and_then(|value| value.as_string())
    .unwrap_or_else(|| "browser local time".into());
    view! {<div class="git-file-timeline" node_ref=root role="group" aria-label="File history timeline">
        <div class="git-timeline-axis">
            {move || {
                let ticks=axis.get().ticks;
                let same_day=ticks.first().zip(ticks.last()).is_some_and(|((_,first),(_,last))|js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(*first)).to_date_string()==js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(*last)).to_date_string());
                ticks.into_iter().map(move |(position,time)| {
                let date=js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(time));
                view! {<time class="git-timeline-tick" style=format!("left:{}%",position*100.0)>{if same_day {date.to_locale_time_string("default").as_string().unwrap_or_default()}else{date.to_locale_date_string("default", &wasm_bindgen::JsValue::UNDEFINED).as_string().unwrap_or_default()}}</time>}
            }).collect_view()}}
            {move ||axis.get().clusters.into_iter().map(|cluster| {
                let hashes=cluster.hashes;
                let description=hashes.iter().filter_map(|hash|actions.commits.with(|commits|commits.iter().find(|commit|&commit.hash==hash).map(|commit|format!("{} · {} · {}",commit.committed_at,commit.subject,commit.hash)))).collect::<Vec<_>>().join("; ");
                view! {<details class="git-timeline-cluster" data-align=if cluster.position <= 0.5 {"start"} else {"end"} style=format!("left:{}%",cluster.position*100.0)><summary title=description.clone() aria-label=description><span class="git-timeline-dot"></span>{(hashes.len()>1).then(||hashes.len().to_string())}</summary>
                    <div class="ui-feedback-overlay">{hashes.into_iter().filter_map(|hash|actions.commits.with(|commits|commits.iter().find(|commit|commit.hash==hash).cloned())).map(|commit|{
                        let chosen=commit.clone();let hash=commit.hash.clone();view!{<button class="btn recent-item" aria-pressed=move ||actions.selected.with(|selected|selected.as_ref().is_some_and(|selected|selected.hash==hash)) on:click=move |_|actions.select.run((chosen.clone(),None,None))>{format!("{} · {} · {}",commit.committed_at,commit.subject,commit.hash)}</button>}
                    }).collect_view()}</div>
                </details>}
            }).collect_view()}
        </div>
        <span class="form-hint">{format!("Local timezone: {timezone}")}</span>
        <Show when=move ||!axis.get().undated.is_empty()><div role="group" aria-label="Undated commits">"Undated"
            {move ||axis.get().undated.into_iter().filter_map(|hash|actions.commits.with(|commits|commits.iter().find(|commit|commit.hash==hash).cloned())).map(|commit|{let chosen=commit.clone();view!{<button class="btn" on:click=move |_|actions.select.run((chosen.clone(),None,None))>{format!("{} · {}",commit.subject,commit.hash)}</button>}}).collect_view()}
        </div></Show>
    </div>}
}

#[component]
fn HistoryRow(
    hash: String,
    actions: crate::state_actions::git_history::GitHistoryActions,
    graph: Memo<std::collections::BTreeMap<String, openwebide_core::git::GitGraphRow>>,
    file_history: bool,
) -> impl IntoView {
    let hash = StoredValue::new(hash);
    let commit = Memo::new(move |_| {
        actions.commits.with(|commits| {
            commits
                .iter()
                .find(|commit| commit.hash == hash.get_value())
                .cloned()
        })
    });
    let row = Memo::new(move |_| graph.with(|graph| graph.get(&hash.get_value()).cloned()));
    let width = move || row.with(|row| row.as_ref().map_or(12, |row| row.width.max(1) * 14 + 12));
    view! {<button class="btn git-history-row" aria-pressed=move ||actions.selected.with(|value|value.as_ref().is_some_and(|value|value.hash==hash.get_value())) class:selected=move ||actions.selected.with(|value|value.as_ref().is_some_and(|value|value.hash==hash.get_value())) on:click=move |_|if let Some(commit)=commit.get_untracked(){actions.select.run((commit,None,None));}>
        <svg width=width height="100%" viewBox=move ||format!("0 0 {} 32",width()) preserveAspectRatio="none" class="git-graph" aria-hidden="true" hidden=file_history>
            {move ||row.get().map(|row| {
                let paths=row.edges.iter().map(|edge| {
                    let path=if edge.through_commit {format!("M {} 14 C {} 24 {} 24 {} 32",edge.from*14+12,edge.from*14+12,edge.to*14+12,edge.to*14+12)}else{format!("M {} 0 L {} 14 C {} 24 {} 24 {} 32",edge.from*14+12,edge.from*14+12,edge.from*14+12,edge.to*14+12,edge.to*14+12)};
                    view!{<path d=path fill="none" stroke=format!("var(--git-lane-{})",edge.color%6) stroke-width="2"/>}
                }).collect_view();
                view!{{paths}{row.incoming.then(||view!{<path d=format!("M {} 0 L {} 14",row.lane*14+12,row.lane*14+12) stroke=format!("var(--git-lane-{})",row.color%6) stroke-width="2"/>})}<circle cx=row.lane*14+12 cy="14" r="4" fill=format!("var(--git-lane-{})",row.color%6)/>}
            })}
        </svg>
        <span class="git-commit-subject"><span class="git-commit-title">{move ||commit.with(|commit|commit.as_ref().map(|commit|commit.subject.clone()))}</span><span class="git-commit-refs">{move ||commit.with(|commit|commit.as_ref().map(|commit|commit.refs.join(" · ")))}</span></span>
        <span class="git-commit-author">{move ||commit.with(|commit|commit.as_ref().map(|commit|commit.author.clone()))}</span><time>{move ||commit.with(|commit|commit.as_ref().map(|commit|commit.authored_at.chars().take(10).collect::<String>()))}</time><code>{hash.get_value().chars().take(8).collect::<String>()}</code>
    </button>}
}
