use std::{any::Any, cell::RefCell, rc::Rc};

use leptos::prelude::*;
use openwebide_core::{ChatSession, Connection, Project, ProviderKind, WorkspaceMode};
use openwebide_frontend::{
    backend::Api,
    components::{ChatPane, ConfirmDialog, Editor},
    state::{
        auth::AuthState,
        chat::ChatState,
        git::GitState,
        layout::LayoutState,
        projects::ProjectsState,
        settings::{SettingsState, Theme},
        ui::UiState,
        workspace::WorkspaceState,
    },
    state_actions::{
        chat::{ChatActionContext, ChatActions},
        workspace::WorkspaceActions,
    },
    testing::fake_backend::FakeBackend,
};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

#[derive(Clone)]
pub struct TestState {
    pub auth: AuthState,
    pub monitors: openwebide_frontend::state::monitors::MonitorsState,
    pub monitor_actions: openwebide_frontend::monitors::MonitorActions,
    pub scheduled: openwebide_frontend::state::scheduled::TasksState,
    pub task_actions: openwebide_frontend::scheduled::TaskActions,
    pub skills: openwebide_frontend::state::skills::SkillsState,
    pub skill_actions: openwebide_frontend::project_skills::ProjectSkillActions,
    pub memories: openwebide_frontend::state::memories::MemoriesState,
    pub memory_actions: openwebide_frontend::project_memory::ProjectMemoryActions,
    pub api: Api,
    pub fake: Rc<FakeBackend>,
    pub chat: ChatState,
    pub session_management: openwebide_frontend::state::sessions::SessionsState,
    pub projects: ProjectsState,
    pub workspace: WorkspaceState,
    pub settings: SettingsState,
    pub ui: UiState,
    pub git: GitState,
    pub bridge: RwSignal<Option<openwebide_frontend::bridge::BridgeConn>, LocalStorage>,
}

impl TestState {
    fn new() -> Self {
        Self::with_backend(Rc::new(FakeBackend::default()))
    }

    fn with_backend(fake: Rc<FakeBackend>) -> Self {
        let api: Api = StoredValue::new_local(fake.clone());
        let ui = UiState::new();
        let projects = ProjectsState::new();
        let workspace = WorkspaceState::with_active_project(projects.active_project);
        let git = GitState::with_active_project(projects.active_project);
        let chat = ChatState::with_active_session_and_toast(workspace.active_session, ui.toast);
        let settings = SettingsState::new(Theme::Dark, "ws://localhost:3001".into());
        provide_context(api);
        provide_context(ui);
        let auth = AuthState::new();
        provide_context(auth);
        provide_context(LayoutState::with_active_project(projects.active_project));
        provide_context(projects);
        provide_context(workspace);
        provide_context(git);
        provide_context(chat);
        let skills = openwebide_frontend::state::skills::SkillsState::new();
        provide_context(skills);
        let skill_actions = openwebide_frontend::project_skills::ProjectSkillActions::new(
            api, skills, auth, projects, chat,
        );
        provide_context(skill_actions);
        let memories = openwebide_frontend::state::memories::MemoriesState::new();
        provide_context(memories);
        let session_management =
            openwebide_frontend::state::sessions::SessionsState::new(chat, projects.active_project);
        provide_context(session_management);
        provide_context(settings);
        provide_context(openwebide_frontend::project_git::ProjectGit::new(
            api,
            projects,
            settings,
            expect_context::<AuthState>(),
        ));
        let memory_actions = openwebide_frontend::project_memory::ProjectMemoryActions::new(
            api,
            memories,
            auth,
            projects,
            chat,
            expect_context::<openwebide_frontend::project_host::ProjectHost>(),
        );
        provide_context(memory_actions);
        let plugins = openwebide_frontend::state::plugins::PluginsState::default();
        provide_context(plugins);
        provide_context(
            openwebide_frontend::project_plugins::ProjectPluginActions::new(
                api,
                plugins,
                expect_context::<openwebide_frontend::project_host::ProjectHost>(),
                auth,
                projects,
                chat,
                settings,
            ),
        );
        let monitors = openwebide_frontend::state::monitors::MonitorsState::new();
        provide_context(monitors);
        let monitor_actions =
            openwebide_frontend::monitors::MonitorActions::new(api, monitors, auth, projects, chat);
        provide_context(monitor_actions);
        let scheduled = openwebide_frontend::state::scheduled::TasksState::new();
        provide_context(scheduled);
        let task_actions =
            openwebide_frontend::scheduled::TaskActions::new(api, scheduled, auth, projects, chat);
        provide_context(task_actions);
        Self {
            monitors,
            monitor_actions,
            scheduled,
            task_actions,
            auth,
            skills,
            skill_actions,
            memories,
            memory_actions,
            api,
            fake,
            chat,
            session_management,
            projects,
            workspace,
            settings,
            ui,
            git,
            bridge: RwSignal::new_local(None),
        }
    }

    pub fn seed_project(&self) {
        let project = Project {
            id: 1,
            name: "test".into(),
            mode: WorkspaceMode::Remote,
            path: Some("test".into()),
            user_id: None,
            created_at: 0,
        };
        self.fake.projects.borrow_mut().push(project.clone());
        self.projects.projects.set(vec![project]);
        self.projects.active_project.set(Some(1));
    }

    pub fn seed_skill_plugin(&self) -> openwebide_core::plugins::PreparedPlugin {
        use openwebide_core::plugins::PluginTool;
        let mut prepared = self.seed_memory_plugin();
        prepared.manifest.contributions.tools = [
            "skill_create",
            "skill_update",
            "skill_delete",
            "skill_search",
            "skill_list",
            "skill_read",
        ]
        .into_iter()
        .map(|name| PluginTool {
            name: name.into(),
            description: "SDK Skills fixture".into(),
            parameters: serde_json::json!({"type":"object"}),
            requires_approval: matches!(name, "skill_create" | "skill_update" | "skill_delete"),
        })
        .collect();
        prepared.manifest.executable.as_mut().unwrap().library = "skills_fixture".into();
        self.fake
            .project_plugin_entries
            .borrow_mut()
            .get_mut(&1)
            .unwrap()[0]
            .prepared = prepared.clone();
        prepared
    }

    pub fn seed_scheduling_plugin(&self) {
        use openwebide_core::plugins::{
            PluginInstallation, PluginTool, PluginUpdatePolicy, ProjectPlugin, RustPlugin,
        };
        let mut prepared = openwebide_core::plugins::testing::receipt();
        prepared.manifest.compatibility.plugin_api = 3;
        prepared.manifest.contributions.skills.clear();
        prepared.manifest.contributions.tools = [
            "schedule_create",
            "schedule_update",
            "schedule_set_enabled",
            "schedule_delete",
            "monitor",
        ]
        .into_iter()
        .map(|name| PluginTool {
            name: name.into(),
            description: "Scheduling UI fixture".into(),
            parameters: serde_json::json!({"type":"object"}),
            requires_approval: true,
        })
        .collect();
        prepared.manifest.executable = Some(RustPlugin {
            manifest: "Cargo.toml".into(),
            library: "tasks_fixture".into(),
            sdk_version: "0.1.0".into(),
            capabilities: vec!["collections".into()],
        });
        self.fake.project_plugin_entries.borrow_mut().insert(
            1,
            vec![ProjectPlugin {
                id: 1,
                revision: 1,
                enabled: true,
                prepared: prepared.clone(),
            }],
        );
        self.fake.plugins.borrow_mut().push(PluginInstallation {
            prepared,
            revision: 1,
            installed_at: 0,
            hosts: vec!["server".into()],
            default_enabled: true,
            update_policy: PluginUpdatePolicy::Notify,
        });
    }

    pub fn seed_memory_plugin(&self) -> openwebide_core::plugins::PreparedPlugin {
        use openwebide_core::plugins::{PluginTool, ProjectPlugin, RustPlugin};
        let mut prepared = openwebide_core::plugins::testing::receipt();
        prepared.manifest.compatibility.plugin_api = 3;
        prepared.manifest.contributions.skills.clear();
        prepared.manifest.contributions.tools = [
            "memory_create",
            "memory_update",
            "memory_delete",
            "memory_search",
            "memory_list",
            "memory_read",
        ]
        .into_iter()
        .map(|name| PluginTool {
            name: name.into(),
            description: "Host memory fixture".into(),
            parameters: serde_json::json!({"type":"object"}),
            requires_approval: matches!(name, "memory_create" | "memory_update" | "memory_delete"),
        })
        .collect();
        prepared.manifest.executable = Some(RustPlugin {
            manifest: "Cargo.toml".into(),
            library: "memory_fixture".into(),
            sdk_version: "0.1.0".into(),
            capabilities: vec!["collections".into()],
        });
        self.fake.project_plugin_entries.borrow_mut().insert(
            1,
            vec![ProjectPlugin {
                id: 1,
                revision: 1,
                enabled: true,
                prepared: prepared.clone(),
            }],
        );
        prepared
    }

    pub fn seed_connection(&self) {
        let connection = Connection {
            id: 1,
            name: "Ollama".into(),
            kind: ProviderKind::Ollama,
            base_url: "http://localhost:11434".into(),
            model: Some("qwen3:8b".into()),
            enabled: true,
            context_limit: Some(8192),
            tool_stream_unsupported: false,
            tool_stream_revision: 0,
            tool_selection: Default::default(),
        };
        self.fake.connections.borrow_mut().push(connection.clone());
        self.settings.connections.set(vec![connection]);
        self.settings.default_connection.set(Some(1));
    }

    pub fn seed_session(&self) {
        let session = ChatSession {
            pinned: false,
            archived: false,
            auto_title: false,
            title_revision: 0,
            id: 1,
            name: "test".into(),
            connection_id: Some(1),
            system_prompt_id: None,
            project_id: Some(1),
            user_id: None,
            created_at: 0,
        };
        self.fake.sessions.borrow_mut().push(session.clone());
        self.chat.sessions.set(vec![session]);
        self.chat.active_session.set(Some(1));
    }
}

pub struct Mounted {
    pub root: web_sys::HtmlElement,
    pub state: TestState,
    unmount: Option<Box<dyn Any>>,
}

impl Drop for Mounted {
    fn drop(&mut self) {
        self.unmount.take();
        self.root.remove();
    }
}

pub fn mount_test<N: IntoView + 'static>(view: impl FnOnce(TestState) -> N + 'static) -> Mounted {
    mount_backend(None, view)
}

pub fn mount_test_with_backend<N: IntoView + 'static>(
    fake: Rc<FakeBackend>,
    view: impl FnOnce(TestState) -> N + 'static,
) -> Mounted {
    mount_backend(Some(fake), view)
}

fn mount_backend<N: IntoView + 'static>(
    fake: Option<Rc<FakeBackend>>,
    view: impl FnOnce(TestState) -> N + 'static,
) -> Mounted {
    let document = web_sys::window().unwrap().document().unwrap();
    let root: web_sys::HtmlElement = document.create_element("div").unwrap().unchecked_into();
    document.body().unwrap().append_child(&root).unwrap();
    let state = Rc::new(RefCell::new(None));
    let slot = state.clone();
    let handle = leptos::mount::mount_to(root.clone(), move || {
        let state = fake.map_or_else(TestState::new, TestState::with_backend);
        *slot.borrow_mut() = Some(state.clone());
        view(state)
    });
    let state = state.borrow_mut().take().unwrap();
    Mounted {
        root,
        state,
        unmount: Some(Box::new(handle)),
    }
}

pub async fn settle() {
    for _ in 0..50 {
        JsFuture::from(js_sys::Promise::resolve(&JsValue::NULL))
            .await
            .unwrap();
    }
}

/// Wait for the observable result, yielding browser tasks and animation frames.
pub async fn wait_until(description: &str, ready: impl Fn() -> bool) {
    wait_until_with_timeout(description, 3000, ready).await;
}

/// Boundary-size correctness checks include debug WASM and complete DOM oracles;
/// they are not production latency thresholds. Ordinary contracts keep 3 s.
pub async fn wait_until_with_timeout(description: &str, timeout_ms: u32, ready: impl Fn() -> bool) {
    let deadline = js_sys::Date::now() + f64::from(timeout_ms);
    while !ready() {
        assert!(
            js_sys::Date::now() < deadline,
            "Timed out waiting for {description}"
        );
        openwebide_frontend::util::sleep_ms(10).await;
        settle().await;
    }
}

pub fn chat_actions(state: TestState) -> ChatActions {
    ChatActions::new(ChatActionContext {
        api: state.api,
        chat: state.chat,
        projects: state.projects,
        workspace: state.workspace,
        settings: state.settings,
        ui: state.ui,
        git: state.git,
        bridge: state.bridge,
        request_open: Callback::new(|_| ()),
        refresh_git: Callback::new(|()| ()),
        on_sync_click: Callback::new(|()| ()),
        on_commit: Callback::new(|_: openwebide_core::GitCommitRequest| ()),
        on_checkout: Callback::new(|_: (String, bool)| ()),
    })
}

pub fn chat_view(state: TestState) -> impl IntoView {
    let actions = chat_actions(state);
    view! {
        <ChatPane
            on_select_connection_model=actions.select_connection_model
            on_send=actions.send on_resume_run=actions.resume_run on_stop=actions.stop
            conversation_actions=actions.conversation
            queue_actions=actions.queue
            on_permission=actions.permission on_permission_always=actions.permission_always on_slash_command=actions.slash_command on_rewind=actions.rewind />
    }
}

pub fn editor_view(state: TestState) -> impl IntoView {
    let read_only = RwSignal::new(false);
    let actions = WorkspaceActions::new(
        state.api,
        state.projects,
        state.workspace,
        state.ui,
        read_only,
        Callback::new(|()| ()),
    );
    view! {
        <Editor read_only=read_only.into() on_open_lossy=actions.on_open_lossy on_save=actions.on_save on_accept=actions.on_accept on_reject=actions.on_reject />
        <ConfirmDialog />
    }
}

impl Mounted {
    pub fn element(&self, selector: &str) -> web_sys::HtmlElement {
        self.root
            .query_selector(selector)
            .unwrap()
            .unwrap_or_else(|| panic!("missing {selector}"))
            .unchecked_into()
    }

    pub fn click(&self, selector: &str) {
        self.element(selector).click();
    }

    pub fn click_text(&self, text: &str) {
        fn find(parent: &web_sys::Element, text: &str) -> Option<web_sys::HtmlElement> {
            let mut child = parent.first_element_child();
            while let Some(element) = child {
                if (element.tag_name() == "BUTTON" || element.class_list().contains("recent-item"))
                    && element.text_content().unwrap_or_default().contains(text)
                {
                    return Some(element.unchecked_into());
                }
                if let Some(found) = find(&element, text) {
                    return Some(found);
                }
                child = element.next_element_sibling();
            }
            None
        }
        find(&self.root, text)
            .unwrap_or_else(|| panic!("missing control {text}"))
            .click();
    }

    pub fn input(&self, value: &str) {
        let input: web_sys::HtmlTextAreaElement = self.element(".composer-input").unchecked_into();
        input.set_value(value);
        let init = web_sys::EventInit::new();
        init.set_bubbles(true);
        input
            .dispatch_event(&web_sys::Event::new_with_event_init_dict("input", &init).unwrap())
            .unwrap();
    }

    pub fn key(&self, key: &str, code: &str, alt: bool) {
        let init = web_sys::KeyboardEventInit::new();
        init.set_key(key);
        init.set_code(code);
        init.set_alt_key(alt);
        init.set_bubbles(true);
        init.set_cancelable(true);
        self.element(".composer-input")
            .dispatch_event(
                &web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init)
                    .unwrap(),
            )
            .unwrap();
    }
}

pub fn command_actions(
    state: TestState,
) -> openwebide_frontend::state_actions::commands::CommandActions {
    use openwebide_frontend::state_actions::{
        commands::{CommandActionContext, CommandActions},
        layout::LayoutActions,
    };
    let layout = expect_context::<LayoutState>();
    let layout_actions = LayoutActions::new(state.api, layout, state.auth, state.ui);
    provide_context(layout_actions);
    let chat_actions = chat_actions(state.clone());
    let actions = CommandActions::new(CommandActionContext {
        workspace: state.workspace,
        chat: state.chat,
        settings: state.settings,
        ui: state.ui,
        layout: layout_actions,
        new_session: chat_actions.on_new_session,
        open_local: Callback::new(|()| ()),
        open_remote: Callback::new(|()| ()),
        open_settings: Callback::new(move |()| state.settings.show_settings.set(true)),
        slash: chat_actions.slash_command,
    });
    provide_context(actions);
    let projects = state.projects;
    let navigation = openwebide_frontend::state_actions::navigation::NavigationActions::new(
        projects,
        state.chat,
        state.ui,
        layout_actions,
        Callback::new(move |id| {
            projects.open_tab(id);
            projects.select_project(id);
        }),
        Callback::new(move |()| projects.active_project.set(None)),
    );
    provide_context(navigation);
    let workspace_actions = openwebide_frontend::state_actions::workspace::WorkspaceActions::new(
        state.api,
        state.projects,
        state.workspace,
        state.ui,
        RwSignal::new(false),
        Callback::new(|()| ()),
    );
    provide_context(workspace_actions);
    provide_context(
        openwebide_frontend::state_actions::omnibar::OmnibarActions::new(
            openwebide_frontend::state_actions::omnibar::OmnibarContext {
                api: state.api,
                projects: state.projects,
                chat: state.chat,
                ui: state.ui,
                commands: actions,
                navigation,
                open_file: workspace_actions.request_open,
            },
        ),
    );
    actions
}

pub async fn choose_dropdown(mounted: &Mounted, selector: &str, value: &str) {
    mounted.click(selector);
    settle().await;
    let root = mounted
        .element(selector)
        .closest(".ui-dropdown")
        .unwrap()
        .unwrap();
    let options = root.query_selector_all("[data-value]").unwrap();
    let option = (0..options.length())
        .filter_map(|index| options.item(index))
        .filter_map(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
        .find(|option| option.get_attribute("data-value").as_deref() == Some(value))
        .expect("dropdown option");
    option.click();
    settle().await;
}

/// Activate an action through its visible overflow menu, including repeated message rows.
pub async fn click_action(mounted: &Mounted, selector: &str) {
    use wasm_bindgen::JsCast;
    if let Some(backdrop) = mounted
        .root
        .query_selector(".ui-dropdown-backdrop")
        .unwrap()
    {
        backdrop.dyn_into::<web_sys::HtmlElement>().unwrap().click();
        settle().await;
    }
    let triggers = mounted
        .root
        .query_selector_all(".ui-action-menu > .ui-dropdown-trigger")
        .unwrap();
    for index in 0..triggers.length() {
        let trigger = triggers
            .item(index)
            .unwrap()
            .dyn_into::<web_sys::HtmlElement>()
            .unwrap();
        trigger.click();
        settle().await;
        if let Some(action) = mounted.root.query_selector(selector).unwrap() {
            action.dyn_into::<web_sys::HtmlElement>().unwrap().click();
            settle().await;
            return;
        }
        trigger.click();
        settle().await;
    }
    panic!("Missing menu action: {selector}");
}

pub fn recovery_editor_view(state: TestState) -> impl IntoView {
    let read_only = RwSignal::new(false);
    let actions = WorkspaceActions::new(
        state.api,
        state.projects,
        state.workspace,
        state.ui,
        read_only,
        Callback::new(|()| ()),
    );
    provide_context(actions);
    let recovery = openwebide_frontend::state::editor_recovery::EditorRecoveryState::new();
    provide_context(recovery);
    provide_context(
        openwebide_frontend::state_actions::editor_recovery::RecoveryActions::new(
            state.api,
            state.auth,
            state.projects,
            state.workspace,
            recovery,
            read_only,
            actions.request_open,
        ),
    );
    view! {
        <Editor read_only=read_only.into() on_open_lossy=actions.on_open_lossy on_save=actions.on_save on_accept=actions.on_accept on_reject=actions.on_reject />
        <ConfirmDialog />
    }
}
