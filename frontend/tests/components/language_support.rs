//! Explicit paired/server test-host setup for existing editor behavior contracts.
use super::{
    local_bridge::{
        HttpGuard, fake_bridge_http, probe_folder, set_bridge_language_handler, set_bridge_plugin,
    },
    support::{Mounted, TestState, mount_test},
};
use leptos::prelude::*;
use openwebide_core::{
    User, UserId, UserRole,
    plugins::{ProjectPlugin, execution::InvokePlugin, languages::*},
};
use openwebide_frontend::{
    state::{
        auth::AuthState, languages::LanguageStatus, plugins::PluginsState, projects::ProjectsState,
        settings::SettingsState,
    },
    testing::{
        fake_backend::{FakeBackend, LanguageHandler},
        languages::{standard_binding, standard_handler},
    },
};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};
use wasm_bindgen::{JsCast, prelude::*};

struct PendingLanguage {
    call: InvokePlugin,
    sender: futures::channel::oneshot::Sender<Result<LanguageOutput, String>>,
}

pub struct LanguageHost {
    http: HttpGuard,
    callback: Closure<dyn FnMut(String) -> js_sys::Promise>,
    previous: Option<String>,
    binding: ProjectPlugin,
    handler: LanguageHandler,
    engine: LanguageHandler,
    paused: Rc<Cell<bool>>,
    pending: Rc<RefCell<VecDeque<PendingLanguage>>>,
    pub calls: Rc<Cell<usize>>,
}
impl LanguageHost {
    pub async fn new() -> Self {
        let previous = openwebide_frontend::idb::get_bridge_pairing_token()
            .await
            .unwrap();
        openwebide_frontend::idb::set_bridge_pairing_token("standard-language-test-token")
            .await
            .unwrap();
        let http = HttpGuard(fake_bridge_http());
        let binding = standard_binding();
        let engine = standard_handler(&binding);
        let paused = Rc::new(Cell::new(false));
        let pending = Rc::new(RefCell::new(VecDeque::new()));
        let calls = Rc::new(Cell::new(0));
        let handler: LanguageHandler = {
            let engine = engine.clone();
            let paused = paused.clone();
            let pending = pending.clone();
            let calls = calls.clone();
            Rc::new(move |call| {
                if matches!(
                    serde_json::from_str::<LanguageInput>(&call.arguments).unwrap(),
                    LanguageInput::Analyze(_)
                ) {
                    calls.set(calls.get() + 1);
                    if paused.get() {
                        let (sender, receiver) = futures::channel::oneshot::channel();
                        pending
                            .borrow_mut()
                            .push_back(PendingLanguage { call, sender });
                        return Box::pin(async move {
                            receiver
                                .await
                                .unwrap_or_else(|_| Err("Fixture request cancelled".into()))
                        });
                    }
                }
                engine(call)
            })
        };
        let mut prepared = binding.prepared.clone();
        prepared.host_id = "fixture-paired".into();
        set_bridge_plugin(&http.0, &serde_json::to_string(&prepared).unwrap());
        let bridge_handler = handler.clone();
        let callback = Closure::<dyn FnMut(String) -> js_sys::Promise>::new(move |wire: String| {
            let handler = bridge_handler.clone();
            wasm_bindgen_futures::future_to_promise(async move {
                let call = serde_json::from_str(&wire).unwrap();
                let output = handler(call)
                    .await
                    .map_err(|error| JsValue::from(js_sys::Error::new(&error)))?;
                Ok(JsValue::from_str(&serde_json::to_string(&output).unwrap()))
            })
        });
        set_bridge_language_handler(&http.0, callback.as_ref().unchecked_ref());
        Self {
            http,
            callback,
            previous,
            binding,
            handler,
            engine,
            paused,
            pending,
            calls,
        }
    }
    pub fn pause(&self) {
        self.paused.set(true);
    }
    pub fn resume(&self) {
        self.paused.set(false);
    }
    pub fn pending(&self) -> usize {
        self.pending.borrow().len()
    }
    pub fn next_request(&self) -> AnalysisRequest {
        let pending = self.pending.borrow();
        let LanguageInput::Analyze(request) =
            serde_json::from_str(&pending.front().unwrap().call.arguments).unwrap()
        else {
            unreachable!("only analysis calls are deferred")
        };
        request
    }
    /// The in-process public toolkit completes synchronously. Delivering its
    /// result here lets DOM polling hold and release normal host publications.
    pub fn respond_now(&self, valid: bool) {
        use futures::FutureExt;
        let PendingLanguage { call, sender } = self.pending.borrow_mut().pop_front().unwrap();
        let result = if valid {
            (self.engine)(call)
                .now_or_never()
                .expect("the fixture toolkit does not perform asynchronous I/O")
        } else {
            Err("Fixture request cancelled".into())
        };
        let _ = sender.send(result);
    }
    pub fn respond_status(&self, status: AnalysisStatus) {
        let request = self.next_request();
        let pending = self.pending.borrow_mut().pop_front().unwrap();
        let _ = pending
            .sender
            .send(Ok(LanguageOutput::Analyzed(Box::new(AnalysisResponse {
                version: request.version,
                document: request.document,
                request: request.request,
                revision: request.revision,
                language: request.language,
                source_digest: request.source_digest,
                status,
                analysis: None,
            }))));
    }
    /// Emulate host cache eviction through the real engine's Close contract.
    pub fn forget_document(&self) {
        use futures::FutureExt;
        let request = self.next_request();
        let mut call = self.pending.borrow().front().unwrap().call.clone();
        call.arguments = serde_json::to_string(&LanguageInput::Close {
            version: request.version,
            document: request.document,
        })
        .unwrap();
        assert!(matches!(
            (self.engine)(call).now_or_never().unwrap().unwrap(),
            LanguageOutput::Closed { .. }
        ));
    }
    pub async fn respond(&self) {
        let PendingLanguage { call, sender } = self.pending.borrow_mut().pop_front().unwrap();
        let _ = sender.send((self.engine)(call).await);
    }
    pub fn cancel_pending(&self) {
        for pending in self.pending.borrow_mut().drain(..) {
            let _ = pending.sender.send(Err("Fixture request cancelled".into()));
        }
    }
    fn seed_project(
        &self,
    ) -> impl Fn(AuthState, ProjectsState, SettingsState, Rc<FakeBackend>, PluginsState) + Clone + use<>
    {
        let binding = self.binding.clone();
        let handler = self.handler.clone();
        move |auth: AuthState,
              project_state: ProjectsState,
              settings: SettingsState,
              fake: Rc<FakeBackend>,
              plugins: PluginsState| {
            assert!(
                project_state.active_project.get_untracked().is_some(),
                "seed a project first"
            );
            let projects = project_state.projects.with_untracked(|projects| {
                projects
                    .iter()
                    .map(|project| project.id)
                    .collect::<Vec<_>>()
            });
            if auth.user.get_untracked().is_none() {
                auth.set_user(User {
                    id: UserId::new(1),
                    username: "editor-owner".into(),
                    role: UserRole::User,
                    created_at: 0,
                });
            }
            settings.bridge_url.set("ws://bridge.test:3001".into());
            project_state.local_handles.update(|handles| {
                for project in &projects {
                    handles
                        .entry(*project)
                        .or_insert_with(|| probe_folder().unchecked_into());
                }
            });
            *fake.language_handler.borrow_mut() = Some(handler.clone());
            for project in projects {
                fake.project_plugin_entries
                    .borrow_mut()
                    .insert(project, vec![binding.clone()]);
            }
            plugins.project_plugins.set(vec![binding.clone()]);
        }
    }
    pub fn seed(&self) -> impl Fn(&TestState) + Clone + use<> {
        let seed = self.seed_project();
        move |state| {
            seed(
                state.auth,
                state.projects,
                state.settings,
                state.fake.clone(),
                expect_context::<PluginsState>(),
            );
        }
    }
    pub fn mount<N: IntoView + 'static>(
        &self,
        view: impl FnOnce(TestState) -> N + 'static,
    ) -> Mounted {
        let seed = self.seed_project();
        mount_test(move |state| {
            let (auth, projects, settings, fake, plugins) = (
                state.auth,
                state.projects,
                state.settings,
                state.fake.clone(),
                expect_context::<PluginsState>(),
            );
            let view = view(state);
            let project = projects.active_project.get_untracked();
            if project.is_some() {
                untrack(|| seed(auth, projects, settings, fake.clone(), plugins));
            }
            // Signals and the backend do not retain the editor's Owner. Capturing
            // TestState here would keep its own cleanup alive after unmount.
            let seeded = Cell::new(project);
            Effect::new(move |_| {
                let project = projects.active_project.get();
                if seeded.replace(project) != project && project.is_some() {
                    untrack(|| seed(auth, projects, settings, fake.clone(), plugins));
                }
            });
            view
        })
    }
    pub async fn close(self) {
        self.cancel_pending();
        openwebide_frontend::util::yield_task().await;
        if let Some(previous) = &self.previous {
            openwebide_frontend::idb::set_bridge_pairing_token(previous)
                .await
                .unwrap();
        } else {
            openwebide_frontend::idb::delete_bridge_pairing_token()
                .await
                .unwrap();
        }
        // Keep the callback live until mounted consumers have been unmounted.
        drop(self.callback);
        drop(self.http);
    }
}

pub async fn ready(state: &TestState) {
    let actions = state.editor_actions();
    let started = js_sys::Date::now();
    let reported = Cell::new(false);
    super::support::wait_until("installed language service", || {
        if let LanguageStatus::Unavailable(error) = state.languages.status.get_untracked()
            && state.languages.settings_loaded.get_untracked()
            && state.languages.selected.get_untracked().is_some()
        {
            panic!("Language fixture unavailable: {error}");
        }
        let ready = state.languages.status.get_untracked() == LanguageStatus::Ready && !actions.syntax_is_pending();
        if !ready && js_sys::Date::now() - started >= 2500.0 && !reported.replace(true) {
            wasm_bindgen_test::console_log!("language readiness: project={:?} path={:?} status={:?} loaded={} selected={:?} pending={} limit={:?} inputs={} source_bytes={} source_prefix={:?} prepared={:?}",
                state.projects.active_project.get_untracked(), state.workspace.open_file.get_untracked(),
                state.languages.status.get_untracked(), state.languages.settings_loaded.get_untracked(),
                state.languages.selected.with_untracked(|provider| provider.as_ref().map(|provider| provider.definition.id.clone())),
                actions.syntax_is_pending(), actions.limit(), state.fake.language_inputs.borrow().len(),
                state.workspace.content.with_untracked(|source| source.len()),
                state.workspace.content.with_untracked(|source| source.chars().take(80).collect::<String>()),
                state.workspace.editor_preparation.with_untracked(|prepared| prepared.as_ref().map(|prepared| (prepared.scope.key.clone(), prepared.status)))
            );
        }
        ready
    })
    .await;
}
