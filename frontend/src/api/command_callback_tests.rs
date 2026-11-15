//! Exercise the browser facade and both real HTTP adapters without folder handles.
use super::*;
use crate::{
    backend::Backend,
    project_host::ProjectHost,
    state::{
        auth::AuthState,
        projects::ProjectsState,
        settings::{SettingsState, Theme},
    },
};
use leptos::prelude::*;
use openwebide_core::{UserId, UserRole};
use std::{cell::Cell, rc::Rc};
use wasm_bindgen::prelude::*;
use wasm_bindgen_test::*;

#[wasm_bindgen(inline_js = r#"
export function callbackFetch(hold) {
    const original = window.fetch;
    const state = {requests: [], finish: null};
    window.fetch = async request => {
        const body = await request.clone().text();
        if (request.url.endsWith('/bridge/token')) {
            return new Response(JSON.stringify({token:'fixture', expires_at:4102444800}), {status:200});
        }
        state.requests.push({url:request.url, method:request.method, body:JSON.parse(body)});
        if (hold) return new Promise(resolve => {state.finish = () => resolve(new Response('true', {status:200}));});
        return new Response('true', {status:200});
    };
    state.restore = () => {window.fetch = original;};
    return state;
}
export function callbackRestore(state) {state.restore();}
export function callbackRequests(state) {return JSON.stringify(state.requests);}
export function callbackFinish(state) {state.finish();}
export function callbackReady(state) {return state.finish !== null;}
export function callbackTick() {return new Promise(resolve => setTimeout(resolve, 0));}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = callbackFetch)]
    fn fetch(hold: bool) -> JsValue;
    #[wasm_bindgen(js_name = callbackRestore)]
    fn restore(state: &JsValue);
    #[wasm_bindgen(js_name = callbackRequests)]
    fn requests(state: &JsValue) -> String;
    #[wasm_bindgen(js_name = callbackFinish)]
    fn finish(state: &JsValue);
    #[wasm_bindgen(js_name = callbackReady)]
    fn ready(state: &JsValue) -> bool;
    #[wasm_bindgen(js_name = callbackTick)]
    async fn tick();
}
struct Fetch(JsValue);
impl Drop for Fetch {
    fn drop(&mut self) {
        restore(&self.0);
    }
}
fn account(id: i64) -> User {
    User {
        id: UserId::new(id),
        username: format!("account-{id}"),
        role: UserRole::User,
        created_at: 0,
    }
}
fn fixture(mode: WorkspaceMode) -> (Owner, BackendApi, AuthState, ProjectsState, ProjectHost) {
    crate::testing::initialize_runtime();
    let owner = Owner::new();
    let (api, auth, projects, host) = owner.with(|| {
        let mut api = BackendApi::from_location();
        api.base
            .set_value("http://current-callback.test/api".into());
        api.cross_origin = true;
        let auth = AuthState::new();
        auth.set_user(account(1));
        let projects = ProjectsState::new();
        projects.projects.set(vec![Project {
            id: 1,
            name: "callback".into(),
            mode,
            path: Some("unavailable-original-folder".into()),
            user_id: Some(UserId::new(1)),
            created_at: 0,
        }]);
        projects.needs_grant.update(|pending| {
            pending.insert(1);
        });
        let settings = SettingsState::new(Theme::Dark, "ws://native-callback.test:43123".into());
        let backend: Rc<dyn Backend> = Rc::new(api);
        let host = ProjectHost::new(StoredValue::new_local(backend), projects, settings, auth);
        (api, auth, projects, host)
    });
    (owner, api, auth, projects, host)
}

#[wasm_bindgen_test]
async fn current_backend_address_reaches_both_adapters_without_folder_permission() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        let (owner, api, _, projects, host) = fixture(mode);
        let mock = Fetch(fetch(false));
        tick().await;
        // Read the current client address at invocation, including an address
        // changed since ProjectHost construction.
        api.base.set_value("http://next-callback.test/api".into());
        assert!(host.reconnect_language_commands(1, || true).await.unwrap());
        let sent: Vec<serde_json::Value> = serde_json::from_str(&requests(&mock.0)).unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["method"], "POST");
        assert_eq!(
            sent[0]["url"],
            if mode == WorkspaceMode::Local {
                "http://native-callback.test:43123/plugins/languages/command/reconnect"
            } else {
                "http://next-callback.test/api/projects/1/language-commands/reconnect"
            }
        );
        assert_eq!(
            sent[0]["body"],
            json!({"user":1,"project":1,"callback":"http://next-callback.test/api"})
        );
        assert!(
            projects
                .local_handles
                .with_untracked(std::collections::HashMap::is_empty)
        );
        assert!(
            projects
                .needs_grant
                .with_untracked(|pending| pending.contains(&1))
        );
        drop(mock);
        owner.cleanup();
    }
}

#[wasm_bindgen_test]
async fn callback_results_cannot_cross_an_account_or_caller_context_change() {
    for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
        for change_account in [false, true] {
            let (owner, _, auth, _, host) = fixture(mode);
            let mock = Fetch(fetch(true));
            tick().await;
            let current = Rc::new(Cell::new(true));
            let guard = current.clone();
            let request = host.reconnect_language_commands(1, move || guard.get());
            let change = async {
                for _ in 0..1000 {
                    if ready(&mock.0) {
                        break;
                    }
                    tick().await;
                }
                assert!(ready(&mock.0), "Callback request never reached its adapter");
                if change_account {
                    auth.set_user(account(2));
                } else {
                    current.set(false);
                }
                finish(&mock.0);
            };
            let (result, ()) = futures::join!(request, change);
            assert!(result.is_err());
            drop(mock);
            owner.cleanup();
        }
    }
}
